package feedhub.app.detail

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sign

/**
 * [ImageViewer] 触屏与缩放手势的纯几何 / 状态机逻辑（不依赖 Compose 运行时，方便离线单测全覆盖）：
 * 1. **1× 横竖方向锁**：过 `touchSlop` 时锁定横轴（左右 1:1 露出相邻图翻页，`dismissDragY = 0`）或纵轴（仅允许向下拉关闭，
 *    禁止向上飘起 `dy < 0` 与斜向乱飘）；
 * 2. **按真实画面比例夹取平移边界**：信箱黑边图（如手机竖屏看 16:9 横图、桌面横屏看竖长图）按适配后的真实图片尺寸算 `maxPan`，
 *    不会把图片拖进黑边虚空；
 * 3. **放大后滑到边缘无缝切图**：放大态下先在真实图片边界内平移，贴到左 / 右边缘继续横向拉动时将溢出位移无缝交给相邻图分页；
 *    半途往回拉会先把相邻图收回，再恢复图片内部平移；
 * 4. **Fling 轻拂与慢拖结算**：结合 `VelocityTracker` 速度与位移比例判定翻页 / 关闭 / 反向甩回取消。
 */

/** 相邻两张大图之间的黑色分隔缝宽度。 */
internal val ViewerPageGap = 16.dp

/** 快速轻拂（Fling）翻页 / 下拉关闭的最小同向位移门槛（防轻点抬手瞬间抖动误触发）。 */
internal val ViewerFlingMinDistance = 20.dp

/** 横向快速轻拂（Fling）切图的速度门槛（dp/s）。 */
internal val ViewerFlingVelocity = 300.dp

/** 1× 下拉快速轻拂关闭查看器的纵向速度门槛（dp/s）。 */
internal val ViewerDismissFlingVelocity = 380.dp

/** 横向慢速拖动（未达轻拂速度）翻页所需的视口宽度比例。 */
internal const val ViewerSlowDragFraction = 0.20f

/** 横向慢速拖动翻页距离上限（宽屏 / 平板不必拖过半个屏幕）。 */
internal val ViewerSlowDragMax = 140.dp

/** 1× 慢速下拉关闭所需的视口高度比例。 */
internal const val ViewerDismissDragFraction = 0.15f

/** 横向优先判定的方向锁比例（放大态边缘切图防斜擦误触发）。 */
internal const val ViewerAxisLockRatio = 1.15f

/** 图片按 `ContentScale.Fit` 塞进 `[boxW, boxH]` 之后的真实显示宽高；比例未知时退回容器宽高。 */
internal fun viewerFittedSize(boxW: Float, boxH: Float, imageAspect: Float?): Size {
    if (boxW <= 0f || boxH <= 0f) return Size.Zero
    val r = imageAspect?.takeIf { it > 0f && it.isFinite() } ?: return Size(boxW, boxH)
    val byWidthH = boxW / r
    return if (byWidthH <= boxH) Size(boxW, byWidthH) else Size(boxH * r, boxH)
}

/** 放大 [scale] 倍后，图片左/右与上/下相对视口中心最多能平移多少像素（未超出视口的那一轴为 0）。 */
internal fun viewerMaxPan(scale: Float, boxW: Float, boxH: Float, imageAspect: Float?): Offset {
    if (scale <= 1.01f || boxW <= 0f || boxH <= 0f) return Offset.Zero
    val fitted = viewerFittedSize(boxW, boxH, imageAspect)
    val maxX = max(0f, (fitted.width * scale - boxW) / 2f)
    val maxY = max(0f, (fitted.height * scale - boxH) / 2f)
    return Offset(maxX, maxY)
}

/** 将平移量 [offset] 夹在真实可见图片边缘之内。 */
internal fun viewerClampPan(
    scale: Float,
    offset: Offset,
    boxW: Float,
    boxH: Float,
    imageAspect: Float?,
): Offset {
    val maxPan = viewerMaxPan(scale, boxW, boxH, imageAspect)
    if (maxPan == Offset.Zero) return Offset.Zero
    return Offset(
        x = offset.x.coerceIn(-maxPan.x, maxPan.x),
        y = offset.y.coerceIn(-maxPan.y, maxPan.y),
    )
}

