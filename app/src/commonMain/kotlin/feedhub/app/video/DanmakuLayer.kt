package feedhub.app.video

import androidx.compose.foundation.Canvas
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.CanvasDrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Canvas as GCanvas
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import feedhub.core.Danmaku

/**
 * 盖在视频上的弹幕层。**调用点一处都不用改**（`PlayerView` / `DanmakuPreview`），安卓和桌面各有一份实现：
 *
 * - 桌面（[ComposeDanmakuLayer]）：还是 Compose `Canvas`，桌面不动。
 * - 安卓（`DanmakuLayer.android.kt`）：弹幕画在**自己的一块 `SurfaceView`**（透明的 media overlay，
 *   叠在视频 `SurfaceView` 之上、Compose 控件之下），由一个**独立的画线程**按 `Choreographer` 的 vsync
 *   用 `lockHardwareCanvas` 画。这样弹幕在动的时候 App 主窗口一帧都不用重画（Compose 不重组、RenderThread 不为弹幕干活），
 *   屏幕还是 120Hz 就 120 帧，**不降刷新率、不降弹幕帧**。
 *
 * 两边共用的只有"排布 / 精灵"这套逻辑（[DanmakuScheduler]、[DanmakuSettings.layout]），换的只是"往哪画、谁来驱动"，
 * 见 [DanmakuRenderLoop]。
 *
 * [onSurface] 只在安卓上有意义：`true`（默认，播放器里）走 SurfaceView；`false` 留给设置页那块**预览**
 * （[DanmakuPreview]）——预览底下是 Compose 画的渐变底、不是视频，用 SurfaceView 的话那块会变成透到桌面
 * （SurfaceView 在窗口下面，窗口上被它"挖洞"的地方就没有东西垫着了）。桌面忽略这个参数。
 */
@Composable
expect fun DanmakuLayer(
    timeline: DanmakuTimeline,
    state: PlayerState,
    settings: DanmakuSettings,
    modifier: Modifier = Modifier,
    onSurface: Boolean = true,
)

/**
 * 桌面的做法（也是安卓设置页预览的做法）：每一帧 [DanmakuScheduler] 排好该画的弹幕，再从精灵缓存里取每条弹幕
 * 预先画好的图（文字 + 描边）直接贴上去，所以每帧不重新测量 / 排版文字，只画此刻可见的几十条。
 * 暂停（[PlayerState.playing] 为 false）时不跑帧循环，画面停住。
 *
 * 设置 [settings] 怎么生效：过滤、合并（[DanmakuSettings.filter]）在这里交给 [DanmakuTimeline.setFilter]，只有真的变了才对已取回的弹幕重新处理；
 * 影响排布的（[DanmakuSettings.layout]）交给 [DanmakuScheduler.configure]，变了才重排，从当前时间点起；
 * 透明度每帧直接用，描边、字重变了只是精灵缓存作废重画，不动轨道。
 */
@Composable
fun ComposeDanmakuLayer(timeline: DanmakuTimeline, state: PlayerState, settings: DanmakuSettings, modifier: Modifier = Modifier) {
    val measurer = rememberTextMeasurer(cacheSize = 512)
    val dens = LocalDensity.current
    val sprites = remember(measurer, dens) { DanmakuSprites(measurer, dens) }
    val scheduler = remember(timeline, sprites) { DanmakuScheduler(timeline) { sprites.textWidth(it) } }
    val filter = settings.filter
    SideEffect { timeline.setFilter(filter) }
    var tick by remember { mutableLongStateOf(0L) }
    // 窗口看不见时弹幕不动（看得见了按当前位置接着排，见 DanmakuScheduler）。
    // 缓冲时也不动：进度本来就不走（`nowMs` 在缓冲里返回锚点），硬跑帧循环只是空转。
    // 所以这条不再是"换倍速弹幕停住"的原因。
    val running = state.playing && !state.buffering && LocalWindowVisible.current
    LaunchedEffect(running) {
        if (running) while (true) androidx.compose.runtime.withFrameNanos { tick = it }
    }
    // 倍速变了要重画一帧：`scheduler.playbackSpeed` 决定**新出现**的滚动弹幕走多快，已经排上的弹幕按自己的
    // durationMs 走（不该跟着重算，否则整屏弹幕会跳一下）。播放中帧循环本来就每帧在跑，这里只在播放中补一次，
    // 暂停时改倍速（下拉菜单里改了又继续）也能立刻用上新速率。
    LaunchedEffect(state.speed) { if (running) androidx.compose.runtime.withFrameNanos { tick = it } }
    Canvas(modifier.clipToBounds().graphicsLayer()) {
        @Suppress("UNUSED_EXPRESSION") tick
        @Suppress("UNUSED_EXPRESSION") state.positionMs // 暂停时跳转，也要重画
        if (size.width < 1f || size.height < 1f) return@Canvas
        val layout = settings.layout(size.width, size.height, density)
        sprites.configure(layout.fontPx, layout.fontWeight, settings.strokeWidth * density)
        scheduler.configure(size.width, size.height, layout)
        scheduler.playbackSpeed = state.speed
        val now = state.nowMs()
        for (a in scheduler.advance(now)) {
            val sprite = sprites.sprite(a.danmaku)
            val pad = sprites.pad
            drawImage(
                sprite,
                topLeft = Offset(a.x(now, size.width) - pad, scheduler.y(a) + (scheduler.laneHeight - sprite.height) / 2),
                alpha = settings.opacity,
            )
        }
    }
}

