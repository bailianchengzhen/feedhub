package feedhub.app.embed

import ai.onnxruntime.OnnxTensor
import ai.onnxruntime.OrtEnvironment
import ai.onnxruntime.OrtSession
import android.content.Context
import android.util.Log
import feedhub.app.library.EMBEDDING_MODEL_ID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileOutputStream
import java.nio.FloatBuffer
import java.nio.LongBuffer
import java.util.concurrent.locks.ReentrantLock

/**
 * 安卓上的文本向量：ONNX Runtime + **打进安装包**的 EmbeddingGemma 2 q4 图。
 *
 * 模型文件（`androidApp/src/main/assets/embed/`，构建时从 `-Pfeedhub.embedModelDir` 拷进去，
 * - `model_q4.onnx_data`：它的外部权重（174 MB，`noCompress`，两个文件必须在同一目录）；
 * - `tokenizer.json`：Gemma 的 BPE + byte fallback 词表（Kotlin 实现见 [GemmaTokenizer]）。
 *
 * 为什么第一次用时先把它们拷到 `noBackupFilesDir/embed/<modelId>/`：ONNX Runtime 加载**外部权重**要一个真实的
 * 文件路径（assets 里的东西是压缩包里的条目，没有路径）。拷贝**不在冷启动路径上**：`MainActivity` 只建这个对象
 * （`assets.list()` 一次，判断"带没带模型"），真正的拷贝在第一次 [embed] 的后台线程里做，拷完校验大小。
 *
 * 会话：`intraOpNumThreads = 4`、`OptLevel.ALL_OPT`；
 * **懒加载**，第一次算的时候才建（载入后约 300 MB），退到后台 / 释放时 [close] 掉，下次再懒建。
 *
 * 输入输出照模型：`input_ids` / `attention_mask`（int64 `[b, len]`），`image_features` / `video_features` /
 * `audio_features` 各喂**空的 `[0, 512]` float**（这个模型是多模态的，文字输入也必须有这三个口）；
 * 输出取 `sentence_embedding`（`[b, 768]`，模型自己已经做过 L2 归一化，这里仍然再归一化一次兜底）。
 */
