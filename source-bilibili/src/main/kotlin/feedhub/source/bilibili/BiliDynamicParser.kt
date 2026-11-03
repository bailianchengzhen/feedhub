package feedhub.source.bilibili

import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.obj
import feedhub.core.path
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import org.jsoup.parser.Parser

/**
 * 动态 / 图文（opus）/ 专栏 的详情。
 *
 * 两个接口配合：
 * - `/x/polymer/web-dynamic/v1/detail`：返回的 `data.item` 和信息流里的动态条目同一个结构（[BiliParser.dynamic] 直接能转），
 *   带 `basic.comment_type` / `comment_id_str`（评论区的类型和 oid）；转发动态的原动态在 `orig`。
 * - `/x/polymer/web-dynamic/v1/opus/detail`：只对图文 / 专栏有内容（转发、视频动态返回 `item: null` 加 `fallback`），
 *   `data.item.modules[]` 里 `MODULE_TYPE_CONTENT` 的 `paragraphs[]` 是全文——信息流和 v1/detail 里的 `opus.summary` 是截断的。
 *
 * 段落 `para_type`：1 文字，2 图片，3 分割线，4 引用，5 列表，6 链接卡片，7 代码，8 标题。
 */
internal object BiliDynamicParser {
    private val P = BILIBILI

    /** 评论区在哪：`type` 是 B站评论区类型（1 视频、11 图文、12 专栏、17 动态……），`oid` 是该类型下的资源 id。 */
    data class CommentTarget(val type: Int, val oid: String)

    /** 这条动态的正文在 `opus/detail` 里（图文 / 专栏）。 */
    fun isOpus(detail: JsonObject): Boolean =
        detail.path("data", "item", "modules", "module_dynamic", "major")?.string("type") == "MAJOR_TYPE_OPUS"

    /** `v1/detail` 的响应里取评论区。 */
    fun commentTarget(json: JsonObject): CommentTarget? {
        val basic = json.path("data", "item")?.obj("basic") ?: return null
        val type = basic.int("comment_type") ?: return null
        val oid = (basic.string("comment_id_str") ?: basic.string("rid_str"))?.takeIf { it.isNotEmpty() && it != "0" } ?: return null
        return CommentTarget(type, oid)
    }

    /**
     * 详情。[detail] 是 `v1/detail` 的响应；[opus] 是同一条的 `opus/detail` 响应，可以为 null 或 `item: null`
     * （这时正文退回动态自带的摘要和图片）。
     */
    fun detail(detail: JsonObject, opus: JsonObject?): FeedDetail {
        val raw = detail.path("data", "item") ?: throw ParseException(P, "动态详情缺少 data.item")
        val item = BiliParser.dynamic(raw) ?: throw ParseException(P, "动态详情里没有可用的内容")
        val modules = opus?.path("data", "item")?.arr("modules").orEmpty().mapNotNull { it.obj() }
        val paragraphs = modules.firstOrNull { it.string("module_type") == "MODULE_TYPE_CONTENT" }?.obj("module_content")?.arr("paragraphs")
        val blocks = if (paragraphs != null && paragraphs.isNotEmpty()) {
            // 图文的图片有两种放法：夹在段落里（para_type 2），或者放在顶部的相册 MODULE_TYPE_TOP（正文里就没有图了）
            val album = modules.firstOrNull { it.string("module_type") == "MODULE_TYPE_TOP" }
                ?.obj("module_top")?.obj("display")?.obj("album")?.arr("pics")
            pictures(album) + opusBlocks(paragraphs)
        } else {
            summaryBlocks(raw)
        }
        return FeedDetail(item = item, body = RichText(blocks))
    }

    // ---------- 摘要（没有 opus/detail 时） ----------

    /** 动态自带的文字 + 图片：转发 / 文字动态在 `desc`，图文在 `major.opus.summary` + `pics`，老式图片动态在 `major.draw.items`。 */
    private fun summaryBlocks(item: JsonObject): List<Block> {
        val dyn = item.path("modules", "module_dynamic") ?: return emptyList()
        val major = dyn.obj("major")
        val nodes = dyn.obj("desc")?.arr("rich_text_nodes")
            ?: major?.obj("opus")?.obj("summary")?.arr("rich_text_nodes")
        val (spans, images) = richNodes(nodes)
        val blocks = BiliText.paragraphs(spans).toMutableList<Block>()
        blocks += images
        major?.obj("opus")?.arr("pics").orEmpty().forEach { p ->
            p.obj()?.let { o -> o.text("url")?.let { blocks += Block.Image(it.toHttps(), o.int("width")?.takeIf { w -> w > 0 }, o.int("height")?.takeIf { h -> h > 0 }) } }
        }
        major?.obj("draw")?.arr("items").orEmpty().forEach { p ->
            p.obj()?.let { o -> o.text("src")?.let { blocks += Block.Image(it.toHttps(), o.int("width")?.takeIf { w -> w > 0 }, o.int("height")?.takeIf { h -> h > 0 }) } }
        }
        return blocks
    }

