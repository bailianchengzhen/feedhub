package feedhub.source.douyin

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
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject

/**
 * 抖音的播放地址，纯函数（`DouyinVideoTest` 喂 fixtures）。
 *
 * ## 地址从哪来
 *
 * 响应里能播的地址有好几处，`video` 对象里就有三重：
 *
 * - `video.bit_rate[]`：每档清晰度一项，带 `bit_rate`（码率）、`is_h265`（0 = AVC、1 = HEVC）、
 *   `play_addr`（这一档的地址）、`play_addr_265` / `play_addr_h264`（同一档的另一种编码）、`FPS`、`format`。
 *   **这一层最全**：分辨率写在各自的 `play_addr.width` / `height` 上，所以清晰度按每条流自己的尺寸算，
 *   和 f2 issue #214 记的一样（高分辨率的 H.265 码率可能比低分辨率的 H.264 还低，只取第一条会漏档）。
 *   `format: "dash"` 的那些是**只有画面**的 DASH 轨（路径 `/media-video-avc1/`、`/media-video-hvc1/`，
 *   声音在 `bit_rate_audio` 里），**不收**——收进来就会放出没声音的片子。
 * - `video.play_addr` / `play_addr_265` / `play_addr_h264`：作品的主地址（和 bit_rate 里同分辨率的常常重复）。
 * - `video.download_addr` / `video.misc_download_addrs`：带水印的下载地址，**只在别的都没有时兜底**。
 *
 * ## 每个 `url_list` 里是两种形态，**要挑跳转那种**
 *
 * 每个地址的 `url_list` 是 `[CDN 直链, CDN 直链（另一台）, https://www.douyin.com/aweme/v1/play/?video_id=…]`：
 *
 * | 形态 | 样子 | 实测 |
 * |---|---|---|
 * | CDN 直链 | `https://v26-webf.douyinvod.com/<签名坐标>/<签名坐标>/video/tos/cn/tos-cn-ve-15c000-ce/<对象 id>/?a=6383&…&ft=…&l=…`（~555 字符） | **403 Forbidden**（openresty）。带上 `Referer` + 浏览器 UA、甚至把 `Accept` / `Accept-Language` / `sec-fetch-*` / `Origin` / `Range` 都补齐全套，照样 403 |
 * | 跳转 | `https://www.douyin.com/aweme/v1/play/?video_id=…&line=0&file_id=…&sign=…&is_play_url=1&source=PackSourceEnum_AWEME_DETAIL&biz_sign=…&aid=6383`（~361 字符） | **302 → `v11-weba.douyinvod.com` → 206 `video/mp4`**，只要 `Referer` + 浏览器 UA |
 *
 * 所以 [urlList] 把"跳转"那种排到最前（路径 `/aweme/v1/play/`），CDN 直链留在 `backupUrls` 里。
 * **每档清晰度的跳转地址各不相同**，
 * 所以清晰度阶梯不会因为"都指向同一个跳转"而塌掉。
 * 我们**没有**带 Cookie 去试过 CDN 直链（本任务打媒体一律不带 Cookie），所以"直链一定不行"这句话不成立；
 * 只确定：不带 Cookie 时它 403、跳转那条能用。
 *
 * ## 时效
 *
 * 两种形态都带时效参数（直链的 `l` / `ft`，跳转的 `sign` / `biz_sign`），**会过期**；
 * [DouyinSource.play] 只复用 60 秒内刚取过的同一条作品（[AWEME_REUSE_MS]，点开详情时那份），再旧就重新取 `aweme/detail`。
 *
 * ## 编码
 *
 * AVC 和 HEVC 都列出来（`is_h265`、`play_addr_265` / `play_addr_h264`），[ProgressiveStream.codec] 填好，
 * 由前端按硬件解码能力挑。抖音给的是音画一体的 mp4（`format: "mp4"`），所以走 [PlayInfo.progressive]，
 * 没有 DASH 轨。
 */
internal object DouyinVideo {
    private val P = DOUYIN

