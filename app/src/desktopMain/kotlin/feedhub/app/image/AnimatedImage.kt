package feedhub.app.image

import androidx.compose.runtime.mutableIntStateOf
import coil3.Canvas
import coil3.Image
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.Codec
import org.jetbrains.skia.Data
import org.jetbrains.skia.FilterMipmap
import org.jetbrains.skia.FilterMode
import org.jetbrains.skia.MipmapMode
import org.jetbrains.skia.Rect
import org.jetbrains.skia.Image as SkiaImage
import java.util.concurrent.atomic.AtomicBoolean

/**
 * 会动的图（GIF / 动态 WebP），放在 Coil 的内存缓存里，由 [AnimationClock] 驱动。
 *
 * **画**：Coil 在 Compose 里把非位图的 [Image] 包成 `ImagePainter`，每次重画都调 [draw]。[draw] 里读了快照状态 [version]，
 * Compose 在绘制阶段追踪这种读取，状态一变就只重画这一层——所以各个调用点（`AsyncImage`）不用改。
 * [draw] 只管把当前帧 [current] 画上去并向时钟报到；换帧由时钟的线程在后台解好下一帧，再改 [version]。
 *
 * **内存**：内存缓存里放的是这个对象——原始文件的字节（Skia 的 `Codec` 持有）+ 第一帧的缩略图 [poster]，几百 KB 到几 MB，
 * 不是每一帧都解成像素。要显示哪一帧才解哪一帧（GIF 的帧依赖前一帧：[Codec.readPixels] 带 `priorFrame`，顺序播放时在
 * 一块工作缓冲里接着上一帧解，不用从头来），解出来的帧缩到显示大小，少量缓存在 [cache]（超过 [CACHE_BYTES] 丢最久没用的）；
 * 离开屏幕一会儿（[AnimationClock] 不再被它的 [draw] 叫醒）就把帧缓存和工作缓冲全放掉，只剩 poster。
 *
 * 线程：[draw] 在界面线程；解帧在 [AnimationClock] 的解码线程；两边共用的 [current] 和 [cache] 由 [lock] 保护——画的时候持锁，
 * 被淘汰的帧也在持锁时 close，所以不会画到一张已经释放的图。解码器本身（[codec]、[work]）由 [decodeLock] 保护。
 */
