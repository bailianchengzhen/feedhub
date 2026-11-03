package feedhub.app.video

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.database.ContentObserver
import android.graphics.Color
import android.media.AudioManager
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.view.ViewGroup
import androidx.annotation.OptIn
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.dash.DashMediaSource
import androidx.media3.exoplayer.dash.manifest.DashManifestParser
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.media3.exoplayer.source.MediaSource as ExoMediaSource
import androidx.media3.exoplayer.source.MergingMediaSource
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.ui.PlayerView
import feedhub.core.VideoCodec
import okhttp3.OkHttpClient
import java.io.ByteArrayInputStream
import kotlin.math.roundToInt

/**
 * 安卓的播放器：**Media3 ExoPlayer**（`PlayerEngine` 的安卓实现）。
 *
 * 数据源用 `media3-datasource-okhttp`，客户端就是**这个平台的 OkHttp 客户端**
 * （`graph.ctx.client(platform).build()`：平台代理、「指定代理没填全就失败」的 fail-closed、
 * Referer / User-Agent 都在它里面）。所以清单和分片都跟着平台代理走，**安卓上不需要桌面那套
 * 本机 `VideoRelay`**（那是给 GStreamer 的 dashdemux2 补的：它用自己的 libsoup 下分片，不吃我们设的代理；
 * Media3 的 DashMediaSource 所有请求都走我们给的 DataSource，天然就是同一个代理）。
 *
 * 各种流的形态见 [MediaPlan]：
 * - 分轨 DASH（B站 / Reddit）：用现有 [DashManifest] 拼一份静态 MPD，**sideload** 给 `DashMediaSource`
 *   （`DashMediaSource.Factory.createMediaSource(DashManifest, MediaItem)`，不发清单请求；分片按
 *   `SegmentBase` 的初始化段 / sidx 范围取）。不走 MergingMediaSource：分片 mp4 靠 sidx 才能精确跳转，
 *   而且初始化 / 索引范围平台已经给了，照抄进 MPD 最省事、和桌面 dashdemux2 一个路子。
 * - 音画一体的 mp4 / m3u8：`ProgressiveMediaSource` / `HlsMediaSource`；两条轨又没有 SegmentBase 信息时
 *   才用 `MergingMediaSource` 把两个整段文件 source 合起来。
 *
 * 硬解能力：[AndroidCodecs] 问 `MediaCodecList`（`isHardwareAccelerated`），填进 [hardwareCodecs]，
 * 于是 `StreamSelection` 的"自动"在安卓上也成立（能硬解的编码优先）。
 *
 * 线程：`PlayerEngine` 的约定是"任意线程调用都行"，所以所有 ExoPlayer 调用都 post 到主线程。
 * 状态（位置 / 缓冲 / 错误 / 实际解码器名 / 硬解标志）按接口约定更新。
 *
 * @param client 这个平台的 OkHttp 客户端；null = 用一份不带平台规则的基础客户端（测试 / 没组装图的地方）。
 */
