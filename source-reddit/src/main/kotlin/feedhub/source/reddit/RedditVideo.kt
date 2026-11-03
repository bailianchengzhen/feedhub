package feedhub.source.reddit

import feedhub.core.ApiException
import feedhub.core.AudioStream
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.SegmentBase
import feedhub.core.VideoCodec
import feedhub.core.VideoStream
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.obj
import feedhub.core.qualityOfSize
import feedhub.core.text
import kotlinx.serialization.json.JsonObject
import org.w3c.dom.Element
import org.w3c.dom.Node
import java.io.StringReader
import javax.xml.parsers.DocumentBuilderFactory
import org.xml.sax.InputSource

/**
 * `reddit_video`（v.redd.it 上托管的视频）→ [PlayInfo]。纯解析 + 一次 MPD 取回由调用方做。
 *
 * 结构来自 Reddit 公开的 `.json`（`secure_media.reddit_video` / `media.reddit_video` /
 * `preview.reddit_video_preview`）和 `v.redd.it` 的 DASH 清单：
 *
 * - `fallback_url` 是**只有画面**的 mp4（`DASH_720.mp4` 之类），`hls_url` 是音画一体的 m3u8，
 *   `dash_url` 是 `DASHPlaylist.mpd`；
 * - 路线 C 用 DASH：清单里的 `Representation` 带 `BaseURL` + `SegmentBase`（`indexRange` / `Initialization`），
 *   正好是 [VideoStream.segmentBase] 要的形状，交给现有的中转 + `DashManifest` 播放（和 B站的单文件 DASH 一样）；
 * - **HLS 也优先 DASH**：清单取得到就用 DASH，取不到（网络错误 / 结构变了）才退回
 *   `hls_url` 的整段 m3u8（音画一体），再不行退回 `fallback_url` 的无声 mp4——这条回退链在 [play] 里写死，
 *   结果里如实标出用的是哪条；
 * - 红迪的 gif 帖也是 `reddit_video`（`is_gif=true`，没有音轨），一样能播。
 *
 * 参考（只学思路 / 字段名，不抄代码）：Octonaut `Services/RedditJSONCodec.swift` 的 `videoMedia`、
 * `AVFoundationMediaService.swift` 的 `RedditDASHManifest`（AGPL-3.0）；redlib `src/utils.rs` 的 `Media::parse`（AGPL-3.0）。
 */
internal object RedditVideo {

    /** [feedhub.core.VideoPart.id]，整条帖子就是一个视频。 */
    const val MAIN = "main"

    /** [AudioStream.id]，红迪只会给一路音轨。 */
    private const val AUDIO_ID = 1

    class Video(
        val id: String,
        val fallbackUrl: String?,
        val hlsUrl: String?,
        val dashUrl: String?,
        val width: Int?,
        val height: Int?,
        val durationSec: Int?,
        val isGif: Boolean,
        val hasAudio: Boolean,
    )

    class Mpd(val durationMs: Long?, val video: List<VideoStream>, val audio: List<AudioStream>)

    /**
     * 从一条帖子（或它的 crosspost 原帖）里挑出 `reddit_video`。字段名见 Octonaut `videoMedia` / redlib `Media::parse`：
     * 优先 `secure_media`，其次 `media`，最后 `preview.reddit_video_preview`。
     */
    fun videoOf(post: JsonObject): Video? {
        val candidates = listOfNotNull(post, post.arr("crosspost_parent_list")?.firstOrNull()?.obj())
        for (o in candidates) {
            val rv = sequenceOf(
                o.obj("secure_media")?.obj("reddit_video"),
                o.obj("media")?.obj("reddit_video"),
                o.obj("preview")?.obj("reddit_video_preview"),
            ).firstOrNull { it != null } ?: continue
            val fallback = rv.text("fallback_url")
            val hls = rv.text("hls_url")
            val dash = rv.text("dash_url")
            val id = rv.text("dash_url")?.let(::videoIdOf) ?: fallback?.let(::videoIdOf)
            val video = Video(
                id = id ?: o.text("id") ?: continue,
                fallbackUrl = fallback,
                hlsUrl = hls,
                dashUrl = dash,
                width = rv.int("width"),
                height = rv.int("height"),
                durationSec = rv.int("duration"),
                isGif = rv.bool("is_gif") == true,
                hasAudio = rv.bool("has_audio") != false,
            )
            if (video.fallbackUrl != null || video.hlsUrl != null || video.dashUrl != null) return video
        }
        return null
    }

