package feedhub.app.video

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.PorterDuff
import android.graphics.Typeface
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.view.Choreographer
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.ui.unit.IntSize
import feedhub.core.Danmaku
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.ceil
import kotlin.math.roundToInt

/**
 * 弹幕自己的那块画布：一块**透明的 `SurfaceView`**，用 `setZOrderMediaOverlay(true)` 夹在
 * 视频的 `SurfaceView`（media，在窗口下面）和 Compose 画的控件（窗口表面）**之间**——
 * 于是三层从上到下是"控件 / 弹幕 / 视频"，系统自己叠，App 主窗口一帧都不用重画。
 *
 * 画在**自己的线程**上（[HandlerThread] + 那个线程上的 `Choreographer`），每帧 `lockHardwareCanvas()`，
 * 画完 `unlockCanvasAndPost`。屏幕 120Hz 就 120 帧——**不降刷新率、不降帧**。
 *
 * 生命周期（见 [DanmakuRenderLoop]，那边是纯逻辑、有单测）：
 * - 在播 && 没缓冲 && 窗口看得见 → 每帧画完再要下一帧；
 * - 暂停 / 缓冲 / 看不见 → 画完手上这一帧就**不再注册 vsync**（不空转）；跳转 / 改设置 / surface 回来时画一帧；
 * - [release]：撤掉回调、停线程、交还 Surface。
 */
internal class DanmakuSurfaceView(
    context: Context,
    timeline: DanmakuTimeline,
    private val state: PlayerState,
    density: Float,
) : SurfaceView(context), SurfaceHolder.Callback {

    private val input = DanmakuFrameInput()
    private val sprites = AndroidDanmakuSprites()
    private val painter = HardwareCanvasPainter(this)

    @Volatile
    private var destroyed = false

    /** 画线程可以往 surface 上画吗（surfaceCreated ~ surfaceDestroyed 之间）。 */
    @Volatile
    private var surfaceReady = false

    private var surfaceWidth = 0
    private var surfaceHeight = 0

    private val thread = HandlerThread("feedhub-danmaku")
    private val handler: Handler
    private var loop: DanmakuRenderLoop<Bitmap>? = null

    init {
        input.density = density
        // 在播用 PlayerState 的插值（只读 @Volatile 字段，见 renderNowMs）；暂停 / 缓冲时用界面推过来的定值。
        input.clockMs = { nanos -> if (input.running) state.renderNowMs(nanos) else input.pausedAtMs }
        // 透明：只有弹幕文字有像素，别的都是透明的，视频从这里透上来。
        holder.setFormat(PixelFormat.TRANSLUCENT)
        // 在视频的 SurfaceView 之上、窗口表面之下（窗口里的 Compose 控件照样盖在弹幕上面）。
        setZOrderMediaOverlay(true)
        isFocusable = false
        isClickable = false
        holder.addCallback(this)
        thread.start()
        handler = Handler(thread.looper)
        handler.post {
            loop = DanmakuRenderLoop(
                timeline = timeline,
                sprites = sprites,
                painter = painter,
                driver = ChoreographerFrameDriver(),
                input = input,
                surfaceSize = {
                    if (surfaceReady && surfaceWidth > 0 && surfaceHeight > 0) IntSize(surfaceWidth, surfaceHeight) else null
                },
            )
        }
    }

    /**
     * 界面线程：把这一帧要用的值推给画线程。
     *
     * 只在这里 post 一次"画一帧"的请求（[DanmakuRenderLoop.invalidate] 自己会合并重复请求）——**每秒 0~几次**，
     * 不是每帧一次；连续播放时是画线程自己按 vsync 一轮一轮要的。
     */
    fun update(running: Boolean, speed: Float, settings: DanmakuSettings, density: Float, pausedAtMs: Long) {
        if (destroyed) return
        input.speed = speed
        input.settings = settings
        input.density = density
        // 一次写完：画线程读到的"要不要跑 / 暂停在哪"一定是同一份
        input.setPlayback(running, pausedAtMs)
        handler.post { loop?.invalidate() }
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        // 尺寸在 surfaceChanged 里给；这里只放行（有的机型 surfaceCreated 之后才来 surfaceChanged）
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        surfaceWidth = width
        surfaceHeight = height
        surfaceReady = true
        handler.post { loop?.invalidate() }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // Surface 在这个回调**返回之后**就被销毁了：先让画线程把手上这一帧收尾（同一线程的消息排在当前帧之后）
        surfaceReady = false
        val done = CountDownLatch(1)
        handler.post {
            painter.surfaceReady = false
            done.countDown()
        }
        done.await(500, TimeUnit.MILLISECONDS)
    }

    /** 界面线程：释放（离开播放器 / 关掉弹幕时）。之后再调 [update] 什么都不做。 */
    fun release() {
        if (destroyed) return
        destroyed = true
        holder.removeCallback(this)
        val done = CountDownLatch(1)
        handler.post {
            loop?.release()
            painter.surfaceReady = false
            done.countDown()
        }
        done.await(1, TimeUnit.SECONDS)
        loop = null
        thread.quitSafely()
    }
}

