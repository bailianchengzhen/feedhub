package feedhub.app.captcha

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable

/** 安卓：验证覆盖层打开时返回键 = 取消验证（和网页登录页的处理一致）。 */
@Composable
internal actual fun CaptchaBackHandler(enabled: Boolean, onBack: () -> Unit) {
    BackHandler(enabled = enabled, onBack = onBack)
}
