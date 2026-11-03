package feedhub.app.embed

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.library.EMBEDDING_MODEL_ID
import feedhub.app.library.npuModelId
import feedhub.rust.RustEmbed
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.feedhub_ffi.FfiTokenSourceKind

/**
 * 一批向量实际用的后端：写进性能记录、设置页也显示。
 *
 * **CPU 和 NPU 算出来的不是同一批点**，
 * 所以两边各有自己的 [Embedder.modelId]，库里按 `item_embedding.model` 分开，绝不在同一个索引里混着比。
 */
enum class EmbedBackend { CPU, NPU }

/**
 * NPU 路线的状态。
 *
 * 这个密封接口只是给设置页 / 测试认的类型，
 * 文案（[backendLabel] / [explanation]）还在 Kotlin——那是界面的事，而且要从测试里读。
 */
sealed interface NpuStatus {
    /** 这个包没带 NPU 模型（构建时没加 `-Pfeedhub.embedNpu=true`）。开关灰显。 */
    data object NoModel : NpuStatus

    /** 带了模型，但开关关着：用 CPU。 */
    data object Off : NpuStatus

    /**
     * **只带 NPU 模型**的包，开关关着。
     *
     * 这种包里没有 CPU 的 q4 模型，所以「用 NPU 算」就是本地向量的**总开关**：关着 = 一条都不算，
     * 没有"回退到 CPU"这回事。
     */
    data class Disabled(val reason: String) : NpuStatus

    /**
     * **只带 NPU 模型**的包，NPU 真用不了（没 QNN 设备 / 编图失败 / 推理失败…）。
     *
     * 没有 CPU 可退回，所以本地向量整个不可用——安静地不算、不存（和"包里没带模型"同一种安静）。
     */
    data class Unavailable(val reason: String) : NpuStatus

    /** 开关开着，正在探设备 / 编图；这段时间**要算的批等着**（不回退 CPU）。 */
    data class Preparing(val step: String) : NpuStatus

    /**
     * 已经在用 NPU 算。[lens] 是**现在有几张图就绪**（升序，例如 `[32, 128]`）——设置页那行写在它上面，
     */
    data class Active(val detail: String, val lens: List<Int> = emptyList()) : NpuStatus

    /** 用不了，已回退 CPU（[reason] 是给人看的一句话，例如"这台设备没有 NPU"）。 */
    data class Fallback(val reason: String) : NpuStatus
}

/** 设置页那一行"当前实际在用的后端"（短标签；带长度时就是"NPU · 32 / 128 token"）。 */
val NpuStatus.backendLabel: String
    get() = when (this) {
        NpuStatus.NoModel, NpuStatus.Off -> "CPU"
        is NpuStatus.Disabled -> "关着"
        is NpuStatus.Unavailable -> "不可用"
        is NpuStatus.Preparing -> "NPU 编译中"
        is NpuStatus.Active -> if (lens.isEmpty()) "NPU" else "NPU · ${lens.joinToString(" / ")} token"
        is NpuStatus.Fallback -> "回退到 CPU"
    }

/** 设置页那一行下面的说明（回退时就是"回退到 CPU（原因）"）。 */
val NpuStatus.explanation: String
    get() = when (this) {
        NpuStatus.NoModel -> "这个包没带 NPU 模型（构建时没加 -Pfeedhub.embedNpu=true）"
        NpuStatus.Off -> "CPU（「用 NPU 算」关着）"
        is NpuStatus.Disabled -> "本地向量关着（$reason）"
        is NpuStatus.Unavailable -> "本地向量不可用（$reason；这个包没有 CPU 模型可退回）"
        is NpuStatus.Preparing -> "NPU 编译中（$step；这段时间要算的批先等着，不回退 CPU）"
        is NpuStatus.Active -> "NPU（$detail）"
        is NpuStatus.Fallback -> "回退到 CPU（$reason）"
    }

