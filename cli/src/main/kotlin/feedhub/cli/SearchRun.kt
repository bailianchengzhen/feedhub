package feedhub.cli

import feedhub.brain.ContentFilter
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.brain.SearchMerge
import feedhub.brain.SearchPlan

/**
 * 一次跨平台搜索的结果（[collectMixedSearch] 的返回值）。
 *
 * 和 `AppGraph.search` 那边的 `SearchState` 算出来的东西是同一套规则（[SearchPlan] / [SearchMerge]），
 * 只是这边是**串行**取、取完一次给结论：cli 不需要超时 / 先到先上屏 / 点灭标签这些交互。
 */
internal class SearchOutcome(
    /** 混排、去重、按 limit 截过之后要输出的条目。 */
    val items: List<FeedItem>,
    /** 被 18+ / 内容过滤整条藏掉的条数。 */
    val hidden: Int,
    /** 有几家回了内容。 */
    val answered: Int,
    /** 每家下一次从哪翻（null = 到底了）。 */
    val next: Map<Platform, Cursor?>,
    /** 失败的那几家：异常原样留着，由命令层决定怎么报 / 写冷却。 */
    val failures: Map<Platform, Throwable>,
)

/**
 * `search` 的核心：按 [wanted] 里的页号取（1 起，`-n` 是 1..N，`--page` 是单独一页），
 * 每页各自按平台顺序轮转混排，再把几页接起来；跨页同一个 `key` 只留第一条。
 *
 * - 每家一页只取 [SearchPlan.quota] 条（**只有多家时才按配额截**；单平台把整页给出来）；
 * - 18+ / 内容过滤在进桶之前过（[SearchPlan.keep]）；
 * - [strict] = true（单平台）：出错直接抛给调用方；false（`all`）：记进 [SearchOutcome.failures] 接着问下一家；
 * - [beforePlatform] 在请求每一家**之前**调（命令层用它做冷却检查：冷却期一个请求都不发）。
 */
internal suspend fun collectMixedSearch(
    candidates: List<Pair<Platform, SearchSource>>,
    request: SearchRequest,
    wanted: List<Int>,
    cursor: Cursor?,
    rules: ContentFilter.Rules,
    limit: Int? = null,
    strict: Boolean = false,
    beforePlatform: (Platform) -> Unit = {},
): SearchOutcome {
    require(wanted.isNotEmpty()) { "wanted 不能为空" }
    val deepest = wanted.max()
    val order = candidates.map { it.first }
    val perPage = mutableMapOf<Int, MutableMap<Platform, List<FeedItem>>>()
    val nextByPlatform = LinkedHashMap<Platform, Cursor?>()
    val failures = LinkedHashMap<Platform, Throwable>()
    var hidden = 0
    var answered = 0

    for ((p, source) in candidates) {
        try {
            beforePlatform(p)
            var cur: Cursor? = cursor
            var pageNo = 1
            while (pageNo <= deepest) {
                val result = source.search(request, cur)
                val kept = SearchPlan.keep(result.items, rules)
                hidden += result.items.size - kept.size
                if (kept.isNotEmpty()) answered++
                if (pageNo in wanted) {
                    val quota = if (candidates.size > 1) SearchPlan.quota(candidates.size, source.search.pageSize) else kept.size
                    perPage.getOrPut(pageNo) { mutableMapOf() }[p] = kept.take(quota)
                }
                cur = result.next
                if (cur == null) break
                if (pageNo == deepest) break
                pageNo++
            }
            nextByPlatform[p] = cur
        } catch (e: Exception) {
            if (strict) throw e
            failures[p] = e
            nextByPlatform[p] = null
        }
    }

    val merged = ArrayList<FeedItem>()
    val keys = HashSet<String>()
    for (idx in wanted) {
        for (item in SearchMerge.roundRobin(perPage[idx].orEmpty(), order)) {
            if (keys.add(item.key)) merged += item
        }
    }
    return SearchOutcome(
        items = limit?.let { merged.take(it) } ?: merged,
        hidden = hidden,
        answered = answered,
        next = nextByPlatform,
        failures = failures,
    )
}
