package feedhub.app.video

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Subtitles
import androidx.compose.material.icons.filled.SubtitlesOff
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.input.pointer.util.addPointerInputChange
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.lerp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import feedhub.app.AppGraph
import feedhub.app.detail.CommentSortChips
import feedhub.app.detail.CommentsState
import feedhub.app.detail.DetailDebug
import feedhub.app.detail.DetailPhoto
import feedhub.app.detail.DetailThumbnails
import feedhub.app.detail.LocalDetailHost
import feedhub.app.detail.LocalDetailThumbnails
import feedhub.app.detail.Poster
import feedhub.app.detail.commentsList
import feedhub.app.detail.fullTime
import feedhub.app.detail.shareText
import feedhub.app.detail.shareTitle
import feedhub.app.feed.Avatar
import feedhub.app.mine.ConfirmDialog
import feedhub.app.ui.compactCount
import feedhub.core.Comment
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FollowSource
import feedhub.core.Media
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.roundToInt

private val Accent = Color(0xFFFB7299)
private val OnVideo = Color.White

/** 桌面端鼠标移开时叠层（左下文字、右侧按钮）的平时透明度；触屏端没有 hover，平时保持 1f 清晰可读。 */
private const val DesktopOverlayIdleAlpha = 0.85f

/** 窄屏半屏评论抽屉占容器高度的比例（上方 48% 留给视频完整缩放居中播放）。 */
private const val HalfCommentsHeightFraction = 0.52f

/** 长按倍速胶囊松手后停留多久。 */
private const val SPEED_HINT_LINGER_MS = 900L

/**
 * 竖屏视频的沉浸界面（抖音 / TikTok 式）：**视频详情页的另一种样子**。
 * 尺寸跟着被给的地方走——双栏（平板横屏 / 桌面）时 [feedhub.app.SplitPane] 只给它右栏那么大，
 * 全屏打开方式 / 手机竖屏才占满整个窗口；评论面板也只在它里面滑出 / 升起，不盖到左栏去。
 *
 *    松手结合速度（`VelocityTracker`）与距离判定，切页顺势滑出整屏，触屏连划不被滚轮节流吞掉；
 * 2. **零延迟单击暂停 + 中央半透明播放标**：竖屏沉浸不挂双击放大（横屏片源走右侧专门的「横屏全屏」按钮），
 *    单击暂停 / 继续 0ms 立即响应；暂停或播完停住时画面正中显示半透明 `▶`；
 * 3. **全屏统一触控热区 + 底部进度条调进度**：单击暂停与长按 3 倍速（含上滑锁定）覆盖整个沉浸舞台（含横屏视频上下留白区）；
 *    屏幕中上部取消全屏横滑拉进度，统一归底部进度条热区（横滑加粗轨道 + 大字时间预览 + 自动淡出叠层清屏，竖滑直通翻页）；
 * 4. **半屏评论联动**：窄屏半屏评论升起时，视频平滑缩放到上半屏居中完整显示；点上方视频区直接收回评论弹窗（不暂停、不翻页）；
 *    评论顶栏拖拽与列表到顶继续下拉均可跟手收回评论面板。
 */
