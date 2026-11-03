package feedhub.app

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.layoutId
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import feedhub.app.feed.FeedGridSpec
import feedhub.app.feed.PaneAnimMillis
import feedhub.app.feed.clampFeedWidth
import feedhub.app.feed.minFeedWidth
import feedhub.app.feed.twoPaneColumns
import feedhub.app.ui.HorizontalResizePointer
import kotlin.math.roundToInt

/** 同一个详情栏里换了一条内容时，新内容淡入的时长和起始透明度（轻轻闪一下，不重放整段过渡）。 */
private const val ItemFadeMillis = 150
private const val ItemFadeFrom = 0.4f

/** 分隔线可见的宽度，和能抓住它拖的透明热区宽度（热区骑在线上，不占布局宽度）。 */
private val LineWidth = 1.dp
private val HandleWidth = 8.dp

private object FeedSlot
private object DetailSlot
private object HandleSlot

/** 记住最近一次非空的值（不是状态，只在关闭动画里用来继续画刚关掉的详情）。 */
private class LastValue<T : Any> {
    var value: T? = null
}

/**
 * 右栏这一帧要不要再套一层淡入（[FadeInOnChange]）。**纯函数**，好让"第一次打开不淡"这条被离线测试盯住。
 *
 * 只有"**已经在双栏里、又换了一条**"才淡——那才是这一层的用途（轻轻闪一下，不重放整段过渡）。
 * **第一次打开详情不淡**：外面已经有一次 [PaneAnimMillis] 的滑入 + 淡入了，这里再来一层 0.4 → 1 的 150ms
 * 就是"点进去先暗一下再亮起来"，用户看到的正是"渲染了一下"。
 *
 * @param previous 上一帧画的那一条（还没开过详情时是 null）
 * @param current 这一帧要画的那一条（关详情的那一帧是刚关掉的那条，会继续画）
 * @param open 现在到底有没有详情
 */
internal fun shouldFadeDetailOnChange(previous: Any?, current: Any?, open: Boolean): Boolean =
    open && previous != null && current != null && previous != current

/**
 * 宽屏的内容区：左边信息流（[feedContent]），点开一条后右边出详情（[detailContent]，[detail] 非空时显示）。
 * 没有详情时信息流占满；分隔线可以左右拖，松手时把左栏占内容区的比例通过 [onRatioChange] 交出去存（拖的过程中不存）。
 *
 * **过渡动画**（[PaneAnimMillis]）：进度 0（全宽）→ 1（双栏），左栏可见宽度从全宽收到 [ratio] 对应的宽度，
 * 详情栏的左边缘跟着左栏的右边缘一路滑进来并淡入；关闭是反过来。详情栏始终按它最终的宽度排版，动画里只是挪位置 / 改透明度，不会每帧重排。
 *
 * 信息流栏的网格**不按动画途中的宽度排**（那样每帧都重排，列数在阈值上还会跳变）：列数和排版宽度由**目标状态**一次定下来（[FeedGridSpec]）——
 * 点开的那一刻就换成双栏的列数和栏宽（卡片靠 `animateItem` 滑到新位置，和详情滑入同时发生，眼睛在详情那边，不显得突兀），
 * 关闭的那一刻就换回全宽的列数和宽度；之后动画只改左栏的**可见宽度**（裁剪）。所以动画的每一帧网格都没有重排，只有一次布局：
 * 打开时网格已经在左边排好，右边空出来的地方被滑进来的详情一点点盖住；关闭时网格已经按全宽排好，随着左栏变宽一点点露出来。
 *
 * @param stateKey 换页签时换：每个页签有自己的开 / 关状态，换页签直接落在那个页签的状态上，不放动画。
 * @param columns 全宽时的列数，用来算双栏时的列数和左栏最窄宽度；设置是「自动」的话外面已经按窗口宽度
 *   解析好了（`App.kt` 的 `homeColumns`），这里只当成一个数用。
 * @param ratio 左栏占内容区宽度的比例（设置里存的）。
 */
