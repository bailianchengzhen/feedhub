package feedhub.app.captcha.helper

import feedhub.app.captcha.CaptchaLog
import feedhub.app.captcha.CaptchaNavigationPolicy
import feedhub.app.captcha.HelperCommand
import org.cef.CefApp
import org.cef.CefBrowserSettings
import org.cef.CefClient
import org.cef.browser.CefBrowser
import org.cef.browser.CefFrame
import org.cef.browser.FeedhubOffscreenBrowser
import org.cef.handler.CefLifeSpanHandlerAdapter
import org.cef.handler.CefLoadHandler
import org.cef.handler.CefLoadHandlerAdapter
import org.cef.handler.CefRequestHandlerAdapter
import org.cef.network.CefRequest
import java.awt.Rectangle
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.SwingUtilities
import kotlin.concurrent.thread

/**
 * 验证码子进程的入口：JCEF 只在这个进程里初始化，验证完（成功 / 取消 / 失败 / 超时）整个进程退出，
 * App 进程里不再有 CEF 的消息泵。
 *
 * 怎么起：打进 deb 时是 `/opt/feedhub/bin/feedhub --captcha-helper`（同一个启动器、同一份运行时和类路径，见 `feedhub.app.main`），
 * 开发 / 测试时是 `java -cp <同一份类路径> feedhub.app.captcha.helper.CaptchaHelperMain`（见 `CaptchaHelperLauncher`）。
 *
 * 流程：stdin 读第一条 `Open`（地址、CSS 尺寸、设备像素比）→ 建 CEF（首次会下载原生包，进度经通道回给 App）→
 * 建一个 [FeedhubOffscreenBrowser]（离屏、不开窗口），`onPaint` 的脏区经 stdout 传回 App；
 * App 的鼠标 / 滚轮 / 键盘 / 焦点 / 尺寸从 stdin 来，在 CEF 的 UI 线程（JCEF 在 Linux 上就是 AWT EDT）上转给浏览器；
 * 光标变化回传给 App。
 *
 * 退出：收到 `Close`、或者 stdin EOF（App 退出 / 崩溃 / 被杀）→ 关浏览器（等 `onBeforeClose`）→ `client.dispose` →
 * `CefApp.dispose` → 等 CEF 收完 → `halt(0)`。整个过程有看门狗，[SHUTDOWN_BUDGET_MS] 内没退完就直接 `halt`。
 *
 * 导航白名单（[CaptchaNavigationPolicy]）和"拦掉所有弹窗"在这里照旧生效。日志（`jcef.log`）只记状态和耗时，地址去掉 query。
 */
object CaptchaHelperMain {

    @JvmStatic
    fun main(args: Array<String>) {
        val channel = ChildChannel.open()
        val t0 = System.nanoTime()
        channel.hello(ProcessHandle.current().pid())
        val open = runCatching { channel.readCommand() }.getOrNull() as? HelperCommand.Open
        if (open == null) {
            CaptchaLog.log("helper: no Open command; exiting")
            Runtime.getRuntime().halt(0)
            return
        }
        CaptchaLog.log("helper start ${open.width}x${open.height}@${open.scale} url=${CaptchaLog.redact(open.url)}")
        val session = HelperSession(channel, open, t0)
        thread(name = "feedhub-captcha-stdin", isDaemon = true) {
            try {
                while (true) {
                    val cmd = channel.readCommand() ?: break
                    if (cmd is HelperCommand.Close) { session.shutdown("close"); return@thread }
                    session.handle(cmd)
                }
                session.shutdown("stdin EOF")
            } catch (e: Exception) {
                session.shutdown("stdin ${e.javaClass.simpleName}")
            }
        }
        session.start()
    }
}

/** 收尾的总预算：超过就直接 `halt`。 */
private const val SHUTDOWN_BUDGET_MS = 8_000L

