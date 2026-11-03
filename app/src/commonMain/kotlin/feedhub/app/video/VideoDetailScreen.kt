package feedhub.app.video

import feedhub.app.LocalPlatforms
import feedhub.app.detail.communityLabel
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.SideEffect
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.MonetizationOn
import androidx.compose.material.icons.outlined.PlayCircleOutline
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material.icons.outlined.Subtitles
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties
import feedhub.app.AppGraph
import feedhub.app.LocalDetailInRightPane
import feedhub.app.requestsLandscapeFullscreen
import feedhub.app.detail.AdultHiddenDetail
import feedhub.app.detail.CommentSortChips
import feedhub.app.detail.detailHiddenAsAdult
import feedhub.app.detail.fullTime
import feedhub.app.detail.CommentsState
import feedhub.app.detail.DetailCache
import feedhub.app.detail.DetailDebug
import feedhub.app.detail.DetailPhoto
import feedhub.app.detail.DetailTitleText
import feedhub.app.detail.DetailTopBar
import feedhub.app.detail.LocalDetailHost
import feedhub.app.detail.LocalDetailNavigator
import feedhub.app.detail.LocalDetailThumbnails
import feedhub.app.detail.Poster
import feedhub.app.detail.posterLabel
import feedhub.app.detail.titleEmotes
import feedhub.app.detail.PostDetailScreen
import feedhub.app.detail.RichTextView
import feedhub.app.detail.RichTextTranslation
import feedhub.app.detail.rememberRichTextTranslation
import feedhub.app.translate.TranslationEntryRow
import feedhub.app.translate.LocalTranslation
import feedhub.app.detail.commentsList
import feedhub.app.feed.Avatar
import feedhub.app.feed.FeedCard
import feedhub.app.feed.ProvideCardTexts
import feedhub.app.library.ExposureSurface
import feedhub.app.ui.compactCount
import feedhub.app.ui.duration
import feedhub.brain.AdultFilter
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Media
import feedhub.core.FollowSource
import feedhub.core.Platform
import feedhub.core.FollowKind
import feedhub.core.NotLoggedInException
import feedhub.core.RiskControlException
import feedhub.core.VideoPart
import feedhub.core.VideoSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import feedhub.feed.exposureInputs
import feedhub.feed.relatedVisibleItems
import feedhub.app.feed.ExposureTracker
import feedhub.app.feed.ExposureTicker
import feedhub.app.feed.listVisibleRelatedItems

/**
 * 视频详情：播放器（整宽 16:9，最高占详情的 [playerHeight] 比例）+ 一列滚动：标题 / UP 主 / 数据 / 简介 / 分 P，
 * 再往下是"评论 | 相关推荐"标签（[VideoTab]，参考 BiliPai 平板版，默认评论；标签栏是 LazyColumn 的粘性头，滚评论时一直在顶上）。
 * 宽窄屏同一个样子，只是相关推荐、分 P 的列数随宽度变（[relatedColumns]、[partColumns]）。
 * 两个标签的数据（评论分页、相关推荐）都放在标签之外，来回切不丢。
 * 相关推荐点了就在当前详情里换成那个视频（压进详情栈，返回回到上一个视频；旧的播放器释放、进度保存）。
 *
 * 要在 [feedhub.app.detail.DetailPane] 里用：顶栏、简介富文本、评论区、楼中楼、大图、"已复制"提示都是它提供的公共部件。
 * 键盘（沉浸界面和这里共用一套，见 [videoKeyAction]）：空格 暂停 / 继续，← / → 快退 / 快进 5 秒，↑ / ↓ 音量，
 * M 静音，`[` / `]` 减速 / 加速，F 放大 / 还原，Esc 先退出放大（沉浸界面里先关评论面板）；没拦下就交给 DetailPane。
 * 安卓的**系统返回键**走 [DetailHost.playerZoomed] / [DetailHost.playerCommentsOpen]：先退放大 → 关评论面板 → 才返回上一层
 *
 * **竖屏视频**（高 > 宽）换成沉浸界面：画面撑满、左下叠信息 + 右侧按钮列，
 * 评论面板在它内部滑出 / 升起。宽高比：拿到流之前用详情分 P / 卡片封面给的，拿到流之后以真实分辨率为准（[immersionAspect]）。
 * **上下滑连续刷**：手指上下滑 / 滚轮 / ↑↓ / PageUp·PageDown 换来源列表里的上一条 / 下一条；
 * 路上切到横屏视频时还留在沉浸界面，那里右侧会多一个**「横屏全屏」**按钮，
 * 点它走的就是这里的放大 / 全屏（[toggleZoom]，安卓转横屏、桌面铺满窗口），退出回到同一条视频的沉浸界面。
 */
