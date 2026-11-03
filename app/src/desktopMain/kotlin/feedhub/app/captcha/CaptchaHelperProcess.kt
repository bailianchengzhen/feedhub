package feedhub.app.captcha

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.captcha.RustCaptcha
import org.jetbrains.skia.ColorAlphaType
import org.jetbrains.skia.ColorType
import org.jetbrains.skia.Image
import org.jetbrains.skia.ImageInfo
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread

/**
 * 怎么起验证码子进程（命令行 + 环境变量）。纯数据，离线可测（`CaptchaHelperLauncherTest`）。
 *
 * - **一般情况**（deb 和开发 / 测试时都是）：`<java.home>/bin/java --add-exports… -cp <java.class.path> <主类>`。
 * 不经过 jpackage 的启动器；
 * - **退路**：运行时里没有 `bin/java`（老的打包）时用 jpackage 的启动器 `/opt/feedhub/bin/feedhub --captcha-helper`
 *   （`jpackage.app-path` 系统属性给出启动器路径；`feedhub.app.main` 第一件事就认这个参数），同一份运行时、类路径和 `--add-exports`。
 *
 * 环境变量：
 * - **不继承 App 的 `JAVA_TOOL_OPTIONS`**（启动器脚本给主进程开的 AppCDS 归档是主进程的），换成子进程自己的一份：
 *   headless、Serial GC、堆上限 256 MB；打包的 App 里（有 `jpackage.app-path`，jpackage 启动器和 `feedhub-launch` 都会设）再加子进程自己的 AppCDS 归档（`captcha-helper-cds.jsa`，和主进程的分开）；
 * - 去掉 `WAYLAND_DISPLAY`、设 `GDK_BACKEND=x11`（CEF 135 看到 Wayland 会初始化失败，这台机器验证过的组合，原来 `JcefEnv` 做的事）。
 */
internal class CaptchaHelperLauncher(
    val command: List<String>,
    /** 子进程环境的改动：值为 null 表示删掉这个变量。 */
    val env: Map<String, String?>,
) {
    fun processBuilder(): ProcessBuilder = ProcessBuilder(command).also { pb ->
        val e = pb.environment()
        for ((k, v) in env) if (v == null) e.remove(k) else e[k] = v
    }

    companion object {
        /** deb 启动器的参数（`feedhub.app.main` 判断它；子进程入口 `CaptchaHelperMain` 不在 App 的正常路径上加载）。 */
        const val FLAG = "--captcha-helper"
        const val HELPER_MAIN = "feedhub.app.captcha.helper.CaptchaHelperMain"

        /** 子进程要和 App 一样的 `--add-exports`（JCEF 的 OSR / 键盘事件要用），开发路径上手动带。 */
        private val ADD_EXPORTS = listOf(
            "--add-exports", "java.base/java.lang=ALL-UNNAMED",
            "--add-exports", "java.desktop/sun.awt=ALL-UNNAMED",
            "--add-exports", "java.desktop/sun.java2d=ALL-UNNAMED",
        )

        fun forCurrentProcess(
            mainClass: String = HELPER_MAIN,
            props: (String) -> String? = System::getProperty,
            parentEnv: Map<String, String> = System.getenv(),
            cacheDir: File = CaptchaLog.installDir().parentFile,
        ): CaptchaHelperLauncher {
            val appPath = props("jpackage.app-path")
            val java = props("java.home")?.let { File(it, "bin/java") }
            val archive = File(cacheDir, "captcha-helper-cds.jsa").path
            val command = try {
                RustCaptcha.buildLauncherCommand(
                    javaBin = java?.path,
                    canExecuteJava = java != null && java.canExecute(),
                    classPath = props("java.class.path").orEmpty(),
                    mainClass = mainClass,
                    appPath = appPath,
                    canExecuteApp = appPath != null && File(appPath).canExecute(),
                )
            } catch (e: Exception) {
                error("起不了验证码子进程：${java ?: "没有 java.home"} 不能执行，也没有 jpackage 启动器")
            }
            val env = RustCaptcha.buildLauncherEnv(appPath, archive)
            // 保险：App 自己的 JAVA_TOOL_OPTIONS 一定不进子进程（上面那一行已经整份换掉）
            check(parentEnv["JAVA_TOOL_OPTIONS"].let { it == null || env["JAVA_TOOL_OPTIONS"] != it })
            return CaptchaHelperLauncher(command, env)
        }
    }
}

