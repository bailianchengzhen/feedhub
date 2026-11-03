package feedhub.app.embed

import ai.onnxruntime.OnnxTensor
import ai.onnxruntime.OrtEnvironment
import ai.onnxruntime.OrtEpDevice
import ai.onnxruntime.OrtSession
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Build
import android.system.Os
import android.util.Log
import feedhub.app.library.npuCoverModelId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileOutputStream
import java.nio.FloatBuffer
import java.security.MessageDigest
import java.util.concurrent.locks.ReentrantLock

/**
 * 安卓上的**封面向量**：视觉图 + 纯文字那张图串起来算一条内容的向量。
 *
 * ```
 * 封面缩略图（384×384 拉伸）→ pixel_values [1,576,768] f32
 *        ▼  [视觉图] cover_vis_b1_8x8_qnn.onnx
 *   image_features [64,512]
 *        ▼  宿主：用**文本主干自己的词表**（[NpuEmbedTable]，从共用的 271 MB 权重里 mmap 读出来）查 id 的
 *           embedding，按「<bos> <|image> 64×<|image|> <image|> 正文 <eos> 0…」拼成 inputs_embeds [8,128,512] f32
 *        ▼  [文本图] model_w8a16_b8_l128_emb.onnx —— **和纯文字向量是同一张图、同一个会话**（[CoverTextRunner]）
 *   sentence_embedding [8,768] → MRL 256 → float16 存库（model = …-img8x8）
 * ```
 *
 * NPU 上最多四个会话；现在纯文字的图也吃 `inputs_embeds`（混合校准重新量化，纯文字和封面
 * 两种输入对 fp32 的余弦都和原来的专用图持平），封面的文本那一步直接借它，NPU 上只有**文本、视觉**两个会话。
 *
 * 会话怎么管：
 * 1. **两张图的 ctx 都编好了 → 常驻**：视觉会话建一次就留着，文本会话本来就是纯文字那个，窗口之间不关，
 *    退到后台 / 关开关时由 [close] 关。研究里"都从 ctx 加载"是能共存的组合。
 * 2. **还要现编（第一次 / ctx 坏了）→ 分时**：编图时**绝不能有别的 QNN context 在场**（研究里"另一张活着时
 *    现编"必挂，`QNN_COMMON_ERROR_MEM_ALLOC` / 6001），所以先让纯文字把会话放掉（[TextSessionGate]），
 *    视觉算完关掉再跑文本那一步（文本会话这时才建，要编就在这时编）；视觉 ctx 写出来之后下一个窗口就是常驻。
 * 3. **常驻撞了 QNN 错**（6001 之类，多半是两张图同时在场顶不住）→ **这个进程退回分时**、同一个窗口
 *    马上按分时重算一遍（不删 ctx：ctx 本身是好的）；分时也失败才是真坏：视觉图坏了删它的 ctx（文本图的
 *    ctx 归纯文字那条管，不在这里删）、返回 null，调度器把封面口径标成本次进程不可用；**纯文字向量不受影响**。
 * 什么时候调这个类由 [CoverVectorScheduler] 决定。
 *
 * 视觉图的 QNN 会话用 `offload_graph_io_quantization=0`：图输入 / 输出上的 Q/DQ 留在 QNN 图里
 * （`simple_op_builder.cc` 在 `=1` 时明确不接这两种节点），这是"整张图 1 个分区"的前提之一。
 */
/**
 * 视觉图在包里的位置（`assets/embed-cover/`，构建时 `stageCoverModel` 拷进来）。
 *
 * 为什么单独一个目录：文本图、271 MB 的共用权重、词表和词表矩阵的位置都在 `assets/embed-npu/`
 * （纯文字那条的），这里只放视觉图（自带权重，162 MB）。运行时解到 `noBackupFilesDir/embed-cover/<空间>/`。
 */
object CoverAssets {
    /** assets 里的目录。 */
    const val DIR: String = "embed-cover"

    /** 视觉编码器（`pixel_values [1,576,768]` → `image_features [64,512]`）。 */
    const val VISION_FILE: String = "cover_vis_b1_8x8_qnn.onnx"

