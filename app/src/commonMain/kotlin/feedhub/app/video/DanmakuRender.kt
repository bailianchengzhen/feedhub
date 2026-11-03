package feedhub.app.video

import androidx.compose.ui.unit.IntSize
import feedhub.core.Danmaku
import kotlin.math.ceil

/**
 * 安卓弹幕层的"画什么、什么时候画"——纯逻辑，不碰任何 `android.*` / Compose 绘制 API，好写单元测试
 * （`DanmakuRenderLoopTest`：暂停 / 后台 / 关弹幕时不注册 vsync、跳转重排、释放）。
 *
 * 分工：
 * - **画线程**（安卓是一个自己的 `HandlerThread`）上跑 [DanmakuRenderLoop]：[DanmakuScheduler] 排轨道、算位置，
 *   [DanmakuSpriteSource] 给每条弹幕预先画好的精灵（同一条只画一次），[DanmakuPainter] 把这一帧真的画到某块画布上。
 * - **vsync** 由 [DanmakuFrameDriver] 提供（安卓 = 画线程上的 `Choreographer`；测试 = 手动 `fire`）。
 * - **状态**（在播吗、倍速、设置、播放位置）每帧从 [DanmakuFrameInput] 读，字段都是 `@Volatile`：
 *   界面线程写、画线程读，**不读 Compose 状态**（`PlayerState` 另有一份只读普通字段的时钟，见 `renderNowMs`）。
 *
 * 什么时候不画：不在播 / 缓冲 / 窗口看不见（[DanmakuFrameInput.running] 为 false）→ 画完手上这一帧就**不再注册**
 * 下一帧回调（不空转）；暂停时只有"跳转 / 换设置 / 尺寸变了"这类一次性请求（[invalidate]）才画一帧。
 */

/** 一条弹幕预先画好的图（安卓 = `android.graphics.Bitmap`，测试 = 随便什么）。 */
internal interface DanmakuSpriteSource<S> {
    /** 精灵四周留出的边（放得下描边，和原来 Compose 那份一样加 1 像素给抗锯齿）。 */
    val pad: Float

    /** 换字号 / 字重 / 描边（像素，外沿可见的宽度）；和现在的一样就什么都不做，不一样精灵缓存作废。 */
    fun configure(fontPx: Float, weight: Int, strokePx: Float)

    /** 文字本身的宽度（像素），排轨道用。 */
    fun width(d: Danmaku): Float

    /** 这条弹幕的图；同一条（文字 + 颜色 + 字号）只画一次。 */
    fun sprite(d: Danmaku): S

    /** 精灵的高度（像素）。 */
    fun height(s: S): Float
}

/** 把一帧画到某块画布上（安卓 = `SurfaceView` 的 `lockHardwareCanvas`；测试 = 记下画了些什么）。 */
internal interface DanmakuPainter<S> {
    /** 开始一帧（清空上一帧）。画布现在用不了（surface 没了 / 被别处锁着）就返回 false，这一帧什么都不画。 */
    fun beginFrame(widthPx: Float, heightPx: Float): Boolean

    fun draw(sprite: S, left: Float, top: Float, alpha: Float)

    /** 收尾（安卓上 = `unlockCanvasAndPost`），[beginFrame] 返回 true 时**一定**会调到。 */
    fun endFrame()
}

/** 一帧 vsync 的回调（参数是 vsync 的时间戳，`System.nanoTime()` 同一个时基）。 */
internal fun interface DanmakuFrameCallback {
    fun onFrame(frameTimeNanos: Long)
}

/** 下一帧从哪来（安卓 = 画线程上的 `Choreographer`）。 */
internal interface DanmakuFrameDriver {
    fun post(callback: DanmakuFrameCallback)

    fun cancel(callback: DanmakuFrameCallback)
}

/**
 * 每帧读的输入。界面线程写、画线程读，所以全是 `@Volatile` 的普通字段——**不能放 Compose 状态进来**
 * （那要从别的线程读快照，见 `PlayerState.renderNowMs` 的注释）。
 */
internal class DanmakuFrameInput {
    /**
     * "该连续要帧吗" + "暂停时画在哪（毫秒）"：**这两个一起写、一起读**（[setPlayback] 一次写完）。
     * 分开写的话，刚恢复播放的那一帧可能拿到"`running=true` + 上一次暂停的旧位置"，弹幕会闪一下回到老位置。
     */
    @Volatile
    private var playback = Playback(false, 0L)

    /** 该连续要帧吗：在播 && 没在缓冲 && 窗口看得见（和原来 Compose 那份的 `running` 一个意思）。 */
    var running: Boolean
        get() = playback.running
        set(value) = setPlayback(value, playback.pausedAtMs)

