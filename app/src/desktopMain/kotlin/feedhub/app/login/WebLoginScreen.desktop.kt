package feedhub.app.login

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import feedhub.core.LoginSpec
import feedhub.core.Platform
import okhttp3.Cookie

/**
 * 桌面的「网页登录」**当前没有**：桌面走扫码（[feedhub.app.login.QrLoginDialog]）、设备码，或"从 Chrome 导入"
 *
 * 真被调用到就立刻当作"用户取消"（不画东西、不联网）：`AccountsPane` 只在 `graph.webLogin != null` 时
 * 才会显示这个入口，桌面组装里它是 null，所以正常情况下走不到这里。
 */
@Composable
actual fun WebLoginScreen(
    platform: Platform,
    login: LoginSpec,
    proxy: WebLoginProxyPlan,
    onDone: (List<Cookie>) -> Unit,
    onCancel: () -> Unit,
) {
    LaunchedEffect(platform) { onCancel() }
}
