package feedhub.app.api

import feedhub.app.AppGraph
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.nio.file.Path
import java.nio.file.Paths
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * 桌面 App 的对外接口。
 *
 * **跟着 App 活**：`AppGraph` 盯着设置里的开关（[feedhub.app.AppSettings.appApi]），打开时调 [start]、
 * 关掉时调 [stop]、关窗口时调 [close]（`AppGraph.shutdown` 那条路）。**没有独立程序、没有自己的进程**，
 * 更没有"App 没开时"这种东西：App 没开，socket 文件就不存在。
 *
 * 请求走传进来的那份 [AppGraph]（同一个 `source()` 缓存、同一份 Cookie / 设置 / 资料库 / 代理 / 风控处理），
 * 所以这里**不建任何客户端、不读任何文件、不记任何限速或冷却**；这里只记"被调了几次、最近一次什么时候"。
 *
 * @param graph App 的那一份（用 lambda 是因为 `DesktopAppApi` 要在 `AppGraph` 建好之前交给它）。
 * @param socketPath socket 放哪；默认 [$XDG_RUNTIME_DIR/feedhub/api.sock]（没有就是 `~/.cache/feedhub/api.sock`）。
 *   测试 / 截图传临时目录。
 */
class DesktopAppApi(
    private val graph: () -> AppGraph,
    val socketPath: Path = defaultApiSocketPath(),
    /**
     * `/image` 用哪份图片加载器。默认是 **App 自己那个 Coil 单例**（`App.kt` 里
     * `setSingletonImageLoaderFactory { appImageLoader(context, graph) }` 建的那份）：这样 `/image` 的磁盘缓存
     * 就是 App 刷流时那个，同一张图 App 看过就算命中（0 个请求）。
     * 测试 / 探针（没有 Compose 界面、没有单例）传一份自己建的。
     */
    imageLoader: () -> coil3.ImageLoader = {
        // 正常的顺序是"界面第一次组合时装上工厂 → 任何请求来取单例"。但开关上一次是开着的、App 刚起来时，
        // 接口可能比第一次组合还早收到请求：那时单例还没建，直接 get 会让 Coil 用**默认**加载器占住单例
        // （没有 App 的磁盘缓存、没有 App 的代理），之后界面也只能用那个。所以先补装一次工厂再取。
        // 单例已经建好的话 setSafe 是空操作，不影响任何东西。
        val current = graph()
        coil3.SingletonImageLoader.setSafe { context -> feedhub.app.image.appImageLoader(context, current) }
        coil3.SingletonImageLoader.get(coil3.PlatformContext.INSTANCE)
    },
    /**
     * 路径上有别人在听时，
     * 隔多久再试一次、最多试多久。测试传小值。
     */
    private val busyRetryIntervalMs: Long = BUSY_RETRY_INTERVAL_MS,
    private val busyRetryWindowMs: Long = BUSY_RETRY_WINDOW_MS,
) : AppApi {
    private val _status = MutableStateFlow(AppApiStatus(socketPath = socketPath.toString()))
    override val status: StateFlow<AppApiStatus> = _status.asStateFlow()

    private val calls = AtomicLong()
    private val closed = AtomicBoolean(false)

    @Volatile
    private var lastCallAt: Long? = null

    @Volatile
    private var lastError: String? = null

    /** 后台重试线程（只在"别人在听"时起；[stop] / [close] 时打断）。由 `this` 的锁保护。 */
    private var retry: Thread? = null

    private val routes = ApiRoutes(graph, { if (_status.value.listening) socketPath.toString() else null }, imageLoader)

    private val server = UnixHttpServer(socketPath) { request ->
        val reply = routes.handle(request)
        // 只记计数、时间和错误码（**不记参数、不记内容**）
        calls.incrementAndGet()
        lastCallAt = System.currentTimeMillis()
        lastError = reply.errorCode
        // 设置页那张卡片要跟着变（次数 / 最近一次 / 最近一次失败），只更新这几个计数
        publish(enabled = _status.value.enabled)
        reply
    }

    @Synchronized
    override fun start() {
        if (closed.get() || server.listening) {
            publish(enabled = true)
            return
        }
        server.start()
        if (server.listening) {
            System.err.println("[接口] 对外接口在听：$socketPath（只读 HTTP over unix socket，给 agent 用）")
        } else {
            System.err.println("[接口] 对外接口起不来：${server.lastError ?: "未知原因"}（$socketPath）")
            if (server.busy) retryWhileBusy()
        }
        publish(enabled = true)
    }

    @Synchronized
    override fun stop() {
        cancelRetry()
        server.stop()
        if (!closed.get()) publish(enabled = false)
    }

    @Synchronized
    override fun close() {
        closed.set(true)
        cancelRetry()
        server.stop()
        publish(enabled = false)
    }

    /**
     * 路径上有别人在听：每 [busyRetryIntervalMs] 再试一次，最多 [busyRetryWindowMs]。重启 App 时旧进程
     * 还在收尾、socket 还在听，新进程以前判成"别人在听"就再也不试了，开关只能靠用户拨一下。
     * 真有另一个 feedhub 一直开着时，试满时间就停（不抢、不删人家的 socket，和以前一样报原因）。
     */
    private fun retryWhileBusy() {
        if (retry?.isAlive == true) return
        val deadline = System.nanoTime() + busyRetryWindowMs * 1_000_000
        retry = Thread({
            while (true) {
                try {
                    Thread.sleep(busyRetryIntervalMs)
                } catch (_: InterruptedException) {
                    return@Thread
                }
                if (!retryOnce(deadline)) return@Thread
            }
        }, "feedhub-api-retry").apply {
            isDaemon = true
            start()
        }
    }

    /** 再试一次；返回 true = 还要接着试。 */
    @Synchronized
    private fun retryOnce(deadline: Long): Boolean {
        if (Thread.currentThread().isInterrupted || retry !== Thread.currentThread()) return false
        if (closed.get() || !_status.value.enabled || server.listening) return false
        server.start()
        publish(enabled = true)
        if (server.listening) {
            System.err.println("[接口] 对外接口在听：$socketPath（等上一个进程让出来之后）")
            return false
        }
        return server.busy && System.nanoTime() < deadline
    }

    private fun cancelRetry() {
        retry?.interrupt()
        retry = null
    }

    private fun publish(enabled: Boolean) {
        _status.value = AppApiStatus(
            enabled = enabled,
            listening = server.listening,
            socketPath = socketPath.toString(),
            calls = calls.get(),
            lastCallAt = lastCallAt,
            lastError = lastError,
            error = if (server.listening) null else server.lastError,
        )
    }

    companion object {
        /** "别人在听"时隔多久再试。 */
        const val BUSY_RETRY_INTERVAL_MS: Long = 1_000

        /** "别人在听"时最多试多久：比 `AppGraph.shutdown` 的收尾上限（3 秒）宽裕得多。 */
        const val BUSY_RETRY_WINDOW_MS: Long = 10_000

        /**
         * socket 放哪：**优先 [XDG_RUNTIME_DIR]**（`/run/user/<uid>`，登录会话一结束系统就清掉，最不容易留垃圾），
         * 没有就退到 `~/.cache/feedhub/api.sock`（和 App 的其它缓存放一处）。
         */
        fun defaultApiSocketPath(): Path {
            val runtime = System.getenv("XDG_RUNTIME_DIR")?.trim()?.takeIf { it.isNotEmpty() }
            if (runtime != null) return Paths.get(runtime, "feedhub", "api.sock")
            val cache = System.getenv("XDG_CACHE_HOME")?.trim()?.takeIf { it.isNotEmpty() }
                ?: Paths.get(System.getProperty("user.home"), ".cache").toString()
            return Paths.get(cache, "feedhub", "api.sock")
        }
    }
}
