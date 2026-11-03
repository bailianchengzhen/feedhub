package feedhub.app.video

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.core.FeedDetail
import feedhub.core.FollowKind
import feedhub.core.FollowSource
import feedhub.core.FollowState
import feedhub.core.FollowTarget
import feedhub.core.NotLoggedInException
import feedhub.present.RustPresent
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext

/**
 * 关注按钮的显示条件：
 * **平台实现了 [FollowSource]**（现在只有 B站 UP 主、贴吧吧）**且**详情给了关注状态（[FeedDetail.follow]）才显示；
 * 别的平台 / 没登录（接口没给状态）都没有这个按钮。
 *
 * 这里只把两件事实备齐。
 */
fun immersionFollowVisible(detail: FeedDetail?, source: FollowSource?): Boolean =
    RustPresent.immersionFollowVisible(detail?.follow != null, source != null)

/**
 * 沉浸界面右下 / 头像下面那个"关注"按钮的状态机（写操作只有关注 / 取关）。
 *
 * 约定和"我的 → 关注管理"（[feedhub.app.mine.FollowingState]）一致：
 * **取关必须先 [click]（弹确认框）→ [confirmUnfollowNow] 才真的调 `setFollowing`**，界面自己弹那个确认框；
 * 关注不是破坏性操作，点一下就直接调，不弹确认。失败了只提示一行字，不自动重试。
 * 状态只在 [scope]（界面是主线程）里改，方便离线测试。
 */
class ImmersionFollow(
    state: FollowState,
    private val source: FollowSource,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
    private val onMessage: (String) -> Unit = {},
) {
    val target: FollowTarget = state.target

    /** 现在是不是已关注。接口没给状态时是 null（显示成"未关注"，点一下去关注）。 */
    var following: Boolean? by mutableStateOf(state.following)
        private set

    /** 正在调接口（按钮暂时不好再点）。 */
    var busy: Boolean by mutableStateOf(false)
        private set

    /** 用户点了"已关注"、还没确认：界面弹确认框。 */
    var confirmUnfollow: Boolean by mutableStateOf(false)
        private set

    /** 按钮上的几个字（贴吧关注的是吧，写明）。文案在 Rust（`present::rules::follow_label`）。 */
    val label: String
        get() = RustPresent.followLabel(busy, following, target.kind == FollowKind.FORUM)

    /** 点一下按钮：没关注就直接关注（不弹确认）；已经关注了只是弹确认框，不调接口。 */
    fun click() {
        if (busy) return
        if (following == true) {
            confirmUnfollow = true
            return
        }
        set(true)
    }

    fun cancelUnfollow() {
        confirmUnfollow = false
    }

    /** 确认框里点了"取消关注"：这是唯一会真的取关的入口；没有待确认的（重复调用）什么都不做。 */
    fun confirmUnfollowNow() {
        if (!confirmUnfollow) return
        confirmUnfollow = false
        set(false)
    }

    private fun set(follow: Boolean) {
        if (busy) return
        busy = true
        scope.launch {
            try {
                withContext(io) { source.setFollowing(target, follow) }
                following = follow
                onMessage(RustPresent.followSuccessMessage(follow, target.name))
            } catch (e: CancellationException) {
                throw e
            } catch (e: NotLoggedInException) {
                onMessage(RustPresent.followLoginMessage(target.platform.label))
            } catch (e: Exception) {
                onMessage(RustPresent.followFailedMessage(e.message))
            } finally {
                busy = false
            }
        }
    }
}
