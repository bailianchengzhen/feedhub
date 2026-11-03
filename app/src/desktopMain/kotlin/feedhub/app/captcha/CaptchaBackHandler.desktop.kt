package feedhub.app.captcha

import androidx.compose.runtime.Composable

/** 桌面没有返回键：验证覆盖层的取消就是右上角的 × / 底部的「取消」（见 [CaptchaOverlay]）。 */
@Composable
internal actual fun CaptchaBackHandler(enabled: Boolean, onBack: () -> Unit) {
    // 什么也不做（参数不用，保留和安卓实际实现一致的签名）
}
