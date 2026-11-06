package feedhub.app

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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.layoutId
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import feedhub.app.feed.FeedGridSpec
import feedhub.app.feed.clampFeedWidth
import feedhub.app.feed.minFeedWidth
import feedhub.app.feed.twoPaneColumns
import feedhub.app.ui.HorizontalResizePointer
import kotlin.math.roundToInt

/** 分隔线可见的宽度，和能抓住它拖的透明热区宽度（热区骑在线上，不占布局宽度）。 */
private val LineWidth = 1.dp
private val HandleWidth = 8.dp

private object FeedSlot
private object DetailSlot
private object HandleSlot

/**
 * 宽屏的内容区：左边信息流（[feedContent]），点开一条后右边直接显示详情（[detailContent]，[detail] 非空时显示）。
 * 没有详情时信息流占满；分隔线可以左右拖，松手时把左栏占内容区的比例通过 [onRatioChange] 交出去存（拖的过程中不存）。
 *
 * 点开或关闭直接一步到位，不再做逐帧挤宽与卡片位移动画。
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

    BoxWithConstraints(modifier.clipToBounds()) {
        val total = maxWidth
        val minFeed = minFeedWidth(total, columns)
        // 拖动中的左栏宽度（null = 没在拖，用存着的比例）；raw 是不夹范围的累计值，指针拖过头再拖回来时手感才对得上
        var dragging by remember(stateKey) { mutableStateOf<Dp?>(null) }
        var raw by remember(stateKey) { mutableFloatStateOf(0f) }
        // 拖到一半详情被关了（Esc）：拖动的手势跟着没了，不会再有"松手"，别让拖动中的宽度一直留着
        LaunchedEffect(open) { if (!open) dragging = null }
        val split = clampFeedWidth(dragging ?: (total * ratio), total, minFeed)
        val spec = remember(open, split, total, columns) {
            if (open) FeedGridSpec(twoPaneColumns(split, total, columns), split)
            else FeedGridSpec(columns, total)
        }
        val density = LocalDensity.current

        Layout(
            content = {
                Box(Modifier.layoutId(FeedSlot).clipToBounds()) { feedContent(spec) }
                if (detail != null) {
                    Box(Modifier.layoutId(DetailSlot)) {
                        detailContent(detail)
                    }
                    SplitHandle(
                        enabled = true,
                        onStart = { raw = split.value },
                        onDelta = { px ->
                            raw += px / density.density
                            dragging = clampFeedWidth(raw.dp, total, minFeed)
                        },
                        onEnd = {
                            dragging?.let { onRatioChange(it / total) }
                            dragging = null
                        },
                        modifier = Modifier.layoutId(HandleSlot),
                    )
                }
            },
        ) { measurables, constraints ->
            val w = constraints.maxWidth
            val h = constraints.maxHeight
            val splitPx = split.roundToPx().coerceIn(0, w)
            val linePx = LineWidth.roundToPx().coerceAtLeast(1)
            val feedPx = if (open) splitPx else w
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
 * 两栏之间的分隔线：画的是 1dp 的细线，能抓住拖的热区比它宽（[HandleWidth]）。悬停（桌面）或拖动时线变粗变色，鼠标指针换成左右拖的样子。
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
 */
fun Modifier.fixedLayoutWidth(width: Dp): Modifier = layout { measurable, constraints ->
    val w = width.roundToPx()
    val placeable = measurable.measure(Constraints(w, w, constraints.minHeight, constraints.maxHeight))
    layout(constraints.maxWidth, placeable.height) { placeable.place(0, 0) }
}