@Composable
fun ImmersionScreen(
    graph: AppGraph,
    item: FeedItem,
    detail: FeedDetail?,
    session: PlaybackSession?,
    danmaku: DanmakuSettings,
    onDanmakuChange: (DanmakuSettings) -> Unit,
    loadError: String?,
    comments: CommentsState,
    poster: Poster,
    commentsOpen: Boolean,
    onCommentsOpenChange: (Boolean) -> Unit,
    onClose: () -> Unit,
    onToggleZoom: () -> Unit,
    /** 调音量时画面中间短暂显示的一行字（null = 不显示）；由 [VideoDetailScreen] 按共用的音量算好传进来。 */
    volumeText: String?,
    /** "要登录才能播"时那个直接打开登录的按钮（见 [PlayerView]）；null = 这个平台没有能在这里打开的登录。 */
    onLogin: (() -> Unit)? = null,
    /** 滚轮 / 键盘触发的上一条（-1）/ 下一条（+1），带节流。 */
    onStep: (Int) -> Unit = {},
    /** 触屏整屏划页松手结算触发的上一条 / 下一条（不走滚轮节流，返回是否真的切换成功）。 */
    onTouchStep: (Int) -> Boolean = { delta -> onStep(delta); true },
    /** 下一条 / 上一条的条目：拖动时紧挨在当前页下方 / 上方 1:1 露出完整预览。null = 没有那一条。 */
    nextItem: FeedItem? = null,
    previousItem: FeedItem? = null,
    /** 切换被挡下时的一句话（"已经是最后一条了" / "正在加载更多…" / "这条不在信息流里"），由外面自动消失。 */
    hint: String? = null,
    /** 详情加载失败时的重试；null = 不显示重试（例如播放器自己报的错，那里有自己的重试）。 */
    onRetry: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    // 性能记录：页面名临时盖成"沉浸"（退出沉浸就恢复成详情页，见 PerfScreen）
    DisposableEffect(Unit) {
        feedhub.app.perf.PerfHooks.overlay("沉浸")
        onDispose { feedhub.app.perf.PerfHooks.overlay(null) }
    }
    val host = LocalDetailHost.current
    val shown = detail?.item ?: item
    val ratio = immersionAspect(item, detail?.video, session?.info, session?.part, session?.state?.videoWidth ?: 0, session?.state?.videoHeight ?: 0)
    val cover = itemCover(item, shown)
    val coverPlaceholder = cover?.let { c -> LocalDetailThumbnails.current.hint(c)?.thumbnail }
    val hoverSource = remember { MutableInteractionSource() }
    val hovered by hoverSource.collectIsHoveredAsState()
    val contentId = (shown.detail as? DetailTarget.Native)?.id ?: shown.id

    val followState = detail?.follow
    val followSource = graph.source(shown.platform) as? FollowSource
    val currentHost = rememberUpdatedState(host)
    val scope = rememberCoroutineScope()
    val follow = remember(detail, followState, followSource, scope) {
        if (!immersionFollowVisible(detail, followSource)) null
        else followState?.let { s -> followSource?.let { src -> ImmersionFollow(s, src, scope, onMessage = { currentHost.value?.showToast(it) }) } }
    }

    var volumeOpen by remember(item.key) { mutableStateOf(false) }
    // 底部进度条正在横向拖动时的比例（0~1）；非 null 时自动淡出文案与右侧按钮并显示大号时间预览
    var scrubFraction by remember(item.key) { mutableStateOf<Float?>(null) }

    // 舞台长按 3 倍速的顶部小胶囊提示
    val speedPressing = session?.speedPressing == true
    var speedHint by remember(item.key) { mutableStateOf<PlayerHint?>(null) }
    var speedHintTick by remember(item.key) { mutableIntStateOf(0) }
    LaunchedEffect(speedHintTick, speedPressing) {
        if (speedHint == null || speedPressing) return@LaunchedEffect
        delay(SPEED_HINT_LINGER_MS)
        speedHint = null
    }

    // 整屏上下划页位移（像素，往下为正）：常驻不随 item.key 销毁，拖动阶段同步写浮点状态（零协程延迟），
    // 且只在布局阶段 Modifier.offset 与绘制阶段 drawWithContent 里读取，划页全程零重组、零重测量。
    var pageOffsetPx by remember { mutableFloatStateOf(0f) }
    var pendingStep by remember { mutableIntStateOf(0) }
    var isTouchDragging by remember { mutableStateOf(false) }
    var settleJob by remember { mutableStateOf<Job?>(null) }
    LaunchedEffect(item.key) {
        if (!isTouchDragging && pendingStep == 0) {
            settleJob?.cancel()
            settleJob = null
            pageOffsetPx = 0f
        }
    }

    val density = LocalDensity.current
    val haptic = LocalHapticFeedback.current

    SideEffect {
        DetailDebug.immersionVolume = { open -> volumeOpen = open; true }
        DetailDebug.immersionUnfollow = { follow?.click(); follow != null }
    }

    BoxWithConstraints(modifier.fillMaxSize().clipToBounds().background(Color.Black)) {
        val paneW = maxWidth
        val paneH = maxHeight
        // 评论摆哪边只看窗口与画面尺寸（不绑 commentsOpen，免得关闭动画期间从右栏跳到底栏）
        val commentsOnSide = immersionCommentsOnSide(paneW, paneH, ratio)
        val sideOpen = commentsOpen && commentsOnSide
        val halfSheetOpen = commentsOpen && !commentsOnSide

        // 窄屏半屏评论的展开进度（0 = 收起，1 = 完全展开）：点按钮开合与手指拖拽下拉共用同一个连续状态
        val sheetProgress = remember(item.key) { Animatable(if (halfSheetOpen) 1f else 0f) }
        LaunchedEffect(halfSheetOpen) {
            val target = if (halfSheetOpen) 1f else 0f
            sheetProgress.animateTo(target, animationSpec = tween(durationMillis = if (halfSheetOpen) 240 else 200, easing = FastOutSlowInEasing))
        }
        val sheetFraction = if (commentsOnSide) 0f else sheetProgress.value.coerceIn(0f, 1f)
        val sheetHeight = paneH * HalfCommentsHeightFraction
        val sheetHeightPx = with(density) { sheetHeight.toPx() }.coerceAtLeast(1f)

        val videoW = if (sideOpen) paneW - ImmersionCommentsPanelWidth else paneW
        // 半屏评论展开时，视频舞台平滑缩放到上半屏居中完整显示
        val stageH = lerp(paneH, paneH - sheetHeight, sheetFraction)
        val videoSize = fitVideoSize(videoW, stageH, ratio)

        val danmakuBand = (stageH - videoSize.height) / 2 - if (sheetFraction < 0.2f) ImmersionTopRow else 8.dp
        val danmakuAbove = danmakuBand >= ImmersionDanmakuMinBand

        val paneHeightPx = with(density) { paneH.toPx() }.coerceAtLeast(1f)
        val minFlingDistPx = with(density) { ImmersionFlingMinDistance.toPx() }
        val flingVelPx = with(density) { ImmersionFlingVelocity.toPx() }
        val maxSlowDragPx = with(density) { ImmersionSlowDragMax.toPx() }

        // 拖进度条 / 长按倍速 / 半屏评论展开时自动把四周文案和按钮淡出清屏
        val clearOverlays = scrubFraction != null || speedPressing
        val baseOverlayAlpha = when {
            clearOverlays -> 0f
            isTouchDevice -> 1f
            hovered -> 1f
            else -> DesktopOverlayIdleAlpha
        }
        val animatedOverlayAlpha by animateFloatAsState(
            targetValue = baseOverlayAlpha,
            animationSpec = tween(durationMillis = 140),
            label = "immersionOverlayAlpha",
        )
        val overlayAlpha = (animatedOverlayAlpha * (1f - sheetFraction * 2.2f).coerceIn(0f, 1f)).coerceIn(0f, 1f)

        val currentNext by rememberUpdatedState(nextItem)
        val currentPrev by rememberUpdatedState(previousItem)
        val currentTouchStep by rememberUpdatedState(onTouchStep)
        val currentStep by rememberUpdatedState(onStep)
        val currentPaneHeightPx by rememberUpdatedState(paneHeightPx)
        val currentMinFlingDistPx by rememberUpdatedState(minFlingDistPx)
        val currentFlingVelPx by rememberUpdatedState(flingVelPx)
        val currentMaxSlowDragPx by rememberUpdatedState(maxSlowDragPx)

        // 外层静止视口承载整屏上下划手势：
        // 内部相邻预览页与当前视频舞台均通过布局阶段 Modifier.offset { IntOffset(...) } 同步推进，
        // 让 Android Media3 的 SurfaceView 硬件视频窗口坐标在 RenderThread 120Hz 紧锁页面不脱节。
        Box(
            Modifier.width(videoW).fillMaxHeight().clipToBounds()
                .immersionSwipes(
                    onDown = {
                        isTouchDragging = true
                        settleJob?.cancel()
                        settleJob = null
                        val commit = pendingStep
                        if (commit != 0) {
                            pendingStep = 0
                            if (currentTouchStep(commit)) {
                                pageOffsetPx = 0f
                            }
                        } else if (abs(pageOffsetPx) > 0.5f) {
                            pageOffsetPx = 0f
                        }
                    },
                    onDrag = { rawDy ->
                        val hasNeighbor = if (rawDy < 0f) currentNext != null else if (rawDy > 0f) currentPrev != null else true
                        pageOffsetPx = immersionDragOffset(rawDy, currentPaneHeightPx, hasNeighbor = hasNeighbor)
                    },
                    onSettle = { rawDy, velocityY ->
                        isTouchDragging = false
                        val hPx = currentPaneHeightPx
                        val step = immersionSettleStep(
                            dyPx = rawDy,
                            velocityYPxPerSec = velocityY,
                            paneHeightPx = hPx,
                            minFlingDistancePx = currentMinFlingDistPx,
                            flingVelocityPxPerSec = currentFlingVelPx,
                            maxSlowDragPx = currentMaxSlowDragPx,
                        )
                        val targetNeighbor = when {
                            step > 0 -> currentNext
                            step < 0 -> currentPrev
                            else -> null
                        }
                        when {
                            step != 0 && targetNeighbor != null -> {
                                pendingStep = step
                                val endY = if (step > 0) -hPx else hPx
                                val startY = pageOffsetPx
                                val durationMs = pagerSettleDurationMs(endY - startY, velocityY, hPx)
                                settleJob?.cancel()
                                settleJob = scope.launch {
                                    if (durationMs > 0) {
                                        animate(
                                            initialValue = startY,
                                            targetValue = endY,
                                            animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                        ) { v, _ -> pageOffsetPx = v }
                                    } else {
                                        pageOffsetPx = endY
                                    }
                                    if (pendingStep == step) {
                                        pendingStep = 0
                                        if (currentTouchStep(step)) {
                                            pageOffsetPx = 0f
                                        } else {
                                            val backMs = pagerSettleDurationMs(pageOffsetPx, 0f, hPx)
                                            if (backMs > 0) {
                                                animate(
                                                    initialValue = pageOffsetPx,
                                                    targetValue = 0f,
                                                    animationSpec = tween(durationMillis = backMs, easing = PagerCubicOutEasing),
                                                ) { v, _ -> pageOffsetPx = v }
                                            } else {
                                                pageOffsetPx = 0f
                                            }
                                        }
                                    }
                                }
                            }
                            step != 0 -> {
                                // 到列表头 / 尾：触发提示与 loadMore，画面平滑弹回原位
                                currentTouchStep(step)
                                val startY = pageOffsetPx
                                val durationMs = pagerSettleDurationMs(startY, velocityY, hPx)
                                settleJob?.cancel()
                                if (durationMs > 0) {
                                    settleJob = scope.launch {
                                        animate(
                                            initialValue = startY,
                                            targetValue = 0f,
                                            animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                        ) { v, _ -> pageOffsetPx = v }
                                    }
                                } else {
                                    pageOffsetPx = 0f
                                }
                            }
                            else -> {
                                val startY = pageOffsetPx
                                val durationMs = pagerSettleDurationMs(startY, velocityY, hPx)
                                settleJob?.cancel()
                                if (durationMs > 0) {
                                    settleJob = scope.launch {
                                        animate(
                                            initialValue = startY,
                                            targetValue = 0f,
                                            animationSpec = tween(durationMillis = durationMs, easing = PagerCubicOutEasing),
                                        ) { v, _ -> pageOffsetPx = v }
                                    }
                                } else {
                                    pageOffsetPx = 0f
                                }
                            }
                        }
                    },
                    onAbort = { isTouchDragging = false },
                    onWheelStep = { delta -> currentStep(delta) },
                    suppressed = {
                        session?.speedPressing == true || scrubFraction != null || halfSheetOpen || sheetFraction > 0.05f
                    },
                ),
        ) {
            if (nextItem != null) {
                key("next", nextItem.key) {
                    NeighborStagePreview(
                        neighbor = nextItem,
                        paneW = videoW,
                        paneH = paneH,
                        favorite = graph.library.isFavorite(nextItem.key),
                        muted = session?.let { it.state.muted || it.state.volume == 0 } ?: AppVolume.muted,
                        volume = session?.state?.volume ?: AppVolume.percent,
                        modifier = Modifier
                            .width(videoW)
                            .fillMaxHeight()
                            .offset { IntOffset(0, (currentPaneHeightPx + pageOffsetPx).roundToInt()) }
                            .drawWithContent { if (pageOffsetPx < -0.5f) drawContent() },
                    )
                }
            }
            if (previousItem != null) {
                key("prev", previousItem.key) {
                    NeighborStagePreview(
                        neighbor = previousItem,
                        paneW = videoW,
                        paneH = paneH,
                        favorite = graph.library.isFavorite(previousItem.key),
                        muted = session?.let { it.state.muted || it.state.volume == 0 } ?: AppVolume.muted,
                        volume = session?.state?.volume ?: AppVolume.percent,
                        modifier = Modifier
                            .width(videoW)
                            .fillMaxHeight()
                            .offset { IntOffset(0, (-currentPaneHeightPx + pageOffsetPx).roundToInt()) }
                            .drawWithContent { if (pageOffsetPx > 0.5f) drawContent() },
                    )
                }
            }

            // 当前视频舞台
            Box(
                Modifier.width(videoW).height(stageH).hoverable(hoverSource)
                    .offset { IntOffset(0, pageOffsetPx.roundToInt()) },
            ) {
            // 1. 铺底：封面的模糊放大版
            if (cover != null) {
                DetailPhoto(
                    url = cover,
                    placeholder = coverPlaceholder,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    placeholderColor = Color.Black,
                    modifier = Modifier.fillMaxSize().graphicsLayer { scaleX = 1.15f; scaleY = 1.15f }.blur(32.dp),
                )
            }
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)))

            // 2. 画面：按当前舞台高度适配居中（PlayerView 内部手势关闭，由第 5 层全舞台统一接管）
            Box(Modifier.align(Alignment.Center).size(videoSize)) {
                when {
                    session != null && session.engine.unavailable == null ->
                        PlayerView(
                            session = session,
                            danmaku = danmaku,
                            onDanmakuChange = onDanmakuChange,
                            zoomed = false,
                            onToggleZoom = onToggleZoom,
                            modifier = Modifier.fillMaxSize(),
                            controls = false,
                            poster = cover,
                            posterPlaceholder = coverPlaceholder,
                            onLogin = onLogin,
                            drawDanmaku = !danmakuAbove,
                            interactive = false,
                            showCenterStatus = false,
                        )
                    cover != null ->
                        DetailPhoto(
                            url = cover,
                            placeholder = coverPlaceholder,
                            contentDescription = null,
                            contentScale = ContentScale.Fit,
                            placeholderColor = Color.Black,
                            modifier = Modifier.fillMaxSize(),
                        )
                    else -> CircularProgressIndicator(Modifier.align(Alignment.Center).size(32.dp), color = OnVideo, strokeWidth = 3.dp)
                }
            }

            // 3. 横屏片源画面上方的弹幕带
            val timeline = session?.danmakuTimeline
            if (danmakuAbove && session != null && session.danmakuOn && timeline != null && session.engine.unavailable == null && session.state.hasFrame) {
                val topOffset = if (sheetFraction < 0.2f) ImmersionTopRow else 8.dp
                Box(
                    Modifier.align(Alignment.TopCenter).padding(top = topOffset)
                        .size(videoSize.width, danmakuBand).clipToBounds(),
                ) {
                    DanmakuLayer(timeline, session.state, danmaku, Modifier.fillMaxSize(), onSurface = false)
                }
            }

            // 4. 上下渐变遮罩（半屏评论展开时淡出，保持上半屏画面通透）
            if (overlayAlpha > 0.01f) {
                Box(
                    Modifier.fillMaxWidth().height(96.dp).align(Alignment.TopCenter)
                        .graphicsLayer { alpha = overlayAlpha }
                        .background(Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.55f), Color.Transparent))),
                )
                Box(
                    Modifier.fillMaxWidth().height(200.dp).align(Alignment.BottomCenter)
                        .graphicsLayer { alpha = overlayAlpha }
                        .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.65f)))),
                )
            }

            // 5. 全舞台统一交互层：
            //    - 半屏评论展开时（用户明确要求）：点视频不触发暂停，直接收回评论弹窗；向下拖也可连带收起评论
            //    - 平时：全屏范围（含上下模糊留白区）单击 0ms 零延迟暂停/继续 + 触屏长按 3 倍速与上滑锁定
            if (halfSheetOpen) {
                Box(
                    Modifier.fillMaxSize()
                        .pointerInput(onCommentsOpenChange) {
                            detectTapGestures(onTap = { onCommentsOpenChange(false) })
                        }
                        .pointerInput(sheetHeightPx) {
                            val vt = VelocityTracker()
                            detectVerticalDragGestures(
                                onDragStart = { vt.resetTracking() },
                                onVerticalDrag = { change, dragAmount ->
                                    vt.addPointerInputChange(change)
                                    change.consume()
                                    scope.launch {
                                        sheetProgress.snapTo((sheetProgress.value - dragAmount / sheetHeightPx).coerceIn(0f, 1f))
                                    }
                                },
                                onDragEnd = {
                                    val vy = vt.calculateVelocity().y
                                    val draggedPx = (1f - sheetProgress.value) * sheetHeightPx
                                    if (commentSheetShouldDismiss(draggedPx, sheetHeightPx, vy, flingVelPx)) {
                                        onCommentsOpenChange(false)
                                    } else {
                                        scope.launch { sheetProgress.animateTo(1f, tween(durationMillis = 200, easing = FastOutSlowInEasing)) }
                                    }
                                },
                                onDragCancel = {
                                    scope.launch { sheetProgress.animateTo(1f, tween(durationMillis = 200, easing = FastOutSlowInEasing)) }
                                },
                            )
                        },
                )
            } else {
                val stageGesture = if (isTouchDevice) {
                    Modifier.immersionStageTouchGestures(
                        onTap = { session?.togglePlay() },
                        onLongPressStart = {
                            val s = session ?: return@immersionStageTouchGestures
                            val wasLocked = s.speedLocked
                            s.longPressStart()
                            haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                            speedHint = if (wasLocked) {
                                PlayerHint.Speed(s.baseSpeed, locked = false, note = "上滑取消锁定")
                            } else {
                                PlayerHint.Speed(PlayerGestureMath.LONG_PRESS_SPEED, locked = false, note = "上滑锁定")
                            }
                            speedHintTick++
                        },
                        onLongPressSwipeUp = {
                            val s = session ?: return@immersionStageTouchGestures
                            val locked = s.longPressSwipeUp()
                            haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                            speedHint = if (locked) {
                                PlayerHint.Speed(PlayerGestureMath.LONG_PRESS_SPEED, locked = true)
                            } else {
                                PlayerHint.Speed(s.baseSpeed, locked = false, note = "已取消锁定")
                            }
                            speedHintTick++
                        },
                        onLongPressEnd = {
                            session?.longPressEnd()
                            speedHintTick++
                        },
                        density = density.density,
                    )
                } else {
                    // 桌面鼠标：单击零延迟暂停 / 继续，不挂双击放大
                    Modifier.pointerInput(session) {
                        detectTapGestures(onTap = { session?.togglePlay() })
                    }
                }
                Box(stageGesture.fillMaxSize())
            }

            // 6. 中央状态层：暂停 / 播完时的半透明播放角标 + 缓冲圈 / 错误说明 / 登录与重试按钮 + 倍速顶部胶囊
            val showPausedBadge = session != null && session.engine.unavailable == null &&
                immersionPausedIndicatorVisible(
                    playing = session.state.playing,
                    buffering = session.state.buffering,
                    loading = session.loading,
                    hasFrame = session.state.hasFrame,
                    ended = session.state.ended,
                    hasError = session.error != null || session.state.error != null,
                )
            AnimatedVisibility(
                visible = showPausedBadge,
                enter = fadeIn(tween(120)),
                exit = fadeOut(tween(120)),
                modifier = Modifier.align(Alignment.Center),
            ) {
                Box(
                    Modifier.size(60.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.36f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Filled.PlayArrow,
                        contentDescription = "继续播放",
                        tint = Color.White.copy(alpha = 0.85f),
                        modifier = Modifier.size(38.dp),
                    )
                }
            }

            if (session != null && session.engine.unavailable == null) {
                CenterStatus(session, onLogin)
            } else {
                val note = session?.engine?.unavailable ?: loadError
                if (note != null) {
                    Surface(
                        shape = RoundedCornerShape(10.dp),
                        color = Color.Black.copy(alpha = 0.62f),
                        modifier = Modifier.align(Alignment.Center).padding(24.dp),
                    ) {
                        Column(
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(6.dp),
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
                        ) {
                            Text(note, color = OnVideo, fontSize = 13.sp)
                            if (onRetry != null) {
                                TextButton(onClick = onRetry) { Text("重试", color = Accent) }
                            }
                        }
                    }
                }
            }
            PlayerHintOverlay(speedHint)

            // 7. 顶栏、右侧按钮列、左下作者文案（半屏评论展开时隐藏，拖进度/长按倍速时自动淡出清屏）
            if (!halfSheetOpen && sheetFraction < 0.45f) {
                IconButton(
                    onClick = onClose,
                    modifier = Modifier.align(Alignment.TopStart).padding(4.dp).graphicsLayer { alpha = overlayAlpha },
                ) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回", tint = OnVideo) }

                run {
                    val author = shown.author
                    var menu by remember { mutableStateOf(false) }
                    Box(Modifier.align(Alignment.TopEnd).padding(4.dp).graphicsLayer { alpha = overlayAlpha }) {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, contentDescription = "更多", tint = OnVideo) }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            if (session?.hasDanmaku == true) {
                                DropdownMenuItem(
                                    text = { Text("弹幕设置") },
                                    leadingIcon = { Icon(Icons.Filled.Tune, contentDescription = null) },
                                    onClick = {
                                        menu = false
                                        session.danmakuPanelOpen = true
                                        session.poke()
                                    },
                                )
                            }
                            if (author != null) {
                                DropdownMenuItem(
                                    text = { Text("屏蔽作者") },
                                    leadingIcon = { Icon(Icons.Outlined.Block, contentDescription = null) },
                                    onClick = {
                                        menu = false
                                        graph.library.block(author)
                                        host?.closeAll() ?: onClose()
                                    },
                                )
                            }
                        }
                    }
                }

                ImmersionActions(
                    graph = graph,
                    item = shown,
                    session = session,
                    follow = follow,
                    commentsOpen = commentsOpen,
                    onComments = { onCommentsOpenChange(!commentsOpen) },
                    volumeOpen = volumeOpen,
                    onVolumeOpenChange = { volumeOpen = it },
                    landscape = isLandscapeVideo(ratio),
                    onFullscreen = onToggleZoom,
                    overlayAlpha = overlayAlpha,
                    modifier = Modifier.align(Alignment.BottomEnd).padding(end = 6.dp, bottom = 64.dp),
                )

                ImmersionInfo(
                    item = shown,
                    overlayAlpha = overlayAlpha,
                    interactive = !clearOverlays,
                    modifier = Modifier.align(Alignment.BottomStart).padding(start = 14.dp, end = 76.dp, bottom = 26.dp),
                )
            }

            // 8. 底部进度条拖动时的大号时间预览（"01:24 / 04:10"，清屏后居中悬在进度条上方，手指不挡）
            val activeScrub = scrubFraction
            if (activeScrub != null && session != null && session.state.durationMs > 0) {
                val totalMs = session.state.durationMs
                val targetMs = (activeScrub * totalMs).toLong().coerceIn(0L, totalMs)
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 52.dp)
                        .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(50))
                        .padding(horizontal = 16.dp, vertical = 6.dp),
                ) {
                    Text(clock(targetMs), color = OnVideo, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
                    Text("  /  ${clock(totalMs)}", color = OnVideo.copy(alpha = 0.65f), fontSize = 18.sp, fontWeight = FontWeight.Medium)
                }
            }

            // 9. 底边进度条（横向拖动调进度、点按跳转、竖滑放行给外层切视频）
            if (session != null && session.engine.unavailable == null && !halfSheetOpen && sheetFraction < 0.45f) {
                ImmersionProgress(
                    session = session,
                    onScrubFractionChange = { scrubFraction = it },
                    modifier = Modifier.align(Alignment.BottomCenter).fillMaxWidth().padding(horizontal = 10.dp),
                )
            }

            // 10. 边界提示 / 音量中央短提示 / 竖向音量面板
            if (hint != null && volumeText == null) {
                Surface(
                    shape = RoundedCornerShape(50),
                    color = Color.Black.copy(alpha = 0.66f),
                    modifier = Modifier.align(Alignment.Center).padding(16.dp),
                ) {
                    Text(hint, color = OnVideo, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 14.dp, vertical = 8.dp))
                }
            }

            if (volumeText != null) {
                Surface(
                    shape = RoundedCornerShape(50),
                    color = Color.Black.copy(alpha = 0.66f),
                    modifier = Modifier.align(Alignment.Center).padding(16.dp),
                ) {
                    Text(volumeText, color = OnVideo, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 14.dp, vertical = 8.dp))
                }
            }

            if (volumeOpen && session != null && session.engine.unavailable == null && !halfSheetOpen) {
                Box(Modifier.fillMaxSize().immersionClickable { volumeOpen = false })
                VolumePanel(
                    volume = session.state.volume,
                    muted = session.state.muted,
                    onVolume = { session.setVolume(it) },
                    onToggleMute = { session.toggleMute() },
                    modifier = Modifier.align(Alignment.CenterEnd).padding(end = VolumePanelEndPadding),
                )
            }
            }
        }

        // 评论面板：宽屏从右侧滑出；窄屏从底部升起半屏并支持顶栏下拉与列表到顶继续下拉跟手收回
        if (commentsOnSide) {
            AnimatedVisibility(
                visible = commentsOpen,
                enter = slideInHorizontally { it } + fadeIn(),
                exit = slideOutHorizontally { it } + fadeOut(),
                modifier = Modifier.align(Alignment.CenterEnd),
            ) {
                ImmersionComments(
                    comments = comments,
                    poster = poster,
                    total = shown.stats.comments,
                    bottomSheet = false,
                    onOpenReplies = { host?.openReplies(it, poster, contentId) },
                    onClose = { onCommentsOpenChange(false) },
                    modifier = Modifier.width(ImmersionCommentsPanelWidth).fillMaxHeight(),
                )
            }
        } else if (halfSheetOpen || sheetFraction > 0.001f) {
            ImmersionComments(
                comments = comments,
                poster = poster,
                total = shown.stats.comments,
                bottomSheet = true,
                onOpenReplies = { host?.openReplies(it, poster, contentId) },
                onClose = { onCommentsOpenChange(false) },
                onSheetDragDelta = { dy ->
                    scope.launch {
                        sheetProgress.snapTo((sheetProgress.value - dy / sheetHeightPx).coerceIn(0f, 1f))
                    }
                },
                onSheetDragSettle = { vy ->
                    val draggedPx = (1f - sheetProgress.value) * sheetHeightPx
                    if (commentSheetShouldDismiss(draggedPx, sheetHeightPx, vy, flingVelPx)) {
                        onCommentsOpenChange(false)
                    } else {
                        scope.launch { sheetProgress.animateTo(1f, tween(durationMillis = 200, easing = FastOutSlowInEasing)) }
                    }
                },
                currentDragPx = { (1f - sheetProgress.value) * sheetHeightPx },
                modifier = Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(sheetHeight)
                    .graphicsLayer { translationY = sheetHeightPx * (1f - sheetFraction) },
            )
        }
    }
}

