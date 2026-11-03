package feedhub.app.login

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Color
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.webkit.CookieManager
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.webkit.ProxyConfig
import androidx.webkit.ProxyController
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature
import feedhub.core.LoginSpec
import feedhub.core.Platform
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.Cookie
import java.util.concurrent.Executor
import kotlin.coroutines.resume

/**
 * 安卓的「网页登录」界面：**系统 WebView** 打开平台官方登录页（[LoginSpec.webLoginUrl]），
 * 用户自己在页面里登录 / 扫码；我们每 [POLL_MS] 看一眼 `CookieManager`，**关键 Cookie**
 * （[LoginSpec.requiredCookies]）齐了、而且**和页面加载完 3 秒后的基线比变了**（[WebLoginCookies.isNewLogin]）
 * 就 [onDone]（交给 `AccountsState.webLoginDone` 写进 Cookie 罐），然后覆盖层关掉。
 *
 * 为什么要基线：小红书的 `a1` / `web_session` 在**游客态**就有，只看"齐了"会在页面刚打开时把游客 Cookie
 * 当登录、把登录页关掉。自动判定没触发时（例如 WebView 里本来就有全套 Cookie），
 * 关键 Cookie 齐了之后右上角会出现「完成登录」，用户自己点一下保存。
 *
 * 守规矩的地方：
 * - **不注入脚本、不自动填表、不点任何按钮**：只有页面自己的 JS 在跑（那些登录页本来就靠 JS 渲染，
 *   所以 `javaScriptEnabled` 打开；这不是"我们注入脚本"）；
 * - UA 一律用**设备默认的**，不换桌面版：WebView 的 Client Hints / 屏幕 / 触摸都是手机的，只换 UA 会变成
 *   "自称电脑、其他全是手机"的矛盾指纹；
 * - 不发 `X-Requested-With: <包名>` 头（[hideRequestedWith]）：它只说明"这是某个 App 里的 WebView"，平台用不上，
 *   风控却能用；系统 Chrome 不发它；
 * - 链接都留在本 WebView 里（`shouldOverrideUrlLoading` 只放行 http/https，别的 scheme 直接丢掉，免得弹出别的 App）；
 * - **代理 fail-closed**：打开页面前按 [WebLoginProxyPlan] 给 WebView 设 `ProxyController` 覆盖；
 *   [WebLoginProxyPlan.Blocked] / WebView 不支持 / 设置超时 → **一个字节都不加载**，只显示原因；
 * - 判定 / 拷贝的规则是纯函数（[WebLoginCookies]，桌面 JVM 上有单测），这里只负责"问 CookieManager"和画界面。
 *
 * 界面是**整屏覆盖层**（不再是 Compose `Dialog`）：软键盘弹出时 `imePadding()` 会把 WebView 一起缩上去
 * （Dialog 是另一个窗口，Android 15+ 的边到边模式下 ADJUST_RESIZE 不跟着 Compose 的 insets 走，密码框 /
 * 验证码会被键盘挡住）；返回键也在这里接（网页能后退就后退，不能才关）。
 *
 * 关闭（用户点"关闭"、或者登录完成）就把 WebView `destroy()` 掉，并清掉 WebView 的代理覆盖
 * （`ProxyController` 是**进程级**的，不清会漏给下次打开的任何 WebView —— 安卓的验证码 WebView 也一样）。
 * **没有登录完成时什么都不写盘**。
 */
