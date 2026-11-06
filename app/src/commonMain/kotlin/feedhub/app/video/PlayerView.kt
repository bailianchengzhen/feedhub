package feedhub.app.video

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.FullscreenExit
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Subtitles
import androidx.compose.material.icons.filled.SubtitlesOff
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.zIndex
import androidx.compose.ui.graphics.Brush
import androidx.compose.foundation.clickable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.PopupPositionProvider
import feedhub.app.detail.DetailPhoto
import feedhub.app.ui.duration
import kotlin.math.min
import kotlin.math.roundToInt
import kotlinx.coroutines.delay

private val Scrim = Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.15f), Color.Black.copy(alpha = 0.92f)))
private val Accent = Color(0xFFFB7299)

/**
 * 播放器：画面 + 弹幕 + 控件。控件在鼠标 / 键盘有动作时显示，播放中 3 秒没动静就隐藏（暂停时一直显示）。
 * 点画面暂停 / 继续，双击放大 / 还原；放大就是占满窗口（[zoomed]），不用系统全屏。
 *
 * 弹幕按 [danmaku] 画；控件条上弹幕开关旁边的"弹幕设置"按钮打开 [DanmakuSettingsPanel]（放大时也能用），面板里改的立刻通过 [onDanmakuChange] 生效并存下来。
 *
 * @param controls 要不要画底部的常规控件条（进度条、清晰度、音量、放大……）。沉浸界面（[ImmersionScreen]）自己画
 *   一条细进度条和右侧按钮列，用 false；横屏视频"全屏"（放大）时还是用常规控件条。
 *   **触屏手势两种样子都有**，只是单击 / 双击的语义不同。
 * @param onLogin 这个平台的登录入口（现在只有 YouTube 的设备码登录）：会话报"要登录才能播"时，
 *   错误下面多一个按钮直接打开它；null = 这个平台没有能在这里打开的登录方式（只显示提示文字）。
 */