private fun itemCover(primary: FeedItem, fallback: FeedItem = primary): String? =
    primary.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: fallback.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: fallback.media.filterIsInstance<Media.Image>().firstOrNull()?.url

/**
 * 相邻上一条 / 下一条视频在 1:1 跟手推移期间的整屏预览页：版式与真进入该条时的首帧一致
 * （模糊封面铺底 + 居中清晰等比封面 + 上下渐变 + 顶栏与右侧操作列 + 左下角作者标题），
 * 直接用 [AsyncImage] 预解码进 Coil 内存缓存（不污染 `DetailDebug.imagesTotal`），滑到位松手切换时零跳变、零闪跳。
 */
@Composable
private fun NeighborStagePreview(
    neighbor: FeedItem,
    paneW: Dp,
    paneH: Dp,
    favorite: Boolean,
    muted: Boolean,
    volume: Int,
    modifier: Modifier = Modifier,
) {
    val cover = itemCover(neighbor)
    val thumbs = remember(neighbor.key) { DetailThumbnails.of(neighbor.key) }
    val placeholder = cover?.let { thumbs.hint(it)?.thumbnail }
    val ratio = immersionAspect(neighbor)
    val videoSize = fitVideoSize(paneW, paneH, ratio)

    Box(modifier.clipToBounds().background(Color.Black)) {
        if (cover != null) {
            val bgModifier = Modifier.fillMaxSize().graphicsLayer { scaleX = 1.15f; scaleY = 1.15f }.blur(32.dp)
            if (placeholder != null && placeholder != cover) {
                AsyncImage(model = placeholder, contentDescription = null, contentScale = ContentScale.Crop, modifier = bgModifier)
            }
            AsyncImage(model = cover, contentDescription = null, contentScale = ContentScale.Crop, modifier = bgModifier)
        }
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)))
        if (cover != null) {
            Box(Modifier.align(Alignment.Center).size(videoSize)) {
                if (placeholder != null && placeholder != cover) {
                    AsyncImage(model = placeholder, contentDescription = null, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxSize())
                }
                AsyncImage(model = cover, contentDescription = null, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxSize())
            }
        }
        Box(
            Modifier.fillMaxWidth().height(96.dp).align(Alignment.TopCenter)
                .background(Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.55f), Color.Transparent))),
        )
        Box(
            Modifier.fillMaxWidth().height(200.dp).align(Alignment.BottomCenter)
                .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.65f)))),
        )
        Box(Modifier.align(Alignment.TopStart).padding(4.dp).size(48.dp), contentAlignment = Alignment.Center) {
            Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = null, tint = OnVideo)
        }
        Box(Modifier.align(Alignment.TopEnd).padding(4.dp).size(48.dp), contentAlignment = Alignment.Center) {
            Icon(Icons.Outlined.MoreVert, contentDescription = null, tint = OnVideo)
        }
        NeighborActionsPreview(
            item = neighbor,
            favorite = favorite,
            muted = muted,
            volume = volume,
            landscape = isLandscapeVideo(ratio),
            modifier = Modifier.align(Alignment.BottomEnd).padding(end = 6.dp, bottom = 64.dp),
        )
        ImmersionInfo(
            item = neighbor,
            overlayAlpha = 1f,
            interactive = false,
            modifier = Modifier.align(Alignment.BottomStart).padding(start = 14.dp, end = 76.dp, bottom = 26.dp),
        )
    }
}