    /** [VideoPart.id]，整条作品就是一个视频（和小黑盒 / 贴吧 / 酷安的 `main` 一致）。 */
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

    /** `video.duration` 是毫秒（真实录制：`149676` = 149 秒）。 */
    fun durationSec(video: JsonObject): Int? =
        video.long("duration")?.takeIf { it > 0 }?.let { (it / 1000).toInt() }

    /** 作品的像素尺寸：`video.play_addr` 优先，没有就取 bit_rate 里最大的一档。 */
    fun size(video: JsonObject): Pair<Int?, Int?> {
        for (key in listOf("play_addr", "play_addr_h264", "play_addr_265")) {
            val a = video.obj(key) ?: continue
            val w = a.int("width")
            val h = a.int("height")
            if (w != null && h != null && w > 0 && h > 0) return w to h
        }
        val best = streams(video).maxByOrNull { (it.width?.toLong() ?: 0L) * (it.height ?: 0) }
        return best?.width to best?.height
    }

    /**
     * `video` 对象里所有能播的流，同一个地址只留一条；按清晰度、再按码率从高到低排。
     */
    fun streams(video: JsonObject): List<Stream> {
        val out = LinkedHashMap<String, Stream>()
        fun add(addr: JsonObject?, codec: VideoCodec, bandwidth: Long?, fallbackH: Int?, fallbackW: Int?) {
            if (addr == null) return
            val urls = urlList(addr)
            val primary = urls.firstOrNull() ?: return
            if (out.containsKey(primary)) return
            val w = addr.int("width")?.takeIf { it > 0 } ?: fallbackW
            val h = addr.int("height")?.takeIf { it > 0 } ?: fallbackH
            out[primary] = Stream(
                quality = quality(w, h)?.id,
                codec = codec,
                url = primary,
                backupUrls = urls.drop(1),
                width = w,
                height = h,
                bandwidth = bandwidth,
                sizeBytes = addr.long("data_size")?.takeIf { it > 0 },
            )
        }

        // 1) bit_rate[]：每档清晰度一条，编码按 is_h265 分；同一档的 play_addr_265 / play_addr_h264 是另一种编码
        val mainW = video.int("width")
        val mainH = video.int("height")
        for (e in video.arr("bit_rate").orEmpty()) {
            val o = e as? JsonObject ?: continue
            if (o.long("bit_rate") == null && o.obj("play_addr") == null) continue
            // `format: "dash"` 的那些是**只有画面、没有声音**的 DASH 轨（地址路径是 /media-video-avc1/、
            // /media-video-hvc1/，声音在 bit_rate_audio 里），放进 progressive 会放出没声音的片子，
            // 所以只收音画一体的 `format: "mp4"`。
            if (o.text("format") == "dash") continue
            val bw = o.long("bit_rate")
            val h265 = o.int("is_h265") == 1
            add(o.obj("play_addr"), if (h265) VideoCodec.HEVC else VideoCodec.AVC, bw, mainH, mainW)
            if (h265) add(o.obj("play_addr_h264"), VideoCodec.AVC, bw, mainH, mainW)
            else add(o.obj("play_addr_265"), VideoCodec.HEVC, bw, mainH, mainW)
        }

        // 2) 作品主地址
        add(video.obj("play_addr"), VideoCodec.AVC, null, mainH, mainW)
        add(video.obj("play_addr_h264"), VideoCodec.AVC, null, mainH, mainW)
        add(video.obj("play_addr_265"), VideoCodec.HEVC, null, mainH, mainW)

        // 3) 兜底：带水印的下载地址，只有别的都没有时才用得上
        if (out.isEmpty()) {
            add(video.obj("download_addr"), VideoCodec.AVC, null, mainH, mainW)
        }
        if (out.isEmpty()) {
            for (s in miscDownloadAddrs(video)) add(s, VideoCodec.AVC, null, mainH, mainW)
        }

        return out.values.sortedWith(
            compareByDescending<Stream> { it.quality ?: 0 }.thenByDescending { it.bandwidth ?: 0L },
        )
    }

