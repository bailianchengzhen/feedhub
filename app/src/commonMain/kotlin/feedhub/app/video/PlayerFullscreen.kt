package feedhub.app.video

import androidx.compose.runtime.Composable

/**
 * 播放器"放大 / 全屏"时对窗口做的事。**桌面是空实现**（全屏就是 App 内那层 Popup，行为不变）。
 *
 * 安卓上（[PlayerFullscreenEffect] 的 actual）：
 * - 进全屏**隐藏系统栏**（状态栏 + 手势条），边缘滑动可以临时把它们叫出来（`BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`）；
 *   退出全屏恢复显示；
 * - **横屏视频**进全屏时请求横屏（`SENSOR_LANDSCAPE`），退出时还原成进来之前的 `requestedOrientation`
 *   （两个 Activity 的清单都声明了 `configChanges=orientation|screenSize…`，转屏不会重建界面、播放不中断）；
 * - 竖屏视频（沉浸界面放大）只隐藏系统栏、**不转屏**。
 *
 * @param active 现在在不在"放大 / 全屏"里。
 * @param landscape 这个视频是横屏视频吗（竖屏视频不转屏）。
 */
@Composable
expect fun PlayerFullscreenEffect(active: Boolean, landscape: Boolean)
