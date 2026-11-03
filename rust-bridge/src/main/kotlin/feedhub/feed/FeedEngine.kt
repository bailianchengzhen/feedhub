package feedhub.feed

import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiFeedAction
import uniffi.feedhub_ffi.FfiFeedBatchCfg
import uniffi.feedhub_ffi.FfiFeedConfig
import uniffi.feedhub_ffi.FfiFeedContentType
import uniffi.feedhub_ffi.FfiFeedErrorRow
import uniffi.feedhub_ffi.FfiFeedKind
import uniffi.feedhub_ffi.FfiFeedMode
import uniffi.feedhub_ffi.FfiFeedOrder
import uniffi.feedhub_ffi.FfiFeedOrigin
import uniffi.feedhub_ffi.FfiFeedRef
import uniffi.feedhub_ffi.FfiFeedRequest
import uniffi.feedhub_ffi.FfiFeedRules
import uniffi.feedhub_ffi.FfiFeedState
import uniffi.feedhub_ffi.FfiPageOutcome
import uniffi.feedhub_ffi.FfiPageStep

/**
 * **信息流状态机**（`rust/crates/feedhub-core/src/feed/feeds.rs`）的 Kotlin 门面。
 *
 *
 * Kotlin 剩下的：
 * - **Compose 状态容器**：`FeedState` 里的 `mutableStateOf`——值从 [view] 抄，返回的是一份快照；
 * - **协程触发**：`scope.launch` / `delay` / `withTimeoutOrNull` / `Job.cancel`；
 * - **真正的收发**：`Source.feed(kind, cursor)`（OkHttp，一个字节都不进 Rust）；
 * - **资料库 / 快照文件 / 平台对象 / 设置**：宿主的东西；
 * - **错误文案**：[PageOutcome.Failed.describe] 由宿主的 `describe(e)` 算好（它认异常类型）。
 *
 * 这个类**没有开关**：`.so` 加载失败就抛（和 `feedhub.brain.Brain` 一样，不悄悄退回 Kotlin 实现——
 * Kotlin 那份已经删了，留着就没有"两份会漂"的问题可言）。
 *
 * 时间由宿主注入（所有方法的 `nowMs`），Rust 不读时钟、不取随机数。
 */
class FeedEngine(kind: FeedKind, config: Config = Config()) : AutoCloseable {

    /** 引擎的固定参数（和 `FeedState` 构造函数的默认值一致）。 */
    data class Config(
        val waitMs: Long = 5_000,
        val resumeStaleMs: Long = 5 * 60_000,
        val maxBlankPages: Int = 3,
        val refreshMinAnimationMs: Long = 400,
    )

    /** 每一批现读的规则。 */
    data class Batch(
        val contentType: ContentType,
        val maxPerPlatform: Int,
        val order: Order,
        val rules: feedhub.brain.ContentFilter.Rules,
    )

    /**
     * 内容类型：设置里的「信息流 → 内容类型」。**取值和 `feedhub.app.ContentType` 一一对应**
     * （`app` 的那个是界面层的，桥接层不能依赖 `:app`，所以这一份是桥接层的镜像）。
     */
    enum class ContentType { MIXED, VIDEO, TEXT }

    /** 首页的批内顺序（`feedhub.app.feed.FeedOrder` 的镜像）。 */
    enum class Order { INTERLEAVE, TIME }

    /** 一条要宿主去发的请求。 */
    data class Request(val round: Long, val platform: String, val cursor: String?, val background: Boolean)

    /** 引擎返回给宿主"这一趟要做什么"。[newContent] 是新出现在屏幕上的 key。 */
    data class Action(
        val requests: List<Request>,
        /** 宿主接下来做什么（**引擎指定**，宿主不要自己猜）。 */
        val flow: Flow,
        val mode: Mode,
        val remainMs: Long,
        val newContent: List<String>,
        val screenChanged: Boolean,
        val prefetchedChanged: Boolean,
        val saveSnapshot: Boolean,
        val started: Boolean,
        val refreshRound: Long?,
        val authExpired: List<String>,
        val log: List<String>,
    )

    enum class Mode { SKIPPED, USE_PREFETCHED, REQUEST }

    /** 宿主接下来做什么。 */
    enum class Flow {
        NONE,

        /** 等 [Action.remainMs] 之后调 [commitPrefetched]。 */
        COMMIT_PREFETCHED,

        /** 现场请求的刷新：发请求 → 等齐 / 超时 → [finishRound] → 等 remainMs → [commitRound]。 */
        REFRESH_ROUND,

        /** 一轮（翻页 / 预取 / 重试）：发请求（可能是空的）→ 等齐 / 超时 → [finishRound]。 */
        ROUND,
    }

