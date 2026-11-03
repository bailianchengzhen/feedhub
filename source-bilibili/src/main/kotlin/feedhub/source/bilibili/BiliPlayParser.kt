package feedhub.source.bilibili

import feedhub.core.AudioStream
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.SegmentBase
import feedhub.core.VideoCodec
import feedhub.core.VideoStream
import feedhub.core.arr
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/** 播放地址（`/x/player/wbi/playurl`）→ [PlayInfo]。纯函数，单测喂 fixtures。 */
internal object BiliPlayParser {
    private val P = BILIBILI

    /**
     * 请求要带 `fnval=4048`（DASH + HDR + 4K + 杜比音频 / 视界 + 8K + AV1）时返回 `dash`：
     * `video[]`（每个清晰度、每种编码各一条，`codecid` 7 AVC / 12 HEVC / 13 AV1）、`audio[]`，另有 `dolby.audio[]`、`flac.audio`；
     * 老视频和不给 DASH 的情况返回 `durl[]`（整段文件）。
     * `support_formats` 是这个视频的全部清晰度（包括没登录 / 不是大会员拿不到的），哪些真给了流看 `dash.video[].id`。
     */
    fun play(json: JsonObject, headers: Map<String, String>): PlayInfo {
        val data = json.obj("data") ?: throw ParseException(P, "播放地址缺少 data")
        val dash = data.obj("dash")
        val durl = data.arr("durl").orEmpty().mapNotNull { it.obj() }
        if (dash == null && durl.isEmpty()) throw ParseException(P, "播放地址既没有 dash 也没有 durl")

        val video = dash?.arr("video").orEmpty().mapNotNull { it.obj()?.let(::videoStream) }.sortedByDescending { it.quality }
        val audio = dash?.let(::audioStreams).orEmpty()
        val current = data.int("quality")
        val progressive = durl.mapNotNull { progressive(it, current) }

        val available: Set<Int> = if (dash != null) video.map { it.quality }.toSet() else setOfNotNull(current)
        return PlayInfo(
            durationMs = data.long("timelength")?.takeIf { it > 0 } ?: dash?.long("duration")?.takeIf { it > 0 }?.times(1000),
            // DASH 时 data.quality 是"请求的"清晰度，实际给到的看视频轨里最高的
            quality = if (dash != null) video.maxOfOrNull { it.quality } else current,
            qualities = qualities(data, available),
            video = video,
            audio = audio,
            progressive = progressive,
            headers = headers,
        )
    }

