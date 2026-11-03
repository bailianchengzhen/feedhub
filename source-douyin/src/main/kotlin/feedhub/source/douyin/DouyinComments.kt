package feedhub.source.douyin

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Emotes
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/**
 * 评论解析：`GET /aweme/v1/web/comment/list/`（一级）和 `/comment/list/reply/`（楼中楼）。
 * 两个接口的评论项结构一样，共用一个 [comment]（纯函数，`DouyinCommentsTest` 喂 fixtures）。
 *
 * 见过的字段：`comments[]`，每项 `{cid text user{uid sec_uid nickname avatar_thumb}
 * create_time(秒) digg_count reply_comment_total ip_label stick_position level is_hot reply_comment text_extra
 * image_list video_list}`；顶层 `{comments cursor has_more total status_code}`。
 * `create_time` 是秒。这一页里 `reply_comment`（楼中楼预览）都是 null、
 * `image_list` / `video_list` 也都是 null —— 前者按"有就放 previewReplies"处理；
 * 形状按 `aweme/image_list` 的两种写法认，没有真实样本），`video_list` 仍然没见样本、不做。
 *
 * 楼中楼里 `reply_comment[]` 是平台捎带的几条预览（`previewReplies`），字段和一级评论一样。
 *
 * **置顶**：`stick_position` 是真实字段，但这一页 19 条全是 0，没有见过非 0 的样本；
 * [Comment.pinned] 按 "> 0 = 置顶" 处理（按字段名的理解，未经证实）。
 */
internal object DouyinComments {
    private val P = DOUYIN

    /** [next] 为 null 表示没有下一页。 */
    class CommentPage(val items: List<Comment>, val next: String?)

    /**
     * 这一页里所有评论正文（含楼中楼预览）。取表情表之前先扫它，避免拿整段 JSON 去匹配 `[xxx]`
     * （JSON 的方括号会误报，白花一个请求）。
     */
    fun texts(json: JsonObject): String {
        val out = StringBuilder()
        fun collect(o: JsonObject) {
            o.text("text")?.let { out.append(it).append('\n') }
            o.arr("reply_comment").orEmpty().forEach { (it as? JsonObject)?.let(::collect) }
        }
        json.arr("comments").orEmpty().forEach { (it as? JsonObject)?.let(::collect) }
        return out.toString()
    }

    fun page(json: JsonObject, emotes: Map<String, String>): CommentPage {
        val items = json.arr("comments").orEmpty().mapNotNull { (it as? JsonObject)?.let { o -> comment(o, emotes) } }
        val cursor = json.long("cursor")?.toString()
        val hasMore = json.bool("has_more") == true
        return CommentPage(items, cursor?.takeIf { hasMore && items.isNotEmpty() })
    }

    fun comment(o: JsonObject, emotes: Map<String, String>): Comment? {
        // 一级评论是 cid，楼中楼里有的是 reply_id
        val id = o.text("cid") ?: o.text("reply_id") ?: return null
        val text = o.text("text").orEmpty()
        val blocks = ArrayList<Block>(2)
        if (text.isNotEmpty()) blocks += Block.Paragraph(Emotes.split(text, emotes))
        // 评论配图
        blocks += o.arr("image_list").orEmpty().mapNotNull { (it as? JsonObject)?.let(::picture) }
        return Comment(
            platform = P,
            id = id,
            author = o.obj("user")?.let(::user),
            content = RichText(blocks),
            publishedAt = o.long("create_time")?.takeIf { it > 0 },
            likes = o.long("digg_count"),
            replyCount = o.long("reply_comment_total"),
            replyTo = replyTo(o),
            ipLocation = o.text("ip_label"),
            floor = null,
            // 置顶：真实响应里有 `stick_position`。
            // "> 0 就是置顶"是按字段名的理解，**没有见过非 0 的样本**，见 KDoc。
            pinned = (o.long("stick_position") ?: 0L) > 0L,
            previewReplies = o.arr("reply_comment").orEmpty()
                .mapNotNull { (it as? JsonObject)?.let { s -> comment(s, emotes) } },
        )
    }

    /**
     * 楼中楼里"回复 @xxx"：`reply_to_userid` / `reply_to_username` 是平铺的两个字段，
     * 没有头像也没有 sec_uid，所以 [UserRef.url] 留 null（不要去猜主页地址）。
     */
    private fun replyTo(o: JsonObject): UserRef? {
        val uid = o.text("reply_to_userid") ?: return null
        return UserRef(P, uid, o.text("reply_to_username") ?: uid, null, null)
    }

    private fun user(o: JsonObject): UserRef? {
        val secUid = o.text("sec_uid")
        val uid = o.text("uid")
        val id = secUid ?: uid ?: return null
        return UserRef(
            P,
            id,
            o.text("nickname") ?: id,
            o.obj("avatar_thumb")?.let(::avatar)?.toHttps(),
            secUid?.let(DouyinParser::userUrl),
        )
    }

    private fun avatar(o: JsonObject): String? =
        o.arr("url_list").orEmpty().firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }

    /**
     * 评论配图（`image_list[]`）。
     * 所以这里两种写法都认，只要有一种对得上就能画图：
     * - `{thumb_url:{url_list:[…]}, origin_url:{url_list:[…]}, width, height}`（评论配图常见写法）；
     * - `{url_list:[…], width, height}`（图文作品那种，直接平铺）。
     *
     * 显示用缩略图（`thumb_url` 优先，平铺的 `url_list` 次之），`original` 放原图（大图查看层用）。
     * 两种都没有就跳过，不猜地址。
     */
    private fun picture(o: JsonObject): Block.Image? {
        val flat = urlList(o)
        val small = urlList(o.obj("thumb_url")) ?: flat
        val full = urlList(o.obj("origin_url")) ?: urlList(o.obj("download_url")) ?: flat
        val url = (small ?: full)?.toHttps() ?: return null
        return Block.Image(
            url = url,
            width = o.int("width")?.takeIf { it > 0 },
            height = o.int("height")?.takeIf { it > 0 },
            original = full?.toHttps()?.takeIf { it != url },
        )
    }

    private fun urlList(o: JsonObject?): String? = o?.arr("url_list").orEmpty()
        .firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content?.takeIf(String::isNotBlank) }
}
