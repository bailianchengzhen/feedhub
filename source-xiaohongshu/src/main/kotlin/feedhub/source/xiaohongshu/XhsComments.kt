package feedhub.source.xiaohongshu

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.parseCount
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * 评论：`GET /api/sns/web/v2/comment/page`（一级）和 `/comment/sub/page`（楼中楼）的响应。
 * 字段：`data.{comments[], cursor, has_more, time, xsec_token, user_id}`；评论
 * `{id content create_time(毫秒) like_count ip_location pictures[] at_users[] show_tags[] sub_comment_count sub_comment_cursor
 * sub_comment_has_more sub_comments[] user_info{user_id nickname image} status invalid}`，楼中楼多一个 `target_comment{id user_info}`（回复的是谁）。
 * `data.user_id` 是**当前登录账号**，不要存进 fixtures。
 */
internal object XhsComments {
    private val P = XHS

    /** [next] 为 null 表示没有下一页。 */
    class CommentPage(val items: List<Comment>, val next: String?)

    fun page(json: JsonObject, emotes: Map<String, String>): CommentPage {
        val data = json.obj("data") ?: return CommentPage(emptyList(), null)
        val items = data.arr("comments").orEmpty().mapNotNull { (it as? JsonObject)?.let { o -> comment(o, emotes) } }
        val cursor = data.text("cursor")
        val hasMore = data.bool("has_more") == true
        return CommentPage(items, cursor?.takeIf { hasMore && items.isNotEmpty() })
    }

    fun comment(o: JsonObject, emotes: Map<String, String>): Comment? {
        if (o.bool("invalid") == true) return null
        val id = o.text("id") ?: return null
        val mentions = o.arr("at_users").orEmpty().mapNotNull { (it as? JsonObject)?.let(::user) }
        val blocks = XhsBody.blocks(o.string("content").orEmpty(), emotes, emptyMap(), mentions) +
            o.arr("pictures").orEmpty().mapNotNull { (it as? JsonObject)?.let(::picture) }
        val tags = o.arr("show_tags").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content }
        return Comment(
            platform = P,
            id = id,
            author = o.obj("user_info")?.let(::user),
            content = RichText(blocks),
            publishedAt = o.long("create_time")?.let { it / 1000 },
            likes = parseCount(o.string("like_count")),
            replyCount = o.string("sub_comment_count")?.let(::parseCount),
            replyTo = o.obj("target_comment")?.obj("user_info")?.let(::user),
            ipLocation = o.text("ip_location"),
            pinned = tags.any { it in PINNED_TAGS },
            previewReplies = o.arr("sub_comments").orEmpty().mapNotNull { (it as? JsonObject)?.let { s -> comment(s, emotes) } },
        )
    }

    /** 置顶评论的标记。录到的 `show_tags` 里只见过 `is_author`（作者回复），置顶的写法没见到，这几个是按常见写法猜的。 */
    private val PINNED_TAGS = setOf("top", "is_top", "pinned")

    private fun user(o: JsonObject): UserRef? {
        val id = o.text("user_id") ?: return null
        return UserRef(P, id, o.text("nickname") ?: id, (o.text("image") ?: o.text("avatar"))?.toHttps(), XhsParser.userUrl(id))
    }

    private fun picture(o: JsonObject): Block.Image? {
        val url = (o.text("url_default") ?: o.text("url_pre") ?: o.text("url"))?.toHttps() ?: return null
        return Block.Image(url = url, width = o.int("width"), height = o.int("height"))
    }
}
