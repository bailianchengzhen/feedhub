package feedhub.app.video

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.isSpecified
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupProperties
import coil3.compose.AsyncImage
import feedhub.app.detail.DetailHost
import feedhub.app.detail.LocalDetailHost
import feedhub.app.library.MemoryLibrary
import feedhub.app.ui.duration
import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.VideoPart
import kotlinx.coroutines.delay

/**
 * 一个详情页里正文夹着的视频（[Block.Video]）的播放协调：**同一时间只有一个持有播放器**，点另一个时前一个暂停并释放（记住位置，再点接着播）。
 * 由 [DetailHost] 持有（每条打开的内容一个）；每个视频用 `id/partId` 当 key。
 */
@Stable
class InlineVideoState {
    /** 持有播放器的那个视频的 key（播放中或暂停着）；null = 都在显示封面。 */
    var activeKey: String? by mutableStateOf(null)
        private set

    /** 正在播的那个是否放大（全屏，用完整的常规播放器）。 */
    var zoomed by mutableStateOf(false)

    /** 视频详情页的主播放器的"暂停"（页面顶上那个）；没有就是 null。行内视频开播时先把它暂停。 */
    var pauseMain: (() -> Unit)? = null

    /** 这次开播是不是静音（自动播放时静音开播）。 */
    var mutedStart = false
        private set

    private class Resume(val positionMs: Long, val durationMs: Long)

    private val resume = HashMap<String, Resume>()

    /** [key] 开播：主播放器先暂停；前一个在播的自然不再是 [activeKey]，它的组合会收到并释放播放器、记下位置。 */
    fun start(key: String, muted: Boolean = false) {
        pauseMain?.invoke()
        if (activeKey != key) zoomed = false
        mutedStart = muted
        activeKey = key
    }

    /** [key] 不再播了（滚出可见区域、或者页面关了）：只有它自己是当前在播的才清掉。 */
    fun stop(key: String) {
        if (activeKey == key) stopActive()
    }

    fun stopActive() {
        activeKey = null
        zoomed = false
    }

    /** [key] 的播放器被释放前播到了 [positionMs]（总长 [durationMs]）：记下来，下次点开接着播（快播完了就从头）。 */
    fun savePosition(key: String, positionMs: Long, durationMs: Long) {
        resume[key] = Resume(resumeStart(positionMs, durationMs), durationMs)
    }

    /** 下次点开从哪开始（毫秒）；没记过就是 0。 */
    fun resumePosition(key: String): Long = resume[key]?.positionMs ?: 0L

    /** 封面上细进度线的位置（0~1）：记过位置且知道总长才有，没有就是 0。 */
    fun resumeFraction(key: String): Float = resume[key]?.let { r ->
        if (r.durationMs > 0 && r.positionMs > 0) (r.positionMs.toFloat() / r.durationMs).coerceIn(0f, 1f) else 0f
    } ?: 0f
}

/**
 * 正文里的视频，像 GIF 一样嵌在文字中间：
 * - **外观**：按视频本身的宽高比，最宽和正文里的图片一样，太高的竖屏视频限高（[inlineVideoWidth]）；圆角和图片一致，没有黑底的播放器框，也没有进度条 / 清晰度 / 编码标签 / 音量。
 * - **默认是封面**：封面 + 中间的播放键 + 时长，**点一下才开始加载播放**（想改成滚到可见时静音自动播，见 [InlineVideoDefaults.AUTOPLAY_WHEN_VISIBLE]）。
 * - **播放中**：播完从头循环；点视频切换播放 / 暂停（暂停时中间是播放键）；鼠标悬停才淡入底部的 播放 / 暂停、倍速、全屏（[EmbeddedPlayer]），底边一条很细、不能拖的进度线。
 *   声音照常有，用全 App 共用的音量（[AppVolume]），嵌入上没有音量控件。默认清晰度不超过 720P（[InlineVideoDefaults.EMBEDDED_QUALITY]）。
 * - **全屏**：点全屏盖满整个窗口，用完整的常规播放器（[PlayerView]：进度条、清晰度、音量……），清晰度换成设置里的默认；Esc 或按钮退出，回到嵌入，位置和播放 / 暂停状态都保持。
 * - **同一时间只有一个在播**（[InlineVideoState]）：点另一个时前一个暂停（释放播放器，再点从原位置继续）；滚出可见区域（[shouldStopWhenHidden]）、详情关了、切页签都会暂停并释放。
 *
 * 播放地址不在 [Block.Video] 里（会过期），点了之后用 `id/partId` 向平台的 [feedhub.core.VideoSource.play] 现取。
 * 进度不记到历史（用一次性的资料库，不会冲掉帖子本身的播放进度）。不在 [feedhub.app.detail.DetailPane] 里时（拿不到 [LocalDetailHost]）只画封面。
 *
 * @param maxWidth 视频最宽多少（评论里不用撑满）。
 */
