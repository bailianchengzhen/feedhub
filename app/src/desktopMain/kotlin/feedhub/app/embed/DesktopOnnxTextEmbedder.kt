package feedhub.app.embed

import feedhub.app.library.EMBEDDING_MODEL_ID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.locks.ReentrantLock

/**
 * 桌面（deb）上的文本向量：ONNX Runtime **CPU**，
 * 和安卓 CPU 路径**同一张 q4 图**（`accuracy_level=4`）、**同一个模型标识**（[EMBEDDING_MODEL_ID]）、
 * **同一个词表**（[GemmaTokenizer]），所以两边算出来的向量在同一个空间里，导出的 JSON Lines 可以直接一起回放。
 *
 * 和安卓版（`app/src/androidMain/.../OnnxTextEmbedder.kt`）的差别只有"文件从哪儿来 / 怎么调库"：
 * - 安卓从 assets 拷到私有目录、用 ORT 的 JVM 绑定；桌面是 deb 里现成的文件（[DesktopEmbedRuntime]），
 *   经过一层 JNI shim（[OrtNative]）直接调 ORT 的 C API；
 * - 线程数默认 **3**（`intraOpNumThreads`，用户桌面只有 ~4 GB 空闲内存、平时还开着别的东西），
 *   inter-op 1，**关掉 ORT 的自旋等待**（一批算完不自旋），批大小沿用调度器的 8；
 * - 计算跑在**一条自己的后台守护线程**上（`Dispatchers.Default` 留给界面 / 别的协程），一次只算一批。
 *   Linux 上 JVM 的线程优先级基本被忽略，所以"省电"靠的是线程数 + 批间 gap + 只在前台算，而不是 `nice`。
 *
 * 懒加载：构造函数只 `stat` 几个文件（判断"这个包装了没有"），词表和会话都等到第一次 [embed] 才建
 * （词表约 224 MB、会话约 155 MB RSS，[close] 释放会话、下次再懒建）。没模型时 [available] = false，[embed] 返回 null，
 * 不抛、不崩——和安卓"包里没带模型"一个样子，设置页那一行写「未包含模型」。
 */
