package feedhub.app.mine

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.core.Cursor
import feedhub.core.FollowSource
import feedhub.core.FollowTarget
import feedhub.mine.FollowingEngine
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext

/**
 * "我的 → 关注管理"里一个平台的关注列表：分页读 [FollowSource.followings]，取关走 `unfollow`。
 *
 * 这个类只剩：Compose 状态容器、协程、真正的 `followings()` /
 * `setFollowing(..., false)`。
 *
 * **写操作约定（一条没变）**：取关只有一条路：[askUnfollow]（点"取关"）→ 界面弹确认框 → [confirmUnfollow]
 * 才会调 `setFollowing`；读列表、翻页、重试都不会碰它，失败了也不自动重试。
 * 状态只在 [scope]（主线程）里改。
 */
class FollowingState(
    val source: FollowSource,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
) {
    private val engine = FollowingEngine()

    var items: List<FollowTarget> by mutableStateOf(emptyList())
        private set
    var loading by mutableStateOf(false)
        private set

    /** 读列表失败的原因（一行字）；有值时不再自动翻页，点"重试"才继续。 */
    var error: String? by mutableStateOf(null)
        private set

    /** 还有没有下一页。 */
    var hasMore by mutableStateOf(true)
        private set
    var loaded by mutableStateOf(false)
        private set

    /** 正在取关的对象（[FollowTarget.id]）。 */
    var busy: Set<String> by mutableStateOf(emptySet())
        private set

    /** 最近一次取关失败的原因。 */
    var actionError: String? by mutableStateOf(null)
        private set

    /** 用户点了某项的"取关"、还没确认：界面弹确认框。 */
    var pendingUnfollow: FollowTarget? by mutableStateOf(null)
        private set

    private var job: Job? = null

    /** 第一次显示时加载。 */
    fun ensureLoaded() {
        dispatch(engine.ensureLoaded())
    }

    fun refresh() {
        job?.cancel()
        dispatch(engine.refresh())
    }

    /** 滑到底时调：取下一页。 */
    fun loadMore() {
        dispatch(engine.loadMore())
    }

    /** 读列表失败后点"重试"：接着取当前这一页。 */
    fun retry() {
        dispatch(engine.retry())
    }

    fun askUnfollow(target: FollowTarget) {
        engine.askUnfollow(target)
        syncView()
    }

    fun cancelUnfollow() {
        engine.cancelUnfollow()
        syncView()
    }

    /** 确认框里点了"取消关注"：**这是唯一会真的取关的入口**。 */
    fun confirmUnfollow() {
        val action = engine.confirmUnfollow()
        syncView()
        val target = action.unfollow ?: return
        runUnfollow(target)
    }

    fun dismissActionError() {
        engine.dismissActionError()
        syncView()
    }

    // ------------------------------------------------------------------ 内部

    /** 引擎说"去取一页"：真的取，结果喂回去（**只喂一次**，成功 / 失败二选一）。 */
    private fun dispatch(action: FollowingEngine.Action) {
        syncView()
        if (!action.wantsLoad) return
        val from = action.load
        job = scope.launch {
            val outcome = try {
                val page = withContext(io) { source.followings(from) }
                FollowingEngine.ListOutcome.Ok(page.items, page.next)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                FollowingEngine.ListOutcome.Failed(e.message ?: e::class.simpleName ?: "未知错误")
            }
            engine.onList(outcome)
            syncView()
        }
    }

    /** 真的取关（**只有 [confirmUnfollow] 会调到这里**）。失败记在 [actionError]，不重试。 */
    private fun runUnfollow(target: FollowTarget) {
        scope.launch {
            val outcome = try {
                withContext(io) { source.setFollowing(target, false) }
                FollowingEngine.UnfollowOutcome.Ok
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                FollowingEngine.UnfollowOutcome.Failed(e.message ?: e::class.simpleName ?: "未知错误")
            }
            engine.onUnfollow(target, outcome)
            syncView()
        }
    }

    private fun syncView() {
        val view = engine.view()
        val nextItems = engine.items()
        if (items != nextItems) items = nextItems
        if (loading != view.loading) loading = view.loading
        if (loaded != view.loaded) loaded = view.loaded
        if (hasMore != view.hasMore) hasMore = view.hasMore
        if (error != view.error) error = view.error
        if (actionError != view.actionError) actionError = view.actionError
        val nextBusy = view.busy
        if (busy != nextBusy) busy = nextBusy
        if (pendingUnfollow != view.pendingUnfollow) pendingUnfollow = view.pendingUnfollow
    }
}