    // ---------- 富文本节点（信息流 / 详情的 rich_text_nodes，以及段落里的 rich） ----------

    /**
     * `rich_text_nodes` → Span，另把"查看图片"节点带的图片单独返回。
     * 节点 `type`：TEXT 文字；EMOJI 表情（`emoji.icon_url`，动态表情用 `gif_url` / `webp_url`，动图要会动）；AT（`rid` 是 mid）；TOPIC 话题（跳到搜索）；
     * WEB 网址、BV 视频、CV 专栏、OGV 番剧、VOTE 投票、MAIL 等带 `jump_url` 的都做成链接；
     * VIEW_PICTURE 的图片放进第二个返回值；GOODS（商品）、LOTTERY（抽奖）丢掉。
     */
    fun richNodes(nodes: JsonArray?): Pair<List<Span>, List<Block.Image>> {
        val spans = mutableListOf<Span>()
        val images = mutableListOf<Block.Image>()
        for (n in nodes.orEmpty()) {
            val o = n.obj() ?: continue
            if (o.string("type") == "RICH_TEXT_NODE_TYPE_VIEW_PICTURE") {
                images += pictures(o.arr("pics"))
                continue
            }
            richSpan(o)?.let { spans += it }
        }
        return spans to images
    }

    /** EMOJI 节点的图片：动态表情包的表情（`gif_url` / `webp_url` 非空）是会动的，用它；普通表情只有静态的 `icon_url`。 */
    private fun emojiUrl(e: JsonObject?): String? = (e?.text("gif_url") ?: e?.text("webp_url") ?: e?.text("icon_url"))?.toHttps()

    /**
     * 节点里的表情：文字代码（`[打call]`，同 `emoji.text` / 节点的 `text`）→ 图片地址（同 [richNodes]）。
     * 信息流卡片用：摘要是纯文本，里面的 `[打call]` 靠它换成图片，见 [feedhub.core.FeedItem.emotes]。
     */
    fun emoteTable(nodes: JsonArray?): Map<String, String> {
        val out = LinkedHashMap<String, String>()
        for (n in nodes.orEmpty()) {
            val o = n.obj() ?: continue
            if (o.string("type") != "RICH_TEXT_NODE_TYPE_EMOJI") continue
            val e = o.obj("emoji")
            val code = e?.text("text") ?: o.text("text") ?: continue
            emojiUrl(e)?.let { out.putIfAbsent(code, it) }
        }
        return out
    }

    private fun pictures(pics: JsonArray?): List<Block.Image> = pics.orEmpty().mapNotNull { p ->
        val o = p.obj() ?: return@mapNotNull null
        val url = (o.text("src") ?: o.text("url"))?.toHttps() ?: return@mapNotNull null
        Block.Image(url, o.int("width")?.takeIf { it > 0 }, o.int("height")?.takeIf { it > 0 })
    }

    /** 一个 rich 节点 → Span；丢弃的类型返回 null。 */
    private fun richSpan(o: JsonObject): Span? {
        val text = o.string("text") ?: o.string("orig_text") ?: ""
        val url = o.text("jump_url")?.toHttps()?.takeIf { it.startsWith("http://") || it.startsWith("https://") }
        return when (o.string("type")) {
            "RICH_TEXT_NODE_TYPE_TEXT" -> text.takeIf { it.isNotEmpty() }?.let { Span.Text(it) }
            "RICH_TEXT_NODE_TYPE_EMOJI" -> {
                val e = o.obj("emoji")
                val icon = emojiUrl(e)
                when {
                    icon != null -> Span.Emote(e?.text("text") ?: text, icon)
                    text.isNotEmpty() -> Span.Text(text)
                    else -> null
                }
            }
            "RICH_TEXT_NODE_TYPE_AT" -> {
                val mid = o.text("rid")?.takeIf { it != "0" }
                if (mid != null) Span.Mention(text, UserRef(P, mid, text.removePrefix("@"), url = BiliParser.spaceUrl(mid)))
                else text.takeIf { it.isNotEmpty() }?.let { Span.Text(it) }
            }
            "RICH_TEXT_NODE_TYPE_GOODS", "RICH_TEXT_NODE_TYPE_LOTTERY" -> null
            "RICH_TEXT_NODE_TYPE_BV" -> {
                val bvid = o.text("rid")?.takeIf(BvAv::isBvid)
                val link = url ?: bvid?.let { BiliParser.videoUrl(it) }
                if (link != null && text.isNotEmpty()) Span.Link(text, link, bvid?.let { DetailTarget.Native(it) } ?: BiliLinks.target(link))
                else text.takeIf { it.isNotEmpty() }?.let { Span.Text(it) }
            }
            else -> when {
                // 其余带链接的（网址、专栏、番剧、投票、话题 TOPIC……）都做成链接，站内视频 / 动态能原生打开
                url != null && text.isNotEmpty() -> Span.Link(text, url, BiliLinks.target(url))
                text.isNotEmpty() -> Span.Text(text)
                else -> null
            }
        }
    }

    // ---------- opus 段落 ----------