/**
 * 画线程上的 vsync：`Choreographer` 建在**画线程自己的 Looper** 上（不是主线程），所以弹幕的帧和 App 主窗口
 * 的帧是两回事——主线程不出帧，弹幕照样按屏幕刷新率动。
 */
internal class ChoreographerFrameDriver : DanmakuFrameDriver {
    private val choreographer: Choreographer = Choreographer.getInstance()
    private var callback: DanmakuFrameCallback? = null
    private var frame: Choreographer.FrameCallback? = null

    override fun post(callback: DanmakuFrameCallback) {
        // 已经在等下一帧了：换掉要调的那个（同一个 driver 只有一帧在飞）
        if (frame != null) {
            this.callback = callback
            return
        }
        this.callback = callback
        val fc = object : Choreographer.FrameCallback {
            override fun doFrame(frameTimeNanos: Long) {
                frame = null
                val c = this@ChoreographerFrameDriver.callback
                this@ChoreographerFrameDriver.callback = null
                c?.onFrame(frameTimeNanos)
            }
        }
        frame = fc
        choreographer.postFrameCallback(fc)
    }

    override fun cancel(callback: DanmakuFrameCallback) {
        if (this.callback !== callback) return
        this.callback = null
        frame?.let {
            choreographer.removeFrameCallback(it)
            frame = null
        }
    }
}

/**
 * 画线程：把一帧画到 `SurfaceView` 的硬件画布上（`lockHardwareCanvas`），每帧先清成全透明。
 * Surface 用不了（还没建好 / 已经销毁）时 [beginFrame] 返回 false，这一帧什么都不画。
 */
internal class HardwareCanvasPainter(private val view: SurfaceView) : DanmakuPainter<Bitmap> {
    @Volatile
    var surfaceReady = true

    private var canvas: Canvas? = null
    private val paint = Paint()

    override fun beginFrame(widthPx: Float, heightPx: Float): Boolean {
        if (!surfaceReady) return false
        val c = try {
            view.holder.lockHardwareCanvas()
        } catch (_: RuntimeException) {
            return false // surface 正在销毁 / 被别处锁着：这一帧跳过，下一帧再试
        }
        canvas = c
        c.drawColor(Color.TRANSPARENT, PorterDuff.Mode.CLEAR)
        return true
    }

    override fun draw(sprite: Bitmap, left: Float, top: Float, alpha: Float) {
        val c = canvas ?: return
        val a = (alpha.coerceIn(0f, 1f) * 255f).roundToInt()
        if (a <= 0) return
        paint.alpha = a
        c.drawBitmap(sprite, left, top, paint)
    }

    override fun endFrame() {
        val c = canvas ?: return
        canvas = null
        try {
            view.holder.unlockCanvasAndPost(c)
        } catch (_: RuntimeException) {
            // surface 已经没了：这一帧丢掉就行
        }
    }
}

/**
 * 弹幕精灵（安卓版）：每条弹幕（文字 + 颜色 + 字号）第一次要画时用 `android.graphics.Paint` 排版一次、
 * 连描边画进一张 [Bitmap]（ARGB_8888），之后每帧只是 `drawBitmap`（硬件画布会把同一张图缓存成纹理）。
 * LRU，最多 [MAX] 张；字号、字重、描边变了（窗口缩放、改设置）就清空重来。
 *
 * 和桌面那份 Compose 精灵画出来的一样：同一个系统字体（`Typeface.DEFAULT` + 字重）、同样的字号换算
 * （`DanmakuSettings.layout` 给的是像素）、同样的描边（外沿 [strokePx] 宽：先按 `strokePx * 2` 描一圈、
 * 再用文字盖住里面一半）、同样"太暗的字配白描边、其余黑描边"。
 */