/** Rust 的状态 → Kotlin 的密封接口（设置页用）。 */
internal fun RustEmbed.RouterStatus.toNpuStatus(): NpuStatus = when (kind) {
    RustEmbed.RouterStatusKind.NoModel -> NpuStatus.NoModel
    RustEmbed.RouterStatusKind.Off -> NpuStatus.Off
    RustEmbed.RouterStatusKind.Disabled -> NpuStatus.Disabled(reason)
    RustEmbed.RouterStatusKind.Unavailable -> NpuStatus.Unavailable(reason)
    RustEmbed.RouterStatusKind.Preparing -> NpuStatus.Preparing(detail)
    RustEmbed.RouterStatusKind.Active -> NpuStatus.Active(detail, lens)
    RustEmbed.RouterStatusKind.Fallback -> NpuStatus.Fallback(reason)
}

/**
 * NPU 那一侧的实现。
 *
 * 安卓是 [feedhub.app.embed.NpuOnnxTextEmbedder]（QNN EP + HTP + context binary）；桌面 / 测试给假的
 * （离线测"后端选择 / 回退"），所以这层只放**语义**，不碰 ORT / Android。
 *
 * 所有方法都**不抛**：用返回值 / null 表示失败——向量是"有了更好"的东西，不能把界面或后台拖崩
 * （和 [Embedder.embed] 一个口径）。
 */
interface NpuEngine {
    /**
     * 写进 `item_embedding.model` 的标识。**同一个包里所有长度的图共用这一个**：
     * 装得下的文本，短图和长图算出来是同一个向量，
     * 所以换图**不是**换向量空间，已经算好的向量不用重算。
     */
    val modelId: String

    /** 这张图每条最多几个 token（含 `<bos>` / `<eos>`）——输入按它补 / 截，**不再写死 32**。 */
    val seqLen: Int

    /** 包里带没带这张图（只查 assets，不读内容）。 */
    val bundled: Boolean

    /** 探头：这台设备能不能走 NPU。null = 能，否则是原因（给人看的一句话，例如"这台设备没有 NPU"）。 */
    suspend fun probe(): String?

    /** 编图 / 加载 context binary 并建会话。null = 成功，否则是原因。 */
    suspend fun prepare(): String?

    /** 用 NPU 算一批（定长形状由实现按 [seqLen] 处理）。没就绪 / 算失败返回 null。 */
    suspend fun embed(texts: List<String>): List<FloatArray>?

    /** 这一批文本各几个 token（同一个分词器；还没准备好就是 null）。 */
    suspend fun tokenLengths(texts: List<String>): IntArray? = null

    /** 按 token 截断（同一个分词器；还没准备好就是 null）。 */
    suspend fun truncateToTokens(text: String, maxTokens: Int): String? = null

    /** 释放会话（退到后台 / 关掉开关时）。context binary 留在磁盘上，下次直接加载。 */
    fun close()

    /** 设置页显示的一句话（后端型号 / 每条多少 token / context 多大）。 */
    val detail: String
}

/**
 * 包里那几张 NPU 图。
 *
 * 一个长度一张固定形状的图，**全部编好、按批挑最短够用的那张**（[RoutingEmbedder]）。拆成接口是为了离线能测
 * （假的工厂 → 假的 [NpuEngine]），安卓那边就是 [feedhub.app.embed.NpuOnnxTextEmbedder] 的工厂。
 */
interface NpuEngineFactory {
    /** 包里带的长度（升序、非空）。 */
    val lengths: List<Int>

    /**
     * 这个包的**向量空间标识**用的长度 = 包里最长的那个（`npuModelId(spaceLen)`）。
     *
     * 为什么不是"当前用的那张"：短图算出来的向量和最长那张是同一个空间，
     * 所以 model 只跟着"这个包最多能看多少正文"走，换图不换 id、不触发整库重算。
     */
    val spaceLen: Int

    /** 按长度造一个引擎（每个长度一张图 / 一份 context 缓存）。 */
    fun create(seqLen: Int): NpuEngine