@OptIn(UnstableApi::class)
class Media3PlayerEngine(
    private val context: Context,
    client: OkHttpClient?,
) : PlayerEngine {

    override val state = PlayerState()

    /** Media3 是系统自带的（打进 APK），没有"装不上"这一说。 */
    override val unavailable: String? = null

    override val hardwareCodecs: Set<VideoCodec> get() = AndroidCodecs.hardware

    private val main = Handler(Looper.getMainLooper())

    /**
     * 音频焦点 + 拔耳机+ 播放器池复用（E9）：
     * - `setAudioAttributes(..., handleAudioFocus = true)`：放视频时向系统要 `USAGE_MEDIA / CONTENT_TYPE_MOVIE` 的焦点，
     *   别的 App 出声（来电、导航、另一个播放器）就自己让开，回来再继续——以前两个声音会叠在一起放；
     * - `setHandleAudioBecomingNoisy(true)`：拔耳机 / 断蓝牙时 ExoPlayer 自己暂停，不会突然外放；
     * - 实例从 [ExoPlayerPool] 取用，详情切换 / 沉浸划页时复用底层 `ExoPlayer` 与音频轨道，配合放大的
     *   `DefaultLoadControl` 成批拉取缓冲，让移动基带更早进入休眠。
     *
     * **不做**后台播放 / 画中画 / 媒体通知：切后台 `MainActivity.onPause` 立刻暂停。
     */
    private val player: ExoPlayer = ExoPlayerPool.acquire(context)

    private val http = client ?: OkHttpClient()

    @Volatile
    private var released = false

    override fun open(source: MediaSource, startMs: Long, autoPlay: Boolean) {
        val plan = mediaPlanOf(source)
        if (plan == null) {
            main.post { state.error = "没有可播放的流" }
            return
        }
        main.post {
            if (released) return@post
            state.error = null
            state.ended = false
            state.playing = autoPlay
            state.buffering = true
            state.durationMs = source.durationMs ?: 0L
            state.bufferedMs = startMs
            state.jump(startMs)
            state.videoWidth = 0
            state.videoHeight = 0
            state.decoder = null
            state.hardwareDecoding = null
            state.setRunning(false)
            runCatching { buildMediaSource(plan, source.headers) }
                .onSuccess { mediaSource ->
                    player.setMediaSource(mediaSource)
                    player.playWhenReady = autoPlay
                    player.prepare()
                    if (startMs > 0) player.seekTo(startMs)
                    ticker.sync()
                }
                .onFailure { e ->
                    state.error = "打开播放器失败：${e.message ?: e::class.simpleName}"
                    state.buffering = false
                    state.playing = false
                }
        }
    }

    /**
     * 按 [MediaPlan] 建 [MediaSource]。DASH 是 sideload 一份清单对象（不请求清单）；
     * 分片 / 整段文件的请求全走 [OkHttpDataSource]，请求头用平台给的 [headers]。
     */
    private fun buildMediaSource(plan: MediaPlan, headers: Map<String, String>): ExoMediaSource {
        val dataSource = OkHttpDataSource.Factory(http).setDefaultRequestProperties(headers)
        val primary = requireNotNull(plan.primaryVideoUrl) { "没有视频地址" }
        val mediaItem = MediaItem.Builder().setUri(Uri.parse(primary)).build()
        return when (plan.kind) {
            MediaPlan.Kind.DASH -> {
                val manifest = DashManifestParser()
                    .parse(Uri.parse(primary), ByteArrayInputStream(plan.mpd.toByteArray(Charsets.UTF_8)))
                DashMediaSource.Factory(dataSource).createMediaSource(manifest, mediaItem)
            }

            MediaPlan.Kind.MERGED -> MergingMediaSource(
                ProgressiveMediaSource.Factory(dataSource).createMediaSource(MediaItem.fromUri(primary)),
                ProgressiveMediaSource.Factory(dataSource).createMediaSource(MediaItem.fromUri(plan.audioUrls.first())),
            )

            MediaPlan.Kind.HLS -> HlsMediaSource.Factory(dataSource).createMediaSource(mediaItem)

            MediaPlan.Kind.PROGRESSIVE -> ProgressiveMediaSource.Factory(dataSource).createMediaSource(mediaItem)
        }
    }

    override fun play() {
        main.post {
            if (released) return@post
            // 播完再点播放 = 从头重播：Media3 在 STATE_ENDED 上 play() 不一定自己回到开头，
            // 而我们的位置是镜像，不归零的话画面回去了、进度条还停在结尾。这里显式 seek 到 0。
            if (state.ended || player.playbackState == Player.STATE_ENDED) {
                state.ended = false
                state.jump(0)
                state.setRunning(false)
                runCatching { player.seekTo(0) }
            }
            state.playing = true
            state.setRunning(!state.buffering)
            player.play()
            ticker.sync() // 真的开始播了才起定时器
        }
    }

    override fun pause() {
        // 已经在主线程（界面点暂停、Activity.onPause）就当场停，不再排一轮队列：锁屏时声音要立刻断
        onMain {
            if (released) return@onMain
            state.playing = false
            state.setRunning(false)
            player.pause()
            // 停之前把真实位置补读一次（ticker 每 250 ms 才报一次，不补的话进度条停在最多 250 ms 之前），
            // 然后停掉 ticker：暂停时不需要任何定时器
            ticker.refreshNow()
            ticker.sync()
        }
    }

    private fun onMain(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) block() else main.post(block)
    }

    override fun seekTo(positionMs: Long) {
        main.post {
            if (released) return@post
            val target = positionMs.coerceAtLeast(0)
            state.jump(target)
            state.bufferedMs = target.coerceAtLeast(state.bufferedMs)
            player.seekTo(target)
        }
    }

    /**
     * 安卓的音量**就是系统媒体音量**：`STREAM_MUSIC`。滑块 / 手势 / 侧键改的是同一个值，
     * `PlayerState.volume` 只是它的镜像。
     *
     * 还得写一遍
     * 快照状态 → 白出一帧）。现在只在 `VOLUME_CHANGED_ACTION` / `Settings` 变化时读一次，见
     * [volumeReceiver] / [volumeObserver]；监听只在播放器活着时注册（[release] 注销）。
     */
    override val volumeFollowsSystem: Boolean = true

    private val audioManager: AudioManager? =
        runCatching { context.getSystemService(Context.AUDIO_SERVICE) as? AudioManager }.getOrNull()

    /** 只让这个播放器不出声（行内视频静音自动播）：不碰系统音量。 */
    @Volatile
    private var silenced = false

    /** 上一次不是 0 的音量（取消静音时恢复到这里）。 */
    private var lastAudiblePercent = 100

    override fun setVolume(percent: Int) {
        val p = percent.coerceIn(0, 100)
        main.post {
            if (released) return@post
            setSystemMusicVolume(p)
            state.volume = p
            state.muted = p == 0
            if (p > 0) lastAudiblePercent = p
            applyVolume()
        }
    }

    override fun setMuted(muted: Boolean) {
        main.post {
            if (released) return@post
            if (muted) {
                if (state.volume > 0) lastAudiblePercent = state.volume
                setSystemMusicVolume(0)
                state.volume = 0
                state.muted = true
            } else {
                val p = lastAudiblePercent.coerceIn(1, 100)
                setSystemMusicVolume(p)
                state.volume = p
                state.muted = false
            }
            applyVolume()
        }
    }

    override fun setSilenced(silenced: Boolean) {
        main.post {
            if (released) return@post
            this@Media3PlayerEngine.silenced = silenced
            applyVolume()
        }
    }

    /**
     * `player.volume` 固定 1（音量全交给系统，否则两份音量会相乘，怎么调都不对）；只有"只让这个播放器不出声"时才是 0。
     */
    private fun applyVolume() {
        player.volume = if (silenced) 0f else 1f
    }

    /** 百分比 → `STREAM_MUSIC` 的档位（四舍五入到系统自己的音量级数）。 */
    private fun setSystemMusicVolume(percent: Int) {
        val am = audioManager ?: return
        val max = runCatching { am.getStreamMaxVolume(AudioManager.STREAM_MUSIC) }.getOrDefault(0)
        if (max <= 0) return
        val index = (percent.coerceIn(0, 100) / 100f * max).roundToInt().coerceIn(0, max)
        runCatching { am.setStreamVolume(AudioManager.STREAM_MUSIC, index, 0) }
    }

    /** 系统媒体音量现在是百分之多少。 */
    private fun systemMusicPercent(am: AudioManager): Int {
        val max = runCatching { am.getStreamMaxVolume(AudioManager.STREAM_MUSIC) }.getOrDefault(0)
        if (max <= 0) return state.volume
        val index = runCatching { am.getStreamVolume(AudioManager.STREAM_MUSIC) }.getOrDefault(0)
        return (index * 100f / max).roundToInt().coerceIn(0, 100)
    }

    /**
     * 开新播放器时从系统音量起步（别把 App 里记住的旧百分比当成真相）。
     */
    private fun adoptSystemVolume() {
        val am = audioManager ?: return
        val p = systemMusicPercent(am)
        state.volume = p
        state.muted = p == 0
        if (p > 0) lastAudiblePercent = p
    }

    override fun setRate(rate: Float) {
        val r = rate.coerceIn(0.25f, 4f)
        main.post {
            if (released) return@post
            player.playbackParameters = PlaybackParameters(r)
            state.setSpeed(r)
        }
    }

    override fun setLooping(loop: Boolean) {
        main.post {
            if (released) return@post
            player.repeatMode = if (loop) Player.REPEAT_MODE_ONE else Player.REPEAT_MODE_OFF
        }
    }

    @Composable
    override fun Surface(modifier: Modifier) {
        val exo = player
        // 播放中屏幕常亮：跟着 state.playing 走（暂停 / 缓冲 / 播完 / 离开就交回系统超时）。
        // 读 state.playing 放在 AndroidView 外面，值变了才会重画一次 update。
        val keepOn = state.playing && !state.ended
        AndroidView(
            modifier = modifier,
            factory = { ctx ->
                PlayerView(ctx).apply {
                    useController = false
                    setShutterBackgroundColor(Color.BLACK)
                    layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                    player = exo
                }
            },
            update = { view ->
                if (view.player !== exo) view.player = exo
                view.keepScreenOn = keepOn
            },
            onRelease = { view ->
                if (view.player === exo) view.player = null
            },
        )
    }

    override fun release() {
        if (released) return
        released = true
        // 先清掉还没跑的 play / seek 等，再归还播放器池。以前顺序反了：刚 post 的 release 也被 removeCallbacksAndMessages
        // 一起删掉，播放器从没真正释放，关掉的视频在后台接着放（锁屏有声、再开一个就两个声音叠在一起）
        ticker.stop()
        main.removeCallbacksAndMessages(null)
        onMain {
            unregisterVolumeWatchers()
            player.removeListener(listener)
            player.removeAnalyticsListener(analytics)
            ExoPlayerPool.recycle(player)
        }
    }

    // ---------- 状态同步 ----------

    private val listener = object : Player.Listener {
        /**
         * 播放意图变了（`playWhenReady`）：音频焦点被别的 App 抢走 / 拔耳机 / 系统暂停时 ExoPlayer 自己压下来，
         * 界面的播放 / 暂停状态和进度插值要跟着停（否则进度条会继续往前跑）。
         */
        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            state.playing = playWhenReady
            state.setRunning(playWhenReady && !state.buffering && !state.ended)
            ticker.sync() // 被音频焦点 / 拔耳机压下来就停；回来接着播再起
        }

        override fun onPlaybackStateChanged(playbackState: Int) {
            state.buffering = playbackState == Player.STATE_BUFFERING
            when (playbackState) {
                Player.STATE_ENDED -> {
                    state.ended = true
                    state.playing = false
                    if (state.durationMs > 0) state.report(state.durationMs)
                }

                Player.STATE_READY -> if (state.durationMs <= 0) player.duration.takeIf { it > 0 }?.let { state.durationMs = it }
            }
            state.setRunning(state.playing && !state.buffering && !state.ended)
            ticker.sync() // 播完（STATE_ENDED）就停；缓冲完了接着跑
        }

        override fun onVideoSizeChanged(videoSize: VideoSize) {
            if (videoSize.width > 0 && videoSize.height > 0) {
                state.videoWidth = videoSize.width
                state.videoHeight = videoSize.height
            }
        }

        override fun onRenderedFirstFrame() {
            state.hasFrame = true
            state.buffering = false
            state.setRunning(state.playing && !state.ended)
            ticker.sync()
        }

        override fun onPlayerError(error: PlaybackException) {
            state.error = playbackErrorText(error)
            state.playing = false
            state.buffering = false
            state.setRunning(false)
            ticker.sync()
        }
    }

    /**
     * 实际在用的解码器名（`onVideoDecoderInitialized`）+ 硬解标志：Media3 不直接给"硬解吗"，
     * 拿解码器名去 [AndroidCodecs] 里查（和我们查 [hardwareCodecs] 是同一份表）。
     */
    private val analytics = object : AnalyticsListener {
        override fun onVideoDecoderInitialized(
            eventTime: AnalyticsListener.EventTime,
            decoderName: String,
            initializedTimestampMs: Long,
            initializationDurationMs: Long,
        ) {
            state.decoder = decoderName
            state.hardwareDecoding = AndroidCodecs.isHardware(decoderName)
        }
    }

    /** 位置 / 已缓冲的位置：每 [TICK_MS] 在主线程上问一次（`PlayerState` 的插值在两次之间自己算）。 */
    private fun syncFromPlayer() {
        if (released) return
        if (!state.ended) {
            val pos = player.currentPosition
            if (pos >= 0) state.report(pos)
            val buffered = player.bufferedPosition
            state.bufferedMs = maxOf(state.bufferedMs, buffered.coerceAtLeast(state.positionMs))
            val duration = player.duration
            if (duration > 0 && state.durationMs <= 0) state.durationMs = duration
        }
    }

    /**
     * 位置同步的定时器：**只在真的在播时跑**。
     * 暂停 / 播完 / 切后台（`pause()`）/ 释放都会停掉，恢复播放（`sync()`）再起。
     */
    private val ticker = PlaybackTicker(
        everyMs = TICK_MS,
        scheduler = TickScheduler { delayMs, action ->
            // 同一个 Runnable 实例才能被 removeCallbacks 撤掉（lambda 每次 SAM 转换都是新对象）
            val runnable = Runnable { action() }
            main.postDelayed(runnable, delayMs)
            val cancel = { main.removeCallbacks(runnable) }
            cancel
        },
        shouldRun = { !released && state.playing && !state.ended },
        onTick = {
            syncFromPlayer()
            // 兜底：个别 ROM 上广播 / Settings 监听都不一定发得到，播放中顺手读一次系统音量。
            // 这不额外唤醒（本来就在这一拍里），值没变 mirrorSystemVolume 就什么都不写（不会多要一帧）。
            syncSystemVolume()
        },
    )

    // ---------- 系统音量----------

    /**
     * 侧键 / 别处改了系统媒体音量：把镜像更新进 `PlayerState`（以前是 ticker 每 250 ms 轮询一次）。
     * 只认 `STREAM_MUSIC`（铃声 / 通话 / 闹钟不动播放器的音量显示）。
     */
    private val volumeReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context?, intent: Intent?) {
            if (intent?.action != VOLUME_CHANGED_ACTION) return
            // 没带流类型（常量名对不上 / ROM 自己发的）就照样同步一次：宁可多读一次，也别让滑块不跟手
            val stream = intent.getIntExtra(EXTRA_VOLUME_STREAM_TYPE, -1)
            if (stream != -1 && stream != AudioManager.STREAM_MUSIC) return
            syncSystemVolume()
        }
    }

    /**
     * 广播在个别 ROM 上不一定发得到（被裁 / 只发给系统进程），所以再加一道 `Settings` 的监听：
     * 音量的持久化值一变就读一次。两个监听都只在播放器活着时注册。
     */
    private val volumeObserver = object : ContentObserver(main) {
        override fun onChange(selfChange: Boolean) {
            syncSystemVolume()
        }
    }

    private fun syncSystemVolume() {
        val am = audioManager ?: return
        val pct = systemMusicPercent(am)
        if (state.mirrorSystemVolume(pct) && pct > 0) lastAudiblePercent = pct
    }

    private fun registerVolumeWatchers() {
        if (volumeWatchers) return
        volumeWatchers = true
        runCatching {
            // targetSdk 33+ 注册"非系统广播"的接收器必须显式声明导出性；系统广播两种写法都收得到，
            // 这里统一按 NOT_EXPORTED（只收系统发的音量变化，不收别的 App 发的同名广播）
            val filter = IntentFilter(VOLUME_CHANGED_ACTION)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                context.registerReceiver(volumeReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
            } else {
                context.registerReceiver(volumeReceiver, filter)
            }
        }
        runCatching {
            val resolver = context.contentResolver
            // 只精确监听 STREAM_MUSIC 相关的具体键（notifyForDescendants = false），
            // 不再订阅 Settings.System / Settings.Secure 整棵树（避免自动亮度等高频系统写入触发 Binder 唤醒，E8）。
            for (name in MUSIC_VOLUME_SETTING_KEYS) {
                Settings.System.getUriFor(name)?.let { uri ->
                    resolver.registerContentObserver(uri, false, volumeObserver)
                }
            }
        }
    }

    private fun unregisterVolumeWatchers() {
        if (!volumeWatchers) return
        volumeWatchers = false
        runCatching { context.unregisterReceiver(volumeReceiver) }
        runCatching { context.contentResolver.unregisterContentObserver(volumeObserver) }
    }

    /** 音量监听注册过没有（注册一次就够，[release] 时注销）。 */
    private var volumeWatchers = false

    init {
        player.addListener(listener)
        player.addAnalyticsListener(analytics)
        adoptSystemVolume()
        onMain {
            if (!released) applyVolume()
        }
        registerVolumeWatchers()
        // 打开就能播（autoPlay）的话 [open] 会 sync；这里是兜底：还没有流也先把定时器留在停着的状态
        ticker.sync()
    }

    private companion object {
        const val TICK_MS = 250L

        /**
         * 音量变化的广播 / 额外字段。`AudioManager` 里这两个是 `@hide`（SDK stub 里取不到），
         * 值就是 AOSP 里那两串（老代码一直这么用）。
         */
        const val VOLUME_CHANGED_ACTION = "android.media.VOLUME_CHANGED_ACTION"
        const val EXTRA_VOLUME_STREAM_TYPE = "android.media.EXTRA_VOLUME_STREAM_TYPE"

        /** AudioService 落盘媒体音量的 Settings.System 键（按输出设备后缀区分）。 */
        val MUSIC_VOLUME_SETTING_KEYS = arrayOf(
            "volume_music",
            "volume_music_speaker",
            "volume_music_headset",
            "volume_music_headphone",
            "volume_music_bt_a2dp",
            "volume_music_usb_headset",
        )
    }
}

