package feedhub.app.captcha

import com.sun.net.httpserver.HttpExchange
import com.sun.net.httpserver.HttpServer
import feedhub.core.CaptchaChallenge
import feedhub.core.Platform
import feedhub.core.plugin.CaptchaPage
import feedhub.plugins.InstalledPlugins
import kotlinx.coroutines.Dispatchers
import java.awt.Desktop
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.URI
import java.util.concurrent.Executors
import kotlin.concurrent.thread
import kotlin.coroutines.CoroutineContext

/**
 * 桌面端的验证码处理：起一个只监听 127.0.0.1 的临时 HTTP 服务，把这个平台的验证页（插件描述的 `captchaPage`，
 * 现在只有酷安的 `CoolapkCaptchaPage`）挂上去。
 *
 * 现在的真实流程：
 * 1. **内嵌 JCEF（CEF 135）覆盖层优先**：`App` 在详情区里用 `CaptchaBrowser` 加载这个本机页面
 * ，用户在里面完成网易易盾验证；页面把 proof 直接 `fetch` 回这里的本机服务，不经过子进程的通道；
 * 2. JCEF 起不来 / 首次下载 CEF 失败时**退回系统浏览器**打开同一个地址（[openInSystemBrowser]，
 *    用户在弹框里点"用浏览器打开"）；
 * 3. 两种方式都是同一个本机页面，页面把 proof 回传给本地服务，这里再交回给 `CoolapkSource`
 *    写 `validate` cookie 并重试评论请求。
 *
 * 排队 / 取消 / 超时 / proof 校验在 [CaptchaHandlerBase] 里（和安卓共用）；这个类只管"本机 HTTP 服务"这一层。
 *
 * **不碰验证码本身**：不解、不重放、不换设备绕。用户完成一次，cookie 留到服务端不认它为止
 * （我们存 30 天，服务端提前失效就再弹一次，见 `CoolapkSource.VALIDATE_TTL_MS`）。
 */
class DesktopCaptchaHandler(
    prompt: CaptchaPromptState,
    /** Compose 状态要在主线程改；测试里换成 `Dispatchers.Unconfined`。 */
    uiContext: CoroutineContext = Dispatchers.Main,
    /** 各平台的验证页（插件描述的 `captchaPage`，现在只有酷安）；测试里可以换。 */
    pages: (Platform) -> CaptchaPage? = { InstalledPlugins.catalog[it]?.captchaPage },
    /** 给用户多少时间完成验证（毫秒）；测试里给短值。 */
    timeoutMs: Long = CaptchaHandlerBase.DEFAULT_TIMEOUT_MS,
    /** 打开浏览器；测试里换成 no-op，避免真弹窗口。 */
    private val browser: (String) -> Unit = ::openSystemBrowser,
) : CaptchaHandlerBase(prompt, uiContext, pages, timeoutMs) {

    override suspend fun mountPage(
        challenge: CaptchaChallenge,
        page: CaptchaPage,
        submitUrl: String,
    ): MountedPage {
        val html = page.html(challenge.captchaId, submitUrl)
        val server = HttpServer.create(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0), 0)
        server.executor = Executors.newSingleThreadExecutor { r ->
            Thread(r, "feedhub-captcha").apply { isDaemon = true }
        }
        val origin = "http://127.0.0.1:${server.address.port}"
        server.createContext("/") { exchange ->
            try {
                handle(exchange, origin, html)
            } catch (e: Exception) {
                runCatching { respond(exchange, 500, "验证服务出错：${e.message}") }
            } finally {
                exchange.close()
            }
        }
        server.start()
        return MountedPage(url = "$origin/", html = html, close = { server.stop(0) })
    }

    /** 内嵌浏览器起不来 / 下载失败时的退路：用系统浏览器打开同一个本机验证页。 */
    override fun openInSystemBrowser() {
        currentUrl?.let { url ->
            thread(isDaemon = true, name = "feedhub-captcha-open") { browser(url) }
        }
    }

    private fun handle(exchange: HttpExchange, origin: String, html: String) {
        // DNS rebinding：别的网页把自己的域名解析到 127.0.0.1 再来读这个端口时，Host 是它的域名。
        // 只认我们自己发出去的那个地址，页面里的回传 token 就读不走
        if (exchange.requestHeaders.getFirst("Host") != origin.removePrefix("http://")) {
            respond(exchange, 403, "forbidden")
            return
        }
        val path = exchange.requestURI.path
        if (exchange.requestMethod != "GET") {
            respond(exchange, 405, "只接受 GET")
            return
        }
        when (path) {
            "/", "/index.html" -> {
                val bytes = html.toByteArray(Charsets.UTF_8)
                exchange.responseHeaders.set("Content-Type", "text/html; charset=utf-8")
                exchange.responseHeaders.set("Cache-Control", "no-store")
                exchange.sendResponseHeaders(200, bytes.size.toLong())
                exchange.responseBody.use { it.write(bytes) }
            }
            CaptchaHandlerBase.PROOF_PATH -> {
                // 先把响应写完再唤醒 solve：完成会去 stop(0) 服务，反过来客户端会看到被截断的响应
                when (val result = checkSubmit(origin + exchange.requestURI.toString())) {
                    is SubmitResult.Accepted -> {
                        respond(exchange, 200, "OK")
                        deliverProof(result.proof)
                    }
                    SubmitResult.WrongToken -> respond(exchange, 403, "token 不对")
                    SubmitResult.BadProof -> respond(exchange, 400, "验证结果格式不对")
                    SubmitResult.NoPage -> respond(exchange, 404, "not found")
                }
            }
            "/favicon.ico" -> exchange.sendResponseHeaders(204, -1)
            else -> respond(exchange, 404, "not found")
        }
    }

    private fun respond(exchange: HttpExchange, code: Int, text: String) {
        val bytes = text.toByteArray(Charsets.UTF_8)
        exchange.responseHeaders.set("Content-Type", "text/plain; charset=utf-8")
        exchange.responseHeaders.set("Cache-Control", "no-store")
        exchange.sendResponseHeaders(code, bytes.size.toLong())
        exchange.responseBody.use { it.write(bytes) }
    }
}

/** 用系统默认浏览器打开验证页；AWT 不行就退回 xdg-open。 */
private fun openSystemBrowser(url: String) {
    runCatching {
        if (Desktop.isDesktopSupported() && Desktop.getDesktop().isSupported(Desktop.Action.BROWSE)) {
            Desktop.getDesktop().browse(URI(url))
            return
        }
    }
    runCatching { ProcessBuilder("xdg-open", url).start() }.onFailure {
        // 浏览器打不开时用户还能从弹框里复制地址；不要因此让评论请求一直挂着
    }
}
