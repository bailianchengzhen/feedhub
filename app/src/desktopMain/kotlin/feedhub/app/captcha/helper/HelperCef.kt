package feedhub.app.captcha.helper

import feedhub.app.captcha.CaptchaLog
import me.friwi.jcefmaven.CefAppBuilder
import me.friwi.jcefmaven.EnumProgress
import me.friwi.jcefmaven.MavenCefAppHandlerAdapter
import org.cef.CefApp
import java.io.File
import java.nio.file.Files
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * 子进程里的 CEF 初始化。
 *
 * CEF 152 的 OSR 初始化会
 * 卡在 INITIALIZING 或 `ContentMainRun failed with exit code 28`；同一台机器换成 135 后
 * OSR 正常渲染易盾滑块（`app:jcefProbe` 的截图作证）。不要随手升 152+，先在目标机器上跑 probe。
 *
 * 原生包（~334MB 解压后）第一次用时由 jcefmaven 下载到 `~/.cache/feedhub/jcef`，**不打进 deb**。
 * 窗口系统：App 起子进程时已经去掉 `WAYLAND_DISPLAY`、设好 `GDK_BACKEND=x11`（CEF 135 看到 Wayland 会初始化失败），
 * 这里再加 `--ozone-platform=x11`。
 */
internal object HelperCef {
    private val terminated = CountDownLatch(1)

    /** 初始化失败（`INITIALIZATION_FAILED`）：`createClient` 照样会返回，所以要单独看。 */
    @Volatile var initFailed = false
        private set

    /** 这次用的临时资料目录（共用的那份被别的活进程占着时才有），退出时删掉。 */
    @Volatile private var tempProfile: File? = null

    /**
     * 选 CEF 的 user-data-dir：平时用共用的 `jcef/cache`。
     * - 里面留着上一次进程被杀时的 `SingletonLock`（指向已经死掉的 PID）：新进程会以为 profile 还被占着、初始化直接
     *   `INITIALIZATION_FAILED`，所以先清掉。子进程被按 PID 杀掉之后下一次就靠它；
     * - lock 指向**活着的**进程（比如还开着的旧版 App——它在自己进程里用过验证码、一直占着；或者另一个 App 实例的验证码）：
     *   不动它，这一次用 `jcef/cache-tmp-*` 临时目录，退出时删掉。
     */
    private fun profileDir(base: File): File {
        val shared = File(base, "cache")
        shared.mkdirs()
        // 上次没删掉的临时目录（进程被杀）：一天以前的顺手清掉
        base.listFiles { f -> f.isDirectory && f.name.startsWith("cache-tmp-") && System.currentTimeMillis() - f.lastModified() > 86_400_000L }
            ?.forEach { it.deleteRecursively() }
        val lock = File(shared, "SingletonLock")
        if (!Files.isSymbolicLink(lock.toPath()) && !lock.exists()) return shared
        val target = runCatching { Files.readSymbolicLink(lock.toPath()).toString() }.getOrNull()
        val pid = target?.substringAfterLast('-')?.toLongOrNull()
        val alive = pid != null && ProcessHandle.of(pid).map { it.isAlive }.orElse(false)
        if (alive) {
            val tmp = Files.createTempDirectory(base.toPath(), "cache-tmp-").toFile()
            tempProfile = tmp
            CaptchaLog.log("CEF profile lock held by live pid $pid; using a temporary profile this time")
            return tmp
        }
        CaptchaLog.log("cleaning stale CEF profile locks (target=$target)")
        listOf("SingletonLock", "SingletonCookie", "SingletonSocket").forEach { File(shared, it).delete() }
        return shared
    }

    /** CEF 收完之后调：删掉这次的临时资料目录（如果有）。 */
    fun cleanup() {
        tempProfile?.let { runCatching { it.deleteRecursively() } }
    }

    /** 建 CefApp（会阻塞到原生包就绪；首次会下载）。**不能在 EDT 上调**。[progress] 给 App 显示下载进度。 */
    fun build(progress: (String) -> Unit): CefApp {
        val dir = CaptchaLog.installDir()
        val profile = profileDir(dir)
        val builder = CefAppBuilder()
        builder.setInstallDir(dir)
        builder.setProgressHandler { state, percent ->
            when (state) {
                EnumProgress.DOWNLOADING ->
                    progress(if (percent >= 0) "正在下载浏览器内核… ${percent.toInt()}%（首次使用，约 100MB）" else "正在下载浏览器内核…（首次使用，约 100MB）")
                EnumProgress.EXTRACTING -> progress("正在解压浏览器内核…")
                EnumProgress.INITIALIZING -> progress("正在启动浏览器内核…")
                else -> {}
            }
        }
        builder.getCefSettings().windowless_rendering_enabled = true
        builder.getCefSettings().root_cache_path = profile.absolutePath
        builder.addJcefArgs(
            "--no-sandbox", // 原生包装在用户 cache 里，没有 SUID sandbox；页面只有本机验证码 + 易盾 SDK
            "--disable-gpu", "--in-process-gpu", "--disable-gpu-sandbox", "--disable-gpu-compositing",
            "--ozone-platform=x11",
            "--disable-dev-shm-usage",
            "--enable-unsafe-swiftshader",
        )
        // 开发 / 探针用的出网闸：除了本机回环，一律走一个连不上的代理（Chromium 对 127.0.0.1 默认不走代理）。
        // 正常使用不设（易盾的脚本要从网上来）
        if (System.getenv("FEEDHUB_CAPTCHA_OFFLINE") == "1") builder.addJcefArgs("--proxy-server=socks5://127.0.0.1:9")
        builder.setAppHandler(object : MavenCefAppHandlerAdapter() {
            override fun stateHasChanged(state: CefApp.CefAppState?) {
                CaptchaLog.log("cef state=$state")
                if (state == CefApp.CefAppState.INITIALIZATION_FAILED) initFailed = true
                if (state == CefApp.CefAppState.TERMINATED || state == CefApp.CefAppState.INITIALIZATION_FAILED) terminated.countDown()
            }
        })
        return try {
            builder.build()
        } catch (t: Throwable) {
            CaptchaLog.log("CEF build failed: ${t.message}")
            throw t
        }
    }

    /** `CefApp.dispose()` 之后等 CEF 真的收完（最多 [timeoutMs]）。 */
    fun awaitTerminated(timeoutMs: Long): Boolean = terminated.await(timeoutMs, TimeUnit.MILLISECONDS)
}
