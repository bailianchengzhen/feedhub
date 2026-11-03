package feedhub.app.captcha

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * 详情页里嵌的人机验证浏览器。
 *
 * - 桌面：JCEF（CEF 135）离屏渲染，组件塞进详情页的覆盖层；页面来自本机临时 HTTP 服务；
 * - 安卓：系统 WebView；页面不来自本机服务，而是 [CaptchaPageHost] 在自己的地址上直接返回（见
 *   `AndroidCaptchaHandler`）——WebView 里除了易盾自己的脚本，**一个字节都不出设备**。
 *
 * [onError] 在准备失败（CEF 下载 / 初始化 / 加载）时回调，覆盖层据此显示「用系统浏览器打开」的退路。
 * 实现方负责在离开组合时释放浏览器。
 */
@Composable
expect fun CaptchaBrowser(
    url: String,
    modifier: Modifier = Modifier,
    onError: (String) -> Unit = {},
    /** 验证页 / proof 回传从哪来（安卓 WebView 拦截；桌面走本机 HTTP 服务，不需要）。 */
    host: CaptchaPageHost? = null,
)