class DesktopOnnxTextEmbedder(
    private val assets: DesktopEmbedRuntime.Assets = DesktopEmbedRuntime.assets(),
    /** ORT 的 intra-op 线程数（默认 3：4 核给界面 + 用户别的程序留余量）。 */
    private val intraOpThreads: Int = 3,
    private val log: (String) -> Unit = {},
) : Embedder, EmbeddingRuntimeInfo {
    /** 写进 `item_embedding.model` 的标识：**和安卓 CPU 路径完全一致**，所以两个设备的向量是同一个空间。 */
    override val modelId: String = EMBEDDING_MODEL_ID

    /** 这个构建 / 这台机器上文件齐不齐（只 stat，不装载 .so、不读词表）。 */
    val bundled: Boolean = assets.complete

    @Volatile
    private var failed = false

    override val available: Boolean get() = bundled && !failed

    /** 一次只算一批：会话 + 词表都由它保护。 */
    private val lock = ReentrantLock()

    /**
     * 算批的后台线程：**一条**、守护、最低优先级。
     *
     * 为什么单独一条而不是 `Dispatchers.Default`：桌面版那个池也是界面协程用的，一批 90 ms 占着一个
     * 池线程会拖慢别的协程；这里自己一条、算完就闲着（不忙等、不建定时器）。
     */
    private val worker: ExecutorService by lazy {
        Executors.newSingleThreadExecutor { r ->
            Thread(r, "feedhub-embed").apply {
                isDaemon = true
                priority = Thread.MIN_PRIORITY
            }
        }
    }

    private var session: Long = 0
    private var closeRequested = false

    @Volatile
    private var tokenizer: GemmaTokenizer? = null
    private val tokenizerLock = Any()

    /** 会话建好了没有（日志 / 排查用）。 */
    @Volatile
    var prepared: Boolean = false
        private set

    override suspend fun embed(texts: List<String>): List<FloatArray>? {
        if (texts.isEmpty()) return emptyList()
        if (!available) return null
        return withContext(worker.asCoroutineDispatcher()) {
            lock.lock()
            try {
                val tok = tokenizerOrLoad() ?: error("词表读不出来")
                val handle = session.takeIf { it != 0L } ?: openSession().also { session = it }
                runBatch(tok, handle, texts)
            } catch (e: Throwable) {
                failed = true
                log("[向量] 推理失败，这一轮不再试：${e::class.simpleName}: ${e.message}")
                null
            } finally {
                if (closeRequested) {
                    closeRequested = false
                    closeSessionLocked()
                }
                lock.unlock()
            }
        }
    }

    /** 这批文本各几个 token（含 `<bos>` / `<eos>`；调度器的"按长度分桶"用不上，但导出 / 测试要对齐）。 */
    override suspend fun tokenLengths(texts: List<String>): IntArray? = withContext(Dispatchers.Default) {
        if (!available) return@withContext null
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.tokenLengths(texts)
    }

    /** 按 token 截断（详情正文用，见 [DETAIL_MAX_TOKENS]）；和安卓同一份实现。 */
    override suspend fun truncateToTokens(text: String, maxTokens: Int): String? = withContext(Dispatchers.Default) {
        if (!available) return@withContext null
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.truncateToTokens(text, maxTokens)
    }

    /** 释放会话（约 155 MB）；正在算的话等这一批算完自己关。 */
    override fun close() {
        if (lock.tryLock()) {
            try {
                closeSessionLocked()
            } finally {
                lock.unlock()
            }
        } else {
            closeRequested = true
        }
    }

    private fun closeSessionLocked() {
        val handle = session
        session = 0
        prepared = false
        if (handle != 0L) runCatching { OrtNative.nativeClose(handle) }
    }

    // ---------------------------------------------------------------- 装载

    private fun tokenizerOrLoad(): GemmaTokenizer? {
        tokenizer?.let { return it }
        return synchronized(tokenizerLock) {
            tokenizer ?: runCatching { loadTokenizer() }
                .onFailure { log("[向量] 词表读不出来：${it.message ?: it::class.simpleName}") }
                .getOrNull()
                ?.also { tokenizer = it }
        }
    }

    private fun loadTokenizer(): GemmaTokenizer {
        val started = System.currentTimeMillis()
        val file = assets.tokenizer ?: error("找不到 ${DesktopEmbedRuntime.TOKENIZER_FILE}")
        val tok = GemmaTokenizer.fromJson(file.readBytes())
        log("[向量] 词表读好了（${file.length() / 1024 / 1024} MB，${System.currentTimeMillis() - started} ms）")
        return tok
    }

    private fun openSession(): Long {
        // shim 的加载失败要一直记得（`loaded` 里缓着失败原因），别每批都试一次
        DesktopEmbedRuntime.load(assets)?.let { reason -> error("ONNX Runtime 装载失败：$reason") }
        val model = assets.model ?: error("找不到 ${DesktopEmbedRuntime.MODEL_FILE}")
        val started = System.currentTimeMillis()
        val handle = OrtNative.nativeCreate(model.absolutePath, intraOpThreads, 1)
        check(handle != 0L) { "建会话失败：${OrtNative.nativeLastError()}" }
        prepared = true
        log(
            "[向量] 会话建好了（${System.currentTimeMillis() - started} ms，$intraOpThreads 线程，" +
                "ORT ${runCatching { OrtNative.nativeVersion() }.getOrDefault("?")}，${model.name}）",
        )
        return handle
    }

    // ---------------------------------------------------------------- 推理

    /** 和安卓 `OnnxTextEmbedder.runBatch` 同一套：每条最多 [GemmaTokenizer.DEFAULT_MAX_TOKENS] 个 token、
     * 批内右补到最长那条、喂 5 个输入、取 `sentence_embedding`、逐条 L2 归一化。 */
    private fun runBatch(tokenizer: GemmaTokenizer, handle: Long, texts: List<String>): List<FloatArray>? {
        // 整批一次过边界：分词在 Rust 里，别一条一条调
        val encoded = tokenizer.encodeBatch(texts, GemmaTokenizer.DEFAULT_MAX_TOKENS)
        val maxLen = encoded.maxOf { it.size }.coerceAtLeast(1)
        val batch = encoded.size
        val ids = LongArray(batch * maxLen)
        val mask = LongArray(batch * maxLen)
        for (b in encoded.indices) {
            val row = encoded[b]
            for (i in row.indices) {
                ids[b * maxLen + i] = row[i].toLong()
                mask[b * maxLen + i] = 1L
            }
        }
        val flat = OrtNative.nativeEmbed(handle, ids, mask, batch, maxLen)
            ?: error("推理返回空：${OrtNative.nativeLastError()}")
        val dim = flat.size / batch
        check(dim > 0 && dim * batch == flat.size) { "输出大小不对：${flat.size} / $batch = $dim" }
        return (0 until batch).map { b ->
            val v = flat.copyOfRange(b * dim, (b + 1) * dim)
            l2NormalizeInPlace(v)
        }
    }

    /** 设置页「当前实际用的后端」那行：`ONNX Runtime 1.31.0 · CPU · 3 线程`。 */
    override val runtimeLabel: String
        get() {
            val version = if (prepared) runCatching { OrtNative.nativeVersion() }.getOrNull() else null
            return if (version != null) "CPU · $intraOpThreads 线程 · ORT $version" else "CPU · $intraOpThreads 线程"
        }

    /** 模型文件。 */
    fun describeSource(): String = assets.describe()

    /** 模型文件本身（测试 / 排查用）。 */
    fun modelFile(): File? = assets.model
}