@Composable
fun PlayerView(
    session: PlaybackSession,
    danmaku: DanmakuSettings,
    onDanmakuChange: (DanmakuSettings) -> Unit,
    zoomed: Boolean,
    onToggleZoom: () -> Unit,
    modifier: Modifier = Modifier,
    controls: Boolean = true,
    /** 这条视频的封面：**第一帧出来之前先垫着**。 */
    poster: String? = null,
    /**
     * 封面的占位：封面原图到之前先画它，
     * 换视频时那几秒就不会先黑再糊（见 [feedhub.app.detail.DetailPhoto]）。
     */
    posterPlaceholder: String? = null,
    onLogin: (() -> Unit)? = null,
    /**
     * 弹幕画不画在播放器里。沉浸界面竖屏放横屏片源、上方空白够高时传 false：弹幕改由 [ImmersionScreen] 画在画面**上方**
     * 那块空白里。取弹幕照旧由这里跑。
     */
    drawDanmaku: Boolean = true,
    /**
     * 播放器内部要不要自己挂点击 / 触屏手势。沉浸界面（[ImmersionScreen]）手势覆盖整个舞台（含上下模糊留白区）
     * 并改成零延迟单击暂停 + 底部进度条调进度，这里传 false。
     */
    interactive: Boolean = true,
    /** 画面正中的状态层（缓冲圈 / 错误说明 / 登录按钮）画不画在这里；沉浸界面挪到舞台手势层之上画，传 false。 */
    showCenterStatus: Boolean = true,
) {
    val engine = session.engine
    val state = session.state
    var menuOpen by remember { mutableStateOf(false) }
    var controlsVisible by remember { mutableStateOf(true) }
    var size by remember { mutableStateOf(IntSize.Zero) }
    // "用户动了"的计数：任何操作都重置 3 秒自动隐藏的计时（单击显隐控件也走它）
    var activityTick by remember { mutableIntStateOf(0) }
    LaunchedEffect(activityTick, session.activity, state.playing, menuOpen, session.loading, session.danmakuPanelOpen, controlsVisible) {
        if (!controls || !controlsVisible) return@LaunchedEffect // 沉浸界面没有控件条：别为它排这个延时
        if (state.playing && !menuOpen && !session.danmakuPanelOpen) {
            delay(3000)
            controlsVisible = false
        }
    }
    // 键盘 / 控件条上的操作（播放暂停、拖动、调音量……）总是把控件条叫回来；触屏上"点画面"是显隐切换，见下面
    LaunchedEffect(session.activity, session.loading) {
        controlsVisible = true
        activityTick++
    }
    // 长按加速的手指还按着没有：拖动那边看到它就不抢这次手势（不然普通拖动一消费，长按就被取消、倍速会弹回去）。
    // 锁定状态和"按着没有"都在会话里（[PlaybackSession.speedLocked] / [PlaybackSession.speedPressing]）；松手一定清掉
    // "按着"，浮层到时间就收
    val speedHeld = session.speedPressing
    // 手势中间浮层（拖动进度 / 亮度 / 音量 / 长按倍速）
    var hint by remember { mutableStateOf<PlayerHint?>(null) }
    var hintTick by remember { mutableIntStateOf(0) }
    LaunchedEffect(hintTick, speedHeld) {
        if (hint == null) return@LaunchedEffect
        if (speedHeld) return@LaunchedEffect // 长按还按着就一直显示
        delay(HINT_LINGER_MS)
        hint = null
    }
    val density = LocalDensity.current.density
    val brightness = rememberWindowBrightness()
    val haptic = LocalHapticFeedback.current
    // 桌面：鼠标一动就把控件条叫回来（原来就是这样，手势只加在触屏上）；触屏 / 无控件条不这么做
    val pointerModifier = if (isTouchDevice || !controls) {
        Modifier
    } else {
        Modifier.pointerInput(Unit) {
            // 只看鼠标有没有动，不消费事件
            awaitPointerEventScope {
                while (true) {
                    val e = awaitPointerEvent(PointerEventPass.Initial)
                    if (e.type != PointerEventType.Exit) {
                        if (!controlsVisible) controlsVisible = true
                        activityTick++
                    }
                }
            }
        }
    }
    val gestureModifier: Modifier? = if (interactive && isTouchDevice) {
        Modifier.playerTouchGestures(
            durationMs = { state.durationMs },
            positionMs = { state.nowMs() },
            onTap = {
                if (controls) {
                    controlsVisible = !controlsVisible
                    activityTick++
                } else {
                    session.togglePlay()
                }
            },
            onDoubleTap = {
                if (controls) session.togglePlay() else onToggleZoom()
                haptic.performHapticFeedback(HapticFeedbackType.LongPress)
            },
            onLongPressStart = {
                val wasLocked = session.speedLocked
                session.longPressStart()
                haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                hint = if (wasLocked) {
                    PlayerHint.Speed(session.baseSpeed, locked = false, note = "上滑取消锁定")
                } else {
                    PlayerHint.Speed(PlayerGestureMath.LONG_PRESS_SPEED, locked = false, note = "上滑锁定")
                }
                hintTick++
            },
            onLongPressSwipeUp = {
                val locked = session.longPressSwipeUp()
                haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                hint = if (locked) {
                    PlayerHint.Speed(PlayerGestureMath.LONG_PRESS_SPEED, locked = true)
                } else {
                    PlayerHint.Speed(session.baseSpeed, locked = false, note = "已取消锁定")
                }
                hintTick++
            },
            onLongPressEnd = {
                session.longPressEnd()
                hintTick++ // 松手后浮层停留 HINT_LINGER_MS 再收
            },
            isSpeedHeld = { session.speedPressing },
            onSeek = { session.seekTo(it) },
            onSeekPreview = { hint = it; hintTick++ },
            volumePercent = { if (state.muted) 0 else state.volume },
            onVolume = { session.setVolume(it) },
            brightness = brightness,
            onLevel = { hint = it; hintTick++ },
            density = density,
            verticalLevels = controls,
        )
    } else {
        null
    }
    Box(
        modifier.background(Color.Black).onSizeChanged { size = it }.then(pointerModifier),
    ) {
        engine.Surface(Modifier.fillMaxSize())
        // 首帧之前垫封面：换视频时新播放器 / 新 Surface 从创建到出第一帧之间是黑的，
        // 有封面就看不到那段黑屏；第一帧到了（state.hasFrame）就撤掉。
        if (poster != null && !state.hasFrame && engine.unavailable == null) {
            DetailPhoto(
                url = poster,
                placeholder = posterPlaceholder,
                contentDescription = null,
                contentScale = ContentScale.Fit,
                // 画面底色是黑的：封面没下来之前也用黑的，别在画面中间先铺一块灰
                placeholderColor = Color.Black,
                modifier = Modifier.fillMaxSize(),
            )
        }
        val timeline = session.danmakuTimeline
        if (session.danmakuOn && timeline != null && engine.unavailable == null) {
            if (drawDanmaku) {
            // 弹幕只画在**画面本身**的矩形里。
            // 取不到片源分辨率（还没出第一帧）就先铺满，出了分辨率马上收进画面。
            val videoBox = videoRect(size, state.videoWidth, state.videoHeight)
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                val area = if (videoBox == null) {
                    Modifier.fillMaxSize()
                } else {
                    with(LocalDensity.current) {
                        Modifier.size(videoBox.width.toDp(), videoBox.height.toDp())
                    }
                }
                Box(area.clipToBounds()) {
                    DanmakuLayer(timeline, state, danmaku, Modifier.fillMaxSize())
                }
            }
            }
            LaunchedEffect(timeline) { session.runDanmakuLoader() }
        }
        // 点击 / 双击：常规详情页走手势 / 双击放大；沉浸界面（interactive = false）在外面铺满整舞台接管
        if (interactive) {
            Box(
                (gestureModifier ?: Modifier.pointerInput(zoomed) {
                    detectTapGestures(onTap = { session.togglePlay() }, onDoubleTap = { onToggleZoom() })
                }).fillMaxSize(),
            )
        }
        if (showCenterStatus) CenterStatus(session, onLogin)
        PlayerHintOverlay(hint)
        // 底部控件（沉浸界面用自己的一条细进度条 + 右侧按钮列，不画这一条）
        if (controls) {
            AnimatedVisibility(
                visible = controlsVisible && engine.unavailable == null,
                enter = fadeIn(),
                exit = fadeOut(),
                // zIndex 明确把控件条放在弹幕上面；底色也换成更实的渐变，
                // 底下的弹幕不会透上来（以前是透明 → 0.72 黑，正好是控件那一行最透）
                modifier = Modifier.align(Alignment.BottomCenter).zIndex(1f),
            ) {
                Controls(session, zoomed, onToggleZoom, onMenuOpen = { menuOpen = it })
            }
            // 触屏：弹幕设置放在画面右上角
            if (isTouchDevice && session.hasDanmaku) {
                AnimatedVisibility(
                    visible = controlsVisible && engine.unavailable == null,
                    enter = fadeIn(),
                    exit = fadeOut(),
                    modifier = Modifier.align(Alignment.TopEnd).zIndex(1f),
                ) {
                    CompositionLocalProvider(LocalMiniChrome provides !zoomed) {
                        Box(Modifier.padding(4.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.35f))) {
                            DanmakuSettingsButton(session)
                        }
                    }
                }
            }
        }
        if (session.danmakuPanelOpen && engine.unavailable == null) {
            DanmakuPanelPopup(session, danmaku, onDanmakuChange, size)
        }
    }
}