@Composable
private fun NeighborActionsPreview(
    item: FeedItem,
    favorite: Boolean,
    muted: Boolean,
    volume: Int,
    landscape: Boolean,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item.author?.let { Avatar(it.avatar, 42) }
        PreviewActionPill(Icons.Outlined.ChatBubbleOutline, item.stats.comments?.let(::compactCount))
        PreviewActionPill(if (favorite) Icons.Filled.Star else Icons.Outlined.StarBorder, null, tint = if (favorite) Accent else OnVideo)
        PreviewActionPill(Icons.Outlined.Share, null)
        PreviewActionPill(null, PlaybackSpeed.label(PlaybackSpeed.DEFAULT))
        PreviewActionPill(
            if (muted) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp,
            if (muted) "静音" else "$volume%",
            tint = if (muted) OnVideo.copy(alpha = 0.75f) else OnVideo,
        )
        if (landscape) PreviewActionPill(Icons.Filled.Fullscreen, null)
    }
}

@Composable
private fun PreviewActionPill(icon: ImageVector?, label: String?, tint: Color = OnVideo) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Box(
            Modifier.size(48.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.32f)),
            contentAlignment = Alignment.Center,
        ) {
            if (icon != null) {
                Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(26.dp))
            } else if (label != null) {
                Text(label, color = tint, fontSize = 13.sp, fontWeight = FontWeight.Medium)
            }
        }
        if (icon != null && label != null) Text(label, color = OnVideo, fontSize = 11.sp)
    }
}

