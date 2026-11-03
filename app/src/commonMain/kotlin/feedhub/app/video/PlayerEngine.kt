package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import feedhub.core.Platform
import feedhub.core.ProxyMode
import feedhub.core.SegmentBase
import feedhub.core.VideoCodec
import okhttp3.OkHttpClient

/**
 * 一次播放用的流。B站是 DASH：视频轨和音频轨分开，各自有主地址和备用地址（按顺序试）。
 * 音画一体的文件只给 [videoUrls]、[audioUrls] 留空。
 */
class MediaSource(
    val videoUrls: List<String>,
    val audioUrls: List<String> = emptyList(),
    /** 拉流必带的请求头（B站是 Referer 和 User-Agent）。 */
    val headers: Map<String, String> = emptyMap(),
    /** 视频轨 + 音频轨的平均码率（bit/s），用来估算"已缓冲到哪"，不知道就 null。 */
    val bandwidthBps: Long? = null,
    /** 编码，界面上显示用。 */
    val codec: VideoCodec? = null,
    /** 已知的总时长（毫秒），播放器报出真实时长之前先用它（进度条、续播不用等）。 */
    val durationMs: Long? = null,
    /**
     * DASH 单文件流的索引信息（B站的 initialization / indexRange）。有它，GStreamer 引擎才能把两条轨拼成一份 MPD 交给 dashdemux
     * （分片的 fMP4 靠 sidx 才能快速跳转）；没有就按 [videoUrls] 当音画一体的文件播。
     */
    val dash: DashInfo? = null,
    /**
     * 这个视频所在平台的代理（见 [feedhub.core.ProxyMode]）。流本身也是这个平台的内容，
     * 所以"按平台指定代理"时视频流也要跟着走。播放器不走 OkHttp，由各引擎自己设：
     * GStreamer 设 `souphttpsrc.proxy`，VLC 加 `:http-proxy=` / `:socks=`（翻译见 `video/PlayerProxy.kt`）。
     *
     * **只对不经中转的那一路有效**（音画一体的 mp4 直接给 `souphttpsrc` / libvlc）：DASH / HLS 和需要
     * 中转的整段 mp4 都走 [relayFactory]，代理由中转在 Rust 里按平台取。
     */
    val proxy: ProxyMode = ProxyMode.System,
    /**
     * 分片中转的工厂：给一个平台，回一个"登记 / 路由都在 Rust 里"的本机中转服务句柄
     * （[RelayHandle]，桌面实现是 `VideoRelay`）。DASH（含 `.mpd` 直链）和 HLS 的清单 / 分片都经它
     * 指回本机回环，播放器只连本机。
     *
     * 为什么是**工厂**而不是一个客户端：中转要按平台算代理（`ProxyMode` → 那一档），而"按平台指定代理"
     * 这件事在 App 里归 `AppGraph`（`ctx.proxy` + `ProxyTarget.of`）。工厂让播放器这一层既不知道 OkHttp、
     * 也不知道代理表。
     *
     * null = 不中转，地址原样交给播放器：音画一体的 mp4 本来就是这样（由 `souphttpsrc` 直接取、设 [proxy]），
     * 但 DASH / HLS 的分片这样会**直连 CDN、不走代理**——只在开发工具 / 测试里可接受。
     */
    val relayFactory: RelayFactory? = null,
)

/**
 * 造一个本机中转（[MediaSource.relayFactory]）。
 *
 * 返回的那一份只属于**这一次播放**：引擎在拆管线 / 换地址时关掉它（`close()` 会把在飞的上游请求一起取消）。
 */
fun interface RelayFactory {
    fun open(): RelayHandle
}

/**
 * 一个本机中转的最低接口（**生产实现是 Rust 里那个服务**，见 `feedhub.player.RustRelayServer`）。
 *
 * 播放器只用这五个动作：登记一个远端地址 / 一份远端清单 / 一个模板、发一份自己拼的清单、关掉。
 * `Range`、代理、fail-closed、清单改写、边收边发全在中转那边（Rust），这一层不碰。
 */
