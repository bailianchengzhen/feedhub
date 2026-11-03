package feedhub.app.captcha

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * 评论验证码弹框的状态。由平台实现（桌面端 `DesktopCaptchaHandler`）写，[feedhub.app.App] 里读。
 *
 * 不放业务字段：验证码 id / proof 都不进这里（proof 只在 Source 和 Cookie 罐里），
 * 这里只给用户看"哪个平台、为什么、页面地址、现在到哪一步"。
 */
class CaptchaPromptState {
    var visible by mutableStateOf(false)
        internal set
    var platformLabel by mutableStateOf("")
        internal set
    /** 服务端原话 + 一句解释。 */
    var message by mutableStateOf("")
        internal set
    /** 本地验证页地址（浏览器打开的那个）。 */
    var url by mutableStateOf<String?>(null)
        internal set
    /** 当前状态：已打开浏览器 / 等待验证 / 自动回传失败等。 */
    var status by mutableStateOf("")
        internal set

    internal fun show(platformLabel: String, message: String, url: String?, status: String) {
        this.platformLabel = platformLabel
        this.message = message
        this.url = url
        this.status = status
        visible = true
    }

    internal fun updateStatus(status: String) {
        this.status = status
    }

    internal fun hide() {
        visible = false
        url = null
        status = ""
    }
}