/**
 * 弹幕精灵：每条弹幕（文字 + 颜色 + 字号）第一次要画时，用 [TextMeasurer] 排版一次、连描边画进一张 [ImageBitmap]，之后每帧只是贴图。
 * LRU，最多 [MAX] 张；字号、字重、描边变了（窗口缩放、改设置）就清空重来。
 */
internal class DanmakuSprites(private val measurer: TextMeasurer, private val density: Density) {
    @Volatile
    var fontPx: Float = 20f
        private set
    private var weight = 4
    private var strokePx = 2f

    /** 换字号 / 字重 / 描边（像素，外沿可见的宽度）；和现在的一样就什么都不做，不一样精灵缓存作废。 */
    @Synchronized
    fun configure(fontPx: Float, weight: Int, strokePx: Float) {
        if (fontPx == this.fontPx && weight == this.weight && strokePx == this.strokePx) return
        this.fontPx = fontPx
        this.weight = weight
        this.strokePx = strokePx
        cache.clear()
    }

    /** 精灵四周留出的边，放得下描边（加 1 像素给抗锯齿）。 */
    val pad: Float get() = spritePad(strokePx)

    private data class Key(val text: String, val color: Int, val size: Int)

    private val cache = object : LinkedHashMap<Key, ImageBitmap>(256, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Key, ImageBitmap>?) = size > MAX
    }

    private fun style(d: Danmaku): TextStyle =
        TextStyle(fontSize = with(density) { (fontPx * sizeFactor(d)).toSp() }, fontWeight = FontWeight(weight * 100))

    private fun sizeFactor(d: Danmaku) = (d.fontSize / 25f).coerceIn(0.6f, 1.6f)

    /** 文字本身的宽度（像素）。TextMeasurer 自带缓存，同样的文字不会重复排版。 */
    @Synchronized
    fun textWidth(d: Danmaku): Float =
        measurer.measure(d.text, style(d), softWrap = false, maxLines = 1).size.width.toFloat()

    @Synchronized
    fun sprite(d: Danmaku): ImageBitmap {
        val key = Key(d.text, d.color, (fontPx * sizeFactor(d)).toInt())
        return cache.getOrPut(key) { render(d) }
    }

    private fun render(d: Danmaku): ImageBitmap {
        val layout = measurer.measure(d.text, style(d), softWrap = false, maxLines = 1)
        val p = pad
        val w = (layout.size.width + 2 * p).toInt().coerceAtLeast(1)
        val h = (layout.size.height + 2 * p).toInt().coerceAtLeast(1)
        val bitmap = ImageBitmap(w, h)
        val canvas = GCanvas(bitmap)
        val rgb = d.color and 0xFFFFFF
        val fill = Color(0xFF000000.toInt() or rgb)
        // 太暗的文字配白色描边，其余黑色
        val luminance = (0.299 * (rgb shr 16 and 0xFF) + 0.587 * (rgb shr 8 and 0xFF) + 0.114 * (rgb and 0xFF)) / 255
        val outline = if (luminance < 0.2) Color.White else Color.Black
        CanvasDrawScope().draw(density, LayoutDirection.Ltr, canvas, Size(w.toFloat(), h.toFloat())) {
            val at = Offset(p, p)
            // 描边画在文字轮廓的两侧各一半，之后文字盖在上面，露在外面的就是 strokePx 宽
            if (strokePx > 0f) drawText(layout, color = outline, topLeft = at, drawStyle = Stroke(width = strokePx * 2, join = StrokeJoin.Round))
            drawText(layout, color = fill, topLeft = at)
        }
        return bitmap
    }

    private companion object {
        const val MAX = 400
    }
}
