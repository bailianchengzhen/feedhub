package feedhub.app.captcha

import android.annotation.SuppressLint
import android.graphics.Color
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature
import java.io.ByteArrayInputStream

/**
 * 安卓端的验证码浏览器：系统 WebView（和网页登录同一套）。
 *
 * 页面从哪来：**不加载任何网上的东西**。WebView 打开 [feedhub.app.captcha.AndroidCaptchaHandler.PAGE_URL]
 * 那个固定的本机地址，`WebViewClient.shouldInterceptRequest` 问 [host]（就是 `AndroidCaptchaHandler`）：
 * - 当前验证页 → 直接返回插件生成的 HTML（[CaptchaPageHost.pageFor]）；
 * - 页面把 proof `fetch` 回 `/proof?token=…&value=…` → 收下、返回 `200 OK`（[CaptchaPageHost.submit]）；
 * - 别的地址（易盾从 `cstaticdun.126.net` 拿脚本、iframe 里的静态资源）→ 返回 null，照常联网。
 *
 * 也就是说**拦截范围只有当前验证页的 scheme + host + port + path**（见 `CaptchaHandlerBase.pageFor`）：
 * WebView 里就算有别的页面，也碰不到我们的地址。
 *
 * 规矩不变：不注入脚本、不改页面、不自动点；页面自己就是插件里的那颗 [feedhub.core.plugin.CaptchaPage]，
 * 用户在易盾的滑块里自己完成。
 *
 * 离开组合（成功 / 取消 / 超时，覆盖层消失）时 `destroy()` 掉 WebView。
 */
@SuppressLint("SetJavaScriptEnabled")
@Composable
actual fun CaptchaBrowser(
    url: String,
    modifier: Modifier,
    onError: (String) -> Unit,
    host: CaptchaPageHost?,
) {
    val context = LocalContext.current
    val webView = remember(url, host) {
        val pageUrl = url.substringBefore('#')
        WebView(context).apply {
            setBackgroundColor(Color.WHITE)
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            // 验证页不开新窗口、不自动弹窗：滑块在同一个页面里（和网页登录一致）
            settings.javaScriptCanOpenWindowsAutomatically = false
            settings.setSupportMultipleWindows(false)
            settings.cacheMode = WebSettings.LOAD_NO_CACHE
            hideRequestedWith(this)
            webViewClient = object : WebViewClient() {
                override fun shouldInterceptRequest(
                    view: WebView,
                    request: WebResourceRequest,
                ): WebResourceResponse? {
                    val target = request.url.toString()
                    // 只认当前验证页那个地址（host 为空 / 地址不对都返回 null，走正常网络）
                    host?.pageFor(target)?.let { html ->
                        return WebResourceResponse(
                            "text/html",
                            "utf-8",
                            200,
                            "OK",
                            mapOf("Cache-Control" to "no-store"),
                            ByteArrayInputStream(html.toByteArray(Charsets.UTF_8)),
                        )
                    }
                    if (host?.submit(target) == true) {
                        return WebResourceResponse(
                            "text/plain",
                            "utf-8",
                            200,
                            "OK",
                            mapOf("Cache-Control" to "no-store"),
                            ByteArrayInputStream("OK".toByteArray(Charsets.UTF_8)),
                        )
                    }
                    return null
                }

                override fun onReceivedError(
                    view: WebView,
                    request: WebResourceRequest,
                    error: android.webkit.WebResourceError,
                ) {
                    if (request.isForMainFrame) {
                        // 文案只要原因，不带地址（地址里可能有回传 token；proof 更不在地址里）
                        onError("${error.errorCode} ${error.description}")
                    }
                }

                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    // 主框架只能停在验证页上：页面里的链接 / 重定向
                    // 不能把这个 WebView 带去别的网站。proof 是 fetch 回传的，不走导航，不受影响
                    if (request.isForMainFrame) return request.url.toString().substringBefore('#') != pageUrl
                    // 子框架（易盾的 iframe）只放行 http/https；别的 scheme 丢掉，不弹别的 App（和网页登录一致）
                    val scheme = request.url.scheme?.lowercase()
                    return scheme != "http" && scheme != "https"
                }

                @Deprecated("老 API 的签名，一起拦住")
                override fun shouldOverrideUrlLoading(view: WebView, url: String): Boolean {
                    // 老 API 分不出主 / 子框架：按主框架处理，只认验证页本身
                    return url.substringBefore('#') != pageUrl
                }
            }
            loadUrl(url)
        }
    }
    DisposableEffect(webView) {
        onDispose { runCatching { webView.destroy() } }
    }
    AndroidView(modifier = modifier, factory = { webView })
}

/**
 * 不让 WebView 给每个请求加 `X-Requested-With: <包名>`（空白名单 = 哪个站都不发），和网页登录一样。
 * 老 WebView 不支持这个开关就算了。
 */
private fun hideRequestedWith(view: WebView) {
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.REQUESTED_WITH_HEADER_ALLOW_LIST)) return
    runCatching { WebSettingsCompat.setRequestedWithHeaderOriginAllowList(view.settings, emptySet()) }
}