/**
 * 弹幕设置面板，浮在播放器右侧。用一个能聚焦的浮层（[PanelPopup]：桌面的 Popup 带 `onPreviewKeyEvent`，安卓的没有，
 * 见那个 expect/actual）：放大时播放器自己是个不抢焦点的 Popup，里面的输入框（屏蔽词）打不了字，面板要有自己的可聚焦层；
 * 面板开着时键盘归它，空格、方向键不会去暂停 / 快进视频。
 * Esc 或点面板外面关掉。高度尽量让开底部的控件条（[PanelMinHeight] ~ [PanelMaxHeight]），放不下就在面板里滚。
 */
@Composable
private fun DanmakuPanelPopup(session: PlaybackSession, danmaku: DanmakuSettings, onChange: (DanmakuSettings) -> Unit, playerSize: IntSize) {
    val density = LocalDensity.current
    val playerW = with(density) { playerSize.width.toDp() }
    val playerH = with(density) { playerSize.height.toDp() }
    PanelPopup(
        positionProvider = remember { PanelPosition(with(density) { PanelMargin.roundToPx() }) },
        onDismissRequest = { session.danmakuPanelOpen = false },
        // Esc 关面板：只靠 PopupProperties 的 dismissOnBackPress，面板里没有聚焦的控件时收不到，所以自己接
        onPreviewKey = { e ->
            val esc = e.type == KeyEventType.KeyDown && e.key == Key.Escape
            if (esc) session.danmakuPanelOpen = false
            esc
        },
    ) {
        DanmakuSettingsPanel(
            settings = danmaku,
            onChange = onChange,
            onClose = { session.danmakuPanelOpen = false },
            modifier = Modifier.width(minOf(PanelWidth, playerW - PanelMargin * 2)).heightIn(max = maxOf(playerH - PanelMargin * 2 - ControlsReserve, PanelMinHeight).coerceAtMost(PanelMaxHeight)),
            notice = if (session.danmakuOn) null else ({
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("弹幕现在是关着的，要打开才看得到效果", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
                    TextButton(onClick = { session.danmakuOn = true }) { Text("打开") }
                }
            }),
        )
    }
}