    /** 一次平台请求的结果。 */
    sealed interface Outcome {
        data class Ok(val items: List<FeedItem>, val next: String?) : Outcome

        /** [describe] 是给用户看的一行字（宿主用 `describe(e)` 算好）。 */
        data class Failed(val notLoggedIn: Boolean, val describe: String) : Outcome
    }

    /** [onPage] 的答复：`next` 非空就再取一页这一家。 */
    data class Step(val next: String?, val action: Action)

    /** 宿主抄回 Compose 状态用的快照。 */
    data class View(
        val refreshing: Boolean,
        val loadingMore: Boolean,
        val loadedOnce: Boolean,
        val refreshedTimes: Int,
        val retrying: String?,
        val errors: Map<String, String>,
        val notLoggedIn: Set<String>,
        val hasPrefetched: Boolean,
    )

    /** 一条内容属于哪一批、平台第几条（`seen_item` 用）。 */
    data class Origin(val batchId: Long, val platformRank: Int)

    init { RustNative.ensureLoaded() }

    private val native: FfiFeedState = FfiFeedState(kind.toFfi(), config.toFfi())

    // ------------------------------------------------------------------ 读

    fun screenKeys(): List<String> = load { native.screenKeys() }

    fun prefetchedKeys(): List<String> = load { native.prefetchedKeys() }

    fun originOf(key: String): Origin? = load {
        native.originOf(key)?.let { Origin(it.batchId, it.platformRank) }
    }

    fun view(): View = load { native.view().toCore() }

    fun roundId(): Long? = load { native.roundId()?.toLong() }

    fun timeoutMessage(): String = load { native.timeoutMessage() }

    fun waitMs(): Long = load { native.waitMs() }

    fun refreshMinAnimationMs(): Long = load { native.refreshMinAnimationMs() }

    // ------------------------------------------------------------------ 入口

    fun loadSnapshot(cached: List<FeedItem>, platforms: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.loadSnapshot(cached.map { it.toFfi() }, platforms, cfg.toFfi(), nowMs).toCore() }

