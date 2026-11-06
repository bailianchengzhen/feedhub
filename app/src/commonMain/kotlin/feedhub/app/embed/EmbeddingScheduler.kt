package feedhub.app.embed

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.library.EMBEDDING_DIM
import feedhub.app.library.EMBEDDING_MODEL_ID
import feedhub.app.library.EmbeddingStat
import feedhub.app.library.Library
import feedhub.app.library.SEEN_ITEM_RETENTION_DAYS
import feedhub.core.FeedItem
import feedhub.rust.RustEmbed
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext
import uniffi.feedhub_ffi.FfiEmbedStepKind

/**
 * 后台给卡片算向量。
 *
 * 这个类只剩
 * **协程循环 + 调推理 + Compose 状态**（宿主那一半）：
 *
 * ```
 * 拉一步（Rust 说下一步做什么）→ 做（分词 / 推理 / 取补算）→ 交回去 → 下一轮
 * ```
 *
 * 三条输入、三个队列（**优先级从上到下**，Rust 排的）：
 * 1. **首页 / 关注的新卡片**（[offer]，来源 `feed`）——**永远优先**；
 * 2. **点开过的条目 + 已经加载出来的正文**（[offerDetail]）——用"标题 + 正文开头"重新算一份、覆盖原向量
 *    （同一个 key、同一个 model；来源记 `favorite` / `history`）；正文只用来算向量，**不存库、不导出**；
 * 3. **收藏 / 历史里的条目**（[requestBackfill]）——补算，队列空了才取下一块。
 *
 * 节制条件（Rust 执行，参数由构造器注入）：只在前台算、一次 [batchSize] 条、每两批之间 [gapMs] 让出、
 * 受「记录浏览行为」管、18+ 不算、已经有向量的不再算、只记条数和耗时。
 *
 * @param dim 存下来的维数。
 * @param backfillChunk 补算一次从库里取几条。
 * @param backfillWindowMs 补算只看最近多少毫秒内的收藏 / 历史。
 * @param graphLengths 后端包里带的**固定长度图**的 token 数（升序；≤1 张 = 不用分组）。
 */
