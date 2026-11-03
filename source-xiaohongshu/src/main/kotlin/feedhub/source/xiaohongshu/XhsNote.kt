package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Stats
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.arr
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.parseCount
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/**
 * 笔记详情：`POST /api/sns/web/v1/feed` 的 `data.items[0].note_card`。
 * 字段：`note_id type(normal|video) title desc time(毫秒) last_update_time ip_location user
 * interact_info{liked_count collected_count comment_count share_count …} image_list[] tag_list[] at_user_list[] video{capa media image media_v2}`。
 * `title` 经常是空串（正文在 `desc`）。
 */
internal object XhsNote {
    private val P = XHS

    /** 响应里的 `note_card`；`xsec_token` 失效或缺失时接口回 `code 0` 但没有条目（redbook 的说法），没有就抛带原因的错。 */
    fun noteCard(json: JsonObject): JsonObject =
        json.obj("data")?.arr("items")?.firstNotNullOfOrNull { (it as? JsonObject)?.obj("note_card") }
            ?: throw ApiException(
                P, "xsec_token",
                "笔记打不开：入口里的 xsec_token 已失效或缺失（很久以前收藏的笔记会这样），请从信息流或作者主页重新打开",
            )

    /** 标题 + 正文，检查有没有表情记号用。 */
    fun texts(note: JsonObject): String = (note.string("title").orEmpty()) + "\n" + (note.string("desc").orEmpty())

    fun detail(note: JsonObject, ref: XhsParser.NoteRef, emotes: Map<String, String>): FeedDetail {
        val id = note.text("note_id") ?: ref.noteId
        val isVideo = note.text("type") == "video"
        val title = note.text("title")
        val desc = note.string("desc").orEmpty()
        val topics = note.arr("tag_list").orEmpty().mapNotNull { (it as? JsonObject) }
            .filter { it.text("type") == "topic" }
            .mapNotNull { t -> t.text("name")?.let { n -> t.text("id")?.let { n to it } } }
            .toMap()
        val mentions = note.arr("at_user_list").orEmpty().mapNotNull { (it as? JsonObject)?.let(XhsParser::user) }
        val images = note.arr("image_list").orEmpty().mapNotNull { (it as? JsonObject)?.let(::image) }
        val cover = images.firstOrNull()?.url
        val video = note.obj("video")
        val duration = video?.obj("capa")?.int("duration")
        val interact = note.obj("interact_info")
        val summary = XhsBody.plain(desc).trim().ifEmpty { null }

        val item = FeedItem(
            platform = P,
            id = id,
            kind = if (isVideo) ItemKind.VIDEO else ItemKind.POST,
            title = title,
            summary = summary,
            author = note.obj("user")?.let(XhsParser::user),
            publishedAt = note.long("time")?.let { it / 1000 } ?: XhsParser.idTime(id),
            media = if (isVideo) listOf(Media.Video(cover, duration, images.firstOrNull()?.width, images.firstOrNull()?.height)) else images.map { Media.Image(it.url, it.width, it.height) },
            stats = Stats(
                likes = parseCount(interact?.string("liked_count")),
                favorites = parseCount(interact?.string("collected_count")),
                comments = parseCount(interact?.string("comment_count")),
                shares = parseCount(interact?.string("share_count")),
            ),
            emotes = Emotes.used(emotes, title, summary),
            url = XhsParser.noteUrl(id, ref.xsecToken, ref.xsecSource ?: "pc_feed"),
            detail = DetailTarget.Native(XhsParser.nativeId(id, ref.xsecSource ?: "pc_feed", ref.xsecToken)),
        )
        // 图文笔记：图片先（轮播图的顺序），再正文；视频笔记：正文就是简介，视频放 FeedDetail.video
        val blocks = (if (isVideo) emptyList() else images) + XhsBody.blocks(desc, emotes, topics, mentions)
        return FeedDetail(
            item = item,
            body = RichText(blocks),
            video = if (isVideo) videoInfo(note, duration) else null,
        )
    }

    /** 笔记里的一张图。`url_default` 是带日期签名的默认图（webp），`file_id` 对应原图 `sns-img-bd.xhscdn.com/<file_id>`。 */
    private fun image(o: JsonObject): Block.Image? {
        val url = (o.text("url_default") ?: o.text("url_pre") ?: o.text("url"))?.toHttps() ?: return null
        val original = o.text("file_id")?.let { "https://sns-img-bd.xhscdn.com/$it" }
        return Block.Image(url = url, width = o.int("width"), height = o.int("height"), original = original?.takeIf { it != url })
    }

    private fun videoInfo(note: JsonObject, duration: Int?): VideoInfo {
        val (w, h) = XhsVideo.size(note)
        return VideoInfo(parts = listOf(VideoPart(id = XhsVideo.MAIN, durationSec = duration, width = w, height = h)))
    }
}