    /** 图结构的指纹（sha256 前 8 位）：ctx 缓存名带着它，换图 = 换名字（和纯文字那条同一个做法）。 */
    fun tag(sha256Hex: String): String = sha256Hex.take(8)

    /** context binary 的缓存名：`ctx-cover-vis-<sha8>.onnx`。 */
    fun ctxFileName(kind: String, tag: String): String = "ctx-cover-$kind-$tag.onnx"
}

/**
 * 文本那一步的**布局**（视觉图的 64 个 soft token 放哪；文本图本身不知道这件事，只吃 `inputs_embeds`）。
 *
 * ```
 * 位置  0      : <bos>
 *       1      : <|image>              （255999）
 *       2..65  : 64 个 <|image|>       （视觉特征原样填进去，不查表）
 *       66     : <image|>              （258882）
 *       67..   : 正文（最多 60 个 token）
 *       …      : <eos>
 *       余下   : 0（填充，embedding 取词表第 0 行；attention_mask = 0）
 * ```
 *
 * 和图里 `build_text_vision.py` 的"图在最前面 + 静态切片"一致；研究里对拍过"图在前 / 图在后"的余弦
 * 0.9779（最小 0.9404）、成对 AUC 只差 0.002，所以为了 HTP 把图放最前面是量过代价的。
 */
object CoverLayout {
    /** 文本图里的批（烘死的）。 */
    const val BATCH: Int = NPU_BATCH

    /** 文本图的序列长度（烘死的，借的那张图必须是这个长度）：正文只剩 60 个 token。 */
    const val TEXT_LEN: Int = 128

    /** 图占多少个 soft token（8×8 方阵）。 */
    const val IMAGE_TOKENS: Int = 64

    /** 正文从第几个位置开始（2 + 64 + 1）。 */
    const val BODY_START: Int = 2 + IMAGE_TOKENS + 1

    /** 正文最多几个 token（含结尾的 `<eos>`）：128 − 67 − 1 = 60。 */
    const val BODY_TOKENS: Int = TEXT_LEN - BODY_START - 1

    /** 视觉图输入边长 / patch 边长（384 = 8 × 48，16 像素一个 patch）。 */
    const val SIDE: Int = COVER_INPUT_SIDE
    const val PATCH: Int = COVER_PATCH

    /** 视觉特征的维数（每张图 64×512）。 */
    const val FEATURE_DIM: Int = 512

    /** 图里的特殊 token（`tokenizer.json` 的 `added_tokens`；都远小于词表 262144）。 */
    const val TOKEN_IMAGE_OPEN: Int = 255999   // <|image>
    const val TOKEN_IMAGE: Int = 258880        // <|image|>
    const val TOKEN_IMAGE_CLOSE: Int = 258882  // <image|>
}

