package feedhub.source.xiaohongshu

import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Stats
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.obj
import feedhub.core.parseCount
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import java.math.BigInteger
import kotlinx.serialization.json.JsonObject

/**
 * 小红书搜索（`POST /api/sns/web/v1/search/notes`，v1，`edith.xiaohongshu.com`）的**请求拼装 + 解析**（纯函数）。
 *
 * 口径和 Rust 那边（`rust/crates/feedhub-core/src/xhs/search.rs`）逐项一致，两边由
 * `XhsSearchEquivalenceTest` 用 MockWebServer 比对（请求逐字节 + 解析逐字段）——**这一家没有"搬进 Rust 之前的老
 * Kotlin 版"可以照抄**，所以这份 Kotlin 实现是新写的参照，不是历史遗留。
 *
 * 为什么是 v1/edith：`x-rap-param` 的请求哈希前缀写死 `//edith.xiaohongshu.com`（[XhsRap.API_PREFIX]），
 * v1 和它一致；
 * 换域名要单独批准。
 *
 * 只做「综合」排序、`note_type=0`（视频 / 图文都要）；用户 / 话题卡片丢掉（`FeedItem` 没有对应模型）。
 */
internal object XhsSearch {
    /** 搜索笔记的路径（v1，edith 域）。 */
    const val PATH = "/api/sns/web/v1/search/notes"

    /** 一页多少条（网页实抓的 `page_size`）。 */
    const val PAGE_SIZE = 20

    /** 只做「综合」；能力里也只声明这一档。 */
    const val SORT = "general"

    /** 不限类型（视频 / 图文都收）。 */
    const val NOTE_TYPE = 0

    /** 从搜索结果点进笔记时网页用的 `xsec_source`。 */
    const val SOURCE = "pc_search"

    /** `0-9a-z` 之后是大写 base36（`redbook` / `xhs-mcp` / `xiaohongshu-cli` 三家一致）。 */
    private const val ALPHABET36 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"

    /** 随机数上界：`1..=2147483646`（四个参考实现的 `ceil(0x7ffffffe * random())`）。 */
    const val RANDOM_BOUND = 2_147_483_647L

    /**
     * 一轮搜索的 `search_id`：`base36((nowMs << 64) + randomPart)`。
     *
     * 时间 / 随机数由调用方给（和 Rust 那边同一个式子、同一个字母表），所以同一个 `(nowMs, randomPart)`
     * 一定得到同一个 id——`x-rap-param` 的补位长度和真正的请求要用同一对值。
     */
    fun searchId(nowMs: Long, randomPart: Long): String {
        val num = BigInteger.valueOf(nowMs).shiftLeft(64).add(BigInteger.valueOf(randomPart))
        if (num.signum() == 0) return "0"
        val base = BigInteger.valueOf(36)
        val out = StringBuilder()
        var left = num
        while (left.signum() > 0) {
            val (q, r) = left.divideAndRemainder(base)
            out.append(ALPHABET36[r.toInt()])
            left = q
        }
        return out.reverse().toString()
    }

    /** 游标：`<search_id>|<下一页页码>`。 */
    fun encodeCursor(searchId: String, page: Int): Cursor = Cursor("$searchId|$page")

    /**
     * 游标 → (search_id, 页码)。`cursor` 是 null / 空串 → (null, 1)：新一轮搜索，id 现造。
     * 解不出来（没有竖线 / 页码不是正整数）就当第一页、id 用整串——和别家的游标一样宽容，不抛错。
     */
    fun decodeCursor(cursor: Cursor?): Pair<String?, Int> {
        val text = cursor?.value?.takeIf { it.isNotEmpty() } ?: return null to 1
        val id = text.substringBefore('|')
        val page = text.substringAfter('|', "").toIntOrNull()?.takeIf { it >= 1 } ?: 1
        return id to page
    }