    /** 取视频 id：`https://v.redd.it/<id>/...` 的第一段路径。 */
    fun videoIdOf(url: String): String? =
        url.substringAfter("//").substringAfter('/', "").substringBefore('/').takeIf { it.isNotBlank() }

    /**
     * 要取回哪份清单：优先响应里的 `dash_url`，没有就从 `fallback_url` / `hls_url` 的同目录拼
     * `DASHPlaylist.mpd`（Octonaut 也是这么拼 `HLSPlaylist.m3u8` 的）。
     */
    fun dashUrlOf(video: Video): String? {
        video.dashUrl?.let { return it }
        val any = video.fallbackUrl ?: video.hlsUrl ?: return null
        val idx = any.lastIndexOf('/')
        if (idx <= "https://x".length) return null
        return any.substring(0, idx + 1) + "DASHPlaylist.mpd"
    }

    /**
     * 解析 `DASHPlaylist.mpd`。[mpdUrl] 用来把相对的 `BaseURL` 拼成绝对地址。
     * 结构不对（或没有视频轨）返回 null，调用方回退 HLS / fallback。
     */
    fun decodeMpd(xml: String, mpdUrl: String): Mpd? = runCatching {
        val factory = DocumentBuilderFactory.newInstance().apply {
            runCatching { setFeature("http://apache.org/xml/features/disallow-doctype-decl", true) }
            runCatching { setFeature("http://xml.org/sax/features/external-general-entities", false) }
            runCatching { setFeature("http://xml.org/sax/features/external-parameter-entities", false) }
            isExpandEntityReferences = false
            isNamespaceAware = false
        }
        val doc = factory.newDocumentBuilder().parse(InputSource(StringReader(xml)))
        val root = doc.documentElement ?: return null
        val durationMs = parseIsoDuration(root.getAttribute("mediaPresentationDuration"))
        val video = mutableListOf<VideoStream>()
        val audio = mutableListOf<AudioStream>()
        for (adaptation in root.descendants("AdaptationSet")) {
            val mime = adaptation.getAttribute("mimeType").ifBlank { adaptation.getAttribute("contentType") }
            val audioSet = mime.contains("audio", ignoreCase = true)
            for (rep in adaptation.descendants("Representation")) {
                val base = rep.childText("BaseURL") ?: adaptation.childText("BaseURL") ?: continue
                val url = resolve(mpdUrl, base.trim())
                val sb = rep.child("SegmentBase")?.let { el ->
                    SegmentBase(
                        initialization = el.child("Initialization")?.getAttribute("range").orEmpty(),
                        indexRange = el.getAttribute("indexRange"),
                    )
                }
                if (sb != null && (sb.indexRange.isBlank() || sb.initialization.isBlank())) continue
                val codecs = rep.getAttribute("codecs").takeIf { it.isNotBlank() }
                val bandwidth = rep.getAttribute("bandwidth").toLongOrNull()
                val width = rep.getAttribute("width").toIntOrNull()
                val height = rep.getAttribute("height").toIntOrNull()
                if (audioSet) {
                    audio += AudioStream(
                        id = audio.size + AUDIO_ID,
                        codecs = codecs,
                        url = url,
                        bandwidth = bandwidth,
                        segmentBase = sb,
                    )
                } else {
                    val q = qualityOfSize(width, height)?.id ?: 0
                    video += VideoStream(
                        quality = q,
                        codec = codecOf(codecs),
                        codecs = codecs,
                        url = url,
                        bandwidth = bandwidth,
                        width = width,
                        height = height,
                        frameRate = rep.getAttribute("frameRate").takeIf { it.isNotBlank() },
                        segmentBase = sb,
                    )
                }
            }
        }
        // 没有视频轨就不算解析成功：调用方据此回退 HLS / fallback
        if (video.isEmpty()) null
        else Mpd(
            durationMs,
            video.sortedWith(compareByDescending<VideoStream> { it.quality }.thenByDescending { it.bandwidth ?: 0 }),
            audio,
        )
    }.getOrNull()

