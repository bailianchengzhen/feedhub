package feedhub.app.translate

import feedhub.translate.RustTranslate
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * 一组完整的 Bergamot 离线翻译模型文件（默认对应 Mozilla Firefox Release 的英译中 `enzh` base-memory 模型）。
 */
data class BergamotModelFiles(
    val model: File?,
    val srcVocab: File?,
    val trgVocab: File?,
    val shortlist: File?,
    val ssplitPrefix: File?,
    /** 来源说明（用于设置页状态展示与排障，不含用户内容）。 */
    val source: String,
) {
    val complete: Boolean
        get() = listOf(model, srcVocab, trgVocab, shortlist).all { it != null && it.isFile && it.length() > 0L }

    val missing: List<String>
        get() = buildList {
            if (model == null || !model.isFile) add(MODEL_FILE)
            if (srcVocab == null || !srcVocab.isFile) add(SRC_VOCAB_FILE)
            if (trgVocab == null || !trgVocab.isFile) add(TRG_VOCAB_FILE)
            if (shortlist == null || !shortlist.isFile) add(SHORTLIST_FILE)
        }

    fun totalBytes(): Long =
        listOfNotNull(model, srcVocab, trgVocab, shortlist, ssplitPrefix)
            .distinctBy { it.absolutePath }
            .filter { it.isFile }
            .sumOf { it.length() }

    fun describe(): String = if (complete) {
        val mb = "%.1f".format(totalBytes() / 1024.0 / 1024.0)
        "$source（共 $mb MB）"
    } else {
        "缺少 ${missing.joinToString(" / ")}（查找位置：$source）"
    }

    companion object {
        /** 翻译缓存键里的引擎与模型版本盐（与旧 HY-MT GGUF 缓存自动隔离）。 */
        const val MODEL_ID = "bergamot-enzh-v060"

        /** 当前内置模型仅支持英文（`en`）源语言译入简体中文；日/韩等其他语种暂不触发翻译。 */
        val SUPPORTED_SOURCES: Set<String> = setOf("en")

        const val JNI_LIB = "libfeedhub_bergamot.so"
        const val MODEL_FILE = "model.enzh.intgemm.alphas.bin"
        const val SHORTLIST_FILE = "lex.50.50.enzh.s2t.bin"
        const val SRC_VOCAB_FILE = "srcvocab.enzh.spm"
        const val TRG_VOCAB_FILE = "trgvocab.enzh.spm"
        const val SSPLIT_PREFIX_FILE = "nonbreaking_prefix.en"

        /**
         * 从目录解析 Bergamot 模型文件：优先匹配内置 `enzh` 标准文件名，同时兼容用户在桌面端指定
         * 任意 Mozilla Bergamot 模型目录（如 `model.*.intgemm*.bin` + 分离或共用 `.spm` 词表）。
         */
        fun fromDirectory(dir: File, sourceLabel: String = dir.absolutePath): BergamotModelFiles {
            if (!dir.isDirectory) {
                return BergamotModelFiles(null, null, null, null, null, sourceLabel)
            }
            val entries = dir.listFiles()?.filter { it.isFile }.orEmpty()
            val model = File(dir, MODEL_FILE).takeIf { it.isFile }
                ?: entries.firstOrNull { it.name.startsWith("model.") && it.name.endsWith(".bin") }
            val shortlist = File(dir, SHORTLIST_FILE).takeIf { it.isFile }
                ?: entries.firstOrNull { it.name.startsWith("lex.") && it.name.endsWith(".bin") }
            val sharedVocab = entries.firstOrNull { it.name.startsWith("vocab.") && it.name.endsWith(".spm") }
            val srcVocab = File(dir, SRC_VOCAB_FILE).takeIf { it.isFile }
                ?: entries.firstOrNull { it.name.startsWith("srcvocab.") && it.name.endsWith(".spm") }
                ?: sharedVocab
            val trgVocab = File(dir, TRG_VOCAB_FILE).takeIf { it.isFile }
                ?: entries.firstOrNull { it.name.startsWith("trgvocab.") && it.name.endsWith(".spm") }
                ?: sharedVocab
            val prefix = File(dir, SSPLIT_PREFIX_FILE).takeIf { it.isFile }
                ?: entries.firstOrNull { it.name.startsWith("nonbreaking_prefix.") }
            return BergamotModelFiles(
                model = model,
                srcVocab = srcVocab,
                trgVocab = trgVocab,
                shortlist = shortlist,
                ssplitPrefix = prefix,
                source = sourceLabel,
            )
        }
    }
}