    /**
     * **只要分词**（不需要会话）：调度器在 NPU 还没就绪时也要数 token 才能按桶取批。
     *
     * 为什么不让调用方自己去建一个引擎：分词器是懒加载的，`prepare()` 本来也要读它；
     * 这里借用工厂的"按长度缓存引擎"，读一次就一直在。null = 这个工厂不提供（离线假实现就是这样，
     * 调度器会退回"数量未知、按最长那张"）。
     */
    suspend fun tokenLengths(texts: List<String>): IntArray? = null

    /** 同上，按 token 截断（详情正文用）。 */
    suspend fun truncateToTokens(text: String, maxTokens: Int): String? = null
}

/**
 * **每个进程只能注册一次**的执行器（QNN EP 的硬性限制：再注册会报 "EP library already registered"）。
 *
 * 放在 commonMain 是为了能离线测（假 registrar 第二次调用就抛那条错）；安卓那边传进来的就是
 * `OrtEnvironment.registerExecutionProviderLibrary`。
 *
 * 失败**不记死**：注册失败（例如第一次时 QNN 库还没准备好）可以再试；只有"已经注册过"才当成成功。
 */
class OnceRegistration(private val doRegister: () -> Unit) {
    private val lock = Any()
    private var done = false

    /** 最近一次失败的原因（给日志 / 设置页看）。 */
    @Volatile
    var lastFailure: String? = null
        private set

    /** 已经成功过就直接返回 null；否则尝试一次，失败返回原因（不抛）。 */
    fun ensure(): String? = synchronized(lock) {
        if (done) return null
        try {
            doRegister()
            done = true
            lastFailure = null
            null
        } catch (e: Throwable) {
            val message = "${e::class.simpleName}: ${e.message}"
            if (message.contains("already registered", ignoreCase = true)) {
                // 别的代码路径已经注册过（或上一次其实成功了）：算可用
                done = true
                lastFailure = null
                null
            } else {
                lastFailure = message
                message
            }
        }
    }
}

/**
 * 设置页 / [feedhub.app.AppGraph] 用的 NPU 开关面（只有 [RoutingEmbedder] 实现）。
 *
 * 分成接口是为了桌面 / 测试 / 没带 NPU 的包完全不认识这一套（`graph.embeddingBackend == null`，
 * 设置页就只有一个灰显的开关 + 一句"这个包没带 NPU 模型"）。
 */
interface EmbeddingBackendControl {
    /** 包里带没带 NPU 模型（没带 → 设置页那个开关灰显）。 */
    val npuBundled: Boolean

    /**
     * 包里带没带 **CPU** 的 q4 模型。
     *
     * `false` = 这个 NPU 包**没带 CPU 模型**（`-Pfeedhub.embedNpu=true` 的构建不拷 `embeddinggemma-2` 那三个文件）：
     * 设置页要把「用 NPU 算」写成"本地向量的总开关"，并且不能再说"回退到 CPU"。
     */
    val cpuBundled: Boolean

    /** 当前状态（Compose 状态，设置页直接读；改了会重组）。 */
    val npuStatus: NpuStatus

    /** 包里带的那几张图的长度（升序；空 = 没模型）。设置页用来写"包里另带 …"。 */
    val npuLengths: List<Int>

    /** 设置里的「用 NPU 算」变了（不用重启 App）。 */
    fun setRequested(on: Boolean)

    /** 前后台变了（编译只在前台做；退到后台把会话放掉）。 */
    fun onForeground(active: Boolean)
}