private val PanelWidth = 340.dp
private val PanelMargin = 8.dp

/** 面板的高度：尽量不超出播放器（让开控件条），播放器太矮时也给到 [PanelMinHeight]（会伸到播放器下面去），再高也不过 [PanelMaxHeight]。 */
private val PanelMinHeight = 420.dp
private val PanelMaxHeight = 640.dp

/** 底部控件条（进度条 + 一排按钮）大约这么高，面板不压在它上面。 */
private val ControlsReserve = 80.dp

/** 面板贴着播放器右上角，离边 [margin] 像素。 */
private class PanelPosition(private val margin: Int) : PopupPositionProvider {
    override fun calculatePosition(anchorBounds: IntRect, windowSize: IntSize, layoutDirection: LayoutDirection, popupContentSize: IntSize): IntOffset =
        IntOffset((anchorBounds.right - popupContentSize.width - margin).coerceAtLeast(anchorBounds.left), anchorBounds.top + margin)
}

/**
 * 画面中间：不能播放的提示、出错说明、缓冲圈。
 *
 * 暂停时**不再**画大播放键；
 * 播放 / 暂停走点画面和控制条上的按钮。
 */
@Composable
internal fun BoxScope.CenterStatus(session: PlaybackSession, onLogin: (() -> Unit)?) {
    val state = session.state
    val unavailable = session.engine.unavailable
    val error = unavailable ?: session.error ?: state.error
    when {
        error != null -> Column(
            Modifier.align(Alignment.Center).padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(error, color = Color.White, fontSize = 14.sp, textAlign = TextAlign.Center)
            // "登录就能继续"的那种（YouTube 要确认不是机器人 / 要求登录）：给一个按钮直接打开登录
            if (session.needsLogin && onLogin != null) {
                TextButton(onClick = onLogin) { Text("登录${session.platformLabel}", color = Accent) }
            }
            session.errorHint?.let {
                Text(it, color = Color.White.copy(alpha = 0.75f), fontSize = 12.sp, textAlign = TextAlign.Center)
            }
            if (unavailable == null) {
                TextButton(onClick = { session.retry() }) { Text("重试", color = Accent) }
            }
        }
        session.loading || state.buffering -> CircularProgressIndicator(
            Modifier.align(Alignment.Center).size(36.dp),
            color = Color.White,
            strokeWidth = 3.dp,
        )
    }
}