interface RelayHandle : AutoCloseable {
    /** 播放器看到的基地址（`http://127.0.0.1:<port>`）。 */
    val base: String

    /** 登记一个完整的远端地址（单个文件、一个分片、一把密钥），返回本机地址。 */
    fun resource(remoteUrl: String): String

    /** 登记一个远端播放列表 / 清单（m3u8，或远端 `.mpd`）：播放器取的时候现取现改写。 */
    fun playlist(remoteUrl: String): String

    /** 登记一个远端分片模板（带 `$Number$` / `$Time$` 这类占位符），返回本机模板地址。 */
    fun template(remoteTemplate: String): String

    /** 放一份我们自己拼好、已经改写过的清单，返回本机地址。 */
    fun publishMpd(mpd: String, baseUrl: String?): String

    // ---------- 检查工具用（默认什么都不做；生产实现是 Rust 那个服务） ----------

    /** 已经向上游取过多少次。 */
    val upstreamRequests: Int get() = 0

    /** 向上游取过的地址的主机名。 */
    val upstreamHostNames: Set<String> get() = emptySet()

    /** 曾经同时有几个上游请求在飞。 */
    val maxInFlight: Int get() = 0

    /** 上游请求结束时回调一次（在中转的处理线程里调，别做重活）。 */
    var onUpstreamDone: ((RelayUpstreamStat) -> Unit)?
        get() = null
        set(_) = Unit
}

/** 一次上游请求的记账（检查工具 / 吞吐测量台用）。 */
class RelayUpstreamStat(val bytes: Long, val ms: Long, val range: String?, val ok: Boolean)

/**
 * 起一个本机中转（[MediaSource.relayFactory] 的实现）。
 *
 * 桌面：Rust 里那个真服务（`RustRelayServer`，回环 HTTP + 边收边转发）。安卓：**还没有**（Media3 走
 * `ResolvingDataSource`，分片请求本来就在自己的 OkHttp 里过代理，不需要这一层），所以安卓那一份直接抛
 * ——调用方只在桌面路径上真正用 [MediaSource.relayFactory]，见 `Media3PlayerEngine`。
 */
expect fun createRelay(
    platform: Platform,
    proxy: feedhub.rust.ProxyTarget,
    headers: Map<String, String> = emptyMap(),
    readTimeoutMs: Int = 0,
): RelayHandle

/** DASH 里一条轨的描述，拼 MPD 用。 */
class DashTrack(
    val segmentBase: SegmentBase,
    val codecs: String? = null,
    val bandwidth: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    val frameRate: String? = null,
)

class DashInfo(val video: DashTrack, val audio: DashTrack? = null)

/**
 * 播放器此刻的状态，界面读它。各字段都是 Compose 的状态，可以从任意线程写。
 * [positionMs] 只有几百毫秒更新一次，要平滑的位置（进度条、弹幕）用 [nowMs]。
 *
 * **另一个线程读位置**用 [renderNowMs]：它只读下面那几个 `@Volatile` 的
 * 普通字段，**一个 Compose 状态都不读**（[buffering] / [durationMs] 在 [bufferingMirror] / [durationMirror] 留了镜像，
 * 每次赋值一起写）。锚点打包成一个对象 [Anchor] 一次写完，画线程不会读到"新的毫秒 + 旧的纳秒"这种半截值
 * （那会让某一帧的弹幕位置往前跳 250 ms 再回来）。
 */