@SuppressLint("SetJavaScriptEnabled")
@Composable
actual fun WebLoginScreen(
    platform: Platform,
    login: LoginSpec,
    proxy: WebLoginProxyPlan,
    onDone: (List<Cookie>) -> Unit,
    onCancel: () -> Unit,
) {
    val context = LocalContext.current
    var webView by remember(platform) { mutableStateOf<WebView?>(null) }
    var gate by remember(platform) { mutableStateOf<Gate>(Gate.Preparing) }
    var attempt by remember(platform) { mutableIntStateOf(0) }
    // 页面加载完成（第一次）的时刻：基线 Cookie 在这之后 [BASELINE_SETTLE_MS] 采（给页面自己的 JS 一点时间，
    // 免得把"游客 Cookie"当成登录后的新值；见 [WebLoginCookies.isNewLogin]）
    var pageFinishedAt by remember(platform) { mutableStateOf(0L) }
    var baseline by remember(platform) { mutableStateOf<Map<String, String>?>(null) }
    var complete by remember(platform) { mutableStateOf(false) }
    val url = login.webLoginUrl

    // 0) WebView 先建出来（代理覆盖要求在 WebView 初始化之后设），**但先不放进组合**：
    //    - 代理没就绪时它一个字节都不会加载；
    //    - 放进去的时机也会影响 Compose 给 AndroidView 挖的洞：实测在 factory 里写 state（先挂上、后挪位置）
    //      会让挖孔盖住标题栏（酷安登录页出现过标题 / 关闭按钮看不见、但无障碍树里还在）。所以改成
    //      "effect 里建好 → 代理就绪 → 再交给 AndroidView 显示"。
    LaunchedEffect(platform, attempt) {
        gate = Gate.Preparing
        clearWebViewProxy()
        webView = webView ?: createLoginWebView(context) { if (pageFinishedAt == 0L) pageFinishedAt = System.currentTimeMillis() }
        when (val plan = proxy) {
            is WebLoginProxyPlan.Blocked -> gate = Gate.Failed(plan.message)
            WebLoginProxyPlan.Direct -> gate = Gate.Ready
            is WebLoginProxyPlan.Override -> {
                if (!WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) {
                    // 老 WebView 没有 ProxyController 支持：境外平台不能没代理就打开（红线），直接拒绝
                    gate = Gate.Failed(
                        "这台设备的 WebView 不支持给网页单独设代理（版本太老）。为了不直连境外平台，页面没有加载。" +
                            "可以先用桌面版「从 Chrome 导入」登录。",
                    )
                } else {
                    val ok = applyWebViewProxy(plan)
                    if (ok) {
                        gate = Gate.Ready
                    } else {
                        // 超时 / 失败：把可能已经生效的覆盖清掉，绝不落到直连
                        clearWebViewProxy()
                        gate = Gate.Failed("给 WebView 设置代理失败（超时）。为了不直连，页面没有加载；点「重试」再来一次。")
                    }
                }
            }
        }
    }

    // 1) 代理就绪之后才加载页面（保证 setProxyOverride 已经生效）
    LaunchedEffect(gate, url) {
        if (gate !is Gate.Ready) return@LaunchedEffect
        var view = webView
        var waited = 0
        while (view == null && waited < 2000) {
            delay(20)
            waited += 20
            view = webView
        }
        view?.loadUrl(url)
    }

    // 2) 关键 Cookie 一个不少、而且**和页面刚打开时的基线比变了**才交出去（只比较值，不打印 / 不落盘）
    LaunchedEffect(platform, gate) {
        if (gate !is Gate.Ready) return@LaunchedEffect
        while (true) {
            delay(POLL_MS)
            val cookies = readLoginCookies(login)
            complete = WebLoginCookies.isComplete(login.requiredCookies, cookies)
            if (pageFinishedAt == 0L) continue
            val base = baseline
            if (base == null) {
                // 页面加载完之后再等一小会儿采基线：小红书的 a1 / web_session 游客态就会种，
                // 采得太早会把它们当成"登录后的 Cookie"
                if (System.currentTimeMillis() - pageFinishedAt >= BASELINE_SETTLE_MS) {
                    baseline = WebLoginCookies.values(login.requiredCookies, cookies)
                }
                continue
            }
            if (WebLoginCookies.isNewLogin(login.requiredCookies, base, cookies)) {
                onDone(cookies)
                return@LaunchedEffect
            }
        }
    }

    // 3) 关掉覆盖层 / 换平台：销毁 WebView、清代理（ProxyController 是进程级的）
    DisposableEffect(platform) {
        onDispose {
            clearWebViewProxy()
            runCatching { webView?.destroy() }
        }
    }

    // 返回键：网页能后退就后退，不能才关整个登录页
    BackHandler {
        val view = webView
        if (view != null && view.canGoBack()) view.goBack() else onCancel()
    }

    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
        // imePadding：软键盘弹出时把 WebView 缩短（输入框会跟着页面滚，不躲在键盘后面）
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing).imePadding()) {
            Row(
                Modifier.fillMaxWidth().padding(start = 12.dp, end = 4.dp, top = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
                    Text("${platform.label} 登录", style = MaterialTheme.typography.titleMedium, maxLines = 1)
                    // 窄屏只留一行：原来那段三行说明在手机上太占地方（任务第 5 条）
                    Text(
                        "自己登录；登录好会自动保存，也可以点「完成登录」",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Spacer(Modifier.padding(horizontal = 4.dp))
                if (complete) {
                    // 自动判定只在"登录后 Cookie 变了"时触发；已经登录过（WebView 里本来就有全套 Cookie）
                    // 或者平台把游客 Cookie 也种全了的时候，用户点这里自己保存。
                    TextButton(onClick = { onDone(readLoginCookies(login)) }) { Text("完成登录") }
                }
                TextButton(onClick = onCancel) { Text("关闭") }
            }
            Box(Modifier.weight(1f).fillMaxWidth()) {
                if (gate is Gate.Ready) {
                    val view = webView
                    if (view != null) {
                        // 只负责"显示 effect 里建好的那个 WebView"；factory 里不写任何 state、不新建 View
                        AndroidView(modifier = Modifier.fillMaxSize(), factory = { view })
                    }
                }
                when (val g = gate) {
                    Gate.Preparing -> Overlay {
                        CircularProgressIndicator()
                        Text("正在准备登录页…", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    is Gate.Failed -> Overlay {
                        Text(g.message, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(onClick = { attempt++ }) { Text("重试") }
                            TextButton(onClick = onCancel) { Text("关闭") }
                        }
                    }
                    Gate.Ready -> Unit
                }
            }
        }
    }
}

/** 覆盖层里的阶段：准备代理 / 就绪（WebView 可以加载了）/ 打不开（一句话原因）。 */
private sealed interface Gate {
    data object Preparing : Gate
    data object Ready : Gate
    data class Failed(val message: String) : Gate
}

/** 居中的说明 + 按钮（准备中 / 代理没就绪）。 */
@Composable
private fun Overlay(content: @Composable () -> Unit) {
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
        Column(
            Modifier.fillMaxSize().padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        ) { content() }
    }
}

/**
 * 建一个配好的登录 WebView（**不加载页面**）。放在 effect 里建、代理就绪后再交给 `AndroidView` 显示，
 * 别在 `AndroidView.factory` 里建 —— 见上面第 0 步的注释。
 */
private fun createLoginWebView(context: android.content.Context, onFinished: () -> Unit): WebView = WebView(context).apply {
    setBackgroundColor(Color.WHITE)
    settings.javaScriptEnabled = true
    settings.domStorageEnabled = true
    settings.javaScriptCanOpenWindowsAutomatically = false
    settings.setSupportMultipleWindows(false)
    hideRequestedWith(this)
    webViewClient = object : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, request: android.webkit.WebResourceRequest): Boolean {
            val scheme = request.url.scheme?.lowercase()
            // 只放行 http/https；别的 scheme（bilibili:// 这类）丢掉，不弹别的 App
            return scheme != "http" && scheme != "https"
        }

        @Deprecated("老 API 的签名，一起拦住")
        override fun shouldOverrideUrlLoading(view: WebView, url: String): Boolean {
            val scheme = runCatching { Uri.parse(url).scheme?.lowercase() }.getOrNull()
            return scheme != "http" && scheme != "https"
        }

        override fun onPageFinished(view: WebView, url: String) = onFinished()
    }
    val cm = CookieManager.getInstance()
    cm.setAcceptCookie(true)
    // 登录页里的第三方身份（QQ / 微信 / 微博登录）要种 Cookie，默认值在安卓上是拒绝的
    cm.setAcceptThirdPartyCookies(this, true)
}

