package feedhub.app.video

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.skiaCanvas
import androidx.compose.ui.layout.onSizeChanged
import org.jetbrains.skia.ColorAlphaType
import org.jetbrains.skia.ColorType
import org.jetbrains.skia.Data
import org.jetbrains.skia.EncodedImageFormat
import org.jetbrains.skia.Image
import org.jetbrains.skia.ImageInfo
import org.jetbrains.skia.SamplingMode
import java.nio.ByteBuffer

/**
 * `Data.makeWithoutCopy` 要一个 `Managed` 当"这块像素内存的所有权令牌"：**它被 close 的时候
 * Skia 才会去 free 像素**。我们**不拥有**那块内存（它是 Rust 的 `mmap`，生命周期由引擎管），
 * 所以令牌必须是个"什么也不释放"的东西。`Managed` 是抽象类、构造还不接受 0 地址
 * （会抛 `Can't wrap nullptr`），所以拿一个**空 `Bitmap`**：它自己那 0 字节和我们的像素
 * 无关，close 它不会碰 Rust 那块内存。进程里只一个、一直不 close——`Data` 的终结器因此
 * 永远不会去 free 引擎的像素。
 */
private val FRAME_OWNER_TOKEN: org.jetbrains.skia.Bitmap by lazy { org.jetbrains.skia.Bitmap() }

/**
 * 播放器引擎把解出来的 BGRA 帧交到这里，界面在 Compose 的 Canvas 里按比例居中画出来（弹幕和控件盖在上面）。
 * VLC 和 GStreamer 两个引擎共用。
 */
internal class FrameSurface(private val state: PlayerState) {
    private val lock = Any()
    private var latest: Image? = null
    private var pixels = ByteArray(0)
    private var tick by mutableLongStateOf(0L)

    @Volatile private var closed = false

    /**
     * 窗口看得见吗（[LocalWindowVisible]，`Draw` 里同步过来）。看不见时 [submit] 照收帧、但不触发重画——
     * 原来最小化后照样每秒画几十帧；重新看得见时补画一次最新帧。
     */
    @Volatile var visible = true
        private set

    /** 请求过几次重画（测试 / 测量用）。 */
    internal val redrawRequests: Long get() = tick

    /** 这次不画的帧数（测试 / 测量用）。 */
    @Volatile var skippedRedraws = 0L
        private set

    @Volatile private var lastSubmitNanos = 0L
    private val pending = java.util.concurrent.atomic.AtomicReference<(() -> Unit)?>(null)

    /**
     * 让 [write]（写界面状态，如播放位置）跟下一帧画面一起生效：画面正在走（[FRAME_GAP_NS] 内来过帧、窗口看得见）就挂起来，
     * [submit] 下一帧时先写它再触发重画，两件事落在同一次重画里；画面没在走（暂停、缓冲、看不见）就当场写。
     * 挂着还没写的那次被新的顶掉（只要最新的位置）。
     */
    fun withNextFrame(nowNanos: Long = System.nanoTime(), write: () -> Unit) {
        if (visible && !closed && nowNanos - lastSubmitNanos < FRAME_GAP_NS) {
            pending.set(write)
            // 挂上之后画面恰好停了：没人来取，就自己写
            if (System.nanoTime() - lastSubmitNanos >= FRAME_GAP_NS) pending.getAndSet(null)?.invoke()
        } else {
            pending.getAndSet(null)?.invoke()
            write()
        }
    }

    fun setVisible(v: Boolean) {
        if (visible == v) return
        visible = v
        if (v) tick++
    }

    /** 画布在屏幕上的像素尺寸变了（GStreamer 引擎据此让 GPU 直接缩放到这个大小）。 */
    @Volatile var onSurfaceSize: ((Int, Int) -> Unit)? = null

    @Volatile var frames = 0L
        private set

    /**
     * **从共享内存收一帧**。
     *
     * [addr] 是 Rust 那边 `mmap` 出来的块，帧在 `addr + slot*slotStride + SLOT_HEADER` 处，
     * BGRA、每行 [stride] 字节。[seq] 是这一帧的序号（用来判断"是不是新帧"）。
     *
     * **零拷贝**：`Data.makeWithoutCopy` 直接把这块原生内存包给 Skia，**不拷像素**——
     * 所以这条路上每帧的成本只有"造一个 Skia 的 Image 包装"（几十微秒），1080p60 也不会
     * 把主线程拖住。前提是读者占着这一槽时写者不会覆写它（Rust 侧的三槽轮转保证了）。
     *
     * 调用方读之前要先 `acquire`（Rust 侧的 `frame_seq` / `reader_slot` 协议），读完释放。
     */
    fun submitShared(addr: Long, slot: Long, slotStride: Long, headerBytes: Int, w: Int, h: Int, stride: Int) {
        synchronized(lock) {
            if (closed) return
            val pixelAddr = addr + slot * slotStride + headerBytes
            val size = stride * h
            if (size <= 0) return
            // `makeWithoutCopy` 不拷像素，只造一个 Skia 的 Data 句柄指到那块原生内存；
            // 那个 `Managed` 是"这块内存归谁管"的令牌。我们**不拥有**这块内存（它是 Rust 的
            // `mmap`，生命周期由引擎管），所以给一个真的、但不释放任何东西的令牌：
            // `Managed` 的构造不接受 0 地址（`Can't wrap nullptr`），所以拿一个 1 字节的
            // 空分配当令牌——它自己的 `close()` 什么也不做（终结器是 0），也就不会去 free
            // 我们那块 Rust 内存。
            val data = Data.makeWithoutCopy(pixelAddr, size, FRAME_OWNER_TOKEN)
            val img = Image.makeRaster(
                ImageInfo(w, h, ColorType.BGRA_8888, ColorAlphaType.OPAQUE),
                data,
                stride,
            )
            latest?.close()
            latest = img
            frames++
        }
        lastSubmitNanos = System.nanoTime()
        pending.getAndSet(null)?.invoke()
        if (visible) { tick++; state.videoFrames++ } else skippedRedraws++
    }