    /** 暂停 / 缓冲时画在哪（毫秒）。 */
    val pausedAtMs: Long get() = playback.pausedAtMs

    /** 原子地更新"要不要跑"和"暂停在哪"（[running] / [pausedAtMs] 是一次写入，画线程不会读到半新半旧）。 */
    fun setPlayback(running: Boolean, pausedAtMs: Long) {
        playback = Playback(running, pausedAtMs)
    }

    private class Playback(val running: Boolean, val pausedAtMs: Long)

    /** 倍速（决定新出现的滚动弹幕走多快，见 [DanmakuDefaults.scrollMs]）。 */
    @Volatile
    var speed: Float = 1f

    /** 弹幕设置（透明度、字号、区域、过滤……）。过滤在界面线程交给 [DanmakuTimeline.setFilter]。 */
    @Volatile
    var settings: DanmakuSettings = DanmakuSettings()

    /** 每 dp 多少像素（`DanmakuSettings.layout` 要它换算字号 / 描边）。 */
    @Volatile
    var density: Float = 1f

    /** 这一帧的播放位置（毫秒）：在播时按 [PlayerState] 的插值算，暂停时是一个定值。 */
    @Volatile
    var clockMs: (Long) -> Long = { 0L }
}

/**
 * 弹幕画循环。**所有方法都在画线程上调用**（安卓那边把界面线程的更新 post 到画线程，见 `DanmakuSurfaceView`）。
 *
 * @param surfaceSize 画布尺寸（像素）；surface 还没建好 / 已经没了就返回 null，这时一帧都不画（但该要的 vsync 照要，
 *   等 surface 回来就能立刻接上）。
 */
internal class DanmakuRenderLoop<S>(
    private val timeline: DanmakuTimeline,
    private val sprites: DanmakuSpriteSource<S>,
    private val painter: DanmakuPainter<S>,
    private val driver: DanmakuFrameDriver,
    private val input: DanmakuFrameInput,
    private val surfaceSize: () -> IntSize?,
) {
    private val scheduler = DanmakuScheduler(timeline) { sprites.width(it) }
    private val callback = DanmakuFrameCallback { onFrame(it) }

    private var scheduled = false
    private var released = false

    /** 画出来的帧数（调试 / 测试用）。 */
    var frames = 0
        private set

    /** 最近一帧用的播放位置（毫秒）；还没画过是 [Long.MIN_VALUE]。 */
    var lastFrameMs = Long.MIN_VALUE
        private set

    /**
     * 要一帧：暂停时的跳转、改了设置、surface 回来 / 尺寸变了都走它。已经在等下一帧就什么都不做（合并）。
     * 在播时不调用也行——每一帧画完自己会再要下一帧。
     */
    fun invalidate() {
        if (released || scheduled) return
        scheduled = true
        driver.post(callback)
    }

    /** vsync 到了（画线程）。画一帧；还在播就接着要下一帧，否则**到此为止**（不空转）。 */
    fun onFrame(frameTimeNanos: Long) {
        scheduled = false
        if (released) return
        draw(frameTimeNanos)
        if (!released && input.running) invalidate()
    }

    /** 释放：撤掉还没来的回调，之后 [invalidate] / [onFrame] 都不再画。 */
    fun release() {
        if (released) return
        released = true
        if (scheduled) {
            scheduled = false
            driver.cancel(callback)
        }
    }

    private fun draw(frameTimeNanos: Long) {
        val size = surfaceSize() ?: return
        val width = size.width.toFloat()
        val height = size.height.toFloat()
        if (width < 1f || height < 1f) return
        val settings = input.settings
        val now = input.clockMs(frameTimeNanos)
        val layout = settings.layout(width, height, input.density)
        timeline.setFilter(settings.filter)
        sprites.configure(layout.fontPx, layout.fontWeight, settings.strokeWidth * input.density)
        scheduler.configure(width, height, layout)
        scheduler.playbackSpeed = input.speed
        if (!painter.beginFrame(width, height)) return
        try {
            val visible = scheduler.advance(now)
            val pad = sprites.pad
            for (a in visible) {
                val sprite = sprites.sprite(a.danmaku)
                painter.draw(
                    sprite,
                    a.x(now, width) - pad,
                    scheduler.y(a) + (scheduler.laneHeight - sprites.height(sprite)) / 2f,
                    settings.opacity,
                )
            }
            frames++
            lastFrameMs = now
        } finally {
            painter.endFrame()
        }
    }
}

/** 描边的留边（和 [DanmakuSprites.pad] 同一个算法，安卓那份精灵也用）。 */
internal fun spritePad(strokePx: Float): Float = ceil(strokePx) + 1f
