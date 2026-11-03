package feedhub.source.xiaohongshu

import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Platform
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.obj
import feedhub.core.parseCount
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/** 响应 JSON → 统一模型的纯函数。 */
internal object XhsParser {
    private val P = XHS

    /** `user/me` 的响应。游客态（`guest`）或没有 user_id 返回 null。 */
    fun me(json: JsonObject): UserRef? {
        val d = json.obj("data") ?: return null
        if (d.bool("guest") == true) return null
        val id = d.text("user_id") ?: return null
        return UserRef(
            platform = P,
            id = id,
            name = d.text("nickname") ?: d.text("red_id") ?: id,
            avatar = (d.text("images") ?: d.text("imageb") ?: d.text("image") ?: d.text("avatar"))?.toHttps(),
            url = userUrl(id),
        )
    }

    fun userUrl(id: String) = "https://www.xiaohongshu.com/user/profile/$id"

    /** 电脑版笔记页。带 `xsec_token` 才打得开（不带会跳到 /404/sec_…），所以分享链接也带上。 */
    fun noteUrl(id: String, xsecToken: String?, xsecSource: String = "pc_feed"): String =
        "https://www.xiaohongshu.com/explore/$id" +
            if (xsecToken.isNullOrEmpty()) "" else "?xsec_token=${XhsApi.encodeComponent(xsecToken)}&xsec_source=$xsecSource"

    // ---------- 笔记 id ----------

    /**
     * 笔记 id 是 24 位十六进制（和 MongoDB ObjectId 同构），前 8 位是创建时间（Unix 秒）。
     * 它 ≤ 笔记的发布时间 `time`，实测相差 0 到约一天（定时发布和先存草稿再发的会更大）；
     * 推荐流的卡片没有发布时间，逐条调详情又会触发风控，所以信息流先用它，打开详情后再用真实的 `time`。
     */
    fun idTime(noteId: String, nowSec: Long = System.currentTimeMillis() / 1000): Long? {
        if (noteId.length != 24) return null
        val t = noteId.substring(0, 8).toLongOrNull(16) ?: return null
        return t.takeIf { it in 1_356_998_400L..(nowSec + 86_400) }
    }

    // ---------- 信息流 ----------

    /** [FeedItem.detail] 里的 id：`<笔记 id>@<xsec_source>@<xsec_token>`（后两段详情接口要用，卡片里自带）。 */
    fun nativeId(noteId: String, xsecSource: String, xsecToken: String?): String =
        if (xsecToken.isNullOrEmpty()) noteId else "$noteId@$xsecSource@$xsecToken"

    /** [nativeId] 的逆运算。只有笔记 id 时 source / token 为 null。 */
    class NoteRef(val noteId: String, val xsecSource: String?, val xsecToken: String?)

    fun parseNativeId(id: String): NoteRef {
        val parts = id.split('@', limit = 3)
        return if (parts.size == 3) NoteRef(parts[0], parts[1].ifEmpty { null }, parts[2].ifEmpty { null }) else NoteRef(parts[0], null, null)
    }

    class HomefeedPage(
        val items: List<FeedItem>,
        /** 下一页要带的 `cursor_score`；响应里没有就是 null（没有下一页）。 */
        val cursorScore: String?,
        /** 响应里的条目总数（包括被丢掉的），翻页时 `note_index` 累加它。 */
        val rawCount: Int,
    )

    /** 这一页所有卡片的标题拼在一起，用来检查有没有表情记号。 */
    fun homefeedTitles(json: JsonObject): String = json.obj("data")?.arr("items").orEmpty()
        .mapNotNull { (it as? JsonObject)?.obj("note_card")?.text("display_title") }.joinToString("\n")

    /**
     * `POST homefeed` 的响应：`data.{cursor_score, items[]}`。只收 `model_type == "note"` 的卡片。
     * [emotes] 是表情表（`[笑哭R]` → 图片地址），只挑标题里真有的放进 [FeedItem.emotes]。
     */
    fun homefeed(
        json: JsonObject,
        emotes: Map<String, String> = emptyMap(),
        xsecSource: String = "pc_feed",
        /** 判断笔记 id 里的创建时间像不像真的时用的"现在"（秒）。测试里固定，Rust 侧同样由调用方传。 */
        nowSec: Long = System.currentTimeMillis() / 1000,
    ): HomefeedPage {
        val data = json.obj("data") ?: return HomefeedPage(emptyList(), null, 0)
        val raw = data.arr("items").orEmpty()
        val items = raw.mapNotNull { (it as? JsonObject)?.let { o -> homefeedCard(o, emotes, xsecSource, nowSec) } }
        return HomefeedPage(items, data.text("cursor_score"), raw.size)
    }

    private fun homefeedCard(
        item: JsonObject,
        emotes: Map<String, String>,
        xsecSource: String,
        nowSec: Long,
    ): FeedItem? {
        if (item.text("model_type").let { it != null && it != "note" }) return null
        if (item.bool("ignore") == true) return null
        val id = item.text("id") ?: return null
        val card = item.obj("note_card") ?: return null
        val token = item.text("xsec_token")
        val isVideo = card.text("type") == "video"
        val title = card.text("display_title")
        val cover = card.obj("cover")
        val coverUrl = (cover?.text("url_default") ?: cover?.text("url_pre") ?: cover?.text("url"))?.toHttps()
        val width = cover?.int("width")
        val height = cover?.int("height")
        val media = buildList {
            if (isVideo) add(Media.Video(coverUrl, card.obj("video")?.obj("capa")?.int("duration"), width, height))
            else if (coverUrl != null) add(Media.Image(coverUrl, width, height))
        }
        val user = card.obj("user")?.let(::user)
        return FeedItem(
            platform = P,
            id = id,
            kind = if (isVideo) ItemKind.VIDEO else ItemKind.POST,
            title = title,
            author = user,
            publishedAt = idTime(id, nowSec),
            media = media,
            stats = Stats(likes = likes(card.obj("interact_info")?.string("liked_count"))),
            emotes = Emotes.used(emotes, title),
            url = noteUrl(id, token, xsecSource),
            detail = DetailTarget.Native(nativeId(id, xsecSource, token)),
        )
    }

    /** 点赞数文本："4673"、"1.3万"、"10万+"（超过 10 万的封顶写法，去掉 `+` 按 10 万算）。 */
    fun likes(text: String?): Long? = parseCount(text?.trim()?.trimEnd('+'))

    /** 卡片 / 详情里的作者：`user_id`、`nickname`（= `nick_name`）、`avatar`。 */
    fun user(o: JsonObject): UserRef? {
        val id = o.text("user_id") ?: return null
        return UserRef(
            platform = P,
            id = id,
            name = o.text("nickname") ?: o.text("nick_name") ?: id,
            avatar = o.text("avatar")?.toHttps(),
            url = userUrl(id),
        )
    }
}