@Composable
fun VideoDetailScreen(graph: AppGraph, item: FeedItem, id: String, onClose: () -> Unit, modifier: Modifier = Modifier) {
    val source = graph.source(item.platform) as VideoSource
    val scope = rememberCoroutineScope()
    val host = LocalDetailHost.current
    val languageTranslation = LocalTranslation.current
    // 沉浸播放预取到的那一条（上下滑切过来的）：详情一开始就有，省掉一次请求；切过去不白屏也靠它。
    val navigator = LocalDetailNavigator.current
    val prefetched = remember(item) { navigator?.prefetcher?.readyFor(item) }
    // **同一条内容第二次点开不重来一遍**：详情（视频这里是简介 + 分 P）进 DetailCache，
    // 命中时一进来就有——分 P / 简介 / 播放器位置第一次画就是最终的样子，不用先黑底 + 封面再换一遍。
    val cacheKey = remember(item.key) { DetailCache.itemKey(item) }
    var detail: FeedDetail? by remember(item) { mutableStateOf(prefetched?.detail ?: DetailCache.get(cacheKey)) }
    var loadError: String? by remember(item) { mutableStateOf(null) }
    var reload by remember(item) { mutableStateOf(0) }
    LaunchedEffect(id, reload) {
        // 预取到的详情（reload = 0）直接用，不再发一次；用户点「重试」时 reload 已经 +1，照常现取。
        // 缓存够新时也一样跳过（省一次往返）。
        if (reload == 0 && detail != null && DetailCache.isFresh(cacheKey)) return@LaunchedEffect
        loadError = null
        try {
            // 视频详情页也有首屏：和图文详情一样记一行 detail（耗时 / 限速排队）、总超时、失败记 detail_failed
            val metered = feedhub.app.perf.meteredRequest(feedhub.app.perf.PerfRequest.DETAIL, item.platform, Dispatchers.IO) {
                source.detail(id)
            }
            detail = metered.value
            DetailCache.put(cacheKey, metered.value)
            feedhub.app.perf.PerfHooks.detailLoaded(item.platform, metered.value.item.kind, metered.dMs, metered.queueMs)
        } catch (e: CancellationException) {
            throw e
        } catch (e: RiskControlException) {
            // 已经有缓存（旧内容）就不换成错误：用户正在看的是对的，这次后台刷新失败而已
            if (detail == null) loadError = "被${item.platform.label}风控拦了，请稍后再试（${e.message}）" // 不自动重试
        } catch (e: Exception) {
            if (detail == null) loadError = "加载失败：${e.message ?: e::class.simpleName}"
        }
    }

    // 详情（视频这里是简介）**已经加载出来**：用"标题 + 简介"重算这条的向量。
    // 只在详情真的到了之后调，不为了向量多发一个请求；简介只用来算向量，不存库。
    LaunchedEffect(detail) {
        val loaded = detail ?: return@LaunchedEffect
        graph.onDetailLoaded(loaded.item, loaded.body.textOnly())
    }
    val d = detail
    val language = item.language
    // 简介整条判定（标题 + 简介）：桌面上「点亮平台的自动翻」照旧（进详情就自动翻）；
    // 手机 / 没点亮的平台点了才翻。
    val descriptionAuto = remember(d?.body, item.title, item.summary, language, item.platform, languageTranslation?.enabled, languageTranslation?.target) {
        languageTranslation?.enabled == true &&
            languageTranslation.shouldAutoTranslate(
                listOfNotNull(item.title, d?.body?.textOnly()).joinToString("\n").ifBlank { item.summary.orEmpty() },
                language,
                item.platform,
            )
    }
    // 简介那一行入口（整条视频一个）
    val descriptionTranslation = (d?.body ?: item.summary?.let { null })?.let {
        rememberRichTextTranslation(it, descriptionAuto, language)
    }
    // 平台在详情接口里才标的 18+（YouTube 年龄限制）：和从信息流直接打开同一条规则，只画一行、不加载播放
    if (d != null && detailHiddenAsAdult(d.item)) {
        AdultHiddenDetail(onClose, modifier.fillMaxSize())
        return
    }
    // 详情里没有可播放的视频（例如酷安里只是一条站外微博链接）：当图文帖看，视频以卡片的形式在正文里
    if (d != null && d.video == null) {
        PostDetailScreen(graph, item, id, onClose, modifier)
        return
    }
    val session = remember(d) {
        d?.takeIf { it.video != null }?.let {
            PlaybackSession(
                // 平台客户端：平台代理 / fail-closed / Referer / UA 都在里面，播放器经它取清单和分片
                engine = createPlayerEngine(graph.ctx.client(item.platform).build()),
                source = source,
                item = item,
                id = id,
                parts = it.video?.parts.orEmpty(),
                // 弹幕是插件声明的能力（描述里 capabilities.danmaku）；渲染在主程序里
                danmakuSegmentMs = it.video?.danmakuSegmentMs?.takeIf { graph.platforms[item.platform]?.capabilities?.danmaku == true },
                library = graph.library,
                settings = { graph.settings.value },
                scope = scope,
                lastPartStore = graph.ctx.store,
                volume = AppVolume,
                builtinProxy = { graph.proxyCore?.proxyMode() ?: feedhub.app.NO_BUILTIN_CORE },
                // 分片经本机中转、由 Rust 按这个平台的代理去取（fail-closed / 请求头都在那一条判定里）
                relayFactory = { graph.relay(item.platform) },
                // 预取到的播放地址（分 P 对得上才用，见 PlayPrefetcher）：切过来时省掉一次取流
                prefetchedPlayInfo = { partId -> prefetched?.takeIf { it.partId == partId }?.info },
            )
        }
    }
    // 要登录才能播（现在只有 YouTube 的设备码登录）：错误下面给按钮直接打开，成功后重试这一条
    val deviceLogin = remember(source) { source as? DeviceCodeLoginSource }
    var loginDialog by remember(item) { mutableStateOf(false) }
    val onLogin: (() -> Unit)? = deviceLogin?.let { { loginDialog = true } }
    DisposableEffect(session) {
        session?.loadPart(session.initialPartIndex, resume = true)
        onDispose { session?.release() }
    }
    // 正文里的视频（行内播放）和这里的主播放器同一时间只播一个：它开播时暂停主播放器，主播放器开播时让它停
    DisposableEffect(session, host) {
        host?.videos?.pauseMain = { session?.engine?.pause() }
        onDispose { host?.videos?.pauseMain = null }
    }
    LaunchedEffect(session, session?.state?.playing) {
        if (session?.state?.playing == true) host?.videos?.stopActive()
    }
    // 播放中每 5 秒存一次进度
    LaunchedEffect(session) {
        val s = session ?: return@LaunchedEffect
        everyWhilePlaying(playing = { s.state.playing && !s.state.ended }, everyMs = 5_000L) {
            withContext(Dispatchers.IO) { s.saveProgress() }
        }
    }

    val comments = remember(source, id) { CommentsState(source, id, scope, platforms = graph.platforms) }
    LaunchedEffect(comments) { comments.page.loadMore() }
    val shown = d?.item ?: item
    val poster = Poster(shown.author?.id ?: item.author?.id, posterLabel(shown, graph.platforms))
    SideEffect {
        DetailDebug.comments = { comments.page.items }
        DetailDebug.poster = poster
        DetailDebug.contentId = id
        // 时间线：这一屏现在是什么。和图文详情一样**现读**这里的
        // snapshot 状态（不能捕获组合时的 local val，那样量到的是旧值）。
        DetailDebug.timelineProbe = {
            val loaded = detail
            val phase = when {
                loaded != null -> "详情到了（分P=${loaded.video?.parts?.size ?: 0}）"
                loadError != null -> "详情失败"
                else -> "详情没到（黑底 + 卡片封面）"
            }
            val live = session
            "$phase 会话=${if (live != null) "建好了" else "还没建"} 播放中=${live?.state?.playing == true} " +
                "评论=${comments.page.items.size} ${DetailDebug.imageStatus()}"
        }
    }

    // 放大 / 全屏和沉浸界面的评论面板放在 DetailHost 里：系统的返回键由 DetailPane 的
    // BackHandler → host.dismissTop() 处理，先退放大、再关评论面板，两次之后才关详情。
    // 不在 DetailPane 里（host 为 null，理论上没有）时退回本页局部状态，行为不变。
    var zoomedLocal by remember { mutableStateOf(false) }
    val zoomed = host?.playerZoomed ?: zoomedLocal
    val setZoomed: (Boolean) -> Unit = { v -> if (host == null) zoomedLocal = v else host.playerZoomed = v }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    val danmaku = graph.settings.value.danmaku
    val onDanmakuChange: (DanmakuSettings) -> Unit = { v -> graph.settings.update { it.copy(danmaku = v) } }
    val onOpenReplies: (Comment) -> Unit = { host?.openReplies(it, poster, id) }
    val onOpenRelated: (FeedItem) -> Unit = { host?.openItem(it) }
    // 播放器下面那一列的滚动位置（"评论 / 相关推荐"标签贴上顶时要用）；沉浸界面用不到它。
    // 视频详情的顶栏**不跟滚动**，所以这里不再给顶栏算淡入。
    val bodyListState = rememberLazyListState()

    // ---------------- 沉浸界面（竖屏视频） ----------------
    val aspect = immersionAspect(
        item = item,
        video = d?.video,
        info = session?.info,
        part = session?.part,
        engineWidth = session?.state?.videoWidth ?: 0,
        engineHeight = session?.state?.videoHeight ?: 0,
    )
    // 竖屏（拿到流之后以真实分辨率为准）才进沉浸界面；横屏照旧是上面这个详情页。
    // 沉浸界面里上下滑换到下一条**横屏**视频时还留在沉浸界面（[DetailNavigator.switched]）：
    // 那是"连续刷"的同一个会话，切过去就弹回普通详情页等于把连续刷打断了。
    val immersive = immersionStays(isPortraitVideo(aspect), navigator?.switched == true)
    // 上下切换的队列：来源列表里的视频条目（跳过图文 / 直播）；当前这条不在列表里就退化成只有它自己。
    // 队列现读列表（信息流 loadMore 之后跟着变长），所以这里每次组合都拿得到最新的下一条。
    val queue = remember(item, navigator) {
        navigator?.let { n -> ImmersionQueue({ n.items() }, item, { n.loadMore() }, { n.loadingMore() }) }
    }
    // 切换被挡下时的一句话（2 秒后自己消失）
    var switchHint by remember(item) { mutableStateOf<String?>(null) }
    LaunchedEffect(switchHint) {
        if (switchHint != null) {
            delay(2_000)
            switchHint = null
        }
    }
    val switch: (Int) -> Unit = { delta ->
        when (val step = queue?.step(delta)) {
            is ImmersionStep.Go -> navigator?.select(step.item)
            is ImmersionStep.End -> switchHint = when {
                !step.forward -> "已经是第一条了"
                step.loading -> "正在加载更多…"
                else -> "已经是最后一条了"
            }
            null -> switchHint = "这条不在信息流里，不能上下切换"
        }
    }
    // 只预取下一条：详情 + 播放地址（省请求、风控也温和）。**只在前台**（退到后台当场取消，什么都不留），
    // 只留一份（换目标就丢旧的，见 PlayPrefetcher）。
    val foreground = graph.foreground
    val nextItem = queue?.next
    val previousItem = queue?.previous
    LaunchedEffect(immersive, nextItem?.key, foreground.generation) {
        if (!immersive || !foreground.isForeground) return@LaunchedEffect
        navigator?.prefetcher?.prefetch(nextItem)
    }
    // ---------------- 全屏 ----------------
    // 放大 = 隐藏系统栏（边缘滑动临时唤出）；横屏视频进全屏还转成横屏，退出还原（见 PlayerFullscreenEffect，桌面空实现）。
    // **双栏的右栏里不请求转屏**（真平板 / 折叠屏展开态）：详情只在右栏，转屏会把详情挪进 `SplitPane`、
    // 组合身份变了，放大态跟着丢，等于刚进全屏就被踢出来。
    // 详情铺满整窗（手机竖屏 / **手机横屏** / 「打开方式 = 全屏」）时不再受"打开方式"限制：横屏视频就转成横屏，
    // 设计对照 I1 修完（手机横屏不再切成双栏）之后转屏不会丢放大态了。规则只有 [requestsLandscapeFullscreen] 一处。
    // 沉浸界面里的"横屏全屏"：上下滑过来的横屏视频在沉浸界面里是按宽度适配的，点那个按钮走这里；
    // **只有这条视频本身是横屏才转**（竖屏视频放大不该把手机扳横，见 isLandscapeVideo）。
    val inRightPane = LocalDetailInRightPane.current
    PlayerFullscreenEffect(
        active = zoomed,
        landscape = requestsLandscapeFullscreen(
            zoomed = zoomed,
            twoPane = inRightPane,
            immersive = immersive,
            landscapeVideo = isLandscapeVideo(aspect),
        ),
    )
    // 手机转成横屏 + 横屏视频 → 自动进全屏（B站 / YouTube 习惯）。用户自己退出放大后，当前横屏不再自动进
    // （等转回竖屏再重置），否则"退回详情页"会被立刻又顶成全屏。桌面（不是触屏）不接；平板双栏的右栏（详情只占
    // 半屏，不是整窗铺满）也不接——那里全屏是用户自己点的。
    val windowInfo = LocalWindowInfo.current
    val windowLandscape = windowInfo.containerSize.let { it.width > it.height }
    var detailSize by remember { mutableStateOf(IntSize.Zero) }
    val detailFillsWindow = detailSize.width > 0 && detailSize.width >= windowInfo.containerSize.width * 0.9f
    var autoFullscreenSuppressed by remember { mutableStateOf(false) }
    LaunchedEffect(windowLandscape, immersive, detailFillsWindow) {
        if (!windowLandscape) autoFullscreenSuppressed = false
        if (isTouchDevice && windowLandscape && detailFillsWindow && !immersive && !autoFullscreenSuppressed) setZoomed(true)
    }
    // 放大按钮 / 还原按钮：还原时如果人还横着拿，就记下"这次别再自动进"
    val toggleZoom: (Boolean) -> Unit = { want ->
        if (!want && windowLandscape) autoFullscreenSuppressed = true
        setZoomed(want)
    }
    // 沉浸界面的评论面板也放 DetailHost（和 zoomed 同一个层级，见上面）：返回键先关它
    var commentsOpenLocal by remember { mutableStateOf(false) }
    val commentsOpen = host?.playerCommentsOpen ?: commentsOpenLocal
    val setCommentsOpen: (Boolean) -> Unit = { v -> if (host == null) commentsOpenLocal = v else host.playerCommentsOpen = v }
    // 调音量时屏幕中间短暂显示音量值（↑↓ 和右边那个音量滑块都走这里）
    var volumeTick by remember { mutableIntStateOf(0) }
    var volumeVisible by remember { mutableStateOf(false) }
    LaunchedEffect(volumeTick) {
        if (volumeTick == 0) return@LaunchedEffect
        volumeVisible = true
        delay(VOLUME_HINT_MS)
        volumeVisible = false
    }
    // 屏幕中间那行字：按共用的音量（AppVolume）显示，静音 / 0 写"静音"
    val volumeText = if (volumeVisible) {
        volumeOverlayText(session?.state?.volume ?: AppVolume.percent, session?.state?.muted ?: AppVolume.muted)
    } else {
        null
    }
    // 截图工具（`open=immersion-comments` / `keys=Down` / `print:immersion`）走这里点开评论面板、上下切、看状态
    SideEffect {
        DetailDebug.immersionComments = { open -> setCommentsOpen(open); true }
        DetailDebug.immersionStep = { delta -> switch(delta); true }
        DetailDebug.immersionInfo = {
            "沉浸界面 immersive=$immersive 竖屏=${isPortraitVideo(aspect)} 评论面板=$commentsOpen " +
                "宽高比=$aspect 横屏全屏按钮=${isLandscapeVideo(aspect)} 队列=${queue?.index ?: -1}/${queue?.items?.size ?: 0} 下一条=${queue?.next?.id ?: "没有"} " +
                "活着的会话=${ActivePlaybackSessions.alive().size} 提示=$switchHint " +
                "预取=${navigator?.prefetcher?.targetKey ?: "没有"}${if (navigator?.prefetcher?.isReady == true) "（已备好）" else ""}"
        }
    }

    Box(
        modifier.fillMaxSize()
            .onSizeChanged { detailSize = it }
            .focusRequester(focus)
            .focusable()
            .pointerInput(Unit) {
                // 点页面里任何地方都把键盘焦点拿回来（点了左边信息流再回来，快捷键还能用）；不消费事件
                awaitPointerEventScope {
                    while (true) {
                        val e = awaitPointerEvent(PointerEventPass.Initial)
                        if (e.type == PointerEventType.Press) runCatching { focus.requestFocus() }
                    }
                }
            }
            .onPreviewKeyEvent { e ->
                if (e.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                // Esc 有状态（放大 / 评论面板），先自己处理；别的键都问 [videoKeyAction]，沉浸界面和常规详情页共用一套
                if (e.key == Key.Escape) {
                    return@onPreviewKeyEvent when {
                        zoomed -> { setZoomed(false); true }
                        immersive && commentsOpen -> { setCommentsOpen(false); true }
                        else -> false // 交给 DetailPane（关大图 → 楼中楼 → 返回）
                    }
                }
                // 沉浸界面里 ↑↓ 是上一条 / 下一条，按住 Shift 才是调音量（那边没有控件条，
                // 音量入口是右侧按钮列的滑块 + 这把 Shift+↑↓；安卓侧键照旧是系统音量）
                when (val action = videoKeyAction(e.key, immersion = immersive && !e.isShiftPressed)) {
                    VideoKeyAction.TogglePlay -> session?.togglePlay().let { true }
                    is VideoKeyAction.Seek -> session?.seekBy(action.deltaMs).let { true }
                    is VideoKeyAction.Step -> { switch(action.delta); true }
                    // 沉浸界面里调音量要顺便把音量值显示在画面中间（常规详情页有控件条，不用）
                    is VideoKeyAction.Volume -> {
                        session?.changeVolume(action.delta)
                        if (immersive) volumeTick++
                        true
                    }
                    VideoKeyAction.ToggleMute -> {
                        session?.toggleMute()
                        if (immersive) volumeTick++
                        true
                    }
                    is VideoKeyAction.Speed -> session?.stepSpeed(action.delta).let { true }
                    VideoKeyAction.ToggleZoom -> { toggleZoom(!zoomed); true }
                    null -> false
                }
            },
    ) {
        if (immersive) {
            // 竖屏视频：抖音 / TikTok 式的界面，占被给的地方那么大（双栏时只有右栏）
            ImmersionScreen(
                graph = graph,
                item = item,
                detail = d,
                session = session,
                danmaku = danmaku,
                onDanmakuChange = onDanmakuChange,
                loadError = loadError,
                comments = comments,
                poster = poster,
                commentsOpen = commentsOpen,
                onCommentsOpenChange = setCommentsOpen,
                onClose = onClose,
                onToggleZoom = { toggleZoom(true) },
                volumeText = volumeText,
                onLogin = onLogin,
                onStep = switch,
                nextItem = nextItem,
                previousItem = previousItem,
                hint = switchHint,
                onRetry = if (loadError != null) ({ reload++ }) else null,
                modifier = Modifier.fillMaxSize(),
            )
        } else {
        Column(Modifier.fillMaxSize()) {
            DetailTopBar(
                graph, item, onClose, loaded = d?.item,
                // 视频详情：平台名**一进来就显示**。
                // 窄屏顶栏原来跟着播放器下面那一列滚动淡入，视频页一打开顶上就是播放器、根本没滚过，等于一直不显示。
                // 图文详情的窄屏顶栏照旧滚动淡入（PostDetailScreen 那边没动）。
                titleAlpha = { 1f },
            )
            BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
                val boxW = maxWidth
                val boxH = maxHeight
                val playerSlot: @Composable (Modifier) -> Unit = { m ->
                    if (session != null && !zoomed) {
                        // 窗口宽、高度又被 playerHeight 封了顶时，播放器（含弹幕、控件）保持 16:9 居中，两侧是黑边
                        Box(m.background(Color.Black), contentAlignment = Alignment.Center) {
                            PlayerView(
                                session, danmaku, onDanmakuChange, zoomed = false, onToggleZoom = { toggleZoom(true) },
                                modifier = Modifier.fillMaxHeight().aspectRatio(16f / 9f),
                                poster = playerPoster(shown, d),
                                posterPlaceholder = playerPoster(shown, d)?.let { cover ->
                                    LocalDetailThumbnails.current.hint(cover)?.thumbnail
                                },
                                onLogin = onLogin,
                            )
                        }
                    } else {
                        Box(m.background(Color.Black), contentAlignment = Alignment.Center) {
                            // 播放器还没建好（详情在飞）：先画卡片上已经下好的封面，别给一块黑屏 + 转圈
                            val cover = playerPoster(shown, d)
                            if (cover != null) {
                                DetailPhoto(
                                    url = cover,
                                    placeholder = LocalDetailThumbnails.current.hint(cover)?.thumbnail,
                                    contentDescription = null,
                                    contentScale = ContentScale.Fit,
                                    placeholderColor = Color.Black,
                                    modifier = Modifier.fillMaxSize(),
                                )
                            }
                            if (session == null && loadError == null) {
                                CircularProgressIndicator(Modifier.size(32.dp), color = Color.White, strokeWidth = 3.dp)
                            }
                        }
                    }
                }
                Column(Modifier.fillMaxSize()) {
                    playerSlot(Modifier.fillMaxWidth().height(playerHeight(boxW, boxH)))
                    VideoBody(
                        graph = graph,
                        item = shown,
                        detail = d,
                        loadError = loadError,
                        onRetry = { reload++ },
                        session = session,
                        comments = comments,
                        poster = poster,
                        commentTotal = shown.stats.comments ?: item.stats.comments,
                        onOpenReplies = onOpenReplies,
                        onOpenRelated = onOpenRelated,
                        descriptionTranslation = descriptionTranslation,
                        listState = bodyListState,
                        modifier = Modifier.weight(1f).fillMaxWidth(),
                    )
                }
            }
        }
        }
        // 放大：占满整个窗口。用不抢焦点的 Popup 盖在整个窗口上（所以返回键不被它吃掉，走页面 / DetailPane 那一层），
        // 键盘还是由页面处理。系统栏隐藏 + 横屏视频转横屏见 [FullscreenEffect]。
        if (zoomed && session != null) {
            Popup(
                popupPositionProvider = WindowOrigin,
                properties = PopupProperties(focusable = false, dismissOnBackPress = false, dismissOnClickOutside = false, clippingEnabled = false),
            ) {
                PlayerView(
                    session, danmaku, onDanmakuChange, zoomed = true, onToggleZoom = { toggleZoom(false) },
                    modifier = Modifier.fillMaxSize(),
                    poster = playerPoster(shown, d),
                    posterPlaceholder = playerPoster(shown, d)?.let { cover ->
                        LocalDetailThumbnails.current.hint(cover)?.thumbnail
                    },
                    onLogin = onLogin,
                )
            }
        }
    }
    if (loginDialog && deviceLogin != null) {
        feedhub.app.login.DeviceCodeLoginDialog(
            platformLabel = item.platform.label,
            source = deviceLogin,
            onDismiss = { loginDialog = false },
            onSuccess = { loginDialog = false; session?.retry() },
            foreground = graph.foreground,
        )
    }
}

