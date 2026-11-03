package feedhub.app.translate

import android.content.Context
import feedhub.app.SettingsStore
import feedhub.app.settings.AppInfo
import feedhub.rust.RustStoreHost
import feedhub.translate.RustTranslateEngine
import feedhub.translate.TrEngineConfig
import feedhub.translate.TrEngineStatus
import feedhub.translate.TrTimeouts
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * **安卓上的本地翻译引擎**。
 *
 * 和桌面 [feedhub.app.translate.LlamaTranslationEngine] 是**同一份 Rust**
 * （`feedhub-core/src/translate/`：进程启停 / 就绪判断 / 空闲回收 / 重启策略 / chat 请求），
 * 这个类只做安卓特有的四件事（都落在"留宿主"那几类里）：
 *
 * | 这儿做的 | 分类 | 为什么 |
 * | 定位 `nativeLibraryDir/libllamaserver.so` | 调操作系统 | Android 只允许执行 `nativeLibraryDir` 下的文件 |
 * | 首次用时把 assets 里的 GGUF **流式**拷到私有目录 | 读写文件（启动那一下） | `llama-server` 要文件路径，assets 里的不是一个普通路径 |
 * | 空闲心跳（60 秒问一次 Rust）+ 退后台立刻停 | 定时器 / 前后台 | `.so` 里不起线程、不读时钟 |
 * | `StateFlow` | 状态容器 | 和桌面一模一样；**发请求不在宿主** |
 *
 * ## 和桌面的四个不同
 *
 * 1. **后端**：不走桌面的 Vulkan AMD 核显 名字匹配，走 Rust 的 `translate::device` 那条
 *    **NPU（Hexagon HTP）→ GPU（Adreno OpenCL）→ NPU+GPU 分层 → CPU** 的自动降级
 *    （`TrEngineConfig.mobile = true`）。每档要过"进程活着 + `/health` 通 + 真翻一句不像坏"
 *    三道判据，失败降一档并记进 `state.json` 的 `translate.backend.v1`（带 App 版本，
 *    升级后重新从 NPU 试）。`AppSettings.translationAndroidBackend` 是**强制某一档**的覆盖
 *    （`null` = 自动，默认），只给排错 / `tools/translate/phone_bench.sh` 用。
 * 2. **时间常量**：并发槽 1、空闲 2 分钟、30 次重启（[TrTimeouts.android]；桌面是 2 / 5 分钟 / 150）。
 *    `-t` 由 Rust 的 device 层按档位给（前三档 4、CPU 档按核数一半）。
 * 3. **退到后台立刻停 server**：手机上一个 1.8B 模型常驻就是 1.1 GB 内存，用户切走了就该让出来
 *    （桌面只在空闲 5 分钟后停）。[onForeground] 由 `AppGraph.onForeground` 喂。
 * 4. **降级记忆要 `ctx.store`**：引擎用 `withStore` 建（同一个 `state.json`），不然"降到哪一档"
 *    只活在内存里，App 一重启又从 NPU 试一遍。
 *
 * ## 模型从 assets 拷出来（取舍）
 *
 * llama.cpp 的 `llama-server` 要**文件路径**（`-m <path>`），APK 里的 asset 不是一个能 `open()` 的普通路径
 * （它是 zip 里的一段，得先解出来）。三条路：
 * - **流式拷到私有目录**（选的这条）：`noCompress += "gguf"` 之后是一次纯 IO 复制（1.1 GB 大约 3～6 秒，
 *   只在**第一次**用时做一次）；写在 `noBackupFilesDir`（不进云备份、不占用户可清理的缓存配额），
 *   拷完校验**字节数**（sha256 已经在构建时钉过了，运行时不重复算 1.1 GB）；`.tmp` + `renameTo` 落盘，
 *   拷一半被杀掉下次会重来（不会留一个半截的 model.gguf 让 llama-server 报一个看不懂的错）。
 * - 不拷、直接用 `/proc/self/fd/<n>`：llama.cpp 会 mmap 它，但 asset 的 fd 在 APK 里是**压缩段**
 *   （除非 noCompress），而且进程一换 fd 就没了，完全不稳。
 * - 把模型放外置存储让用户自己拷：多一步手动操作，用户要的是"装上就能用"。
 */
