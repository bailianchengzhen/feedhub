package feedhub.app.perf

import feedhub.app.Tab
import feedhub.app.video.isVideoPost
import feedhub.core.FeedItem

/**
 * 现在在哪一页（任务：`screen` 从现有导航状态取）。
 *
 * 分两层：**基准页**由 `App` 按（页签 / 详情 / 搜索）算好写进来（[setBase]）；**覆盖层**（图片查看器、竖屏沉浸界面）
 * 各自在自己的组合里 [setOverlay] / 清掉——它们是盖在详情上的弹层，组合结束就没了，不会把基准页改坏。
 *
 * 只读一个 volatile 字符串：采样在 IO 线程上读，界面在主线程写，不加锁也不会读到半个值。
 */
object PerfScreen {
    @Volatile
    private var base: String = "首页"

    @Volatile
    private var overlay: String? = null

    /** 采样行里写的页面名（覆盖层优先）。 */
    val current: String get() = overlay ?: base

    fun setBase(name: String) {
        base = name
    }

    /** 图片查看器 / 沉浸播放这类盖在页面上的；null = 退出覆盖层。 */
    fun setOverlay(name: String?) {
        overlay = name
    }
}

/**
 * 页面名（纯函数，离线测）：任务列的那几种——首页 / 关注 / 我的 / 设置 / 详情-图文 / 详情-视频 / 搜索
 * （图片查看器和沉浸由 [PerfScreen] 的覆盖层补）。顺序：详情 > 搜索 > 页签——详情和搜索都是盖在当前页签上的。
 */
fun perfScreenName(tab: Tab, detail: FeedItem?, searchOpen: Boolean): String = when {
    detail != null -> if (detail.isVideoPost()) "详情-视频" else "详情-图文"
    searchOpen -> "搜索"
    tab == Tab.FOLLOWING -> "关注"
    tab == Tab.MINE -> "我的"
    tab == Tab.SETTINGS -> "设置"
    else -> "首页"
}