@Composable
fun InlineVideo(
    block: Block.Video,
    modifier: Modifier = Modifier,
    maxWidth: Dp = Dp.Unspecified,
    autoplayWhenVisible: Boolean = InlineVideoDefaults.AUTOPLAY_WHEN_VISIBLE,
) {
    val host = LocalDetailHost.current
    val key = remember(block) { "${block.id}/${block.partId}" }
    val state = host?.videos
    // 页面关了、或者被 LazyColumn 回收（滑出屏幕）：别让它还占着"正在播"
    DisposableEffect(key, state) { onDispose { state?.stop(key) } }

    // 比例：播起来之前用声明的宽高，播放器报出真实尺寸之后用真实的（框正好贴着画面，没有黑边）；播放器释放后也记着
    var actual by remember(key) { mutableStateOf(IntSize.Zero) }
    val ratio = inlineVideoRatio(block.width, block.height, actual.width, actual.height)

    val active = state != null && state.activeKey == key
    val zoomed = active && state.zoomed

    // 可见比例：被所有祖先（滚动列表的视口等）裁剪之后还看得见的部分
    val fraction = remember(key) { mutableFloatStateOf(1f) }
    val hidden by remember(key) { derivedStateOf { shouldStopWhenHidden(fraction.floatValue) } }
    val shown by remember(key) { derivedStateOf { fraction.floatValue >= InlineVideoDefaults.AUTOPLAY_ABOVE } }
    // 只在"刚变成看不见"的那一下停（点开一个只露出一角的封面不会立刻又被停掉）；放大时页面可以不在视野里
    LaunchedEffect(hidden) {
        if (hidden && state != null && state.activeKey == key && !state.zoomed) state.stop(key)
    }
    LaunchedEffect(shown, autoplayWhenVisible, state) {
        if (state != null && shouldAutoplay(autoplayWhenVisible, fraction.floatValue, state.activeKey)) state.start(key, muted = true)
    }

    BoxWithConstraints(modifier.fillMaxWidth(), contentAlignment = if (maxWidth.isSpecified) Alignment.TopStart else Alignment.TopCenter) { // 和图片一样：正文里居中，评论里靠左
        val w = inlineVideoWidth(this.maxWidth, maxWidth, ratio)
        Box(
            Modifier.width(w).aspectRatio(ratio).clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceVariant)
                .onGloballyPositioned { c ->
                    if (c.isAttached) {
                        val b = c.boundsInWindow()
                        fraction.floatValue = visibleFraction(b.width.toInt(), b.height.toInt(), c.size.width, c.size.height)
                    }
                },
        ) {
            if (state != null && host != null && active) {
                InlinePlayer(block, host, state, key, zoomed, onVideoSize = { actual = it })
            } else {
                Cover(block, resumeFraction = state?.resumeFraction(key) ?: 0f, onClick = state?.let { s -> { s.start(key) } })
            }
        }
    }
}

/** 封面：图 + 中间的播放键 + 右下角时长 + 左下角标题；记过位置的（被释放过的）底边有一条细进度线。没有封面就是底色。 */
@Composable
private fun Cover(block: Block.Video, resumeFraction: Float, onClick: (() -> Unit)?, zoomedBehind: Boolean = false) {
    Box(
        Modifier.fillMaxSize()
            .then(if (onClick != null) Modifier.pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onClick) else Modifier),
    ) {
        CoverImage(block)
        if (zoomedBehind) {
            // 放大看的时候页面里留着封面（布局不跳），压暗一点表示"在别处播放"
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)))
            return@Box
        }
        block.title?.takeIf { it.isNotBlank() }?.let {
            Box(Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(44.dp).background(TitleScrim))
            Text(
                it,
                color = Color.White,
                fontSize = 12.sp,
                maxLines = 1,
                modifier = Modifier.align(Alignment.BottomStart).padding(start = 8.dp, bottom = 8.dp).fillMaxWidth(0.7f),
            )
        }
        PlayBadge(Modifier.align(Alignment.Center))
        block.durationSec?.takeIf { it > 0 }?.let {
            Text(
                duration(it),
                color = Color.White,
                fontSize = 11.sp,
                modifier = Modifier.align(Alignment.BottomEnd).padding(8.dp).background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(4.dp)).padding(horizontal = 5.dp, vertical = 1.dp),
            )
        }
        if (resumeFraction > 0f) {
            Box(Modifier.align(Alignment.BottomStart).fillMaxWidth().height(LineHeight).background(LineTrack))
            Box(Modifier.align(Alignment.BottomStart).fillMaxWidth(resumeFraction).height(LineHeight).background(LineFill))
        }
    }
}