internal class AnimatedImage private constructor(
    private val codec: Codec,
    val format: AnimatedFormat,
    val timeline: FrameTimeline,
    private val requiredFrames: IntArray,
    /** 给 Coil / Painter 看的大小（和静态图一样：按请求的大小缩放后的尺寸，保持宽高比）。 */
    override val width: Int,
    override val height: Int,
    /** 实际解出来的帧的大小（<= [width] x [height]，不放大；画的时候再拉伸）。 */
    private val frameWidth: Int,
    private val frameHeight: Int,
    val sourceWidth: Int,
    val sourceHeight: Int,
    private val poster: SkiaImage,
    private val encodedBytes: Long,
) : Image {

    override val size: Long get() = encodedBytes + frameWidth.toLong() * frameHeight * 4 + OVERHEAD_BYTES

    /** 动图在多个地方同时显示（信息流卡片 + 双栏的详情）时共用同一份解码结果，所以可以共享。 */
    override val shareable: Boolean get() = true

    val frameCount: Int get() = timeline.frameCount

    // ---------- 画 ----------

    /** 每显示出一张新的帧加 1。[draw] 里读它，所以 Compose 会在它变的时候重画画了这张图的那一层。 */
    private val version = mutableIntStateOf(0)

    private val lock = Any()

    /** 现在要画的那一帧（只在持 [lock] 时读写）。一开始是 [poster]；poster 不在 [cache] 里，也永远不会被释放。 */
    private var current: SkiaImage = poster

    /** 已解出来的帧（访问顺序，最久没用的在前）。只在持 [lock] 时读写。 */
    private val cache = LinkedHashMap<Int, SkiaImage>(16, 0.75f, true)
    private var cacheBytes = 0L

    @Volatile
    var shownIndex = 0
        private set

    override fun draw(canvas: Canvas) {
        version.intValue // 读一下，让 Compose 追踪（状态变了会重画）
        AnimationClock.onDraw(this)
        synchronized(lock) {
            val img = current
            canvas.drawImageRect(
                img,
                Rect.makeWH(img.width.toFloat(), img.height.toFloat()),
                Rect.makeWH(width.toFloat(), height.toFloat()),
                SAMPLING,
                null,
                true,
            )
        }
    }

    // ---------- 解码 ----------

    private val decodeLock = Any()

    /** 工作缓冲：原始大小，装着最近解出来的那一帧（合成好的完整画面）。顺序播放时下一帧接着它解。 */
    private var work: Bitmap? = null
    private var workIndex = -1

    @Volatile
    private var closed = false

    /** 解不出来（文件坏了）就不再尝试，停在当前帧。 */
    @Volatile
    var broken = false
        private set

    /** 解第 [index] 帧并显示出来（改 [current]、[version]）；已经缓存了就不用再解。在解码线程上调。 */
    private fun show(index: Int) {
        val img = frame(index) ?: return
        synchronized(lock) {
            if (closed) return
            current = img
            shownIndex = index
            evict()
        }
        val now = AnimationClock.now()
        // 上一次换帧界面已经重画过了（lastDraw >= lastBump）：从这一次开始计"界面有多久没重画"
        if (lastDrawNs >= lastBumpNs) unackedSinceNs = now
        lastBumpNs = now
        version.intValue += 1
    }

    /** 第 [index] 帧（缩到显示大小的）；没缓存就解。返回的图可能随后被淘汰释放，所以只在持锁时用，或者交给 [show]。 */
    private fun frame(index: Int): SkiaImage? {
        synchronized(lock) {
            if (closed) return null
            cache[index]?.let { return it }
        }
        val made = synchronized(decodeLock) {
            if (closed) return null
            decodeFrame(index)
        }
        synchronized(lock) {
            if (closed) { made.close(); return null }
            val existing = cache[index]
            if (existing != null) {
                made.close()
                return existing
            }
            cache[index] = made
            cacheBytes += bytesOf(made)
            return made
        }
    }

    /** 提前解好下一帧放进缓存，到点了直接用。 */
    private fun prefetch(index: Int) {
        if (index == shownIndex || broken || closed) return
        synchronized(lock) { if (cache.containsKey(index)) return }
        val made = synchronized(decodeLock) {
            if (closed) return
            decodeFrame(index)
        }
        synchronized(lock) {
            if (closed || cache.containsKey(index)) { made.close(); return }
            cache[index] = made
            cacheBytes += bytesOf(made)
            evict()
        }
    }

    /** 持 [lock] 时调：缓存超过 [CACHE_BYTES]（至少留 3 帧）就丢最久没用的，不丢正在显示的那张。 */
    private fun evict() {
        val limit = maxOf(CACHE_BYTES, 3L * frameWidth * frameHeight * 4)
        if (cacheBytes <= limit) return
        val it = cache.entries.iterator()
        while (it.hasNext() && cacheBytes > limit) {
            val e = it.next()
            if (e.value === current) continue
            it.remove()
            cacheBytes -= bytesOf(e.value)
            e.value.close()
        }
    }

    /** 解第 [index] 帧，缩到 [frameWidth] x [frameHeight]。持 [decodeLock] 时调。 */
    private fun decodeFrame(index: Int): SkiaImage {
        val bmp = work ?: Bitmap().also {
            check(it.allocN32Pixels(sourceWidth, sourceHeight)) { "分配 ${sourceWidth}x$sourceHeight 的工作缓冲失败" }
            work = it
        }
        // 这一帧依赖的那一帧正好在工作缓冲里（顺序播放时的常态）就接着它解；否则让 Skia 自己从依赖链上头开始解
        val required = requiredFrames[index]
        val prior = if (required >= 0 && required == workIndex) required else -1
        workIndex = -1
        codec.readPixels(bmp, index, prior)
        workIndex = index
        return snapshot(bmp)
    }

    /** 把工作缓冲里的画面拷成一张不可变的图，需要的话缩小到显示大小。 */
    private fun snapshot(bmp: Bitmap): SkiaImage {
        if (frameWidth == sourceWidth && frameHeight == sourceHeight) return SkiaImage.makeFromBitmap(bmp)
        val dst = Bitmap()
        check(dst.allocN32Pixels(frameWidth, frameHeight)) { "分配 ${frameWidth}x$frameHeight 的帧失败" }
        val ok = bmp.peekPixels()?.use { src -> dst.peekPixels()?.use { src.scalePixels(it, DOWNSCALE) } } ?: false
        check(ok) { "缩放帧失败" }
        dst.setImmutable()
        return SkiaImage.makeFromBitmap(dst)
    }

    private fun bytesOf(img: SkiaImage): Long = img.width.toLong() * img.height * 4

    // ---------- 播放（由 AnimationClock 的线程调）----------

    /** 界面线程最近一次 [draw] 的时刻（[AnimationClock.now] 的时间）。 */
    @Volatile
    var lastDrawNs = 0L

    /** 最近一次换帧的时刻。 */
    @Volatile
    private var lastBumpNs = 0L

    /** 换了帧但界面一直没重画，从什么时候开始的（只有 lastDrawNs < lastBumpNs 时有意义）。 */
    @Volatile
    private var unackedSinceNs = 0L

    /** 已经播完（有限次循环的动图），停在最后一帧，不再需要时钟。 */
    @Volatile
    var finished = false
        private set

    private var positionNs = 0L
    private var lastStepNs = 0L

    /** 现在时间轴上该显示的帧。 */
    @Volatile
    private var wantedIndex = 0

    private val decoding = AtomicBoolean(false)

    /**
     * 时钟每转一圈调一次。推进播放位置，必要时安排解下一帧。
     * 返回离下一次要叫它还有几毫秒；[AnimationClock.STOP] 表示不用再跟踪它了。
     *
     * @param inline 在调用线程上同步解帧（测试和截图工具用，要的是确定的结果）；平时在解码线程上异步解。
     */
    fun step(now: Long, inline: Boolean): Long {
        if (closed || broken) return AnimationClock.STOP
        if (!isVisible(now)) {
            // 一段时间没被画了：放掉解出来的东西，回到第一帧。解帧还在进行的话下一圈再来
            return if (release()) AnimationClock.STOP else IDLE_RETRY_MS
        }
        if (lastStepNs == 0L) lastStepNs = now
        positionNs += (now - lastStepNs).coerceIn(0L, MAX_STEP_NS)
        lastStepNs = now
        val posMs = positionNs / 1_000_000
        val wanted = timeline.frameAt(posMs)
        if (wanted != wantedIndex) {
            wantedIndex = wanted
            requestShow(inline)
        }
        if (timeline.isFinished(posMs)) {
            // 播完了：等最后一帧显示出来就收工（帧缓存放掉，只留正在显示的）
            if (shownIndex == wanted && !decoding.get()) {
                finished = true
                trimToCurrent()
                return AnimationClock.STOP
            }
            return DECODE_WAIT_MS
        }
        if (decoding.get()) return DECODE_WAIT_MS
        return timeline.msUntilNextFrame(posMs)
    }

    private fun isVisible(now: Long): Boolean {
        // 换了帧之后界面线程重画过（lastDrawNs 不早于换帧时刻）就是还在屏幕上；一直没重画的给一点时间（界面卡一下、窗口刚好在重排），
        // 从第一次没人理的换帧算起，超过 VISIBLE_GRACE_NS 还没人画就当离屏了
        return lastDrawNs >= lastBumpNs || now - unackedSinceNs <= VISIBLE_GRACE_NS
    }

    private fun requestShow(inline: Boolean) {
        if (!decoding.compareAndSet(false, true)) return // 已经有任务在解了，它解完会再看 wantedIndex
        val task = Runnable {
            try {
                while (true) {
                    val target = wantedIndex
                    if (target == shownIndex) break
                    show(target)
                    prefetch((target + 1) % frameCount)
                }
            } catch (t: Throwable) {
                broken = true
                System.err.println("动图解码失败，停在当前帧：$t")
            } finally {
                decoding.set(false)
            }
            // 放掉标志之后 wantedIndex 可能又变了
            if (!broken && wantedIndex != shownIndex) requestShow(inline)
            AnimationClock.wake()
        }
        if (inline) task.run() else AnimationClock.executeDecode(task)
    }

    /** 离开屏幕：放掉帧缓存和工作缓冲，回到第一帧、时间归零。解帧在进行中返回 false（稍后再试）。 */
    private fun release(): Boolean {
        if (!decoding.compareAndSet(false, true)) return false
        try {
            synchronized(decodeLock) {
                work?.close()
                work = null
                workIndex = -1
            }
            synchronized(lock) {
                for (img in cache.values) img.close()
                cache.clear()
                cacheBytes = 0
                current = poster
                shownIndex = 0
            }
            wantedIndex = 0
            positionNs = 0
            lastStepNs = 0
            lastBumpNs = 0
            unackedSinceNs = 0
        } finally {
            decoding.set(false)
        }
        return true
    }

    /** 播完之后：帧缓存只留正在显示的那张，工作缓冲放掉。 */
    private fun trimToCurrent() {
        synchronized(decodeLock) {
            work?.close()
            work = null
            workIndex = -1
        }
        synchronized(lock) {
            val it = cache.entries.iterator()
            while (it.hasNext()) {
                val e = it.next()
                if (e.value === current) continue
                it.remove()
                e.value.close()
            }
            cacheBytes = cache.values.sumOf { bytesOf(it) }
        }
    }

    /** 当前缓存了几帧（测试用）。 */
    val cachedFrames: Int get() = synchronized(lock) { cache.size }

    /** 当前有没有工作缓冲（测试用）。 */
    val hasWorkBuffer: Boolean get() = synchronized(decodeLock) { work != null }

    /** 彻底放掉（测试和工具用；正常运行时靠 GC，Coil 的内存缓存不会通知）。 */
    fun close() {
        closed = true
        synchronized(decodeLock) { work?.close(); work = null; workIndex = -1 }
        synchronized(lock) { for (img in cache.values) img.close(); cache.clear(); cacheBytes = 0 }
        codec.close()
    }

    companion object {
        /** 每张动图最多缓存这么多字节的帧。 */
        const val CACHE_BYTES = 8L * 1024 * 1024

        /** 帧的像素数上限（超过的缩小），免得几千像素的大图一帧就占几十 MB。 */
        const val MAX_FRAME_PIXELS = 4_000_000

        private const val OVERHEAD_BYTES = 4096L
        private const val MAX_STEP_NS = 2_000_000_000L
        private const val VISIBLE_GRACE_NS = 1_000_000_000L
        private const val IDLE_RETRY_MS = 20L
        private const val DECODE_WAIT_MS = 8L

        private val SAMPLING = org.jetbrains.skia.SamplingMode.LINEAR
        private val DOWNSCALE = FilterMipmap(FilterMode.LINEAR, MipmapMode.NEAREST)

        /**
         * 从文件字节创建；帧数不足 2（其实是静态图）或者解不出来返回 null。
         * [sizeFor] 按原图大小算出给 Coil 看的大小（调用方按请求的大小算好，保持宽高比）。
         */
        fun create(bytes: ByteArray, format: AnimatedFormat, sizeFor: (srcW: Int, srcH: Int) -> Pair<Int, Int>): AnimatedImage? {
            val codec = Codec.makeFromData(Data.makeFromBytes(bytes))
            var ok = false
            try {
                if (codec.frameCount < 2) return null
                val infos = codec.framesInfo
                val timeline = FrameTimeline(
                    IntArray(infos.size) { FrameTimeline.normalizeDelay(infos[it].duration, format) },
                    FrameTimeline.playsOf(codec.repetitionCount),
                )
                val srcW = codec.width
                val srcH = codec.height
                val (w, h) = sizeFor(srcW, srcH)
                // 帧不放大（画的时候再拉伸），像素太多就缩小
                var fw = minOf(w, srcW).coerceAtLeast(1)
                var fh = minOf(h, srcH).coerceAtLeast(1)
                if (fw.toLong() * fh > MAX_FRAME_PIXELS) {
                    val k = Math.sqrt(MAX_FRAME_PIXELS.toDouble() / (fw.toLong() * fh))
                    fw = (fw * k).toInt().coerceAtLeast(1)
                    fh = (fh * k).toInt().coerceAtLeast(1)
                }
                val image = AnimatedImage(
                    codec, format, timeline, IntArray(infos.size) { infos[it].requiredFrame },
                    w.coerceAtLeast(1), h.coerceAtLeast(1), fw, fh, srcW, srcH,
                    poster = posterOf(codec, srcW, srcH, fw, fh),
                    encodedBytes = bytes.size.toLong(),
                )
                ok = true
                return image
            } finally {
                if (!ok) codec.close()
            }
        }

        /** 第 0 帧缩到显示大小（用完工作缓冲就放掉，播放时再按需分配）。 */
        private fun posterOf(codec: Codec, srcW: Int, srcH: Int, fw: Int, fh: Int): SkiaImage {
            val bmp = Bitmap()
            check(bmp.allocN32Pixels(srcW, srcH)) { "分配 ${srcW}x$srcH 失败" }
            try {
                codec.readPixels(bmp, 0, -1)
                if (fw == srcW && fh == srcH) return SkiaImage.makeFromBitmap(bmp)
                val dst = Bitmap()
                check(dst.allocN32Pixels(fw, fh)) { "分配 ${fw}x$fh 失败" }
                val ok = bmp.peekPixels()?.use { src -> dst.peekPixels()?.use { src.scalePixels(it, DOWNSCALE) } } ?: false
                check(ok) { "缩放第一帧失败" }
                dst.setImmutable()
                return SkiaImage.makeFromBitmap(dst)
            } finally {
                bmp.close()
            }
        }
    }
}