/**
 * CPU / NPU 的后端路由。
 *
 * 这个类现在只剩宿主那一半（[RustEmbed.Router] 补不上的那些）：
 * - **协程**：准备协程的 `launch` / `cancel` / 代号（Rust 给代号，Kotlin 只管 `Job`）；
 * - **引擎句柄**：[NpuEngine] 的 map（`LinkedHashMap<Int, NpuEngine>`）——句柄是框架对象，过不了边界；
 * - **框架调用**：`probe()` / `prepare()` / `embed()` / `close()`，以及 `cpu.close()`；
 * - **Compose 状态**：[npuStatus] 这个 `mutableStateOf`（界面在读）。
 *
 * 判定**全在 Rust**
 * （`feedhub-core/src/embed/router.rs`），走"宿主拉一步、Rust 回一步"：
 *
 * ```
 * prepare_step(charging) ─┬─ Wait{delay}    → delay → 再拉
 *                         ├─ Probe{len}     → create(len).probe()      → report_probe(reason)
 *                         ├─ Compile{len}   → cpu.close(); prepare()   → report_compile(reason, detail)
 *                         ├─ Commit{len}    → engines[len] = e          → commit(len)
 *                         ├─ Discard{len}   → e.close()
 *                         └─ Finished
 * embed_step(maxTokens)  ─┬─ WaitReady{delay, startPrepare} → 可能 startPrepare
 *                         ├─ UseCpu{model}  → begin_cpu() … end_cpu()
 *                         └─ RunNpu{len}    → embed()  → report_embed(len, ok)
 * ```
 *
 * 行为口径（一条没改，
 * **只带 NPU 模型**的包里没有 CPU，就是"本地向量整个关着"
 *   （[NpuStatus.Disabled]，不算、不存）；
 * - **开关打开**：先在前台探设备，然后**按长度从小到大**一张一张编（[NpuStatus.Preparing]）。
 *   这段期间 `npuLengths` 里的每张图**都没有就绪**的批**等着**（不占 CPU、不丢、不写库、不计失败）；
 * - **多张图**：每一批按"这批里最长那条的 token 数"挑**能装下它的最短的那张**；
 * - **真失败** → 有 CPU 时 [NpuStatus.Fallback] 并**用 CPU**，没有 CPU（NPU 包）时 [NpuStatus.Unavailable]；
 * - 编图只在前台、并且（**在充电** 或 **手头没有在算的批**）时开始；编图前会先把 CPU 会话放掉；
 * - 退到后台：会话全放掉（context binary 在磁盘上，回前台加载很快）。
 *
 * **NPU 包传 null**（包里没带 CPU 模型，也不建 CPU 会话）。
 * @param npu 包里那几张 NPU 图；null = 这份构建没有（桌面、没带 QCN 的包）。
 */