@Composable
private fun CoverImage(block: Block.Video) {
    block.cover?.let {
        AsyncImage(model = it, contentDescription = block.title, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
    }
}

/** 中间的圆形播放键（封面和暂停时都用它）。 */
@Composable
private fun PlayBadge(modifier: Modifier) {
    Box(modifier.size(52.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.5f)), contentAlignment = Alignment.Center) {
        Icon(Icons.Filled.PlayArrow, contentDescription = "播放", tint = Color.White, modifier = Modifier.size(32.dp))
    }
}

/** 点了之后才进入组合：建引擎和会话、取地址、开播；离开组合（换了别的视频在播 / 滑出屏幕 / 页面关了）就释放，并记下位置。 */
@Composable
private fun InlinePlayer(block: Block.Video, host: DetailHost, state: InlineVideoState, key: String, zoomed: Boolean, onVideoSize: (IntSize) -> Unit) {
    val graph = host.graph
    val scope = rememberCoroutineScope()
    val source = host.videoSource()
    if (source == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text("${host.platform.label}的视频还不能在这里播放", color = MaterialTheme.colorScheme.onSurfaceVariant, fontSize = 13.sp)
        }
        return
    }
    val session = remember(key) {
        PlaybackSession(
            // 平台客户端：和详情页同一个约定（平台代理 / fail-closed / 请求头）
            engine = createPlayerEngine(host.graph.ctx.client(host.platform).build()),
            source = source,
            // 只用来给会话当 key；资料库是一次性的，所以进度不会写到真实的历史里
            item = FeedItem(host.platform, "inline:$key", ItemKind.VIDEO, detail = DetailTarget.Native(block.id)),
            id = block.id,
            parts = listOf(VideoPart(block.partId, durationSec = block.durationSec, width = block.width, height = block.height)),
            danmakuSegmentMs = null,
            library = MemoryLibrary(),
            settings = { graph.settings.value },
            scope = scope,
            volume = AppVolume,
            builtinProxy = { graph.proxyCore?.proxyMode() ?: feedhub.app.NO_BUILTIN_CORE },
            relayFactory = { graph.relay(host.platform) },
        ).apply {
            qualityLimit = InlineVideoDefaults.EMBEDDED_QUALITY // 嵌入先用不超过 720P 的，放大再换设置里的
            setLooping(true)
            if (state.mutedStart) startMuted()
        }
    }
    DisposableEffect(session) {
        session.loadPart(0, startMs = state.resumePosition(key))
        onDispose {
            state.savePosition(key, session.state.nowMs(), session.state.durationMs)
            session.release()
        }
    }
    val videoW = session.state.videoWidth
    val videoH = session.state.videoHeight
    LaunchedEffect(videoW, videoH) { if (videoW > 0 && videoH > 0) onVideoSize(IntSize(videoW, videoH)) }

    val danmaku = graph.settings.value.danmaku
    val onDanmakuChange: (DanmakuSettings) -> Unit = { v -> graph.settings.update { it.copy(danmaku = v) } }
    // 要登录才能播（YouTube 设备码）：错误下面给一个按钮直接打开登录，成功后重试这一条
    val deviceLogin = remember(source) { source as? feedhub.core.DeviceCodeLoginSource }
    var loginDialog by remember(session) { mutableStateOf(false) }
    val onLogin: (() -> Unit)? = deviceLogin?.let { { loginDialog = true } }
    LaunchedEffect(zoomed) {
        host.refocus() // 放大 / 还原之后焦点会丢（点过的按钮离开了组合），拉回详情页，Esc 才有用
        if (zoomed) {
            // 放大：用常规播放器——不循环，声音按共用的音量，清晰度换成设置里的默认（要重新加载一下，所以放在放大的时候做）
            session.setLooping(false)
            session.restoreVolume()
            session.qualityLimit = null
            session.applyDefaultQuality()
        } else {
            session.setLooping(true)
            // 在全屏里播完了（没有循环）再退出来：嵌入像 GIF 一样从头接着播
            if (session.state.ended) session.engine.play()
        }
    }
    if (!zoomed) {
        EmbeddedPlayer(block, session, onZoom = { state.zoomed = true }, modifier = Modifier.fillMaxSize(), onLogin = onLogin)
    } else {
        Cover(block, resumeFraction = 0f, onClick = null, zoomedBehind = true)
        // 放大：占满整个窗口（不是系统全屏），和视频页一样用不抢焦点的 Popup 盖在整个窗口上；Esc 由详情页（DetailHost.dismissTop）收回
        Popup(
            popupPositionProvider = WindowOrigin,
            properties = PopupProperties(focusable = false, dismissOnBackPress = false, dismissOnClickOutside = false, clippingEnabled = false),
        ) {
            PlayerView(session, danmaku, onDanmakuChange, zoomed = true, onToggleZoom = { state.zoomed = false }, modifier = Modifier.fillMaxSize(), onLogin = onLogin)
        }
    }
    if (loginDialog && deviceLogin != null) {
        feedhub.app.login.DeviceCodeLoginDialog(
            platformLabel = host.platform.label,
            source = deviceLogin,
            onDismiss = { loginDialog = false },
            onSuccess = { loginDialog = false; session.retry() },
            foreground = graph.foreground,
        )
    }
}