class AndroidTranslationEngine(
    private val context: Context,
    private val settings: SettingsStore,
    private val cacheDir: java.nio.file.Path,
) : TranslationEngine {
    /**
     * 缓存键的一部分（换模型要换 id，旧译文不会串）。和桌面同一个模型、同一个 id——
     * 两边的 `translations.jsonl` 键因此**互通**（但两边的缓存目录不同，实际不共享）。
     */
    override val id: String = "hy-mt1.5-1.8b-q4"

    /** `-np`：安卓固定 **1**（见 [TrTimeouts.android]）；值从 Rust 现问，别两处各写一个常量。 */
    override val parallelism: Int get() = if (engineCreated) engine?.parallelism() ?: ANDROID_PARALLELISM else ANDROID_PARALLELISM

    private val statusFlow = MutableStateFlow(TranslationStatus.STOPPED)
    override val status: StateFlow<TranslationStatus> = statusFlow.asStateFlow()

    @Volatile
    private var engineCreated = false

    /**
     * Rust 引擎。**懒建**：没有翻译请求时连 `.so` 都不碰（`RustNative` 那边也是懒加载）。
     * 建不出来（`.so` 缺失 / ABI 不对）→ null，[available] 为 false，界面显示"未包含翻译引擎"。
     */
    private val engine: RustTranslateEngine? by lazy {
        runCatching {
            RustTranslateEngine(
                statusSink = { s ->
                    val mapped = s.toTranslationStatus()
                    statusFlow.value = mapped
                    if (mapped == TranslationStatus.READY && foreground) {
                        startHeartbeat()
                    } else if (mapped == TranslationStatus.STOPPED || mapped == TranslationStatus.FAILED) {
                        stopHeartbeat()
                    }
                },
                // 降级记忆落 `ctx.store`（`translate.backend.v1`，和设置同一个 `state.json`）：
                // 不传的话"降到哪一档"只活在内存里，App 一重启又从 NPU 试一遍。
                store = RustStoreHost(settings.rawStore),
                timeouts = TrTimeouts.android(),
            ).also { engineCreated = true }
        }.onFailure { e ->
            android.util.Log.w(LOG_TAG, "本地翻译引擎建不起来：${e.message}")
        }.getOrNull()
    }

    private val closed = AtomicBoolean(false)

    /** 空闲心跳（和桌面同一个口径：定时器是宿主的活，每 60 秒问一次 Rust）。 */
    @Volatile
    private var heartbeat: Thread? = null

    @Volatile
    private var foreground = true

    /**
     * 这个组装能不能翻译：**包里带了模型 + `nativeLibraryDir` 里有可执行文件**。
     *
     * 两个都要有：只有可执行文件没有模型 = 起得来但翻不了（llama-server 会报"找不到模型"）；
     * 只有模型没有可执行文件 = 起不来（构建时没编 / ABI 不对）。
     *
     * **故意不查 [engine]**：那个会加载 `libfeedhub_ffi.so`（17 MB 的 `.so` 映射）。`AppGraph` 构造时
     * 就会问一次 [available]（决定界面显不显示翻译入口），不该在那一步就把 Rust 核心拉起来——
     * 用户可能一次翻译都不用。Rust 核心真加载不了时，第一次 [translate] 会报一条说得清的错误
     * （`RustNative.ensureLoaded` 的文案），那比"入口都不显示"更容易排障。
     *
     * 两个便宜的条件：一次 `File.isFile`（`nativeLibraryDir`）+ 一次 assets 清单（不读内容）。
     */
    val available: Boolean by lazy {
        serverBinary() != null && modelAssetPresent()
    }

    /** 为什么不可用（一行给界面看的文案；可用时是 null）。 */
    fun unavailableReason(): String? = when {
        serverBinary() == null -> "这个 APK 里没有 llama-server（构建时没跑 tools/translate/build-android.sh）"
        !modelAssetPresent() -> "这个 APK 里没有翻译模型（构建时没加 -Pfeedhub.translateModel）"
        else -> null
    }

    override suspend fun translate(text: String, target: String): String {
        val e = engine ?: error(unavailableReason() ?: "本地翻译不可用")
        val s = settings.value
        return e.translate(
            TrEngineConfig(
                enabled = s.translationOn,
                modelPath = ensureModelFile().absolutePath,
                serverPath = serverBinary()?.absolutePath ?: error("找不到 llama-server"),
                cacheDir = cacheDir,
                // 安卓不传 ICD：手机上那条路根本不碰 Vulkan（没有 RADV，也没有 独显）
                icdFile = null,
                // 降级记忆的失效钥匙：App 升级后重新从 NPU 试（新版本可能修好了 skel / 驱动）
                appVersion = AppInfo.VERSION,
                // **手机形态**：不走桌面的"没有 AMD 核显 就不启动"，走候选档位 + CPU 兜底。
                // x86_64 模拟器上一个加速设备都没有，但它仍然是手机形态。
                mobile = true,
                // 强制档位（排错 / 测速）；`null` = 自动降级
                forcedBackend = s.translationAndroidBackend?.name,
            ),
            text = text,
            target = target,
        )
    }

    /**
     * App 进 / 退前台（`AppGraph.onForeground` 调）。
     *
     * 退到后台：**立刻**停 `llama-server`（手机上一个 1.8B 模型常驻就是 1.1 GB，用户切走了就该让出来）。
     * 回前台：只在 server 已经在跑时恢复心跳检查；没在跑的话下一次 [translate] 会自己再起
     * （Rust 的 `server_port` 看到没有进程就起）。
     *
     * 第 1 轮（E10）曾改成"退后台也常驻"，同步回开发仓库时没有采纳。
     */
    override fun onForeground(active: Boolean) {
        foreground = active
        if (!active) {
            stopHeartbeat()
            // 走 Rust 的 shutdown 会把这个引擎标成"已关闭"、之后不能再 translate；
            // 这里只要**停进程**。没建过引擎就别为了停它去建一个（懒建会加载 `.so`）。
            if (engineCreated) runCatching { engine?.stopServerBlocking() }
        } else if (engineCreated && statusFlow.value == TranslationStatus.READY) {
            startHeartbeat()
        }
    }

    private fun startHeartbeat() {
        if (heartbeat?.isAlive == true) return
        heartbeat = thread(name = "feedhub-translate-idle", isDaemon = true) {
            while (!closed.get() && foreground) {
                // **sleep 要自己吞掉 InterruptedException**：`stopHeartbeat` 用 interrupt 叫醒它，
                // 不接就是一次未捕获异常。醒来之后循环条件会再看一遍。
                try {
                    Thread.sleep(IDLE_HEARTBEAT_MS)
                } catch (_: InterruptedException) {
                    return@thread
                }
                if (closed.get() || !foreground) break
                // 只有没有请求在跑时才停（重启不能打断正在跑的请求）——那个判断在 Rust 里
                runCatching { engine?.reapBlocking() }
            }
        }
    }

    private fun stopHeartbeat() {
        heartbeat?.interrupt()
        heartbeat = null
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        stopHeartbeat()
        if (engineCreated) {
            runCatching { engine?.close() }
        }
    }

    // -----------------------------------------------------------------------
    // 后端 / 可执行文件 / 模型
    // -----------------------------------------------------------------------

    /**
     * `llama-server` 在哪：**只认 `nativeLibraryDir`**（Android 只允许执行那下面的文件；
     * `filesDir` 从 Android 10 起是 noexec，拷出来也起不来）。找不到就 null。
     *
     * **只有一份可执行文件**：`tools/translate/build-android.sh` 把 CPU + OpenCL + Hexagon
     * 三个后端编进同一个静态文件，后端由 `--device` / `-ngl` 在运行时挑（见 Rust `translate::device`）。
     * 不再有 `libllamaserver-opencl.so` 那种"按设置换文件名"的分叉。
     *
     * 调试用的环境变量 `FEEDHUB_LLAMA_SERVER`（和 sing-box 那个 `FEEDHUB_SINGBOX` 同一个思路）。
     */
    private fun serverBinary(): File? = listOfNotNull(
        System.getenv(ENV_SERVER)?.takeIf { it.isNotBlank() }?.let { File(it) },
        File(context.applicationInfo.nativeLibraryDir, SERVER_BINARY),
    ).firstOrNull { it.isFile }

    /** assets 里有没有模型（只查一遍清单，不读内容）。 */
    private fun modelAssetPresent(): Boolean = runCatching {
        context.assets.open("$ASSET_DIR/$ASSET_MODEL").use { it.available() > 0 }
    }.getOrDefault(false)

    /**
     * 把 assets 里的 GGUF 拷到私有目录（**第一次**用时做一次；之后按字节数确认一遍就复用）。
     *
     * `noCompress += "gguf"` 之后这是一次纯 IO 复制。落盘用 `.tmp` + `renameTo`：
     * 拷一半被杀掉（用户点了翻译马上切走）下次会重来，不会留一个半截的 `model.gguf`
     * 让 llama-server 报一个看不懂的错。
     */
    private fun ensureModelFile(): File {
        val dir = File(context.noBackupFilesDir, "translate")
        val target = File(dir, "model.gguf")
        synchronized(MODEL_LOCK) {
            val expected = assetSize()
            if (target.isFile && expected > 0 && target.length() == expected) return target
            dir.mkdirs()
            val tmp = File(dir, "model.gguf.tmp")
            runCatching { tmp.delete() }
            context.assets.open("$ASSET_DIR/$ASSET_MODEL").use { input ->
                FileOutputStream(tmp).use { out -> input.copyTo(out, 1 shl 16) }
            }
            check(tmp.length() > 0) { "翻译模型拷出来是空的（assets 里没有 $ASSET_MODEL）" }
            if (expected > 0) {
                check(tmp.length() == expected) {
                    "翻译模型大小不对：拷了 ${tmp.length()}，assets 里是 $expected"
                }
            }
            check(tmp.renameTo(target)) { "翻译模型改名失败（${tmp.absolutePath} → ${target.absolutePath}）" }
            android.util.Log.i(
                LOG_TAG,
                "翻译模型已拷到私有目录：${target.absolutePath}（${target.length() / 1024 / 1024} MB）",
            )
            return target
        }
    }

    private fun assetSize(): Long = runCatching {
        context.assets.openFd("$ASSET_DIR/$ASSET_MODEL").use { it.length }
    }.getOrElse {
        // 有的 APK 打包方式下 openFd 拿不到（例如压缩过的 asset）：退回读一遍长度
        runCatching {
            context.assets.open("$ASSET_DIR/$ASSET_MODEL").use { input ->
                val buf = ByteArray(1 shl 16)
                var total = 0L
                while (true) {
                    val n = input.read(buf)
                    if (n <= 0) break
                    total += n
                }
                total
            }
        }.getOrDefault(-1L)
    }

    private companion object {
        const val LOG_TAG = "feedhub-translate"

        /** assets 里的目录 / 文件名（和 `androidApp/build.gradle.kts` 的 `stageTranslationModel` 对应）。 */
        const val ASSET_DIR = "translate"
        const val ASSET_MODEL = "model.gguf"

        /**
         * 可执行文件名（`nativeLibraryDir` 下）。**只有一份**：CPU + Adreno OpenCL + Hexagon HTP
         * 三个后端编在同一个静态文件里（`tools/translate/build-android.sh`），后端由 argv / 环境变量挑。
         */
        const val SERVER_BINARY = "libllamaserver.so"

        /** 调试用的可执行文件覆盖（和 `FEEDHUB_SINGBOX` 同一个思路）。 */
        const val ENV_SERVER = "FEEDHUB_LLAMA_SERVER"

        /** 空闲回收的心跳间隔（和桌面同一个数字）。 */
        const val IDLE_HEARTBEAT_MS = 60_000L

        /** 并发槽数的兜底值（引擎还没建出来时读它）；真值从 Rust 现问。 */
        const val ANDROID_PARALLELISM = 1

        /** 拷模型是全局一次（多个翻译并发进来时别拷两遍）。 */
        val MODEL_LOCK = Any()
    }
}

private fun TrEngineStatus.toTranslationStatus(): TranslationStatus = when (this) {
    TrEngineStatus.STOPPED -> TranslationStatus.STOPPED
    TrEngineStatus.STARTING -> TranslationStatus.STARTING
    TrEngineStatus.READY -> TranslationStatus.READY
    TrEngineStatus.FAILED -> TranslationStatus.FAILED
}
