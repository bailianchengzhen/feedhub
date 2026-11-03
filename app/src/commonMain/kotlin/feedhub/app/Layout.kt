package feedhub.app

import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/*
 * 窗口摆放：什么算"宽屏"、什么时候是双栏、全屏放大的时候转不转屏。
 *
 * 这几条都是纯函数，放在一处：界面（[App]、[feedhub.app.video.VideoDetailScreen]）只读结果，
 * 表格在 `LayoutTest` 里。以前"宽 ≥ 600dp"一个条件就把手机横屏（915×412dp）当成了平板：
 * 切到左侧图标栏 + 双栏，右栏被压到 445dp（比我们自己定的 [feedhub.app.feed.MinDetailWidth] 480dp 还窄），
 */

/** 宽屏布局的宽度门槛（Android 窗口尺寸类的 Medium 起点）：再窄就是手机竖屏 / 窄窗口，用底部导航栏。 */
val WideLayoutMinWidth = 600.dp

/**
 * 宽屏布局的高度门槛（Android 窗口尺寸类的 Compact 高度上限 480dp）：**高度不够也不算宽屏**。
 * 手机横屏是 915×412dp——宽度过了，高度没过，所以它仍然是手机（底部导航 + 单页，详情全屏）。
 */
val WideLayoutMinHeight = 480.dp

/**
 * 是不是宽屏布局：**宽 ≥ 600dp 且高 ≥ 480dp** 才用左侧竖排图标栏（[App] 的 `wide`）。
 *
 * 覆盖到的几档（[LayoutTest] 一张表盯住）：
 * - 手机竖屏 412×915 → 否；手机横屏 915×412 → 否（宽度够、高度不够）；
 * - 平板竖屏 800×1280 → 是；平板横屏 1280×800 → 是；
 * - 桌面窗口 1200×800 → 是；桌面窗口被拖得很矮（1200×400）→ 否（和手机横屏同一种摆放：底部导航 + 单页）。
 *
 * @param width / @param height 窗口（内容区）的 dp 宽高，直接传 `BoxWithConstraints` 的 `maxWidth` / `maxHeight`。
 */
fun isWideLayout(width: Dp, height: Dp): Boolean = width >= WideLayoutMinWidth && height >= WideLayoutMinHeight

/**
 * 详情现在摆在**双栏的右栏**里吗（[App] 的 `twoPane`）：宽屏布局 + 设置里「打开方式 = 双栏」。
 *
 * 它开着的时候双栏（和右栏那条详情）
 * 原样留在底下、什么都不动——关掉搜索页回到一模一样的状态（详情不跳位置、不重开、播放进度不丢）。
 * 以前 `searchOpen` 一开就把它压成 false，于是右栏那条详情被卸载、同时又以全屏覆盖层的身份画到
 * 搜索页上面。
 *
 * 手机（含横屏）永远是 false：宽 < 600 或高 < 480，[isWideLayout] 就已经挡住了。
 */
fun showsTwoPane(wide: Boolean, detailMode: DetailMode): Boolean =
    wide && detailMode == DetailMode.TWO_PANE

/**
 * 播放器"放大 / 全屏"要不要请求横屏（[feedhub.app.video.PlayerFullscreenEffect] 的 `landscape`）。
 *
 * 规则：
 * - **双栏里不转**（真平板 / 折叠屏展开态）：详情只在右栏，转屏会把详情挪进 `SplitPane`、组合身份变了，
 *   放大态跟着丢，等于刚进全屏就被踢出来。
 *   双栏里"放大"照旧只隐藏系统栏、铺满右栏，不扳手机。
 * - 详情铺满整窗（手机竖屏 / 手机横屏 / 「打开方式 = 全屏」）时：**横屏视频**才转屏，
 *   竖屏视频放大再扳横没有意义（沉浸界面里竖向视频撑满高，放大只会裁掉上下）。
 * - 沉浸界面里只认这条视频自己是不是横屏：沉浸界面的"横屏全屏"按钮只在横屏视频上出现
 *   （[feedhub.app.video.isLandscapeVideo]），点它就是放大 + 转横屏；竖屏视频点不到这个按钮。
 *
 * @param zoomed 现在在不在放大 / 全屏里（[feedhub.app.detail.DetailHost.playerZoomed]）。
 * @param twoPane 详情在不在双栏的右栏里（[showsTwoPane]）。
 * @param immersive 现在是不是沉浸界面（竖屏视频的抖音式界面）。
 * @param landscapeVideo 这条视频本身是不是横屏（宽 > 高）。
 */
fun requestsLandscapeFullscreen(
    zoomed: Boolean,
    twoPane: Boolean,
    immersive: Boolean,
    landscapeVideo: Boolean,
): Boolean = zoomed && !twoPane && (!immersive || landscapeVideo)

/**
 * 这条详情现在是不是摆在双栏的右栏里（给详情里面的界面读）。
 *
 * [App] 在 `SplitPane` 的右栏里提供 `true`，全屏详情的覆盖层不提供（默认 `false` = 铺满整窗）。
 * [feedhub.app.video.VideoDetailScreen] 用它决定全屏放大要不要转屏（[requestsLandscapeFullscreen]）：
 * 双栏的右栏里转屏会丢掉放大态（组合身份变了），铺满整窗时不会。
 */
val LocalDetailInRightPane = compositionLocalOf { false }