/**
 * 不让 WebView 给每个请求加 `X-Requested-With: <包名>`（空白名单 = 哪个站都不发）。
 * 老 WebView 不支持这个开关就算了（照旧发，登录照样能用）。
 */
private fun hideRequestedWith(view: WebView) {
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.REQUESTED_WITH_HEADER_ALLOW_LIST)) return
    runCatching { WebSettingsCompat.setRequestedWithHeaderOriginAllowList(view.settings, emptySet()) }
}

/**
 * 给 WebView 设进程级代理覆盖。`ProxyController` 的回调在代理真正生效之后才来，所以**必须等它**
 * （最多等 [PROXY_TIMEOUT_MS]）；等不到就当失败，调用方清掉覆盖、不加载页面。
 */
private suspend fun applyWebViewProxy(plan: WebLoginProxyPlan.Override): Boolean {
    val config = ProxyConfig.Builder().addProxyRule(plan.rule).build()
    val executor = mainExecutor()
    val applied = withTimeoutOrNull(PROXY_TIMEOUT_MS) {
        suspendCancellableCoroutine { cont ->
            runCatching { ProxyController.getInstance().setProxyOverride(config, executor) { if (cont.isActive) cont.resume(true) } }
                .onFailure { if (cont.isActive) cont.resume(false) }
        }
    } ?: false
    // 进程级覆盖生效 / 撤销都告诉 WebViewProxyGate：验证码 WebView 据此决定"现在能不能弹"
    // （国内平台的人机验证不能从境外代理出去，见 WebViewProxyGate）
    if (applied) WebViewProxyGate.setActive(true)
    return applied
}