    fun opusBlocks(paragraphs: JsonArray): List<Block> = paragraphs.mapNotNull { it.obj() }.flatMap(::paragraph)

    private val HEADING_LEVELS = setOf("large", "xLarge", "xxLarge", "xxxLarge")

    private fun paragraph(p: JsonObject): List<Block> = when (p.int("para_type")) {
        1 -> {
            val nodes = p.obj("text")?.arr("nodes")
            val heading = nodes.orEmpty().any { it.obj()?.obj("word")?.string("font_level") in HEADING_LEVELS }
            val (spans, images) = textNodes(nodes)
            val paras = BiliText.paragraphs(spans)
            (if (heading) paras.map { Block.Heading(it.spans) } else paras) + images
        }
        4 -> BiliText.paragraphs(textNodes(p.obj("text")?.arr("nodes")).first).map { Block.Quote(it.spans) }
        8 -> BiliText.paragraphs(textNodes(p.obj("heading")?.arr("nodes")).first).map { Block.Heading(it.spans) }
        2 -> pictures(p.obj("pic")?.arr("pics") ?: p.obj("pic")?.let { JsonArray(listOf(it)) })
        3 -> listOf(Block.Divider)
        5 -> list(p.obj("list"))
        6 -> listOfNotNull(p.obj("link_card")?.obj("card")?.let(::linkCard))
        7 -> p.obj("code")?.text("content")?.let {
            // 代码被 B站 HTML 转义过，引号还转了两道（&amp;#39;），语言写成 language-javascript
            listOf(Block.Code(unescape(it), p.obj("code")?.text("lang")?.removePrefix("language-")))
        }.orEmpty()
        else -> emptyList()
    }

    private fun unescape(s: String): String {
        var cur = s
        repeat(3) {
            val next = Parser.unescapeEntities(cur, false)
            if (next == cur) return cur
            cur = next
        }
        return cur
    }

    /** 段落里的节点：WORD 是带样式的文字，RICH 同信息流的 rich 节点，FORMULA 是 LaTeX（保留源码）。 */
    private fun textNodes(nodes: JsonArray?): Pair<List<Span>, List<Block.Image>> {
        val spans = mutableListOf<Span>()
        val images = mutableListOf<Block.Image>()
        for (n in nodes.orEmpty()) {
            val o = n.obj() ?: continue
            when (o.string("type")) {
                "TEXT_NODE_TYPE_WORD" -> o.obj("word")?.let { w ->
                    val words = w.string("words").orEmpty()
                    if (words.isNotEmpty()) {
                        val style = w.obj("style")
                        spans += Span.Text(words, bold = style?.bool("bold") == true, italic = style?.bool("italic") == true)
                    }
                }
                "TEXT_NODE_TYPE_RICH" -> o.obj("rich")?.let { r ->
                    if (r.string("type") == "RICH_TEXT_NODE_TYPE_VIEW_PICTURE") images += pictures(r.arr("pics"))
                    else richSpan(r)?.let { spans += it }
                }
                "TEXT_NODE_TYPE_FORMULA" -> o.obj("formula")?.text("latex_content")?.let { spans += Span.Text(it) }
            }
        }
        return spans to images
    }

    /** 列表项变成带 "• " / "1. " 前缀的段落，嵌套层级用两个空格缩进。 */
    private fun list(l: JsonObject?): List<Block> {
        val ordered = l?.string("theme") == "number" || l?.int("style") == 1
        return l?.arr("items").orEmpty().mapNotNull { it.obj() }.mapIndexedNotNull { i, item ->
            val (spans, _) = textNodes(item.arr("nodes"))
            if (spans.isEmpty()) return@mapIndexedNotNull null
            val indent = "  ".repeat(((item.int("level") ?: 1) - 1).coerceAtLeast(0))
            val prefix = if (ordered) "${item.int("order") ?: (i + 1)}. " else "• "
            Block.Paragraph(listOf<Span>(Span.Text(indent + prefix)) + spans)
        }
    }

    /**
     * 链接卡片：视频（UGC）、通用卡片（COMMON）、直播、图文等变成 [Block.Card]；商品（GOODS）和打不开的（ITEM_NULL）丢掉。
     * 字段名照 PiliPlus `models/dynamics/article_content_model.dart`：卡片对象在以类型命名的子对象里（`ugc` / `common` / `live` / `opus` ……）。
     */
    private fun linkCard(card: JsonObject): Block.Card? {
        val type = card.string("type") ?: return null
        if (type.endsWith("GOODS") || type.endsWith("ITEM_NULL")) return null
        val sub = card.obj(type.removePrefix("LINK_CARD_TYPE_").lowercase()) ?: return null
        val title = sub.text("title") ?: return null
        val url = sub.text("jump_url")?.toHttps()?.takeIf { it.startsWith("http") }
        return Block.Card(
            title = title,
            subtitle = sub.text("desc_second") ?: sub.text("desc") ?: sub.text("desc1"),
            cover = sub.text("cover")?.toHttps(),
            url = url,
            target = url?.let(BiliLinks::target),
        )
    }
}
