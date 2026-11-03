package feedhub.app.video

import androidx.compose.runtime.Composable

/** 桌面：全屏就是 App 内那层 Popup（Esc / 放大按钮进出），不碰系统栏、不转屏，行为不变。 */
@Composable
actual fun PlayerFullscreenEffect(active: Boolean, landscape: Boolean) {}
