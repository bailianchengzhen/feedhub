package feedhub.app.video

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.AppSettings
import feedhub.app.DefaultQuality
import feedhub.app.NO_BUILTIN_CORE
import feedhub.app.library.Library
import feedhub.app.proxyFor
import feedhub.core.FeedItem
import feedhub.core.KeyValueStore
import feedhub.core.NotLoggedInException
import feedhub.core.PlayInfo
import feedhub.core.ProxyMode
import feedhub.core.RiskControlException
import feedhub.core.VideoPart
import feedhub.core.VideoSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/**
 * 整个 App 共用的音量：新开的播放器（视频页的、正文里的）从这里取初值，在任何一个播放器里调了音量 / 静音都记在这里，下一个沿用。
 * 只在内存里，重启后回到 100%、不静音。[PlaybackSession] 默认各用各的（测试里互不影响），界面里传 [AppVolume]。
 */
@Stable
class SharedVolume(percent: Int = 100, muted: Boolean = false) {
    var percent by mutableIntStateOf(percent.coerceIn(0, 100))
    var muted by mutableStateOf(muted)
}

/** 界面里所有播放器共用的那一份音量。 */
val AppVolume = SharedVolume()

/**
 * 默认清晰度被 [limit] 封顶之后是哪一档：设置里选的和上限里**低**的那个（[DefaultQuality] 按从高到低排）。[limit] 为 null 就是设置本身。
 * 行内嵌入用它把默认清晰度压到 720P。
 */
fun limitQuality(setting: DefaultQuality, limit: DefaultQuality?): DefaultQuality =
    if (limit == null) setting else DefaultQuality.entries[maxOf(setting.ordinal, limit.ordinal)]

/**
 * 一个视频页的播放会话：取播放地址 → 选流 → 交给 [engine] 播；换清晰度 / 换分 P；续播和进度保存；弹幕数据。
 * 界面（[PlayerView]）和键盘快捷键都操作它。
 */
