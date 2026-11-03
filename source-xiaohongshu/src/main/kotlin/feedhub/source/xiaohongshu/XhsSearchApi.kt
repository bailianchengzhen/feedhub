package feedhub.source.xiaohongshu

import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.Page
import kotlin.random.Random

/**
 * 搜索"**取一页**"的接缝（照 [XhsFeedApi] 的写法）。
 *
 * 两个实现：
 * - [KotlinXhsSearchApi]：`XhsApi.postRaw` + [XhsSearch] 的解析（参照实现，`XhsSource` 默认走这条）；
 * - `HostedXhsSearchApi`（`XhsHosted.kt`）：Rust 平台对象（Rust 拼 URL / body / 头 / 签名 / 判错 / 解析，
 *   收发回调宿主用 `XhsApi.http`），`RustXhsSource` 走这条。
 *
 * 表情表不在这里：Rust / Kotlin 都只用调用方给的表情表，取表（按标题里有没有记号）和贴回在 [XhsSource.search]
 * 里做一次，两条路的语义因此一样。
 */
internal interface XhsSearchApi {
    /** 搜一页。[nowMs] 是这一次请求的"现在"（签名、`search_id`、笔记 id 推时间都用它）。 */
    suspend fun search(keyword: String, cursor: Cursor?, nowMs: Long): XhsSearchPage
}

/**
 * Kotlin 参照实现：请求体照 [XhsSearch.body]，签名 / 限速 / 错误映射照旧走 [XhsApi]（`postRaw` 会要 `a1`，
 * 没有就抛 `NotLoggedInException`；461 / 471 / 300011 之类由 `XhsApi.checkResponse` 映射）。
 *
 * `search_id` 只在**第一页**现造（`cursor == null`）：同一轮搜索的所有页复用同一个（参考实现都这么做）。
 * 随机数从调用方的 [Random] 里取，位置在签名随机量**之前**——Rust 那边的 `HostedXhsSearchApi` 也在这个位置取，
 * 两边同一个 `Random(seed)` 才会产出同一串请求（`XhsSearchEquivalenceTest` 守着）。
 */
internal class KotlinXhsSearchApi(
    private val api: XhsApi,
    private val random: Random,
) : XhsSearchApi {
    override suspend fun search(keyword: String, cursor: Cursor?, nowMs: Long): XhsSearchPage {
        val randomPart = if (cursor == null) random.nextLong(1, XhsSearch.RANDOM_BOUND) else 0L
        val (decoded, page) = XhsSearch.decodeCursor(cursor)
        val searchId = decoded ?: XhsSearch.searchId(nowMs, randomPart)
        val json = XhsApi.checkResponse(api.postRaw(XhsSearch.PATH, XhsSearch.body(keyword, page, searchId)))
        return XhsSearch.parse(json, page, searchId, emptyMap(), nowMs / 1000)
    }
}

/** 搜索的一页（`SearchSource` 要的是 [Page]；这里带上标题拼串，好让调用方决定要不要取表情表）。 */
internal class XhsSearchPage(val items: List<FeedItem>, val next: Cursor?, val titles: String)
