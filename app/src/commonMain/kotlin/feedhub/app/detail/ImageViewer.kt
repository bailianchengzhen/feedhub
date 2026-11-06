package feedhub.app.detail

import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.input.pointer.util.addPointerInputChange
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties
import coil3.compose.AsyncImage
import feedhub.app.video.PagerCubicOutEasing
import feedhub.app.video.isTouchDevice
import feedhub.app.video.pagerSettleDurationMs
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.roundToInt

/** 大图查看层的状态：这一组图和当前是第几张。 */
@Stable
class ImageViewerState(val images: List<ViewerImage>, index: Int) {
    var index: Int by mutableIntStateOf(index)

    fun next() {
        if (index < images.lastIndex) index++
    }

    fun prev() {
        if (index > 0) index--
    }
}

private const val MaxScale = 8f

/** 大图查看层的背景：纯黑。 */
private val ViewerBackground = Color(0xFF000000)

/** 底部菜单的底色（查看层永远是黑底，菜单也跟着用深色，不跟 App 的深色 / 浅色设置走）。 */
private val SheetBackground = Color(0xFF1E2126)

private val WholeWindow = object : PopupPositionProvider {
    override fun calculatePosition(anchorBounds: IntRect, windowSize: IntSize, layoutDirection: LayoutDirection, popupContentSize: IntSize) =
        IntOffset.Zero
}

/**
 * 覆盖整个窗口的大图查看层（用 Popup，双栏时也盖住左边的信息流）。
 *
 * 桌面：滚轮缩放（以鼠标位置为中心）、放大后拖动平移、双击在 1 倍和 2.5 倍之间平滑切换、
 * 左右方向键 / 两侧箭头按钮切换同一篇里的图；Esc、右上角的关闭按钮或在 1 倍时点一下关闭。
 *
 * 触屏：
 * - 双指捏合缩放 + 双指中心跟随平移；双击 1× ⇄ 2.5× 平滑缩放；
 * - **左右 1:1 露出相邻图跟手翻页**（带 16dp 黑色分隔缝，支持 `VelocityTracker` 快速轻拂 Fling 切图与连划不断触）；
 * - **1× 横竖方向锁**：横滑翻页时纵向锁死不斜飘，竖向仅允许向下拖关闭（禁止向上飘起 `dy < 0`）；
 * - **放大后滑到边缘可切图**：按图片真实显示比例夹取平移边界，贴到左右边缘继续横拉时无缝带出上 / 下一张；
 * - **触屏隐藏左右箭头按钮**（仅桌面鼠标保留）；长按弹出底部菜单「保存到相册 / 分享图片 / 复制图片链接」。
 */
@Composable
fun ImageViewer(state: ImageViewerState, onClose: () -> Unit) {
    // 性能记录：页面名临时盖成"图片查看器"（关掉就恢复成下面那一页，见 PerfScreen）
    DisposableEffect(Unit) {
        feedhub.app.perf.PerfHooks.overlay("图片查看器")
        onDispose { feedhub.app.perf.PerfHooks.overlay(null) }
    }
    Popup(popupPositionProvider = WholeWindow, onDismissRequest = onClose, properties = PopupProperties(focusable = true)) {
        ViewerContent(state, onClose)
    }
}