class NpuOnnxCoverEmbedder(
    context: Context,
    /** 写进 `item_embedding.model` 的空间标识（`…-htp-l128-img8x8`）。 */
    private val spaceModelId: String = npuCoverModelId(CoverLayout.TEXT_LEN),
    /** 分时用：算封面前把纯文字会话放掉（null = 这个组装里没有纯文字那条）。 */
    private val textSessions: TextSessionGate? = null,
    /** 文本那一步借的图（纯文字那张 128 token 的；null = 包里没有，封面这条算没带图）。 */
    private val textRunner: CoverTextRunner? = null,
    private val log: (String) -> Unit = { Log.i(TAG, it) },
) : CoverEmbedder {
    private val appContext = context.applicationContext

    override val modelId: String = spaceModelId

    /** 包里带没带视觉图 + 能借的文本图（只看 assets 的名字，不读内容）。 */
    override val bundled: Boolean = textRunner?.seqLen == CoverLayout.TEXT_LEN && runCatching {
        val names = appContext.assets.list(NpuAssets.DIR)?.toSet().orEmpty()
        val cover = appContext.assets.list(CoverAssets.DIR)?.toSet().orEmpty()
        CoverAssets.VISION_FILE in cover && NpuAssets.fileFor(CoverLayout.TEXT_LEN) in names &&
            NpuAssets.TABLE_FILE in names && NpuAssets.WEIGHTS_FILE in names && TOKENIZER_FILE in names
    }.getOrDefault(false)

    @Volatile
    override var detail: String = "HTP · 封面 8×8 + 文本 128 token"
        private set

    private val lock = ReentrantLock()
    private var vision: OrtSession? = null

    /** 常驻撞过 QNN 错：这个进程里退回分时（每个窗口现建、用完就关）。 */
    @Volatile
    private var residentBroken = false

    /** [close] 被叫时正好有窗口在算：这个窗口算完再关（不在调用方线程上等锁）。 */
    @Volatile
    private var closeAfterWindow = false

    /** 视觉图的指纹（sha256 前 8 位）：ctx 缓存名带着它，换图就换名字（不会加载到上一版的 ctx）。 */
    @Volatile
    private var tags: Map<String, String> = emptyMap()

    @Volatile
    override var lastWindow: CoverWindowStats? = null
        private set

    private val registration = OnceRegistration {
        ortEnv().registerExecutionProviderLibrary(QNN_EP_NAME, qnnPlugin().absolutePath)
    }

    // ------------------------------------------------------------------ 探头 / 准备

    override suspend fun probe(): String? = withContext(Dispatchers.IO) {
        if (!bundled) return@withContext "这个包没带封面的视觉图"
        // 和纯文字那条同一个坑：QNN 库只有 arm64-v8a，要看**主 ABI**（x86_64 模拟器的 SUPPORTED_ABIS 里也有 arm64）
        val primaryAbi = Build.SUPPORTED_ABIS.firstOrNull().orEmpty()
        if (primaryAbi != "arm64-v8a") return@withContext "这台设备没有 NPU"
        if (!qnnPlugin().isFile) return@withContext "这个包里没有 QNN 插件，不是 NPU 构建"
        prepareAdspPath()
        val env = ortEnv()
        registration.ensure()?.let { return@withContext "QNN 插件注册失败：$it" }
        val devices = try {
            env.getEpDevices()
        } catch (e: Throwable) {
            return@withContext "读不到 EP 设备：${e.message ?: e::class.simpleName}"
        }
        // 和纯文字那条同一个判定（`NpuOnnxTextEmbedder` 里那个 `isQnn()` 是类内 private 扩展，这里照抄一行）
        if (devices.none { it.epName.contains("QNN", ignoreCase = true) }) {
            return@withContext "注册了 QNN 插件，但没有 QNN 设备"
        }
        null
    }

    override suspend fun prepare(): String? = withContext(Dispatchers.IO) {
        lock.lock()
        try {
            if (!bundled) return@withContext "这个包没带封面的视觉图"
            val runner = textRunner ?: return@withContext "没有能借的文本图"
            ensureCopied() ?: return@withContext "视觉图没解包"
            runner.tokenizerOrNull() ?: return@withContext "词表读不出来"
            runner.embedTableOrNull() ?: return@withContext "词表矩阵打不开（${NpuAssets.TABLE_FILE}）"
            // QNN EP 每个进程只能注册一次；纯文字那条可能已经注册过，这里 ensure() 会认"已注册"
            registration.ensure()?.let { return@withContext "QNN 插件注册失败：$it" }
            if (qnnDevice() == null) return@withContext "没有 QNN 设备（QNN 插件没注册 / 这台机器没有 NPU）"
            null
        } catch (e: Throwable) {
            "封面准备失败：${e.message ?: e::class.simpleName}"
        } finally {
            lock.unlock()
        }
    }

    // ------------------------------------------------------------------ 推理

    override suspend fun embed(items: List<CoverInput>): List<FloatArray?>? {
        if (items.isEmpty()) return emptyList()
        return withContext(Dispatchers.Default) {
            lock.lock()
            try {
                val runner = textRunner ?: return@withContext null
                val dir = ensureCopied() ?: run {
                    log("[封面] 视觉图没解包，跳过")
                    return@withContext null
                }
                val tok = runner.tokenizerOrNull() ?: run {
                    log("[封面] 词表读不出来")
                    return@withContext null
                }
                val tbl = runner.embedTableOrNull() ?: run {
                    log("[封面] 词表矩阵打不开")
                    return@withContext null
                }
                val resident = !residentBroken && ctxReady(dir) && runner.ctxReady()
                if (resident) {
                    try {
                        return@withContext runWindow(items, dir, runner, tok, tbl, resident = true)
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        // 常驻顶不住（几张 QNN 图同时在场）：这个进程退回分时，同一个窗口马上按分时再算一遍。
                        // ctx 本身是好的，不删。
                        log("[封面] 常驻会话失败，这个进程改回分时：${e::class.simpleName}: ${e.message}")
                        residentBroken = true
                        closeSessions()
                    }
                }
                try {
                    runWindow(items, dir, runner, tok, tbl, resident = false)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    log("[封面] 这一轮失败：${e::class.simpleName}: ${e.message}")
                    // QNN 的 6001 / MEM_ALLOC 都走这里：视觉图坏了删它的 ctx；文本图是纯文字那条的，
                    // 它自己加载失败会删、这里不动。调度器把封面标成本次进程不可用
                    if (e is CoverInferenceFailure && e.kind == VISION_KIND) dropVisionCtx()
                    closeSessions()
                    null
                }
            } finally {
                if (closeAfterWindow) {
                    closeAfterWindow = false
                    closeSessions()
                }
                lock.unlock()
            }
        }
    }

    /**
     * 算一个窗口（调用方拿着 [lock]）。[resident] = 视觉会话常驻（建过就复用、算完不关）；
     * 否则分时：先让纯文字放掉会话，视觉算完关掉再跑文本那一步（文本会话留着，纯文字接着用）。
     * 推理失败抛出去（[CoverInferenceFailure] 之类），由 [embed] 决定退回分时还是整条停。
     */
    private fun runWindow(
        items: List<CoverInput>,
        dir: File,
        runner: CoverTextRunner,
        tok: GemmaTokenizer,
        tbl: NpuEmbedTable,
        resident: Boolean,
    ): List<FloatArray?>? {
        val started = System.currentTimeMillis()
        var compiled = 0
        // 分时第 0 步：纯文字那条先把会话放掉（编图时绝不能有别的 QNN context 在场）；
        // 上一个窗口借用时建的、路由还没认领的那个也放掉（路由的 releaseSessions 管不到它）
        if (!resident) {
            runCatching { textSessions?.releaseSessions() }
            runCatching { runner.releaseBorrowed() }
        }

        // 1) 视觉：一张一张算（视觉图 batch 固定成 1）
        val t0 = System.currentTimeMillis()
        val vis = vision ?: openSession(VISION_KIND, dir, CoverAssets.VISION_FILE)?.let { (session, fresh) ->
            if (fresh) compiled++
            session
        } ?: return null
        vision = vis
        val feats = ArrayList<FloatArray?>(items.size)
        for (item in items) {
            feats += runVision(vis, item.image)
        }
        val visionMs = System.currentTimeMillis() - t0
        if (!resident) {
            // 分时：关掉视觉会话（释放 160 MB + HTP context），再跑文本那一步
            runCatching { vis.close() }
            vision = null
        }

        // 2) 文本：一批 8 条，借纯文字那张图（没会话它自己从 ctx 建 / 现编）
        val t1 = System.currentTimeMillis()
        val textCompiles = !runner.ctxReady()
        val usable = items.indices.filter { feats[it] != null }
        val out = arrayOfNulls<FloatArray>(items.size)
        var textBatches = 0
        for (chunk in usable.chunked(CoverLayout.BATCH)) {
            val embeds = buildInputsEmbeds(tok, tbl, chunk.map { items[it].text }, chunk.map { feats[it]!! })
            val vecs = runText(runner, embeds.first, embeds.second)
            textBatches++
            for ((k, idx) in chunk.withIndex()) out[idx] = vecs[k]
        }
        val textMs = System.currentTimeMillis() - t1
        if (textCompiles && textBatches > 0) compiled++
        if (usable.isEmpty()) return ArrayList<FloatArray?>(items.size)
        lastWindow = CoverWindowStats(
            count = usable.size,
            visionImages = items.size,
            visionMs = visionMs,
            textBatches = textBatches,
            textMs = textMs,
            totalMs = System.currentTimeMillis() - started,
            compiled = compiled,
        )
        detail = "HTP · 封面 8×8 + 文本 128 token（和纯文字共用）· ${if (resident) "常驻" else "分时"}" +
            lastWindow?.let { " · 上一轮 ${it.msPerItem} ms/条" }.orEmpty()
        return out.toList()
    }

    /** 视觉图的 ctx 已经编好了吗（编好了才能常驻：现编时不能有别的 QNN context 在场）。 */
    private fun ctxReady(dir: File): Boolean {
        val tag = tags[CoverAssets.VISION_FILE] ?: return false
        val ctx = File(dir, CoverAssets.ctxFileName(VISION_KIND, tag))
        return ctx.isFile && ctx.length() >= MIN_CTX_BYTES
    }

    /** 关掉视觉会话 + 文本会话要是这边借用时建的也关掉（调用方拿着 [lock]）。 */
    private fun closeSessions() {
        runCatching { vision?.close() }
        vision = null
        runCatching { textRunner?.releaseBorrowed() }
    }

    /** 建一个会话：有 ctx 就先从 ctx 加载（快），失败 / 没有就现编一次并把 ctx 写出来。 */
    private fun openSession(kind: String, dir: File, assetFile: String): Pair<OrtSession, Boolean>? {
        val env = ortEnv()
        val device = qnnDevice() ?: run {
            log("[封面] 没有 QNN 设备")
            return null
        }
        val tag = tags[assetFile] ?: run {
            log("[封面] 读不出 $assetFile 的指纹")
            return null
        }
        val ctx = File(dir, CoverAssets.ctxFileName(kind, tag))
        val started = System.currentTimeMillis()
        val options = OrtSession.SessionOptions()
        options.setOptimizationLevel(OrtSession.SessionOptions.OptLevel.ALL_OPT)
        options.setIntraOpNumThreads(2)
        options.addExecutionProvider(listOf(device), QNN_OPTIONS)
        if (ctx.isFile && ctx.length() >= MIN_CTX_BYTES) {
            try {
                val s = env.createSession(ctx.absolutePath, options)
                log("[封面] ${kind}：从 ctx 加载 ${System.currentTimeMillis() - started} ms（${ctx.length() / 1024 / 1024} MB）")
                return s to false
            } catch (e: Throwable) {
                log("[封面] ${kind}：ctx 加载失败，删掉重编（${e.message ?: e::class.simpleName}）")
                runCatching { ctx.delete() }
            }
        }
        return try {
            val gen = OrtSession.SessionOptions()
            gen.setOptimizationLevel(OrtSession.SessionOptions.OptLevel.ALL_OPT)
            gen.setIntraOpNumThreads(2)
            gen.addExecutionProvider(listOf(device), QNN_OPTIONS)
            gen.addConfigEntry("ep.context_enable", "1")
            gen.addConfigEntry("ep.context_file_path", ctx.absolutePath)
            gen.addConfigEntry("ep.context_embed_mode", "1")
            val s = env.createSession(File(dir, assetFile).absolutePath, gen)
            log(
                "[封面] ${kind}：现编图 ${System.currentTimeMillis() - started} ms" +
                    "（ctx ${if (ctx.isFile) ctx.length() / 1024 / 1024 else 0} MB）",
            )
            s to true
        } catch (e: Throwable) {
            log("[封面] ${kind}：建会话失败（${e.message ?: e::class.simpleName}）")
            runCatching { ctx.delete() }
            null
        }
    }

    /**
     * 一张封面 → 64×512 的视觉特征（解码 / 缩放 / patchify 都在这里）。
     *
     * **两种失败要分开**：
     * - **这张图解不开**（BitmapFactory / 缩放失败）→ 返回 null，调用方只跳过这一条；
     * - **推理本身抛了**（QNN 6001 / MEM_ALLOC / 形状不对）→ 抛 [CoverInferenceFailure]，整个窗口失败。
     *   以前这里一律吞成 null，结果是"QNN 坏了还接着算下一条"，每条都再失败一次，而且窗口不会标成
     *   本次进程不可用（还会一直重试）。
     */
    private fun runVision(session: OrtSession, bytes: ByteArray): FloatArray? {
        val pixels = pixelsOf(bytes) ?: run {
            log("[封面] 这张图解码不了（${bytes.size} B），跳过")
            return null
        }
        val env = ortEnv()
        val shape = longArrayOf(1, (CoverLayout.SIDE / CoverLayout.PATCH).toLong().let { it * it }, 768)
        try {
            OnnxTensor.createTensor(env, FloatBuffer.wrap(pixels), shape).use { input ->
                session.run(mapOf("pixel_values" to input)).use { result ->
                    val out = result.get(0) as OnnxTensor
                    val dim = out.info.shape.last().toInt()
                    val total = out.info.shape.fold(1L) { a, d -> a * d }.toInt()
                    val values = FloatArray(total)
                    out.floatBuffer.get(values)
                    if (dim != CoverLayout.FEATURE_DIM) {
                        throw CoverInferenceFailure(VISION_KIND, "视觉特征维数不对：$dim")
                    }
                    return values
                }
            }
        } catch (e: CoverInferenceFailure) {
            throw e
        } catch (e: Throwable) {
            throw CoverInferenceFailure(VISION_KIND, "${e::class.simpleName}: ${e.message}", e)
        }
    }

    /** 一次推理 / 建会话真的失败了（不是"这张图解不开"）：整个窗口停 + 删这张图的 ctx。 */
    private class CoverInferenceFailure(
        val kind: String,
        message: String,
        cause: Throwable? = null,
    ) : RuntimeException(message, cause)

    /** 一批（≤8）的 `inputs_embeds [8,128,512]` + `attention_mask [8,128]`。 */
    private fun buildInputsEmbeds(
        tok: GemmaTokenizer,
        tbl: NpuEmbedTable,
        texts: List<String>,
        feats: List<FloatArray>,
    ): Pair<FloatArray, LongArray> {
        val len = CoverLayout.TEXT_LEN
        val batch = CoverLayout.BATCH
        val dim = CoverLayout.FEATURE_DIM
        val embeds = FloatArray(batch * len * dim)
        val mask = LongArray(batch * len)
        val emptyIds = tok.encode("", 0)
        val bos = emptyIds.firstOrNull() ?: 2
        val eos = emptyIds.lastOrNull() ?: 1
        // 整批一次过边界：正文：<bos> + body + <eos>，按「图占 65 个位置」截到 60 个
        val encoded = tok.encodeBatch(texts, CoverLayout.BODY_TOKENS + 2)
        for (row in texts.indices) {
            val ids = encoded[row]
            val body = if (ids.size >= 2) ids.copyOfRange(1, ids.size - 1) else IntArray(0)
            val n = minOf(body.size, CoverLayout.BODY_TOKENS)
            val base = row * len * dim
            // 0: <bos>，1: <|image>（两个都从词表里查）
            tbl.writeRow(bos, embeds, base + 0 * dim)
            tbl.writeRow(CoverLayout.TOKEN_IMAGE_OPEN, embeds, base + 1 * dim)
            // 2..65: 64 个图 token（视觉特征原样，不查表）
            val feat = feats[row]
            feat.copyInto(embeds, base + 2 * dim, 0, minOf(feat.size, CoverLayout.IMAGE_TOKENS * dim))
            // 66: <image|>
            tbl.writeRow(CoverLayout.TOKEN_IMAGE_CLOSE, embeds, base + 66 * dim)
            // 67..: 正文，最后一个位置是 <eos>
            var at = CoverLayout.BODY_START
            for (i in 0 until n) {
                tbl.writeRow(body[i], embeds, base + (at++) * dim)
            }
            tbl.writeRow(eos, embeds, base + (at++) * dim)
            // 填充位置：id 0 的 embedding（和研究里的对拍口径完全一样：emb 是整条 ids 查出来的）
            while (at < len) {
                tbl.writeRow(0, embeds, base + (at++) * dim)
            }
            // attention_mask：真的 token 是 1（图 token 也是 1），填充 0
            for (i in 0 until (CoverLayout.BODY_START + n + 1)) mask[row * len + i] = 1L
        }
        return embeds to mask
    }

    private fun runText(runner: CoverTextRunner, embeds: FloatArray, mask: LongArray): List<FloatArray> =
        try {
            runner.runEmbeds(embeds, mask)
        } catch (e: Throwable) {
            throw CoverInferenceFailure(TEXT_KIND, "${e::class.simpleName}: ${e.message}", e)
        }

    /** 视觉图推理 / 编图真的失败了：把它的 ctx 删掉（坏缓存不能留着"下次加载成功却跑坏图"）。 */
    private fun dropVisionCtx() {
        runCatching {
            val dir = modelDir() ?: return@runCatching
            val tag = tags[CoverAssets.VISION_FILE] ?: return@runCatching
            val f = File(dir, CoverAssets.ctxFileName(VISION_KIND, tag))
            if (f.delete()) log("[封面] 删掉了坏掉的 ${f.name}")
        }
    }

    /** 关掉常驻的视觉会话（退到后台 / 关开关 / 退出）。正有窗口在算就让它算完自己关，不在这里等锁。 */
    override fun close() {
        if (lock.tryLock()) {
            try {
                closeSessions()
            } finally {
                lock.unlock()
            }
        } else {
            closeAfterWindow = true
        }
    }

    // ------------------------------------------------------------------ 图像预处理

    /**
     * 封面字节 → `pixel_values [1,576,768]`（**拉伸**到 384×384，patchify 行优先，像素 /255 不归一化）。
     *
     * 和 `tools/embed-vision/egv_common.py` 的口径一致（8×8 方阵这条线不保持长宽比，研究里量过：
     * 和等比缩放的向量余弦 0.9925、成对 AUC 差 0.002）；缩放用 `createScaledBitmap` 的双线性
     */
    private fun pixelsOf(bytes: ByteArray): FloatArray? {
        val bmp = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return null
        val side = COVER_INPUT_SIDE
        val scaled = if (bmp.width == side && bmp.height == side) {
            bmp
        } else {
            Bitmap.createScaledBitmap(bmp, side, side, true)
        }
        val px = IntArray(side * side)
        scaled.getPixels(px, 0, side, 0, 0, side, side)
        if (scaled !== bmp) scaled.recycle()
        bmp.recycle()
        // patchify 在 commonMain（`coverPatchify`）：纯整数 / 浮点运算，桌面单测能拿它和 Python 参考脚本逐位对拍
        return coverPatchify(px, side)
    }

    // ------------------------------------------------------------------ 词表 / 文件

    /**
     * 把 assets 里的视觉图拷到私有目录（`noBackupFilesDir/embed-cover/<空间>/`）。视觉图自带权重，
     * 不引用共用的那份；文本那一步用的图、权重、词表都在纯文字那条的目录里，这里不碰。
     *
     * ctx 缓存名带**图指纹**（[CoverAssets.ctxFileName]）：换图 = 换名字，不会"加载成功却跑旧图"。
     * **视觉图这一版的 ctx 留着**（重编要几十秒）。
     */
    private fun ensureCopied(): File? {
        val dir = modelDir() ?: return null
        val wanted = listOf(CoverAssets.VISION_FILE)
        val sizes = wanted.associateWith { assetSize(it) }
        // 指纹只算一次：162 MB 的图每个窗口都 sha256 一遍是白烧 IO（assets 在一个进程里不会变）
        if (tags.isEmpty()) {
            tags = wanted.associateWith { CoverAssets.tag(assetSha256(it)) }
        }
        val myTags = tags
        val keepCtx = CoverAssets.ctxFileName(VISION_KIND, myTags.getValue(CoverAssets.VISION_FILE))
        val marker = File(dir, "COPY_OK")
        val markerText = buildString {
            append(spaceModelId).append('\n')
            myTags.toSortedMap().forEach { (n, t) -> append(NpuAssets.stagedMarker(n, t)).append('\n') }
        }
        synchronized(COPY_LOCK) {
            val complete = marker.isFile && marker.readText() == markerText && sizes.all { (name, size) ->
                val f = File(dir, name)
                f.isFile && (size <= 0 || f.length() == size)
            }
            if (complete) return dir
            dir.mkdirs()
            dir.listFiles()?.forEach { f ->
                // 软链（老版本的共用权重）也走这里：delete 删的是链本身，不碰纯文字那份
                if (f.name != "COPY_OK" && f.name !in sizes.keys && f.name != keepCtx) runCatching { f.delete() }
            }
            for ((name, expected) in sizes) {
                val target = File(dir, name)
                if (target.isFile && (expected <= 0 || target.length() == expected)) continue
                val tmp = File(dir, "$name.tmp")
                runCatching { tmp.delete() }
                runCatching {
                    appContext.assets.open("${CoverAssets.DIR}/$name").use { input ->
                        FileOutputStream(tmp).use { out -> input.copyTo(out, 1 shl 16) }
                    }
                }.onFailure {
                    log("[封面] 拷 $name 失败：${it.message}")
                    return null
                }
                check(tmp.length() > 0) { "$name 拷出来是空的" }
                check(tmp.renameTo(target)) { "$name 改名失败" }
                log("[封面] 已拷出 $name（${target.length() / 1024 / 1024} MB，指纹 ${myTags[name] ?: "-"}）")
            }
            marker.writeText(markerText)
            return dir
        }
    }

    /** 私有目录：`noBackupFilesDir/embed-cover/<空间标识>/`（和纯文字那条的 `embed-npu/<空间>` 分开）。 */
    private fun modelDir(): File? = runCatching {
        val root = File(appContext.noBackupFilesDir, CoverAssets.DIR)
        File(root, spaceModelId).also { it.mkdirs() }
    }.getOrNull()

    private fun assetSize(name: String): Long = runCatching {
        appContext.assets.open("${CoverAssets.DIR}/$name").use { it.available().toLong() }
    }.getOrDefault(-1L)

    private fun assetSha256(name: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
        appContext.assets.open("${CoverAssets.DIR}/$name").use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n <= 0) break
                digest.update(buf, 0, n)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    // ------------------------------------------------------------------ 环境

    private fun ortEnv(): OrtEnvironment = OrtEnvironment.getEnvironment().also { env ->
        runCatching { env.setTelemetry(false) }
    }

    private fun qnnPlugin(): File = File(appContext.applicationInfo.nativeLibraryDir, QNN_PLUGIN)

    private fun qnnDevice(): OrtEpDevice? = runCatching {
        ortEnv().getEpDevices().firstOrNull { it.epName.contains("QNN", ignoreCase = true) }
    }.getOrNull()

    private fun prepareAdspPath() {
        val libDir = File(appContext.applicationInfo.nativeLibraryDir)
        val value = libDir.absolutePath + ";/vendor/dsp/cdsp;/vendor/lib/rfsa/adsp;/system/lib/rfsa/adsp;/dsp"
        runCatching { Os.setenv("ADSP_LIBRARY_PATH", value, true) }
            .onFailure { log("[封面] ADSP_LIBRARY_PATH 没设上：${it.message}") }
    }

    private companion object {
        const val TAG = "feedhub-embed"
        const val TOKENIZER_FILE = "tokenizer.json"
        const val QNN_EP_NAME = "qnn"
        const val QNN_PLUGIN = NpuOnnxTextEmbedder.QNN_PLUGIN
        const val VISION_KIND = "vis"
        const val TEXT_KIND = "tv"

        /** 小于这个就当缓存坏了（正常编出来是几百 MB）。 */
        const val MIN_CTX_BYTES = 20L * 1024 * 1024

        val COPY_LOCK = Any()

        /**
         * 视觉图的 QNN 选项：`offload_graph_io_quantization=0`。
         *
         * 为什么：图的输入 / 输出上各有一对 Q/DQ
         * （`pixel_values` 是 f32 图输入 → Q；`image_features` 是 DQ → 图输出），
         * `=1` 时 QNN 明确**不接**"输入接图输入的 Q""输出接图输出的 DQ"（`simple_op_builder.cc`），
         * 于是这两张图永远多出 2 个 CPU 节点、至少 3 个分区。改成 0 之后 Q/DQ 留在 QNN 图里，
         * 整张图才能是 1 个分区（`tools/embed/qnn_capability.py --offload-io 0` 报的就是这个口径）。
         */
        val QNN_OPTIONS: Map<String, String> = mapOf(
            "backend_type" to "htp",
            "offload_graph_io_quantization" to "0",
        )
    }
}