@Composable
fun <T : Any> SplitPane(
    detail: T?,
    stateKey: Any,
    columns: Int,
    ratio: Float,
    onRatioChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
    detailContent: @Composable (T) -> Unit,
    feedContent: @Composable (FeedGridSpec) -> Unit,
) {
    val open = detail != null
    // 0 = 全宽（没有详情），1 = 双栏
    val progress = remember(stateKey) { Animatable(if (open) 1f else 0f) }
    LaunchedEffect(progress, open) {
        progress.animateTo(if (open) 1f else 0f, tween(PaneAnimMillis, easing = FastOutSlowInEasing))
    }
    // 关闭动画里详情还要继续画一会儿，所以把最近一次的条目留着
    val last = remember(stateKey) { LastValue<T>() }
    // 上一帧画的是谁 + 当时是不是已经开着：用来区分"第一次打开"和"已经在双栏里换了另一条"
    val previous = remember(stateKey) { LastValue<T>() }
    val wasOpenWith = previous.value
    if (detail != null) last.value = detail
    val shown = detail ?: last.value
    if (detail != null) previous.value = detail
    val moving by remember(progress) { derivedStateOf { progress.value > 0f } }
    // 能拖分隔线：双栏已经完全展开（动画途中不让拖）
    val ready by remember(progress, open) { derivedStateOf { open && progress.value >= 1f } }
    // 换了一条内容（不是"第一次打开"）：这时候才需要那一层 150ms 的淡入，见 [shouldFadeDetailOnChange]
    val switching = shouldFadeDetailOnChange(previous = wasOpenWith, current = detail, open = open)

    BoxWithConstraints(modifier.clipToBounds()) {
        val total = maxWidth
        val minFeed = minFeedWidth(total, columns)
        // 拖动中的左栏宽度（null = 没在拖，用存着的比例）；raw 是不夹范围的累计值，指针拖过头再拖回来时手感才对得上
        var dragging by remember { mutableStateOf<Dp?>(null) }
        var raw by remember { mutableFloatStateOf(0f) }
        // 拖到一半详情被关了（Esc）：拖动的手势跟着没了，不会再有"松手"，别让拖动中的宽度一直留着
        LaunchedEffect(open) { if (!open) dragging = null }
        val split = clampFeedWidth(dragging ?: (total * ratio), total, minFeed)
        // 网格的排法只看目标状态（开 / 关），不看动画进度
        val spec = remember(open, split, total, columns) {
            if (open) FeedGridSpec(twoPaneColumns(split, total, columns), split)
            else FeedGridSpec(columns, total)
        }
        val density = LocalDensity.current

        Layout(
            content = {
                Box(Modifier.layoutId(FeedSlot).clipToBounds()) { feedContent(spec) }
                if (shown != null && (open || moving)) {
                    Box(Modifier.layoutId(DetailSlot).graphicsLayer { alpha = progress.value }) {
                        FadeInOnChange(shown, animate = switching) { detailContent(shown) }
                    }
                    SplitHandle(
                        enabled = ready,
                        onStart = { raw = split.value },
                        onDelta = { px ->
                            raw += px / density.density
                            dragging = clampFeedWidth(raw.dp, total, minFeed)
                        },
                        onEnd = {
                            dragging?.let { onRatioChange(it / total) }
                            dragging = null
                        },
                        modifier = Modifier.layoutId(HandleSlot).graphicsLayer { alpha = progress.value },
                    )
                }
            },
        ) { measurables, constraints ->
            val w = constraints.maxWidth
            val h = constraints.maxHeight
            val splitPx = split.roundToPx().coerceIn(0, w)
            val linePx = LineWidth.roundToPx().coerceAtLeast(1)
            // 左栏可见宽度：全宽 → 双栏的宽度，随进度走。这一步只改位置和裁剪，网格自己的排版宽度由 spec 定
            val feedPx = (w + (splitPx - w) * progress.value).roundToInt()
            val feed = measurables.first { it.layoutId == FeedSlot }.measure(Constraints.fixed(feedPx, h))
            val detailPx = (w - splitPx - linePx).coerceAtLeast(0)
            val detailPlaceable = measurables.firstOrNull { it.layoutId == DetailSlot }?.measure(Constraints.fixed(detailPx, h))
            val handlePx = HandleWidth.roundToPx()
            val handlePlaceable = measurables.firstOrNull { it.layoutId == HandleSlot }?.measure(Constraints.fixed(handlePx, h))
            layout(w, h) {
                feed.place(0, 0)
                detailPlaceable?.place(feedPx + linePx, 0)
                // 热区居中骑在线上：线在热区里是居中的，所以热区左边 = 线的位置 - 热区里线的偏移
                handlePlaceable?.place(feedPx - ((handlePx - linePx) / 2f).roundToInt(), 0)
            }
        }
    }
}

