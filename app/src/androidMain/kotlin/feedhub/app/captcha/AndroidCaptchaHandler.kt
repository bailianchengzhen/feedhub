package feedhub.app.captcha

import android.os.Handler
import android.os.Looper
import feedhub.app.login.WebViewProxyGate
import feedhub.core.CaptchaChallenge
import feedhub.core.Platform
import feedhub.core.plugin.CaptchaPage
import feedhub.plugins.InstalledPlugins
import kotlinx.coroutines.Dispatchers

/**
 * 安卓端的人机验证处理（酷安评论的网易易盾）。语义和桌面**完全一致**（同一时间只开一个验证页、后来的请求等
 * 同一份 proof、取消 / 超时返回 null、proof 不进日志）：排队 / 取消 / 超时 / 校验都在 [CaptchaHandlerBase]，
 * 这个类只补安卓的两处不同：
 *
 * 1. **没有本机 HTTP 服务**：[mountPage] 只给出一个不监听的固定本机地址
 *    （[PAGE_URL]），页面 HTML 和 proof 回传由 [CaptchaBrowser] 的 `WebViewClient.shouldInterceptRequest`
 *    经 [CaptchaPageHost] 直接截下来。因为地址固定、不绑定端口，**不占端口、也不需要 `INTERNET` 之外的任何东西**；
 * 2. **网页登录的代理覆盖是进程级的**：登录页开着时 `ProxyController` 的覆盖还生效，那时弹验证码会让一个国内页面
 *    走境外代理（或者干脆加载失败）。所以 [refuseReason] 先问 [WebViewProxyGate]，还开着就**不弹**，
 *    `solve` 返回 null（Source 会报"已取消人机验证"，用户关掉登录页再重试评论即可）。
 *    这一版不排队等登录页关（等多久都说不清），
 *
 * 页面本身**不改、不注入脚本、不自动点**：易盾的脚本照它自己的域名加载（国内直连），用户本人在里面完成。
 *
 * @param prompt 覆盖层的状态（App 读它决定要不要画 [CaptchaOverlay]）。
 * @param pages 各平台的验证页（插件描述的 `captchaPage`，现在只有酷安）；测试 / 演示可以换。
 * @param webLoginProxyActive 网页登录的 WebView 代理覆盖还在吗（测试可换）。
 */
class AndroidCaptchaHandler(
    prompt: CaptchaPromptState = CaptchaPromptState(),
    pages: (Platform) -> CaptchaPage? = { InstalledPlugins.catalog[it]?.captchaPage },
    timeoutMs: Long = CaptchaHandlerBase.DEFAULT_TIMEOUT_MS,
    private val webLoginProxyActive: () -> Boolean = { WebViewProxyGate.isActive },
) : CaptchaHandlerBase(prompt, Dispatchers.Main, pages, timeoutMs), CaptchaPageHost {

    // 安卓的 WebView 在 shouldInterceptRequest 里走这两个（页面 / proof 回传都不出设备）
    override fun pageFor(url: String): String? = currentPageHtml(url)

    override fun submit(url: String): Boolean = acceptSubmit(url)

    override suspend fun mountPage(
        challenge: CaptchaChallenge,
        page: CaptchaPage,
        submitUrl: String,
    ): MountedPage = MountedPage(url = PAGE_URL, html = page.html(challenge.captchaId, submitUrl))

    override fun refuseReason(): String? =
        if (webLoginProxyActive()) "网页登录还开着，WebView 的代理覆盖还没清；先关掉登录页再试" else null

    /**
     * `shouldInterceptRequest` 在 WebView 自己的线程上，覆盖层一收就会拆 WebView；所以先让 200 交回去，
     * 回主线程再唤醒 `solve`（覆盖层的显示 / 收起本来就只能在主线程改）。
     */
    override fun deliverProof(proof: String) {
        Handler(Looper.getMainLooper()).post { completeProof(proof) }
    }

    companion object {
        /**
         * 验证页所在的"本机地址"。**没有服务在这个端口上监听**：WebView 的请求在
         * `shouldInterceptRequest` 里就被换成了页面 / 200 响应，所以它只是给页面一个正常的 origin
         * （`fetch` 是相对地址，同源）。选一个高位端口，避免和系统 / 调试服务撞上。
         */
        const val PORT: Int = 47_813
        const val ORIGIN: String = "http://127.0.0.1:$PORT"

        /** 浏览器加载的验证页地址（路径固定；token 在页面里的回传地址上，不在这个地址上）。 */
        const val PAGE_URL: String = "$ORIGIN/captcha"
    }
}
