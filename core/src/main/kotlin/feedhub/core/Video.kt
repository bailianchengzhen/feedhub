package feedhub.core

import kotlinx.serialization.Serializable

/**
 * 能原生播放视频的平台额外实现：播放地址（和弹幕）。五个平台都实现。
 * `id` 和 [NativeDetailSource.detail] 的一样，`partId` 是 [VideoPart.id]。
 *
 * 正文里夹着的视频（[Block.Video]）也走这里：[Block.Video.id] / [Block.Video.partId] 原样交给 [play]，
 * 所以 `partId` 的写法由各 Source 自己约定（不止 [VideoPart.id]，例如小黑盒的 `seg:1` 表示正文里第 1 个视频），只要 [play] 认得就行。
 *
 * **地址会过期**（小黑盒的 auth、知乎的 auth_key、贴吧的 tbau 都是临时签名）：[play] 每次都向平台现取，不要缓存或复用详情里的旧地址；
 * 前端也只在要播放时才调它。
 */
interface VideoSource : NativeDetailSource {
    /**
     * 播放地址。[quality] 是 [Quality.id]，null 表示让平台按登录态给能播的最高清晰度。
     * 返回的流里通常同时有几种编码，选哪个由播放器按设备的硬件解码能力决定（见 [VideoCodec]）。
     * 不是 DASH 的平台（小黑盒、知乎、贴吧、酷安）把 mp4 / m3u8 放进 [PlayInfo.progressive]。
     */
    suspend fun play(id: String, partId: String, quality: Int? = null): PlayInfo

    /**
     * 弹幕，一段一段取（B站每段 6 分钟，[segment] 从 1 开始）。超出视频长度的段返回空列表。
     * 顺序不保证，前端自己按时间排。没有弹幕的平台不用覆盖：默认空列表，并且 [VideoInfo.danmakuSegmentMs] 留 null
     * （前端据此隐藏弹幕按钮、不去取弹幕）。
     */
    suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> = emptyList()
}

/** 视频详情里的视频部分（[FeedDetail.video]）。 */
@Serializable
data class VideoInfo(
    val parts: List<VideoPart>,
    /** 弹幕每段多长（毫秒），前端按播放进度算该取第几段。 */
    val danmakuSegmentMs: Long? = null,
)

@Serializable
data class VideoPart(
    /** 播放和弹幕用的 id（B站的 cid）。 */
    val id: String,
    val title: String? = null,
    val durationSec: Int? = null,
    val width: Int? = null,
    val height: Int? = null,
)

@Serializable
data class PlayInfo(
    val durationMs: Long? = null,
    /** 这次返回的实际清晰度。 */
    val quality: Int? = null,
    /** 平台列出的全部清晰度（包括要登录 / 大会员、这次没给流的）。 */
    val qualities: List<Quality> = emptyList(),
    /** DASH 视频轨（无声音），同一清晰度可能有几种编码。 */
    val video: List<VideoStream> = emptyList(),
    /** DASH 音频轨。 */
    val audio: List<AudioStream> = emptyList(),
    /** 音画一体的整段文件（老视频、非 DASH 的平台）；有 DASH 时一般为空。 */
    val progressive: List<ProgressiveStream> = emptyList(),
    /** 拉流时必须带的请求头（B站要 Referer 和 UA，否则 403）。 */
    val headers: Map<String, String> = emptyMap(),
)

@Serializable
data class Quality(
    val id: Int,
    /** "1080P 高清"之类。 */
    val label: String,
    /** 这次是否给了这个清晰度的流（没登录 / 不是大会员时高清晰度不给）。 */
    val available: Boolean = true,
)

/** 视频编码。播放器选流时优先选设备能硬件解码的那种（安卓上查 `MediaCodecInfo.isHardwareAccelerated`）。 */
@Serializable
enum class VideoCodec { AVC, HEVC, AV1, OTHER }

@Serializable
data class VideoStream(
    val quality: Int,
    val codec: VideoCodec,
    /** 原样的 codecs 字符串，如 "avc1.640032"、"hev1.1.6.L150.90"，给 MediaCodec 判断 profile 用。 */
    val codecs: String? = null,
    val url: String,
    val backupUrls: List<String> = emptyList(),
    val bandwidth: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    val frameRate: String? = null,
    /** DASH 的 SegmentBase（B站给的 initialization / indexRange），ExoPlayer 用单文件 DASH 播放时要。 */
    val segmentBase: SegmentBase? = null,
)

@Serializable
data class AudioStream(
    val id: Int,
    val codecs: String? = null,
    val url: String,
    val backupUrls: List<String> = emptyList(),
    val bandwidth: Long? = null,
    val segmentBase: SegmentBase? = null,
)

@Serializable
data class SegmentBase(val initialization: String, val indexRange: String)

/**
 * 音画一体的整段文件。[url] 一般是 mp4；HLS（`.m3u8`）也放这里，GStreamer 的 uridecodebin3 会自己挑 hlsdemux。
 * 同一清晰度有几种编码（知乎的 AVC / HEVC 两套）时各给一条并填 [codec]，播放器按硬解能力挑。
 */
@Serializable
data class ProgressiveStream(
    val quality: Int? = null,
    val url: String,
    val backupUrls: List<String> = emptyList(),
    val sizeBytes: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    /** 视频编码；不知道就 null（按 AVC 对待，最通用）。 */
    val codec: VideoCodec? = null,
    /** 码率（bit/s），同一清晰度有几条时用来挑；不知道就 null。 */
    val bandwidth: Long? = null,
)

/**
 * 没有 DASH 清晰度档位的平台，按画面尺寸给 [Quality]：id 沿用 B站的数字（80 = 1080P、64 = 720P、32 = 480P、16 = 360P、120 = 4K），
 * 这样设置里的"默认清晰度"对所有平台一样有效。按短边算，竖屏视频（720×960）也是 720P；留了一点余量（848×476 算 480P）。
 */
fun qualityOfSize(width: Int?, height: Int?): Quality? {
    val short = listOfNotNull(width, height).filter { it > 0 }.minOrNull() ?: return null
    return when {
        short >= 2000 -> Quality(120, "4K")
        short >= 1000 -> Quality(80, "1080P")
        short >= 700 -> Quality(64, "720P")
        short >= 460 -> Quality(32, "480P")
        else -> Quality(16, "360P")
    }
}

@Serializable
data class Danmaku(
    val id: String,
    /** 出现时间，相对视频开头的毫秒数。 */
    val timeMs: Long,
    val mode: DanmakuMode,
    val text: String,
    /** 0xRRGGBB。 */
    val color: Int = 0xFFFFFF,
    /** 平台原值，B站常见 25（标准）、18（小）。 */
    val fontSize: Int = 25,
    /** 平台给的权重 / 屏蔽等级（B站 0~10，越高越该显示），前端做弹幕密度时用。 */
    val weight: Int? = null,
)

@Serializable
enum class DanmakuMode {
    SCROLL,
    TOP,
    BOTTOM,

    /** 逆向、高级、代码弹幕等，前端可以直接不显示。 */
    OTHER,
}
