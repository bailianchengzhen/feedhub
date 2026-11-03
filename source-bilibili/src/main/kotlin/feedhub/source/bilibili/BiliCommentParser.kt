package feedhub.source.bilibili

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import java.net.URLEncoder

/**
 * 评论（`/x/v2/reply/wbi/main`）和楼中楼（`/x/v2/reply/reply`）→ [Comment]。
 *
 * 评论条目的结构（视频、动态、专栏的评论区都一样）：
 * - `content.message` 是纯文本，里面的 `[doge]` 对应 `content.emote` 的键，`@名字` 对应 `content.members`（`at_name_to_mid` 是同一份的 名字 → mid），
 *   `content.jump_url` 的键是文本里要变成链接的关键词（搜索词、BV 号、网址），`content.pictures` 是评论配图；
 * - 楼中楼里回复别人的评论，`message` 开头自带 `回复 @某某 :`（`parent` ≠ `root` 时），这里拆成 [Comment.replyTo]；
 * - `replies` 是楼中楼预览，`rcount` 是楼中楼总数，`reply_control.location` 是 `IP属地：xx`。
 */
internal object BiliCommentParser {
    private val P = BILIBILI
    private const val IP_PREFIX = "IP属地："

    /**
     * 一页一级评论。置顶评论在第一页的 `top_replies` 里（其余页没有），放在最前面并标 [Comment.pinned]。
     * 游标是 `cursor.pagination_reply.next_offset`，`cursor.is_end` 为 true 或没有 offset 就是到头了。
     */
    fun mainList(json: JsonObject): Page<Comment> {
        val data = json.obj("data") ?: throw ParseException(P, "评论缺少 data")
        val pinned = data.arr("top_replies").orEmpty().mapNotNull { it.obj()?.let { o -> comment(o, pinned = true) } }
        val pinnedIds = pinned.mapTo(HashSet()) { it.id }
        val normal = data.arr("replies").orEmpty().mapNotNull { it.obj()?.let { o -> comment(o) } }.filter { it.id !in pinnedIds }
        val cursor = data.obj("cursor")
        val next = cursor?.obj("pagination_reply")?.text("next_offset")
        val end = cursor?.bool("is_end") == true
        return Page(pinned + normal, if (end || next == null || (normal.isEmpty() && pinned.isEmpty())) null else Cursor(next))
    }

    /** 楼中楼的一页。页码从 1 开始，`page.count` 是总数。 */
    fun subReplies(json: JsonObject, pn: Int, ps: Int): Page<Comment> {
        val data = json.obj("data") ?: throw ParseException(P, "楼中楼缺少 data")
        val items = data.arr("replies").orEmpty().mapNotNull { it.obj()?.let { o -> comment(o) } }
        val total = data.obj("page")?.long("count") ?: 0L
        return Page(items, if (items.isNotEmpty() && pn.toLong() * ps < total) Cursor((pn + 1).toString()) else null)
    }

    /** 一条评论（一级或楼中楼）。不可见的（`invisible`）返回 null。 */
    fun comment(o: JsonObject, pinned: Boolean = false): Comment? {
        if (o.bool("invisible") == true) return null
        val id = (o.string("rpid_str") ?: o.string("rpid"))?.takeIf { it.isNotEmpty() && it != "0" } ?: return null
        val root = o.long("root") ?: 0L
        val parent = o.long("parent") ?: 0L
        val (content, replyTo) = content(o.obj("content"), replyPrefix = root != 0L && parent != root && parent != 0L)
        return Comment(
            platform = P,
            id = id,
            author = o.obj("member")?.let { BiliParser.user(it.string("mid"), it.text("uname"), it.text("avatar")) }
                ?: BiliParser.user(o.string("mid_str") ?: o.string("mid"), null, null),
            content = content,
            publishedAt = o.long("ctime")?.takeIf { it > 0 },
            likes = o.long("like"),
            replyCount = o.long("rcount"),
            replyTo = replyTo,
            ipLocation = o.obj("reply_control")?.text("location")?.removePrefix(IP_PREFIX)?.trim()?.takeIf { it.isNotEmpty() },
            pinned = pinned || o.obj("reply_control")?.bool("is_up_top") == true,
            previewReplies = o.arr("replies").orEmpty().mapNotNull { it.obj()?.let { r -> comment(r) } },
        )
    }