@Composable
private fun Controls(session: PlaybackSession, zoomed: Boolean, onToggleZoom: () -> Unit, onMenuOpen: (Boolean) -> Unit) {
    val state = session.state
    // 进度条平滑：播放中按"一次走约 2 像素"读插值后的位置；暂停时不循环（等状态变），窗口看不见时不跑
    val visible = LocalWindowVisible.current
    val pos by produceState(state.positionMs, state.playing, visible) {
        value = state.nowMs()
        while (visible) awaitProgressUpdate(state) { value = it }
    }
    // 触屏上没放大（详情页里那块小播放器）用**小号控件条**：
    // 一行 36dp 的按钮在上、进度条贴着画面底边，底色是"大半透明、只有最底下压暗"的渐变（BiliPai 的 透明→0.1→0.9），
    // 没有一整条实心的黑带。放大 / 桌面还是原来那条。
    val mini = isTouchDevice && !zoomed
    val progress = @Composable {
        ProgressBar(
            pos, state.bufferedMs, state.durationMs, onSeek = { session.seekTo(it) },
            compact = !zoomed, touchHeight = if (mini) MiniProgressTouchHeight else ProgressTouchHeight,
        )
    }
    // 上边留白 6dp：进度条本体从 20dp 高变成 44dp 热区（E7），减掉这 12dp 让那条细线的**视觉位置**和以前一样
    BoxWithConstraints(
        Modifier.fillMaxWidth().background(if (mini) MiniScrim else Scrim).padding(horizontal = 8.dp)
            .padding(top = if (mini) 24.dp else 6.dp, bottom = if (mini) 0.dp else 2.dp),
    ) {
        val compact = maxWidth < 480.dp
        CompositionLocalProvider(LocalMiniChrome provides mini) { Column {
            if (!mini) progress()
            // 触屏上不放静音和音量条
            // （音量就是系统媒体音量，侧键 / 右侧上下滑都能调），弹幕设置挪到画面右上角。时间文字占剩下的宽度、
            // 放不下就截，右边的按钮（尤其是放大）永远在。
            Row(verticalAlignment = Alignment.CenterVertically) {
                IconBtn(if (state.playing) Icons.Filled.Pause else Icons.Filled.PlayArrow, if (state.playing) "暂停" else "播放") { session.togglePlay() }
                Text(
                    "${clock(pos)} / ${clock(state.durationMs)}",
                    color = Color.White,
                    fontSize = if (mini) 11.sp else 12.sp,
                    maxLines = 1,
                    softWrap = false,
                    overflow = TextOverflow.Clip,
                    modifier = Modifier.weight(1f).padding(horizontal = 2.dp),
                )
                if (session.hasDanmaku) { // 没有弹幕的平台不放弹幕开关和弹幕设置
                    IconBtn(if (session.danmakuOn) Icons.Filled.Subtitles else Icons.Filled.SubtitlesOff, "弹幕") {
                        session.danmakuOn = !session.danmakuOn
                        session.poke()
                    }
                    if (!isTouchDevice) DanmakuSettingsButton(session)
                }
                SpeedMenu(session, onMenuOpen)
                QualityMenu(session, onMenuOpen)
                if (!isTouchDevice) {
                    IconBtn(if (state.muted || state.volume == 0) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp, "静音") { session.toggleMute() }
                    if (!compact) {
                        VolumeSlider(if (state.muted) 0 else state.volume) { session.setVolume(it) }
                    }
                }
                IconBtn(if (zoomed) Icons.Filled.FullscreenExit else Icons.Filled.Fullscreen, if (zoomed) "还原" else "放大", onToggleZoom)
            }
            if (mini) progress()
        } }
    }
}

/** 小号控件条（触屏、没放大）：按钮 36dp、图标 20dp、字 12sp（BiliPai 手机竖屏是 32dp / 20dp / 11sp）。 */
private val LocalMiniChrome = staticCompositionLocalOf { false }

private val MiniScrim = Brush.verticalGradient(
    0f to Color.Transparent,
    0.35f to Color.Black.copy(alpha = 0.1f),
    1f to Color.Black.copy(alpha = 0.78f),
)

/** 小号控件条里贴底边的进度条热区（线还是 2dp，居中）。 */
private val MiniProgressTouchHeight = 22.dp

@Composable
private fun DanmakuSettingsButton(session: PlaybackSession) {
    IconBtn(Icons.Filled.Tune, "弹幕设置") {
        session.danmakuPanelOpen = !session.danmakuPanelOpen
        session.poke()
    }
}

