package feedhub.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable

/** 安卓：交给 [BackHandler]（在 `MainActivity` 这个 ComponentActivity 里，它的 dispatcher 一定是有的）。 */
@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) {
    BackHandler(enabled = enabled, onBack = onBack)
}