    /** 评论正文 → 富文本，以及 `回复 @某某 :` 开头拆出来的被回复的人。 */
    fun content(c: JsonObject?, replyPrefix: Boolean): Pair<RichText, UserRef?> {
        if (c == null) return RichText.EMPTY to null
        var text = BiliText.normalize(c.string("message").orEmpty())

        // @：名字 → 用户。members 里有头像，at_name_to_mid 只有 mid
        val users = LinkedHashMap<String, UserRef>()
        c.obj("at_name_to_mid")?.forEach { (name, mid) ->
            val m = (mid as? kotlinx.serialization.json.JsonPrimitive)?.content
            BiliParser.user(m, name, null)?.let { users[name] = it }
        }
        c.arr("members").orEmpty().forEach { m ->
            val o = m.obj() ?: return@forEach
            val name = o.text("uname") ?: return@forEach
            BiliParser.user(o.string("mid"), name, o.text("avatar"))?.let { users[name] = it }
        }

        var replyTo: UserRef? = null
        if (replyPrefix) {
            for ((name, user) in users.entries.sortedByDescending { it.key.length }) {
                val head = "回复 @$name"
                if (!text.startsWith(head)) continue
                replyTo = user
                text = text.removePrefix(head).trimStart().removePrefix(":").removePrefix("：").trimStart()
                break
            }
        }

        val tokens = mutableListOf<BiliText.Token>()
        c.obj("emote")?.forEach { (code, e) ->
            // 动态表情（表情包里的"动态"那几组、tv_ 系列）除了静态的 `url`（png）还有 `gif_url`，用 gif 才会动
            val url = e.obj()?.let { it.text("gif_url") ?: it.text("url") } ?: return@forEach
            tokens += BiliText.Token(code, Span.Emote(code, url.toHttps()))
        }
        for ((name, user) in users) tokens += BiliText.Token("@$name", Span.Mention("@$name", user))
        c.obj("jump_url")?.forEach { (keyword, j) ->
            linkSpan(keyword, j.obj())?.let { tokens += BiliText.Token(keyword, it, once = j.obj()?.bool("match_once") == true) }
        }

        val blocks = BiliText.paragraphs(BiliText.spans(text, tokens)).toMutableList<Block>()
        c.arr("pictures").orEmpty().forEach { p ->
            val o = p.obj() ?: return@forEach
            val url = o.text("img_src") ?: return@forEach
            blocks += Block.Image(url.toHttps(), o.int("img_width")?.takeIf { it > 0 }, o.int("img_height")?.takeIf { it > 0 })
        }
        return RichText(blocks) to replyTo
    }

    /**
     * `jump_url` 的一项 → 链接。键是评论里的原文，`title` 是该显示的文字（BV 号、短网址会换成视频标题 / "网页链接"）。
     * 搜索关键词的 `pc_url` 带一长串埋点参数，只留 keyword；没有 `pc_url` 的从键本身推（BV 号、网址）。
     */
    private fun linkSpan(keyword: String, j: JsonObject?): Span.Link? {
        val title = j?.text("title") ?: keyword
        val pc = j?.text("pc_url")?.toHttps()
        val url: String = when {
            pc != null -> {
                val u = pc.toHttpUrlOrNull()
                if (u != null && u.host == "search.bilibili.com") {
                    "https://search.bilibili.com/all?keyword=" + URLEncoder.encode(u.queryParameter("keyword") ?: keyword, "UTF-8").replace("+", "%20")
                } else pc
            }
            BvAv.isBvid(keyword) -> BiliParser.videoUrl(keyword)
            keyword.startsWith("http://") || keyword.startsWith("https://") -> keyword
            else -> return null
        }
        val target = if (BvAv.isBvid(keyword)) DetailTarget.Native(keyword) else BiliLinks.target(url)
        return Span.Link(title, url, target)
    }
}
