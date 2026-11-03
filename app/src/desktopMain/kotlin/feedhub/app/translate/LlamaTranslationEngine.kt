package feedhub.app.translate

import feedhub.app.SettingsStore
import feedhub.app.settings.AppInfo
import feedhub.core.KeyValueStore
import feedhub.translate.RustTranslateEngine
import feedhub.translate.TrEngineConfig
import feedhub.translate.TrEngineStatus
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * 本地翻译：`llama.cpp` 的 `llama-server`（Vulkan / **只允许 AMD 核显**）+ HY-MT GGUF。
 *
 *
 * | 原来在这儿的 | 现在在哪 |
 * | 找 `llama-server` / 模型 / ICD、`--list-devices`、按名字挑 AMD 核显、`-dev VulkanN` | Rust `translate::server` |
 * | 起进程 / 等 `/health` 就绪 / 停进程（SIGTERM→SIGKILL）/ 空闲 5 分钟 / 累计 150 次重启 | Rust `translate::engine` |
 * | chat 请求的 body / 解析 / `max_tokens` 规则 | Rust `translate::engine` |
 * | 空闲心跳定时器（每 60 秒问一次）、状态 Flow | 还在这儿（宿主的活） |
 * | 到 `llama-server` 的 HTTP | **Rust** |
 *
 * 生命周期没变：
 * - 第一次 [translate] 才启动，状态从 [TranslationStatus.STARTING] 到 [TranslationStatus.READY]；
 *   状态由 Rust 推过来（`FfiTrStatusHost`），界面上的"正在加载模型"还是实时的；
 * - **启动前先按名字选设备**：只加载 RADV 驱动、只认名字里带 `RADV` / `Radeon` 的设备，
 *   找不到就报错不启动，绝不退回独显；
 * - **并发**：[parallelism] = `-np`，同时最多这么多请求真正在跑；排队在 [TranslationScheduler] 里；
 * - 空闲 5 分钟或累计 150 次请求后停掉，**只在没有请求在跑时停**，重启不会打断正在跑的请求；
 * - App 退出时 [close] 收进程。
 *
 * 只连 `127.0.0.1`：Rust 那份客户端是**直连**的（`ProxyPlan::direct()`），不走用户设置的代理。
 */
class LlamaTranslationEngine(
    private val settings: SettingsStore,
    private val cacheDir: Path,
    /**
     * `ctx.store`：手机上记"上次降到哪一档"（`translate.backend.v1`），桌面也会写一份但只当排障记录。
     * 关键是**同一个 KeyValueStore**（`state.json`），不然升级后那条记录清不掉、也没法跨进程看。
     */
    private val store: KeyValueStore,
) : TranslationEngine {
    override val id: String = "hy-mt1.5-1.8b-q4"

    /**
     * `-np`：同时能处理几个请求。
     * 单条延迟只涨三成，4 槽的延迟代价（+78%）对"卡片一条条翻出来"的观感不划算。
     *
     * 值本身在 Rust（`translate::engine::PARALLELISM`，同一个数字），这里现问一次，
     * 免得两处各写一个常量。
     */
    override val parallelism: Int get() = engine.parallelism()

    private val statusFlow = MutableStateFlow(TranslationStatus.STOPPED)
    override val status: StateFlow<TranslationStatus> = statusFlow.asStateFlow()

    /**
     * 引擎。**到 `llama-server` 的 HTTP 在 Rust**：这里不再建 OkHttp 客户端，
     * 也不再有连接池要释放——那份专用客户端（直连 + 2 秒连接超时 + 120 秒读超时）在
     * `feedhub-ffi/src/translate.rs` 的 `local_translate_client`。
     */
    private val engine = RustTranslateEngine(
        statusSink = { s -> statusFlow.value = s.toTranslationStatus() },
        store = feedhub.rust.RustStoreHost(store),
    )

    private val closed = AtomicBoolean(false)

    init {
        Runtime.getRuntime().addShutdownHook(Thread { close() })
        // 空闲回收的心跳：定时器是宿主的活（`.so` 里不起线程、不读时钟），每 60 秒问一次 Rust
        thread(name = "feedhub-translate-idle", isDaemon = true) {
            while (!closed.get()) {
                Thread.sleep(60_000)
                if (closed.get()) break
                // 只有没有请求在跑时才停（重启不能打断正在跑的请求）——那个判断在 Rust 里
                runCatching { engine.reapBlocking() }
            }
        }
    }

    override suspend fun translate(text: String, target: String): String {
        val s = settings.value
        return engine.translate(
            TrEngineConfig(
                enabled = s.translationOn,
                modelPath = s.translationModelPath,
                serverPath = s.translationServerPath,
                cacheDir = cacheDir,
                appVersion = AppInfo.VERSION,
                // 桌面永远不是手机：AMD 核显 红线那条路（就算哪天真有 HTP / OpenCL 也不改规则）
                mobile = false,
            ),
            text = text,
            target = target,
        )
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        // 普通线程上收尾：关窗口的收尾线程 / shutdown hook / IO 线程谁来调都行
        runCatching { engine.close() }
    }
}

private fun TrEngineStatus.toTranslationStatus(): TranslationStatus = when (this) {
    TrEngineStatus.STOPPED -> TranslationStatus.STOPPED
    TrEngineStatus.STARTING -> TranslationStatus.STARTING
    TrEngineStatus.READY -> TranslationStatus.READY
    TrEngineStatus.FAILED -> TranslationStatus.FAILED
}
