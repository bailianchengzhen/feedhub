package feedhub.app.video

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.Subtitles
import androidx.compose.material.icons.filled.SubtitlesOff
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
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import kotlinx.coroutines.launch
import feedhub.app.AppGraph
import feedhub.app.detail.CommentSortChips
import feedhub.app.detail.CommentsState
import feedhub.app.detail.DetailDebug
import feedhub.app.detail.DetailPhoto
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

private val Accent = Color(0xFFFB7299)
private val OnVideo = Color.White

/** 叠层（左下文字、右侧按钮）平时的透明度；鼠标在画面上时变清楚。 */
private const val OverlayIdleAlpha = 0.72f

/**
 * 竖屏视频的沉浸界面：**视频详情页的另一种样子**。
 * 尺寸跟着被给的地方走——双栏（平板横屏 / 桌面）时 [feedhub.app.SplitPane] 只给它右栏那么大，
 * 全屏打开方式 / 手机竖屏才占满整个窗口；评论面板也只在它里面滑出 / 升起，不盖到左栏去。
 *
 * 画面：竖屏按高度撑满、居中，两边用这条视频封面的模糊放大版铺底。左下角叠作者 / 标题（两行，点开全看）/
 * 发布时间，底边一条细进度条；右侧一竖排按钮（作者头像、**关注**（头像下面，平台不支持关注就不显示）、评论、
 * 收藏、分享、倍速、**音量**（点开一条竖向滑块）、B站的弹幕开关、**横屏全屏**（只有这一条视频本身是横屏时才出现））。
 * 点画面暂停 / 继续（暂停时中间一个播放键）；
 * 叠层平时半透明，鼠标在上面时变清楚。左上是返回，右上"⋯"里是屏蔽作者 / 弹幕设置（详情顶栏的那几项在
 * 沉浸界面里的去处）。
 *
 * **上下滑连续刷**：手指上下滑 / 滚轮 / ↑↓ / PageUp·PageDown 换来源列表里的
 * 上一条 / 下一条视频（队列和判定在 [ImmersionQueue]，键盘在 [videoKeyAction]，这里只报"要切哪一边"）。
 * 拖动时整块内容跟着手指走、底下先露出下一条的封面（"跟手"、不白屏），松手没滑够就弹回去。
 * **长按 3 倍速 / 上滑锁定**和常规详情页是同一份手势（[PlayerView]）；**从长按开始的这一整次触摸都归倍速**
 * （上滑锁定后松手保持倍速、不换视频），音量面板 / 进度条这类滑杆上的拖动也不归翻页。
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
    /** 往上滑（+1 = 下一条）/ 往下滑（-1 = 上一条）。是不是真的能切、到没到尾部由 [VideoDetailScreen] 判（它手上有队列）。 */
    onStep: (Int) -> Unit = {},
    /** 下一条 / 上一条的条目：拖动时在底下先露出它的封面（"跟手"、不白屏）。null = 没有那一条。 */
    nextItem: FeedItem? = null,
    previousItem: FeedItem? = null,
    /** 切换被挡下时的一句话（"已经是最后一条了" / "正在加载更多…" / "这条不在信息流里"），由外面自动消失。 */
    hint: String? = null,
    /** 详情加载失败时的重试；null = 不显示重试（例如播放器自己报的错，那里有自己的重试）。 */
    onRetry: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    // 性能记录：页面名临时盖成"沉浸"（退出沉浸就恢复成详情页，见 PerfScreen）
    androidx.compose.runtime.DisposableEffect(Unit) {
        feedhub.app.perf.PerfHooks.overlay("沉浸")
        onDispose { feedhub.app.perf.PerfHooks.overlay(null) }
    }
    val host = LocalDetailHost.current
    val shown = detail?.item ?: item
    val ratio = immersionAspect(item, detail?.video, session?.info, session?.part, session?.state?.videoWidth ?: 0, session?.state?.videoHeight ?: 0)
    val cover = item.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: shown.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: shown.media.filterIsInstance<Media.Image>().firstOrNull()?.url
    // 封面的占位：卡片上已经下好的那张缩略图
    val coverPlaceholder = cover?.let { c -> LocalDetailThumbnails.current.hint(c)?.thumbnail }
    // 鼠标在画面上时叠层变清楚（平时半透明，不挡视频）
    val hoverSource = remember { MutableInteractionSource() }
    val hovered by hoverSource.collectIsHoveredAsState()
    val overlayAlpha = if (hovered) 1f else OverlayIdleAlpha
    val contentId = (shown.detail as? DetailTarget.Native)?.id ?: shown.id
    // 关注按钮：平台实现了 FollowSource 且详情给了关注状态才显示（[immersionFollowVisible]，测试测的就是这一个判断）。
    // onMessage 走 rememberUpdatedState：host 换了也提示得到（对象存下来不会跟着重组换 lambda）。
    val followState = detail?.follow
    val followSource = graph.source(shown.platform) as? FollowSource
    val currentHost = rememberUpdatedState(host)
    val scope = rememberCoroutineScope()
    val follow = remember(detail, followState, followSource, scope) {
        if (!immersionFollowVisible(detail, followSource)) null
        else followState?.let { s -> followSource?.let { src -> ImmersionFollow(s, src, scope, onMessage = { currentHost.value?.showToast(it) }) } }
    }
    // 音量滑块开着没有（点音量按钮开 / 再点或点别处收起）
    var volumeOpen by remember { mutableStateOf(false) }
    // "跟手"：手指竖着拖的时候整块内容跟着走（松手弹回 0；真的切过去了这个组合会被换掉）。按条目重置。
    var drag by remember(item.key) { mutableStateOf(0f) }
    val density = LocalDensity.current
    // 截图工具（`open=immersion-volume` / `open=immersion-unfollow`）用的钩子
    SideEffect {
        DetailDebug.immersionVolume = { open -> volumeOpen = open; true }
        DetailDebug.immersionUnfollow = { follow?.click(); follow != null }
    }

    BoxWithConstraints(modifier.fillMaxSize().clipToBounds().background(Color.Black)) {
        val paneW = maxWidth
        val paneH = maxHeight
        val side = commentsOpen && immersionCommentsOnSide(paneW, paneH, ratio)
        // 评论在右边滑出时视频往左让位：画面在剩下的那块地方里重新适配（还是居中）
        val videoW = if (side) paneW - ImmersionCommentsPanelWidth else paneW
        val videoSize = fitVideoSize(videoW, paneH, ratio)
        // 横屏片源在竖屏里：画面上方那块空白（扣掉返回 / "⋯" 那一行）够高就把弹幕挪上去，不压画面
        val danmakuBand = (paneH - videoSize.height) / 2 - ImmersionTopRow
        val danmakuAbove = danmakuBand >= ImmersionDanmakuMinBand
        val paneHeightPx = with(density) { paneH.toPx() }
        val thresholdPx = with(density) { ImmersionSwipeThreshold.toPx() }
        // 手指往上滑（drag < 0）= 下一条：底下先露出下一条的封面
        val neighbor = if (drag < 0f) nextItem else previousItem
        val neighborCover = neighbor?.let { n ->
            n.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
                ?: n.media.filterIsInstance<Media.Image>().firstOrNull()?.url
        }
        // 没有那一条时只给一点点回弹（"到头了"的手感），不再整块跟着走
        val dragFollow = if (neighbor == null) drag * 0.15f else immersionDragOffset(drag, paneHeightPx)

        // 拖动时底下那一条：封面铺满 + 压暗（下一条的封面先垫着，切过去不白屏）
        if (drag != 0f && neighbor != null) {
            Box(Modifier.fillMaxSize().background(Color.Black)) {
                if (neighborCover != null) {
                    AsyncImage(
                        model = neighborCover,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize().graphicsLayer { scaleX = 1.05f; scaleY = 1.05f }.blur(12.dp),
                    )
                }
                Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.35f)))
            }
        }

        Box(
            Modifier.fillMaxHeight().width(videoW).hoverable(hoverSource)
                .graphicsLayer { translationY = dragFollow }
                .immersionSwipes(
                    thresholdPx = thresholdPx,
                    paneHeightPx = paneHeightPx,
                    onDrag = { dy -> drag = dy },
                    onSettle = { dy ->
                        val step = immersionSwipeStep(dy, thresholdPx)
                        if (step != 0) onStep(step)
                        // 弹回去（真的切过去了的话这个组合马上被换掉，动画跟着取消，看不到也不重要）
                        scope.launch { animate(drag, 0f, animationSpec = tween(180)) { v, _ -> drag = v } }
                    },
                    onStep = onStep,
                    suppressed = { session?.speedPressing == true },
                ),
        ) {
            // 铺底：封面的模糊放大版（不是纯黑；抖音网页版就是这样）
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

            // 画面：竖屏按高度撑满、居中（两边是模糊铺底）
            Box(Modifier.align(Alignment.Center).size(videoSize)) {
                when {
                    session != null && session.engine.unavailable == null ->
                        // poster：首帧之前先垫这一条的封面
                        PlayerView(
                            session, danmaku, onDanmakuChange, zoomed = false, onToggleZoom = onToggleZoom,
                            modifier = Modifier.fillMaxSize(), controls = false, poster = cover, posterPlaceholder = coverPlaceholder, onLogin = onLogin,
                            drawDanmaku = !danmakuAbove,
                        )
                    cover != null -> {
                        // 播放器还不能放（引擎没装 / 详情还没回来）：先画这张封面（也就是测试帧），
                        // 中间写明原因；详情加载失败的给「重试」（和常规详情页一样，不再只在角上留一行小字）。
                        // 原图没下来之前先用卡片上的缩略图垫着
                        DetailPhoto(
                            url = cover,
                            placeholder = coverPlaceholder,
                            contentDescription = null,
                            contentScale = ContentScale.Fit,
                            placeholderColor = Color.Black,
                            modifier = Modifier.fillMaxSize(),
                        )
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
                    else -> CircularProgressIndicator(Modifier.align(Alignment.Center).size(32.dp), color = OnVideo, strokeWidth = 3.dp)
                }
            }

            // 画面上方的弹幕带：底下是 Compose 画的模糊铺底、不是视频，所以走 Compose 那一份（onSurface = false，
            // 安卓的 SurfaceView 弹幕会把铺底挖空，见 DanmakuLayer 的说明）。取弹幕还是 PlayerView 在跑。
            val timeline = session?.danmakuTimeline
            if (danmakuAbove && session != null && session.danmakuOn && timeline != null && session.engine.unavailable == null && session.state.hasFrame) {
                Box(
                    Modifier.align(Alignment.TopCenter).padding(top = ImmersionTopRow)
                        .size(videoSize.width, danmakuBand).clipToBounds(),
                ) {
                    DanmakuLayer(timeline, session.state, danmaku, Modifier.fillMaxSize(), onSurface = false)
                }
            }

            // 上下两条渐变：顶部返回按钮、底部文字在上面看得清
            Box(
                Modifier.fillMaxWidth().height(96.dp).align(Alignment.TopCenter)
                    .background(Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.55f), Color.Transparent))),
            )
            Box(
                Modifier.fillMaxWidth().height(200.dp).align(Alignment.BottomCenter)
                    .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.65f)))),
            )

            // 左上：返回（和双栏 / 全屏现有的关闭详情方式一样，Esc 也行）
            IconButton(
                onClick = onClose,
                modifier = Modifier.align(Alignment.TopStart).padding(4.dp).graphicsLayer { alpha = overlayAlpha },
            ) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回", tint = OnVideo) }

            // 右上："⋯"——详情顶栏里的屏蔽作者 / 弹幕设置在沉浸界面里的去处（收藏 / 分享在右边那一列）。
            // 没有作者也画（弹幕设置还要用），只是里面少一项。
            run {
                val author = shown.author
                var menu by remember { mutableStateOf(false) }
                Box(Modifier.align(Alignment.TopEnd).padding(4.dp).graphicsLayer { alpha = overlayAlpha }) {
                    IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, contentDescription = "更多", tint = OnVideo) }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        // 弹幕设置：常规详情页在触屏上是画面右上角一个按钮，沉浸界面右上角是"⋯"，
                        // 就收在这里（面板还是同一个，弹幕开关在右侧按钮列里）
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

            // 右侧一竖排按钮
            ImmersionActions(
                graph = graph,
                item = shown,
                session = session,
                follow = follow,
                commentsOpen = commentsOpen,
                onComments = { onCommentsOpenChange(!commentsOpen) },
                volumeOpen = volumeOpen,
                onVolumeOpenChange = { volumeOpen = it },
                // 横屏全屏按钮：只有这一条视频本身是横屏（宽 > 高）才给，见 isLandscapeVideo
                landscape = isLandscapeVideo(ratio),
                onFullscreen = onToggleZoom,
                overlayAlpha = overlayAlpha,
                modifier = Modifier.align(Alignment.BottomEnd).padding(end = 6.dp, bottom = 64.dp),
            )

            // 左下：作者 + 标题 / 文案 + 发布时间
            ImmersionInfo(
                item = shown,
                overlayAlpha = overlayAlpha,
                modifier = Modifier.align(Alignment.BottomStart).padding(start = 14.dp, end = 76.dp, bottom = 24.dp),
            )

            // 底边一条细进度条
            if (session != null && session.engine.unavailable == null) {
                ImmersionProgress(
                    session,
                    modifier = Modifier.align(Alignment.BottomCenter).fillMaxWidth().padding(horizontal = 10.dp),
                )
            }

            // 切换被挡下时的一句话（已经是最后一条了 / 正在加载更多… / 这条不在信息流里）：和音量提示同一个位置，
            // 音量正在显示时让音量优先（那一下用户更关心音量）
            if (hint != null && volumeText == null) {
                Surface(
                    shape = RoundedCornerShape(50),
                    color = Color.Black.copy(alpha = 0.66f),
                    modifier = Modifier.align(Alignment.Center).padding(16.dp),
                ) {
                    Text(hint, color = OnVideo, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 14.dp, vertical = 8.dp))
                }
            }

            // 调音量时画面中间短暂显示音量值（↑↓ / M 走过这里；滑块自己显示在面板上）
            if (volumeText != null) {
                Surface(
                    shape = RoundedCornerShape(50),
                    color = Color.Black.copy(alpha = 0.66f),
                    modifier = Modifier.align(Alignment.Center).padding(16.dp),
                ) {
                    Text(volumeText, color = OnVideo, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 14.dp, vertical = 8.dp))
                }
            }

            // 音量滑块：点音量按钮开（也可以让滑块自己开，见上面的 DetailDebug 钩子）。画在最上面，
            // 下面先铺一层"点别处收起"的透明遮罩——它挡在画面和其他按钮之上，所以点哪儿（包括音量按钮自己）都先收起。
            if (volumeOpen && session != null && session.engine.unavailable == null) {
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

        // 评论：右栏够宽就从右边滑出（视频已经往左让位），不够宽就从底下升起半屏
        if (side) {
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
                    onOpenReplies = { host?.openReplies(it, poster, contentId) },
                    onClose = { onCommentsOpenChange(false) },
                    modifier = Modifier.width(ImmersionCommentsPanelWidth).fillMaxHeight(),
                )
            }
        } else {
            AnimatedVisibility(
                visible = commentsOpen,
                enter = slideInVertically { it } + fadeIn(),
                exit = slideOutVertically { it } + fadeOut(),
                modifier = Modifier.align(Alignment.BottomCenter),
            ) {
                ImmersionComments(
                    comments = comments,
                    poster = poster,
                    total = shown.stats.comments,
                    onOpenReplies = { host?.openReplies(it, poster, contentId) },
                    onClose = { onCommentsOpenChange(false) },
                    modifier = Modifier.fillMaxWidth().height(paneH * 0.5f),
                )
            }
        }
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
    /** 这条视频是不是横屏（宽 > 高）：是才给"横屏全屏"按钮（竖视频放大没有意义，见 [isLandscapeVideo]）。 */
    landscape: Boolean,
    /** "横屏全屏"：走常规播放器那套放大 / 全屏（安卓转横屏、桌面铺满窗口），复用 `ImmersionScreen` 的 `onToggleZoom`。 */
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
        // 作者头像（和左下角的名字一样，点了去作者主页）
        if (author != null) {
            Box(Modifier.immersionClickable { author.url?.let { host?.openLink(it, null) } }) { Avatar(author.avatar, 42) }
        }
        // 关注。平台不支持关注 / 详情没给状态就没有这个按钮。
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
        // 倍速：和播放器里的是同一份（0.5~3 倍）
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
        // 音量：点开一条竖向滑块（再点 / 点别处收起）；静音或 0 时图标不一样。音量还是全 App 共用那一份。
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
        // B站有弹幕（别的平台没有，不给这个按钮）
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
        // 横屏全屏：沉浸界面是按竖屏视频排的，上下滑过来的横屏视频按宽度适配、上下留白，
        // 给一个按钮进常规播放器那套全屏。**只有这条视频本身是横屏才画**（竖屏 / 正方形 / 尺寸不知道都不画）。
        if (landscape) {
            ActionButton(icon = Icons.Filled.Fullscreen, label = null, description = "横屏全屏", onClick = onFullscreen)
        }
    }
}

