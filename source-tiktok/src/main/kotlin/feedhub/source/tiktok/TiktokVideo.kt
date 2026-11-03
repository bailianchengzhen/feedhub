package feedhub.source.tiktok

import feedhub.core.ApiException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.VideoCodec
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.arr
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.qualityOfSize
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * TikTok 的播放地址，纯函数（`TiktokVideoTest` 喂 fixtures）。
 *
 * ## 地址从哪来
 *
 * - `video.playAddr`：**字符串**（不是对象），同一源（`v16-webapp-prime.tiktok.com` 之类）的 mp4，
 *   catbus 的 `itemMedia()` 就取它（`n.url(v.video.playAddr)`），注释写"同源 h264"。
 * - `video.downloadAddr`：带水印的下载地址，只在前面的都没有时兜底。
 * - `video.bitrateInfo[]`：每档清晰度一项，字段名是**大驼峰**：`Bitrate`、`CodecType`（`h264` / `bytevc1`，
 *   catbus 只在注释里说 `playAddr` 是 h264，没有见到 `bytevc1`）、`GearName`、`QualityType`、
 *   `PlayAddr{UrlList, Width, Height, DataSize, FileHash, Uri}`。每个 `UrlList` 里通常有多条 CDN 地址。
 * - `video.cover` / `originCover` / `dynamicCover`、`video.duration`（**毫秒**）、`video.width/height`。
 *
 * 这些地址**都带时效签名**（`VExpiration` / `VSignature` 或 `l` / `ft` 那类，3MH 记的是约 40~48 小时），
 * 所以 [TiktokSource.play] 每次重新取一次视频页，不复用详情里那份（和抖音 / 小黑盒 / 小红书一样）。
 *
 * ## 编码
 *
 * 每档清晰度按 `CodecType` 分 AVC / HEVC / AV1，全部列进 [PlayInfo.progressive]，
 * 由前端按硬件解码能力挑。TikTok web 给的是**音画一体的 mp4**（`video_encoding=dash` 只是 query 里的
 * 一个开关，返回的仍是整段 mp4），所以走 progressive、没有 DASH 轨、也没有弹幕。
 */
internal object TiktokVideo {
    private val P = TIKTOK

    /** [VideoPart.id]，整条作品就是一个视频（和抖音 / 小黑盒 / 贴吧 / 酷安的 `main` 一致）。 */
    const val MAIN = "main"

    /** 一条可播的流。 */
    class Stream(
        val quality: Int?,
        val codec: VideoCodec,
        val url: String,
        val backupUrls: List<String>,
        val width: Int?,
        val height: Int?,
        val bandwidth: Long?,
        val sizeBytes: Long?,
    )

    /**
     * `video.duration` **是秒**，
     * 而 1044 那条的 `video.size` = 43029023 字节、`bitrateInfo[].Bitrate` = 329601，
     * `43029023 × 8 / 329601 ≈ 1044` 秒——正好对上（catbus 的 `n.seconds()` 也是原样取用、不除 1000）。
     */
    fun durationSec(video: JsonObject): Int? = video.long("duration")?.takeIf { it > 0 }?.toInt()

    fun durationMs(video: JsonObject): Long? = durationSec(video)?.let { it * 1000L }

    /** 作品的像素尺寸：`video.width/height` → 最大的一档 bitrateInfo → `playAddr` 那条没有尺寸信息。 */
    fun size(video: JsonObject): Pair<Int?, Int?> {
        val w = video.int("width")
        val h = video.int("height")
        if (w != null && h != null && w > 0 && h > 0) return w to h
        val best = streams(video).maxByOrNull { (it.width?.toLong() ?: 0L) * (it.height ?: 0) }
        return best?.width to best?.height
    }

    /**
     * `video` 对象里所有能播的流，同一个地址只留一条；按清晰度、再按码率从高到低排。
     *
     * `bitrateInfo` 里的每条本身就是一档（带自己的 `PlayAddr.Width/Height`），所以清晰度按**每条流
     * 自己的短边**算（和抖音一样：高分辨率的 H.265 码率可能比低分辨率的 H.264 还低，只取第一条会漏档）。
     */
    fun streams(video: JsonObject): List<Stream> {
        val out = LinkedHashMap<String, Stream>()
        fun add(urls: List<String>, codec: VideoCodec, bandwidth: Long?, sizeBytes: Long?, w: Int?, h: Int?) {
            val primary = urls.firstOrNull() ?: return
            if (out.containsKey(primary)) return
            out[primary] = Stream(
                quality = quality(w, h)?.id,
                codec = codec,
                url = primary,
                backupUrls = urls.drop(1),
                width = w,
                height = h,
                bandwidth = bandwidth,
                sizeBytes = sizeBytes,
            )
        }

        for (e in video.arr("bitrateInfo").orEmpty()) {
            val o = e as? JsonObject ?: continue
            val addr = o.obj("PlayAddr") ?: o.obj("playAddr") ?: continue
            add(
                urls = urlList(addr),
                codec = codecOf(o.text("CodecType") ?: o.text("codecType")),
                bandwidth = o.long("Bitrate") ?: o.long("bitrate"),
                sizeBytes = addr.long("DataSize")?.takeIf { it > 0 },
                w = addr.int("Width") ?: addr.int("width"),
                h = addr.int("Height") ?: addr.int("height"),
            )
        }

        // 作品主地址：web 给的是**字符串**（catbus 的 itemMedia 取的就是它），有些版本给 `{UrlList:[…]}`；
        // 两种形态都认。它没有自己的尺寸，用 video.width/height。
        val mainW = video.int("width")
        val mainH = video.int("height")
        for (url in addrUrls(video, "playAddr")) {
            add(listOf(url), VideoCodec.AVC, null, null, mainW, mainH)
        }
        if (out.isEmpty()) {
            for (url in addrUrls(video, "downloadAddr")) {
                add(listOf(url), VideoCodec.AVC, null, null, mainW, mainH)
            }
        }

        return out.values.sortedWith(
            compareByDescending<Stream> { it.quality ?: 0 }.thenByDescending { it.bandwidth ?: 0L },
        )
    }