/**
 * 进程级 `ExoPlayer` 复用池（E9）：保留最多 [MAX_POOL_SIZE] 个空闲实例（当前 1 个 + 预热 1 个），
 * 划沉浸模式或频繁进出视频详情时免去反复初始化 `ExoPlayer` 与底层音频会话的开销。
 */
@OptIn(UnstableApi::class)
internal object ExoPlayerPool {
    private const val MAX_POOL_SIZE = 2
    private const val MIN_BUFFER_MS = 30_000
    private const val MAX_BUFFER_MS = 120_000
    private const val BUFFER_FOR_PLAYBACK_MS = 1_500
    private const val BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS = 3_000

    private val main = Handler(Looper.getMainLooper())
    private val pool = ArrayDeque<ExoPlayer>(MAX_POOL_SIZE)

    fun acquire(context: Context): ExoPlayer {
        synchronized(this) {
            if (pool.isNotEmpty()) {
                return pool.removeLast()
            }
        }
        return buildPlayer(context.applicationContext)
    }

    /** 必须在主线程调用（由 [Media3PlayerEngine.release] 保证）。 */
    fun recycle(player: ExoPlayer) {
        val cleaned = runCatching {
            player.stop()
            player.clearMediaItems()
            player.clearVideoSurface()
            player.playWhenReady = false
            player.playbackParameters = PlaybackParameters.DEFAULT
            player.repeatMode = Player.REPEAT_MODE_OFF
            player.volume = 1f
            true
        }.getOrDefault(false)
        if (!cleaned) {
            runCatching { player.release() }
            return
        }
        val keep = synchronized(this) {
            if (pool.size < MAX_POOL_SIZE) {
                pool.addLast(player)
                true
            } else {
                false
            }
        }
        if (!keep) {
            runCatching { player.release() }
        }
    }

