package feedhub.app.detail

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.feed.describe
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.NativeDetailSource
import feedhub.core.Page
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import feedhub.present.RustPresentPaging
import uniffi.feedhub_ffi.FfiPresentCommentDisplayItem
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext

/**
 * 一列分页数据的状态（评论、楼中楼用）：滑到底调 [loadMore]，第一页也是它。
 *
 * 状态机、去重、游标管理下沉至 Rust（[uniffi.feedhub_ffi.FfiPagingEngine]），
 * 本类只保留 Compose `mutableStateOf` 与协程调用。
 */
class PagedState<T>(
    private val scope: CoroutineScope,
    private val key: (T) -> String,
    private val io: CoroutineContext = Dispatchers.IO,
    private val fetch: suspend (Cursor?) -> Page<T>,
) {
    private val engine = RustPresentPaging.newPagingEngine()

    var items: List<T> by mutableStateOf(emptyList())
        private set
    var loading by mutableStateOf(false)
        private set
    /** 界面上显示的一句人话（[friendlyError]）；**不是**技术文案。 */
    var error: String? by mutableStateOf(null)
        private set

    /** 上一次失败的技术细节（不显示在界面上，写日志 / 调试用）。 */
    var technicalError: String? by mutableStateOf(null)
        private set

    /** 已经拿到最后一页。 */
    var ended by mutableStateOf(false)
        private set

    /** 至少成功拿过一页。 */
    var loaded by mutableStateOf(false)
        private set

    private var job: Job? = null

    /**
     * 用缓存里的第一页把这个状态**摆成"已经拿过第一页"**：第一次组合就是完整的，
     * 不会先闪一下"还没有评论 / 转圈"。
     */
    fun seed(items: List<T>, next: Cursor?, ended: Boolean, fresh: Boolean) {
        this.items = items
        engine.seed(items.map(key), next?.value, ended, fresh)
        this.ended = engine.isEnded()
        this.loaded = engine.isLoaded()
    }

    /**
     * 每拿到一页调一次（[ended] = 这一页之后还有没有；[firstPage] = 这是不是第一页）。
     */
    var onPage: ((Page<T>, Boolean, Cursor?, Boolean) -> Unit)? = null

    fun loadMore() {
        val step = engine.beginLoad() ?: return
        loading = true
        error = null
        val refetchFirst = step.refetchFirst
        job = scope.launch {
            try {
                val cursor = step.cursor?.let(::Cursor)
                val page = withContext(io) { fetch(cursor) }
                val itemKeys = page.items.map(key)
                val res = engine.completePage(itemKeys, page.next?.value, step.refetchFirst, step.isFirstPage)
                if (res.replace) {
                    items = page.items
                } else {
                    items = items + res.keptIndices.map { page.items[it.toInt()] }
                }
                ended = res.ended
                loaded = res.loaded
                onPage?.invoke(page, res.ended, res.nextCursor?.let(::Cursor), res.isFirstPage)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val tech = describe(e)
                val friendly = friendlyError(e)
                engine.failPage(friendly, tech, refetchFirst)
                technicalError = engine.technicalError()
                error = engine.error()
                println("[feedhub] 评论加载失败：$technicalError")
            } finally {
                loading = engine.isLoading()
            }
        }
    }

    fun cancel() {
        job?.cancel()
        engine.cancel()
        loading = engine.isLoading()
    }
}

/**
 * 一条内容的评论区：当前排序 + 对应的分页数据。换排序就换一份新的分页数据（从第一页开始）。
 */