// ---------------------------------------------------------------- 右侧按钮列

@Composable
private fun ImmersionActions(
    graph: AppGraph,
    item: FeedItem,
    session: PlaybackSession?,
    follow: ImmersionFollow?,
    commentsOpen: Boolean,
    onComments: () -> Unit,
    volumeOpen: Boolean,
    onVolumeOpenChange: (Boolean) -> Unit,
    landscape: Boolean,
    onFullscreen: () -> Unit,
    overlayAlpha: Float,
    modifier: Modifier = Modifier,
) {
    val host = LocalDetailHost.current
    val author = item.author
    val favorite = graph.library.isFavorite(item.key)
    var speedMenu by remember { mutableStateOf(false) }
    Column(
        modifier.graphicsLayer { alpha = overlayAlpha },
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        if (author != null) {
            Box(Modifier.immersionClickable { author.url?.let { host?.openLink(it, null) } }) { Avatar(author.avatar, 42) }
        }
        if (follow != null) FollowButton(follow)
        ActionButton(
            icon = Icons.Outlined.ChatBubbleOutline,
            label = item.stats.comments?.let(::compactCount),
            description = if (commentsOpen) "关闭评论" else "评论",
            onClick = onComments,
        )
        ActionButton(
            icon = if (favorite) Icons.Filled.Star else Icons.Outlined.StarBorder,
            label = null,
            description = if (favorite) "取消收藏" else "收藏",
            tint = if (favorite) Accent else OnVideo,
            onClick = {
                if (favorite) {
                    graph.library.removeFavorite(item.key)
                    host?.showToast("已取消收藏")
                } else {
                    graph.library.addFavorite(item)
                    host?.showToast("已收藏")
                }
            },
        )
        ActionButton(
            icon = Icons.Outlined.Share,
            label = null,
            description = "分享",
            onClick = {
                item.url?.let { host?.copy(shareText(shareTitle(item), it), "已复制标题和链接") }
            },
        )
        Box {
            ActionButton(
                icon = null,
                label = PlaybackSpeed.label(session?.state?.speed ?: PlaybackSpeed.DEFAULT),
                description = "倍速",
                onClick = { speedMenu = true },
            )
            SpeedPopup(speedMenu, session?.state?.speed ?: PlaybackSpeed.DEFAULT, onDismiss = { speedMenu = false }) { s ->
                speedMenu = false
                session?.setSpeed(s)
            }
        }
        if (session != null && session.engine.unavailable == null) {
            val muted = session.state.muted || session.state.volume == 0
            ActionButton(
                icon = if (muted) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp,
                label = if (muted) "静音" else "${session.state.volume}%",
                description = "音量",
                tint = if (muted) OnVideo.copy(alpha = 0.75f) else OnVideo,
                onClick = { onVolumeOpenChange(!volumeOpen) },
            )
        }
        if (session?.hasDanmaku == true) {
            ActionButton(
                icon = if (session.danmakuOn) Icons.Filled.Subtitles else Icons.Filled.SubtitlesOff,
                label = null,
                description = "弹幕",
                tint = if (session.danmakuOn) Accent else OnVideo,
                onClick = {
                    session.danmakuOn = !session.danmakuOn
                    session.poke()
                },
            )
        }
        if (landscape) {
            ActionButton(icon = Icons.Filled.Fullscreen, label = null, description = "横屏全屏", onClick = onFullscreen)
        }
    }
}

