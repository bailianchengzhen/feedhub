package feedhub.app.translate

import android.content.Context
import feedhub.app.SettingsStore
import kotlinx.coroutines.flow.StateFlow
import java.io.File
import java.io.FileOutputStream

/**
 * 安卓端进程内本地轻量翻译引擎（Project Bergamot / Firefox Translations `enzh`）。
 *
 * 与桌面版共用同一套 `jvmShared` 的 [BergamotTranslationEngine] 与 `libfeedhub_bergamot.so`：
 * - **无子进程**：不再拉起 `llama-server` 子进程或开本机 HTTP 端口，彻底告别 1.1 GB 大模型常驻；
 * - **极小体积与按需解包**：内置英译中量化模型共约 48 MB（`assets/translate/enzh/`），首次调用 [translate] 时
 *   按字节数校验拷入 `noBackupFilesDir/translate/bergamot-enzh-v060/`（顺手清理旧版遗留的 1.1 GB `model.gguf`）；
 * - **严格省内存**：空闲 2 分钟或收到 `onForeground(false)`（App 退到后台）立即释放原生会话内存（约 180 MB），
 *   平时与后台零原生内存、零后台线程、零定时器。
 */
class AndroidTranslationEngine(
    context: Context,
    @Suppress("UNUSED_PARAMETER") settings: SettingsStore? = null,
    @Suppress("UNUSED_PARAMETER") cacheDir: java.nio.file.Path? = null,
) : TranslationEngine {

    private val appContext: Context = context.applicationContext ?: context

    private val delegate = BergamotTranslationEngine(
        loadLibrary = { loadNativeLibrary(appContext) },
        resolveModelFiles = { ensureExtractedModels() },
        idleTimeoutMs = BergamotTranslationEngine.ANDROID_IDLE_TIMEOUT_MS,
        unloadOnBackground = true,
    )

    override val id: String get() = delegate.id
    override val parallelism: Int get() = delegate.parallelism
    override val status: StateFlow<TranslationStatus> get() = delegate.status

    /** 当前 APK 是否同时打包了 `libfeedhub_bergamot.so` 与 `assets/translate/enzh/` 模型（纯文件清单检查，不触发加载）。 */
    val available: Boolean by lazy { isBundled(appContext) }

    /** 不可用时的一行原因说明；可用时返回 `null`。 */
    fun unavailableReason(): String? = Companion.unavailableReason(appContext)

    /** 原生会话当前是否驻留内存（供测试与排障检查退后台卸载行为）。 */
    fun isLoaded(): Boolean = delegate.isLoaded()

    override suspend fun translate(text: String, target: String): String {
        if (!available) error(unavailableReason() ?: "本地翻译不可用")
        return delegate.translate(text, target)
    }

    override fun onForeground(active: Boolean) {
        delegate.onForeground(active)
    }

    override fun close() {
        delegate.close()
    }

    private fun ensureExtractedModels(): BergamotModelFiles {
        val baseDir = File(appContext.noBackupFilesDir, "translate")
        val targetDir = File(baseDir, BergamotModelFiles.MODEL_ID)
        synchronized(EXTRACT_LOCK) {
            // 给升级用户腾出闪存空间
            cleanupLegacyGguf(baseDir)

            targetDir.mkdirs()
            for (name in ALL_ASSETS) {
                val expected = assetSize(appContext, "$ASSET_DIR/$name")
                if (expected <= 0L) {
                    if (name in REQUIRED_ASSETS) {
                        error("APK assets 缺少翻译模型文件：$ASSET_DIR/$name")
                    } else {
                        continue
                    }
                }
                val dst = File(targetDir, name)
                if (dst.isFile && dst.length() == expected) continue

                val tmp = File(targetDir, "$name.tmp")
                runCatching { tmp.delete() }
                appContext.assets.open("$ASSET_DIR/$name").use { input ->
                    FileOutputStream(tmp).use { out -> input.copyTo(out, 1 shl 16) }
                }
                check(tmp.length() == expected) {
                    "翻译模型文件拷贝大小不符（$name）：实际 ${tmp.length()}，期望 $expected"
                }
                check(tmp.renameTo(dst)) {
                    "翻译模型文件落盘改名失败：${tmp.absolutePath} -> ${dst.absolutePath}"
                }
            }
            return BergamotModelFiles.fromDirectory(
                targetDir,
                "内置英译中模型（${targetDir.absolutePath}）",
            )
        }
    }

    companion object {
        private const val LOG_TAG = "feedhub-translate"
        const val ASSET_DIR = "translate/enzh"

        private val REQUIRED_ASSETS = listOf(
            BergamotModelFiles.MODEL_FILE,
            BergamotModelFiles.SHORTLIST_FILE,
            BergamotModelFiles.SRC_VOCAB_FILE,
            BergamotModelFiles.TRG_VOCAB_FILE,
        )

        private val ALL_ASSETS = REQUIRED_ASSETS + BergamotModelFiles.SSPLIT_PREFIX_FILE

        private val EXTRACT_LOCK = Any()
        private var loadedNative: Result<Unit>? = null

        fun jniLibraryFile(context: Context): File? =
            File(context.applicationInfo.nativeLibraryDir, BergamotModelFiles.JNI_LIB)
                .takeIf { it.isFile && it.length() > 0L }

        fun modelAssetPresent(context: Context): Boolean = runCatching {
            val entries = context.assets.list(ASSET_DIR)?.toSet().orEmpty()
            REQUIRED_ASSETS.all { it in entries }
        }.getOrDefault(false)

        fun isBundled(context: Context): Boolean =
            jniLibraryFile(context) != null && modelAssetPresent(context)

        fun unavailableReason(context: Context): String? = when {
            jniLibraryFile(context) == null ->
                "当前安装包未包含翻译引擎运行库（${BergamotModelFiles.JNI_LIB}）"
            !modelAssetPresent(context) ->
                "当前安装包未包含英译中翻译模型（assets/$ASSET_DIR）"
            else -> null
        }

        fun describe(context: Context): String {
            unavailableReason(context)?.let { return it }
            val ver = runCatching {
                if (loadedNative?.isSuccess == true) "${BergamotNative.nativeVersion()}，" else ""
            }.getOrDefault("")
            return "内置英译中离线模型已就绪（${ver}约 48 MB，退后台或空闲 2 分钟自动释放内存）"
        }

        @Synchronized
        internal fun loadNativeLibrary(context: Context): String? {
            loadedNative?.let { prev ->
                return prev.exceptionOrNull()?.let { e -> "${e::class.simpleName}: ${e.message}" }
            }
            val result = runCatching {
                try {
                    System.loadLibrary("feedhub_bergamot")
                } catch (_: UnsatisfiedLinkError) {
                    val file = jniLibraryFile(context)
                        ?: error("找不到 ${BergamotModelFiles.JNI_LIB}")
                    System.load(file.absolutePath)
                }
            }
            loadedNative = result
            return result.exceptionOrNull()?.let { e ->
                android.util.Log.w(LOG_TAG, "加载 libfeedhub_bergamot.so 失败", e)
                "${e::class.simpleName}: ${e.message}"
            }
        }

        private fun assetSize(context: Context, assetPath: String): Long = runCatching {
            context.assets.openFd(assetPath).use { it.length }
        }.getOrElse {
            runCatching {
                context.assets.open(assetPath).use { input ->
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

        private fun cleanupLegacyGguf(baseDir: File) {
            for (legacyName in listOf("model.gguf", "model.gguf.tmp")) {
                val legacy = File(baseDir, legacyName)
                if (legacy.exists()) {
                    val freedMb = legacy.length() / 1024 / 1024
                    if (runCatching { legacy.delete() }.getOrDefault(false)) {
                        android.util.Log.i(LOG_TAG, "已清理旧版遗留翻译模型 $legacyName（释放约 $freedMb MB）")
                    }
                }
            }
        }
    }
}
