package feedhub.app.search

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.feed.describe
import feedhub.app.feed.technicalDetail
import feedhub.brain.ContentFilter
import feedhub.brain.SearchPlan
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.search.SearchEngine
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * 搜索页的状态：多个平台的搜索结果混成一批。
 *
 * 这个类只剩三件事：
 * 1. **Compose 状态容器**（`query` / `items` / `errors` / `enabled` …，值从引擎 `view()` 抄）；
 * 2. **协程 + 超时**（`withTimeoutOrNull(firstBatchMs / nextBatchMs)`）——"先到 3 家就上屏"由引擎判，
 *    "等到什么时候就不再等"由这里定时；
 * 3. **真正的收发**：`SearchSource.search(request, cursor)`，结果（含这一家的 `pageSize`）喂回引擎。
 *
 * 规则一条没变：
 * - 一次搜索**每个选中平台只发第一页**（请求数 = 平台数，不随结果多少变）；
 * - 每家一页只取 `SearchPlan.quota` 条进自己的桶，按平台顺序轮转拼成一批；
 * 剩下的平台继续跑；超时的不等了、记一条错误，它们晚到的内容留在桶里，
 *   下次翻页时接在后面（**屏幕上已有的位置不插东西**）；
 * - 翻页：每家取下一页，轮转追加；每家一次搜索最多 `SearchPlan.PAGE_BUDGET` 页；
 * - 18+ 整条隐藏、内容过滤第一层在**进桶之前**过（Rust 的 `brain_filter_shown`）；
 * - 平台标签就是筛选：只有亮着的才请求、结果里也只有它们；最后一家点不灭。
 *
 * 只由主线程 [scope] 改状态，所以不用加锁。
 */