class PlayerState {
    /** 想播放着（没暂停）；缓冲时也是 true。 */
    var playing by mutableStateOf(false)
    /** 正在缓冲 / 打开中，没有画面可看。 */
    var buffering: Boolean
        get() = bufferingState.value
        set(value) {
            bufferingState.value = value
            bufferingMirror = value
        }
    var positionMs by mutableLongStateOf(0L)
    var durationMs: Long
        get() = durationState.value
        set(value) {
            durationState.value = value
            durationMirror = value
        }
    /** 已缓冲到的位置（估算值），不会小于 [positionMs]。 */
    var bufferedMs by mutableLongStateOf(0L)
    var ended by mutableStateOf(false)
    /** 播放出错的说明；null 表示没出错。 */
    var error: String? by mutableStateOf(null)
    var videoWidth by mutableIntStateOf(0)
    var videoHeight by mutableIntStateOf(0)
    /** 音量 0~100。 */
    var volume by mutableIntStateOf(100)
    var muted by mutableStateOf(false)
    /** 实际在用的视频解码器（元素名，如 vah264dec、avdec_h264）；还不知道是 null。 */
    var decoder: String? by mutableStateOf(null)
    /** 视频是硬件解码吗：true = 硬解，false = 软解，null = 还不知道。 */
    var hardwareDecoding: Boolean? by mutableStateOf(null)

    /** 画面上已经出过第一帧（在此之前画面是空的 / 黑的）。行内嵌入据此把封面盖在画面上，第一帧出来才露出视频；之后换清晰度重新打开不会再变回 false。 */
    var hasFrame by mutableStateOf(false)

    /** 画面上来过几帧（窗口看得见时引擎每交一帧 +1）。进度条据此和视频同一帧重画（[awaitProgressFrame]）。 */
    var videoFrames by mutableLongStateOf(0L)

    /** 播放速率（1.0 = 正常）。换清晰度 / 分 P / 拖动都不重置；引擎用 [setSpeed] 改，位置插值要跟着变。 */
    var speed by mutableFloatStateOf(1f)
        private set

    private val bufferingState = mutableStateOf(false)
    private val durationState = mutableLongStateOf(0L)

    @Volatile private var speedFactor = 1f
    @Volatile private var anchor = Anchor(0L, 0L)
    @Volatile private var running = false
    @Volatile private var lastReportedMs = 0L

    /** [buffering] 的镜像（给画线程读，见 [renderNowMs]）。 */
    @Volatile private var bufferingMirror = false

    /** [durationMs] 的镜像（给画线程读，见 [renderNowMs]）。 */
    @Volatile private var durationMirror = 0L

    /** 位置的锚点：**一个对象一次写完**，别的线程要么读到旧的、要么读到新的，不会读到半截。 */
    private class Anchor(val ms: Long, val nanos: Long)

    /** 播放器报告了新位置：校准插值用的锚点。位置和插值差不多时不动它，免得进度条 / 弹幕来回抖。 */
    fun report(positionMs: Long, nanoTime: Long = System.nanoTime()) {
        val predicted = nowMs(nanoTime)
        if (!running || kotlin.math.abs(predicted - positionMs) > DRIFT_MS) {
            anchor = Anchor(positionMs, nanoTime)
        }
        lastReportedMs = positionMs
        this.positionMs = positionMs
    }

    /** 播放速率变了：先把锚点定在当前位置，之后按新速率往前推（引擎在真的改了速率时调）。 */
    fun setSpeed(rate: Float, nanoTime: Long = System.nanoTime()) {
        anchor = Anchor(nowMs(nanoTime), nanoTime)
        speedFactor = rate
        speed = rate
    }

    /** 播放状态变了（开始 / 暂停）：把锚点定在当前位置。 */
    fun setRunning(run: Boolean, nanoTime: Long = System.nanoTime()) {
        anchor = Anchor(nowMs(nanoTime), nanoTime)
        running = run
    }

    /**
     * 系统媒体音量变了：把 [volume] / [muted] 的镜像更新掉。
     *
     * **没变就一个字节都不写**：这些是 Compose 状态，写一次就要一帧；以前 ticker 每 250 ms 读一次系统音量、
     * 值没变也照样写一遍，后台醒着的时候就是白出的帧。返回 true = 真的变了（调用方据此更新"上一次不是 0 的音量"）。
     */
    fun mirrorSystemVolume(percent: Int): Boolean {
        val p = percent.coerceIn(0, 100)
        val muted = p == 0
        if (p == volume && muted == this.muted) return false
        volume = p
        this.muted = muted
        return true
    }

