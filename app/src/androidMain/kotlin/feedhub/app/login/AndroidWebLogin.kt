package feedhub.app.login

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.core.LoginSpec
import feedhub.core.Platform

/**
 * 安卓的「网页登录」：只是"现在在登录哪个平台"这个状态。真正的 WebView 在 [WebLoginScreen] 的安卓实现里
 * （`androidMain` 的 `WebLoginScreen.android.kt`）：`App` 读 [platform]，非 null 就铺开登录页覆盖层。
 *
 * 这里不放 Context / WebView：界面在 Compose 里拿 `LocalContext`，状态保持得越干净越好
 * （换 Activity、配置变化都不会漏一个 View）。
 *
 * [clearCookies] 是退出登录时清 WebView 里的 Cookie（[AndroidWebCookies]）：只删这个平台域（含子域）。
 */
class AndroidWebLogin : WebLogin {
    override var platform: Platform? by mutableStateOf(null)
        private set

    override fun start(platform: Platform) {
        this.platform = platform
    }

    override fun cancel() {
        platform = null
    }

    override fun clearCookies(login: LoginSpec, hosts: List<String>) {
        AndroidWebCookies.clear(login, hosts)
    }
}