class RoutingEmbedder(
    private val cpu: Embedder?,
    private val npu: NpuEngineFactory?,
    private val scope: CoroutineScope,
    /** 在充电吗（"只在前台、充电或空闲时编译"的第一个条件）。前后台由 [onForeground] 喂。 */
    private val charging: () -> Boolean = { false },
    private val log: (String) -> Unit = {},
) : Embedder, EmbeddingBackendControl, TextSessionGate {

    private val cpuModelId: String get() = cpu?.modelId ?: EMBEDDING_MODEL_ID

    /** 这个包里有 CPU 的 q4 模型吗（NPU 包没有：构建时就不拷那三个文件，也不建 CPU 会话）。 */
    private val hasCpu: Boolean get() = cpu?.available == true

    /** 这个包的向量空间长度（最长的那张图；没模型时 0）。 */
    private val spaceLen: Int get() = npu?.spaceLen ?: 0

    /**
     * **判定与状态都在 Rust**（`embed::router::Router`）。构造参数把"这个包里有什么"告诉它，
     * 之后每一件事都由它回。
     */
    private val router = RustEmbed.Router(
        npuLengths = npu?.lengths.orEmpty(),
        spaceLen = spaceLen,
        hasCpu = hasCpu,
        cpuPresent = cpu != null,
        cpuModel = cpuModelId,
        npuSpaceModel = if (spaceLen > 0) npuModelId(spaceLen) else "",
    )

    override val npuLengths: List<Int> get() = router.npuLengths()

    override val npuBundled: Boolean get() = router.npuBundled()

    override val cpuBundled: Boolean get() = router.cpuBundled()

    /** 设置页直接读的 Compose 状态（值由 Rust 给，改了会重组）。 */
    override var npuStatus: NpuStatus by mutableStateOf(router.status().toNpuStatus())
        private set

    /**
     * 就绪的（会话在内存里的）长度 → 引擎。**句柄过不了边界**，所以 map 在宿主这边；
     * "哪几张就绪"以 Rust 那份为准（[RustEmbed.Router.readyLengths]），两边在同一把锁里改。
     */
    private val enginesLock = Any()
    private val engines = LinkedHashMap<Int, NpuEngine>()

    /** 准备协程（`launch` / `cancel` 是宿主的活）。 */
    private var prepareJob: Job? = null

    /** 正在 `Probe` / `Compile` 之间、还没提交的那张引擎。 */
    private var pendingEngine: NpuEngine? = null

    init {
        flushLogs()
    }

    // ------------------------------------------------------------------ 开关 / 前后台

    override fun setRequested(on: Boolean) {
        val start = router.setRequested(on)
        flushLogs()
        publishStatus()
        if (!on) {
            cancelPrepareJob()
            return
        }
        if (start) startPrepare()
    }

    override fun onForeground(active: Boolean) {
        // 退到后台：会话全放掉（context binary 在磁盘上，回前台加载很快）；在跑的准备也停掉。
        // 顺序讲究：先让 Rust 把状态切过去，再关本地的句柄，最后把状态刷到 Compose。
        if (!active) {
            cancelPrepareJob()
            router.onForeground(false)
            closeEngines()
            flushLogs()
            publishStatus()
            return
        }
        // 回前台：Rust 决定"要不要接着准备"，这里只管把 Job 拉起来
        val start = router.onForeground(true)
        flushLogs()
        publishStatus()
        if (start) startPrepare()
    }

    /** 释放所有会话（不退开关、不清 broken）：下次准备会重新加载 context binary。 */
    private fun closeEngines() {
        val closed = synchronized(enginesLock) { engines.values.toList().also { engines.clear() } }
        for (e in closed) runCatching { e.close() }
        pendingEngine?.let { runCatching { it.close() } }
        pendingEngine = null
        router.closeEngines()
    }

    /**
     * 分时：封面向量那侧在**编图 / 算封面之前**把纯文字会话放掉。
     *
     * 为什么：真机上"另一张 QNN context 在场时现编文本图"必挂（6001 / MEM_ALLOC），最省事的做法是
     * 严格串行——封面窗口里只有封面那两张会话活着。放掉之后**不用谁去重建**：[embed] 发现没有就绪的引擎
     * 会自己 `prepare()` 从 ctx 加载（真机 3 s 量级），批不会丢、不写库那一条也不受影响。
     */
    override fun releaseSessions() {
        if (router.readyLengths().isEmpty()) return
        closeEngines()
        flushLogs()
        publishStatus()
    }

    /** 取消正在进行的准备（宿主的 `Job` + Rust 的代号一起推进）。 */
    private fun cancelPrepareJob() {
        prepareJob?.cancel()
        prepareJob = null
        pendingEngine?.let { runCatching { it.close() } }
        pendingEngine = null
        router.cancelPrepare()
    }

    private fun startPrepare() {
        if (prepareJob?.isActive == true) return
        prepareJob = scope.launch {
            try {
                runPrepare()
            } finally {
                prepareJob = null
            }
        }
    }

    /**
     * 一张一张编（短的先来：大部分卡片的 token 中位 21～23，短的先好就能立刻用上）。
     *
     * 每一步"该干什么"都是 Rust 说的（[RustEmbed.Router.prepareStep]）；这里只执行框架调用并把结果报回去。
     */
    private suspend fun runPrepare() {
        try {
            while (true) {
                val step = router.prepareStep(charging())
                // 状态是 Rust 在 prepareStep 里推的（Preparing 的措辞、超时回退…），每拉一步就刷一次
                flushLogs()
                publishStatus()
                when (step) {
                    is RustEmbed.PrepareStep.Wait -> delay(step.delayMs)
                    is RustEmbed.PrepareStep.Probe -> {
                        val engine = npu?.create(step.len) ?: run {
                            router.prepareCancelled()
                            return
                        }
                        pendingEngine = engine
                        publishStatus()
                        router.reportProbe(engine.probe())
                        flushLogs()
                        publishStatus()
                    }
                    is RustEmbed.PrepareStep.Compile -> {
                        // 每一张图都要一个新的引擎对象：探设备只探一次（Probe 那一步），
                        // 第二张图直接从 Compile 开始，所以这里在 pendingEngine 为空时现造一个。
                        val engine = pendingEngine ?: npu?.create(step.len) ?: run {
                            router.prepareCancelled()
                            return
                        }
                        pendingEngine = engine
                        publishStatus()
                        // 编图峰值内存约 1.9 GB：先把 CPU 会话（约 300 MB）放掉；失败的话，下一批算的时候会自己再建
                        runCatching { cpu?.close() }
                        val failure = try {
                            engine.prepare()
                        } catch (err: CancellationException) {
                            throw err
                        } catch (err: Throwable) {
                            "${err::class.simpleName}: ${err.message}"
                        }
                        val next = router.reportCompile(failure, engine.detail)
                        when (next) {
                            is RustEmbed.PrepareStep.Commit -> commitEngine(engine, next.len)
                            is RustEmbed.PrepareStep.Discard,
                            is RustEmbed.PrepareStep.Finished,
                            -> {
                                // 回退了 / 打断了 / 编完已经在后台：这张会话作废
                                runCatching { engine.close() }
                                pendingEngine = null
                            }
                            // 理论上不会再回 Wait / Probe / Compile（一轮里已经过了编图那一步），
                            // 真出现就按"继续拉下一步"处理
                            else -> Unit
                        }
                        flushLogs()
                        publishStatus()
                        if (next is RustEmbed.PrepareStep.Finished) return
                    }
                    is RustEmbed.PrepareStep.Commit -> commitEngine(pendingEngine, step.len)
                    is RustEmbed.PrepareStep.Discard -> {
                        pendingEngine?.let { runCatching { it.close() } }
                        pendingEngine = null
                    }
                    is RustEmbed.PrepareStep.Finished -> return
                }
            }
        } catch (e: CancellationException) {
            router.prepareCancelled()
            throw e
        } catch (e: Throwable) {
            // 兜底：真异常也按"真失败"处理（Rust 那边决定回退 / 不可用）
            router.reportCompile("准备失败：${e.message ?: e::class.simpleName}", "")
            flushLogs()
            publishStatus()
        }
    }

    private fun commitEngine(engine: NpuEngine?, len: Int) {
        val e = engine ?: return
        synchronized(enginesLock) { engines[len] = e }
        pendingEngine = null
        router.commit(len)
        publishStatus()
    }

    /** Rust 的状态 → Compose 字段（每次状态可能变了之后就调）。 */
    private fun publishStatus() {
        npuStatus = router.status().toNpuStatus()
    }

    /** Rust 攒的日志行接到宿主的口（**不含任何内容**）。 */
    private fun flushLogs() {
        for (line in router.drainLogs()) log(line)
    }

    private fun readyLengths(): List<Int> = synchronized(enginesLock) { engines.keys.sorted() }

    private fun readyEngine(len: Int): NpuEngine? = synchronized(enginesLock) { engines[len] }

    // ------------------------------------------------------------------ 状态 / 模型

    /** 写进库的模型标识：判定在 Rust。 */
    override val modelId: String get() = router.modelId()

    override val available: Boolean get() = router.available()

    // ------------------------------------------------------------------ 推理

    override suspend fun embed(texts: List<String>): List<FloatArray>? = embed(texts, Int.MAX_VALUE)

    /**
     * 算一批（任务-向量省电 §1 + §2）。
     *
     * **等 NPU 就绪**是这里最关键的一段：开关开着、还没就绪（探设备 / 编图 / 回前台重新加载）时，
     * 这个方法**挂起等待**（Rust 给间隔，默认 300 ms 看一次，不占 CPU），**不调 CPU**——批留在调度器手里，
     * 不丢、不写库。只有"真失败"才走 CPU。
     */
    override suspend fun embed(texts: List<String>, maxTokens: Int): List<FloatArray>? {
        if (texts.isEmpty()) return emptyList()
        while (true) {
            when (val step = router.embedStep(maxTokens)) {
                is RustEmbed.EmbedStep.RunNpu -> {
                    val engine = readyEngine(step.len)
                    if (engine == null) {
                        // 会话在这期间被关掉了（退到后台 / 换图）：不算失败，回去重新选
                        router.reportEmbed(step.len, false)
                        continue
                    }
                    val vectors: List<FloatArray>? = try {
                        try {
                            engine.embed(texts)
                        } catch (err: CancellationException) {
                            throw err
                        } catch (err: Throwable) {
                            log("[向量] NPU 这一批失败了：${err::class.simpleName}: ${err.message}")
                            null
                        }
                    } finally {
                        // 成功 / 失败都要报账（否则 in_flight 漏账、编图永远等不到空闲）
                    }
                    val ok = vectors != null && vectors.size == texts.size
                    router.reportEmbed(step.len, ok)
                    flushLogs()
                    publishStatus()
                    if (ok) return vectors
                    // 会话被关掉的那条路：Rust 已经不动 broken，回环重选
                    if (router.isBroken()) return cpuEmbed(texts)
                    continue
                }
                is RustEmbed.EmbedStep.UseCpu -> return cpuEmbed(texts)
                is RustEmbed.EmbedStep.WaitReady -> {
                    if (step.startPrepare) startPrepare()
                    delay(step.delayMs)
                }
                is RustEmbed.EmbedStep.Empty -> return emptyList()
            }
        }
    }

    private suspend fun cpuEmbed(texts: List<String>): List<FloatArray>? {
        // 没有 CPU 模型（NPU 包）：没有"退回去算"这回事，直接没有结果（调度器不写库、不计数失败）
        val c = cpu ?: run {
            router.accountCpu()
            return null
        }
        router.beginCpu()
        try {
            return c.embed(texts)
        } finally {
            router.endCpu()
        }
    }

    /**
     * 这批文本各几个 token。顺序（Rust 给的）：已经就绪的 NPU 引擎 → CPU 的词表 → **工厂的"只要分词"那条**
     */
    override suspend fun tokenLengths(texts: List<String>): IntArray? {
        for (source in router.tokenSources()) {
            val answer = when (source.kind) {
                FfiTokenSourceKind.ENGINE -> readyEngine(source.len)?.tokenLengths(texts)
                FfiTokenSourceKind.CPU -> cpu?.tokenLengths(texts)
                FfiTokenSourceKind.FACTORY -> npu?.tokenLengths(texts)
                else -> null
            }
            if (answer != null) return answer
        }
        return null
    }

    override suspend fun truncateToTokens(text: String, maxTokens: Int): String? {
        for (source in router.tokenSources()) {
            val answer = when (source.kind) {
                FfiTokenSourceKind.ENGINE -> readyEngine(source.len)?.truncateToTokens(text, maxTokens)
                FfiTokenSourceKind.CPU -> cpu?.truncateToTokens(text, maxTokens)
                FfiTokenSourceKind.FACTORY -> npu?.truncateToTokens(text, maxTokens)
                else -> null
            }
            if (answer != null) return answer
        }
        return null
    }

    override fun close() {
        runCatching { cpu?.close() }
        closeEngines()
        router.closeSessions()
        flushLogs()
        publishStatus()
    }

    // ------------------------------------------------------------------ 这一批的账

    override val lastEmbedBackend: EmbedBackend
        get() = if (router.lastBatch().backend == "NPU") EmbedBackend.NPU else EmbedBackend.CPU

    override val lastEmbedModelId: String get() = router.lastBatch().model

    override val lastEmbedSeqLen: Int get() = router.lastBatch().seqLen
}
