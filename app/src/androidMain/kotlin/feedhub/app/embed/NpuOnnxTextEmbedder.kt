package feedhub.app.embed

import ai.onnxruntime.OnnxTensor
import ai.onnxruntime.OrtEnvironment
import ai.onnxruntime.OrtEpDevice
import ai.onnxruntime.OrtSession
import android.content.Context
import android.os.Build
import android.system.Os
import android.util.Log
import feedhub.app.library.npuModelId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileOutputStream
import java.nio.FloatBuffer
import java.nio.LongBuffer
import java.security.MessageDigest
import java.util.concurrent.locks.ReentrantLock

/**
 * 安卓上的 NPU 文本向量。
 *
 * 和 CPU 那份（[OnnxTextEmbedder]）是**两条独立的路**：
 * - 模型是 npu-research 里数值验过的 **w8a16 + int8 词表**、固定形状 `[8,L]`、只留文字路径的图；
 * - **几张图共用一份 271 MB 的权重**（ONNX external data，见 [NpuAssets.WEIGHTS_FILE]）：每张图只有 1.5 MB 的
 *   图结构，装到 App 私有目录时也放在**同一个目录**里（external data 的 `location` 是相对 ONNX 文件的路径）；
 * - 同一个包里所有长度的图**共用同一个 `modelId`**（= 最长那张，空间标识）；短图和长图对"装得下的文本"
 *   算出来是同一个向量，所以换图不换向量空间；
 * 从共用权重里按需读行），
 *   于是封面向量的"文本+图"那一步也跑这张图、这个会话（[CoverTextRunner]）——NPU 上只剩文本、视觉两个会话。
 *   新图和老的 L32 / L128 / 封面文本图算出来的向量余弦 ≥ 0.9987（中位 0.9999，比 w8a16 本身的误差小一个量级），
 *   所以 `item_embedding.model` 不变，库里已有的向量照用。
 *
 * 三个真机上踩过的坑：
 * 1. **QNN EP 每个进程只能注册一次**（再注册报 "EP library already registered"）→ [registration] 是进程级单例
 *    （[OnceRegistration]）；
 * 2. Android 12+ 要在 manifest 里声明 `<uses-native-library android:name="libcdsprpc.so"/>`（app 的
 *    `AndroidManifest.xml` 里有），否则 QNN 报 INVALID_CONFIG；
 * 3. **`ADSP_LIBRARY_PATH` 要指向 App 的 native lib 目录**（HTP 的 skel 在那里），否则 DSP 端找不到 skel。
 *
 * 第一次要**编图**（HTP prepare + 编译成 context binary）：真机上十几秒到一分钟、峰值内存约 1.9 GB。
 * 编好的 context binary 缓存在 App 私有目录（`ctx-htp-l<L>-<图指纹>.onnx`：**长度 + 图结构的 sha256 前 8 位**，
 * 换写法换长度都会换名字，见 [NpuAssets.ctxFileName]），之后
 * `createSession(context 文件)` 直接加载；编图只在**前台**、并且（充电 或 空闲）时开始，由 [RoutingEmbedder] 管。
 * 失败时由路由决定：普通包回退 CPU；**只带 NPU 模型的包不回退**（本地向量显示"不可用"，不算、不存）。
 */
