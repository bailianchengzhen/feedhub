package feedhub.app.ui

import androidx.compose.runtime.Composable

/** 桌面：没有系统返回键，什么都不做（保持加这个 expect 之前的行为）。 */
@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) {}