    fun evictAll() {
        val drained = synchronized(this) {
            pool.toList().also { pool.clear() }
        }
        if (drained.isEmpty()) return
        val releaseAll = Runnable {
            for (p in drained) runCatching { p.release() }
        }
        if (Looper.myLooper() == Looper.getMainLooper()) {
            releaseAll.run()
        } else {
            main.post(releaseAll)
        }
    }

    private fun buildPlayer(appContext: Context): ExoPlayer {
        val loadControl = DefaultLoadControl.Builder()
            .setBufferDurationsMs(
                MIN_BUFFER_MS,
                MAX_BUFFER_MS,
                BUFFER_FOR_PLAYBACK_MS,
                BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS,
            )
            .setPrioritizeTimeOverSizeThresholds(true)
            .build()
        return ExoPlayer.Builder(appContext)
            .setLooper(Looper.getMainLooper())
            .setLoadControl(loadControl)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(C.USAGE_MEDIA)
                    .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            .setHandleAudioBecomingNoisy(true)
            .build()
    }
}

/** 播放失败的说明：常见错误给一句能看懂的，其余带 Media3 的错误码名。 */
internal fun playbackErrorText(error: PlaybackException): String {
    val what = when (error.errorCode) {
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT,
        -> "连不上视频服务器"

        PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
        PlaybackException.ERROR_CODE_IO_FILE_NOT_FOUND,
        -> "视频地址取不到（可能被防盗链挡了）"

        PlaybackException.ERROR_CODE_IO_CLEARTEXT_NOT_PERMITTED -> "这个地址不允许明文 HTTP"

        PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
        PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
        PlaybackException.ERROR_CODE_DECODING_FAILED,
        -> "解码器解不了这个视频"

        PlaybackException.ERROR_CODE_PARSING_CONTAINER_MALFORMED,
        PlaybackException.ERROR_CODE_PARSING_MANIFEST_MALFORMED,
        -> "视频流的结构认不出来"

        else -> "这个视频暂时播不了"
    }
    return "播放失败：$what（${error.errorCodeName}）"
}

