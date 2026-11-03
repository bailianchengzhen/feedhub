package feedhub.source.xiaohongshu

import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

/**
 * 关注列表里的一个人（`followings/self` 的 `user_list[]`）。
 *
 * [xsecToken] 是列表项自带的：拉这个人的 `user_posted` 要用它（`xsec_source=pc_followed`），token 会过期，
 * 所以关注列表缓存最多一天（见 [XhsFollowFeed]）。
 */
@Serializable
internal data class FollowedAuthor(
    val id: String,
    val name: String,
    val avatar: String? = null,
    val xsecToken: String? = null,
)

/**
 * 关注流用到的三个接口的响应 → 模型。纯函数。字段：
 *
 * - `GET user/followings/self` → `data.{user_list[], tab_list[], cursor, has_more, total}`；用户项
 *   `{user_id, nickname, image, desc, follow_status, pin_status, fan_count, like_count, note_count, remark, xsec_token}`。
 *   `note_count` 返回 0（不可靠），所以不用；`has_more` 在最后一页之前可能就是 true。
 * - `GET user/followings/extra` → `data.follow_extra_list[] = {user_id, unread_note_count, desc:"2篇笔记未看", desc_style, resource_type:"UNREAD_COUNT"}`。
 * - `GET user_posted` → `data.{notes[], cursor, has_more}`；笔记项 `{note_id, type, display_title, cover, user, interact_info{liked_count, liked, sticky},
 *   time(毫秒), xsec_token}`。**有发布时间 `time`**（推荐流卡片没有）；置顶笔记排在最前面，带 `interact_info.sticky = true`，
 *   它们的 `time` 是自己真实的发布时间，可能比后面的普通笔记老得多。没有视频时长（详情里才有）。
 */
internal object XhsFollowParser {
    private val P = XHS

    /** [cursor] 是下一页要带的游标；[hasMore] 最后一页之前就可能是 true，翻页时要同时看是不是空页（见 [XhsFollowFeed]）。 */
    class FollowingsPage(val users: List<FollowedAuthor>, val cursor: String?, val hasMore: Boolean, val total: Int?)

    fun followings(json: JsonObject): FollowingsPage {
        val data = json.obj("data") ?: throw ParseException(P, "followings/self 缺少 data")
        val users = data.arr("user_list").orEmpty().mapNotNull { (it as? JsonObject)?.let(::followedAuthor) }
        return FollowingsPage(users, data.text("cursor"), data.bool("has_more") == true, data.int("total"))
    }

    private fun followedAuthor(o: JsonObject): FollowedAuthor? {
        val id = o.text("user_id") ?: return null
        return FollowedAuthor(
            id = id,
            name = o.text("nickname") ?: o.text("remark") ?: id,
            avatar = o.text("image")?.toHttps(),
            xsecToken = o.text("xsec_token"),
        )
    }

    /**
     * `followings/extra` 的响应：用户 id → 未看笔记数。只认 `resource_type == UNREAD_COUNT` 的项（没有这个字段也认）；
     * 请求里有、响应里没有的用户由调用方当作 0。
     */
    fun unread(json: JsonObject): Map<String, Int> {
        val list = json.obj("data")?.arr("follow_extra_list") ?: throw ParseException(P, "followings/extra 缺少 follow_extra_list")
        val out = HashMap<String, Int>()
        for (e in list) {
            val o = e as? JsonObject ?: continue
            val type = o.text("resource_type")
            if (type != null && type != "UNREAD_COUNT") continue
            val id = o.text("user_id") ?: continue
            out[id] = (o.int("unread_note_count") ?: 0).coerceAtLeast(0)
        }
        return out
    }

    class PostedNote(val item: FeedItem, val sticky: Boolean)

    /**
     * [hasMore]：这个人还有更早的笔记（关注流只拉第一页，不翻）。
     * [titles]：这一页所有笔记的标题拼串（[postedTitles]），调用方拿它判断要不要去取表情表。
     */
    class PostedPage(val notes: List<PostedNote>, val hasMore: Boolean, val titles: String = "")

    /** 这一页所有笔记的标题拼在一起，用来检查有没有表情记号。 */
    fun postedTitles(json: JsonObject): String = json.obj("data")?.arr("notes").orEmpty()
        .mapNotNull { (it as? JsonObject)?.text("display_title") }.joinToString("\n")

    /**
     * `user_posted` 的响应。[author] 是关注列表里的这个人：笔记项里的 `user` 缺失时用它；[emotes] 是表情表（`[笑哭R]` → 图片地址）。
     * 笔记的 `xsec_source` 用 `pc_user`（从作者主页点进笔记时网页的值），token 是笔记项自己的。
     * 没有发布时间的笔记（响应里没有 `time`，笔记 id 也推不出）丢掉：关注流按发布时间排，没有时间排不了。
     */
    fun posted(
        json: JsonObject,
        author: FollowedAuthor,
        emotes: Map<String, String> = emptyMap(),
        /** 笔记没有 `time` 时用 id 里的创建时间，判断它像不像真的要用"现在"（秒）。Rust 侧同样由调用方传。 */
        nowSec: Long = System.currentTimeMillis() / 1000,
    ): PostedPage {
        val data = json.obj("data") ?: throw ParseException(P, "user_posted 缺少 data")
        val notes = data.arr("notes").orEmpty().mapNotNull { (it as? JsonObject)?.let { o -> postedNote(o, author, emotes, nowSec) } }
        return PostedPage(notes, data.bool("has_more") == true)
    }

    const val SOURCE_USER = "pc_user"

    private fun postedNote(o: JsonObject, author: FollowedAuthor, emotes: Map<String, String>, nowSec: Long): PostedNote? {
        val id = o.text("note_id") ?: return null
        val publishedAt = o.long("time")?.takeIf { it > 0 }?.let { it / 1000 } ?: XhsParser.idTime(id, nowSec) ?: return null
        val token = o.text("xsec_token")
        val isVideo = o.text("type") == "video"
        val title = o.text("display_title")
        val cover = o.obj("cover")
        val coverUrl = (cover?.text("url_default") ?: cover?.text("url_pre") ?: cover?.text("url"))?.toHttps()
        val width = cover?.int("width")
        val height = cover?.int("height")
        val interact = o.obj("interact_info")
        val user = o.obj("user")?.let(XhsParser::user)
            ?: UserRef(P, author.id, author.name, author.avatar, XhsParser.userUrl(author.id))
        val item = FeedItem(
            platform = P,
            id = id,
            kind = if (isVideo) ItemKind.VIDEO else ItemKind.POST,
            title = title,
            author = user,
            publishedAt = publishedAt,
            media = buildList {
                if (isVideo) add(Media.Video(coverUrl, null, width, height))
                else if (coverUrl != null) add(Media.Image(coverUrl, width, height))
            },
            stats = Stats(likes = XhsParser.likes(interact?.string("liked_count"))),
            emotes = Emotes.used(emotes, title),
            url = XhsParser.noteUrl(id, token, SOURCE_USER),
            detail = DetailTarget.Native(XhsParser.nativeId(id, SOURCE_USER, token)),
        )
        return PostedNote(item, sticky = interact?.bool("sticky") == true)
    }
}