    /** [FeedDetail.video]：整条就是视频的作品才有。TikTok 没有弹幕，`danmakuSegmentMs` 留 null。 */
    fun info(o: JsonObject): VideoInfo? {
        val video = o.obj("video") ?: return null
        if (o.obj("imagePost")?.arr("images")?.isNotEmpty() == true) return null
        if (streams(video).isEmpty() && coverUrl(video) == null && video.long("duration") == null) return null
        val (w, h) = size(video)
        return VideoInfo(
            listOf(VideoPart(MAIN, o.text("desc"), durationSec(video), w, h)),
            danmakuSegmentMs = null,
        )
    }

    /**
     * 播放地址。TikTok 只有"音画一体的 mp4"，所以所有档位、各种编码都放进 [PlayInfo.progressive]，
     * 由前端按设置和硬解能力挑。[quality] 是前端要的那一档，有就给这一档当默认值。
     */
    fun play(o: JsonObject, partId: String, quality: Int?, headers: Map<String, String>): PlayInfo {
        if (partId != MAIN) throw ApiException(P, "part", "TikTok 的作品只有一个分段 `$MAIN`，没有 `$partId`")
        val video = o.obj("video") ?: throw ApiException(P, "no_video", "这条作品没有 video 对象")
        val all = streams(video)
        if (all.isEmpty()) throw ApiException(P, "no_stream", "这条作品没有可播放的地址（可能已删除或只对 App 开放）")
        val picked = quality?.let { q -> all.firstOrNull { it.quality == q } } ?: all.first()
        val qualities = all.mapNotNull { s -> quality(s.width, s.height) }.distinctBy { it.id }.sortedByDescending { it.id }
        return PlayInfo(
            durationMs = durationMs(video),
            quality = picked.quality,
            qualities = qualities,
            progressive = all.map { s ->
                ProgressiveStream(
                    quality = s.quality,
                    url = s.url,
                    backupUrls = s.backupUrls,
                    sizeBytes = s.sizeBytes,
                    width = s.width,
                    height = s.height,
                    codec = s.codec,
                    bandwidth = s.bandwidth,
                )
            },
            headers = headers,
        )
    }

    /**
     * `CodecType` → 统一编码。
     * 所以按子串判，别只认字面量。
     */
    fun codecOf(codecType: String?): VideoCodec {
        val t = codecType?.lowercase() ?: return VideoCodec.AVC
        return when {
            t.contains("av1") || t.contains("av01") -> VideoCodec.AV1
            t.contains("hvc1") || t.contains("h265") || t.contains("hevc") || t.contains("bytevc1") -> VideoCodec.HEVC
            t.contains("h264") || t.contains("avc") -> VideoCodec.AVC
            else -> VideoCodec.OTHER
        }
    }

    /**
     * 清晰度档位：短边 ≥ 2000 是 4K（120），1400~1999 是 2K（112，小红书 / 抖音也用这个数字），
     * 其余按 [`qualityOfSize`][feedhub.core.qualityOfSize] 的短边分档。
     */
    fun quality(width: Int?, height: Int?): Quality? {
        val short = listOfNotNull(width, height).filter { it > 0 }.minOrNull() ?: return null
        return if (short in 1400..1999) Quality(112, "2K") else qualityOfSize(width, height)
    }

    /**
     * `UrlList` 里像地址的那些。"像地址"= 带 scheme（`xxx://`）或协议相对（`//`）：
     * 脱敏夹具里的占位串因此被跳过（不猜地址），演示环境里的 `file://` 测试视频也能通过。
     */
    private fun urlList(o: JsonObject): List<String> =
        (o.arr("UrlList") ?: o.arr("urlList")).orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.content?.trim() }
            .filter { it.contains("://") || it.startsWith("//") }
            .map { it.toHttps() }
            .distinct()

    /**
     * 封面：web 的 `video.cover` / `originCover` / `dynamicCover` 也是**字符串**（不是 `{urlList}`），
     * [addrUrls] 两种形态都认。卡片和详情页显示的就是它。
     */
    fun coverUrl(video: JsonObject): String? =
        addrUrls(video, "cover").firstOrNull()
            ?: addrUrls(video, "originCover").firstOrNull()
            ?: addrUrls(video, "dynamicCover").firstOrNull()

    /**
     * `video.<key>` 的地址列表，两种形态都认：web 给的是**字符串**（catbus 的 `itemMedia` 取的就是它），
     * 有些版本给 `{UrlList:[…]}`（和 `bitrateInfo[].PlayAddr` 同形）。
     */
    fun addrUrls(video: JsonObject, key: String): List<String> {
        video.obj(key)?.let { return urlList(it) }
        return video.text(key)?.takeIf { it.contains("://") || it.startsWith("//") }
            ?.let { listOf(it.toHttps()) } ?: emptyList()
    }
}