/**
 * 本机能**硬解**哪些编码：问 `MediaCodecList`。
 *
 * `MediaCodecInfo.isHardwareAccelerated()` 是 API 29 起；更早（minSdk 26）用名字启发式
 * （`OMX.google.` / `c2.android.` 是软解，其余当硬解）。解码器名字 → 是不是硬解由 [isHardware] 回答
 * （播起来之后 Media3 报的实际解码器名也走这里）。
 */
internal object AndroidCodecs {
    /** 设备上带硬解的视频编码（只查一次；没有硬解的编码不出现）。 */
    val hardware: Set<VideoCodec> by lazy { queryHardware() }

    /** 解码器名字 → 是否硬解。 */
    private val decoderHardwareByName: Map<String, Boolean> by lazy {
        runCatching {
            buildMap {
                for (info in MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos) {
                    if (!info.isEncoder) {
                        put(info.name, hardwareOf(info))
                    }
                }
            }
        }.getOrDefault(emptyMap())
    }

    /** 这个解码器名字是不是硬解；查不到这个名字返回 null（不知道）。 */
    fun isHardware(decoderName: String): Boolean? = decoderHardwareByName[decoderName]

    private fun queryHardware(): Set<VideoCodec> = runCatching {
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .asSequence()
            .filter { !it.isEncoder && hardwareOf(it) }
            .flatMap { it.supportedTypes.asSequence() }
            .mapNotNull(::videoCodecOfMime)
            .toSet()
    }.getOrDefault(emptySet())

    private fun hardwareOf(info: MediaCodecInfo): Boolean = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        info.isHardwareAccelerated
    } else {
        // API 29 之前没有这个字段：按名字认软解（Google 的软件解码器 / Android 的 c2.android.*）
        val name = info.name.lowercase()
        !(name.startsWith("omx.google.") || name.startsWith("c2.android."))
    }
}
