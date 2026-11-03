package feedhub.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.input.key.Key
import feedhub.core.FeedItem

/*
 * 盖在页签内容上的那几层。
 *
 * 原来只有**一个**"打开的那条"（`open[tab]`）：首页右栏和搜索页抢同一个槽，而且 `searchOpen` 一开
 * `showsTwoPane` 就变 false → 右栏那条详情被卸载、同时又作为整屏覆盖层画到搜索页上面。
 * 现在拆成两层：
 *  - **页签自己的**那条（[feedDetail]）：双栏右栏 / 全屏，随页签走，搜索页开关**一个字段都不碰**；
 *  - **搜索页自己的**那条（[searchDetail]）：从搜索结果点开的详情，只盖在搜索页上，关掉回搜索页原位置。
 *
 * [ScreenLayers] 是从这几个状态**算出来**的摆放（纯函数、可以照表测），`App` 只按它的结果画。
 */

/** 现在盖在页签内容上的东西（[App] 算好之后照着画）。 */
internal data class ScreenLayers(
    val wide: Boolean,
    val detailMode: DetailMode,
    /** 搜索页开着（整页盖在页签上，不换掉页签内容）。 */
    val searchOpen: Boolean,
    /** 页签（首页 / 关注 / 我的 / 设置）打开着的那一条，双栏时在右栏。 */
    val feedDetail: FeedItem?,
    /** 从搜索页点开的那一条（只在搜索页上，关掉就回搜索页）。 */
    val searchDetail: FeedItem?,
) {
    /** 页签的详情在不在双栏的右栏里（[showsTwoPane]，和搜索页无关）。 */
    val twoPane: Boolean get() = showsTwoPane(wide, detailMode)

    /**
     * 页签的详情现在是不是**全屏覆盖层**（盖在页签内容上）。
     * 双栏时不是（它在右栏）；**搜索页盖着时也不是**——那种时候它待在底下的右栏里不动，
     * 既不画到搜索页上面，也不卸载（卸载了关搜索页就得重建，视频会从头开始）。
     */
    val feedDetailFullScreen: FeedItem? get() = feedDetail?.takeIf { !twoPane && !searchOpen }

    /** 最上面那一条详情（Esc 先关谁、性能记录写哪一页）：搜索页盖着时只认搜索页自己的。 */
    val topDetail: FeedItem? get() = if (searchOpen) searchDetail else feedDetailFullScreen

    /** 页签内容算不算被盖住（行为数据"结算"，不是丢弃）：搜索页盖着，或者全屏详情盖着。 */
    val feedCovered: Boolean get() = searchOpen || (feedDetail != null && !twoPane)

    /**
     * 从这里能不能打开搜索页。全屏详情盖着首页时不能：那个详情会把搜索页盖住，
     * 而把它藏起来又等于把它卸载（回来要重建、播放从头开始）。标题行的放大镜本来也被详情盖着点不到，
     * 这条约束只对桌面快捷键（Ctrl+F / `/`）有意义。
     */
    val canOpenSearch: Boolean get() = !searchOpen && feedDetailFullScreen == null

    /** 性能记录里的页面名看哪条详情（搜索页盖着时它自己那条，否则页签那条——双栏右栏也算）。 */
    val perfDetail: FeedItem? get() = if (searchOpen) searchDetail else feedDetail
}

/**
 * 搜索页这一层的状态：**开关**和**它自己打开的那条详情**。
 *
 * 刻意**不碰**页签的 `open[tab]`（那不是它的字段）：搜索页盖着首页时点开一条结果，首页右栏那条
 * 原样留着；关掉搜索页回首页，右栏还是原来那条（不跳位置、不重开）。
 *
 * 只由主线程（Compose）改，所以不用加锁。纯状态类，`SearchLayersTest` 离线测。
 */