class OnnxTextEmbedder(
    context: Context,
    private val assetDir: String = "embed",
    private val intraOpThreads: Int = 4,
    private val log: (String) -> Unit = { Log.i(TAG, it) },
) : Embedder {
    private val appContext = context.applicationContext

    override val modelId: String = EMBEDDING_MODEL_ID

    /** 打包时带没带模型（`assets/embed/` 里那三个文件在不在）。懒查一次，不在冷启动主线程扫 assets。 */
    val bundled: Boolean by lazy {
        runCatching {
            val names = appContext.assets.list(assetDir)?.toSet().orEmpty()
            MODEL_FILE in names && WEIGHTS_FILE in names && TOKENIZER_FILE in names
        }.getOrDefault(false)
    }

    @Volatile
    private var failed = false

    override val available: Boolean get() = bundled && !failed

    private val lock = ReentrantLock()
    private var env: OrtEnvironment? = null
    private var session: OrtSession? = null

    /** 词表：`@Volatile` + 单独一把小锁（[tokenLengths] 可能在 [embed] 正占着 [lock] 的时候进来）。 */
    @Volatile
    private var tokenizer: GemmaTokenizer? = null
    private val tokenizerLock = Any()
    private var closeRequested = false

    /** 模型就位了没有（拷过文件 / 建过会话）。设置页 / 日志用。 */
    @Volatile
    var prepared: Boolean = false
        private set

    override suspend fun embed(texts: List<String>): List<FloatArray>? {
        if (texts.isEmpty()) return emptyList()
        return withContext(Dispatchers.Default) {
            if (!available) return@withContext null
            lock.lock()
            try {
                val tok = tokenizerOrLoad() ?: error("词表读不出来")
                val session = session ?: openSession().also { session = it }
                runBatch(tok, session, texts)
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

    /**
     * 这批文本各几个 token。
     *
     * 用**和 NPU 同一个** `tokenizer.json`（同一个模型的两个量化版本，词表逐字节相同），所以数出来的长度
     * 就是那张图会截到的长度。词表加载与分词均在后台线程执行，绝不阻塞主线程。
     */
    override suspend fun tokenLengths(texts: List<String>): IntArray? = withContext(Dispatchers.Default) {
        if (!available) return@withContext null
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.tokenLengths(texts)
    }

    /** 按 token 截断（详情正文用，见 [DETAIL_MAX_TOKENS]）；不按字数估。 */
    override suspend fun truncateToTokens(text: String, maxTokens: Int): String? = withContext(Dispatchers.Default) {
        if (!available) return@withContext null
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.truncateToTokens(text, maxTokens)
    }

    /** 词表：读过就直接给；没读过就单独一把小锁里读一次（别和长时间占着的 [lock] 抢）。 */
    private fun tokenizerOrLoad(): GemmaTokenizer? {
        tokenizer?.let { return it }
        return synchronized(tokenizerLock) {
            tokenizer ?: runCatching { loadTokenizer() }
                .onFailure { log("[向量] 词表读不出来：${it.message ?: it::class.simpleName}") }
                .getOrNull()
                ?.also { tokenizer = it }
        }
    }

    /**
     * 释放会话（约 300 MB）。正在算的话**不打断**：标记一下，等这一批算完自己关（下次 [embed] 再懒建）。
     * 词表（约 10 MB）留着——它和会话无关，重新解析 32 MB 的 JSON 不值得。
     */
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
        runCatching { session?.close() }
        session = null
    }

    // ---------------------------------------------------------------- 模型文件

    /** 模型目录：`noBackupFilesDir/embed/<modelId>/`（换模型就换目录，老目录删掉）。 */
    private fun modelDir(): File = File(File(appContext.noBackupFilesDir, assetDir), modelId)

    private fun expectedSize(name: String): Long = runCatching {
        appContext.assets.open("$assetDir/$name").use { it.available().toLong() }
    }.getOrDefault(-1L)

    /** 把 assets 里那三个文件拷到 [modelDir]（已存在且大小对就跳过）；**每次换模型会删掉同级的旧目录**。 */
    private fun ensureCopied(): File {
        val dir = modelDir()
        val marker = File(dir, "COPY_OK")
        val expected = listOf(MODEL_FILE, WEIGHTS_FILE, TOKENIZER_FILE).associateWith { expectedSize(it) }
        val sizesOk = expected.entries.all { (name, size) ->
            val f = File(dir, name)
            f.isFile && (size <= 0 || f.length() == size)
        }
        if (marker.isFile && sizesOk) return dir

        dir.parentFile?.listFiles()?.forEach { sibling ->
            if (sibling.isDirectory && sibling.name != dir.name) {
                // 换模型（modelId 变了）：旧版本目录整个删掉，别白占几百 MB
                runCatching { sibling.deleteRecursively() }
            }
        }
        dir.deleteRecursively()
        dir.mkdirs()
        for ((name, size) in expected) {
            val target = File(dir, name)
            val tmp = File(dir, "$name.tmp")
            appContext.assets.open("$assetDir/$name").use { input ->
                FileOutputStream(tmp).use { out -> input.copyTo(out, 1 shl 16) }
            }
            check(tmp.length() > 0) { "$name 拷出来是空的" }
            if (size > 0) check(tmp.length() == size) { "$name 大小不对：拷了 ${tmp.length()}，assets 里是 $size" }
            check(tmp.renameTo(target)) { "$name 改名失败" }
            log("[向量] 已拷出 $name（${target.length() / 1024 / 1024} MB）")
        }
        marker.writeText("$modelId\n")
        prepared = true
        return dir
    }

    private fun loadTokenizer(): GemmaTokenizer {
        val started = System.currentTimeMillis()
        val bytes = appContext.assets.open("$assetDir/$TOKENIZER_FILE").use { it.readBytes() }
        val tok = GemmaTokenizer.fromJson(bytes)
        log("[向量] 词表读好了（${bytes.size / 1024 / 1024} MB，${System.currentTimeMillis() - started} ms）")
        return tok
    }

    private fun openSession(): OrtSession {
        val dir = ensureCopied()
        val started = System.currentTimeMillis()
        val e = OrtEnvironment.getEnvironment()
        // ORT 1.31 起安卓包带微软的 1DS 遥测（清单里的 TelemetryInitializer 已经删掉了，这里再关一道）
        runCatching { e.setTelemetry(false) }
        env = e
        val options = OrtSession.SessionOptions()
        options.setIntraOpNumThreads(intraOpThreads)
        options.setOptimizationLevel(OrtSession.SessionOptions.OptLevel.ALL_OPT)
        // ORT 的线程池默认算完还会自旋等下一个任务：我们一批 8 条、批间还让出 50 ms，自旋只是白烧电（用户要省电）
        options.addConfigEntry("session.intra_op.allow_spinning", "0")
        options.addConfigEntry("session.inter_op.allow_spinning", "0")
        val s =e.createSession(File(dir, MODEL_FILE).absolutePath, options)
        prepared = true
        log("[向量] 会话建好了（${System.currentTimeMillis() - started} ms，$intraOpThreads 线程）")
        return s
    }

    // ---------------------------------------------------------------- 推理

    private fun runBatch(tokenizer: GemmaTokenizer, session: OrtSession, texts: List<String>): List<FloatArray> {
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
        val env = checkNotNull(env)
        val shape = longArrayOf(batch.toLong(), maxLen.toLong())
        // 多模态的三个输入：文字输入时给空的 [0, 512]
        val emptyFeatures = longArrayOf(0, 512)
        OnnxTensor.createTensor(env, LongBuffer.wrap(ids), shape).use { idsTensor ->
            OnnxTensor.createTensor(env, LongBuffer.wrap(mask), shape).use { maskTensor ->
                OnnxTensor.createTensor(env, FloatBuffer.allocate(0), emptyFeatures).use { img ->
                    OnnxTensor.createTensor(env, FloatBuffer.allocate(0), emptyFeatures).use { video ->
                        OnnxTensor.createTensor(env, FloatBuffer.allocate(0), emptyFeatures).use { audio ->
                            val inputs = mapOf(
                                "input_ids" to idsTensor,
                                "attention_mask" to maskTensor,
                                "image_features" to img,
                                "video_features" to video,
                                "audio_features" to audio,
                            )
                            session.run(inputs).use { result ->
                                val out = result.get("sentence_embedding").orElse(null)
                                    ?: error("模型没有 sentence_embedding 输出")
                                val tensor = out as OnnxTensor
                                val buf = tensor.floatBuffer
                                val dim = tensor.info.shape.last().toInt()
                                val values = FloatArray(batch * dim)
                                buf.get(values)
                                return (0 until batch).map { b ->
                                    val v = values.copyOfRange(b * dim, (b + 1) * dim)
                                    l2NormalizeInPlace(v)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    companion object {
        private const val TAG = "feedhub-embed"

        /** assets 里的文件名（`androidApp/build.gradle.kts` 的 `stageEmbedModel` 用同一组名字）。 */
        const val MODEL_FILE = "model_q4_acc4.onnx"
        const val WEIGHTS_FILE = "model_q4.onnx_data"
        const val TOKENIZER_FILE = "tokenizer.json"
    }
}