internal class AndroidDanmakuSprites : DanmakuSpriteSource<Bitmap> {
    @Volatile
    private var fontPx = 20f
    private var weight = 4
    private var strokePx = 2f

    @Volatile
    private var cachedTypeface: Typeface = buildTypeface(4)

    override var pad: Float = spritePad(2f)
        private set

    // 测宽由时间线在后台 / 界面线程与画线程分别调用，独立一把 Paint + 锁，不和画线程共用（防 B1 竞争）
    private val widthLock = Any()
    private val widthPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.LEFT
        isSubpixelText = true
    }

    // 以下三个 Paint 只在弹幕画线程（render）里使用
    private val renderMeasure = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.LEFT
        isSubpixelText = true
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.LEFT
        isSubpixelText = true
        style = Paint.Style.FILL
    }
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.LEFT
        isSubpixelText = true
        style = Paint.Style.STROKE
        strokeJoin = Paint.Join.ROUND
    }

    override fun configure(fontPx: Float, weight: Int, strokePx: Float) {
        if (fontPx == this.fontPx && weight == this.weight && strokePx == this.strokePx) return
        if (weight != this.weight) {
            cachedTypeface = buildTypeface(weight)
        }
        this.fontPx = fontPx
        this.weight = weight
        this.strokePx = strokePx
        pad = spritePad(strokePx)
        cache.clear()
    }

    private data class Key(val text: String, val color: Int, val size: Int)

    private val cache = object : LinkedHashMap<Key, Bitmap>(256, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Key, Bitmap>?) = size > MAX
    }

    /** 每条弹幕自己的字号系数（和桌面那份一样：按弹幕自带的字号等级 25 为基准，0.6~1.6 倍）。 */
    private fun sizeFactor(d: Danmaku): Float = (d.fontSize / 25f).coerceIn(0.6f, 1.6f)

    private fun sizeOf(d: Danmaku): Float = fontPx * sizeFactor(d)

    private fun prepare(p: Paint, size: Float, tf: Typeface) {
        p.textSize = size
        p.typeface = tf
    }

    override fun width(d: Danmaku): Float = synchronized(widthLock) {
        prepare(widthPaint, sizeOf(d), cachedTypeface)
        widthPaint.measureText(d.text)
    }

    override fun sprite(d: Danmaku): Bitmap {
        val size = sizeOf(d)
        val key = Key(d.text, d.color, size.toInt())
        return cache.getOrPut(key) { render(d, size, key.color) }
    }

    override fun height(s: Bitmap): Float = s.height.toFloat()

    private fun render(d: Danmaku, size: Float, keyColor: Int): Bitmap {
        val p = pad
        val tf = cachedTypeface
        prepare(renderMeasure, size, tf)
        val textWidth = renderMeasure.measureText(d.text)
        val fm = renderMeasure.fontMetrics
        val textHeight = fm.descent - fm.ascent
        val w = ceil(textWidth + 2 * p).toInt().coerceAtLeast(1)
        val h = ceil(textHeight + 2 * p).toInt().coerceAtLeast(1)
        val bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val rgb = keyColor and 0xFFFFFF
        val fillColor = (0xFF000000.toInt()) or rgb
        // 太暗的文字配白色描边，其余黑色（和桌面那份同一个判据）
        val luminance = (0.299 * (rgb shr 16 and 0xFF) + 0.587 * (rgb shr 8 and 0xFF) + 0.114 * (rgb and 0xFF)) / 255
        val outline = if (luminance < 0.2) Color.WHITE else Color.BLACK
        val baseline = p - fm.ascent
        // 描边画在文字轮廓的两侧各一半，之后文字盖在上面，露在外面的就是 strokePx 宽
        if (strokePx > 0f) {
            prepare(stroke, size, tf)
            stroke.strokeWidth = strokePx * 2
            stroke.color = outline
            canvas.drawText(d.text, p, baseline, stroke)
        }
        prepare(fill, size, tf)
        fill.color = fillColor
        canvas.drawText(d.text, p, baseline, fill)
        return bitmap
    }

    private companion object {
        const val MAX = 400

        fun buildTypeface(weight: Int): Typeface =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                Typeface.create(Typeface.DEFAULT, (weight * 100).coerceIn(1, 1000), false)
            } else {
                // API 26 / 27 没有带 weight 的 create：粗体就按 600 分界（Compose 在这两个版本上也是这么近似的）
                Typeface.create(Typeface.DEFAULT, if (weight >= 6) Typeface.BOLD else Typeface.NORMAL)
            }
    }
}