/**
 * 控件条上的图标按钮：**热区 48dp**（Material 的最小触控目标，E7），视觉不变（还是 22dp 的白图标、没有底色）。
 * 以前写死 `size(36.dp)`，手指点不准。
 */
@Composable
private fun IconBtn(icon: ImageVector, description: String, onClick: () -> Unit) {
    if (LocalMiniChrome.current) {
        Box(
            Modifier.size(36.dp).clip(CircleShape).clickable(onClick = onClick).semantics { contentDescription = description },
            contentAlignment = Alignment.Center,
        ) { Icon(icon, contentDescription = null, tint = Color.White, modifier = Modifier.size(20.dp)) }
        return
    }
    IconButton(onClick) {
        Icon(icon, contentDescription = description, tint = Color.White, modifier = Modifier.size(22.dp))
    }
}

/** 倍速菜单：按钮上显示当前倍速（"1.5x"），点开是按钮正上方那条小列表（[SpeedPopup]），当前档高亮。 */
@Composable
private fun SpeedMenu(session: PlaybackSession, onMenuOpen: (Boolean) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val current = session.state.speed
    Box {
        TextButton(
            onClick = { open = true; onMenuOpen(true) },
            modifier = Modifier.heightIn(min = if (LocalMiniChrome.current) 36.dp else 48.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 4.dp),
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

/** 清晰度菜单：平台列出的档位，这次没给流的灰掉，当前的打勾。 */
@Composable
private fun QualityMenu(session: PlaybackSession, onMenuOpen: (Boolean) -> Unit) {
    val info = session.info ?: return
    val options = StreamSelection.qualityOptions(info)
    if (options.isEmpty()) return
    var open by remember { mutableStateOf(false) }
    val current = session.choice?.quality
    val label = options.firstOrNull { it.id == current }?.label?.let(::shortQualityLabel) ?: "清晰度"
    Box {
        TextButton(
            onClick = { open = true; onMenuOpen(true) },
            modifier = Modifier.heightIn(min = if (LocalMiniChrome.current) 36.dp else 48.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 4.dp),
        ) {
            Text(label, color = Color.White, fontSize = 13.sp)
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false; onMenuOpen(false) }) {
            for (o in options) {
                DropdownMenuItem(
                    text = { Text(o.label + if (o.id == current) "  ✓" else "") },
                    enabled = o.enabled,
                    onClick = {
                        open = false
                        onMenuOpen(false)
                        session.selectQuality(o.id)
                    },
                )
            }
        }
    }
}

/** "1080P 高清" → "1080P"。 */
internal fun shortQualityLabel(label: String): String = label.substringBefore(' ').ifEmpty { label }

@Composable
private fun VolumeSlider(volume: Int, onChange: (Int) -> Unit) {
    Box(
        Modifier.width(72.dp).height(48.dp).pointerInput(Unit) {
            awaitEachGesture {
                val down = awaitFirstDown()
                onChange((down.position.x / size.width * 100).toInt().coerceIn(0, 100))
                while (true) {
                    val e = awaitPointerEvent()
                    val c = e.changes.firstOrNull() ?: break
                    if (!c.pressed) break
                    onChange((c.position.x / size.width * 100).toInt().coerceIn(0, 100))
                    c.consume()
                }
            }
        },
        contentAlignment = Alignment.CenterStart,
    ) {
        Box(Modifier.fillMaxWidth().height(3.dp).background(Color.White.copy(alpha = 0.3f), RoundedCornerShape(2.dp)))
        Box(Modifier.fillMaxWidth(volume / 100f).height(3.dp).background(Color.White, RoundedCornerShape(2.dp)))
    }
}

/** 进度条的触控热区高度（E7：≥44dp）。画的那条线还是 3dp，居在中间。 */
internal val ProgressTouchHeight = 44.dp

/**
 * 进度条：灰底 = 全长，浅色 = 已缓冲，粉色 = 已播；点一下或拖动跳转，拖动时显示松手后的位置。沉浸界面也用它（只有这一条）。
 *
 * [compact]：详情页里没全屏的小播放器用细一号的，触控热区不变。
 */
@Composable
internal fun ProgressBar(
    positionMs: Long,
    bufferedMs: Long,
    durationMs: Long,
    onSeek: (Long) -> Unit,
    modifier: Modifier = Modifier,
    compact: Boolean = false,
    touchHeight: Dp = ProgressTouchHeight,
) {
    val line = if (compact) 2.dp else 3.dp
    var dragFraction by remember { mutableFloatStateOf(-1f) }
    val total = durationMs.coerceAtLeast(1)
    val shown = if (dragFraction >= 0) dragFraction else (positionMs.toFloat() / total).coerceIn(0f, 1f)
    val buffered = (bufferedMs.toFloat() / total).coerceIn(0f, 1f)
    Box(
        modifier.fillMaxWidth().height(touchHeight).pointerInput(durationMs) {
            if (durationMs <= 0) return@pointerInput
            awaitEachGesture {
                val down = awaitFirstDown()
                down.consume()
                var f = (down.position.x / size.width).coerceIn(0f, 1f)
                dragFraction = f
                while (true) {
                    val e = awaitPointerEvent()
                    val c = e.changes.firstOrNull() ?: break
                    f = (c.position.x / size.width).coerceIn(0f, 1f)
                    dragFraction = f
                    if (!c.pressed) break
                    c.consume()
                }
                dragFraction = -1f
                onSeek((f * durationMs).toLong())
            }
        },
        contentAlignment = Alignment.CenterStart,
    ) {
        val track = Modifier.fillMaxWidth().height(line)
        Box(track.background(Color.White.copy(alpha = 0.25f), RoundedCornerShape(2.dp)))
        Box(Modifier.fillMaxWidth(buffered).height(line).background(Color.White.copy(alpha = 0.5f), RoundedCornerShape(2.dp)))
        Box(Modifier.fillMaxWidth(shown).height(line).background(Accent, RoundedCornerShape(2.dp)))
        // 拖动块
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val thumb = if (compact) 9.dp else 12.dp
            Box(
                Modifier.padding(start = ((maxWidth - thumb) * shown)).size(thumb).background(Accent, CircleShape),
            )
        }
        if (dragFraction >= 0) {
            Text(
                clock((dragFraction * total).toLong()),
                color = Color.White,
                fontSize = 11.sp,
                modifier = Modifier.align(Alignment.TopStart).padding(start = 0.dp).offsetX(dragFraction)
                    .background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(3.dp)).padding(horizontal = 4.dp),
            )
        }
    }
}

