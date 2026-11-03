package feedhub.source.bilibili

import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FollowKind
import feedhub.core.FollowState
import feedhub.core.FollowTarget
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/** 视频详情（`/x/web-interface/wbi/view/detail`）→ 统一模型。纯函数，单测喂 fixtures。 */
internal object BiliVideoParser {
    private val P = BILIBILI

    /** 弹幕每段 6 分钟。 */
    const val DANMAKU_SEGMENT_MS = 6L * 60 * 1000

    /**
     * `view/detail` 一次返回 `View`（标题、分 P、计数）、`Card`（UP 主资料和"我是否已关注"）、`Related`（相关推荐）。
     * [loggedIn] 为 false 时 `Card.following` 一律是 false（不是真的没关注），所以此时 `follow.following` 给 null。
     */
    fun detail(json: JsonObject, loggedIn: Boolean): FeedDetail {
        val data = json.obj("data") ?: throw ParseException(P, "视频详情缺少 data")
        val view = data.obj("View") ?: throw ParseException(P, "视频详情缺少 data.View")
        val item = videoCard(view) ?: throw ParseException(P, "视频详情缺少 bvid")
        return FeedDetail(
            item = item.copy(summary = description(view)),
            body = RichText(descriptionBlocks(view)),
            video = VideoInfo(parts = parts(view), danmakuSegmentMs = DANMAKU_SEGMENT_MS),
            follow = follow(data.obj("Card"), item.author, loggedIn),
            related = data.arr("Related").orEmpty().mapNotNull { it.obj()?.let(::videoCard) },
        )
    }

    /**
     * 一张视频卡片 → [FeedItem]。推荐流的 `item`、详情的 `View`、`Related` 里的条目字段基本一致
     * （`bvid` / `title` / `pic` / `duration` / `pubdate` / `owner` / `stat`），所以共用一个函数。
     * 番剧（`is_ogv`）没有原生详情，返回 null。
     */
    fun videoCard(o: JsonObject): FeedItem? {
        if (o.bool("is_ogv") == true) return null
        val bvid = o.text("bvid") ?: return null
        val stat = o.obj("stat")
        return FeedItem(
            platform = P,
            id = bvid,
            kind = ItemKind.VIDEO,
            title = o.text("title"),
            author = o.obj("owner")?.let { BiliParser.user(it.string("mid"), it.text("name"), it.text("face")) },
            publishedAt = o.long("pubdate")?.takeIf { it > 0 },
            media = listOf(Media.Video(cover = o.text("pic")?.toHttps(), durationSec = o.int("duration"))),
            stats = Stats(
                views = stat?.long("view"),
                likes = stat?.long("like"),
                comments = stat?.long("reply"),
                shares = stat?.long("share"),
                danmaku = stat?.long("danmaku"),
                favorites = stat?.long("favorite"),
                coins = stat?.long("coin"),
            ),
            reason = o.obj("rcmd_reason")?.text("content") ?: o.text("rcmd_reason"),
            url = BiliParser.videoUrl(bvid),
            detail = DetailTarget.Native(bvid),
        )
    }

    // ---------- 分 P ----------

    /** 没有 `pages` 时（极少见）退回只有 `cid` 的单 P。 */
    private fun parts(view: JsonObject): List<VideoPart> {
        val pages = view.arr("pages").orEmpty().mapNotNull { it.obj() }
        if (pages.isEmpty()) {
            val cid = view.string("cid")?.takeIf { it != "0" } ?: return emptyList()
            return listOf(part(cid, view.text("title"), view.int("duration"), view.obj("dimension")))
        }
        return pages.mapNotNull { p ->
            val cid = p.string("cid")?.takeIf { it.isNotEmpty() && it != "0" } ?: return@mapNotNull null
            part(cid, p.text("part"), p.int("duration"), p.obj("dimension"))
        }
    }

    /** `rotate` = 1 表示宽高要对调（竖屏视频按横屏编码）。 */
    private fun part(cid: String, title: String?, duration: Int?, dim: JsonObject?): VideoPart {
        var w = dim?.int("width")?.takeIf { it > 0 }
        var h = dim?.int("height")?.takeIf { it > 0 }
        if (dim?.int("rotate") == 1) w = h.also { h = w }
        return VideoPart(cid, title, duration, w, h)
    }

    // ---------- 简介 ----------

    /** 摘要用纯文本简介；"-" 是没写简介时的占位。 */
    private fun description(view: JsonObject): String? = view.text("desc")?.takeIf { it.trim() != "-" }

    /**
     * 简介 → 段落。`desc_v2` 是一串 `{raw_text, type, biz_id}`：type 1 文字，type 2 是 @（`biz_id` 是 mid，`raw_text` 是名字）。
     * 老视频 `desc_v2` 可能是 null，那就用 `desc`。文字里的网址和 BV 号变成链接。
     */
    private fun descriptionBlocks(view: JsonObject): List<Block> {
        val nodes = view.arr("desc_v2").orEmpty().mapNotNull { it.obj() }
        val spans: List<Span> = if (nodes.isEmpty()) {
            val text = description(view)?.let(BiliText::normalize) ?: return emptyList()
            BiliText.spans(text, BiliText.linkTokens(text))
        } else {
            nodes.flatMap { n ->
                val raw = BiliText.normalize(n.string("raw_text").orEmpty())
                val mid = n.string("biz_id")?.takeIf { it != "0" }
                if (n.int("type") == 2 && mid != null && raw.isNotEmpty()) {
                    listOf(Span.Mention("@" + raw.removePrefix("@"), UserRef(P, mid, raw.removePrefix("@"), url = BiliParser.spaceUrl(mid))))
                } else {
                    BiliText.spans(raw, BiliText.linkTokens(raw))
                }
            }
        }
        return BiliText.paragraphs(spans)
    }

    // ---------- UP 主 ----------

    private fun follow(card: JsonObject?, author: UserRef?, loggedIn: Boolean): FollowState? {
        val c = card?.obj("card")
        val mid = c?.string("mid")?.takeIf { it != "0" && it.isNotEmpty() } ?: author?.id ?: return null
        val name = c?.text("name") ?: author?.name ?: mid
        val target = FollowTarget(
            platform = P,
            kind = FollowKind.USER,
            id = mid,
            name = name,
            avatar = (c?.text("face") ?: author?.avatar)?.toHttps(),
            description = c?.text("sign"),
            url = BiliParser.spaceUrl(mid),
        )
        return FollowState(target, if (loggedIn) card?.bool("following") else null)
    }
}