/** 以视口内 [anchor] 点为锚点缩放，缩放后紧贴真实图片边界夹取平移量。 */
internal fun viewerZoomAt(
    currentScale: Float,
    currentOffset: Offset,
    factor: Float,
    anchor: Offset,
    boxW: Float,
    boxH: Float,
    imageAspect: Float?,
    maxScale: Float = 8f,
): Pair<Float, Offset> {
    val nextScale = (currentScale * factor).coerceIn(1f, maxScale)
    if (nextScale <= 1.01f || boxW <= 0f || boxH <= 0f) return 1f to Offset.Zero
    val center = Offset(boxW / 2f, boxH / 2f)
    val ratio = nextScale / currentScale.coerceAtLeast(1f)
    val rawOffset = (anchor - center) - (anchor - center - currentOffset) * ratio
    return nextScale to viewerClampPan(nextScale, rawOffset, boxW, boxH, imageAspect)
}

/**
 * 横向分页跟手位移换算：
 * - 对应方向有相邻图时 1:1 跟手走满 `[-stridePx, +stridePx]`；
 * - 第一张往右拉 / 最后一张往左拉时给阻尼回弹（最多 18% 视口宽），提示已到边界。
 */
internal fun viewerPageDragOffset(
    rawDx: Float,
    stridePx: Float,
    boxW: Float,
    hasPrev: Boolean,
    hasNext: Boolean,
): Float {
    if (boxW <= 0f || stridePx <= 0f) return 0f
    val hasNeighbor = when {
        rawDx < 0f -> hasNext
        rawDx > 0f -> hasPrev
        else -> true
    }
    return if (hasNeighbor) {
        rawDx.coerceIn(-stridePx, stridePx)
    } else {
        (rawDx * 0.22f).coerceIn(-boxW * 0.18f, boxW * 0.18f)
    }
}

/**
 * 横向切图松手结算：返回值 `+1` = 下一张、`-1` = 上一张、`0` = 留在当前张。
 * - 左滑（`dxPx < 0`）看下一张，右滑（`dxPx > 0`）看上一张；
 * - 支持快速轻拂（Fling）、反向甩回取消、以及慢拖过视口比例（带上限封顶）翻页。
 */
internal fun viewerHorizontalSettleStep(
    dxPx: Float,
    velocityXPxPerSec: Float,
    boxWidthPx: Float,
    minFlingDistancePx: Float,
    flingVelocityPxPerSec: Float,
    maxSlowDragPx: Float = Float.POSITIVE_INFINITY,
): Int {
    if (boxWidthPx <= 0f) return 0
    // 反向甩回 = 取消切图
    if (dxPx < 0f && velocityXPxPerSec >= flingVelocityPxPerSec) return 0
    if (dxPx > 0f && velocityXPxPerSec <= -flingVelocityPxPerSec) return 0
    // 同向快速轻拂
    if (dxPx <= -minFlingDistancePx && velocityXPxPerSec <= -flingVelocityPxPerSec) return 1
    if (dxPx >= minFlingDistancePx && velocityXPxPerSec >= flingVelocityPxPerSec) return -1
    // 慢速拖动过比例
    val slowThreshold = min(boxWidthPx * ViewerSlowDragFraction, maxSlowDragPx)
        .coerceAtLeast(minFlingDistancePx * 2.5f)
    return when {
        dxPx <= -slowThreshold -> 1
        dxPx >= slowThreshold -> -1
        else -> 0
    }
}

/** 1× 下拉松手要不要关闭查看器：向下轻拂或拖过 15% 高度关闭；反悔向上甩回则留住。 */
internal fun viewerShouldDismiss(
    dyPx: Float,
    velocityYPxPerSec: Float,
    boxHeightPx: Float,
    minFlingDistancePx: Float,
    dismissFlingVelocityPxPerSec: Float,
): Boolean {
    if (boxHeightPx <= 0f || dyPx <= 0f) return false
    if (velocityYPxPerSec <= -dismissFlingVelocityPxPerSec) return false
    if (dyPx >= minFlingDistancePx && velocityYPxPerSec >= dismissFlingVelocityPxPerSec) return true
    return dyPx >= boxHeightPx * ViewerDismissDragFraction
}

/** 单指触摸手势当前锁定的模式。 */
internal enum class ViewerTouchMode {
    Undecided,
    HorizontalPage,
    VerticalDismiss,
    ZoomedPanAndEdge,
}

/** 单次移动事件处理后的输出状态。 */
internal data class ViewerTouchUpdate(
    val consumed: Boolean,
    val panOffset: Offset,
    val pageDragX: Float,
    val dismissDragY: Float,
)

/**
 * 大图查看器**一场单指触屏手势**的账本：
 * - 未过 [slopPx] 前不消费事件，保证单击关闭、双击缩放、长按弹菜单在 1× 与放大态下都不会因手指微颤失灵；
 * - 1× 过阈值后立即锁定横轴（左右翻页）或纵轴（仅向下拉关闭，禁止上飘和斜飘）；
 * - 放大态过阈值后在真实图片边界内平移，若起手已在边缘继续向外横拉、或平移撞到左右边缘后继续横拉，无缝转入相邻图跟手翻页。
 */