/**
 * 关注按钮：放在作者头像正下面。
 * 未关注是一个主色圆底的"+"（抖音那种），已关注换成对勾 + "已关注"；点了已关注的按钮只是弹确认框
 * （取关是写操作，界面里那个 [ConfirmDialog] 确认了才真调接口）。
 */
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

/** 右侧一竖排里的一个按钮：半透明圆底 + 图标（或文字）+ 下面一行小字。 */
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
            Modifier.size(48.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.32f)), // 触控热区 ≥48dp（E7）
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
private fun ImmersionInfo(item: FeedItem, overlayAlpha: Float, modifier: Modifier = Modifier) {
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
                modifier = Modifier
                    .pointerHoverIcon(PointerIcon.Hand)
                    .clickable { author?.url?.let { host?.openLink(it, null) } },
            )
        }
        if (title != null) {
            Text(
                title,
                color = OnVideo,
                fontSize = 14.sp,
                maxLines = if (expanded) 12 else 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 460.dp).pointerHoverIcon(PointerIcon.Hand).clickable { expanded = !expanded },
            )
        }
        item.publishedAt?.let { Text(fullTime(it), color = OnVideo.copy(alpha = 0.75f), fontSize = 11.sp) }
    }
}

/** 底边的细进度条：和播放器里的那条同一份（点 / 拖跳转，拖动时显示松手后的位置）。 */
@Composable
private fun ImmersionProgress(session: PlaybackSession, modifier: Modifier = Modifier) {
    val state = session.state
    val visible = LocalWindowVisible.current
    val pos by produceState(state.positionMs, state.playing, visible) {
        value = state.nowMs()
        while (visible) awaitProgressUpdate(state) { value = it }
    }
    ProgressBar(pos, state.bufferedMs, state.durationMs, onSeek = { session.seekTo(it) }, modifier = modifier)
}