/**
 * 清掉 WebView 的代理覆盖（没支持 / 没设过也无所谓）。
 *
 * 标记先置 false（撤销是异步的，但登录页这时已经不再需要它；验证码页要等用户下一次打开评论才会加载，
 * 中间隔着一次网络往返）。失败也要把标记恢复，不能让验证码永远弹不出来。
 */
private fun clearWebViewProxy() {
    WebViewProxyGate.setActive(false)
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) return
    runCatching {
        ProxyController.getInstance().clearProxyOverride(mainExecutor()) { WebViewProxyGate.setActive(false) }
    }
}

/** 主线程的 [Executor]（`ProxyController` 的回调直接改 Compose 状态，必须回主线程）。 */
private fun mainExecutor(): Executor {
    val handler = Handler(Looper.getMainLooper())
    return Executor { command -> handler.post(command) }
}

/** 多久看一眼 CookieManager（毫秒）：够快（用户一点完就关）又不至于太吵。 */
private const val POLL_MS = 700L

/** 等 WebView 代理生效的上限（毫秒）。超时按失败处理（fail-closed）。 */
private const val PROXY_TIMEOUT_MS = 5_000L

/** 页面加载完之后等这么久再采基线 Cookie（毫秒）：给页面自己的 JS 种游客 Cookie 的时间。 */
private const val BASELINE_SETTLE_MS = 3_000L

/** 现在 WebView 里这个平台的关键 Cookie 长什么样（只给判定 / 手动保存用，值不打印）。 */
private fun readLoginCookies(login: LoginSpec): List<Cookie> {
    val headers = WebLoginCookies.probeUrls(login).mapNotNull { probe ->
        runCatching { CookieManager.getInstance().getCookie(probe) }.getOrNull()
    }
    return WebLoginCookies.parse(headers, login.cookieDomain)
}