@Composable
private fun FollowButton(follow: ImmersionFollow) {
    val following = follow.following == true
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = Modifier.immersionClickable { follow.click() },
    ) {
        Box(
            Modifier.size(38.dp).clip(CircleShape)
                .background(if (following) Color.Black.copy(alpha = 0.32f) else Accent),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                if (following) Icons.Filled.Check else Icons.Filled.Add,
                contentDescription = follow.label,
                tint = OnVideo,
                modifier = Modifier.size(22.dp),
            )
        }
        Text(follow.label, color = OnVideo, fontSize = 11.sp, modifier = Modifier.padding(top = 2.dp))
    }
    if (follow.confirmUnfollow) {
        ConfirmDialog(
            title = "取消关注",
            text = "不再关注「${follow.target.name}」？",
            confirmText = "取消关注",
            onConfirm = follow::confirmUnfollowNow,
            onDismiss = follow::cancelUnfollow,
        )
    }
}

@Composable
private fun ActionButton(
    icon: ImageVector?,
    label: String?,
    description: String,
    onClick: () -> Unit,
    tint: Color = OnVideo,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.immersionClickable(onClick)) {
        Box(
            Modifier.size(48.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.32f)),
            contentAlignment = Alignment.Center,
        ) {
            if (icon != null) {
                Icon(icon, contentDescription = description, tint = tint, modifier = Modifier.size(26.dp))
            } else if (label != null) {
                Text(label, color = tint, fontSize = 13.sp, fontWeight = FontWeight.Medium)
            }
        }
        if (icon != null && label != null) Text(label, color = OnVideo, fontSize = 11.sp)
    }
}

// ---------------------------------------------------------------- 左下角的信息

@Composable
private fun ImmersionInfo(
    item: FeedItem,
    overlayAlpha: Float,
    interactive: Boolean = true,
    modifier: Modifier = Modifier,
) {
    val host = LocalDetailHost.current
    val author = item.author
    val title = item.title?.takeIf { it.isNotBlank() } ?: item.summary?.takeIf { it.isNotBlank() }
    var expanded by remember(item.key) { mutableStateOf(false) }
    Column(modifier.graphicsLayer { alpha = overlayAlpha }, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Avatar(author?.avatar, 30)
            Text(
                "@${author?.name ?: "匿名"}",
                color = OnVideo,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = if (interactive) {
                    Modifier.pointerHoverIcon(PointerIcon.Hand).clickable { author?.url?.let { host?.openLink(it, null) } }
                } else {
                    Modifier
                },
            )
        }
        if (title != null) {
            if (expanded && interactive) {
                // 展开的长文案支持内部滚动，并吃掉拖动/滚轮，不会一边读文案一边把视频划走
                Box(
                    Modifier.widthIn(max = 460.dp).heightIn(max = 180.dp)
                        .swallowsDragsAndWheel()
                        .verticalScroll(rememberScrollState())
                        .pointerHoverIcon(PointerIcon.Hand)
                        .clickable { expanded = false },
                ) {
                    Text(title, color = OnVideo, fontSize = 14.sp)
                }
            } else {
                Text(
                    title,
                    color = OnVideo,
                    fontSize = 14.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.widthIn(max = 460.dp).let { m ->
                        if (interactive) m.pointerHoverIcon(PointerIcon.Hand).clickable { expanded = true } else m
                    },
                )
            }
        }
        item.publishedAt?.let { Text(fullTime(it), color = OnVideo.copy(alpha = 0.75f), fontSize = 11.sp) }
    }
}