internal class ImageViewerTouchTracker(private val slopPx: Float) {
    var mode: ViewerTouchMode = ViewerTouchMode.Undecided
        private set

    /** 用于松手结算的横向未阻尼累计页位移（左滑为负，右滑为正）。 */
    var rawPageDx: Float = 0f
        private set

    /** 用于松手结算的纵向未阻尼下拉位移（>= 0）。 */
    var dismissDragY: Float = 0f
        private set

    private var rawDx: Float = 0f
    private var rawDy: Float = 0f
    private var rawDismissDy: Float = 0f
    private var edgeOverflowX: Float = 0f
    private var startedAtEdge: Boolean = false

    fun onMove(
        delta: Offset,
        scale: Float,
        currentPan: Offset,
        boxW: Float,
        boxH: Float,
        stridePx: Float,
        imageAspect: Float?,
        hasPrev: Boolean,
        hasNext: Boolean,
    ): ViewerTouchUpdate {
        if (scale <= 1.01f) {
            return moveAtOneX(delta, boxW, stridePx, hasPrev, hasNext)
        }
        return moveWhileZoomed(delta, scale, currentPan, boxW, boxH, stridePx, imageAspect, hasPrev, hasNext)
    }

    private fun moveAtOneX(
        delta: Offset,
        boxW: Float,
        stridePx: Float,
        hasPrev: Boolean,
        hasNext: Boolean,
    ): ViewerTouchUpdate {
        if (mode == ViewerTouchMode.Undecided) {
            rawDx += delta.x
            rawDy += delta.y
            val dist = hypot(rawDx, rawDy)
            if (dist <= slopPx) {
                return ViewerTouchUpdate(consumed = false, panOffset = Offset.Zero, pageDragX = 0f, dismissDragY = 0f)
            }
            return if (abs(rawDx) >= abs(rawDy)) {
                mode = ViewerTouchMode.HorizontalPage
                rawPageDx = rawDx - sign(rawDx) * min(abs(rawDx), slopPx)
                val dragX = viewerPageDragOffset(rawPageDx, stridePx, boxW, hasPrev, hasNext)
                ViewerTouchUpdate(consumed = true, panOffset = Offset.Zero, pageDragX = dragX, dismissDragY = 0f)
            } else {
                mode = ViewerTouchMode.VerticalDismiss
                rawDismissDy = rawDy - sign(rawDy) * min(abs(rawDy), slopPx)
                dismissDragY = rawDismissDy.coerceAtLeast(0f)
                ViewerTouchUpdate(consumed = true, panOffset = Offset.Zero, pageDragX = 0f, dismissDragY = dismissDragY)
            }
        }
        return when (mode) {
            ViewerTouchMode.HorizontalPage -> {
                rawPageDx += delta.x
                val dragX = viewerPageDragOffset(rawPageDx, stridePx, boxW, hasPrev, hasNext)
                ViewerTouchUpdate(consumed = true, panOffset = Offset.Zero, pageDragX = dragX, dismissDragY = 0f)
            }
            ViewerTouchMode.VerticalDismiss -> {
                rawDismissDy += delta.y
                dismissDragY = rawDismissDy.coerceAtLeast(0f)
                ViewerTouchUpdate(consumed = true, panOffset = Offset.Zero, pageDragX = 0f, dismissDragY = dismissDragY)
            }
            else -> ViewerTouchUpdate(consumed = false, panOffset = Offset.Zero, pageDragX = 0f, dismissDragY = 0f)
        }
    }