private class HelperSession(
    private val channel: ChildChannel,
    private val open: HelperCommand.Open,
    private val t0: Long,
) {
    private val closing = AtomicBoolean(false)
    private val beforeClose = CountDownLatch(1)

    @Volatile private var app: CefApp? = null
    @Volatile private var client: CefClient? = null
    @Volatile private var browser: FeedhubOffscreenBrowser? = null

    /** 子进程处理过的最近一次输入的 stamp（App 的 nanoTime），随下一帧带回去量延迟。只在 EDT 上写。 */
    @Volatile private var lastInputStamp = 0L
    private var lastW = 0
    private var lastH = 0
    private var frames = 0

    private fun ms() = (System.nanoTime() - t0) / 1_000_000

    /** 在 main 线程上跑：建 CEF 会阻塞到原生包就绪，**不能在 EDT 上**。 */
    fun start() {
        val built = try {
            channel.status("正在启动浏览器内核…")
            HelperCef.build { channel.status(it) }
        } catch (t: Throwable) {
            channel.failed(t.message ?: t.javaClass.simpleName)
            shutdown("build failed")
            return
        }
        app = built
        if (closing.get()) { shutdown("closed during init"); return }
        try {
            val c = built.createClient()
            client = c
            check(!HelperCef.initFailed) { "浏览器内核初始化失败" }
            CaptchaLog.log("helper: cef ready ${ms()}ms")
            install(c, CaptchaNavigationPolicy(CaptchaNavigationPolicy.portOf(open.url)))
            val settings = CefBrowserSettings().apply { windowless_frame_rate = 60 }
            val b = FeedhubOffscreenBrowser(c, open.url, settings, object : FeedhubOffscreenBrowser.Sink {
                override fun onPaint(popup: Boolean, dirty: Array<Rectangle>, buffer: ByteBuffer, width: Int, height: Int) =
                    paint(popup, dirty, buffer, width, height)

                override fun onCursor(awtCursorType: Int) = channel.cursor(awtCursorType)
            })
            b.setView(open.width, open.height, open.scale.toDouble(), false)
            browser = b
            b.createImmediately()
            CaptchaLog.log("helper: browser created ${ms()}ms")
        } catch (t: Throwable) {
            CaptchaLog.log("helper: create failed: ${t.message}")
            channel.failed(t.message ?: t.javaClass.simpleName)
            shutdown("create failed")
        }
    }

    private fun install(c: CefClient, policy: CaptchaNavigationPolicy) {
        c.addRequestHandler(object : CefRequestHandlerAdapter() {
            override fun onBeforeBrowse(b: CefBrowser?, frame: CefFrame?, request: CefRequest?, userGesture: Boolean, isRedirect: Boolean): Boolean {
                val target = request?.url ?: return true
                // frame 拿不到时按"主框架"从严处理
                val mainFrame = frame?.isMain ?: true
                val allowed = if (mainFrame) policy.isMainFrameAllowed(target) else policy.isSubFrameAllowed(target)
                if (!allowed) CaptchaLog.log("blocked navigation (main=$mainFrame redirect=$isRedirect): ${CaptchaLog.redact(target)}")
                return !allowed
            }
        })
        c.addLifeSpanHandler(object : CefLifeSpanHandlerAdapter() {
            override fun onBeforeClose(b: CefBrowser) {
                beforeClose.countDown()
            }

            // 验证码不需要新窗口；也不处理下载
            override fun onBeforePopup(b: CefBrowser?, frame: CefFrame?, targetUrl: String?, targetFrameName: String?): Boolean {
                CaptchaLog.log("blocked popup: ${CaptchaLog.redact(targetUrl)}")
                return true
            }
        })
        c.addLoadHandler(object : CefLoadHandlerAdapter() {
            override fun onLoadEnd(b: CefBrowser?, frame: CefFrame?, httpStatusCode: Int) {
                if (frame?.isMain != true) return
                CaptchaLog.log("helper: main frame loaded http=$httpStatusCode ${ms()}ms")
                channel.loaded(httpStatusCode)
            }

            override fun onLoadError(b: CefBrowser?, frame: CefFrame?, errorCode: CefLoadHandler.ErrorCode?, errorText: String?, failedUrl: String?) {
                if (frame?.isMain != true) return
                CaptchaLog.log("helper: main frame load error $errorCode ${CaptchaLog.redact(failedUrl)}")
            }
        })
    }

    /** CEF 的 UI 线程（EDT）上回调。只转主视图；`<select>` 之类的弹出层（popup）不画（验证码用不到）。 */
    private fun paint(popup: Boolean, dirty: Array<Rectangle>, buffer: ByteBuffer, width: Int, height: Int) {
        if (popup || closing.get() || width <= 0 || height <= 0) return
        val full = width != lastW || height != lastH
        lastW = width
        lastH = height
        val rects = if (full) {
            listOf(intArrayOf(0, 0, width, height))
        } else {
            val clipped = dirty.mapNotNull { r ->
                val x0 = r.x.coerceIn(0, width)
                val y0 = r.y.coerceIn(0, height)
                val x1 = (r.x + r.width).coerceIn(0, width)
                val y1 = (r.y + r.height).coerceIn(0, height)
                if (x1 > x0 && y1 > y0) intArrayOf(x0, y0, x1 - x0, y1 - y0) else null
            }
            // 脏区加起来超过大半幅就整幅发（省掉逐块的头和多次拷贝）
            val area = clipped.sumOf { it[2].toLong() * it[3] }
            if (area * 10 > width.toLong() * height * 6) listOf(intArrayOf(0, 0, width, height)) else clipped
        }
        if (rects.isEmpty()) return
        channel.frame(buffer, width, height, rects, System.nanoTime(), lastInputStamp)
        if (frames++ == 0) CaptchaLog.log("helper: first paint ${width}x$height ${ms()}ms")
    }

    /** stdin 线程上调；CEF 调用一律派到 EDT。 */
    fun handle(cmd: HelperCommand) {
        if (closing.get()) return
        SwingUtilities.invokeLater {
            val b = browser ?: return@invokeLater
            if (closing.get()) return@invokeLater
            runCatching {
                when (cmd) {
                    is HelperCommand.Mouse -> { lastInputStamp = cmd.stamp; b.mouse(cmd.id, cmd.x, cmd.y, cmd.modifiers, cmd.button, cmd.clickCount) }
                    is HelperCommand.Wheel -> { lastInputStamp = cmd.stamp; b.wheel(cmd.x, cmd.y, cmd.modifiers, cmd.rotation, cmd.precise) }
                    is HelperCommand.Key -> { lastInputStamp = cmd.stamp; b.key(cmd.id, cmd.keyCode, cmd.keyChar, cmd.modifiers, cmd.location) }
                    is HelperCommand.Focus -> b.setFocus(cmd.focused)
                    is HelperCommand.Resize -> b.setView(cmd.width, cmd.height, cmd.scale.toDouble(), true)
                    is HelperCommand.Open, HelperCommand.Close -> {}
                }
            }.onFailure { CaptchaLog.log("helper: input failed: ${it.javaClass.simpleName}") }
        }
    }

    /** 收尾并退出进程。可以从任何线程调，只生效一次。 */
    fun shutdown(reason: String) {
        if (!closing.compareAndSet(false, true)) return
        CaptchaLog.log("helper: shutdown ($reason) after ${ms()}ms, $frames frames")
        thread(name = "feedhub-captcha-watchdog", isDaemon = true) {
            Thread.sleep(SHUTDOWN_BUDGET_MS)
            CaptchaLog.log("helper: shutdown budget exceeded; halt")
            Runtime.getRuntime().halt(3)
        }
        thread(name = "feedhub-captcha-shutdown", isDaemon = true) {
            val b = browser
            if (b != null) {
                // CEF 调用都在 EDT 上（老版本在后台线程 close / dispose，几分钟后 libcef SIGILL）
                SwingUtilities.invokeLater {
                    runCatching { b.setWindowVisibility(false) }
                    runCatching { b.close(true) }
                }
                val ok = beforeClose.await(3_000, TimeUnit.MILLISECONDS)
                CaptchaLog.log("helper: onBeforeClose waited=$ok")
            }
            val a = app
            if (a != null) {
                runCatching {
                    SwingUtilities.invokeAndWait {
                        runCatching { client?.dispose() }
                        runCatching { a.dispose() }
                    }
                }
                val ok = HelperCef.awaitTerminated(3_000)
                CaptchaLog.log("helper: cef terminated=$ok")
            }
            HelperCef.cleanup()
            channel.bye()
            Runtime.getRuntime().halt(0)
        }
    }
}