/**
 * 沉浸底边的进度条：
 * - 点按直接跳转；
 * - 横向拖动进入精细调进度态（轨道加粗、通知外层清屏并显示大号时间预览）；
 * - 竖向滑动（大拇指从屏幕底部往上划翻页）**不消费**，直接放行给外层 [immersionSwipes] 切视频。
 */
@Composable
private fun ImmersionProgress(
    session: PlaybackSession,
    onScrubFractionChange: (Float?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state = session.state
    val visible = LocalWindowVisible.current
    val pos by produceState(state.positionMs, state.playing, visible) {
        value = state.nowMs()
        while (visible) awaitProgressUpdate(state) { value = it }
    }
    val durationMs = state.durationMs
    var dragFraction by remember(session) { mutableStateOf<Float?>(null) }
    val currentOnScrub by rememberUpdatedState(onScrubFractionChange)
    val currentOnSeek by rememberUpdatedState<(Long) -> Unit> { ms -> session.seekTo(ms) }

    val scrubbing = dragFraction != null
    val lineHeight by animateDpAsState(targetValue = if (scrubbing) 7.dp else 2.5.dp, animationSpec = tween(120), label = "immersionProgressLine")
    val thumbSize by animateDpAsState(targetValue = if (scrubbing) 14.dp else 6.dp, animationSpec = tween(120), label = "immersionProgressThumb")

    val total = durationMs.coerceAtLeast(1L)
    val shown = dragFraction ?: (pos.toFloat() / total).coerceIn(0f, 1f)
    val buffered = (state.bufferedMs.toFloat() / total).coerceIn(0f, 1f)

    Box(
        modifier
            .fillMaxWidth()
            .height(ProgressTouchHeight)
            .pointerHoverIcon(PointerIcon.Hand)
            .pointerInput(durationMs) {
                if (durationMs <= 0L) return@pointerInput
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    val width = size.width.coerceAtLeast(1).toFloat()
                    var axis: ImmersionProgressAxis? = null
                    var targetFraction = (down.position.x / width).coerceIn(0f, 1f)

                    while (true) {
                        val event = awaitPointerEvent(PointerEventPass.Main)
                        val c = event.changes.firstOrNull { it.id == down.id } ?: break
                        if (!c.pressed) {
                            if (axis == null) {
                                // 轻点进度条：直接跳到点按位置
                                c.consume()
                                currentOnSeek((targetFraction * durationMs).toLong())
                            } else if (axis == ImmersionProgressAxis.HorizontalScrub) {
                                c.consume()
                                dragFraction = null
                                currentOnScrub(null)
                                currentOnSeek((targetFraction * durationMs).toLong())
                            }
                            break
                        }
                        if (axis == null) {
                            val totalDelta = c.position - down.position
                            axis = immersionProgressGestureAxis(totalDelta.x, totalDelta.y, viewConfiguration.touchSlop)
                            if (axis == ImmersionProgressAxis.VerticalPassThrough) {
                                // 竖滑：放手让外层翻页接管
                                break
                            }
                            if (axis == ImmersionProgressAxis.HorizontalScrub) {
                                c.consume()
                                targetFraction = (c.position.x / width).coerceIn(0f, 1f)
                                dragFraction = targetFraction
                                currentOnScrub(targetFraction)
                            }
                        } else if (axis == ImmersionProgressAxis.HorizontalScrub) {
                            c.consume()
                            targetFraction = (c.position.x / width).coerceIn(0f, 1f)
                            dragFraction = targetFraction
                            currentOnScrub(targetFraction)
                        }
                    }
                    if (dragFraction != null) {
                        dragFraction = null
                        currentOnScrub(null)
                    }
                }
            },
        contentAlignment = Alignment.BottomStart,
    ) {
        Box(
            Modifier.fillMaxWidth().padding(bottom = 8.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            val trackShape = RoundedCornerShape(4.dp)
            Box(Modifier.fillMaxWidth().height(lineHeight).background(Color.White.copy(alpha = 0.25f), trackShape))
            Box(Modifier.fillMaxWidth(buffered).height(lineHeight).background(Color.White.copy(alpha = 0.45f), trackShape))
            Box(Modifier.fillMaxWidth(shown).height(lineHeight).background(if (scrubbing) Color.White else Accent, trackShape))
            BoxWithConstraints(Modifier.fillMaxWidth()) {
                Box(
                    Modifier.padding(start = ((maxWidth - thumbSize).coerceAtLeast(0.dp) * shown))
                        .size(thumbSize)
                        .background(if (scrubbing) Color.White else Accent, CircleShape),
                )
            }
        }
    }
}

// ---------------------------------------------------------------- 评论面板

/**
 * 沉浸界面里的评论：
 * - 右栏够宽（[bottomSheet] = false）：从右边滑出，视频往左让位；
 * - 窄屏（[bottomSheet] = true）：从底下升起半屏，支持**按住顶栏下拉**以及**评论列表滚到顶后继续下拉（NestedScroll）**跟手收起。
 */
@Composable
private fun ImmersionComments(
    comments: CommentsState,
    poster: Poster,
    total: Long?,
    bottomSheet: Boolean,
    onOpenReplies: (Comment) -> Unit,
    onClose: () -> Unit,
    onSheetDragDelta: (Float) -> Unit = {},
    onSheetDragSettle: (Float) -> Unit = {},
    currentDragPx: () -> Float = { 0f },
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()
    val latestDragDelta by rememberUpdatedState(onSheetDragDelta)
    val latestDragSettle by rememberUpdatedState(onSheetDragSettle)
    val latestCurrentDrag by rememberUpdatedState(currentDragPx)

    val nestedScroll = remember(bottomSheet) {
        object : NestedScrollConnection {
            override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
                if (!bottomSheet || source != NestedScrollSource.UserInput) return Offset.Zero
                // 面板已经被往下拉出了一截时，手指往上推先把面板推回原位，再滚评论列表
                if (available.y < 0f && latestCurrentDrag() > 0.5f) {
                    val consumeY = maxOf(available.y, -latestCurrentDrag())
                    latestDragDelta(consumeY)
                    return Offset(0f, consumeY)
                }
                return Offset.Zero
            }

            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
                if (!bottomSheet || source != NestedScrollSource.UserInput) return Offset.Zero
                // 评论列表已经在顶部，继续往下拉 → 跟手把评论面板往下拉
                if (available.y > 0f) {
                    latestDragDelta(available.y)
                    return Offset(0f, available.y)
                }
                return Offset.Zero
            }

            override suspend fun onPreFling(available: Velocity): Velocity {
                if (!bottomSheet) return Velocity.Zero
                if (latestCurrentDrag() > 0.5f) {
                    latestDragSettle(available.y)
                    return available
                }
                return Velocity.Zero
            }
        }
    }

    Surface(
        shape = if (bottomSheet) RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp) else RoundedCornerShape(0.dp),
        color = MaterialTheme.colorScheme.background,
        shadowElevation = 8.dp,
        modifier = modifier.swallowsDragsAndWheel(),
    ) {
        Column(Modifier.fillMaxSize().nestedScroll(nestedScroll)) {
            val headerDragModifier = if (bottomSheet) {
                Modifier.pointerInput(Unit) {
                    val vt = VelocityTracker()
                    detectVerticalDragGestures(
                        onDragStart = { vt.resetTracking() },
                        onVerticalDrag = { change, dragAmount ->
                            vt.addPointerInputChange(change)
                            change.consume()
                            latestDragDelta(dragAmount)
                        },
                        onDragEnd = { latestDragSettle(vt.calculateVelocity().y) },
                        onDragCancel = { latestDragSettle(0f) },
                    )
                }
            } else {
                Modifier
            }
            Column(Modifier.fillMaxWidth().then(headerDragModifier)) {
                if (bottomSheet) {
                    Spacer(Modifier.height(8.dp))
                    Box(
                        Modifier.align(Alignment.CenterHorizontally).width(36.dp).height(4.dp)
                            .clip(RoundedCornerShape(2.dp))
                            .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.35f)),
                    )
                }
                Row(
                    Modifier.fillMaxWidth().height(46.dp).padding(start = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        if (total != null) "评论 ${compactCount(total)}" else "评论",
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.weight(1f),
                    )
                    if (comments.sortable) CommentSortChips(comments.sort, comments::select, compact = true)
                    IconButton(onClick = onClose) { Icon(Icons.Outlined.Close, contentDescription = "关闭评论") }
                }
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            SelectionContainer {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 16.dp),
                ) {
                    commentsList(comments, poster, onOpenReplies)
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 音量

private val VolumePanelEndPadding = 74.dp

@Composable
private fun VolumePanel(
    volume: Int,
    muted: Boolean,
    onVolume: (Int) -> Unit,
    onToggleMute: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = Color.Black.copy(alpha = 0.62f),
        modifier = modifier.swallowsDragsAndWheel(),
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(if (muted) "静音" else "$volume%", color = OnVideo, fontSize = 12.sp)
            VerticalVolumeSlider(if (muted) 0 else volume, onVolume)
            IconButton(onClick = onToggleMute) {
                Icon(
                    if (muted || volume == 0) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp,
                    contentDescription = if (muted) "取消静音" else "静音",
                    tint = OnVideo,
                    modifier = Modifier.size(20.dp),
                )
            }
        }
    }
}