    /** 跳转（或换了视频）：位置立刻变成 [positionMs]。 */
    fun jump(positionMs: Long, nanoTime: Long = System.nanoTime()) {
        anchor = Anchor(positionMs, nanoTime)
        lastReportedMs = positionMs
        this.positionMs = positionMs
    }

    /**
     * 此刻的播放位置（毫秒）：播放中在上次报告的基础上按时间往前推，暂停 / 缓冲时不动。不是 Compose 状态，在每帧里读。
     * 播放器正常每 250ms 左右报一次位置；超过 [MAX_AHEAD_MS] 没有新报告就不再往前推（视频卡住了，弹幕和进度条也别跑远）。
     */
    fun nowMs(nanoTime: Long = System.nanoTime()): Long {
        val a = anchor
        if (!running || buffering) return clampToDuration(a.ms, durationMs)
        return clampToDuration(interpolate(a, nanoTime, speedFactor), durationMs)
    }

    /**
     * 和 [nowMs] 同一个位置，**给安卓的弹幕画线程用**：只读 `@Volatile` 字段（[buffering] / [durationMs] 读的是镜像），
     * 一个 Compose 状态都不碰——从别的线程读 Compose 状态要走快照，一帧一次不合适，也不该依赖那个行为。
     */
    fun renderNowMs(nanoTime: Long): Long {
        val a = anchor
        if (!running || bufferingMirror) return clampToDuration(a.ms, durationMirror)
        return clampToDuration(interpolate(a, nanoTime, speedFactor), durationMirror)
    }

    private fun interpolate(a: Anchor, nanoTime: Long, speed: Float): Long {
        val base = a.ms
        val predicted = base + ((nanoTime - a.nanos) / 1_000_000 * speed).toLong()
        // 播放器正常每 250ms 左右报一次位置；超过 MAX_AHEAD_MS 没有新报告就不再往前推（视频卡住了，弹幕和进度条也别跑远）
        return minOf(predicted, lastReportedMs + (MAX_AHEAD_MS * speed).toLong())
    }

    private fun clampToDuration(pos: Long, duration: Long): Long = if (duration > 0) pos.coerceIn(0, duration) else pos.coerceAtLeast(0)

    private companion object {
        const val DRIFT_MS = 350L
        const val MAX_AHEAD_MS = 1200L
    }
}

/**
 * 播放器：commonMain 里只认这个接口。桌面首选 GStreamer（VA-API 硬解），没有就退回 VLCJ（软解）——都是把帧画进 Compose 的图片里，
 * 这样弹幕和控件才能盖在视频上；安卓以后用 Media3 ExoPlayer（`Surface` 里放 PlayerView / SurfaceView）。
 * 所有方法在任意线程调用都行，实现自己切到播放线程。
 */
interface PlayerEngine {
    val state: PlayerState

    /** 非 null：这台机器没法播放（比如 GStreamer 和 libvlc 都没装），界面把这句话显示给用户。 */
    val unavailable: String?

    /** 这台机器能硬件解码的编码（查不到 / 不支持硬解就空）；选流的"自动"据此在能硬解的编码里挑，见 [StreamSelection.autoOrder]。 */
    val hardwareCodecs: Set<VideoCodec> get() = emptySet()

    /**
     * 本机解码能力："能不能真硬解" + "实测多大负载还跟得上"，选流的"自动"按它排序，见 [DecodeCapability]。
     * 默认只报 [hardwareCodecs]；桌面 GStreamer 引擎会把播放过程中量到的结果并进来。
     */
    val decodeCapability: DecodeCapability get() = DecodeCapability.ofHardware(hardwareCodecs)