/** 按 [fraction]（0~1）把子元素放到父元素宽度的这个位置（居中对齐在该点，不超出两端）。 */
private fun Modifier.offsetX(fraction: Float): Modifier = this.then(
    Modifier.layout { measurable, constraints ->
        val p = measurable.measure(constraints.copy(minWidth = 0))
        val parentW = constraints.maxWidth
        val x = (parentW * fraction - p.width / 2f).toInt().coerceIn(0, (parentW - p.width).coerceAtLeast(0))
        layout(p.width, p.height) { p.place(x, -p.height - 2) }
    },
)

/** 毫秒 → "1:05" / "1:02:03"。 */
internal fun clock(ms: Long): String = duration((ms / 1000).toInt().coerceAtLeast(0))

/**
 * 画面在播放器里的实际矩形（fit 居中，和播放器自己的缩放方式一致）；任何一个尺寸还不知道就返回 null
 * （= 用整个播放器当弹幕区域）。
 *
 * 实测 #3 的修法靠它：弹幕以前铺满播放器，横屏片源在竖屏播放器里时上下黑边上也有弹幕。
 */
internal fun videoRect(box: IntSize, videoWidth: Int, videoHeight: Int): IntSize? {
    if (box.width <= 0 || box.height <= 0 || videoWidth <= 0 || videoHeight <= 0) return null
    val scale = min(box.width.toFloat() / videoWidth, box.height.toFloat() / videoHeight)
    return IntSize(
        (videoWidth * scale).roundToInt().coerceAtLeast(1),
        (videoHeight * scale).roundToInt().coerceAtLeast(1),
    )
}