private val TitleScrim = Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.55f)))
private val ControlsScrim = Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.7f)))
private val Accent = Color(0xFFFB7299)
private val LineHeight = 3.dp
/** 行内视频那条细进度线按多宽算（卡片 / 正文里的视频大多不超过这么宽；算宽了只是间隔偏短）。 */
private val InlineTrackGuess = 720.dp
private val LineTrack = Color.Black.copy(alpha = 0.35f)
private val LineFill = Accent

/**
 * 嵌入状态下的播放器：只有画面。悬停时底部淡入 播放 / 暂停、倍速、全屏三个控件，移开淡出；点画面切换播放 / 暂停，暂停时中间是播放键；
 * 底边一条很细、不能拖的进度线一直在。第一帧出来之前画面上盖着封面。
 */
@Composable
private fun EmbeddedPlayer(
    block: Block.Video,
    session: PlaybackSession,
    onZoom: () -> Unit,
    modifier: Modifier = Modifier,
    onLogin: (() -> Unit)? = null,
) {
    val engine = session.engine
    val state = session.state
    val interaction = remember { MutableInteractionSource() }
    val hovered by interaction.collectIsHoveredAsState()
    var menuOpen by remember { mutableStateOf(false) }
    Box(modifier.hoverable(interaction).pointerHoverIcon(PointerIcon.Hand)) {
        engine.Surface(Modifier.fillMaxSize())
        if (!state.hasFrame) CoverImage(block)
        // 点视频本身切换播放 / 暂停
        Box(Modifier.fillMaxSize().pointerInput(session) { detectTapGestures(onTap = { session.togglePlay() }) })
        EmbeddedStatus(session, onLogin)
        AnimatedVisibility(
            visible = (hovered || menuOpen) && engine.unavailable == null && session.error == null && state.error == null,
            enter = fadeIn(tween(150)),
            exit = fadeOut(tween(250)),
            modifier = Modifier.align(Alignment.BottomCenter),
        ) {
            EmbeddedControls(session, onZoom, onMenuOpen = { menuOpen = it })
        }
        if (engine.unavailable == null) ProgressLine(state, Modifier.align(Alignment.BottomStart))
    }
}