/** 沉浸界面里调音量之后，画面中间那行音量值显示多久。 */
private const val VOLUME_HINT_MS = 1200L

internal object WindowOrigin : PopupPositionProvider {
    override fun calculatePosition(anchorBounds: IntRect, windowSize: IntSize, layoutDirection: LayoutDirection, popupContentSize: IntSize) =
        IntOffset.Zero
}

// ---------------------------------------------------------------- 信息

private fun LazyListScope.infoItems(
    graph: AppGraph,
    item: FeedItem,
    detail: FeedDetail?,
    loadError: String?,
    onRetry: () -> Unit,
    descriptionTranslation: RichTextTranslation?,
) {
    item("info") {
        Column(Modifier.fillMaxWidth().padding(vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            item.title?.takeIf { it.isNotBlank() }?.let {
                // 标题也走表情渲染：平台表情代码画成行内小图 + Unicode emoji 用彩色字体，
                // 和图文详情 / 正文 / 卡片同一套。
                DetailTitleText(it, titleEmotes(item), MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            }
            if (loadError != null) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(loadError, color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
                    TextButton(onRetry) { Text("重试") }
                }
            }
            UploaderRow(graph, detail, item)
            StatsRow(item)
            Description(detail, descriptionTranslation)
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

/** UP 主：头像、名字、关注按钮。关注是写操作，只在用户点按钮时才调用 [FollowSource.setFollowing]。 */
@Composable
private fun UploaderRow(graph: AppGraph, detail: FeedDetail?, item: FeedItem) {
    val host = LocalDetailHost.current
    val author = item.author
    val follow = detail?.follow
    val followSource = graph.source(item.platform) as? FollowSource
    var following: Boolean? by remember(follow) { mutableStateOf(follow?.following) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Avatar(author?.avatar, 40)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(author?.name ?: "", style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            // 发布时间；贴吧、小黑盒等有所属的吧 / 话题的，接在后面
            val meta = listOfNotNull(
                item.publishedAt?.let { fullTime(it, item.publishedAtPrecision) },
                item.community?.takeIf { it.isNotBlank() }?.let { communityLabel(it, LocalPlatforms.current[item.platform]?.texts) },
            ).joinToString(" · ")
            if (meta.isNotEmpty()) Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (follow != null && followSource != null) {
            val isFollowing = following == true
            val click: () -> Unit = {
                if (!busy) {
                    busy = true
                    scope.launch {
                        try {
                            withContext(Dispatchers.IO) { followSource.setFollowing(follow.target, !isFollowing) }
                            following = !isFollowing
                            host?.showToast((if (isFollowing) "已取消关注 " else "已关注 ") + follow.target.name)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: NotLoggedInException) {
                            host?.showToast("请先在“我的”里登录${item.platform.label}")
                        } catch (e: Exception) {
                            host?.showToast("操作失败：${e.message}")
                        } finally {
                            busy = false
                        }
                    }
                }
            }
            // 贴吧关注的是吧（不是发帖的人），按钮写明
            val forum = follow.target.kind == FollowKind.FORUM
            if (isFollowing) {
                OutlinedButton(click, enabled = !busy, contentPadding = PaddingValues(horizontal = 14.dp, vertical = 4.dp)) { Text(if (forum) "已关注吧" else "已关注") }
            } else {
                Button(click, enabled = !busy, contentPadding = PaddingValues(horizontal = 14.dp, vertical = 4.dp)) { Text(if (forum) "关注吧" else "关注") }
            }
        }
    }
}

@Composable
private fun StatsRow(item: FeedItem) {
    val s = item.stats
    Row(horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.CenterVertically) {
        s.views?.let { Stat(Icons.Outlined.PlayCircleOutline, it) }
        s.danmaku?.let { Stat(Icons.Outlined.Subtitles, it) }
        s.likes?.let { Stat(Icons.Outlined.ThumbUp, it) }
        s.coins?.let { Stat(Icons.Outlined.MonetizationOn, it) }
        s.favorites?.let { Stat(Icons.Outlined.StarBorder, it) }
    }
}

@Composable
private fun Stat(icon: ImageVector, n: Long) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null, tint = muted, modifier = Modifier.size(16.dp))
        Spacer(Modifier.width(3.dp))
        Text(compactCount(n), style = MaterialTheme.typography.labelMedium, color = muted)
    }
}

/**
 * 简介：富文本（链接、@、图片都能点，走 DetailHost）；长了先折叠，点"展开"看全部。
 *
 * 下面那一行是翻译入口。
 */
@Composable
private fun Description(detail: FeedDetail?, translation: RichTextTranslation?) {
    val body = detail?.body ?: return
    val text = remember(body) { body.plainText() }
    if (text.isBlank()) return
    val shown = translation?.text ?: body
    var expanded by remember(detail) { mutableStateOf(false) }
    val long = text.length > 90 || text.count { it == '\n' } >= 3 || body.blocks.size > 3
    val style = MaterialTheme.typography.bodyMedium.copy(color = MaterialTheme.colorScheme.onSurfaceVariant)
    Column {
        // 收起时只显示纯文字的前三行（按高度裁会露出半行）；展开才按富文本排（链接、图片）
        if (expanded || !long) {
            RichTextView(shown, style = style, maxImageWidth = 360.dp)
        } else {
            Text(shown.plainText(), style = style, maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
        if (long) {
            Text(
                if (expanded) "收起" else "展开",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.clickable { expanded = !expanded }.padding(top = 2.dp),
            )
        }
        TranslationEntryRow(translation?.entry)
    }
}

// ---------------------------------------------------------------- 播放器下面的一列、标签栏

/** 列表左右各留的边距。 */
private val SidePadding = 14.dp

/** 窄于这个宽度（手机竖屏）标签栏收紧留白，免得"评论 N | 相关推荐 | 默认 热门 最新"一行放不下。 */
private val CompactWidth = 420.dp

/**
 * 播放器下面的一列：信息（标题、UP 主、数据、简介）、分 P、粘性的"评论 | 相关推荐"标签栏，再是当前标签的内容。
 * 标签选中的是哪个记在这里；评论的分页状态（[comments]）和相关推荐（[detail]）都在外面，换标签不重新加载。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun VideoBody(
    graph: AppGraph,
    item: FeedItem,
    detail: FeedDetail?,
    loadError: String?,
    onRetry: () -> Unit,
    session: PlaybackSession?,
    comments: CommentsState,
    poster: Poster,
    commentTotal: Long?,
    onOpenReplies: (Comment) -> Unit,
    onOpenRelated: (FeedItem) -> Unit,
    /** 简介那一行翻译入口。 */
    descriptionTranslation: RichTextTranslation?,
    /** 播放器下面这一列的滚动位置：外面（视频详情页）也用它给顶栏算"滚下去淡入平台名"的不透明度。 */
    listState: LazyListState,
    modifier: Modifier = Modifier,
) {
    // 相关推荐的卡片（信息流卡片复用）：曝光按"行"算，行号要和相关推荐那一项的 key（"related-<行号>"）对上。
    // 平台标成 18+ 的整条不显示：列表和曝光跟踪都用过滤后的这一份。
    // 放在标签判断之前算：一页推荐全被藏掉时"相关推荐"这个标签也不该出现。
    val related = remember(detail) {
        AdultFilter.visible(detail?.related.orEmpty().filter { it.detail is DetailTarget.Native })
    }
    // 只有 B站有"相关推荐"；别的平台详情里没给就不显示这个标签（详情还没回来时按插件描述的
    // capabilities.relatedVideos 猜，免得标签闪一下）
    val tabs = if (detail == null) {
        if (LocalPlatforms.current[item.platform]?.capabilities?.relatedVideos == true) VideoTab.entries else listOf(VideoTab.COMMENTS)
    } else {
        if (related.isNotEmpty()) VideoTab.entries else listOf(VideoTab.COMMENTS)
    }
    var selectedTab by remember { mutableStateOf(VideoTab.COMMENTS) }
    val tab = if (selectedTab in tabs) selectedTab else VideoTab.COMMENTS
    val scope = rememberCoroutineScope()
    // 标签栏是列表里的第几项：信息一项，分 P 有的话再一项
    val tabsIndex = if (hasParts(detail, session)) 2 else 1
    val selectTab: (VideoTab) -> Unit = { next ->
        if (next != tab) {
            selectedTab = next
            // 评论 / 推荐已经翻过一段、标签栏钉在顶上时，新标签从头看起，不停在上一个标签的中间
            if (tabsPinned(listState.firstVisibleItemIndex, listState.firstVisibleItemScrollOffset, tabsIndex)) {
                scope.launch { listState.scrollToItem(tabsIndex) }
            }
        }
    }
    BoxWithConstraints(modifier) {
        val columns = relatedColumns(maxWidth - SidePadding * 2)
        val compact = maxWidth < CompactWidth
        val relatedRows = remember(related, columns) { related.chunked(columns) }
        // 相关推荐不是当前这个页签时等于"被盖住"（covered），和首页被全屏详情盖住同一条路径：结算，不是丢弃。
        // 接线见 exposureInputs，别再在这里自己 and 一遍。
        val exposure = rememberUpdatedState(
            exposureInputs(
                recordBehavior = graph.settings.value.recordBehavior,
                covered = tab != VideoTab.RELATED,
                windowFocused = LocalWindowInfo.current.isWindowFocused,
            ),
        )
        val relatedTracker = remember(graph.library) {
            ExposureTracker(graph.library, ExposureSurface.RELATED, enabled = { exposure.value.enabled })
        }
        ExposureTicker(relatedTracker, foreground = exposure.value.foreground) {
            listVisibleRelatedItems(listState, relatedRows)
        }
        // 桌面上标题、简介、评论的文字要能选中复制
        SelectionContainer {
            LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(start = SidePadding, end = SidePadding, bottom = 24.dp)) {
                infoItems(graph, item, detail, loadError, onRetry, descriptionTranslation)
                partsItems(detail, session)
                stickyHeader(key = "tabs") {
                    VideoTabBar(tabs, tab, commentTotal, selectTab, comments.sort, comments::select, compact = compact, sortable = comments.sortable)
                }
                when (tab) {
                    VideoTab.COMMENTS -> commentsList(comments, poster, onOpenReplies)
                    VideoTab.RELATED -> relatedItems(related, detail, loading = loadError == null, columns, onOpenRelated)
                }
            }
        }
    }
}

/**
 * "评论 N | 相关推荐"标签栏：选中的字用主色、下面一道短横线；评论标签选中时右边带排序（默认 / 热门 / 最新）。
 * 底色要不透明，粘在顶上时滚到下面的内容才不会透出来。
 */
@Composable
private fun VideoTabBar(
    tabs: List<VideoTab>,
    tab: VideoTab,
    commentTotal: Long?,
    onSelect: (VideoTab) -> Unit,
    sort: CommentSort,
    onSort: (CommentSort) -> Unit,
    compact: Boolean,
    sortable: Boolean = true,
) {
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.background)) {
        Row(Modifier.fillMaxWidth().height(48.dp), verticalAlignment = Alignment.CenterVertically) {
            for (t in tabs) {
                val label = if (t == VideoTab.COMMENTS && commentTotal != null) "${t.label} ${compactCount(commentTotal)}" else t.label
                TabLabel(label, selected = t == tab, compact = compact, onClick = { onSelect(t) })
            }
            Spacer(Modifier.weight(1f))
            if (tab == VideoTab.COMMENTS && sortable) CommentSortChips(sort, onSort, compact)
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

@Composable
private fun TabLabel(text: String, selected: Boolean, compact: Boolean, onClick: () -> Unit) {
    val color = if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant
    Box(
        Modifier.fillMaxHeight().clip(RoundedCornerShape(topStart = 8.dp, topEnd = 8.dp))
            .pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onClick).padding(horizontal = if (compact) 10.dp else 14.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = MaterialTheme.typography.titleSmall, fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal, color = color)
        if (selected) {
            Box(
                Modifier.align(Alignment.BottomCenter).width(24.dp).height(3.dp)
                    .clip(RoundedCornerShape(topStart = 3.dp, topEnd = 3.dp)).background(color),
            )
        }
    }
}

// ---------------------------------------------------------------- 分 P、相关推荐

/** 有分 P 列表要显示：不止一 P，而且播放会话已经建好。 */
private fun hasParts(detail: FeedDetail?, session: PlaybackSession?) = session != null && (detail?.video?.parts?.size ?: 0) > 1

/**
 * 分 P：整宽的卡片网格（列数 [partColumns]，每格上面"P3 · 12:30"、下面标题），在播的那一 P 高亮，点了就切过去。
 * 默认只摆前几行（[partsCollapsed]），多了点"展开全部"。
 */
private fun LazyListScope.partsItems(detail: FeedDetail?, session: PlaybackSession?) {
    if (!hasParts(detail, session)) return
    val parts = detail?.video?.parts.orEmpty()
    item("parts") {
        var all by remember { mutableStateOf(false) }
        val current = session?.part
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val columns = partColumns(maxWidth)
            val collapsed = partsCollapsed(columns)
            // 播到折叠起来看不见的那一 P（自动下一 P、点了别处的链接）就自动展开，别让高亮藏起来
            val currentIndex = parts.indexOfFirst { it.id == current?.id }
            LaunchedEffect(currentIndex, collapsed) { if (currentIndex >= collapsed) all = true }
            val shownParts = if (all) parts else parts.take(collapsed)
            Column(Modifier.fillMaxWidth().padding(vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(PartGap)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("分 P（${parts.size}）", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                    if (parts.size > collapsed) TextButton({ all = !all }) { Text(if (all) "收起" else "展开全部") }
                }
                shownParts.chunked(columns).forEachIndexed { r, row ->
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(PartGap)) {
                        row.forEachIndexed { c, p ->
                            val i = r * columns + c
                            PartCard(i, p, selected = p.id == current?.id, onClick = { session?.loadPart(i) }, modifier = Modifier.weight(1f))
                        }
                        repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
                    }
                }
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

@Composable
private fun PartCard(index: Int, part: VideoPart, selected: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val container = if (selected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainer
    val onContainer = if (selected) MaterialTheme.colorScheme.onPrimaryContainer else MaterialTheme.colorScheme.onSurface
    Column(
        modifier.clip(RoundedCornerShape(8.dp)).background(container)
            .pointerHoverIcon(PointerIcon.Hand)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        Text(
            listOfNotNull("P${index + 1}", part.durationSec?.let(::duration)).joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = if (selected) onContainer else MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(part.title ?: "", style = MaterialTheme.typography.bodyMedium, color = onContainer, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** 相关推荐：复用信息流的视频卡片，[columns] 列；详情还在加载（[loading]）转圈，没有就写一句。 */
private fun LazyListScope.relatedItems(related: List<FeedItem>, detail: FeedDetail?, loading: Boolean, columns: Int, onOpen: (FeedItem) -> Unit) {
    if (related.isEmpty()) {
        item("related-empty") {
            Box(Modifier.fillMaxWidth().padding(vertical = 32.dp), contentAlignment = Alignment.Center) {
                if (detail == null && loading) {
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                } else {
                    Text("没有相关推荐", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        return
    }
    itemsIndexed(related.chunked(columns), key = { i, _ -> "related-$i" }) { _, row ->
        Row(Modifier.fillMaxWidth().padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            // 一排里的卡片共用一份算好的文案（一屏一次过边界，见 CardTexts.kt）
            ProvideCardTexts(row) {
                for (rel in row) FeedCard(rel, onClick = { onOpen(rel) }, modifier = Modifier.weight(1f))
            }
            repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
        }
    }
}

/**
 * 播放器的垫图：这条视频的封面。
 * 优先详情里的（平台可能换了封面），其次信息流卡片上的；都没有就 null（那几秒还是黑的，但不会再更糟）。
 */
internal fun playerPoster(item: FeedItem, detail: FeedDetail?): String? =
    (detail?.item ?: item).media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: item.media.filterIsInstance<Media.Video>().firstOrNull()?.cover
        ?: (detail?.item ?: item).media.filterIsInstance<Media.Image>().firstOrNull()?.url
