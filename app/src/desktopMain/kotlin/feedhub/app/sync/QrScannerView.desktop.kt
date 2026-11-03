package feedhub.app.sync

import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier

/** 桌面没有摄像头，只出码不扫码（见 `LoginSync.canScan`），这个实现不会被组合；给个说清楚的占位。 */
@Composable
actual fun QrScannerView(onCode: (String) -> Unit, onError: (String) -> Unit, modifier: Modifier) {
    Box(modifier, contentAlignment = Alignment.Center) {
        Text("桌面版不能扫码", style = MaterialTheme.typography.bodyMedium)
    }
}