/** 画面中间：不能播放的提示 / 出错说明（带重试）、缓冲圈、暂停时的播放键。 */
@Composable
private fun BoxScope.EmbeddedStatus(session: PlaybackSession, onLogin: (() -> Unit)?) {
    val state = session.state
    val unavailable = session.engine.unavailable
    val error = unavailable ?: session.error ?: state.error
    // 读取 / 第一帧之前的转圈马上显示；播放中途重新缓冲（包括循环回开头的那一下）超过 0.35 秒才转，免得闪一下
    val first = session.loading || (!state.hasFrame && state.buffering)
    val rebuffering = state.buffering && state.playing
    var slow by remember { mutableStateOf(false) }
    LaunchedEffect(rebuffering) {
        if (rebuffering) {
            delay(350)
            slow = true
        } else {
            slow = false
        }
    }
    when {
        error != null -> {
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.55f)))
            Column(
                Modifier.align(Alignment.Center).padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(error, color = Color.White, fontSize = 13.sp, textAlign = TextAlign.Center)
                if (session.needsLogin && onLogin != null) {
                    TextButton(onClick = onLogin) { Text("登录${session.platformLabel}", color = Accent) }
                }
                session.errorHint?.let {
                    Text(it, color = Color.White.copy(alpha = 0.75f), fontSize = 11.sp, textAlign = TextAlign.Center)
                }
                if (unavailable == null) {
                    TextButton(onClick = { session.retry() }) { Text("重试", color = Accent) }
                }
            }
        }
        first || (rebuffering && slow) -> CircularProgressIndicator(Modifier.align(Alignment.Center).size(32.dp), color = Color.White, strokeWidth = 3.dp)
        !state.playing -> PlayBadge(Modifier.align(Alignment.Center))
    }
}

/** 悬停时底部的控件：播放 / 暂停、倍速、全屏。 */
@Composable
private fun EmbeddedControls(session: PlaybackSession, onZoom: () -> Unit, onMenuOpen: (Boolean) -> Unit) {
    val state = session.state
    Row(
        Modifier.fillMaxWidth().background(ControlsScrim)
            .pointerInput(Unit) { detectTapGestures { } } // 点在控件条的空白处不要穿透成"切换播放"
            .padding(horizontal = 4.dp).padding(top = 14.dp, bottom = LineHeight + 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SmallIconButton(if (state.playing) Icons.Filled.Pause else Icons.Filled.PlayArrow, if (state.playing) "暂停" else "播放") { session.togglePlay() }
        Spacer(Modifier.weight(1f))
        SpeedButton(session, onMenuOpen)
        SmallIconButton(Icons.Filled.Fullscreen, "全屏", onZoom)
    }
}

@Composable
private fun SmallIconButton(icon: ImageVector, description: String, onClick: () -> Unit) {
    Box(Modifier.size(32.dp).clip(CircleShape).clickable(onClick = onClick), contentAlignment = Alignment.Center) {
        Icon(icon, contentDescription = description, tint = Color.White, modifier = Modifier.size(22.dp))
    }
}

/** 倍速：一个小按钮，显示当前倍速（"1.5x"），点开是按钮正上方那条小列表（[SpeedPopup]，档位和常规播放器的一样）。 */
@Composable
private fun SpeedButton(session: PlaybackSession, onMenuOpen: (Boolean) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val current = session.state.speed
    Box {
        Box(
            Modifier.height(28.dp).clip(RoundedCornerShape(14.dp)).clickable { open = true; onMenuOpen(true) }.padding(horizontal = 8.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(PlaybackSpeed.label(current), color = if (current == PlaybackSpeed.DEFAULT) Color.White else Accent, fontSize = 13.sp)
        }
        SpeedPopup(open, current, onDismiss = { open = false; onMenuOpen(false) }) { s ->
            open = false
            onMenuOpen(false)
            session.setSpeed(s)
        }
    }
}

/** 底边一条很细、不能拖的进度线：灰底 + 白色已播（不知道总长就不画）。播放中每帧跟着走，暂停时偶尔看一眼。 */
@Composable
private fun ProgressLine(state: PlayerState, modifier: Modifier = Modifier) {
    val progress = remember(state) { mutableFloatStateOf(0f) }
    val visible = LocalWindowVisible.current
    // 原来播放中每帧都读（整个窗口跟着每帧重画）；现在按"一次走约 2 像素"读，卡片宽的细线最短 33 ms（≈一帧视频）
    val widthPx = with(LocalDensity.current) { InlineTrackGuess.roundToPx() }
    LaunchedEffect(state, visible) {
        while (visible) {
            awaitProgressUpdate(state, widthPx, minMs = 33) { now ->
                val total = state.durationMs
                progress.floatValue = if (total > 0) (now.toFloat() / total).coerceIn(0f, 1f) else 0f
            }
        }
    }
    val lineHeight = with(LocalDensity.current) { LineHeight.toPx() }
    Box(
        modifier.fillMaxWidth().height(LineHeight).drawBehind {
            if (state.durationMs <= 0) return@drawBehind
            drawRect(LineTrack, size = Size(size.width, lineHeight))
            drawRect(LineFill, topLeft = Offset.Zero, size = Size(size.width * progress.floatValue, lineHeight))
        },
    )
}