internal class SearchLayers(
    /** 截图工具用：一打开就停在搜索页上（`initialSearch`）。 */
    open: Boolean = false,
) {
    var isOpen: Boolean by mutableStateOf(open)
        private set

    /** 从搜索页点开的那一条（[isOpen] 为 false 时一定是 null）。 */
    var detail: FeedItem? by mutableStateOf(null)
        private set

    /** 打开搜索页（已经开着就什么都不做）；返回这次是不是真的打开了。 */
    fun open(): Boolean {
        if (isOpen) return false
        isOpen = true
        return true
    }

    /** 从搜索结果点开一条：盖在搜索页上，不动页签那条。 */
    fun openDetail(item: FeedItem) {
        detail = item
    }

    /** 关掉搜索页上的详情（回到搜索页，滚动位置 / 结果都还在）。 */
    fun closeDetail() {
        detail = null
    }

    /** 关掉搜索页：它自己那条详情一起关。页签那条详情**不归这里管**，一个字段都不碰。 */
    fun close() {
        detail = null
        isOpen = false
    }
}

/** 桌面快捷键要干的事（[appKeyAction] / [escapeAction] 的返回值）。 */
internal enum class AppKeyAction {
    /** 打开搜索页（Ctrl+F / `/`）。 */
    OPEN_SEARCH,

    /** 关掉最上面那条详情（Esc；里层自己没关的时候兜底，见 [escapeAction]）。 */
    CLOSE_DETAIL,

    /** 关掉搜索页（Esc）。 */
    CLOSE_SEARCH,

    /** 不处理，交给里层（详情 / 设置 / 输入框）或者什么都不做。 */
    NONE,
}

/**
 * 桌面快捷键里**打开搜索页**的那一半（`onPreviewKeyEvent`，先于里层）；纯逻辑，`ScreensTest` 离线测。
 *
 * - `Ctrl+F` / `/`：只在首页 / 关注、搜索页没开、也没有全屏详情盖着时打开搜索页（[ScreenLayers.canOpenSearch]）。
 *   设置页里有输入框（代理地址那类），`/` 不能抢——所以这两个都只在信息流那两页生效。
 * - `Esc` **不在这里**：它要先让里层（详情的大图查看器 / 楼中楼 / 放大 / 评论面板）过一遍，见 [escapeAction]。
 *
 * @param down 是不是按下那一下（只认 `KeyEventType.KeyDown`，按住不重复触发）
 * @param ctrl 按着 Ctrl（`/` 不要求）
 * @param searchOpen 搜索页开着吗
 * @param fullDetailOpen 有全屏详情盖在最上面吗（搜索页上的 / 页签的那条都算；双栏右栏那条不算）
 * @param feedTab 现在在首页 / 关注
 */
internal fun appKeyAction(
    key: Key,
    down: Boolean,
    ctrl: Boolean,
    searchOpen: Boolean,
    fullDetailOpen: Boolean,
    feedTab: Boolean,
): AppKeyAction = when {
    !down -> AppKeyAction.NONE
    (key == Key.F && ctrl) || key == Key.Slash -> {
        if (feedTab && !searchOpen && !fullDetailOpen) AppKeyAction.OPEN_SEARCH else AppKeyAction.NONE
    }
    else -> AppKeyAction.NONE
}

/**
 * Esc 兜底（`onKeyEvent`，**冒泡到最外层**才轮到这里；纯逻辑，`ScreensTest` 离线测）。
 *
 * 为什么要兜底：详情页自己也会处理 Esc（先关大图 → 楼中楼 → 返回上一层 → 关详情），但它得先**拿到焦点**。
 * 实测（截图工具里点卡片开详情）焦点会停在被点的卡片上，详情收不到 Esc —— 那时候按 Esc 什么都不发生，
 * 盖在搜索页上的详情关不掉、搜索页也就退不出去。冒泡这一层只在"里层没人消费"时才会收到，
 * 所以里层有东西要关（大图 / 放大 / 评论面板 / 楼中楼 / 设置子页）时轮不到它。
 *
 * 顺序和返回键逐层退一致：详情 → 搜索页。
 *
 * @param detailOpen 最上面有一条详情吗（搜索页开着时只看**搜索页自己那条**——页签右栏那条被搜索页盖着，
 *   不归这一次 Esc 管，见 [ScreenLayers.topDetail]）
 */
internal fun escapeAction(searchOpen: Boolean, detailOpen: Boolean): AppKeyAction = when {
    detailOpen -> AppKeyAction.CLOSE_DETAIL
    searchOpen -> AppKeyAction.CLOSE_SEARCH
    else -> AppKeyAction.NONE
}