class PlaybackSession(
    val engine: PlayerEngine,
    private val source: VideoSource,
    private val item: FeedItem,
    private val id: String,
    private val parts: List<VideoPart>,
    private val danmakuSegmentMs: Long?,
    private val library: Library,
    private val settings: () -> AppSettings,
    private val scope: CoroutineScope,
    /** 网络请求跑在哪（取播放地址、弹幕）；测试里换成 Unconfined。 */
    private val io: CoroutineContext = Dispatchers.IO,
    /** 记"上次看到第几 P"（[Library] 的进度只按条目存，没有分 P）；不给就总是从第一 P 开始。 */
    private val lastPartStore: KeyValueStore? = null,
    /** 音量 / 静音：开始时从这里取，之后调了写回去（界面里传 [AppVolume]，所有播放器共用）。 */
    private val volume: SharedVolume = SharedVolume(),
    /**
     * 「内置核心」那一档现在的走法（`graph.proxyCore?.proxyMode()`）。不给 = 没接核心，走代理的平台直接失败。
     * 播放器这一侧：内置核心是本机 SOCKS5，`souphttpsrc` 表达不了 → 和「指定 SOCKS5」一样改走中转
     * （见 `PlayerProxy.gstProxyUri` / `GstPlayerEngine.needsRelay`）。
     */
    private val builtinProxy: () -> ProxyMode = { NO_BUILTIN_CORE },
    /**
     * 分片中转的工厂（App 里是 `graph.relayFactory(platform)`），每次取流现取：DASH / HLS 的清单和分片
     * 都经本机中转（Rust 里那个服务）去取，于是平台代理 / fail-closed / 请求头都跟着那一条判定走
     * （见 [MediaSource.relayFactory]）。null = 不中转（测试 / 没有 AppGraph 的地方）。
     */
    private val relayFactory: (() -> RelayFactory?)? = null,
    /**
     * 沉浸播放预取到的播放地址：按分 P id 给，对得上就不再去平台取一次。
     * 取到的是 null 就照旧现取（预取失败 / 分 P 对不上 / 换了清晰度都走这条路）。
     */
    private val prefetchedPlayInfo: (String) -> PlayInfo? = { null },
) {
    init {
        if (engine.volumeFollowsSystem) {
            // 安卓：系统媒体音量是唯一真相。**不**把 App 记住的那份推下去——不然用户刚用侧键调好，
            // 一开下一个视频又被旧的百分比盖回去。反过来把系统现在的值收进共用的那一份（界面显示用）。
            volume.percent = engine.state.volume
            volume.muted = engine.state.muted
        } else {
            engine.setVolume(volume.percent)
            engine.setMuted(volume.muted)
        }
        // App 关闭时收尾线程要能找到这个会话（存进度 + 释放播放器），见 [ActivePlaybackSessions]
        ActivePlaybackSessions.register(this)
    }

    /** 这条播放会话属于哪个平台（性能记录用：播放事件里只记平台 id，不记标题）。 */
    internal val platform: feedhub.core.Platform get() = item.platform

    var part: VideoPart? by mutableStateOf(null)
        private set
    var info: PlayInfo? by mutableStateOf(null)
        private set
    var choice: StreamChoice? by mutableStateOf(null)
        private set
    var loading by mutableStateOf(false)
        private set

    /** 取地址 / 选流失败的说明；播放器自己的错误在 [PlayerState.error]。 */
    var error: String? by mutableStateOf(null)
        private set

    /**
     * 这次失败**登录就能解决**（平台要求登录 / YouTube 要确认不是机器人）：界面据此给
     * "登录 X 后再播" + 一个直接打开登录的按钮。
     */
    var needsLogin: Boolean by mutableStateOf(false)
        private set

    /** 错误下面那句补充说明（例如"换个代理节点再试"）；null = 不说。 */
    var errorHint: String? by mutableStateOf(null)
        private set

    /** 界面文案用的平台名（"YouTube" / "B站"…），不读插件类。 */
    val platformLabel: String get() = item.platform.label

    /**
     * 默认清晰度的上限：行内嵌入时是 720P（省资源），null = 不限，按设置里的默认清晰度。只影响"没手动选过"时的默认挑选；
     * 放大（去掉上限）之后调 [applyDefaultQuality] 才会换到设置里的那一档。
     */
    var qualityLimit: DefaultQuality? = null

    /** 用户在清晰度菜单里手动选过的档位；选过之后 [applyDefaultQuality] 不再动它。 */
    private var manualQuality: Int? = null

    /** 现在"默认"该挑哪一档：设置里的默认清晰度，被 [qualityLimit] 封顶。 */
    private fun defaultQuality(): DefaultQuality = limitQuality(settings().quality, qualityLimit)

    /**
     * 这个视频要走哪个代理：按它所在平台查设置。
     * 每次取流时现查，所以改了设置下一次播放就生效。
     */
    private fun videoProxy(): ProxyMode = settings().proxyFor(item.platform, builtinProxy())

    /** 这个视频有弹幕吗：平台给了每段多长（[danmakuSegmentMs]）才有；没有弹幕的平台（除 B站外都是）播放器隐藏弹幕按钮和弹幕设置入口、也不去取弹幕。 */
    val hasDanmaku: Boolean = danmakuSegmentMs != null

    /** 弹幕开关：开始时用设置里的默认值，在播放器里切换只管这一次，不改设置。 */
    var danmakuOn by mutableStateOf(settings().danmakuOn)

    /** 弹幕设置面板开着没有：播放器上的"弹幕设置"按钮开关它，控件条开着面板时不自动隐藏。面板的内容改的是 [AppSettings.danmaku]，不在这里。 */
    var danmakuPanelOpen by mutableStateOf(false)

    /** 键盘 / 鼠标操作过就加一，控件条据此重新显示。 */
    var activity by mutableIntStateOf(0)
        private set

    var danmakuTimeline: DanmakuTimeline? by mutableStateOf(null)
        private set
    private var danmakuLoader: DanmakuLoader? = null

    private var loadJob: Job? = null

    val state: PlayerState get() = engine.state

    private val partKey = "video.lastPart.${item.key}"

    /** 第一次打开该播第几 P：上次看到的那一 P（找不到就第一 P）。 */
    val initialPartIndex: Int
        get() = feedhub.player.RustPlayer.initialPartIndex(parts.map { it.id }, lastPartStore?.get(partKey))

    fun poke() {
        activity++
    }

    /**
     * 播第 [index] 个分 P。第一次打开（[resume]）时从历史里的进度续播；不 [resume] 时从 [startMs] 开始
     * （行内视频被释放后再点开，从上次的位置接着播）。
     */
    fun loadPart(index: Int, resume: Boolean = false, startMs: Long = 0L) {
        val p = parts.getOrNull(index)
        if (p == null) {
            error = feedhub.player.RustPlayer.noPartText()
            return
        }
        saveProgress()
        part = p
        if (engine.unavailable != null) return // 播不了（GStreamer 和 VLC 都没装）就别去取播放地址、弹幕了
        loadJob?.cancel()
        // 性能记录（关着时这一行什么都不做，见 PerfHooks.watchVideo）
        feedhub.app.perf.PerfHooks.watchVideo(this, item.platform, scope)
        error = null
        errorHint = null
        needsLogin = false
        loading = true
        info = null
        choice = null
        danmakuLoader = null
        danmakuTimeline = if (!hasDanmaku) null else DanmakuTimeline(danmakuSegmentMs ?: DEFAULT_SEGMENT_MS, settings().danmaku.filter).also { tl ->
            danmakuLoader = DanmakuLoader(tl, p.durationSec?.let { it * 1000L }) { seg -> withContext(io) { source.danmaku(id, p.id, seg) } }
        }
        // 预取到的那一份先在**当前帧同步读出来**：预取对象随时可能被"预取下一条"清掉（见 PlayPrefetcher）
        val prefetched = prefetchedPlayInfo(p.id)
        loadJob = scope.launch {
            try {
                // 取播放地址：记一行（耗时 / 限速排队）、总超时按网络错误、失败记 play_failed + logcat
                val pi = prefetched ?: feedhub.app.perf.meteredRequest(
                    feedhub.app.perf.PerfRequest.PLAY, item.platform, io,
                ) { source.play(id, p.id) }.value
                info = pi
                val sel = StreamSelection.select(pi, settings().codec, defaultQuality(), auto = StreamSelection.autoOrder(engine.hardwareCodecs), capability = engine.decodeCapability)
                if (sel == null) {
                    error = feedhub.player.RustPlayer.noStreamText()
                    return@launch
                }
                choice = sel
                val duration = (pi.durationMs ?: p.durationSec?.let { it * 1000L } ?: 0L)
                val savedProgress = library.progressCached(item.key)
                    ?: withContext(io) { library.progress(item.key) }
                // 续播位置（离结尾不到 5 秒就从头）在 Rust
                val start = feedhub.player.RustPlayer.startPosition(
                    resume = resume,
                    savedMs = savedProgress,
                    startMs = startMs,
                    durationMs = duration,
                )
                engine.open(sel.toMediaSource(pi.headers, duration.takeIf { it > 0 }, videoProxy(), relayFactory?.invoke()) ?: return@launch, start)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // 说什么 / 要不要给"登录"按钮 / 要不要提"换节点"：全在 Rust（风控红线：不重试、不换参数）
                val text = machine.playFailure(
                    feedhub.player.PlayFailure.of(e),
                    item.platform.label,
                    item.platform in settings().proxiedPlatforms,
                )
                needsLogin = text.needsLogin
                error = text.error
                errorHint = text.hint
            } finally {
                loading = false
            }
        }
    }

    fun retry() {
        val idx = parts.indexOf(part).coerceAtLeast(0)
        loadPart(idx, resume = true)
    }

    /** 换清晰度（用户手动选）：同一份 [PlayInfo] 里重新选一路，从当前位置接着播。 */
    fun selectQuality(quality: Int) {
        val pi = info ?: return
        manualQuality = quality
        val sel = StreamSelection.select(pi, settings().codec, settings().quality, quality, StreamSelection.autoOrder(engine.hardwareCodecs), engine.decodeCapability) ?: return
        switchTo(sel, pi)
    }

    /**
     * 按"默认"清晰度（设置里的，被 [qualityLimit] 封顶）重新挑一路：和现在播的不是同一路才换（换了要重新打开，从当前位置接着播，暂停着的还是暂停）。
     * 行内视频放大时用：先去掉 [qualityLimit] 再调它，从 720P 升到设置里的清晰度。用户手动选过的清晰度不动。
     */
    fun applyDefaultQuality() {
        val pi = info ?: return
        if (manualQuality != null) return
        val sel = StreamSelection.select(pi, settings().codec, defaultQuality(), null, StreamSelection.autoOrder(engine.hardwareCodecs), engine.decodeCapability) ?: return
        switchTo(sel, pi)
    }

    private fun switchTo(sel: StreamChoice, pi: PlayInfo) {
        if (sel.video == choice?.video && sel.progressive == choice?.progressive) return
        val source = sel.toMediaSource(pi.headers, state.durationMs.takeIf { it > 0 }, videoProxy(), relayFactory?.invoke()) ?: return
        choice = sel
        engine.open(source, state.nowMs(), autoPlay = state.playing || state.buffering)
    }

    /**
     * 播放 / 暂停。**播完（[PlayerState.ended]）时点播放 = 从头重播**：以前直接 `play()`，
     * Media3 在 `STATE_ENDED` 上会不会自己回到开头并不由我们控制，而我们的进度 / 时间是引擎位置的镜像，
     * 结果就是实测 #1 —— 画面回开头了、进度条还停在结尾 3:38。所以这里明确先回到 0 再播。
     */
    fun togglePlay() {
        apply(machine.togglePlay(snapshot()))
        poke()
    }

    /**
     * 切后台 / 锁屏时**正在播** → 回前台自动接着播。
     * 用户自己点过播放 / 暂停就作废：自己暂停的回来仍然停着。被音频焦点 / 拔耳机压下来的不算"在播"。
     */
    /** 切后台：照旧立刻停，记下停之前是不是在播（onPause、onStop 各调一次，第二次不会把记号抹掉）。 */
    fun pauseForBackground() {
        apply(machine.pauseForBackground(snapshot()))
    }

    /** 回前台：切走前在播才续播，从原位置接着放（播完的不重播）。 */
    fun resumeFromBackground() {
        machine.resumeFromBackground(snapshot())?.let(::apply)
        poke()
    }

    /** 弹幕看的是 `state.speed`（引擎报的真值），所以 [apply] 之后不用额外同步。 */

    fun seekBy(deltaMs: Long) {
        // 位置要按**插值后的当前位置**算（引擎报的值最多落后 250ms）
        val now = state.nowMs()
        apply(machine.seekBy(snapshot().copy(positionMs = now), deltaMs))
        poke()
    }

    fun seekTo(ms: Long) {
        engine.seekTo(ms)
        poke()
    }

    fun changeVolume(delta: Int) = setVolume(state.volume + delta)

    /** 设置音量（0~100），同时取消静音；记到共用的音量里。 */
    fun setVolume(percent: Int) {
        val p = percent.coerceIn(0, 100)
        engine.setMuted(false)
        engine.setVolume(p)
        volume.percent = p
        volume.muted = false
        poke()
    }

    /**
     * 这个播放器先静音，但**不动系统 / 共用的音量**（滚到可见时自动播放就是静音的）；放大看的时候 [restoreVolume] 把声音还回来。
     * 安卓上走 [PlayerEngine.setSilenced]（只让这一个播放器不出声），不是把系统媒体音量拧到 0。
     */
    fun startMuted() {
        engine.setSilenced(true)
    }

    /** 按共用的音量设回来：先解除本播放器的静音；系统音量（安卓）本来就是那一份，不用再推。 */
    fun restoreVolume() {
        engine.setSilenced(false)
        if (!engine.volumeFollowsSystem) {
            engine.setVolume(volume.percent)
            engine.setMuted(volume.muted)
        }
    }

    /** 播完是否从头循环（行内视频像 GIF 一样），见 [PlayerEngine.setLooping]。 */
    fun setLooping(loop: Boolean) = engine.setLooping(loop)

    /**
     * 播放状态机：播完重播、后台续播、
     * 长按加速 / 锁定、倍速的三种状态、进度该不该存、取流失败怎么说——判定全在那边，这里只把
     * `PlayerState` 同步进去、把回来的 [PlayEffect] 依次做掉。
     */
    private val machine = feedhub.player.RustPlaybackSession()

    /** 设置倍速（换清晰度 / 分 P / 拖动都不变）；弹幕随进度走，滚动速度见 [DanmakuDefaults.scrollMs]。用户自己选的，会取消正在进行的临时加速。 */
    fun setSpeed(rate: Float) {
        apply(machine.setSpeed(rate))
        poke()
    }

    /** 临时倍速：按下时调（比如长按 3 倍加速），记住当前倍速；已经在临时加速中就只换速率，原值还是最早那个。 */
    fun holdSpeed(rate: Float) {
        apply(machine.holdSpeed(snapshot(), rate))
        poke()
    }

    /** 松手：恢复成临时加速之前的倍速；没有在临时加速中就什么都不做。 */
    fun releaseSpeed() {
        val effect = machine.releaseSpeed() ?: return
        apply(effect)
        poke()
    }

    /** 长按倍速锁着没有：锁着 = 松手也保持 3 倍速（[longPressSwipeUp]）。用户从菜单换倍速（[setSpeed]）就解除。 */
    val speedLocked: Boolean get() = machine.speedLocked()

    /** 长按倍速的手指还按着没有（拖动手势看到它就不抢这一场）。 */
    val speedPressing: Boolean get() = machine.speedPressing()

    /**
     * 长按按下：没锁 → 临时 3 倍速；锁着 → 临时恢复成锁定之前的倍速（松手回到 3 倍速）。
     */
    fun longPressStart() {
        apply(machine.longPressStart(snapshot()))
        poke()
    }

    /** 长按时上滑：没锁 → 锁定 3 倍速；锁着 → 取消锁定。一次长按只算一次；返回切换后是不是锁着。 */
    fun longPressSwipeUp(): Boolean = machine.longPressSwipeUp()

    /** 松手（或手势被取消）：锁着 → 3 倍速；没锁 → 恢复长按之前的倍速。 */
    fun longPressEnd() {
        machine.longPressEnd()?.let { apply(it); poke() }
    }

    /**
     * 锁定 / 长按之前的倍速（提示里显示"恢复成几倍"用）：临时加速中是**加速前**那个，否则就是当前倍速。
     * 判定在 Rust（`Session::base_speed`；基线里是 `heldFrom ?: state.speed`，WIP 一度退化成直接取当前值）。
     */
    val baseSpeed: Float get() = machine.baseSpeed(state.speed)

    /** 快一档 / 慢一档（键盘 `]` / `[`）。 */
    fun stepSpeed(delta: Int) = setSpeed(PlaybackSpeed.step(state.speed, delta))

    fun toggleMute() {
        // 先算出目标值再写：引擎（安卓的 Media3）是 post 到主线程生效的，state 不会立刻变，读回来会拿到旧值
        val next = !state.muted
        engine.setMuted(next)
        volume.muted = next
        poke()
    }

    /**
     * 取弹幕：每秒看一眼进度，需要的段还没取就取（取到的缓存着，提前取下一段）。在界面的协程里长期运行。
     *
     * 以前不管暂停 / 播完 / 切后台都每秒醒一次（真机后台 5 分钟里
     * `DefaultExecutor` ≈ 5/s，这一条占 1/s），暂停的视频页在后台也一直有计时器。暂停时没有新进度，
     * 什么都不用取；继续播再接着看（见 [everyWhilePlaying]）。
     */
    suspend fun runDanmakuLoader() {
        everyWhilePlaying(
            playing = { state.playing && !state.ended },
            everyMs = 1_000L,
            onTick = { danmakuLoader?.ensure(state.nowMs()) },
        )
    }

    /**
     * 保存播放进度到本地历史（[Library] 的进度只按条目存：存的是当前这一 P 的位置）；离开页面、换分 P、播放中每隔几秒调。
     * 另外记下当前是第几 P，下次打开接着看这一 P；第一 P 不用记（没记就是第一 P），所以单 P 视频不会往存储里写东西。
     */
    @Synchronized
    fun saveProgress() {
        if (part == null) return
        val idx = parts.indexOf(part).coerceAtLeast(0)
        // "该不该存 / 存哪个位置 / 要不要记第几 P"全在 Rust（`Session.save_progress`）
        for (effect in machine.saveProgress(idx, state.nowMs(), state.durationMs, parts.getOrNull(idx)?.id)) {
            when (effect) {
                is feedhub.player.PlayEffect.SaveProgress ->
                    library.saveProgress(item.key, effect.positionMs, effect.durationMs)

                is feedhub.player.PlayEffect.SavePart -> lastPartStore?.let { store ->
                    if (store.get(partKey) != effect.partId) store.put(partKey, effect.partId)
                }

                else -> Unit
            }
        }
    }

    /** `PlayerState` 里状态机要看的那几项。 */
    private fun snapshot(): feedhub.player.PlaybackSnapshot = feedhub.player.PlaybackSnapshot(
        playing = state.playing,
        buffering = state.buffering,
        positionMs = state.nowMs(),
        durationMs = state.durationMs,
        ended = state.ended,
        speed = state.speed,
        muted = state.muted,
        volume = state.volume,
    )

    /**
     * 把 Rust 给回来的一件事做掉。
     *
     * 四种"动引擎"的效果（播放 / 暂停 / 重播 / 跳转 / 改速率）在这里落地；`SaveProgress` / `SavePart` 只有
     * [saveProgress] 会发，别处不该出现（出现了也没事：忽略）。
     */
    private fun apply(effect: feedhub.player.PlayEffect) {
        when (effect) {
            feedhub.player.PlayEffect.Play -> engine.play()
            feedhub.player.PlayEffect.Pause -> engine.pause()
            feedhub.player.PlayEffect.Replay -> {
                state.ended = false
                state.jump(0)
                engine.seekTo(0)
                engine.play()
            }
            is feedhub.player.PlayEffect.Seek -> engine.seekTo(effect.positionMs)
            is feedhub.player.PlayEffect.SetRate -> engine.setRate(effect.rate)
            is feedhub.player.PlayEffect.SaveProgress -> Unit
            is feedhub.player.PlayEffect.SavePart -> Unit
        }
    }

    fun release() {
        feedhub.app.perf.PerfHooks.stopWatchingVideo(this)
        loadJob?.cancel()
        saveProgress()
        engine.release()
        ActivePlaybackSessions.unregister(this)
    }

    private companion object {
        const val DEFAULT_SEGMENT_MS = 360_000L
    }
}