    /**
     * 组成播放信息。有 DASH（视频轨 + 可选的音频轨）就用它，否则退回 HLS / fallback mp4（[Note]）。
     */
    fun play(platform: Platform, video: Video, mpd: Mpd?): PlayInfo {
        val headers = mapOf("Accept" to "*/*")
        if (mpd != null && mpd.video.isNotEmpty()) {
            val sorted = mpd.video.sortedWith(
                compareByDescending<VideoStream> { it.quality }.thenByDescending { it.bandwidth ?: 0 },
            )
            val qualities = sorted.map { Quality(it.quality, qualityLabel(it.width, it.height)) }.distinctBy { it.id }
            return PlayInfo(
                durationMs = mpd.durationMs ?: video.durationSec?.let { it * 1000L },
                quality = qualities.firstOrNull()?.id,
                qualities = qualities,
                video = sorted,
                audio = mpd.audio,
                headers = headers,
            )
        }
        val url = video.hlsUrl ?: video.fallbackUrl
            ?: throw ApiException(platform, "no_video", "这条帖子没有可播放的地址（fallback_url / hls_url / dash_url 都没有）")
        val quality = qualityOfSize(video.width, video.height)
        return PlayInfo(
            durationMs = video.durationSec?.let { it * 1000L },
            quality = quality?.id,
            qualities = listOfNotNull(quality),
            progressive = listOf(
                ProgressiveStream(
                    quality = quality?.id,
                    url = url,
                    width = video.width,
                    height = video.height,
                    // HLS 是音画一体；退回 fallback_url 时只有画面（服务端说 has_audio 也拿不到音轨）
                    codec = null,
                ),
            ),
            headers = headers,
        )
    }

    private fun qualityLabel(width: Int?, height: Int?): String {
        val q = qualityOfSize(width, height)
        val size = if (width != null && height != null) "${width}x$height" else ""
        return listOfNotNull(q?.label, size.takeIf { it.isNotEmpty() }).joinToString(" ")
    }

    private fun codecOf(codecs: String?): VideoCodec = when {
        codecs == null -> VideoCodec.OTHER
        codecs.startsWith("avc") || codecs.startsWith("avc1") -> VideoCodec.AVC
        codecs.startsWith("hev") || codecs.startsWith("hvc") -> VideoCodec.HEVC
        codecs.startsWith("av01") -> VideoCodec.AV1
        else -> VideoCodec.OTHER
    }

    /** `PT1M30.5S` / `PT12S` / `PT1H2M3S` → 毫秒。 */
    fun parseIsoDuration(text: String): Long? {
        val m = Regex("^P(?:(\\d+)D)?T(?:(\\d+)H)?(?:(\\d+)M)?(?:(\\d+(?:\\.\\d+)?)S)?$").matchEntire(text.trim()) ?: return null
        val days = m.groupValues[1].toLongOrNull() ?: 0
        val hours = m.groupValues[2].toLongOrNull() ?: 0
        val minutes = m.groupValues[3].toLongOrNull() ?: 0
        val seconds = m.groupValues[4].toDoubleOrNull() ?: 0.0
        return (((days * 24 + hours) * 60 + minutes) * 60) * 1000 + (seconds * 1000).toLong()
    }

    /** 相对地址按 [base] 拼成绝对地址（`BaseURL` 一般是 `DASH_720.mp4` 这样的同目录文件名）。 */
    private fun resolve(base: String, path: String): String = when {
        path.startsWith("http://") || path.startsWith("https://") -> path
        path.startsWith("//") -> "https:$path"
        path.startsWith("/") -> "https://" + base.substringAfter("//").substringBefore('/') + path
        else -> base.substringBeforeLast('/') + "/" + path
    }

    // ---------- DOM 小工具（只看直接子元素，避免 getElementsByTagName 递归串味） ----------

    private fun Element.child(name: String): Element? =
        (0 until childNodes.length).mapNotNull { childNodes.item(it) as? Element }.firstOrNull { it.tagName == name }

    private fun Element.childText(name: String): String? = child(name)?.textContent?.trim()?.takeIf { it.isNotEmpty() }

    private fun Element.descendants(name: String): List<Element> {
        val out = mutableListOf<Element>()
        fun walk(node: Node) {
            for (i in 0 until node.childNodes.length) {
                val child = node.childNodes.item(i)
                if (child is Element) {
                    if (child.tagName == name) out += child else walk(child)
                }
            }
        }
        walk(this)
        return out
    }
}
