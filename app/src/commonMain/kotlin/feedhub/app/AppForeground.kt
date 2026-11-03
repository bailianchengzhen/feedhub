package feedhub.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * App 现在在不在前台。
 *
 * 安卓上由 Activity 的 `onStart` / `onStop` 喂（`MainActivity` / `DebugStartActivity` 调
 * [feedhub.app.AppGraph.onForeground]）；桌面版不接，永远是 true（桌面没有"退到后台"这回事）。
 *
 * 用来做三件事（都在后台**真的停掉**，不靠"下次组合时再看一眼"）：
 * 1. 信息流刷新 / 翻页 / 预取：后台一个平台请求都不发（[feedhub.app.feed.FeedState.onBackground]）；
 * 2. 卡片图片预取：后台取消在飞的请求（`feed/Preload.kt`）；
 * 3. 刷新率请求：前台要最高、后台撤回。
 *
 * [isForeground] 是 Compose 状态，界面能据此重组；但**后台时安卓的帧时钟会停**，靠重组去停预取来不及，
 * 所以真正的取消动作走 [addListener] 的监听器——在 Activity 的 `onStop` 里同步执行。
 */
class AppForeground {
    var isForeground: Boolean by mutableStateOf(true)
        private set

    /**
     * 每次前后台变化 +1（Compose 状态）。界面把它放进 `LaunchedEffect` 的 key：回前台时哪怕 [isForeground] 又变回
     * true，key 也变了，只在后台停掉的预取才会重新起来（只靠 true/false 的话，前后台之间没有重组过就会一直停着）。
     */
    var generation by mutableIntStateOf(0)
        private set

    /** 变化监听器，**只在主线程增删 / 回调**（Compose 与 Activity 回调都在主线程；测试也是单线程）。 */
    private val listeners = mutableListOf<(Boolean) -> Unit>()

    /** 返回这次是不是真的变了（重复 onStart 不算）。 */
    fun set(active: Boolean): Boolean {
        if (isForeground == active) return false
        isForeground = active
        generation++
        // 复制一份再回调：监听器里可能会增删监听器
        for (listener in listeners.toList()) runCatching { listener(active) }
        return true
    }

    fun addListener(listener: (Boolean) -> Unit) {
        listeners += listener
    }

    fun removeListener(listener: (Boolean) -> Unit) {
        listeners -= listener
    }
}