class NpuOnnxTextEmbedder(
    context: Context,
    /** 包里的一张图（长度 + assets 里的文件名）。 */
    private val variant: NpuModelVariant,
    /** 这个包里带的**全部**长度：它们共用一份权重、一个私有目录（只拷一次，别每张图各拷 271 MB）。 */
    private val variants: List<NpuModelVariant> = listOf(variant),
    /** 写进 `item_embedding.model` 的空间标识（= 最长那张，[NpuAssets.spaceLen]）。 */
    private val spaceModelId: String = npuModelId(variant.seqLen),
    /** NPU 模型在 assets 里的目录（`androidApp` 的 `stageNpuModel` 放的那份）。 */
    private val assetDir: String = NpuAssets.DIR,
    /**
     * 词表在 assets 里的目录（和 NPU 的图放一起：`assets/embed-npu/tokenizer.json`）。
     *
     * `assets/embed/` 是空的，
     * 所以词表跟着 NPU 的资源走（构建时 `stageNpuModel` 从 `-Pfeedhub.embedModelDir` 拿同一个
     * `tokenizer.json`，两个量化版本的词表逐字节相同）。
     */
    private val tokenizerAssetDir: String = NpuAssets.DIR,
    private val log: (String) -> Unit = { Log.i(TAG, it) },
) : NpuEngine, CoverTextRunner {
    private val appContext = context.applicationContext

    override val seqLen: Int = variant.seqLen

    override val modelId: String = spaceModelId

    /** 包里带没带这张图（图 + 共用权重 + 词表都在 `assets/embed-npu/` 里）。懒查一次，避免阻塞冷启动主线程。 */
    override val bundled: Boolean by lazy {
        runCatching {
            val names = appContext.assets.list(assetDir)?.toSet().orEmpty()
            variant.assetFile in names && NpuAssets.WEIGHTS_FILE in names && TOKENIZER_FILE in names &&
                NpuAssets.TABLE_FILE in names
        }.getOrDefault(false)
    }

    @Volatile
    override var detail: String = "HTP · w8a16 · 每条 $seqLen token"
        private set

    /**
     * 这个包里每张图的指纹（`model_w8a16_b8_l<L>_emb.onnx` 的 sha256 前 8 位），[ensureCopied] 算好放这里。
     *
     * 它决定 context binary 的缓存名（[NpuAssets.ctxFileName]：`ctx-htp-l32-ddca13e2.onnx`）。
     * 只按长度命名的缓存会把上一版的 context binary
     * 当成这一版的加载——轻则建会话失败，重则"加载成功"但其实跑的是旧图。
     */
    @Volatile
    private var graphTags: Map<String, String> = emptyMap()

    private val lock = ReentrantLock()
    private var session: OrtSession? = null

    /** 会话是封面那边借用时建的（路由的 [prepare] 还没认领）：封面关的时候顺手关掉。 */
    @Volatile
    private var borrowed = false

    /** 词表矩阵（[prepareLocked] 里打开：要先把共用权重解到私有目录）。 */
    @Volatile
    private var table: NpuEmbedTable? = null

    /** 分词器（和 CPU 那份是同一份 `tokenizer.json`）。运行在多个线程上，单独一把小锁。 */
    @Volatile
    private var tokenizer: GemmaTokenizer? = null
    private val tokenizerLock = Any()

    /** QNN EP 的进程级一次性注册（"每个进程只能注册一次"那条限制）。 */
    private val registration = OnceRegistration {
        val plugin = qnnPlugin()
        ortEnv().registerExecutionProviderLibrary(QNN_EP_NAME, plugin.absolutePath)
    }

    // ------------------------------------------------------------------ 探头 / 准备

    override suspend fun probe(): String? = withContext(Dispatchers.IO) {
        if (!bundled) return@withContext "这个包没带 $seqLen token 的 NPU 模型"
        // QNN 库只有 arm64-v8a，而且要看**主 ABI**：x86_64 模拟器的 `SUPPORTED_ABIS` 里也列着 arm64-v8a
        // （arm 翻译层），按"列表里有没有"判断会漏过去，最后变成"没有 QNN 插件"这种让人看不懂的原因。
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
        if (devices.none { it.isQnn() }) return@withContext "注册了 QNN 插件，但没有 QNN 设备"
        null
    }

    override suspend fun prepare(): String? = withContext(Dispatchers.IO) {
        lock.lock()
        try {
            prepareLocked().also { if (it == null) borrowed = false }
        } finally {
            lock.unlock()
        }
    }

    /**
     * 建会话（调用方拿着 [lock]）。**已经有会话就直接算成功**：封面那边可能先借用建过
     * （[runEmbeds]），路由再来 `prepare()` 时认领这一个，别再建第二个（第二个 QNN context 白占几百 MB）。
     */
    private fun prepareLocked(): String? {
        if (session != null) return null
        return try {
            if (!bundled) return "这个包没带 $seqLen token 的 NPU 模型"
            val dir = ensureCopied()
            tokenizerOrLoad() ?: return "词表读不出来"
            tableOrLoad(dir) ?: return "词表矩阵打不开（${NpuAssets.TABLE_FILE}）"
            // 封面借用时可能先于路由的探头走到这里：DSP 路径 + QNN 注册都要在建会话前就位（重复调用无害）
            prepareAdspPath()
            registration.ensure()?.let { return "QNN 插件注册失败：$it" }
            val env = ortEnv()
            val device = qnnDevice() ?: return "没有 QNN 设备（QNN 插件没注册 / 这台机器没有 NPU）"
            val tag = graphTags[variant.assetFile]
                ?: return "读不出 ${variant.assetFile} 的指纹（图拷不出来？）"
            val ctx = File(dir, NpuAssets.ctxFileName(seqLen, tag))
            val cached = ctx.isFile && ctx.length() >= MIN_CTX_BYTES
            val started = System.currentTimeMillis()
            val options = OrtSession.SessionOptions()
            options.setOptimizationLevel(OrtSession.SessionOptions.OptLevel.ALL_OPT)
            // HTP 上真正算的是 DSP，CPU 这侧只搬输入 / 输出；2 个线程够，别和界面抢核
            options.setIntraOpNumThreads(2)
            options.addExecutionProvider(listOf(device), QNN_OPTIONS)
            // 有缓存就直接吃（真机上快很多，也是"不能把编图放进用户路径"的落点）。
            // 缓存坏了（上次写到一半被系统杀掉 / 换过 SoC）就删掉重编一次，别一路坏到用户手里。
            val fromCache = cached && try {
                session = env.createSession(ctx.absolutePath, options)
                true
            } catch (e: Throwable) {
                log("[向量] context 缓存加载失败，删掉重编：${e.message ?: e::class.simpleName}")
                runCatching { ctx.delete() }
                false
            }
            if (!fromCache) {
                options.addConfigEntry("ep.context_enable", "1")
                options.addConfigEntry("ep.context_file_path", ctx.absolutePath)
                // 1 = 把 context binary **嵌进生成的 ONNX**（单文件）：缓存只有一个文件，
                // 不会出现"ONNX 在、旁边的 .bin 丢了"这种半坏状态（默认的 0 是 ONNX + 一个 .bin）。
                options.addConfigEntry("ep.context_embed_mode", "1")
                session = env.createSession(File(dir, variant.assetFile).absolutePath, options)
            }
            val ctxBytes = if (ctx.isFile) ctx.length() else 0L
            detail = buildString {
                append("HTP · w8a16 · 每条 $seqLen token")
                if (ctxBytes > 0) append(" · context ${ctxBytes / 1024 / 1024} MB")
            }
            log(
                "[向量] NPU 会话建好了（每条 $seqLen token，${if (fromCache) "加载 context 缓存" else "编图"}，" +
                    "${System.currentTimeMillis() - started} ms，context ${ctxBytes / 1024 / 1024} MB）",
            )
            null
        } catch (e: Throwable) {
            session = null
            "NPU 建会话失败：${e.message ?: e::class.simpleName}"
        }
    }

    // ------------------------------------------------------------------ 推理

    override suspend fun embed(texts: List<String>): List<FloatArray>? {
        if (texts.isEmpty()) return emptyList()
        return withContext(Dispatchers.Default) {
            lock.lock()
            try {
                val s = session ?: return@withContext null
                val tok = tokenizer ?: return@withContext null
                val tbl = table ?: return@withContext null
                val out = ArrayList<FloatArray>(texts.size)
                for (batch in npuInputs(tok, texts, seqLen)) {
                    out += runEmbedsOn(s, tbl.lookup(batch.ids), batch.mask, batch.batch, batch.len).take(batch.rows)
                }
                out
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                log("[向量] NPU 推理失败：${e::class.simpleName}: ${e.message}")
                null
            } finally {
                lock.unlock()
            }
        }
    }

    /**
     * 一批 `inputs_embeds [batch,len,dim]` + `attention_mask [batch,len]` → 每行一个 768 维向量
     * （模型自己做过 L2 归一化，这里再兜一次）。纯文字和封面都走这里。
     */
    private fun runEmbedsOn(
        session: OrtSession,
        embeds: FloatArray,
        mask: LongArray,
        batch: Int,
        len: Int,
    ): List<FloatArray> {
        val env = ortEnv()
        val dim = embeds.size / (batch * len)
        val shape2 = longArrayOf(batch.toLong(), len.toLong())
        val shape3 = longArrayOf(batch.toLong(), len.toLong(), dim.toLong())
        OnnxTensor.createTensor(env, FloatBuffer.wrap(embeds), shape3).use { emb ->
            OnnxTensor.createTensor(env, LongBuffer.wrap(mask), shape2).use { m ->
                val inputs = mapOf(INPUT to emb, "attention_mask" to m)
                session.run(inputs, setOf(OUTPUT)).use { result ->
                    val out = result.get(OUTPUT).orElseThrow { IllegalStateException("模型没有 $OUTPUT 输出") }
                    val tensor = out as OnnxTensor
                    val outDim = tensor.info.shape.last().toInt()
                    val values = FloatArray(batch * outDim)
                    tensor.floatBuffer.get(values)
                    return (0 until batch).map { row ->
                        l2NormalizeInPlace(values.copyOfRange(row * outDim, (row + 1) * outDim))
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ 封面借用（CoverTextRunner）

    override fun ctxReady(): Boolean {
        val tag = graphTags[variant.assetFile] ?: runCatching {
            ensureCopied()
            graphTags[variant.assetFile]
        }.getOrNull() ?: return false
        val ctx = File(modelDir(), NpuAssets.ctxFileName(seqLen, tag))
        return ctx.isFile && ctx.length() >= MIN_CTX_BYTES
    }

    override fun tokenizerOrNull(): GemmaTokenizer? = tokenizerOrLoad()

    override fun embedTableOrNull(): NpuEmbedTable? = table ?: runCatching { tableOrLoad(ensureCopied()) }
        .onFailure { log("[向量] 词表矩阵打不开：${it.message ?: it::class.simpleName}") }
        .getOrNull()

    override fun runEmbeds(embeds: FloatArray, mask: LongArray): List<FloatArray> {
        lock.lock()
        try {
            if (session == null) {
                prepareLocked()?.let { throw IllegalStateException(it) }
                borrowed = true
            }
            val s = session ?: throw IllegalStateException("文本会话没建起来")
            return runEmbedsOn(s, embeds, mask, NPU_BATCH, seqLen)
        } finally {
            lock.unlock()
        }
    }

    override fun releaseBorrowed() {
        if (!borrowed) return
        if (lock.tryLock()) {
            try {
                if (borrowed) {
                    runCatching { session?.close() }
                    session = null
                    borrowed = false
                }
            } finally {
                lock.unlock()
            }
        }
    }

    /** 词表矩阵：json 在 assets 里（几百字节），权重用私有目录里解出来的那份（[dir]）。 */
    private fun tableOrLoad(dir: File): NpuEmbedTable? {
        table?.let { return it }
        return synchronized(tokenizerLock) {
            table ?: runCatching {
                val json = appContext.assets.open("$assetDir/${NpuAssets.TABLE_FILE}").use { it.readBytes() }
                NpuEmbedTable.open(json.decodeToString(), File(dir, NpuAssets.WEIGHTS_FILE))
            }.onSuccess {
                log("[向量] 词表矩阵：offset=${it.offset}、${it.vocab}×${it.dim} u8（scale=${it.scale} zp=${it.zeroPoint}）")
            }.onFailure {
                log("[向量] 词表矩阵打不开：${it.message ?: it::class.simpleName}")
            }.getOrNull()?.also { table = it }
        }
    }

    // ------------------------------------------------------------------ 分词（分桶 / 截正文用，和 CPU 共用同一份词表）

    override suspend fun tokenLengths(texts: List<String>): IntArray? = withContext(Dispatchers.Default) {
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.tokenLengths(texts)
    }

    override suspend fun truncateToTokens(text: String, maxTokens: Int): String? = withContext(Dispatchers.Default) {
        val tok = tokenizerOrLoad() ?: return@withContext null
        tok.truncateToTokens(text, maxTokens)
    }

    /** 词表：已经读过就直接给；没读过就在小锁里读一次（编图那会儿 [lock] 是占着的，别在这儿抢）。 */
    private fun tokenizerOrLoad(): GemmaTokenizer? {
        tokenizer?.let { return it }
        return synchronized(tokenizerLock) {
            tokenizer ?: runCatching { loadTokenizer() }
                .onFailure { log("[向量] 词表读不出来：${it.message ?: it::class.simpleName}") }
                .getOrNull()
                ?.also { tokenizer = it }
        }
    }

    override fun close() {
        if (lock.tryLock()) {
            try {
                runCatching { session?.close() }
                session = null
                borrowed = false
            } finally {
                lock.unlock()
            }
        }
    }

    // ------------------------------------------------------------------ 环境 / 文件

    private fun ortEnv(): OrtEnvironment = OrtEnvironment.getEnvironment().also { env ->
        runCatching { env.setTelemetry(false) }
    }

    private fun qnnPlugin(): File = File(appContext.applicationInfo.nativeLibraryDir, QNN_PLUGIN)

    private fun qnnDevice(): OrtEpDevice? = runCatching {
        ortEnv().getEpDevices().firstOrNull { it.isQnn() }
    }.getOrNull()

    private fun OrtEpDevice.isQnn(): Boolean = epName.contains("QNN", ignoreCase = true)

    /** DSP 端找 skel 的路径：App 的 native lib 目录 + 系统默认位置。 */
    private fun prepareAdspPath() {
        val libDir = File(appContext.applicationInfo.nativeLibraryDir)
        val value = libDir.absolutePath + ";/vendor/dsp/cdsp;/vendor/lib/rfsa/adsp;/system/lib/rfsa/adsp;/dsp"
        runCatching { Os.setenv("ADSP_LIBRARY_PATH", value, true) }
            .onFailure { log("[向量] ADSP_LIBRARY_PATH 没设上：${it.message}") }
    }

    /** 模型目录：`noBackupFilesDir/embed-npu/<空间标识>/`（**同一个包的所有长度共用一个目录**）。 */
    private fun modelDir(): File = File(File(appContext.noBackupFilesDir, assetDir), spaceModelId)

    private fun assetSize(name: String): Long = runCatching {
        appContext.assets.open("$assetDir/$name").use { it.available().toLong() }
    }.getOrDefault(-1L)

    /**
     * 把 assets 里的模型拷出来（ORT 加载要真实文件路径，外部权重的 `location` 相对 ONNX 文件）。
     *
     * 三张图 + 一份共用权重都在同一个目录里，**只拷一次**：别的长度的引擎进来时看 `COPY_OK` + 逐个文件的大小。
     *
     * 清理规则（别白占几百 MB，也别白删要重编的）：
     * - 同级的**别的**空间目录（换包 / 空间标识变了）整个删掉；
     * - 本目录里不属于这个包的文件删掉（老版本只带 128、新版本带 32+128 时反过来也留 32），
     *   但**这个包里还有的长度**的 context binary 留着——重编一次要几十秒；
     * - **换写法（图结构变了）时 ctx 也必须失效**：名字里带图指纹（[NpuAssets.ctxFileName]），
     *   于是老名字的文件既不在 [keepCtx] 里、也对不上新名字，按上面的规则删掉；`COPY_OK` 里也记了
     *   每张图的指纹，指纹变了就算"不完整"重拷一遍。
     */
    private fun ensureCopied(): File {
        val wanted = variants.map { it.assetFile } + NpuAssets.WEIGHTS_FILE
        synchronized(COPY_LOCK) {
            val dir = modelDir()
            val marker = File(dir, "COPY_OK")
            val sizes = wanted.associateWith { assetSize(it) }
            // 图只有 1.5 MB，算指纹是毫秒级；271 MB 的权重不算（只比大小，构建时已经 sha256 钉死）
            val tags = variants.associate { it.assetFile to NpuAssets.tag(assetSha256(it.assetFile)) }
            val markerText = buildString {
                append(spaceModelId).append('\n')
                tags.toSortedMap().forEach { (name, tag) ->
                    append(NpuAssets.stagedMarker(name, tag)).append('\n')
                }
            }
            graphTags = tags
            val complete = marker.isFile && marker.readText() == markerText && sizes.all { (name, size) ->
                val f = File(dir, name)
                f.isFile && (size <= 0 || f.length() == size)
            }
            if (complete) return dir

            val keepCtx = variants.map { NpuAssets.ctxFileName(it.seqLen, tags.getValue(it.assetFile)) }.toSet()
            dir.parentFile?.listFiles()?.forEach { sibling ->
                if (sibling.isDirectory && sibling.name != dir.name) {
                    // 换空间（例如从只带 32 的包换成带 128 的）：整个老目录连 context 一起删
                    runCatching { sibling.deleteRecursively() }
                }
            }
            dir.mkdirs()
            dir.listFiles()?.forEach { f ->
                if (f.isFile && f.name != "COPY_OK" && f.name !in sizes.keys && f.name !in keepCtx) {
                    runCatching { f.delete() }
                }
            }
            for ((name, expected) in sizes) {
                val target = File(dir, name)
                if (target.isFile && (expected <= 0 || target.length() == expected)) continue
                val tmp = File(dir, "$name.tmp")
                runCatching { tmp.delete() }
                appContext.assets.open("$assetDir/$name").use { input ->
                    FileOutputStream(tmp).use { out -> input.copyTo(out, 1 shl 16) }
                }
                check(tmp.length() > 0) { "$name 拷出来是空的" }
                if (expected > 0) check(tmp.length() == expected) { "$name 大小不对：拷了 ${tmp.length()}，assets 里是 $expected" }
                check(tmp.renameTo(target)) { "$name 改名失败" }
                log("[向量] 已拷出 $name（${target.length() / 1024 / 1024} MB，图指纹 ${tags[name] ?: "-"}）")
            }
            marker.writeText(markerText)
            return dir
        }
    }

    /** assets 里一个文件的 sha256（只对**图**用：1.5 MB，毫秒级；不碰 271 MB 的权重）。 */
    private fun assetSha256(name: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
        appContext.assets.open("$assetDir/$name").use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n <= 0) break
                digest.update(buf, 0, n)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun loadTokenizer(): GemmaTokenizer {
        val started = System.currentTimeMillis()
        val bytes = appContext.assets.open("$tokenizerAssetDir/$TOKENIZER_FILE").use { it.readBytes() }
        val tok = GemmaTokenizer.fromJson(bytes)
        log("[向量] 词表读好了（NPU 用同一份，${bytes.size / 1024 / 1024} MB，${System.currentTimeMillis() - started} ms）")
        return tok
    }

    companion object {
        private const val TAG = "feedhub-embed"

        /** 小于这个就当缓存坏了（正常编出来是几百 MB；截断的写法一眼能看出来）。 */
        private const val MIN_CTX_BYTES = 20L * 1024 * 1024

        private const val TOKENIZER_FILE = "tokenizer.json"
        private const val INPUT = "inputs_embeds"
        private const val OUTPUT = "sentence_embedding"

        /** `registerExecutionProviderLibrary` 时用的名字（同一份里 `getEpDevices()` 查的就是它）。 */
        private const val QNN_EP_NAME = "qnn"

        /** QNN EP 插件 .so 的名字（`com.qualcomm.qti:onnxruntime-android-qnn` 的 AAR 里那一个）。 */
        const val QNN_PLUGIN = "libonnxruntime_providers_qnn.so"

        /** 拷文件的那把锁：几张图的引擎会先后进来，别各拷一份 271 MB。 */
        private val COPY_LOCK = Any()

        /**
         * QNN provider options：跑 HTP；`offload_graph_io_quantization=0`。
         *
         * 图输入现在是 f32 的 `inputs_embeds`，后面紧跟一个 Q：`=1` 时 QNN 不接"输入是图输入的 Q"
         * （`simple_op_builder.cc`），这个 Q 掉回 CPU、图变成 2 个分区；`=0` 时整张图 1 个分区
         * （`tools/embed/qnn_capability.py --offload-io 0`：4105 个节点全在 QNN）。
         * 以前吃 `input_ids` 的图用的是 `=1`（那时图输入是整数 id，没有这个 Q）。
         */
        private val QNN_OPTIONS: Map<String, String> = mapOf(
            "backend_type" to "htp",
            "offload_graph_io_quantization" to "0",
        )

        /**
         * 这个包里带了哪些长度的图（`assets/embed-npu/` 的文件名）。
         *
         * 名字不合规则（不是 `model_w8a16_b8_l<L>_emb.onnx`）的文件一律忽略（`model_w8a16_b8.weights` 就是这么被
         * 忽略掉的）。
         */
        fun discover(context: Context): List<NpuModelVariant> {
            val assets = runCatching { context.assets.list(NpuAssets.DIR)?.toList().orEmpty() }
                .getOrDefault(emptyList())
            return NpuAssets.variantsIn(assets)
        }
    }
}

/**
 * 安卓上的 [NpuEngineFactory]：包里带了几张图就造几个引擎（每个长度一张图、一份 context 缓存，**共用一份权重**）。
 *
 * 引擎**按长度懒建并缓存**：切回已经建过的长度不会重复读词表（`NpuOnnxTextEmbedder` 自己留着词表）。
 */
class NpuOnnxEngines(
    private val context: Context,
    val variants: List<NpuModelVariant>,
    private val log: (String) -> Unit = { Log.i("feedhub-embed", it) },
) : NpuEngineFactory {
    override val lengths: List<Int> = variants.map { it.seqLen }.sorted()

    override val spaceLen: Int = NpuAssets.spaceLen(lengths)

    private val byLen: Map<Int, NpuModelVariant> = variants.associateBy { it.seqLen }

    private val engines = HashMap<Int, NpuEngine>()

    override fun create(seqLen: Int): NpuEngine = synchronized(engines) {
        engines.getOrPut(seqLen) {
            val variant = byLen[seqLen] ?: error("这个包里没有 $seqLen token 的 NPU 图（有 ${lengths.joinToString()}）")
            NpuOnnxTextEmbedder(
                context = context,
                variant = variant,
                variants = variants,
                spaceModelId = npuModelId(spaceLen),
                log = log,
            )
        }
    }

    /**
     * 封面向量"文本+图"那一步借的引擎：**就是路由手里那个**（[create] 按长度缓存），
     * 所以 NPU 上纯文字和封面共用一个文本会话。包里没有这个长度的图就是 null（封面那条显示没带图）。
     */
    fun coverTextRunner(seqLen: Int): CoverTextRunner? =
        if (seqLen in byLen) create(seqLen) as? CoverTextRunner else null

    /**
     * **只要分词**：用**最短那张**的引擎实例（会话还是懒建的，这里只读 `tokenizer.json`）。
     *
     * 好处：`prepare()` 后面建会话时 `create(seqLen)` 拿到的就是这同一个实例，词表不会读两遍；
     * NPU 还在编图时调度器也能按真实 token 数分桶（短文本立刻能走 32 的图，不用等 128）。
     */
    override suspend fun tokenLengths(texts: List<String>): IntArray? {
        if (lengths.isEmpty()) return null
        return create(lengths.first()).tokenLengths(texts)
    }

    override suspend fun truncateToTokens(text: String, maxTokens: Int): String? {
        if (lengths.isEmpty()) return null
        return create(lengths.first()).truncateToTokens(text, maxTokens)
    }
}