class CommentsState(
    private val source: NativeDetailSource,
    private val id: String,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
    /** 平台目录：评论能不能排序、有没有楼中楼都按插件描述的能力判断。 */
    private val platforms: PlatformCatalog = InstalledPlugins.catalog,
    private val cacheKey: String = DetailCache.key(source.platform, id),
) {
    val sortable: Boolean get() = RustPresentPaging.commentsSortable(platforms[source.platform]?.capabilities?.commentSort)

    val repliesSupported: Boolean get() = repliesSupported(source.platform, platforms)

    var sort: CommentSort by mutableStateOf(CommentSort.DEFAULT)
        private set
    var page: PagedState<Comment> by mutableStateOf(newPage(CommentSort.DEFAULT))
        private set

    private var cachedBatchItems: List<Comment>? = null
    private var cachedBatchPosterId: String? = null
    private var cachedBatchPlatformLabel: String? = null
    private var cachedBatchRepliesSupported: Boolean? = null
    private var cachedBatchMap: Map<String, FfiPresentCommentDisplayItem> = emptyMap()

    /**
     * 一整页一级评论的展示字段批量预计算缓存：同一份 [PagedState.items] 引用只过一次 FFI，
     * 列表滑动与界面重组直接查内存表（0 次 FFI）。
     */
    internal fun displayBatch(posterId: String?, platformLabel: String?): Map<String, FfiPresentCommentDisplayItem> {
        val currentItems = page.items
        val allowed = repliesSupported
        if (currentItems === cachedBatchItems &&
            posterId == cachedBatchPosterId &&
            platformLabel == cachedBatchPlatformLabel &&
            allowed == cachedBatchRepliesSupported
        ) {
            return cachedBatchMap
        }
        val map = if (currentItems.isEmpty()) {
            emptyMap()
        } else {
            RustPresentPaging.prepareCommentBatch(
                comments = currentItems,
                posterId = posterId,
                replyToRootAuthorId = null,
                onOpenRepliesAllowed = allowed,
                platformLabel = platformLabel,
            ).associateBy { it.id }
        }
        cachedBatchItems = currentItems
        cachedBatchPosterId = posterId
        cachedBatchPlatformLabel = platformLabel
        cachedBatchRepliesSupported = allowed
        cachedBatchMap = map
        return map
    }

    /**
     * 知乎回答楼层的**评论预览**：楼层滑到屏幕上时取一次这条回答的第一页评论，
     * 留前几条。按楼 id 存在这里（不放在条目的 `remember` 里：滑出屏幕再滑回来不会再请求一次）；
     * 失败就记一张空表，不重试（点「共 N 条评论」照样能看全部）；**遇到风控 / 未登录就整页不再取预览**。
     */
    val answerPreviews = mutableStateMapOf<String, List<Comment>>()
    private val previewJobs = HashSet<String>()
    @Volatile
    private var previewsStopped = false

    /** 楼层第一次出现在屏幕上时调；同一楼只取一次。 */
    fun loadAnswerPreview(floor: Comment) {
        if (previewsStopped || (floor.replyCount ?: 0L) <= 0L || floor.previewReplies.isNotEmpty()) return
        if (!previewJobs.add(floor.id)) return
        scope.launch {
            val items = try {
                withContext(io) { source.replies(id, floor.id, null) }.items.take(ANSWER_PREVIEW_COMMENTS)
            } catch (e: CancellationException) {
                previewJobs.remove(floor.id)
                throw e
            } catch (e: Exception) {
                if (e is feedhub.core.RiskControlException || e is feedhub.core.NotLoggedInException) previewsStopped = true
                println("[feedhub] 回答评论预览没取到：${describe(e)}")
                emptyList()
            }
            answerPreviews[floor.id] = items
        }
    }

    fun select(next: CommentSort) {
        if (!RustPresentPaging.shouldSwitchSort(sort.name, next.name)) return
        page.cancel()
        sort = next
        page = newPage(next)
        page.loadMore()
    }

    private fun newPage(s: CommentSort): PagedState<Comment> {
        val seed = DetailCache.comments(cacheKey, s)
        val state = PagedState(scope, Comment::id, io) { cursor ->
            if (!commentsSupported(source.platform)) throw UnsupportedOperationException(COMMENTS_OFF)
            val metered = feedhub.app.perf.meteredRequest(feedhub.app.perf.PerfRequest.COMMENTS, source.platform, io) {
                feedhub.app.perf.traceSection("comments.load.${source.platform.key}") { source.comments(id, cursor, s) }
            }
            feedhub.app.perf.PerfHooks.commentsLoaded(source.platform, metered.value.items.size, metered.dMs, metered.queueMs)
            metered.value
        }
        if (seed != null) state.seed(seed.items, seed.next, seed.ended, DetailCache.commentsFresh(cacheKey, s))
        state.onPage = { page, ended, next, firstPage ->
            if (firstPage) DetailCache.putComments(cacheKey, s, page.items, next, ended)
        }
        return state
    }
}

fun commentsSupported(platform: feedhub.core.Platform): Boolean =
    RustPresentPaging.commentsSupported(platform.key)

internal val COMMENTS_OFF: String get() = RustPresentPaging.COMMENTS_OFF

fun repliesSupported(platform: feedhub.core.Platform, platforms: PlatformCatalog = InstalledPlugins.catalog): Boolean =
    RustPresentPaging.repliesSupported(platforms[platform]?.capabilities?.nestedReplies)

fun repliesState(
    source: NativeDetailSource,
    id: String,
    commentId: String,
    scope: CoroutineScope,
    io: CoroutineContext = Dispatchers.IO,
): PagedState<Comment> = PagedState(scope, Comment::id, io) { cursor ->
    feedhub.app.perf.meteredRequest(feedhub.app.perf.PerfRequest.REPLIES, source.platform, io) {
        source.replies(id, commentId, cursor)
    }.value
}
