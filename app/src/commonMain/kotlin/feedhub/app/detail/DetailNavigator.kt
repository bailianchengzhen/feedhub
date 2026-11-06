package feedhub.app.detail

import androidx.compose.runtime.Stable
import androidx.compose.runtime.compositionLocalOf
import feedhub.app.video.PlayPrefetcher
import feedhub.core.FeedItem

/**
 * 沉浸播放要用的"来源列表"：从哪个信息流点进来的、上一条 / 下一条是谁、
 * 换了之后怎么换。
 *
 * 由 `App`（`app/src/commonMain/.../App.kt`）按当前页签提供：首页 / 关注页给对应的 `FeedState`，
 * 别的页签（收藏 / 历史 / 设置 / 搜索）没有信息流列表就是 null——沉浸界面照样画，只是不给上下切换
 * （队列退化成只有当前这一条，见 [feedhub.app.video.ImmersionQueue]）。
 *
 * 换一条 = 换 App 里当前页签打开着的详情（`open[tab] = it`），于是详情栈重置、历史记一条、
 * 双栏时左栏列表跟着选中并滚到它；退出详情就回到来源列表里最后看的那一条。
 *
 * @param select 换成另一条内容（沉浸界面上下滑动时调）。**自带节流**（[selectMinGapMs]）：滚轮连发 /
 *   触摸抖动不会一口气翻好几条。
 * @param loadMore 到列表尾部时叫来源列表取下一页，返回"真的发起了"。
 * @param loadingMore 来源列表是不是正在取下一页（"正在加载更多"和"已经是最后一条"据此分开）。
 * @param prefetcher 只预取下一条的详情 + 播放地址（见 [feedhub.app.video.PlayPrefetcher]）。
 * [switched]：这个详情会话已经在沉浸界面里切换过条目。[select] 时置上，从信息流重新打开一条 / 关掉详情时 [resetSwitched]。
 */
@Stable
class DetailNavigator(
    val items: () -> List<FeedItem>,
    private val onSelect: (FeedItem) -> Unit,
    val loadMore: () -> Boolean = { false },
    val loadingMore: () -> Boolean = { false },
    val prefetcher: PlayPrefetcher? = null,
    private val now: () -> Long = { System.currentTimeMillis() },
) {
    private var lastSelectAt: Long? = null

    /** 这个详情会话已经在沉浸界面里切换过条目（见类注释）。 */
    var switched: Boolean = false
        private set

    /**
     * 真正换条目。返回这次有没有真的换。
     *
     * @param throttled 滚轮 / 键盘连发传 true（[selectMinGapMs] 内只认一次）；触屏整屏划页结算传 false，
     *   避免用户快速连划跳过两条视频时第二下被节流吞掉。
     */
    fun select(item: FeedItem, throttled: Boolean = true): Boolean {
        val t = now()
        val last = lastSelectAt
        if (throttled && last != null && t - last < selectMinGapMs) return false
        lastSelectAt = t
        switched = true
        onSelect(item)
        return true
    }

    /** 从信息流重新打开一条 / 关掉详情：下一个详情会话重新按宽高比判定进不进沉浸界面。 */
    fun resetSwitched() {
        switched = false
    }

    companion object {
        /** 两次换条目至少隔这么久（滚轮一格几毫秒连发，不隔开就会一口气翻几条）。 */
        const val selectMinGapMs = 350L
    }
}

/** 当前详情页的来源列表；不在信息流里（收藏 / 历史 / 搜索 / 独立打开）就是 null。 */
val LocalDetailNavigator = compositionLocalOf<DetailNavigator?> { null }
