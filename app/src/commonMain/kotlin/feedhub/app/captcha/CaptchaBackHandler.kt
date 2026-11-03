package feedhub.app.captcha

import androidx.compose.runtime.Composable

/**
 * 验证覆盖层打开时的返回键 = 取消验证（安卓）。桌面没有这个键，是空实现。
 *
 * 放进 commonMain 是因为覆盖层 [CaptchaOverlay] 是共用的；平台差异只有"有没有返回键"。
 */
@Composable
internal expect fun CaptchaBackHandler(enabled: Boolean, onBack: () -> Unit)
