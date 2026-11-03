package feedhub.source.coolapk

import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.arr
import feedhub.core.obj
import feedhub.core.string
import kotlinx.serialization.json.JsonObject

/**
 * 酷安搜索（`GET /v6/search`）。纯函数 + 参数拼装，单测直接喂夹具。
 *
 * 参数照 c001apk（`ApiService.kt:61-73`、`SearchContentViewModel.kt:86-95`）和 Coolapk-UWP
 * （`UriHelper.cs:198-204`、`SearchingViewModel.cs:155-177`）：
 * `type=feed`、`feedType=all`、`sort`（`none` / `dateline` / `hot` / `reply`）、`searchValue`、`page`、
 * `lastItem`（上一页最后一条的 id）、`showAnonymous=-1`。
 *
 * 响应 `{data:[实体]}` 和信息流同构（`entityType=feed`），所以直接复用 [CoolapkParser.feed]；
 * 卡片、用户、话题等实体丢掉。
 */
internal object CoolapkSearch {
    private val P = COOLAPK

    /** 一页的请求参数。 */
    fun params(request: SearchRequest, page: Int, lastItem: String?): List<Pair<String, String>> = buildList {
        add("type" to "feed")
        add("feedType" to "all")
        add("sort" to sortOf(request.sort))
        add("searchValue" to request.text)
        add("page" to page.toString())
        lastItem?.takeIf { it.isNotBlank() }?.let { add("lastItem" to it) }
        add("showAnonymous" to "-1")
    }

    /** 排序档 → `sort`：`none` 综合 / `dateline` 最新 / `hot` 最热 / `reply` 最多回复。 */
    fun sortOf(sort: SearchSort): String = when (sort) {
        SearchSort.RELEVANCE -> "none"
        SearchSort.LATEST -> "dateline"
        SearchSort.HOT -> "hot"
    }

    /**
     * 一页结果。游标是 `page|lastItem`：`page` 递增，`lastItem` 是这一页最后一条内容的 id
     * （c001apk 的做法；到最后一页时服务端回空 / 没有内容实体，[next] 就是 null）。
     */
    fun parse(json: JsonObject, page: Int): Page<FeedItem> {
        val data = json.arr("data") ?: throw ParseException(P, "搜索结果缺少 data 数组")
        val entities = data.mapNotNull { it as? JsonObject }
        val feeds = entities.filter { it.string("entityType") == "feed" }
        val items = feeds.mapNotNull(::feed)
        val last = feeds.mapNotNull { CoolapkParser.run { it.id() } }.lastOrNull()
        return Page(items, if (items.isEmpty() || last == null) null else Cursor(encode(page + 1, last)))
    }

    fun encode(page: Int, lastItem: String): String = "$page|$lastItem"

    /** 游标 → (page, lastItem)。解不出来就当第一页（不抛错，和 FeedCursor 一样宽容）。 */
    fun decode(cursor: Cursor?): Pair<Int, String?> {
        val text = cursor?.value ?: return 1 to null
        val page = text.substringBefore('|').toIntOrNull()?.coerceAtLeast(1) ?: 1
        val last = text.substringAfter('|', "").takeIf { it.isNotEmpty() }
        return page to last
    }

    private fun feed(o: JsonObject): FeedItem? = CoolapkParser.feed(o)
}
