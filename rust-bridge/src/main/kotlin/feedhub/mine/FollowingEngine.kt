package feedhub.mine

import feedhub.core.Cursor
import feedhub.core.FollowTarget
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiAcctFollowAction
import uniffi.feedhub_ffi.FfiAcctFollowState
import uniffi.feedhub_ffi.FfiAcctFollowTarget
import uniffi.feedhub_ffi.FfiAcctFollowView
import uniffi.feedhub_ffi.FfiAcctListOutcome
import uniffi.feedhub_ffi.FfiAcctUnfollowOutcome

/**
 * **关注管理的状态机**（`rust/crates/feedhub-core/src/feed/following.rs`）的 Kotlin 门面。
 *
 *
 * **写操作的红线在这里**：取关只有一条路 [`askUnfollow`] → 界面弹确认框 → [`confirmUnfollow`]；
 * 只有 `confirmUnfollow` 返回的 [`Action.unfollow`] 非空，宿主才许调 `setFollowing(target, false)`。
 * 读列表、翻页、重试都不会碰它，失败了也**不自动重试**。
 *
 * Kotlin 剩下的：真正的 `followings(cursor)` / `setFollowing(...)`（OkHttp）、Compose 状态容器、协程、确认框界面。
 */
class FollowingEngine : AutoCloseable {

    data class Action(
        /** `Some(cursor)` = 去取一页（`null` 游标取第一页）；`null` = 不发请求。 */
        val load: Cursor?,
        val wantsLoad: Boolean,
        /** **非空 = 宿主去取关这一个**（只有确认之后才可能非空）。 */
        val unfollow: FollowTarget?,
        val itemsChanged: Boolean,
        val started: Boolean,
    )

    data class View(
        val loading: Boolean,
        val loaded: Boolean,
        val hasMore: Boolean,
        val error: String?,
        val actionError: String?,
        val busy: Set<String>,
        val pendingUnfollow: FollowTarget?,
        val itemCount: Int,
    )

    sealed interface ListOutcome {
        data class Ok(val items: List<FollowTarget>, val next: Cursor?) : ListOutcome
        data class Failed(val message: String) : ListOutcome
    }

    sealed interface UnfollowOutcome {
        data object Ok : UnfollowOutcome
        data class Failed(val message: String) : UnfollowOutcome
    }

    init { RustNative.ensureLoaded() }

    private val native: FfiAcctFollowState = FfiAcctFollowState()

    fun ensureLoaded(): Action = load { native.ensureLoaded().toCore() }

    fun refresh(): Action = load { native.refresh().toCore() }

    fun loadMore(): Action = load { native.loadMore().toCore() }

    fun retry(): Action = load { native.retry().toCore() }

    fun onList(outcome: ListOutcome): Action = load { native.onList(outcome.toFfi()).toCore() }

    /** 用户点了某项的"取关"、还没确认：界面弹确认框。 */
    fun askUnfollow(target: FollowTarget) = load { native.askUnfollow(target.toFfi()) }

    fun cancelUnfollow() = load { native.cancelUnfollow() }

    /** 确认框里点了"取消关注"：**唯一**会让宿主真的调 `setFollowing(target, false)` 的入口。 */
    fun confirmUnfollow(): Action = load { native.confirmUnfollow().toCore() }

    fun onUnfollow(target: FollowTarget, outcome: UnfollowOutcome): Action =
        load { native.onUnfollow(target.toFfi(), outcome.toFfi()).toCore() }

    fun dismissActionError() = load { native.dismissActionError() }

    fun items(): List<FollowTarget> = load { native.items().map { it.toCore() } }

    fun view(): View = load { native.view().toCore() }

    override fun close() {
        native.close()
    }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    private fun FollowTarget.toFfi(): FfiAcctFollowTarget = FfiAcctFollowTarget(
        platform = platform.key,
        kind = kind.name,
        id = id,
        name = name,
        avatar = avatar,
        description = description,
        url = url,
    )

    private fun FfiAcctFollowTarget.toCore(): FollowTarget = FollowTarget(
        platform = feedhub.core.Platform(platform),
        kind = runCatching { feedhub.core.FollowKind.valueOf(kind) }.getOrDefault(feedhub.core.FollowKind.USER),
        id = id,
        name = name,
        avatar = avatar,
        description = description,
        url = url,
    )

    private fun ListOutcome.toFfi(): FfiAcctListOutcome = when (this) {
        is ListOutcome.Ok -> FfiAcctListOutcome.Ok(items.map { it.toFfi() }, next?.value)
        is ListOutcome.Failed -> FfiAcctListOutcome.Failed(message)
    }

    private fun UnfollowOutcome.toFfi(): FfiAcctUnfollowOutcome = when (this) {
        UnfollowOutcome.Ok -> FfiAcctUnfollowOutcome.Ok
        is UnfollowOutcome.Failed -> FfiAcctUnfollowOutcome.Failed(message)
    }

    private fun FfiAcctFollowAction.toCore(): Action = Action(
        load = loadCursor?.let { Cursor(it) },
        wantsLoad = wantsLoad,
        unfollow = unfollow?.toCore(),
        itemsChanged = itemsChanged,
        started = started,
    )

    private fun FfiAcctFollowView.toCore(): View = View(
        loading = loading,
        loaded = loaded,
        hasMore = hasMore,
        error = error,
        actionError = actionError,
        busy = busy.toSet(),
        pendingUnfollow = pendingUnfollow?.toCore(),
        itemCount = itemCount.toInt(),
    )
}