/**
 * 进程内轻量本地翻译引擎（Project Bergamot，桌面 Linux 与 Android 双端共用）。
 *
 * 生命周期与资源纪律：
 * - **按需懒加载**：构造时不加载 `.so`、不读模型、不起任何后台线程或定时器；首次调用 [translate] 时
 *   状态从 [TranslationStatus.STOPPED] → [TranslationStatus.STARTING] → [TranslationStatus.READY]；
 * - **空闲自动释放**：超过 [idleTimeoutMs]（桌面默认 5 分钟、安卓默认 2 分钟）无新请求时自动释放原生模型内存
 *   （约 180 MB），状态回到 [TranslationStatus.STOPPED]，空闲协程随即退出（**后台零定时器**）；
 * - **安卓退后台立即释放**：[unloadOnBackground] 为 `true` 时，收到 `onForeground(false)` 立刻释放原生模型。
 */
class BergamotTranslationEngine(
    private val loadLibrary: () -> String?,
    private val resolveModelFiles: () -> BergamotModelFiles,
    private val idleTimeoutMs: Long = DESKTOP_IDLE_TIMEOUT_MS,
    private val unloadOnBackground: Boolean = false,
    private val idleCheckIntervalMs: Long = DEFAULT_IDLE_CHECK_MS,
    private val clockMs: () -> Long = System::currentTimeMillis,
) : TranslationEngine {

    override val id: String = BergamotModelFiles.MODEL_ID

    /** 单副本 `BlockingService` 串行推理（单句中位延迟 ~12 ms，省去多副本额外内存占用）。 */
    override val parallelism: Int = 1

    private val statusFlow = MutableStateFlow(TranslationStatus.STOPPED)
    override val status: StateFlow<TranslationStatus> = statusFlow.asStateFlow()

    private val sessionLock = ReentrantLock()
    private val closed = AtomicBoolean(false)
    private val timerScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private var handle: Long = 0L
    private var loadedSignature: String = ""
    private var lastUsedMs: Long = 0L
    private var idleJob: Job? = null

    @Volatile
    private var foreground: Boolean = true

    @Volatile
    var lastErrorMessage: String? = null
        private set

    override suspend fun translate(text: String, target: String): String {
        if (closed.get()) error("本地翻译引擎已关闭")
        if (!RustTranslate.matchesTarget("zh", target)) {
            error("当前内置模型仅支持翻译为简体中文（en → zh）")
        }
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return text

        return withContext(Dispatchers.IO) {
            sessionLock.withLock {
                if (closed.get()) error("本地翻译引擎已关闭")
                val activeHandle = ensureLoadedLocked()
                val utf8In = text.encodeToByteArray()
                val utf8Out = BergamotNative.nativeTranslate(activeHandle, utf8In)
                lastUsedMs = clockMs()
                if (utf8Out == null) {
                    val detail = BergamotNative.nativeLastError().ifBlank { "未知推理错误" }
                    lastErrorMessage = detail
                    error("本地翻译失败：$detail")
                }
                // 若翻译途中 App 已切到后台（安卓），本条完成后顺手释放模型内存
                if (unloadOnBackground && !foreground) {
                    unloadLocked()
                }
                utf8Out.decodeToString()
            }
        }
    }

    /** 供测试与排障查看原生会话当前是否驻留内存。 */
    fun isLoaded(): Boolean = sessionLock.withLock { handle != 0L }

    /** 主动检查一次空闲超时（测试可注入时钟后直接调用）。 */
    fun reapIfIdle(): Boolean = sessionLock.withLock {
        if (handle != 0L && clockMs() - lastUsedMs >= idleTimeoutMs) {
            unloadLocked()
            true
        } else {
            false
        }
    }

    override fun onForeground(active: Boolean) {
        foreground = active
        if (!active && unloadOnBackground) {
            // 不阻塞主线程：能立刻拿到锁就当场释放，否则正在跑一条 ~12ms 的翻译，交由 IO 协程收尾释放
            if (sessionLock.tryLock()) {
                try {
                    if (!foreground) unloadLocked()
                } finally {
                    sessionLock.unlock()
                }
            } else {
                timerScope.launch(Dispatchers.IO) {
                    sessionLock.withLock {
                        if (!foreground && unloadOnBackground) {
                            unloadLocked()
                        }
                    }
                }
            }
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        sessionLock.withLock {
            unloadLocked()
        }
        timerScope.cancel()
    }

    private fun ensureLoadedLocked(): Long {
        val files = resolveModelFiles()
        if (!files.complete) {
            val msg = "未找到翻译模型文件：${files.describe()}"
            lastErrorMessage = msg
            statusFlow.value = TranslationStatus.FAILED
            error(msg)
        }
        val sig = listOfNotNull(files.model, files.srcVocab, files.trgVocab, files.shortlist, files.ssplitPrefix)
            .joinToString("|") { "${it.absolutePath}:${it.length()}:${it.lastModified()}" }

        if (handle != 0L && sig == loadedSignature) {
            lastUsedMs = clockMs()
            ensureIdleWatchLocked()
            return handle
        }

        // 模型目录切换过：先释放旧句柄再加载新模型
        if (handle != 0L) {
            unloadLocked()
        }

        statusFlow.value = TranslationStatus.STARTING
        val libErr = loadLibrary()
        if (libErr != null) {
            val msg = "加载翻译引擎运行库失败：$libErr"
            lastErrorMessage = msg
            statusFlow.value = TranslationStatus.FAILED
            error(msg)
        }

        val created = BergamotNative.nativeCreate(
            modelPath = files.model!!.absolutePath,
            srcVocabPath = files.srcVocab!!.absolutePath,
            trgVocabPath = files.trgVocab!!.absolutePath,
            shortlistPath = files.shortlist!!.absolutePath,
            ssplitPrefixPath = files.ssplitPrefix?.absolutePath,
        )
        if (created == 0L) {
            val nativeErr = runCatching { BergamotNative.nativeLastError() }.getOrDefault("")
            val msg = nativeErr.ifBlank { "加载 Bergamot 翻译模型失败" }
            lastErrorMessage = msg
            statusFlow.value = TranslationStatus.FAILED
            error(msg)
        }

        handle = created
        loadedSignature = sig
        lastUsedMs = clockMs()
        lastErrorMessage = null
        statusFlow.value = TranslationStatus.READY
        ensureIdleWatchLocked()
        return created
    }

    private fun ensureIdleWatchLocked() {
        if (idleTimeoutMs <= 0L || closed.get()) return
        if (idleJob?.isActive == true) return
        val stepMs = minOf(idleCheckIntervalMs, idleTimeoutMs).coerceAtLeast(50L)
        idleJob = timerScope.launch {
            while (!closed.get()) {
                delay(stepMs)
                val keepRunning = sessionLock.withLock {
                    if (handle == 0L || closed.get()) {
                        false
                    } else if (clockMs() - lastUsedMs >= idleTimeoutMs) {
                        unloadLocked()
                        false
                    } else {
                        true
                    }
                }
                if (!keepRunning) break
            }
        }
    }

    private fun unloadLocked() {
        idleJob?.cancel()
        idleJob = null
        val h = handle
        handle = 0L
        loadedSignature = ""
        if (h != 0L) {
            runCatching { BergamotNative.nativeClose(h) }
            statusFlow.value = TranslationStatus.STOPPED
        } else if (statusFlow.value == TranslationStatus.READY || statusFlow.value == TranslationStatus.STARTING) {
            statusFlow.value = TranslationStatus.STOPPED
        }
    }

    companion object {
        const val DESKTOP_IDLE_TIMEOUT_MS = 5 * 60 * 1_000L
        const val ANDROID_IDLE_TIMEOUT_MS = 2 * 60 * 1_000L
        private const val DEFAULT_IDLE_CHECK_MS = 30_000L
    }
}