/**
 * 同一个详情栏里换了一条内容：新内容从半透明淡入，旧的立刻换掉（不同时画两个详情，免得两个播放器同时起）。
 *
 * **第一次打开详情不淡**：右栏本来就在从 [PaneAnimMillis] 的滑入动画里淡进来
 * （外层 `graphicsLayer { alpha = progress.value }`），这里再来一层 0.4 → 1 的 150ms 就是"点进去先暗一下再亮起来"，
 * 用户看到的就是"渲染了一下"。现在只有**已经在双栏里、又换了一条**的时候才淡（那才是它本来的用途）。
 */
@Composable
private fun <T : Any> FadeInOnChange(key: T, animate: Boolean, content: @Composable () -> Unit) {
    if (!animate) {
        Box { content() }
        return
    }
    val alpha = remember(key) { Animatable(ItemFadeFrom) }
    LaunchedEffect(alpha) { alpha.animateTo(1f, tween(ItemFadeMillis, easing = FastOutSlowInEasing)) }
    Box(Modifier.graphicsLayer { this.alpha = alpha.value }) { content() }
}

/**
 * 两栏之间的分隔线：画的是 1dp 的细线，能抓住拖的热区比它宽（[HandleWidth]）。悬停（桌面）或拖动时线变粗变色，鼠标指针换成左右拖的样子。
 * [enabled] 为 false（过渡动画途中）时只画线，不能拖。
 */
@Composable
private fun SplitHandle(enabled: Boolean, onStart: () -> Unit, onDelta: (Float) -> Unit, onEnd: () -> Unit, modifier: Modifier) {
    val source = remember { MutableInteractionSource() }
    val hovered by source.collectIsHoveredAsState()
    var pressed by remember { mutableStateOf(false) }
    val active = enabled && (hovered || pressed)
    val drag = rememberDraggableState(onDelta)
    Box(
        modifier
            .hoverable(source)
            .then(
                if (enabled) {
                    Modifier
                        .pointerHoverIcon(HorizontalResizePointer)
                        .draggable(
                            state = drag,
                            orientation = Orientation.Horizontal,
                            onDragStarted = { pressed = true; onStart() },
                            onDragStopped = { pressed = false; onEnd() },
                        )
                } else {
                    Modifier
                },
            ),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier.fillMaxHeight().width(if (active) 2.dp else LineWidth)
                .background(if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant),
        )
    }
}

/**
 * 不管外面给多宽，都按 [width] 的宽度排自己的内容（左对齐）；外面窄了多出来的部分由外层裁掉，宽了留白。
 * 双栏的过渡动画里用：栏的可见宽度在变，里面的网格不跟着重排。
 */
fun Modifier.fixedLayoutWidth(width: Dp): Modifier = layout { measurable, constraints ->
    val w = width.roundToPx()
    val placeable = measurable.measure(Constraints(w, w, constraints.minHeight, constraints.maxHeight))
    layout(constraints.maxWidth, placeable.height) { placeable.place(0, 0) }
}