    private fun qualities(data: JsonObject, available: Set<Int>): List<Quality> {
        val formats = data.arr("support_formats").orEmpty().mapNotNull { it.obj() }
        if (formats.isNotEmpty()) {
            return formats.mapNotNull { f ->
                val id = f.int("quality") ?: return@mapNotNull null
                Quality(id, f.text("new_description") ?: f.text("display_desc") ?: id.toString(), id in available)
            }
        }
        // 老接口没有 support_formats，用 accept_quality / accept_description 两个对齐的数组
        val ids = data.arr("accept_quality").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.toIntOrNull() }
        val names = data.arr("accept_description").orEmpty().map { (it as? JsonPrimitive)?.content }
        return ids.mapIndexed { i, id -> Quality(id, names.getOrNull(i) ?: id.toString(), id in available) }
    }

    private fun videoStream(o: JsonObject): VideoStream? {
        val (url, backups) = urls(o) ?: return null
        return VideoStream(
            quality = o.int("id") ?: return null,
            codec = when (o.int("codecid")) {
                7 -> VideoCodec.AVC
                12 -> VideoCodec.HEVC
                13 -> VideoCodec.AV1
                else -> VideoCodec.OTHER
            },
            codecs = o.text("codecs"),
            url = url,
            backupUrls = backups,
            bandwidth = o.long("bandwidth"),
            width = o.int("width")?.takeIf { it > 0 },
            height = o.int("height")?.takeIf { it > 0 },
            frameRate = o.text("frameRate") ?: o.text("frame_rate"),
            segmentBase = segmentBase(o),
        )
    }

    /** 普通音轨、杜比音轨（`dolby.audio[]`）、无损音轨（`flac.audio`，是个对象）都放进来，`id` 区分（30280 / 30250 / 30251 …）。 */
    private fun audioStreams(dash: JsonObject): List<AudioStream> {
        val all = dash.arr("audio").orEmpty().mapNotNull { it.obj() } +
            dash.obj("dolby")?.arr("audio").orEmpty().mapNotNull { it.obj() } +
            listOfNotNull(dash.obj("flac")?.obj("audio"))
        return all.mapNotNull { o ->
            val (url, backups) = urls(o) ?: return@mapNotNull null
            AudioStream(
                id = o.int("id") ?: return@mapNotNull null,
                codecs = o.text("codecs"),
                url = url,
                backupUrls = backups,
                bandwidth = o.long("bandwidth"),
                segmentBase = segmentBase(o),
            )
        }.sortedByDescending { it.bandwidth ?: 0 }
    }

    private fun progressive(o: JsonObject, quality: Int?): ProgressiveStream? {
        val primary = o.text("url") ?: return null
        val backups = o.arr("backup_url").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.takeIf(String::isNotBlank) }
        val ordered = order(listOf(primary) + backups)
        return ProgressiveStream(quality = quality, url = ordered.first(), backupUrls = ordered.drop(1), sizeBytes = o.long("size"))
    }

    private fun segmentBase(o: JsonObject): SegmentBase? {
        val sb = o.obj("segment_base") ?: o.obj("SegmentBase") ?: return null
        val init = sb.text("initialization") ?: sb.text("Initialization") ?: return null
        val index = sb.text("index_range") ?: sb.text("indexRange") ?: return null
        return SegmentBase(init, index)
    }

    /** 主地址 + 备用地址，按 [order] 重排后第一个当主地址。 */
    private fun urls(o: JsonObject): Pair<String, List<String>>? {
        val primary = o.text("base_url") ?: o.text("baseUrl") ?: return null
        val backups = (o.arr("backup_url") ?: o.arr("backupUrl")).orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.content?.takeIf(String::isNotBlank) }
        val ordered = order(listOf(primary) + backups)
        return ordered.first() to ordered.drop(1)
    }

    /**
     * 同一路流的几个地址：不稳定的主机往后排，其余保持 B站给的顺序。
     * playurl 经常把 mcdn（`xy…xy.mcdn.bilivideo.cn:8082`）这类 P2P / 自建节点排在第一位，高峰期容易慢或断，
     * 备用地址里反而有正常的 upos 镜像。思路和"风险主机"的判断照 BiliPai
     * `feature/plugin/CdnOptimizationPolicy.kt` 的 `sortAuthorizedCdnCandidates` / `isRiskyCdnHost`（GPL-3.0）：
     * 主机名含 mcdn / pcdn / gotcha，或者是裸 IP，就算不稳定。这里另加一条：带非默认端口的（如 `*.edge.mountaintoys.cn:4483`）也算——
     * 真实响应里这种边缘节点出现在备用地址里，BiliPai 的规则没覆盖到。
     */
    fun order(urls: List<String>): List<String> {
        val distinct = urls.filter { it.isNotBlank() }.distinct()
        return distinct.filterNot(::isUnstable) + distinct.filter(::isUnstable)
    }

    fun isUnstable(url: String): Boolean {
        val u = url.toHttpUrlOrNull() ?: return false
        val host = u.host.lowercase()
        if ("mcdn" in host || "pcdn" in host || "gotcha" in host) return true
        if (Regex("""\d{1,3}(\.\d{1,3}){3}""").matches(host)) return true
        return u.port != (if (u.isHttps) 443 else 80)
    }
}