    /**
     * 和 [submitShared] 一样把原生内存包成 Skia 图，但**不动界面状态**（不换 `latest`、
     * 不记帧数、不触发重绘）——只给 [GstFrameCostTest][feedhub.app.video.GstFrameCostTest]
     * 量"取一帧要多久"用。
     */
    internal fun submitSharedForMeasurement(
        addr: Long,
        slot: Long,
        slotStride: Long,
        headerBytes: Int,
        w: Int,
        h: Int,
        stride: Int,
    ) {
        if (w <= 0 || h <= 0 || stride <= 0) return
        val pixelAddr = addr + slot * slotStride + headerBytes
        val data = Data.makeWithoutCopy(pixelAddr, stride * h, FRAME_OWNER_TOKEN)
        Image.makeRaster(
            ImageInfo(w, h, ColorType.BGRA_8888, ColorAlphaType.OPAQUE),
            data,
            stride,
        ).close()
        data.close()
    }

    /** 收一帧：[buf] 里是 BGRA，每行 [stride] 字节（≥ w*4），拷贝一份，调用方随后可以释放。
     * */
    fun submit(buf: ByteBuffer, w: Int, h: Int, stride: Int = w * 4) {
        val size = stride * h
        synchronized(lock) {
            if (closed) return
            if (pixels.size != size) pixels = ByteArray(size)
            val src = buf.duplicate()
            src.position(0)
            src.get(pixels, 0, minOf(size, src.remaining()))
            val img = Image.makeRaster(ImageInfo(w, h, ColorType.BGRA_8888, ColorAlphaType.OPAQUE), pixels, stride)
            latest?.close()
            latest = img
            frames++
        }
        lastSubmitNanos = System.nanoTime()
        pending.getAndSet(null)?.invoke()
        if (visible) { tick++; state.videoFrames++ } else skippedRedraws++
    }

    /** 把最新一帧编码成 PNG（验证 / 调试用）；还没有帧就 null。 */
    fun png(): ByteArray? = synchronized(lock) { latest?.encodeToData(EncodedImageFormat.PNG)?.bytes }

    fun close() {
        synchronized(lock) {
            closed = true
            latest?.close()
            latest = null
        }
    }

    /** 清掉当前画面（换了视频，黑屏等新的第一帧）。 */
    fun clear() {
        synchronized(lock) {
            latest?.close()
            latest = null
        }
        tick++
    }

    @Composable
    fun Draw(modifier: Modifier) {
        val shown = LocalWindowVisible.current
        SideEffect { setVisible(shown) }
        Canvas(modifier.background(Color.Black).graphicsLayer().onSizeChanged { onSurfaceSize?.invoke(it.width, it.height) }) {
            @Suppress("UNUSED_EXPRESSION") tick // 读一下，新的一帧来了才重画
            synchronized(lock) {
                val img = latest ?: return@Canvas
                val iw = img.width.toFloat()
                val ih = img.height.toFloat()
                // 画面比例用引擎报的真实尺寸；缓冲区整张图拉伸进这个比例里
                val aspect = if (state.videoWidth > 0 && state.videoHeight > 0) state.videoWidth.toFloat() / state.videoHeight else iw / ih
                val scale = minOf(size.width / aspect, size.height)
                val dw = scale * aspect
                val dh = scale
                val left = (size.width - dw) / 2
                val top = (size.height - dh) / 2
                drawIntoCanvas { c ->
                    c.skiaCanvas.drawImageRect(
                        img,
                        org.jetbrains.skia.Rect.makeWH(iw, ih),
                        org.jetbrains.skia.Rect.makeXYWH(left, top, dw, dh),
                        SamplingMode.LINEAR,
                        null,
                        true,
                    )
                }
            }
        }
    }

    companion object {
        /** 这么久内来过帧就算"画面在走"（最低 15 fps 的片子也够）。 */
        const val FRAME_GAP_NS = 100_000_000L

    }
}