    /** [FeedDetail.video]：整条就是视频的作品才有。抖音没有弹幕，`danmakuSegmentMs` 留 null。 */
    fun info(aweme: JsonObject): VideoInfo? {
        val video = aweme.obj("video") ?: return null
        val images = aweme.arr("images")
        if (!images.isNullOrEmpty()) return null
        if (streams(video).isEmpty() && video.obj("cover") == null && video.long("duration") == null) return null
        val (w, h) = size(video)
        return VideoInfo(
            listOf(VideoPart(MAIN, aweme.text("desc"), durationSec(video), w, h)),
            danmakuSegmentMs = null,
        )
    }

    /**
     * 播放地址。抖音只有"音画一体的 mp4"，所以把所有档位、两种编码都放进 [PlayInfo.progressive]，
     * 由前端按设置和硬解能力挑（`StreamSelection`）。[quality] 是前端要的那一档，有就给这一档当默认值。
     */
    fun play(aweme: JsonObject, partId: String, quality: Int?, headers: Map<String, String>): PlayInfo {
        if (partId != MAIN) throw ApiException(P, "part", "抖音的作品只有一个分段 `$MAIN`，没有 `$partId`")
        val video = aweme.obj("video") ?: throw ApiException(P, "no_video", "这条作品没有 video 对象")
        val all = streams(video)
        if (all.isEmpty()) throw ApiException(P, "no_stream", "这条作品没有可播放的地址（可能已删除或只对 App 开放）")
        val picked = quality?.let { q -> all.firstOrNull { it.quality == q } } ?: all.first()
        val qualities = all.mapNotNull { s -> quality(s.width, s.height) }.distinctBy { it.id }.sortedByDescending { it.id }
        return PlayInfo(
            durationMs = video.long("duration")?.takeIf { it > 0 },
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
     * 清晰度档位：短边 ≥ 2000 是 4K（120），1400~1999 是 2K（112，小红书也用这个数字），
     * 其余按 [`qualityOfSize`][feedhub.core.qualityOfSize] 的短边分档，这样设置里的"默认清晰度"对所有平台一样有效。
     */
    fun quality(width: Int?, height: Int?): Quality? {
        val short = listOfNotNull(width, height).filter { it > 0 }.minOrNull() ?: return null
        return if (short in 1400..1999) Quality(112, "2K") else qualityOfSize(width, height)
    }

    /**
     * `url_list` 里像地址的那些，**把 `/aweme/v1/play/` 那种跳转排到最前**。
     *
     * "像地址"= 带 scheme（`xxx://`）或协议相对（`//`）。脱敏夹具里那些 `redacted-token-1120`
     * 这样的不透明占位串因此被跳过（不猜地址）；演示环境里的 `file://` 测试视频也能通过
     * （`fake=douyin`，见 `app/.../dev/DouyinDemo.kt`）。
     */
    private fun urlList(o: JsonObject): List<String> =
        o.arr("url_list").orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.content?.trim() }
            .filter { it.contains("://") || it.startsWith("//") }
            .map { it.toHttps() }
            .sortedBy { if (isPlayRedirect(it)) 0 else 1 }

    /** 是不是 `https://www.douyin.com/aweme/v1/play/…` 那种跳转地址（不是 CDN 直链）。 */
    private fun isPlayRedirect(url: String): Boolean = url.contains(PLAY_REDIRECT_PATH)

    private const val PLAY_REDIRECT_PATH = "/aweme/v1/play/"

    /**
     * `misc_download_addrs` 是一段 **JSON 字符串**（`{"suffix_scene":{"url_list":[…]}}`）。
     * 只在解析得出来时才用，坏数据直接跳过（不抛异常）。
     */
    private fun miscDownloadAddrs(video: JsonObject): List<JsonObject> {
        val raw = video.text("misc_download_addrs") ?: return emptyList()
        val root = runCatching { feedhub.core.FeedJson.parseToJsonElement(raw).jsonObject }.getOrNull() ?: return emptyList()
        return root.entries.mapNotNull { (_, v) -> (v as? JsonObject)?.takeIf { it.arr("url_list") != null } }
    }
}
