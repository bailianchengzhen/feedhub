package feedhub.app.feed

import androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.util.concurrent.atomic.AtomicBoolean
import feedhub.app.ContentType
import feedhub.brain.ContentFilter
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.Source
import feedhub.feed.FeedEngine
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.CoroutineContext

/**
 * 一个信息流页面（首页或关注）的状态：多个平台的流混在一起。
 *
 * 这个类只剩三件事：
 * 1. **Compose 状态容器**：`items` / `refreshing` / `errors` … 还是 `mutableStateOf`（界面直接读），
 *    值从引擎的 [FeedEngine.View] 抄一份；
 * 2. **协程**：`scope.launch` / `delay` / `withTimeoutOrNull` / `Job.cancel`——"什么时候发、等多久"由引擎说，
 *    **真的等**在这里；
 * 3. **真正的收发**：`Source.feed(kind, cursor)`（OkHttp），结果喂回引擎；错误文案也由这边算
 *    （[describe] 认的是宿主异常类型）。
 *
 * 产品行为一条没变（`FeedStateTest` 的 40 多个用例照过），
 *
 * 所有状态只在 [scope]（主线程）里改，所以不用加锁。
 */
class FeedState(
    val kind: FeedKind,
    private val scope: CoroutineScope,
    /** 这次要请求哪些平台（已按设置里的平台筛选、内容类型、是否支持这个流过滤好）。 */
    private val sources: () -> List<Source>,
    private val contentType: () -> ContentType,
    /**
     * 内容过滤第一层的规则：**每一批现读**，设置页改了立刻对下一批生效。
     */
    private val rules: () -> ContentFilter.Rules = { ContentFilter.Rules() },
    /** **每个平台一批最多几条**（设置里 8 / 12 / 15 / 不限，默认 15）：0 = 不限。 */
    private val maxPerPlatform: () -> Int = { 0 },
    /** **首页的批内顺序**。关注页忽略它。 */
    private val order: () -> FeedOrder = { FeedOrder.INTERLEAVE },
    /**
     * **本地向量打分的注入点**：当前即使给了它，[EmbeddingScore.WEIGHT] 还是 0，
     * 顺序一条都不变（`ScoredOrderTest` 守着）。**引擎那侧现在没有这条线**：权重 0 时它根本不参与，
     * 接权重是下一期的事（到时候在 Rust 里接，`brain::blend_order` 已经在）。
     */
    private val embeddingScore: ((FeedItem) -> Float?)? = null,
    private val io: CoroutineContext = Dispatchers.IO,
    /** 一批最多等多久（毫秒）。 */
    waitMs: Long = 5_000,
    /** "上一批"存在哪（[FeedSnapshot]，冷启动先显示它）；null = 不存（测试 / 演示）。 */
    private val snapshot: FeedSnapshot? = null,
    /** App 现在在前台吗。**后台不发任何平台请求**。 */
    private val foreground: () -> Boolean = { true },
    /** 回到前台时，离上次取到新内容超过这么久就**只预取、不自动替换**屏幕上的内容。 */
    resumeStaleMs: Long = 5 * 60_000L,
    private val now: () -> Long = System::currentTimeMillis,
    /** 调试日志（被内容过滤藏掉的条数**只记数量**，不记内容；默认不记）。 */
    private val log: (String) -> Unit = {},
    /** **有新内容上屏了**：参数是这一批新出现的卡片（App 里接到向量调度器）。 */
    private val onNewContent: (List<FeedItem>) -> Unit = {},
    /** **平台说"没登录"了**：把平台 id 交给上层（`AccountsState.markExpired`，**不发请求**）。 */
    private val onAuthExpired: (Platform) -> Unit = {},
) : AutoCloseable {
    private val closed = AtomicBoolean(false)

    /** 跨页签保留的瀑布流滚动与列分配状态。 */
    val gridState: LazyStaggeredGridState = LazyStaggeredGridState()
    internal var handledRefreshedTimes: Int = 0
    internal var lastLaidOutColumns: Int = 0
    private val engine = FeedEngine(
        kind,
        FeedEngine.Config(
            waitMs = waitMs,
            resumeStaleMs = resumeStaleMs,
            maxBlankPages = MAX_BLANK_PAGES,
            refreshMinAnimationMs = REFRESH_MIN_ANIMATION_MS,
        ),
    )

    var items: List<FeedItem> by mutableStateOf(emptyList())
        private set
    var refreshing by mutableStateOf(false)
        private set
    var loadingMore by mutableStateOf(false)
        private set
    /** 这次刷新 / 加载失败或超时的平台和原因（一行字）。 */
    var errors: Map<Platform, String> by mutableStateOf(emptyMap())
        private set
    /** 这一批平台**明确回了"没登录"**的（空状态要区分"登录过期"和"接口出错"）。 */
    var notLoggedIn: Set<Platform> by mutableStateOf(emptySet())
        private set
    /** 这一页**加载过没有**。 */
    var loadedOnce by mutableStateOf(false)
        private set
    /** 刷新出的新一批放到最上面一次就加一，界面据此回到顶部。 */
    var refreshedTimes by mutableIntStateOf(0)
        private set
    /** 正在单独重试的平台（[retryPlatform]）。null = 没有。 */
    var retrying: Platform? by mutableStateOf(null)
        private set

    /** 备好的那一批（下一次刷新直接上屏）。界面拿它预取卡片图（[PrefetchedImages]）。 */
    var prefetched: List<FeedItem> by mutableStateOf(emptyList())
        private set

    /**
     * 屏幕上的内容（key → 条目）。引擎只记账（key / 平台 / 时间），**整条 `FeedItem` 在宿主**：
     * 一页从 `Source.feed` 回来时就留在这里，屏幕列表按引擎给的顺序、按 key 还原。
     */
    private val byKey = HashMap<String, FeedItem>()

    private var job: Job? = null
    private var prefetchJob: Job? = null
    private var round: Job = SupervisorJob(scope.coroutineContext[Job])
    private var refreshToken: Long? = null
    private var lastScrolledReported: Int = -1
    private var scrollPrefetchSatisfied: Boolean = false
    private val snapshotLoaded: Job

    init {
        // 冷启动先显示上一批：读到时页面还是空的才摆上（刷新先回来了就不摆）；
        // 现在不看的平台、18+、不是现在内容类型的不摆。**按存下来的屏幕顺序摆回去**。
        snapshotLoaded = if (snapshot != null) scope.launch {
            val cached = withContext(io) { snapshot.load() }
            if (items.isNotEmpty() || cached.isEmpty()) return@launch
            remember(cached)
            val action = engine.loadSnapshot(cached, platforms(), batch(), now())
            apply(action)
        } else Job().apply { complete() }
    }

    /**
     * 进页面时调：页面是空的就刷新；摆着上一批、还没刷新过，就只在后台预取新的一批。
     */
    fun ensureLoaded() {
        scope.launch {
            snapshotLoaded.join()
            if (!foreground()) return@launch
            dispatch(engine.ensureLoaded(foreground(), platforms(), batch(), now()))
        }
    }

    /** App 退到后台：取消在飞的刷新 / 翻页 / 预取，之后任何请求入口都直接返回。 */
    fun onBackground() {
        if (foreground()) return
        job?.cancel()
        prefetchJob?.cancel()
        round.cancel()
        round = SupervisorJob(scope.coroutineContext[Job])
        engine.onBackground(foreground())
        refreshToken = null
        syncView()
    }

    /** App 回到前台：**不自动替换屏幕上的内容**。 */
    fun onForeground() {
        if (!foreground()) return
        scope.launch {
            snapshotLoaded.join()
            dispatch(engine.onForeground(foreground(), platforms(), batch(), now()))
        }
    }

    /** 用户往下滑到了"最后一张可见卡片"是第 [lastVisible] 条：**看过当前这批的一半就备下一批**。 */
    fun onScrolledTo(lastVisible: Int) {
        if (closed.get() || !foreground() || lastVisible <= 0) return
        if (lastVisible == lastScrolledReported) return
        lastScrolledReported = lastVisible
        if (scrollPrefetchSatisfied || prefetched.isNotEmpty() || prefetchJob?.isActive == true || refreshing || loadingMore) return
        val action = engine.onScrolledTo(lastVisible, foreground(), platforms(), batch(), now())
        if (action.started) {
            scrollPrefetchSatisfied = true
        }
        if (action.isNoOp()) return
        dispatch(action)
    }

    /** 用户从首页 / 关注**点进详情页**了：用户看详情的时间正好用来把下一批取回来。 */
    fun onOpenDetail() {
        if (closed.get()) return
        scrollPrefetchSatisfied = true
        dispatch(engine.onOpenDetail(foreground(), platforms(), batch(), now()))
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        snapshotLoaded.cancel()
        job?.cancel()
        prefetchJob?.cancel()
        round.cancel()
        engine.close()
    }

    /**
     * 刷新：手上有备好的批次就直接整批放到最上面（不再请求、也不管放了多久）；否则照常请求。
     * [fresh] = true（设置里改了平台 / 内容类型）时一定重新请求，备好的那批丢掉。
     */
    fun refresh(fresh: Boolean = false) {
        dispatch(engine.refresh(fresh, foreground(), platforms(), batch(), now()))
    }

    /**
     * 取下一页（各平台一页，一批）；真的发起了返回 true。后台一律不取。
     *
     * **冷启动摆着上一批、还没刷新过**时滑到底：预取的那一批接在后面当下一页。刷新过之后就不是了。
     */
    fun loadMore(): Boolean {
        val action = engine.loadMore(foreground(), platforms(), batch(), now())
        dispatch(action)
        return action.started
    }

    /**
     * **只重试一个平台**：不清空屏幕上的内容、不动别的平台的错误、不整批重排。
     * 正在刷新 / 翻页 / 预取 / 另一个重试时返回 false。
     */
    fun retryPlatform(platform: Platform): Boolean {
        val action = engine.retryPlatform(platform.key, foreground(), platforms(), batch(), now())
        dispatch(action)
        return action.started
    }

    /** 屏幕上这条内容属于哪一批、平台第几条；null = 不在当前信息流里。 */
    fun originOf(key: String): ItemOrigin? =
        engine.originOf(key)?.let { ItemOrigin(it.batchId, it.platformRank) }

    /**
     * 内容过滤设置改了：把屏幕上**已经显示、按现在的规则应该藏掉**的条目去掉，不用等下一次刷新。
     *
     * 18+ / 名单那一步是 Rust（`FeedFilter.shown` → `brain_filter_shown`），这里把"还留着的 key"交给引擎收拢。
     * **解除不了**：已经被藏掉的条目不在列表里，关掉外出模式后它们要等下一批才回来。
     */
    fun dropHidden() {
        if (items.isEmpty()) return
        val active = rules()
        if (active.isEmpty) return
        val shown = FeedFilterRef.shown(items, active)
        if (shown.size == items.size) return
        apply(engine.dropHidden(shown.map { it.key }))
    }

    // ------------------------------------------------------------------ 派发

    /**
     * 引擎的一次回答：先把状态抄下来，再按引擎说的 [FeedEngine.Action.flow] 真的跑那一套。
     *
     * **为什么由引擎指定 flow**：换一条路（用预取 / 现场请求 / 翻页）的差别不只是"有没有请求"——
     * "用预取"那一路**不能在这里去重**（去重排在最短动画之后，中途退到后台不能白丢一批），
     * "现场请求"那一路反过来要先去重再等动画。这两条时序是 `FeedStateTest` 守着的硬约束，
     * 所以判断只有一份、在 Rust 里。
     */
    private fun dispatch(action: FeedEngine.Action) {
        apply(action)
        when (action.flow) {
            FeedEngine.Flow.NONE -> Unit
            FeedEngine.Flow.COMMIT_PREFETCHED -> startPrefetchedRefresh(action)
            FeedEngine.Flow.REFRESH_ROUND -> startRefreshRound(action)
            FeedEngine.Flow.ROUND -> startRound(action)
        }
    }

    /** 现场请求的刷新：等到最短动画那一步才上屏（引擎给的差额）。 */
    private fun startRefreshRound(action: FeedEngine.Action) {
        val token = action.refreshRound ?: return
        refreshToken = token
        job?.cancel()
        round.cancel()
        round = SupervisorJob(scope.coroutineContext[Job])
        val requests = action.requests
        val platforms = requests.map { Platform(it.platform) }
        job = scope.launch {
            val perfAt = feedhub.app.perf.PerfHooks.feedRefreshStart(kind, platforms)
            var count = 0
            try {
                gather(requests)
                val finished = engine.finishRound(batch(), now())
                count = finished.newContent.size
                if (finished.remainMs > 0) delay(finished.remainMs)
                apply(engine.commitRound(token, now()))
            } finally {
                if (refreshToken == token) refreshToken = null
                engine.refreshDone(token)
                syncView()
                feedhub.app.perf.PerfHooks.feedRefreshEnd(kind, platforms, count, perfAt)
            }
        }
    }

    /** 用备好的那批刷新：只等最短动画，不请求、不去重（去重在 `commit_prefetched` 那一步）。 */
    private fun startPrefetchedRefresh(action: FeedEngine.Action) {
        val token = action.refreshRound ?: return
        refreshToken = token
        job?.cancel()
        round.cancel()
        round = SupervisorJob(scope.coroutineContext[Job])
        job = scope.launch {
            try {
                if (action.remainMs > 0) delay(action.remainMs)
                apply(engine.commitPrefetched(batch(), now()))
            } finally {
                if (refreshToken == token) refreshToken = null
                engine.refreshDone(token)
                syncView()
            }
        }
    }

    /** 一轮（翻页 / 预取 / 重试）：等齐或等满超时，然后由引擎收尾（它自己落地）。 */
    private fun startRound(action: FeedEngine.Action) {
        val background = action.requests.firstOrNull()?.background == true
        val myJob = scope.launch {
            try {
                gather(action.requests)
                apply(engine.finishRound(batch(), now()))
            } finally {
                syncView()
            }
        }
        if (background) prefetchJob = myJob else job = myJob
    }

    /**
     * 并发请求 [requests]，等全部回来或等满超时；期间每一页都喂回引擎。
     *
     * "超时之后还在飞的那几家"由引擎在 [FeedEngine.onTimeout] 里记进慢平台 / 错误表（预取不记）；
     * 它们晚到的内容由引擎分派（刷新 / 翻页进 `late`，预取丢掉）。
     */
    private suspend fun gather(requests: List<FeedEngine.Request>) {
        val jobs = requests.map { request -> scope.launch(round) { fetchChain(request) } }
        if (jobs.isNotEmpty()) {
            withTimeoutOrNull(engine.waitMs()) { jobs.joinAll() }
            engine.onTimeout()
            syncView()
        }
    }

    /** 一家的请求小循环：引擎说"再取一页"（整页都被过滤掉了）就接着取。 */
    private suspend fun fetchChain(request: FeedEngine.Request) {
        var cursor = request.cursor
        while (true) {
            val outcome = fetchOnce(request.platform, cursor)
            val step = engine.onPage(request.round, request.platform, outcome, batch(), now())
            apply(step.action)
            cursor = step.next ?: return
        }
    }

    /**
     * 取一页（真的收发）。**不抛**：失败折成 `Outcome.Failed`（引擎决定记不记、怎么记）。
     */
    private suspend fun fetchOnce(platform: String, cursor: String?): FeedEngine.Outcome {
        val source = sources().firstOrNull { it.platform.key == platform }
            ?: return FeedEngine.Outcome.Failed(false, "这个平台这次没有请求")
        return try {
            val page: Page<FeedItem> = withContext(io) {
                feedhub.app.perf.traceSection("feed.load.${source.platform.key}") {
                    source.feed(kind, cursor?.let { Cursor(it) })
                }
            }
            remember(page.items)
            FeedEngine.Outcome.Ok(page.items, page.next?.value)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // 「我的 → 账号」标过期**不在这里调**：引擎在 `on_page` 里按"这一页是不是没登录失败"决定，
            // 由 [apply] 的 `action.authExpired` 一次性交给上层（预取 / 晚到那几条路也一样）。
            // 这里再调一次，同一个平台一次刷新就会报两回（`FeedAuthExpiredTest` 守着）。
            log("[信息流] $platform 失败：${technicalDetail(e)}")
            FeedEngine.Outcome.Failed(e is NotLoggedInException, describe(e))
        }
    }

    // ------------------------------------------------------------------ 状态落地

    /** 把引擎的回答落到 Compose 状态：屏幕列表按**引擎给的 key 顺序**从 [byKey] 还原。 */
    private fun apply(action: FeedEngine.Action) {
        for (platform in action.authExpired) onAuthExpired(Platform(platform))
        for (line in action.log) log(line)
        if (action.screenChanged) {
            items = engine.screenKeys().mapNotNull { byKey[it] }
            lastScrolledReported = -1
            scrollPrefetchSatisfied = false
            prune()
        }
        if (action.newContent.isNotEmpty()) onNewContent(action.newContent.mapNotNull { byKey[it] })
        if (action.prefetchedChanged) {
            prefetched = engine.prefetchedKeys().mapNotNull { byKey[it] }
        }
        if (action.saveSnapshot) {
            val top = items.take(FeedSnapshot.MAX_ITEMS)
            snapshot?.let { s -> scope.launch(io) { s.save(top) } }
        }
        syncView()
    }

    private fun FeedEngine.Action.isNoOp(): Boolean =
        flow == FeedEngine.Flow.NONE &&
            !started &&
            !screenChanged &&
            !prefetchedChanged &&
            !saveSnapshot &&
            newContent.isEmpty() &&
            authExpired.isEmpty() &&
            log.isEmpty()

    /** 引擎的状态快照 → Compose 状态（值没变就不写，省重组）。 */
    private fun syncView() {
        val view = engine.view()
        if (refreshing != view.refreshing) refreshing = view.refreshing
        if (loadingMore != view.loadingMore) loadingMore = view.loadingMore
        if (loadedOnce != view.loadedOnce) loadedOnce = view.loadedOnce
        if (refreshedTimes != view.refreshedTimes) refreshedTimes = view.refreshedTimes
        val retry = view.retrying?.let { Platform(it) }
        if (retrying != retry) retrying = retry
        val errs = LinkedHashMap<Platform, String>(view.errors.size)
        for ((platform, message) in view.errors) errs[Platform(platform)] = message
        if (errors != errs) errors = errs
        val not = view.notLoggedIn.mapTo(LinkedHashSet()) { Platform(it) }
        if (notLoggedIn != not) notLoggedIn = not
    }

    /** 一页回来的条目先收着（引擎只给 key，整条内容在这里）。 */
    private fun remember(page: List<FeedItem>) {
        for (item in page) byKey[item.key] = item
    }

    /** 屏幕和预取之外的那些条目丢掉（翻页翻得久了别一直涨）。 */
    private fun prune() {
        if (byKey.size <= items.size + prefetched.size + 64) return
        val alive = HashSet<String>(items.size + prefetched.size)
        for (item in items) alive += item.key
        for (item in prefetched) alive += item.key
        for (key in engine.prefetchedKeys()) alive += key
        byKey.keys.retainAll(alive)
    }

    private fun platforms(): List<String> = sources().map { it.platform.key }

    private fun batch(): FeedEngine.Batch = FeedEngine.Batch(
        contentType = when (contentType()) {
            ContentType.MIXED -> FeedEngine.ContentType.MIXED
            ContentType.VIDEO -> FeedEngine.ContentType.VIDEO
            ContentType.TEXT -> FeedEngine.ContentType.TEXT
        },
        maxPerPlatform = maxPerPlatform(),
        order = when (order()) {
            FeedOrder.INTERLEAVE -> FeedEngine.Order.INTERLEAVE
            FeedOrder.TIME -> FeedEngine.Order.TIME
        },
        rules = rules(),
    )

    /** `feedhub.brain.FeedFilter`（`:rust-bridge` 的门面）的简写。 */
    private object FeedFilterRef {
        fun shown(items: List<FeedItem>, rules: ContentFilter.Rules): List<FeedItem> =
            feedhub.brain.FeedFilter.shown(items, rules).items
    }

    companion object {
        /**
         * 一页全被 18+ / 名单过滤掉时最多连续补几页。
         *
         * **真正的判定在 Rust**（`feeds::MAX_BLANK_PAGES`，`FeedEngine.Config.maxBlankPages` 的默认值就是它）；
         * 这里留一份**同值**的命名常量给测试 / 界面读（不为了一个常数加载 `.so`）。
         */
        const val MAX_BLANK_PAGES: Int = 3

        /**
         * **刷新动画至少显示多久**。
         * 400 ms 是用户给的范围里的中间值；转圈 / 下拉刷新的动画至少跑这么久，到点才把新的一批放上屏幕。
         * 现场请求本来就更久的不额外等。**以后要调就改这一个数**（Rust 的 `EngineConfig.refresh_min_animation_ms`
         * 从这里传进去，`FeedEngine.Config` 的默认值也是 400）。
         */
        const val REFRESH_MIN_ANIMATION_MS: Long = 400L
    }
}