class EmbeddingScheduler(
    private val scope: CoroutineScope,
    private val library: Library,
    private val embedder: Embedder?,
    /** 设置里的「记录浏览行为」；关着时不算、不存。 */
    private val enabled: () -> Boolean,
    /** App 在不在前台。 */
    private val foreground: () -> Boolean,
    private val batchSize: Int = 8,
    private val gapMs: Long = 50L,
    private val dim: Int = EMBEDDING_DIM,
    private val now: () -> Long = System::currentTimeMillis,
    private val log: (String) -> Unit = {},
    private val io: CoroutineContext = Dispatchers.IO,
    private val work: CoroutineContext = Dispatchers.Default,
    /** 每算完一批调一次（性能记录用：后端 / 条数 / 耗时 / 模型 / 这张图的长度；**不传内容**）。 */
    private val onBatch: ((EmbedBatchDone) -> Unit)? = null,
    private val backfillChunk: Int = 24,
    private val backfillWindowMs: Long = SEEN_ITEM_RETENTION_DAYS * 86_400_000L,
    private val graphLengths: () -> List<Int> = { emptyList() },
) {
    @Volatile
    private var stateCreated = false

    /** 调度状态机（Rust，懒建：基础版 embedder == null 时不触发 FFI 分配）。队列 / 去重 / 分桶 / 落库的行都在它里面。 */
    private val state by lazy {
        stateCreated = true
        RustEmbed.Scheduler(
            RustEmbed.embedConfig(
                batchSize = batchSize,
                dim = dim,
                backfillChunk = backfillChunk,
                backfillWindowMs = backfillWindowMs,
                detailMaxTokens = RustEmbed.detailMaxTokens,
                detailMaxChars = DETAIL_MAX_CHARS,
            ),
        )
    }

    /** 库操作过河（一次一批）。 */
    private val store by lazy { LibraryEmbedStore(library) }

    /**
     * 写进 `item_embedding.model` 的模型标识（跟着后端变；每批现读）。
     *
     * 换图不触发重算。
     */
    val modelId: String get() = embedder?.modelId ?: EMBEDDING_MODEL_ID

    /** 这个构建带没带模型（设置页显示「EG2 q4 / 未包含模型」）。 */
    val available: Boolean get() = embedder?.available == true

    /** 现在有没有纯文字批**在算或者排队**（封面向量那条用它让路）。 */
    val busy: Boolean get() = job?.isActive == true || (stateCreated && state.busy())

    /** 库里现在一共多少条向量（设置页"已算条数"）。 */
    var count: Int by mutableIntStateOf(0)
        private set

    /** 库里向量按 `(model, source)` 的分布（设置页"首页 / 收藏 / 历史"那一行）。 */
    var stats: List<EmbeddingStat> by mutableStateOf(emptyList())
        private set

    /** 从启动到现在算过多少条。 */
    var computed: Int by mutableIntStateOf(0)
        private set

    /** 从启动到现在**补算**了多少条（收藏 / 历史）。 */
    var backfilled: Int by mutableIntStateOf(0)
        private set

    /** 从启动到现在用**详情正文**覆盖了多少条。 */
    var detailed: Int by mutableIntStateOf(0)
        private set

    /** 最近一轮平均每条多少毫秒；还没算过是 null。 */
    var avgMs: Long? by mutableStateOf(null)
        private set

    /** 最近一批的条数。 */
    var lastBatchSize: Int by mutableIntStateOf(0)
        private set

    private var job: Job? = null
    private var unavailableLogged = false

    init {
        if (embedder != null) refreshCountAsync()
    }

    /**
     * 新内容到了（可以重复给、可以给已经在队列里的）：只留下"值得算 + 还没算 + 还没排在队里"的。
     * 在主线程调用；不阻塞、不建会话。
     */
    fun offer(items: List<FeedItem>) {
        if (items.isEmpty()) return
        // 关着 / 在后台 / 没有模型时**连队列都不进**：否则关着开关浏览一下午，队列会把每一批卡片都攒在内存里
        if (!enabled() || !foreground()) return
        val e = embedder ?: return
        if (!e.available) {
            logUnavailable()
            return
        }
        if (state.offer(items) > 0) ensureWorker()
    }

    /**
     * Rust 排）。
     *
     * 调用的地方：App 启动、回到前台、设置里「记录浏览行为」从关变开。开关关着 / 在后台 / 没模型时什么都不做。
     */
    fun requestBackfill() {
        if (!enabled() || !foreground()) return
        val e = embedder ?: return
        if (!e.available) return
        state.requestBackfill()
        ensureWorker()
    }

    /**
     * **点开过的条目 + 正文加载出来了**：用"标题 + 正文开头"重新算一份，覆盖这条的向量。
     *
     * 正文先折叠空白、**按 token 截到 [DETAIL_MAX_TOKENS]**（不按字数估），格式仍是 `title: … | text: …`；
     * 来源（`favorite` / `history`）在算的时候由 Rust 问库决定；18+ / 开关关着不算；正文空的退回卡片文字；
     * 同一个 model 下同一条只覆盖一次。
     */
    fun offerDetail(item: FeedItem, body: String?) {
        if (item.adult) return
        if (!enabled() || !foreground()) return
        val e = embedder ?: return
        if (!e.available) return
        if (state.offerDetail(item, body)) ensureWorker()
    }

    /** 前后台切换：退到后台**立刻**取消（正在算的那批结果丢掉、不写库）。 */
    fun onForeground(active: Boolean) {
        if (!active) cancel()
    }

    /** 取消在算的当前，清空三个队列（回前台后由 [offer] / [requestBackfill] 重新开始）。 */
    fun cancel() {
        if (stateCreated) state.cancel()
        job?.cancel()
        job = null
    }

    /** 清空统计（设置里"清除行为数据"之后调；库里的行由 `Library.clearExposure` 清）。 */
    fun resetStats() {
        if (stateCreated) state.resetStats()
        count = 0
        computed = 0
        backfilled = 0
        detailed = 0
        avgMs = null
        lastBatchSize = 0
        stats = emptyList()
    }

    /** 重新读库里的条数 / 来源分布（设置页打开时调）。 */
    fun refreshCountAsync() {
        scope.launch {
            count = withContext(io) { library.embeddingCount() }
            stats = withContext(io) { library.embeddingStats() }
        }
    }

    /** 这个构建没带模型时只记一次（别每批都刷屏）。 */
    private fun logUnavailable() {
        if (!unavailableLogged) {
            unavailableLogged = true
            log("[向量] 本地向量现在不可用（没有模型 / 没打开 / NPU 用不了），跳过（不算、不存）")
        }
    }

    private fun ensureWorker() {
        if (job?.isActive == true) return
        if (!enabled() || !foreground()) return
        val e = embedder ?: return
        if (!e.available) return
        job = scope.launch {
            try {
                runWorker(e)
            } finally {
                job = null
            }
        }
    }

    private suspend fun runWorker(embedder: Embedder) {
        while (true) {
            if (!enabled() || !foreground()) {
                state.cancel()
                return
            }
            val step = withContext(io) {
                state.nextStep(now(), modelId, graphLengths(), store)
            }
            when (step.kind) {
                FfiEmbedStepKind.EMPTY -> return
                FfiEmbedStepKind.NEED_BACKFILL -> {
                    // 一次最多取一块（Rust 决定的条数 / 时间窗口）；这一块取空 = 当前不用再问
                    val candidates = withContext(io) {
                        library.embeddingBackfill(step.model, step.sinceMs, step.limit)
                    }
                    state.supplyBackfill(
                        candidates.map { RustEmbed.backfillOf(it.item, it.source, it.at) },
                    )
                }
                FfiEmbedStepKind.PREPARE -> {
                    // 按 token 截断（详情正文）+ 数 token（分桶用）：一整批两次宿主调用
                    val texts = ArrayList<String>(step.texts.size)
                    for (i in step.texts.indices) {
                        val raw = step.texts[i]
                        val cut = step.truncateTo[i]
                        texts += if (cut > 0) (embedder.truncateToTokens(raw, cut) ?: raw) else raw
                    }
                    val tokens = embedder.tokenLengths(texts)
                    state.supplyPrep(texts, tokens ?: IntArray(texts.size) { -1 })
                }
                FfiEmbedStepKind.BATCH -> {
                    if (!runBatch(embedder, step)) return
                }
            }
        }
    }

    /**
     * 跑一批：调推理 → 拼平过边界 → Rust 截断 / 归一化 / 落库 → 更新界面状态。
     *
     * 返回 false = 当前结束（条数对不上这种"再拉也没用"的情况）。
     */
    private suspend fun runBatch(embedder: Embedder, step: RustEmbed.Step): Boolean {
        val started = now()
        val vectors = try {
            withContext(work) { embedder.embed(step.texts, step.maxTokens) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            log("[向量] 这一批算失败，跳过：${e::class.simpleName}")
            state.failBatch()
            return true
        }
        // 退到后台 / 被取消：这一批结果直接丢掉（不写库，回前台重新排）
        if (vectors == null) {
            log("[向量] 这一批没有结果，跳过（${step.count} 条）")
            state.failBatch()
            return true
        }
        if (vectors.size != step.count) {
            log("[向量] 返回 ${vectors.size} 条，和送进去的 ${step.count} 条对不上，跳过")
            state.failBatch()
            return true
        }
        val vecDim = vectors.firstOrNull()?.size ?: 0
        if (vecDim == 0 || vectors.any { it.size != vecDim }) {
            log("[向量] 这一批的维数对不上，跳过")
            state.failBatch()
            return true
        }
        val elapsed = now() - started
        // 拼平（一次过边界）：第 i 条在 [i * vecDim, (i + 1) * vecDim)
        val flat = FloatArray(vectors.size * vecDim)
        for (i in vectors.indices) vectors[i].copyInto(flat, i * vecDim)
        val storedModel = embedder.lastEmbedModelId
        val done = withContext(io) {
            state.finishBatch(flat, vecDim, storedModel, now(), store)
        }
        if (done == null) {
            log("[向量] 这一批没有交回去（条数对不上），跳过")
            return true
        }
        computed += done.count
        count += done.count
        lastBatchSize = done.count
        backfilled += done.fromBackfill
        detailed += done.fromDetail
        bumpStats(done.model, done.sources)
        val totalMs = (avgMs ?: 0L) * (computed - done.count) + elapsed
        avgMs = if (computed > 0) totalMs / computed else null
        onBatch?.invoke(
            EmbedBatchDone(embedder.lastEmbedBackend, done.count, elapsed, storedModel, embedder.lastEmbedSeqLen),
        )
        // 只记条数、耗时、后端和图的长度，绝不记内容
        val kind = when (done.kind) {
            "detail" -> "详情正文覆盖"
            "backfill" -> "补算"
            else -> ""
        }
        val seqLen = embedder.lastEmbedSeqLen
        log(
            "[向量] ${if (kind.isEmpty()) "这一批" else "${kind}这一批"} ${done.count} 条，$elapsed ms" +
                "（${embedder.lastEmbedBackend.name}${if (seqLen > 0) " · $seqLen token" else ""}）",
        )
        if (gapMs > 0) delay(gapMs)
        return true
    }

    /** 把这批的 `(model, source)` 并进 [stats]（省一次查库；[refreshCountAsync] 会整体重读）。 */
    private fun bumpStats(model: String, sources: List<String>) {
        if (model.isEmpty() || sources.isEmpty()) return
        val current = stats.associateBy { it.model to it.source }.toMutableMap()
        for (source in sources) {
            val k = model to source
            current[k] = EmbeddingStat(model, source, (current[k]?.count ?: 0) + 1)
        }
        stats = current.values.sortedWith(compareBy({ it.model }, { it.source }))
    }

}

/**
 * 一批向量算完的账（性能记录口径：后端 / 条数 / 毫秒数 / 模型标识 / 这张图的长度）。
 *
 * **没有标题、正文、URL**（和 `PerfHooks` 的隐私口径一致）。
 */
data class EmbedBatchDone(
    val backend: EmbedBackend,
    val count: Int,
    val dMs: Long,
    val model: String,
    /** 这一批用的是哪张固定长度的图（token 数；0 = CPU / 不知道）。 */
    val len: Int = 0,
)