@Composable
private fun VerticalVolumeSlider(volume: Int, onChange: (Int) -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier
            .width(44.dp)
            .height(ImmersionVolumeSliderHeight)
            .pointerHoverIcon(PointerIcon.Hand)
            .pointerInput(Unit) {
                awaitEachGesture {
                    val down = awaitFirstDown()
                    onChange(volumeFromSliderY(down.position.y, size.height))
                    down.consume()
                    while (true) {
                        val e = awaitPointerEvent()
                        val c = e.changes.firstOrNull() ?: break
                        if (!c.pressed) break
                        onChange(volumeFromSliderY(c.position.y, size.height))
                        c.consume()
                    }
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        val fraction = volume.coerceIn(0, 100) / 100f
        Box(Modifier.width(4.dp).fillMaxHeight().background(Color.White.copy(alpha = 0.3f), RoundedCornerShape(2.dp)))
        Box(Modifier.align(Alignment.BottomCenter).width(4.dp).fillMaxHeight(fraction).background(OnVideo, RoundedCornerShape(2.dp)))
        Box(
            Modifier.align(Alignment.BottomCenter)
                .padding(bottom = (ImmersionVolumeSliderHeight - 12.dp) * fraction)
                .size(12.dp)
                .background(OnVideo, CircleShape),
        )
    }
}

// ---------------------------------------------------------------- 手势

/**
 * 沉浸界面的整屏上下划页手势：
 * - 在 [PointerEventPass.Main]（子控件之后）检查：若内层滑杆 / 文案 / 长按倍速已经消费或占用，整场让路；
 * - 过 `touchSlop` 时按 [ImmersionAxisLockRatio] 锁定主轴：竖向为主立即锁定翻页并 `consume()`（同时取消内层单击等待，
 *   拖出去再拖回起点松手不会误暂停）；横向为主则整场忽略，绝不因斜滑夹带纵向偏移误切视频；
 * - 松手把累计纵向位移和 `VelocityTracker` 算出的纵向速度一并交给 [onSettle] 做 Fling / 比例结算。
 */
@Composable
private fun Modifier.immersionSwipes(
    onDown: () -> Unit,
    onDrag: (Float) -> Unit,
    onSettle: (dyPx: Float, velocityYPxPerSec: Float) -> Unit,
    onAbort: () -> Unit,
    onWheelStep: (Int) -> Unit,
    suppressed: () -> Boolean,
): Modifier {
    val latestOnDown by rememberUpdatedState(onDown)
    val latestOnDrag by rememberUpdatedState(onDrag)
    val latestOnSettle by rememberUpdatedState(onSettle)
    val latestOnAbort by rememberUpdatedState(onAbort)
    val latestOnWheelStep by rememberUpdatedState(onWheelStep)
    val latestSuppressed by rememberUpdatedState(suppressed)
    return this
        .pointerInput(Unit) {
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Main)
                latestOnDown()
                var settled = false
                try {
                    val tracker = ImmersionSwipeTracker(slopPx = viewConfiguration.touchSlop)
                    val velocityTracker = VelocityTracker()
                    velocityTracker.addPointerInputChange(down)

                    while (true) {
                        val event = awaitPointerEvent(PointerEventPass.Main)
                        val change = event.changes.firstOrNull { it.id == down.id } ?: break
                        velocityTracker.addPointerInputChange(change)
                        if (!change.pressed) break
                        val moved = tracker.move(
                            deltaY = change.position.y - change.previousPosition.y,
                            consumed = event.changes.any { it.isConsumed },
                            speedHeld = latestSuppressed(),
                            deltaX = change.position.x - change.previousPosition.x,
                        )
                        if (tracker.locked) {
                            change.consume()
                        }
                        if (moved) latestOnDrag(tracker.offset)
                    }
                    val dy = tracker.settle()
                    val vy = if (tracker.locked && !tracker.owned) velocityTracker.calculateVelocity().y else 0f
                    settled = true
                    latestOnSettle(dy, vy)
                } finally {
                    if (!settled) latestOnAbort()
                }
            }
        }
        .pointerInput(Unit) {
            awaitPointerEventScope {
                var acc = 0f
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Final)
                    if (event.type != PointerEventType.Scroll) continue
                    if (event.changes.any { it.isConsumed }) continue
                    acc += event.changes.firstOrNull()?.scrollDelta?.y ?: 0f
                    val step = immersionWheelStep(acc)
                    if (step != 0) {
                        acc = 0f
                        latestOnWheelStep(step)
                    }
                }
            }
        }
}

@Composable
private fun Modifier.immersionClickable(onClick: () -> Unit): Modifier =
    this.pointerHoverIcon(PointerIcon.Hand).clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onClick)

private fun Modifier.swallowsDragsAndWheel(): Modifier = this.pointerInput(Unit) {
    awaitPointerEventScope {
        while (true) {
            val event = awaitPointerEvent(PointerEventPass.Main)
            if (event.type == PointerEventType.Scroll) {
                event.changes.forEach { it.consume() }
                continue
            }
            event.changes.forEach { change ->
                if (change.pressed && change.positionChange() != Offset.Zero) change.consume()
            }
        }
    }
}