    fun ensureLoaded(foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.ensureLoaded(foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun onBackground(foreground: Boolean) = load { native.onBackground(foreground) }

    fun onForeground(foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.onForeground(foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun onScrolledTo(lastVisible: Int, foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.onScrolledTo(lastVisible, foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun onOpenDetail(foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.onOpenDetail(foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun refresh(fresh: Boolean, foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.refresh(fresh, foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun commitPrefetched(cfg: Batch, nowMs: Long): Action =
        load { native.commitPrefetched(cfg.toFfi(), nowMs).toCore() }

    fun refreshDone(round: Long) = load { native.refreshDone(round) }

    fun loadMore(foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.loadMore(foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun retryPlatform(platform: String, foreground: Boolean, sources: List<String>, cfg: Batch, nowMs: Long): Action =
        load { native.retryPlatform(platform, foreground, sources, cfg.toFfi(), nowMs).toCore() }

    fun onPage(round: Long, platform: String, outcome: Outcome, cfg: Batch, nowMs: Long): Step =
        load {
            val step: FfiPageStep = native.onPage(round.toULong(), platform, outcome.toFfi(), cfg.toFfi(), nowMs)
            Step(step.next, step.action.toCore())
        }

    fun onTimeout() = load { native.onTimeout() }

    fun finishRound(cfg: Batch, nowMs: Long): Action =
        load { native.finishRound(cfg.toFfi(), nowMs).toCore() }

    fun commitRound(round: Long, nowMs: Long): Action =
        load { native.commitRound(round, nowMs).toCore() }

    fun abortRound() = load { native.abortRound() }

    /** 内容过滤改了：把屏幕上按现在的规则该藏掉的去掉（[keptKeys] 是宿主算好的"还留着的"）。 */
    fun dropHidden(keptKeys: List<String>): Action = load { native.dropHidden(keptKeys).toCore() }

    override fun close() {
        native.close()
    }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    // ------------------------------------------------------------------ 转换

    private fun FeedKind.toFfi(): FfiFeedKind = when (this) {
        FeedKind.RECOMMEND -> FfiFeedKind.RECOMMEND
        FeedKind.FOLLOWING -> FfiFeedKind.FOLLOWING
    }

    private fun Config.toFfi(): FfiFeedConfig = FfiFeedConfig(
        waitMs = waitMs,
        resumeStaleMs = resumeStaleMs,
        maxBlankPages = maxBlankPages.toUInt(),
        refreshMinAnimationMs = refreshMinAnimationMs,
    )

    @Volatile
    private var cachedRulesPair: Pair<feedhub.brain.ContentFilter.Rules, FfiFeedRules>? = null

    private fun Batch.toFfi(): FfiFeedBatchCfg {
        val hit = cachedRulesPair
        val ffiRules = if (hit != null && hit.first == rules) {
            hit.second
        } else {
            FfiFeedRules(
                blockWords = rules.blockWords,
                outingMode = rules.outingMode,
                outingKeywords = rules.outingKeywords,
                outingCommunities = rules.outingCommunities,
            ).also { cachedRulesPair = rules to it }
        }
        return FfiFeedBatchCfg(
            contentType = when (contentType) {
                ContentType.MIXED -> FfiFeedContentType.MIXED
                ContentType.VIDEO -> FfiFeedContentType.VIDEO
                ContentType.TEXT -> FfiFeedContentType.TEXT
            },
            maxPerPlatform = maxPerPlatform,
            order = when (order) {
                Order.INTERLEAVE -> FfiFeedOrder.INTERLEAVE
                Order.TIME -> FfiFeedOrder.TIME
            },
            rules = ffiRules,
        )
    }

    private fun FeedItem.toFfi(): FfiFeedRef = FfiFeedRef(
        key = key,
        platform = platform.key,
        publishedAt = publishedAt,
        adult = adult,
        isVideo = isVideoPostItem,
        isLive = kind == ItemKind.LIVE,
        title = title,
        summary = summary,
        community = community,
        repostTitle = repost?.title,
        repostSummary = repost?.summary,
        repostCommunity = repost?.community,
    )

    private fun Outcome.toFfi(): FfiPageOutcome = when (this) {
        is Outcome.Ok -> FfiPageOutcome.Ok(items.map { it.toFfi() }, next)
        is Outcome.Failed -> FfiPageOutcome.Failed(notLoggedIn, describe)
    }

    private fun FfiFeedAction.toCore(): Action = Action(
        requests = requests.map { Request(it.round.toLong(), it.platform, it.cursor, it.background) },
        flow = when (flow) {
            uniffi.feedhub_ffi.FfiFeedFlow.NONE -> Flow.NONE
            uniffi.feedhub_ffi.FfiFeedFlow.COMMIT_PREFETCHED -> Flow.COMMIT_PREFETCHED
            uniffi.feedhub_ffi.FfiFeedFlow.REFRESH_ROUND -> Flow.REFRESH_ROUND
            uniffi.feedhub_ffi.FfiFeedFlow.ROUND -> Flow.ROUND
        },
        mode = when (mode) {
            FfiFeedMode.SKIPPED -> Mode.SKIPPED
            FfiFeedMode.USE_PREFETCHED -> Mode.USE_PREFETCHED
            FfiFeedMode.REQUEST -> Mode.REQUEST
        },
        remainMs = remainMs,
        newContent = newContent,
        screenChanged = screenChanged,
        prefetchedChanged = prefetchedChanged,
        saveSnapshot = saveSnapshot,
        started = started,
        refreshRound = refreshRound,
        authExpired = authExpired,
        log = log,
    )

    private fun uniffi.feedhub_ffi.FfiFeedView.toCore(): View = View(
        refreshing = refreshing,
        loadingMore = loadingMore,
        loadedOnce = loadedOnce,
        refreshedTimes = refreshedTimes,
        retrying = retrying,
        errors = errors.toOrderedMap(),
        notLoggedIn = notLoggedIn.toSet(),
        hasPrefetched = hasPrefetched,
    )

    private fun List<FfiFeedErrorRow>.toOrderedMap(): Map<String, String> {
        val out = LinkedHashMap<String, String>(size)
        for (row in this) out[row.platform] = row.message
        return out
    }

    companion object {
        /** 每家一次搜索最多翻几页的默认值（这里只是给 `Batch` 的默认构造留的钩子）。 */
        const val PAGE_BUDGET_DEFAULT: Int = 3
    }
}

/**
 * 这条内容算不算"视频"（`FeedState.filter` 的内容类型用的那个判断）。
 *
 * **和 `app/.../video/VideoPosts.kt` 的 `isVideoPost()` 是同一个定义**
 * （`kind == VIDEO || media.firstOrNull() is Media.Video`）：桥接层不能依赖 `:app`，所以这里照抄一行，
 * 由 `feedhub.app.feed.FeedEngineParityTest` 拿真 `FeedItem` 逐条对拍守着（贴吧的视频帖 `kind` 是 `POST`、
 * 只有 `media` 里带视频；直播两边都不算）。
 */
val FeedItem.isVideoPostItem: Boolean
    get() = kind == ItemKind.VIDEO || media.firstOrNull() is Media.Video
