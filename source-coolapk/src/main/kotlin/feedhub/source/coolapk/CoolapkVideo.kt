package feedhub.source.coolapk

import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.FeedJson
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.ProgressiveStream
import feedhub.core.UserAgents
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.qualityOfSize
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/**
 * 酷安的视频动态（`feedType=video`，`media_type=2`）。纯函数，单测喂 fixtures。
 *
 * `media_url` 有两种：
 * - **酷安自己托管**：`https://video-qc.coolapk.com/<目录>/<目录>/<名字>.mp4`（腾讯云点播）。`media_info`（JSON 字符串）里有
 *   `duration`（毫秒）、`cover`（封面）、`width` / `height`。**直接拉会 403，正文 `need sign info`**：开了防盗链（URL 签名），
 *   签名只有官方 App 才知道怎么算（详情接口也没给签名；`media_info.requestParams` 里的地址也是不带签名的；网页版动态页只写"请用 App 扫码查看"；
 *   参考项目 Coolapk-UWP 把 `media_url` 当分享链接打开、c001apk-flutter 没做视频，都没有处理）。所以 [play] 老实报错，不假装能播，
 *   见 [feedhub.source.coolapk.CoolapkSource.play]。
 * - **站外链接**：`weibo.com/…`、B站、抖音等，酷安当"视频分享"卡片展示。B站的（`bilibili.com/video/BV…`，`b23.tv` 短链要先跟一次重定向）
 *   转成指向 B站 Source 的 [DetailTarget.Native]（带 `platform = BILIBILI`），用我们自己的 B站播放器看；别的站点只给卡片 + 链接。
 */
internal object CoolapkVideo {
    private val P = COOLAPK
    const val MAIN = "main"

    private val BVID = Regex("BV1[1-9A-HJ-NP-Za-km-z]{9}")
    private val VIDEO_EXT = setOf("mp4", "m4v", "mov", "webm", "m3u8")

    /** `media_info` 里的东西。 */
    class Info(val durationMs: Long?, val cover: String?, val width: Int?, val height: Int?) {
        val durationSec: Int? get() = durationMs?.let { (it / 1000).toInt() }?.takeIf { it > 0 }
    }

    /** 酷安自己托管的视频。 */
    class Hosted(val url: String, val info: Info)

    private fun JsonObject.hasMedia(): Boolean = (long("media_type") ?: 0L) != 0L && text("media_url") != null

    /** `media_info` 是 JSON 字符串；解析不了就当没有。 */
    fun info(o: JsonObject): Info {
        val m = o.text("media_info")?.let { runCatching { FeedJson.parseToJsonElement(it) as? JsonObject }.getOrNull() }
        return Info(
            m?.long("duration")?.takeIf { it > 0 },
            m?.text("cover")?.toHttps(),
            m?.int("width")?.takeIf { it > 0 },
            m?.int("height")?.takeIf { it > 0 },
        )
    }

    /** 封面：`media_pic` 优先，没有用 `media_info.cover`。 */
    fun cover(o: JsonObject): String? = (o.text("media_pic") ?: info(o).cover)?.toHttps()

    /** 酷安自己托管的视频（`*.coolapk.com` 下的视频文件）；站外链接返回 null。 */
    fun hosted(o: JsonObject): Hosted? {
        if (!o.hasMedia()) return null
        val url = o.text("media_url")!!.toHttps()
        val http = url.toHttpUrlOrNull() ?: return null
        val ext = http.encodedPath.substringAfterLast('.', "").lowercase()
        if (!(http.host == "coolapk.com" || http.host.endsWith(".coolapk.com")) || ext !in VIDEO_EXT) return null
        return Hosted(url, info(o))
    }

    /** [FeedDetail.video]：酷安托管的视频才有；没有弹幕。 */
    fun videoInfo(h: Hosted?): VideoInfo? = h?.let { VideoInfo(listOf(VideoPart(MAIN, null, it.info.durationSec, it.info.width, it.info.height))) }

    /** 站外视频的卡片（酷安托管的返回 null）。[resolved] 是 `b23.tv` 短链跟完重定向后的地址。 */
    fun externalCard(o: JsonObject, resolved: String? = null): Block.Card? {
        if (!o.hasMedia() || hosted(o) != null) return null
        val url = o.text("media_url")!!.toHttps()
        val target = bilibiliTarget(resolved ?: url)
        val host = url.toHttpUrlOrNull()?.host ?: url
        return Block.Card(
            title = if (target != null) "B站视频" else "站外视频",
            subtitle = host,
            cover = cover(o),
            url = url,
            target = target,
        )
    }

    /** B站视频链接（`bilibili.com/video/BV…`）→ 指向 B站 Source 的原生详情。别的链接返回 null。 */
    fun bilibiliTarget(url: String): DetailTarget.Native? {
        val http = url.toHttpUrlOrNull() ?: return null
        if (!(http.host == "bilibili.com" || http.host.endsWith(".bilibili.com"))) return null
        val bv = BVID.find(http.encodedPath)?.value ?: return null
        return DetailTarget.Native(bv, Platform("bili"))
    }

    /** `b23.tv` 短链：要跟一次重定向才知道是哪个视频。 */
    fun isShortLink(url: String): Boolean = url.toHttpUrlOrNull()?.host == "b23.tv"

    /** 酷安托管的视频的播放信息（地址不变，不需要现取；防盗链见类说明）。 */
    fun play(h: Hosted): PlayInfo {
        val q = qualityOfSize(h.info.width, h.info.height)
        return PlayInfo(
            durationMs = h.info.durationMs,
            quality = q?.id,
            qualities = listOfNotNull(q),
            progressive = listOf(ProgressiveStream(q?.id, h.url, width = h.info.width, height = h.info.height)),
            headers = mapOf("Referer" to "${CoolapkHtml.WEB}/", "User-Agent" to UserAgents.DESKTOP_CHROME),
        )
    }
}