/**
 * 一次验证码的子进程会话（App 这一头）：起子进程、把画面拼起来给 Compose 画、把输入发过去、收光标和状态，关的时候按 PID 收进程。
 *
 * - **同一时间只有一个**：新会话起的时候把上一个关掉（[active]）；
 * - 关闭（验证成功 / 取消 / 超时 / 失败，Compose 那边离开组合时调 [close]）：先发 `Close` 让子进程体面地关 CEF，
 *   [closeGraceMs] 内没退就 `destroy`、再 `destroyForcibly`；CEF 的渲染 / zygote 子进程按关之前的快照逐个按 PID 收
 *   （`ProcessHandle` 带启动时间，PID 被复用了也不会误杀）。**不用 `pkill`**；
 * - App 退出时（关窗口走 `exitProcess`）shutdown hook 把还活着的会话关掉；App 崩溃 / 被杀时子进程读 stdin 得到 EOF，自己退。
 *
 * 线程：读线程收消息、拼帧、建 Skia 图；发送线程把输入写进子进程的 stdin（不在界面线程上写管道，子进程卡住也不会卡界面）。
 */
internal class CaptchaHelperSession(
    private val url: String,
    private val launcher: () -> CaptchaHelperLauncher = { CaptchaHelperLauncher.forCurrentProcess() },
    private val closeGraceMs: Long = 6_000,
    /** 子进程的 stderr 去哪（默认丢掉：CEF 会把页面的 console 打到 stderr，不进日志）。开发时设 `FEEDHUB_CAPTCHA_STDERR=<文件>`。 */
    private val stderr: ProcessBuilder.Redirect = System.getenv("FEEDHUB_CAPTCHA_STDERR")
        ?.let { ProcessBuilder.Redirect.appendTo(File(it)) } ?: ProcessBuilder.Redirect.DISCARD,
) {
    /** 新帧来了就加一（Compose 的 Canvas 读它，有新帧才重画）。 */
    var tick by mutableLongStateOf(0L)
        private set

    /** 子进程报的光标（AWT 的 `Cursor.*_CURSOR`）。 */
    var cursor by mutableIntStateOf(java.awt.Cursor.DEFAULT_CURSOR)
        private set

    /** 第一帧之前给用户看的状态。 */
    var status by mutableStateOf("正在准备验证码…")
        private set

    /** 起不来 / 子进程中途没了：非 null 时覆盖层显示"用系统浏览器打开"的退路。 */
    var error by mutableStateOf<String?>(null)
        private set

    var hasFrame by mutableStateOf(false)
        private set

    private val imageLock = Any()
    private var image: Image? = null

    private val started = AtomicBoolean(false)
    private val closed = AtomicBoolean(false)
    private val outbox = LinkedBlockingQueue<HelperCommand>()
    @Volatile private var process: Process? = null

    /** 子进程 PID（起来之前是 -1）。 */
    @Volatile var pid: Long = -1
        private set

    // —— 计时（`System.nanoTime`），给结果 / 探针用 ——
    @Volatile var startNanos = 0L
        private set
    @Volatile var helloNanos = 0L
        private set
    @Volatile var firstFrameNanos = 0L
        private set
    @Volatile var loadedNanos = 0L
        private set
    @Volatile var frames = 0
        private set

    /** 每次输入之后第一帧到达 App 的延迟（纳秒）：App 发出输入 → 子进程出帧 → App 收完这一帧。 */
    val inputLatencies = java.util.concurrent.ConcurrentLinkedQueue<Long>()

    /** 子进程 onPaint → App 收完这一帧（纳秒），量通道本身的开销。 */
    val transferLatencies = java.util.concurrent.ConcurrentLinkedQueue<Long>()

    @Volatile private var lastW = 0
    @Volatile private var lastH = 0
    @Volatile private var lastScale = 0f

    /**
     * 画布尺寸（设备像素）和 Compose 的密度。第一次调用时起子进程；之后尺寸变了就通知子进程。
     * CEF 那边用 CSS 像素（= px / density），设备像素比 = density：出的帧和画布 1:1，鼠标坐标也是 px / density。
     */
    fun resize(widthPx: Int, heightPx: Int, density: Float) {
        if (widthPx <= 0 || heightPx <= 0 || closed.get()) return
        val (w, h) = RustCaptcha.calculateLogicalSize(widthPx, heightPx, density)
        if (w == lastW && h == lastH && density == lastScale) return
        lastW = w; lastH = h; lastScale = density
        if (started.compareAndSet(false, true)) start(w, h, density) else send(HelperCommand.Resize(w, h, density))
    }

    fun mouse(id: Int, x: Int, y: Int, modifiers: Int, button: Int, clickCount: Int) =
        send(HelperCommand.Mouse(id, x, y, modifiers, button, clickCount, System.nanoTime()))

    fun wheel(x: Int, y: Int, modifiers: Int, rotation: Int, precise: Double) =
        send(HelperCommand.Wheel(x, y, modifiers, rotation, precise, System.nanoTime()))

    fun key(id: Int, keyCode: Int, keyChar: Char, modifiers: Int, location: Int) =
        send(HelperCommand.Key(id, keyCode, keyChar, modifiers, location, System.nanoTime()))

    private val focusTimer = java.util.concurrent.Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "feedhub-captcha-focus").apply { isDaemon = true }
    }
    private var pendingBlur: java.util.concurrent.ScheduledFuture<*>? = null
    private var focusSent = false

    /**
     * 焦点：点一下验证码时，外面的覆盖层会先抢一下焦点、马上又回到这里（Compose 的 clickable），
     * 失焦延迟 [BLUR_DELAY_MS] 再发，这期间又拿回焦点就当没失过——不然页面每点一下都会 blur / focus 一次。
     */
    @Synchronized
    fun focus(focused: Boolean) {
        pendingBlur?.cancel(false)
        pendingBlur = null
        if (focused) {
            if (!focusSent) { focusSent = true; send(HelperCommand.Focus(true)) }
        } else if (focusSent) {
            pendingBlur = focusTimer.schedule({
                synchronized(this) { if (focusSent) { focusSent = false; send(HelperCommand.Focus(false)) } }
            }, BLUR_DELAY_MS, java.util.concurrent.TimeUnit.MILLISECONDS)
        }
    }

    /** 在锁里用当前这一帧（画的时候调）；还没有帧就不调 [block]。 */
    fun <T> withImage(block: (Image) -> T): T? = synchronized(imageLock) { image?.let(block) }

    private fun send(c: HelperCommand) {
        if (!closed.get() && started.get()) outbox.offer(c)
    }

    private fun start(w: Int, h: Int, scale: Float) {
        active.getAndSet(this)?.close()
        installShutdownHook()
        startNanos = System.nanoTime()
        val p = try {
            launcher().processBuilder()
                .redirectError(stderr)
                .start()
        } catch (e: Exception) {
            fail("起不来验证码进程：${e.message ?: e.javaClass.simpleName}")
            return
        }
        process = p
        pid = p.pid()
        CaptchaLog.log("app: helper pid=$pid started ${w}x$h@$scale")
        outbox.offer(HelperCommand.Open(url, w, h, scale))
        thread(name = "feedhub-captcha-send", isDaemon = true) { sendLoop(p) }
        thread(name = "feedhub-captcha-recv", isDaemon = true) { receiveLoop(p) }
        // 关之前进程自己没了（崩溃 / 被杀 / 初始化失败）：给退路
        p.onExit().thenAccept { proc ->
            if (!closed.get()) {
                CaptchaLog.log("app: helper exited early code=${proc.exitValue()}")
                if (error == null) fail("验证码进程退出了（${proc.exitValue()}）")
            }
        }
    }

    private fun sendLoop(p: Process) {
        val out = DataOutputStream(BufferedOutputStream(p.outputStream, 1 shl 14))
        try {
            while (true) {
                val c = outbox.take()
                HelperWire.writeCommand(out, c)
                // 后面没有排着的就立刻发（鼠标拖动是一条一条来的，攒着反而加延迟）
                if (outbox.isEmpty()) out.flush()
                if (c is HelperCommand.Close) break
            }
        } catch (_: Exception) {
            // 子进程没了：读线程 / onExit 会处理
        } finally {
            runCatching { out.close() }
        }
    }

    private fun receiveLoop(p: Process) {
        val input = DataInputStream(BufferedInputStream(p.inputStream, 1 shl 20))
        val frames = FrameAssembler()
        var lastInput = 0L
        try {
            while (true) {
                val ev = HelperWire.readEvent(input, frames) ?: break
                val now = System.nanoTime()
                when (ev) {
                    is HelperEvent.Hello -> helloNanos = now
                    is HelperEvent.Status -> if (!hasFrame) status = ev.text
                    is HelperEvent.Frame -> {
                        val img = Image.makeRaster(
                            ImageInfo(frames.width, frames.height, ColorType.BGRA_8888, ColorAlphaType.OPAQUE),
                            frames.pixels,
                            frames.width * 4,
                        )
                        synchronized(imageLock) {
                            image?.close()
                            image = img
                        }
                        if (firstFrameNanos == 0L) firstFrameNanos = now
                        this.frames++
                        transferLatencies += now - ev.paintStamp
                        if (ev.inputStamp != 0L && ev.inputStamp != lastInput) {
                            lastInput = ev.inputStamp
                            inputLatencies += now - ev.inputStamp
                        }
                        if (!hasFrame) hasFrame = true
                        tick++
                    }
                    is HelperEvent.Cursor -> cursor = ev.type
                    is HelperEvent.Loaded -> {
                        if (loadedNanos == 0L) {
                            loadedNanos = now
                            CaptchaLog.log("app: helper page loaded ${(now - startNanos) / 1_000_000}ms after spawn")
                        }
                    }
                    is HelperEvent.Failed -> fail(ev.message)
                    HelperEvent.Bye -> break
                }
            }
        } catch (e: Exception) {
            if (!closed.get()) CaptchaLog.log("app: helper channel broke: ${e.javaClass.simpleName}")
        }
    }

    private fun fail(message: String) {
        if (closed.get()) return
        CaptchaLog.log("app: helper failed: $message")
        error = message
    }

    /** 关掉：体面地关 → 超时就按 PID 杀（含 CEF 的子进程）。可以重复调、可以在任何线程调。 */
    fun close() {
        if (!closed.compareAndSet(false, true)) return
        active.compareAndSet(this, null)
        val p = process
        if (p == null) {
            releaseImage()
            return
        }
        val children = runCatching { p.descendants().toList() }.getOrDefault(emptyList())
        outbox.offer(HelperCommand.Close)
        thread(name = "feedhub-captcha-reap", isDaemon = true) {
            reap(p, children, closeGraceMs)
            releaseImage()
        }
    }

    /** App 退出时用：最多等 [graceMs]，然后直接杀。 */
    internal fun closeNow(graceMs: Long) {
        closed.set(true)
        val p = process ?: return
        val children = runCatching { p.descendants().toList() }.getOrDefault(emptyList())
        outbox.offer(HelperCommand.Close)
        reap(p, children, graceMs)
    }

    private fun reap(p: Process, children: List<ProcessHandle>, graceMs: Long) {
        val graceful = p.waitFor(graceMs, TimeUnit.MILLISECONDS)
        if (!graceful) {
            CaptchaLog.log("app: helper pid=${p.pid()} did not exit in ${graceMs}ms; destroying")
            p.destroy()
            if (!p.waitFor(1, TimeUnit.SECONDS)) p.destroyForcibly()
        }
        // CEF 的子进程在主进程退出后自己会走；给一点时间，没走的按快照逐个收
        val deadline = System.nanoTime() + 1_500_000_000L
        while (children.any { it.isAlive } && System.nanoTime() < deadline) Thread.sleep(50)
        val left = children.filter { it.isAlive }
        left.forEach { it.destroyForcibly() }
        CaptchaLog.log(
            "app: helper pid=${p.pid()} closed graceful=$graceful exit=${runCatching { p.exitValue() }.getOrNull()} " +
                "children=${children.size} killedChildren=${left.size}",
        )
    }

    private fun releaseImage() {
        focusTimer.shutdownNow()
        synchronized(imageLock) {
            image?.close()
            image = null
        }
    }

    internal companion object {
        const val BLUR_DELAY_MS = 150L

        /** 同一时间只有一个会话（子进程）。 */
        val active = AtomicReference<CaptchaHelperSession?>(null)
        private val hook = AtomicBoolean(false)

        private fun installShutdownHook() {
            if (!hook.compareAndSet(false, true)) return
            Runtime.getRuntime().addShutdownHook(Thread({ active.getAndSet(null)?.closeNow(1_000) }, "feedhub-captcha-exit"))
        }
    }
}
