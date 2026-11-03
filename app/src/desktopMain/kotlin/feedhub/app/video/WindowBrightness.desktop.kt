package feedhub.app.video

import androidx.compose.runtime.Composable

/** 桌面：播放器手势只在触屏上开（[isTouchDevice] = false），这里什么都不接，行为不变。 */
@Composable
actual fun rememberWindowBrightness(): WindowBrightness = NoWindowBrightness

actual val isTouchDevice: Boolean = false