class SearchState(
    private val scope: CoroutineScope,
    /** 这次要搜哪些平台（按 `plugins.toml` 顺序，已按设置 / 端 / 能力筛好）。每次搜索现读。 */
    private val sources: () -> List<SearchSource>,
    /** 内容过滤规则（每批现读）。 */
    private val rules: () -> ContentFilter.Rules = { ContentFilter.Rules() },
    private val io: kotlin.coroutines.CoroutineContext = Dispatchers.IO,
    /** 没回来的记一条错误。 */
    private val firstBatchMs: Long = 6_000,
    /** 翻页一批最多等多久（毫秒）。 */
    private val nextBatchMs: Long = 8_000,
    /** 每家一次搜索最多翻几页。 */
    pageBudget: Int = SearchPlan.PAGE_BUDGET,
    /** App 在前台吗（安卓由 Activity 喂）：后台一个平台请求都不发。 */
    private val foreground: () -> Boolean = { true },
    private val log: (String) -> Unit = {},
) {
    private val engine = SearchEngine(pageBudget)

    /** 现在搜的词（空 = 还没搜过）。 */
    var query: String by mutableStateOf("")
        private set

    var items: List<FeedItem> by mutableStateOf(emptyList())
        private set

    /** 这次搜索里失败 / 超时的平台和原因（一行字，和首页的 errors 一样）。 */
    var errors: Map<Platform, String> by mutableStateOf(emptyMap())
        private set

    var loading: Boolean by mutableStateOf(false)
        private set

    var loadingMore: Boolean by mutableStateOf(false)
        private set

    /** 这次搜索页上摆出来的平台标签（按插件顺序；点灭只是不搜，标签还在，可以再点亮）。 */
    var platforms: List<Platform> by mutableStateOf(emptyList())
        private set

    /** 这次搜索页里**亮着的**平台：只有它们会被请求、也只有它们的结果会留在列表里。 */
    var enabled: Set<Platform> by mutableStateOf(emptySet())
        private set

    /** 至少回了一条内容的平台数（界面"已到 N / M 家"里的 N）。 */
    var answered by mutableIntStateOf(0)
        private set

    /** 每家都到底了（3 页到顶或没有下一页），再滑也不会发请求。 */
    var exhausted by mutableStateOf(false)
        private set

    /** 屏幕上的整条内容（引擎只记账 key / 平台，`FeedItem` 留在这里）。 */
    private val byKey = HashMap<String, FeedItem>()

    private var job: Job? = null
    private var round: Job = SupervisorJob(scope.coroutineContext[Job])

    /**
     * 打开搜索页时摆出"这次会搜哪些平台"的标签（只画 chips，**不发请求**）。
     * 已经选过（点灭了几家）就不动，免得一打开又把选择清掉。
     */
    fun preparePlatforms() {
        apply(engine.preparePlatforms(sources().map { it.platform.key }))
    }

    /**
     * 开始一次搜索（用户显式提交才会调）。换词 / 重新搜索会取消上一轮在飞的请求，结果清空。
     *
     * **亮着的那几家保留**：点灭了几家之后换词重搜，不会把点灭的又点亮；一个都没亮 / 还没选过时全亮。
     */
    fun search(text: String) {
        if (!foreground()) return
        val all = sources().map { it.platform.key }
        val action = engine.search(text, all, rules())
        // 换词 / 重新搜索：上一轮的结果清掉（引擎那边已经清空了桶和 key）
        byKey.clear()
        apply(action)
        if (action.requests.isEmpty()) {
            syncView()
            return
        }
        cancelRound()
        loading = true
        val roundId = action.requests.first().round
        job = scope.launch {
            try {
                val done = CompletableDeferred<Unit>()
                launchRequests(roundId, action.requests, done)
                // 超时就把没回来的记一条错误再上屏
                val finished = withTimeoutOrNull(firstBatchMs) { done.await() }
                if (finished == null) apply(engine.firstBatchTimeout(firstBatchMs))
            } finally {
                loading = false
                syncView()
            }
        }
    }

    /**
     * 点一下平台标签：**亮 → 灭**（不再请求这一家，已经拿到的结果也从列表里拿掉）/ **灭 → 亮**
     * （用当前关键词补搜这一家）。返回 false = 这次点击没被接受（**最后一家不让点灭**）。
     */
    fun toggle(platform: Platform): Boolean {
        val all = sources().map { it.platform.key }
        val action = engine.toggle(platform.key, all)
        apply(action)
        if (action.requests.isNotEmpty()) {
            // 重新点亮：用当前关键词补一页，结果追加在末尾
            val roundId = action.requests.first().round
            scope.launch(round) { launchRequests(roundId, action.requests, CompletableDeferred()) }
        }
        return action.started
    }

    /**
     * 取下一批（滑到底时调）。真的有动作返回 true。
     *
     * 先花掉桶里攒着的，都空了才向平台要下一页。
     */
    fun loadMore(): Boolean {
        if (!foreground()) return false
        val action = engine.loadMore(foreground())
        apply(action)
        if (!action.started) return false
        if (action.requests.isEmpty()) return true
        val roundId = action.requests.first().round
        loadingMore = true
        job = scope.launch {
            try {
                val done = CompletableDeferred<Unit>()
                launchRequests(roundId, action.requests, done)
                val finished = withTimeoutOrNull(nextBatchMs) { done.await() }
                if (finished == null) apply(engine.nextBatchTimeout(nextBatchMs))
            } finally {
                loadingMore = false
                syncView()
            }
        }
        return true
    }

    /** 关掉搜索页：取消在飞的请求（已经发出去的不会撤回，但结果不再写状态）。 */
    fun close() {
        cancelRound()
        engine.endSearch()
        syncView()
    }

    // ------------------------------------------------------------------ 内部

    private fun cancelRound() {
        job?.cancel()
        round.cancel()
        round = SupervisorJob(scope.coroutineContext[Job])
    }

    /**
     * 把引擎给的请求并发发出去；**引擎说当前收够了**（`loading` / `loadingMore` 落下）就完成 [done]。
     *
     * "先到 3 家就上屏"由引擎在 `onResult` 里判，宿主只负责在每次结果之后看它落没落。
     */
    private fun CoroutineScope.launchRequests(
        roundId: Long,
        requests: List<SearchEngine.Request>,
        done: CompletableDeferred<Unit>,
    ) {
        for (request in requests) {
            launch(round) { fetchOne(roundId, request.platform, request.cursor, done) }
        }
    }

    private suspend fun fetchOne(
        roundId: Long,
        platform: String,
        cursor: String?,
        done: CompletableDeferred<Unit>,
    ) {
        val source = sources().firstOrNull { it.platform.key == platform } ?: run {
            done.complete(Unit)
            return
        }
        val outcome = try {
            val page = withContext(io) { source.search(SearchRequest(query), cursor?.let { Cursor(it) }) }
            remember(page.items)
            SearchEngine.Outcome.Ok(page.items, page.next?.value, source.search.pageSize)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            log("[搜索] $platform 失败：${technicalDetail(e)}")
            SearchEngine.Outcome.Failed(describe(e))
        }
        apply(engine.onResult(roundId, platform, outcome, rules()))
        // 引擎收够了 / 翻页收齐了（两种情况都由引擎把 loading 落下）
        val view = engine.view()
        if (!view.loading && !view.loadingMore) done.complete(Unit)
    }

    private fun remember(page: List<FeedItem>) {
        for (item in page) byKey[item.key] = item
    }

    private fun apply(action: SearchEngine.Action) {
        if (action.screenChanged) {
            items = engine.screenKeys().mapNotNull { byKey[it] }
        }
        for (line in action.log) log(line)
        syncView()
    }

    private fun syncView() {
        val view = engine.view()
        if (query != view.query) query = view.query
        if (loading != view.loading) loading = view.loading
        if (loadingMore != view.loadingMore) loadingMore = view.loadingMore
        val ps = view.platforms.map { Platform(it) }
        if (platforms != ps) platforms = ps
        val on = view.enabled.mapTo(LinkedHashSet()) { Platform(it) }
        if (enabled != on) enabled = on
        if (answered != view.answered) answered = view.answered
        if (exhausted != view.exhausted) exhausted = view.exhausted
        val errs = LinkedHashMap<Platform, String>(view.errors.size)
        for ((platform, message) in view.errors) errs[Platform(platform)] = message
        if (errors != errs) errors = errs
    }

    companion object {
        /** App / cli 共用）。 */
        const val READY_PLATFORMS: Int = SearchPlan.READY_PLATFORMS
    }
}
