package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.VideoCodec
import feedhub.core.arr
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.qualityOfSize
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject

/**
 * 视频笔记的播放地址：`note_card.video.media.stream.<分组>[]`。
 *
 * 实测：
 * - 分组键名被混淆成 `EF4`~`EF7`，**每条流自己的 `video_codec` 也是 `EF4` / `EF5`**。EF4 = h264、EF5 = h265 有 Spider_XHS 的注释和
 *   SSR 页面分组名 `h264/h265/h266/av1` 的顺序为证；EF6、EF7 没见过有内容的流，没确认是什么，按 [VideoCodec.OTHER] 列出（播放器排最后）。
 *   流里的 `video_codec` 要是以后改回 `h264` / `hevc` / `av1` 这样的名字也认。
 * - 每条流是 mp4 直链（`master_url` 加两个 `backup_urls`），带 `sign` 和 `t`（十六进制 Unix 秒）两个参数，**有效期约 3 天**（`t` 比取到的时间晚 3.5 天）；
 *   CDN 不要 Referer、不要 Cookie，支持 Range，https 可用。
 * - 一条笔记有多个分辨率，且 HEVC 一直到 2160×3840：这个例子里 EF4 只有一条 `stream_type 259`（720×1280），EF5 有 520 / 521 / 522 / 523 四条
 *   （720 / 1080 / 1440 / 2160 的短边）。
 * - `stream_type` 259 和 309 是带水印的流，丢掉；**只剩水印流时**（旧视频可能只有它）
 *   还是返回它们，带水印的能放比报错好。
 */
internal object XhsVideo {
    /** [feedhub.core.VideoPart.id]，整条笔记就是一个视频。 */
    const val MAIN = "main"

    private val WATERMARKED = setOf(259, 309)
    private val json = Json { ignoreUnknownKeys = true }

    class Stream(val group: String, val o: JsonObject) {
        val streamType: Int? get() = o.int("stream_type")
        val width: Int? get() = o.int("width")
        val height: Int? get() = o.int("height")
        val watermarked: Boolean get() = streamType in WATERMARKED
    }

    fun streams(note: JsonObject): List<Stream> {
        val groups = note.obj("video")?.obj("media")?.obj("stream") ?: return emptyList()
        return groups.entries.flatMap { (group, list) ->
            (list as? JsonArray).orEmpty().mapNotNull { (it as? JsonObject)?.let { o -> Stream(group, o) } }
        }.filter { it.o.text("master_url") != null }
    }

    /** 视频的像素尺寸：`media_v2` 里是最大的那一档，没有就取流里最大的。 */
    fun size(note: JsonObject): Pair<Int?, Int?> {
        val v2 = note.obj("video")?.text("media_v2")?.let { runCatching { json.parseToJsonElement(it).jsonObject }.getOrNull() }
        val vv = v2?.obj("video")
        val w = vv?.int("width")
        val h = vv?.int("height")
        if (w != null && h != null) return w to h
        val best = streams(note).maxByOrNull { (it.width ?: 0) * (it.height ?: 0) }
        return best?.width to best?.height
    }

    fun codec(s: Stream): VideoCodec = when ((s.o.text("video_codec") ?: s.group).lowercase()) {
        "ef4", "h264", "avc", "avc1" -> VideoCodec.AVC
        "ef5", "h265", "hevc", "hvc1", "hev1" -> VideoCodec.HEVC
        "av1", "av01" -> VideoCodec.AV1
        else -> VideoCodec.OTHER
    }

    /** 清晰度档位：短边 ≥ 2000 是 4K（120），≥ 1400 是 2K（112，B站的 1080P+ 的数字，这里借来区分），其余按 [qualityOfSize]。 */
    fun quality(width: Int?, height: Int?): Quality? {
        val short = listOfNotNull(width, height).filter { it > 0 }.minOrNull() ?: return null
        return if (short in 1400..1999) Quality(112, "2K") else qualityOfSize(width, height)
    }

    fun play(note: JsonObject): PlayInfo {
        val all = streams(note)
        val usable = all.filterNot { it.watermarked }.ifEmpty { all }
        if (usable.isEmpty()) throw ApiException(XHS, "no_stream", "这条笔记没有可播放的视频流")
        // 排序键是（清晰度 desc，码率 desc），**两个键的类型必须一致**：以前第二个键写的是
        // `it.bandwidth ?: 0`，推成 `Comparable<*> & Number`，两条流清晰度相同（比如都没有宽高、或 h264 和
        // h265 同分辨率）时 `compareValues` 会拿 Integer 去调 `Long.compareTo` → ClassCastException。
        // Rust 侧（`xhs/play.rs`）一直按 `(i32, i64)` 元组排，语义就是这个"码率 desc"。
        val progressive = usable.map { s ->
            val o = s.o
            ProgressiveStream(
                quality = quality(s.width, s.height)?.id,
                url = o.text("master_url")!!.toHttps(),
                backupUrls = o.arr("backup_urls").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.takeIf { u -> u.isNotBlank() }?.toHttps() },
                sizeBytes = o.long("size"),
                width = s.width,
                height = s.height,
                codec = codec(s),
                bandwidth = o.long("avg_bitrate") ?: o.long("video_bitrate"),
            )
        }.sortedWith(compareByDescending<ProgressiveStream> { it.quality ?: 0 }.thenByDescending { it.bandwidth ?: 0L })
        val qualities = progressive.mapNotNull { p -> quality(p.width, p.height) }.distinctBy { it.id }.sortedByDescending { it.id }
        val durationMs = usable.firstNotNullOfOrNull { it.o.long("duration") }
            ?: note.obj("video")?.obj("capa")?.int("duration")?.let { it * 1000L }
        return PlayInfo(
            durationMs = durationMs,
            quality = qualities.firstOrNull()?.id,
            qualities = qualities,
            progressive = progressive,
            headers = mapOf("Referer" to XhsApi.WEB_URL),
        )
    }
}