// ---------------------------------------------------------------- 评论面板

/**
 * 沉浸界面里的评论：右栏够宽时是从右边滑出的面板（视频已经往左让位），不够宽是从底下升起的半屏面板。
 * 内容复用现有的评论组件（[commentsList]），"共 N 条回复"照旧走 [feedhub.app.detail.DetailHost.openReplies]。
 */
@Composable
private fun ImmersionComments(
    comments: CommentsState,
    poster: Poster,
    total: Long?,
    onOpenReplies: (Comment) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        shape = RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp),
        color = MaterialTheme.colorScheme.background,
        shadowElevation = 8.dp,
        modifier = modifier,
    ) {
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().height(48.dp).padding(start = 12.dp),
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
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            SelectionContainer {
                LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 16.dp)) {
                    commentsList(comments, poster, onOpenReplies)
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 音量

/** 音量滑块面板离画面右边的距离：让开右侧那一竖排按钮。 */
private val VolumePanelEndPadding = 74.dp

/**
 * 音量滑块面板：上面是音量值，中间一条**竖向**滑块（上大下小，拖 / 点都行），下面一个静音开关。
 * 音量还是全 App 共用那一份（[feedhub.app.video.AppVolume]），这里走 [PlaybackSession.setVolume] / [PlaybackSession.toggleMute]。
 *
 * 面板自己吃掉这一层上的**拖动**和**滚轮**（[swallowsDragsAndWheel]）：外层沉浸界面的上下翻页收不到，
 * 拖滑杆 / 在面板上滚都不会切视频。
 */
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
            IconButton(onClick = onToggleMute) { // 默认就是 48dp 触控热区（E7）
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

/** 竖向音量滑块：按住 / 拖动的位置（上大下小）就是音量；换算是纯函数 [volumeFromSliderY]，离线测得到。 */
@Composable
private fun VerticalVolumeSlider(volume: Int, onChange: (Int) -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier
            .width(44.dp) // 触控热区宽 ≥44dp（E7；那条 4dp 的线还是居中的）
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
        // 轨道
        Box(Modifier.width(4.dp).fillMaxHeight().background(Color.White.copy(alpha = 0.3f), RoundedCornerShape(2.dp)))
        // 已选部分：从下往上
        Box(Modifier.align(Alignment.BottomCenter).width(4.dp).fillMaxHeight(fraction).background(OnVideo, RoundedCornerShape(2.dp)))
        // 滑块头
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
 * 沉浸界面的上下切换手势：
 * - **不消费事件**（只在 [PointerEventPass.Final] 看位置变化），所以"点一下暂停 / 双击放大 / 长按 3 倍速"
 *   照旧由 [PlayerView] 那一套接：轻点位移不到阈值就是一次点按，长按不受影响；
 * - **这一场触摸归别人时整场不翻页**（[ImmersionSwipeTracker]）：长按倍速按着（[suppressed]）、内层控件
 *   （音量滑杆 / 进度条 / 拖进度 / 长按倍速的拖动）在 Main pass 消费了移动——位移清零、不跟手、松手也不补触发
 * - 拖动过程中把位移交给 [onDrag]（整块内容跟着手指走），松手交给 [onSettle]（滑够就切、不够就弹回去）；
 * - 滚轮（桌面）累计到一格就切一条，[onStep] 一边走一边清零（连发时靠 [DetailNavigator] 那边的节流兜底）；
 *   在 [PointerEventPass.Final] 看，**被别人（音量面板）消费掉的滚轮不算**。
 */
private fun Modifier.immersionSwipes(
    thresholdPx: Float,
    paneHeightPx: Float,
    onDrag: (Float) -> Unit,
    onSettle: (Float) -> Unit,
    onStep: (Int) -> Unit,
    suppressed: () -> Boolean,
): Modifier = this
    .pointerInput(thresholdPx, paneHeightPx) {
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Final)
            val tracker = ImmersionSwipeTracker()
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Final)
                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                if (!change.pressed) break
                val moved = tracker.move(
                    deltaY = change.position.y - change.previousPosition.y,
                    // 只看按着时的消费：抬手那一下会被单击检测 consume（常规做法），不能因此吞掉正常翻页
                    consumed = event.changes.any { it.isConsumed },
                    speedHeld = suppressed(),
                )
                if (moved) onDrag(tracker.offset)
            }
            // 长按加速按着 / 滑杆吃掉了这一场：结算 0（不切），跟手的位移也弹回去
            onSettle(tracker.settle())
        }
    }
    .pointerInput(Unit) {
        awaitPointerEventScope {
            var acc = 0f
            while (true) {
                // Final pass：内层控件（音量面板）先消费掉的滚轮不归翻页
                val event = awaitPointerEvent(PointerEventPass.Final)
                if (event.type != PointerEventType.Scroll) continue
                if (event.changes.any { it.isConsumed }) continue
                acc += event.changes.firstOrNull()?.scrollDelta?.y ?: 0f
                val step = immersionWheelStep(acc)
                if (step != 0) {
                    acc = 0f
                    onStep(step)
                }
            }
        }
    }

/** 点了要有点反应：悬停换上手指指针（桌面）。 */
@Composable
private fun Modifier.immersionClickable(onClick: () -> Unit): Modifier =
    this.pointerHoverIcon(PointerIcon.Hand).clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onClick)

/**
 * 这一层自己吃掉按下的**拖动**和**滚轮**：沉浸界面的上下翻页在
 * [PointerEventPass.Final] 看到"移动被消费了"就整场让路（[ImmersionSwipeTracker]），
 * 在 [PointerEventPass.Final] 看到"滚轮被消费了"就不翻。
 *
 * **不消费按下**（位置还没变的那一下）：面板里的滑块 / 静音按钮还要它；这里的 Main pass 在子控件之后跑，
 * 也影响不到子控件自己的手势。
 */
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