    /** 开始播放 [source]，从 [startMs] 起；[autoPlay] 为 false 时停在第一帧。会替换掉正在播的。 */
    fun open(source: MediaSource, startMs: Long = 0L, autoPlay: Boolean = true)

    fun play()
    fun pause()
    fun seekTo(positionMs: Long)
    fun setVolume(percent: Int)
    fun setMuted(muted: Boolean)

    /**
     * 音量是不是**系统媒体音量本身**：安卓 = `STREAM_MUSIC`，[setVolume] / [setMuted] 改的就是系统音量，
     * 播放器自己的 `player.volume` 固定 1（侧键能调、App 里滑块和手势调的是同一个东西）。桌面 = false（App 自己那份音量）。
     *
     * 为 true 时界面不应该在新建播放器时把"App 记住的音量"推下去（那会把用户刚按侧键调好的值盖回去），
     * 见 [PlaybackSession.init]。
     */
    val volumeFollowsSystem: Boolean get() = false

    /**
     * **只让这个播放器不出声**（不动系统音量、不动 App 共用的音量）：正文里的视频"滚到可见时静音自动播放"用它，
     * 放大看的时候再 [setSilenced]`(false)` 把声音还回来。默认就等于 [setMuted]（桌面上两者是一回事）。
     */
    fun setSilenced(silenced: Boolean) = setMuted(silenced)

    /**
     * 设置播放速率（0.25~4，1.0 = 正常；变速不变调）。换视频 / 清晰度 / 分 P / 拖动之后保持。
     * 引擎只管"设成这个值"，不认档位：菜单的档位是 [PlaybackSpeed]，长按加速那种临时倍速由界面层（[PlaybackSession.holdSpeed] / [PlaybackSession.releaseSpeed]）
     * 记住原值、松手再设回来。当前速率在 [PlayerState.speed]。
     */
    fun setRate(rate: Float)

    /**
     * 播完是否从头循环（正文里的行内视频像 GIF 一样）。默认不循环。
     * 循环时引擎在播放结束的那一刻自己跳回开头接着播（不重建播放管线），[PlayerState.ended] 不会变成 true；关掉循环之后播完就照常停在结尾。
     */
    fun setLooping(loop: Boolean) {}

    /** 画面。尺寸由 [modifier] 定，画面按比例居中缩放，空出来的地方是黑的。 */
    @Composable
    fun Surface(modifier: Modifier)

    /** 停止并释放（离开详情页时调）。之后不能再用。 */
    fun release()
}

/**
 * 建一个这个平台的播放器；平台不支持时返回 [unavailable] 非 null 的占位实现，不抛异常。
 *
 * [client] 是**安卓**那个平台客户端的 OkHttp（`graph.ctx.client(platform).build()`：平台代理、
 * "指定代理没填全就失败"的 fail-closed、Referer / UA 都在它里面）——Media3 的 `ResolvingDataSource`
 * 用它当数据源，清单和分片都经它取，代理由此自然跟着平台走。
 *
 * **桌面已经不用它了**：GStreamer / VLC 的清单和分片一律经本机中转
 * （[MediaSource.relayFactory] → Rust 里那个回环服务），代理由中转在 Rust 里按平台取；桌面的
 * `actual` 直接忽略这个参数。
 */
expect fun createPlayerEngine(client: OkHttpClient?): PlayerEngine

/** 没法播放时的占位：什么都不做。 */
class UnavailablePlayer(override val unavailable: String) : PlayerEngine {
    override val state = PlayerState()
    override fun open(source: MediaSource, startMs: Long, autoPlay: Boolean) {}
    override fun play() {}
    override fun pause() {}
    override fun seekTo(positionMs: Long) {}
    override fun setVolume(percent: Int) {}
    override fun setMuted(muted: Boolean) {}
    override fun setRate(rate: Float) {}

    @Composable
    override fun Surface(modifier: Modifier) {}

    override fun release() {}
}
