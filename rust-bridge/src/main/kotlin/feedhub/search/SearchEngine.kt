package feedhub.search

import feedhub.brain.ContentFilter
import feedhub.feed.isVideoPostItem
import feedhub.core.FeedItem
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiFeedRef
import uniffi.feedhub_ffi.FfiSearchAction
import uniffi.feedhub_ffi.FfiSearchOutcome
import uniffi.feedhub_ffi.FfiSearchRequest
import uniffi.feedhub_ffi.FfiSearchState
import uniffi.feedhub_ffi.FfiSearchView

/**
 * **搜索页状态机**（`rust/crates/feedhub-core/src/feed/search.rs`）的 Kotlin 门面。
 *
 *
 * Kotlin 剩下的：Compose 状态容器、协程 / `withTimeoutOrNull` / `Channel`、真正的 `SearchSource.search()`
 * 收发、`describe(e)` 那种"认异常类型"的文案。
 *
 * 没有开关：`.so` 加载失败就抛。
 */
class SearchEngine(pageBudget: Int = PAGE_BUDGET) : AutoCloseable {

    /** 一条要宿主去发的请求。 */
    data class Request(val round: Long, val platform: String, val cursor: String?)

    data class Action(
        val requests: List<Request>,
        val screenChanged: Boolean,
        val appended: List<String>,
        val started: Boolean,
        val firstBatchDone: Boolean,
        val log: List<String>,
    )

    data class View(
        val query: String,
        val loading: Boolean,
        val loadingMore: Boolean,
        val platforms: List<String>,
        val enabled: Set<String>,
        val answered: Int,
        val exhausted: Boolean,
        val errors: Map<String, String>,
    )

    /** 一次搜索请求的结果。[pageSize] 是这家 `SearchCapability.pageSize`（配额的上限）。 */
    sealed interface Outcome {
        data class Ok(val items: List<FeedItem>, val next: String?, val pageSize: Int) : Outcome
        data class Failed(val describe: String) : Outcome
    }

    init { RustNative.ensureLoaded() }

    private val native: FfiSearchState = FfiSearchState(pageBudget)

    fun preparePlatforms(sources: List<String>): Action = load { native.preparePlatforms(sources).toCore() }

    fun search(text: String, sources: List<String>, rules: ContentFilter.Rules): Action =
        load { native.search(text, sources, rules.toFfi()).toCore() }

    fun firstBatchTimeout(firstBatchMs: Long): Action = load { native.firstBatchTimeout(firstBatchMs).toCore() }

    fun nextBatchTimeout(nextBatchMs: Long): Action = load { native.nextBatchTimeout(nextBatchMs).toCore() }

    fun onResult(round: Long, platform: String, outcome: Outcome, rules: ContentFilter.Rules): Action =
        load { native.onResult(round.toULong(), platform, outcome.toFfi(), rules.toFfi()).toCore() }

    fun loadMore(foreground: Boolean): Action = load { native.loadMore(foreground).toCore() }

    fun toggle(platform: String, sources: List<String>): Action = load { native.toggle(platform, sources).toCore() }

    /** 关掉搜索页（UniFFI 的 `close` 是释放句柄，所以这个方法叫 `endSearch`）。 */
    fun endSearch() = load { native.endSearch() }

    fun screenKeys(): List<String> = load { native.screenKeys() }

    fun view(): View = load { native.view().toCore() }

    fun roundId(): Long = load { native.roundId().toLong() }

    override fun close() {
        native.close()
    }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    private fun ContentFilter.Rules.toFfi(): uniffi.feedhub_ffi.FfiFeedRules = uniffi.feedhub_ffi.FfiFeedRules(
        blockWords = blockWords,
        outingMode = outingMode,
        outingKeywords = outingKeywords,
        outingCommunities = outingCommunities,
    )

    private fun FeedItem.toFfi(): FfiFeedRef = FfiFeedRef(
        key = key,
        platform = platform.key,
        publishedAt = publishedAt,
        adult = adult,
        isVideo = isVideoPostItem,
        isLive = kind == feedhub.core.ItemKind.LIVE,
        title = title,
        summary = summary,
        community = community,
        repostTitle = repost?.title,
        repostSummary = repost?.summary,
        repostCommunity = repost?.community,
    )

    private fun Outcome.toFfi(): FfiSearchOutcome = when (this) {
        is Outcome.Ok -> FfiSearchOutcome.Ok(items.map { it.toFfi() }, next, pageSize)
        is Outcome.Failed -> FfiSearchOutcome.Failed(describe)
    }

    private fun FfiSearchAction.toCore(): Action = Action(
        requests = requests.map { Request(it.round.toLong(), it.platform, it.cursor) },
        screenChanged = screenChanged,
        appended = appended,
        started = started,
        firstBatchDone = firstBatchDone,
        log = log,
    )

    private fun FfiSearchView.toCore(): View = View(
        query = query,
        loading = loading,
        loadingMore = loadingMore,
        platforms = platforms,
        enabled = enabled.toSet(),
        answered = answered,
        exhausted = exhausted,
        errors = LinkedHashMap<String, String>(errors.size).also { map ->
            for (row in errors) map[row.platform] = row.message
        },
    )

    companion object {
        /** 每家一次搜索最多翻几页（`SearchPlan.PAGE_BUDGET`）。 */
        const val PAGE_BUDGET: Int = 3

        const val READY_PLATFORMS: Int = 3

        /** 一批的目标条数（`SearchMerge.TARGET_BATCH`）。 */
        const val TARGET_BATCH: Int = 20
    }
}

/** 用不到但保留形状：搜索的请求记录和引擎那边的 `FfiSearchRequest` 是同一个。 */
internal fun FfiSearchRequest.toCore(): SearchEngine.Request =
    SearchEngine.Request(round.toLong(), platform, cursor)