    /** 搜索的请求体（字段与顺序照浏览器实抓；**不带 `filters`**，`session_id` 只有 v2 用）。 */
    fun body(keyword: String, page: Int, searchId: String): String =
        "{\"keyword\":${XhsCodec.jsonString(keyword)},\"page\":${page.coerceAtLeast(1)},\"page_size\":$PAGE_SIZE," +
            "\"search_id\":${XhsCodec.jsonString(searchId)},\"sort\":\"$SORT\",\"note_type\":$NOTE_TYPE," +
            "\"ext_flags\":[],\"geo\":\"\",\"image_formats\":[\"jpg\",\"webp\",\"avif\"]}"

    /**
     * `POST search/notes` 的响应：`data.{items[], has_more}`。
     *
     * [page] / [searchId] 是这一次请求的页码和当前搜索的 id（下一页的游标要用）；
     * [emotes] 是表情表（标题里真有记号才放进 [FeedItem.emotes]）。
     */
    fun parse(
        json: JsonObject,
        page: Int,
        searchId: String,
        emotes: Map<String, String> = emptyMap(),
        nowSec: Long = System.currentTimeMillis() / 1000,
    ): XhsSearchPage {
        val data = json.obj("data") ?: return XhsSearchPage(emptyList(), null, "")
        val raw = data.arr("items").orEmpty()
        val items = raw.mapNotNull { (it as? JsonObject)?.let { o -> card(o, emotes, nowSec) } }
        val hasMore = data.bool("has_more") == true && raw.isNotEmpty()
        return XhsSearchPage(
            items = items,
            next = if (hasMore) encodeCursor(searchId, page + 1) else null,
            titles = raw.mapNotNull { (it as? JsonObject)?.obj("note_card")?.let { c -> c.text("display_title") ?: c.text("title") } }
                .joinToString("\n"),
        )
    }

    /**
     * 搜索结果里的一张卡片：形状和推荐流的卡片一样，但标题在搜索里可能是 `display_title`、也可能只有 `title`；
     * 发布时间可能带 `time`（毫秒），没有就退回笔记 id 前 8 位（同推荐流）。
     */
    private fun card(item: JsonObject, emotes: Map<String, String>, nowSec: Long): FeedItem? {
        if (item.text("model_type").let { it != null && it != "note" }) return null
        if (item.bool("ignore") == true) return null
        val id = item.text("id") ?: return null
        val card = item.obj("note_card") ?: return null
        val token = item.text("xsec_token")?.takeIf { it.isNotEmpty() }
        val isVideo = card.text("type") == "video"
        val title = card.text("display_title") ?: card.text("title")
        val cover = card.obj("cover")
        val coverUrl = (cover?.text("url_default") ?: cover?.text("url_pre") ?: cover?.text("url"))?.toHttps()
        val width = cover?.int("width")
        val height = cover?.int("height")
        val media = buildList {
            if (isVideo) add(Media.Video(coverUrl, card.obj("video")?.obj("capa")?.int("duration"), width, height))
            else if (coverUrl != null) add(Media.Image(coverUrl, width, height))
        }
        val publishedAt = (card.text("time")?.toLongOrNull() ?: item.text("time")?.toLongOrNull())
            ?.takeIf { it > 0 }?.let { it / 1000 }
            ?: XhsParser.idTime(id, nowSec)
        // 搜索卡片的 interact_info 比推荐流全，
        // 数字都是 "1.3万" 这种文本，统一走 parseCount（去掉封顶写法里的 `+`）。
        val interact = card.obj("interact_info")
        fun count(key: String): Long? = parseCount(interact?.string(key)?.trim()?.trimEnd('+'))
        return FeedItem(
            platform = XHS,
            id = id,
            kind = if (isVideo) ItemKind.VIDEO else ItemKind.POST,
            title = title,
            author = card.obj("user")?.let(XhsParser::user),
            publishedAt = publishedAt,
            media = media,
            stats = Stats(
                likes = count("liked_count"),
                comments = count("comment_count"),
                favorites = count("collected_count"),
                shares = count("shared_count"),
            ),
            emotes = Emotes.used(emotes, title),
            url = XhsParser.noteUrl(id, token, SOURCE),
            detail = DetailTarget.Native(XhsParser.nativeId(id, SOURCE, token)),
        )
    }
}