    private fun moveWhileZoomed(
        delta: Offset,
        scale: Float,
        currentPan: Offset,
        boxW: Float,
        boxH: Float,
        stridePx: Float,
        imageAspect: Float?,
        hasPrev: Boolean,
        hasNext: Boolean,
    ): ViewerTouchUpdate {
        val maxPan = viewerMaxPan(scale, boxW, boxH, imageAspect)
        val clampedCurrent = Offset(
            currentPan.x.coerceIn(-maxPan.x, maxPan.x),
            currentPan.y.coerceIn(-maxPan.y, maxPan.y),
        )
        if (mode == ViewerTouchMode.Undecided) {
            rawDx += delta.x
            rawDy += delta.y
            val dist = hypot(rawDx, rawDy)
            if (dist <= slopPx) {
                return ViewerTouchUpdate(consumed = false, panOffset = clampedCurrent, pageDragX = 0f, dismissDragY = 0f)
            }
            mode = ViewerTouchMode.ZoomedPanAndEdge
            val factor = if (dist > 0f) (dist - slopPx) / dist else 0f
            val firstStep = Offset(rawDx * factor, rawDy * factor)
            val atRightEdge = clampedCurrent.x <= -maxPan.x + 1f
            val atLeftEdge = clampedCurrent.x >= maxPan.x - 1f
            val horizontalIntent = abs(rawDx) >= abs(rawDy) * ViewerAxisLockRatio
            if (horizontalIntent && ((rawDx < 0f && atRightEdge) || (rawDx > 0f && atLeftEdge))) {
                startedAtEdge = true
                rawPageDx = firstStep.x
                edgeOverflowX = firstStep.x
                val dragX = viewerPageDragOffset(rawPageDx, stridePx, boxW, hasPrev, hasNext)
                return ViewerTouchUpdate(consumed = true, panOffset = clampedCurrent, pageDragX = dragX, dismissDragY = 0f)
            }
            return applyZoomedStep(firstStep, clampedCurrent, maxPan, stridePx, boxW, hasPrev, hasNext)
        }
        return applyZoomedStep(delta, clampedCurrent, maxPan, stridePx, boxW, hasPrev, hasNext)
    }

    private fun applyZoomedStep(
        step: Offset,
        currentPan: Offset,
        maxPan: Offset,
        stridePx: Float,
        boxW: Float,
        hasPrev: Boolean,
        hasNext: Boolean,
    ): ViewerTouchUpdate {
        // 已经拉出相邻图时，反向回拉先把相邻图推回 0，再恢复原图内部平移
        if (abs(rawPageDx) > 0.01f) {
            val nextRaw = rawPageDx + step.x
            if (rawPageDx < 0f && nextRaw > 0f) {
                rawPageDx = 0f
                edgeOverflowX = 0f
                val nextPanX = (-maxPan.x + nextRaw).coerceIn(-maxPan.x, maxPan.x)
                val nextPanY = (currentPan.y + step.y).coerceIn(-maxPan.y, maxPan.y)
                return ViewerTouchUpdate(consumed = true, panOffset = Offset(nextPanX, nextPanY), pageDragX = 0f, dismissDragY = 0f)
            }
            if (rawPageDx > 0f && nextRaw < 0f) {
                rawPageDx = 0f
                edgeOverflowX = 0f
                val nextPanX = (maxPan.x + nextRaw).coerceIn(-maxPan.x, maxPan.x)
                val nextPanY = (currentPan.y + step.y).coerceIn(-maxPan.y, maxPan.y)
                return ViewerTouchUpdate(consumed = true, panOffset = Offset(nextPanX, nextPanY), pageDragX = 0f, dismissDragY = 0f)
            }
            rawPageDx = nextRaw
            val dragX = viewerPageDragOffset(rawPageDx, stridePx, boxW, hasPrev, hasNext)
            return ViewerTouchUpdate(consumed = true, panOffset = currentPan, pageDragX = dragX, dismissDragY = 0f)
        }

        val nextPanY = (currentPan.y + step.y).coerceIn(-maxPan.y, maxPan.y)
        val candidatePanX = currentPan.x + step.x
        val clampedPanX = candidatePanX.coerceIn(-maxPan.x, maxPan.x)
        val overflowStepX = candidatePanX - clampedPanX

        if (abs(overflowStepX) > 0.01f) {
            if (abs(step.x) >= abs(step.y) * ViewerAxisLockRatio) {
                edgeOverflowX += overflowStepX
                val gate = if (startedAtEdge) 0f else slopPx * 1.1f
                if (abs(edgeOverflowX) > gate) {
                    rawPageDx = edgeOverflowX - sign(edgeOverflowX) * gate
                    val dragX = viewerPageDragOffset(rawPageDx, stridePx, boxW, hasPrev, hasNext)
                    return ViewerTouchUpdate(
                        consumed = true,
                        panOffset = Offset(clampedPanX, nextPanY),
                        pageDragX = dragX,
                        dismissDragY = 0f,
                    )
                }
            } else {
                edgeOverflowX = 0f
            }
        } else if (abs(step.x) > 0.5f) {
            edgeOverflowX = 0f
            startedAtEdge = false
        }

        return ViewerTouchUpdate(
            consumed = true,
            panOffset = Offset(clampedPanX, nextPanY),
            pageDragX = 0f,
            dismissDragY = 0f,
        )
    }
}