@Composable
private fun ViewerContent(state: ImageViewerState, onClose: () -> Unit) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val actions = rememberImageViewerActions()
    val clipboard = LocalClipboardManager.current
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()

    var scale by remember { mutableFloatStateOf(1f) }
    var panOffset by remember { mutableStateOf(Offset.Zero) }
    // 横向整页跟手位移与 1× 纵向收起位移：仅在布局/绘制阶段读取，拖动与动画期间零重组
    var pageDragX by remember { mutableFloatStateOf(0f) }
    var dismissDragY by remember { mutableFloatStateOf(0f) }
    var pendingPageDelta by remember { mutableIntStateOf(0) }
    var isTouchGesture by remember { mutableStateOf(false) }
    var settleJob by remember { mutableStateOf<Job?>(null) }
    var zoomJob by remember { mutableStateOf<Job?>(null) }

    var box by remember { mutableStateOf(IntSize.Zero) }
    var menu by remember { mutableStateOf(false) }
    // 截图工具用：把安卓那一套菜单项都画出来（桌面真机上长按不会弹菜单，保持原样）
    var menuPreview by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var lastPressWasTouch by remember { mutableStateOf(isTouchDevice) }

    // 记录各张图解码后的真实宽高比与加载完成标记（用于真实边界夹取 & 透明 PNG 加载完隐藏底圈）
    val aspectRatios = remember(state) { mutableStateMapOf<Int, Float>() }
    val loadedPages = remember(state) { mutableStateMapOf<Int, Boolean>() }

    val pageGapPx = with(density) { ViewerPageGap.toPx() }
    val minFlingDistPx = with(density) { ViewerFlingMinDistance.toPx() }
    val flingVelPx = with(density) { ViewerFlingVelocity.toPx() }
    val dismissFlingVelPx = with(density) { ViewerDismissFlingVelocity.toPx() }
    val maxSlowDragPx = with(density) { ViewerSlowDragMax.toPx() }

    val image = state.images[state.index]

    // 键盘 / 箭头按钮切换图片时重置缩放与位移（触屏翻页在结算落地同一帧已原子归零）
    LaunchedEffect(state.index) {
        if (!isTouchGesture && pendingPageDelta == 0) {
            settleJob?.cancel()
            settleJob = null
            zoomJob?.cancel()
            zoomJob = null
            scale = 1f
            panOffset = Offset.Zero
            pageDragX = 0f
            dismissDragY = 0f
        }
    }

    // 截图工具（Screenshot 的 open=image-menu）：直接把底部菜单摆出来
    LaunchedEffect(state) {
        if (DetailDebug.pendingImageMenu) {
            DetailDebug.pendingImageMenu = false
            menuPreview = DetailDebug.pendingImageMenuPreview
            DetailDebug.pendingImageMenuPreview = false
            menu = true
        }
    }

    LaunchedEffect(message) {
        if (message != null) {
            delay(2400)
            message = null
        }
    }

    fun currentAspect(): Float? = aspectRatios[state.index]

    /** 以窗口里 [anchor] 这一点为中心即时缩放：缩放前它下面的图像点，缩放后还在原处。 */
    fun zoomAt(factor: Float, anchor: Offset) {
        zoomJob?.cancel()
        zoomJob = null
        val (nextScale, nextOffset) = viewerZoomAt(
            currentScale = scale,
            currentOffset = panOffset,
            factor = factor,
            anchor = anchor,
            boxW = box.width.toFloat(),
            boxH = box.height.toFloat(),
            imageAspect = currentAspect(),
            maxScale = MaxScale,
        )
        scale = nextScale
        panOffset = nextOffset
    }

    fun commitPendingStepIfNeeded() {
        val pending = pendingPageDelta
        if (pending != 0) {
            pendingPageDelta = 0
            settleJob?.cancel()
            settleJob = null
            state.index = (state.index + pending).coerceIn(0, state.images.lastIndex)
            scale = 1f
            panOffset = Offset.Zero
            pageDragX = 0f
            dismissDragY = 0f
        }
    }

    fun stepByButtonOrKey(delta: Int) {
        commitPendingStepIfNeeded()
        val target = state.index + delta
        if (target in state.images.indices) {
            state.index = target
        }
    }

    fun toast(text: String) {
        message = text
    }

    val latestMenu by rememberUpdatedState(menu)
    val latestPageGapPx by rememberUpdatedState(pageGapPx)
    val latestMinFlingDistPx by rememberUpdatedState(minFlingDistPx)
    val latestFlingVelPx by rememberUpdatedState(flingVelPx)
    val latestDismissFlingVelPx by rememberUpdatedState(dismissFlingVelPx)
    val latestMaxSlowDragPx by rememberUpdatedState(maxSlowDragPx)
    val latestOnClose by rememberUpdatedState(onClose)

    Box(
        Modifier.fillMaxSize().clipToBounds().background(ViewerBackground).onSizeChanged { box = it }
            .focusRequester(focus).focusable()
            .onKeyEvent { e ->
                if (e.type != KeyEventType.KeyDown) return@onKeyEvent false
                if (menu) {
                    // 底部菜单开着时，Esc / 返回先关菜单
                    if (e.key == Key.Escape) { menu = false; menuPreview = false; true } else false
                } else when (e.key) {
                    Key.Escape -> { onClose(); true }
                    Key.DirectionLeft, Key.PageUp -> { stepByButtonOrKey(-1); true }
                    Key.DirectionRight, Key.PageDown -> { stepByButtonOrKey(1); true }
                    Key.Plus, Key.Equals, Key.NumPadAdd -> { zoomAt(1.25f, Offset(box.width / 2f, box.height / 2f)); true }
                    Key.Minus, Key.NumPadSubtract -> { zoomAt(1 / 1.25f, Offset(box.width / 2f, box.height / 2f)); true }
                    else -> false
                }
            }
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent()
                        if (event.type == PointerEventType.Scroll && !latestMenu) {
                            val change = event.changes.first()
                            val dy = change.scrollDelta.y
                            if (dy != 0f) {
                                zoomAt(if (dy < 0) 1.2f else 1 / 1.2f, change.position)
                                change.consume()
                            }
                        }
                    }
                }
            }
            // 常驻不随 state.index 重建的手势循环：支持快速连划切图、双指捏合平移、1× 方向锁与放大边缘切图
            .pointerInput(state) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    lastPressWasTouch = down.type != PointerType.Mouse
                    if (latestMenu) return@awaitEachGesture

                    val touch = lastPressWasTouch
                    isTouchGesture = touch
                    zoomJob?.cancel()
                    zoomJob = null
                    settleJob?.cancel()
                    settleJob = null
                    if (pendingPageDelta != 0) {
                        commitPendingStepIfNeeded()
                    } else if (abs(pageDragX) > 0.5f || dismissDragY > 0.5f) {
                        pageDragX = 0f
                        dismissDragY = 0f
                    }

                    val slopPx = viewConfiguration.touchSlop
                    val touchTracker = ImageViewerTouchTracker(slopPx)
                    val velocityTracker = VelocityTracker()
                    velocityTracker.addPointerInputChange(down)

                    var lastPinch = 0f
                    var lastCentroid = Offset.Zero
                    var hadMultiTouch = false
                    var mouseDragAccum = 0f
                    var mouseDragging = false
                    var settled = false

                    try {
                        var pressed: List<PointerInputChange>
                        do {
                            val event = awaitPointerEvent()
                            pressed = event.changes.filter { it.pressed }
                            val boxW = box.width.toFloat().coerceAtLeast(1f)
                            val boxH = box.height.toFloat().coerceAtLeast(1f)
                            val stridePx = boxW + latestPageGapPx

                            when {
                                pressed.size >= 2 -> {
                                    // 双指捏合缩放 + 双指质心同步平移
                                    hadMultiTouch = true
                                    if (abs(pageDragX) > 0.5f || dismissDragY > 0.5f) {
                                        pageDragX = 0f
                                        dismissDragY = 0f
                                    }
                                    val p0 = pressed[0].position
                                    val p1 = pressed[1].position
                                    val d = (p1 - p0).getDistance()
                                    val centroid = (p0 + p1) / 2f
                                    if (lastPinch > 0.5f && d > 0.5f) {
                                        val movedPan = panOffset + (centroid - lastCentroid)
                                        val (nextScale, nextOffset) = viewerZoomAt(
                                            currentScale = scale,
                                            currentOffset = movedPan,
                                            factor = d / lastPinch,
                                            anchor = centroid,
                                            boxW = boxW,
                                            boxH = boxH,
                                            imageAspect = currentAspect(),
                                            maxScale = MaxScale,
                                        )
                                        scale = nextScale
                                        panOffset = nextOffset
                                    }
                                    lastPinch = d
                                    lastCentroid = centroid
                                    pressed.forEach { it.consume() }
                                }
                                pressed.size == 1 -> {
                                    lastPinch = 0f
                                    val change = pressed.first()
                                    velocityTracker.addPointerInputChange(change)
                                    val delta = change.positionChange()
                                    if (delta != Offset.Zero) {
                                        if (hadMultiTouch) {
                                            // 双指松开一根后剩单指：继续平滑平移，不误判为 1× 翻页或点击关闭
                                            if (scale > 1.01f) {
                                                panOffset = viewerClampPan(scale, panOffset + delta, boxW, boxH, currentAspect())
                                            }
                                            change.consume()
                                        } else if (touch) {
                                            val update = touchTracker.onMove(
                                                delta = delta,
                                                scale = scale,
                                                currentPan = panOffset,
                                                boxW = boxW,
                                                boxH = boxH,
                                                stridePx = stridePx,
                                                imageAspect = currentAspect(),
                                                hasPrev = state.index > 0,
                                                hasNext = state.index < state.images.lastIndex,
                                            )
                                            panOffset = update.panOffset
                                            pageDragX = update.pageDragX
                                            dismissDragY = update.dismissDragY
                                            if (update.consumed) change.consume()
                                        } else if (scale > 1.01f) {
                                            // 桌面鼠标在放大后拖动平移（过微距阈值再消费，保证双击缩回不被鼠标微抖吃掉）
                                            mouseDragAccum += hypot(delta.x, delta.y)
                                            if (!mouseDragging && mouseDragAccum > slopPx * 0.5f) {
                                                mouseDragging = true
                                            }
                                            if (mouseDragging) {
                                                panOffset = viewerClampPan(scale, panOffset + delta, boxW, boxH, currentAspect())
                                                change.consume()
                                            }
                                        }
                                    }
                                }
                            }
                        } while (pressed.isNotEmpty())

                        isTouchGesture = false
                        settled = true

                        if (touch && !hadMultiTouch) {
                            val boxW = box.width.toFloat().coerceAtLeast(1f)
                            val boxH = box.height.toFloat().coerceAtLeast(1f)
                            val stridePx = boxW + latestPageGapPx
                            val velocity = velocityTracker.calculateVelocity()

                            when {
                                touchTracker.mode == ViewerTouchMode.VerticalDismiss && dismissDragY > 0.5f -> {
                                    val shouldClose = viewerShouldDismiss(
                                        dyPx = dismissDragY,
                                        velocityYPxPerSec = velocity.y,
                                        boxHeightPx = boxH,
                                        minFlingDistancePx = latestMinFlingDistPx,
                                        dismissFlingVelocityPxPerSec = latestDismissFlingVelPx,
                                    )
                                    val startY = dismissDragY
                                    val targetY = if (shouldClose) boxH else 0f
                                    val durationMs = pagerSettleDurationMs(targetY - startY, velocity.y, boxH)
                                    settleJob = scope.launch {
                                        if (durationMs > 0) {
                                            animate(
                                                initialValue = startY,
                                                targetValue = targetY,
                                                animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                            ) { v, _ -> dismissDragY = v }
                                        } else {
                                            dismissDragY = targetY
                                        }
                                        if (shouldClose) latestOnClose()
                                    }
                                }
                                abs(pageDragX) > 0.5f -> {
                                    val step = viewerHorizontalSettleStep(
                                        dxPx = touchTracker.rawPageDx,
                                        velocityXPxPerSec = velocity.x,
                                        boxWidthPx = boxW,
                                        minFlingDistancePx = latestMinFlingDistPx,
                                        flingVelocityPxPerSec = latestFlingVelPx,
                                        maxSlowDragPx = latestMaxSlowDragPx,
                                    )
                                    val canStep = (step > 0 && state.index < state.images.lastIndex) ||
                                        (step < 0 && state.index > 0)
                                    if (step != 0 && canStep) {
                                        pendingPageDelta = step
                                        val startX = pageDragX
                                        val targetX = if (step > 0) -stridePx else stridePx
                                        val durationMs = pagerSettleDurationMs(targetX - startX, velocity.x, stridePx)
                                        settleJob = scope.launch {
                                            if (durationMs > 0) {
                                                animate(
                                                    initialValue = startX,
                                                    targetValue = targetX,
                                                    animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                                ) { v, _ -> pageDragX = v }
                                            } else {
                                                pageDragX = targetX
                                            }
                                            if (pendingPageDelta == step) {
                                                pendingPageDelta = 0
                                                state.index = (state.index + step).coerceIn(0, state.images.lastIndex)
                                                scale = 1f
                                                panOffset = Offset.Zero
                                                pageDragX = 0f
                                                dismissDragY = 0f
                                            }
                                        }
                                    } else {
                                        val startX = pageDragX
                                        val durationMs = pagerSettleDurationMs(startX, velocity.x, stridePx)
                                        settleJob = scope.launch {
                                            if (durationMs > 0) {
                                                animate(
                                                    initialValue = startX,
                                                    targetValue = 0f,
                                                    animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                                ) { v, _ -> pageDragX = v }
                                            } else {
                                                pageDragX = 0f
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } finally {
                        if (!settled) isTouchGesture = false
                    }
                }
            }
            .pointerInput(state) {
                detectTapGestures(
                    onDoubleTap = { p ->
                        if (latestMenu) return@detectTapGestures
                        zoomJob?.cancel()
                        val fromScale = scale
                        val fromOffset = panOffset
                        val (toScale, toOffset) = if (fromScale > 1.01f) {
                            1f to Offset.Zero
                        } else {
                            viewerZoomAt(
                                currentScale = 1f,
                                currentOffset = Offset.Zero,
                                factor = 2.5f,
                                anchor = p,
                                boxW = box.width.toFloat(),
                                boxH = box.height.toFloat(),
                                imageAspect = currentAspect(),
                                maxScale = MaxScale,
                            )
                        }
                        zoomJob = scope.launch {
                            animate(
                                initialValue = 0f,
                                targetValue = 1f,
                                animationSpec = tween(durationMillis = 160, easing = PagerCubicOutEasing),
                            ) { t, _ ->
                                scale = fromScale + (toScale - fromScale) * t
                                panOffset = fromOffset + (toOffset - fromOffset) * t
                            }
                        }
                    },
                    onTap = { if (scale <= 1.01f && !latestMenu) latestOnClose() },
                    onLongPress = {
                        // 长按 = 底部菜单。只在手指上（鼠标长按不改桌面行为）。
                        if (lastPressWasTouch && !latestMenu) menu = true
                    },
                )
            },
    ) {
        val currentIdx = state.index
        val firstPage = (currentIdx - 1).coerceAtLeast(0)
        val lastPage = (currentIdx + 1).coerceAtMost(state.images.lastIndex)
        val stridePx = box.width.toFloat().coerceAtLeast(1f) + pageGapPx

        // 当前页与前 / 后相邻页按 pageIndex 固定组合身份：
        // 左右跟手滑入时 1:1 露出相邻图（中间留 16dp 黑缝），翻页落定那帧相邻页原地晋升为当前页，零闪烁、零重绑。
        for (pageIndex in firstPage..lastPage) {
            key(pageIndex) {
                val pageImage = state.images[pageIndex]
                val rel = pageIndex - currentIdx
                val isCurrent = rel == 0
                val pageModifier = if (isCurrent) {
                    Modifier
                        .fillMaxSize()
                        .offset { IntOffset(pageDragX.roundToInt(), dismissDragY.roundToInt()) }
                        .graphicsLayer {
                            scaleX = scale
                            scaleY = scale
                            translationX = panOffset.x
                            translationY = panOffset.y
                        }
                } else {
                    Modifier
                        .fillMaxSize()
                        .offset { IntOffset((rel * stridePx + pageDragX).roundToInt(), 0) }
                        .drawWithContent { if (abs(pageDragX) > 0.5f) drawContent() }
                }

                Box(pageModifier) {
                    if (loadedPages[pageIndex] != true) {
                        CircularProgressIndicator(
                            Modifier.align(Alignment.Center).size(28.dp),
                            color = Color.White.copy(alpha = 0.7f),
                            strokeWidth = 2.dp,
                        )
                    }
                    // 原图出来之前先垫着正文里已经加载好的那张
                    if (pageImage.thumb != null && pageImage.thumb != pageImage.url) {
                        AsyncImage(
                            model = pageImage.thumb,
                            contentDescription = null,
                            contentScale = ContentScale.Fit,
                            onSuccess = { s ->
                                val sz = s.painter.intrinsicSize
                                if (sz.width > 0f && sz.height > 0f && sz.width.isFinite() && sz.height.isFinite() && pageIndex !in aspectRatios) {
                                    aspectRatios[pageIndex] = sz.width / sz.height
                                }
                            },
                            modifier = Modifier.fillMaxSize(),
                        )
                    }
                    AsyncImage(
                        model = pageImage.url,
                        contentDescription = pageImage.caption,
                        contentScale = ContentScale.Fit,
                        onSuccess = { s ->
                            loadedPages[pageIndex] = true
                            val sz = s.painter.intrinsicSize
                            if (sz.width > 0f && sz.height > 0f && sz.width.isFinite() && sz.height.isFinite()) {
                                aspectRatios[pageIndex] = sz.width / sz.height
                            }
                        },
                        modifier = Modifier.fillMaxSize(),
                    )
                }
            }
        }

        val showDesktopArrows = !isTouchDevice && !lastPressWasTouch
        if (state.images.size > 1) {
            Text(
                "${state.index + 1} / ${state.images.size}",
                color = Color.White,
                modifier = Modifier.align(Alignment.TopStart).padding(16.dp)
                    .background(Color.Black.copy(alpha = 0.45f), CircleShape).padding(horizontal = 12.dp, vertical = 4.dp),
            )
            if (showDesktopArrows && state.index > 0) {
                ViewerArrow(Modifier.align(Alignment.CenterStart).padding(8.dp), left = true) { stepByButtonOrKey(-1) }
            }
            if (showDesktopArrows && state.index < state.images.lastIndex) {
                ViewerArrow(Modifier.align(Alignment.CenterEnd).padding(8.dp), left = false) { stepByButtonOrKey(1) }
            }
        }
        IconButton(
            onClick = onClose,
            modifier = Modifier.align(Alignment.TopEnd).padding(12.dp).background(Color.Black.copy(alpha = 0.45f), CircleShape),
        ) { Icon(Icons.Outlined.Close, contentDescription = "关闭", tint = Color.White) }

        message?.let {
            Text(
                it,
                color = Color.White,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = if (menu) 220.dp else 40.dp)
                    .background(Color.Black.copy(alpha = 0.7f), CircleShape).padding(horizontal = 16.dp, vertical = 8.dp),
            )
        } ?: run {
            image.caption?.takeIf { it.isNotBlank() }?.let {
                Text(
                    it,
                    color = Color.White,
                    modifier = Modifier.align(Alignment.BottomCenter).padding(24.dp)
                        .background(Color.Black.copy(alpha = 0.45f), CircleShape).padding(horizontal = 14.dp, vertical = 6.dp),
                )
            }
        }

        if (menu) {
            ViewerMenu(
                canSave = actions.canSaveToAlbum || menuPreview,
                canShare = actions.canShare || menuPreview,
                busy = busy,
                onDismiss = { menu = false; menuPreview = false },
                onCopyLink = {
                    clipboard.setText(AnnotatedString(image.url))
                    menu = false; menuPreview = false
                    toast("已复制图片链接")
                },
                onSave = {
                    menu = false; menuPreview = false
                    if (!busy) {
                        busy = true
                        toast("正在保存…")
                        val name = imageFileName(image.caption, state.index, fileStamp(), "jpg")
                        scope.launch {
                            val text = runCatching { actions.saveToAlbum(image.url, name) }
                                .getOrElse { "保存失败：${it.message ?: it::class.simpleName}" }
                            busy = false
                            toast(text)
                        }
                    }
                },
                onShare = {
                    menu = false; menuPreview = false
                    if (!busy) {
                        busy = true
                        scope.launch {
                            val text = runCatching { actions.share(image.url, image.caption) }
                                .getOrElse { "分享失败：${it.message ?: it::class.simpleName}" }
                            busy = false
                            if (text.isNotEmpty()) toast(text)
                        }
                    }
                },
            )
        }
    }
}

/**
 * 长按弹出的底部菜单（不是 `ModalBottomSheet`：查看器本身在一个 Popup 里，再套一层 Dialog 在安卓上返回键 /
 * 键盘 insets 都不好处理；这里就是一个画在查看层里的面板，点遮罩关掉）。
 */
@Composable
private fun ViewerMenu(
    canSave: Boolean,
    canShare: Boolean,
    busy: Boolean,
    onDismiss: () -> Unit,
    onCopyLink: () -> Unit,
    onSave: () -> Unit,
    onShare: () -> Unit,
) {
    Box(Modifier.fillMaxSize()) {
        // 遮罩：点一下关掉菜单（不关查看器）
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)).clickable(onClick = onDismiss))
        Column(
            Modifier.align(Alignment.BottomCenter).fillMaxWidth()
                .background(SheetBackground, RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp))
                .padding(vertical = 8.dp),
        ) {
            Box(Modifier.align(Alignment.CenterHorizontally).padding(top = 4.dp, bottom = 8.dp).size(width = 36.dp, height = 4.dp)
                .background(Color.White.copy(alpha = 0.3f), CircleShape))
            if (canSave) MenuRow(Icons.Outlined.Download, if (busy) "正在保存…" else "保存到相册", enabled = !busy, onClick = onSave)
            if (canShare) MenuRow(Icons.Outlined.Share, "分享图片", enabled = !busy, onClick = onShare)
            MenuRow(Icons.Outlined.Link, "复制图片链接", enabled = !busy, onClick = onCopyLink)
            MenuRow(Icons.Outlined.Close, "取消", enabled = true, onClick = onDismiss)
            Box(Modifier.height(6.dp))
        }
    }
}

@Composable
private fun MenuRow(icon: ImageVector, label: String, enabled: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onClick).padding(horizontal = 22.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Color.White.copy(alpha = if (enabled) 0.9f else 0.4f))
        Text(label, color = Color.White.copy(alpha = if (enabled) 1f else 0.4f))
    }
}

@Composable
private fun ViewerArrow(modifier: Modifier, left: Boolean, onClick: () -> Unit) {
    IconButton(onClick = onClick, modifier = modifier.background(Color.Black.copy(alpha = 0.45f), CircleShape)) {
        Icon(
            if (left) Icons.AutoMirrored.Outlined.KeyboardArrowLeft else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
            contentDescription = if (left) "上一张" else "下一张",
            tint = Color.White,
        )
    }
}
