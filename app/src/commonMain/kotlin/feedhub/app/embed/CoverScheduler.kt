package feedhub.app.embed

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.library.EMBEDDING_DIM
import feedhub.app.library.EMBEDDING_SOURCE_FEED
import feedhub.app.library.EmbeddingStat
import feedhub.app.library.Library
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
import uniffi.feedhub_ffi.FfiCoverStatusKind

/**
 * 封面向量的状态（设置页「封面向量」那一行按它显示）。
 *
 * 和 [NpuStatus] 分开：封面向量是**后加的、只算不排的一条旁路**，它自己坏了 / 没带图，
 * 都不能让纯文字向量那条显示成"不可用"。
 */
sealed interface CoverStatus {
    /** 这个构建没带两张图（普通包 / 桌面）。 */
    data object NoModel : CoverStatus

    /** 「记录浏览行为」关着（封面向量跟着它走，和纯文字一样）。 */
    data object Off : CoverStatus

    /** 队列空：没有要算的。 */
    data object Idle : CoverStatus

    /** 在算：这个窗口第几条 / 一共几条。 */
    data class Running(val done: Int, val total: Int) : CoverStatus

    /** 当前里用不了了（QNN 6001 / 编图失败 / 这个包没带图）：**不影响纯文字向量**，下次启动再试。 */
    data class Unavailable(val reason: String) : CoverStatus
}

/** 设置页那一行的短标签（Rust 给措辞，这里只做映射）。 */
val CoverStatus.label: String
    get() = when (this) {
        CoverStatus.NoModel -> "没带图"
        CoverStatus.Off -> "关着"
        CoverStatus.Idle -> "空闲"
        is CoverStatus.Running -> "算封面 $done/$total"
        is CoverStatus.Unavailable -> "本次不可用"
    }

/**
 * 后台算**封面向量**。
 *
 * 一句话：**前台、纯文字闲着时，一批一个"封面窗口"地用两张 HTP 图**（视觉 → 文本+图），算完存库，
 * 排序权重仍然是 0。和纯文字那条（[EmbeddingScheduler]）并存、互不替换。
 *
 * 这个类只剩协程循环 + 三件宿主的事：
 * **取图**（Coil 缓存 / 计费网络判断）、**推理**（两张 HTP 图，会话常驻）、**前后台**。
 *
 * 节制条件（Rust 执行，每一条都有理由）：
 * - **不等充电**；
 * - **一个窗口 [windowSize] 条**（默认 32）：视觉图 batch 固定成 1，一串 32 张 ≈ 27 s；
 * - **纯文字优先**：有纯文字批在算 / 排在队列里时封面等着（[textIdle]）；
 * - **图片取不到就跳过**（下次再试；连续 4 次丢掉）；
 * - 受「记录浏览行为」管、18+ 不算、已经有封面向量的不算；
 * - 失败兜底：`embed` 整体失败 → 封面口径**本次进程不可用**，不再重试，**纯文字向量一点不受影响**。
 *
 * 线程：Compose 状态只在 [scope] 的线程上改；查库 / 取图 / 推理在后台。
 *
 * @param embedder null 或 [CoverEmbedder.bundled] = false 时整条旁路是"安静地没有"。
 * @param images 封面图来源（Coil 缓存 + App 的图片客户端）；null = 取不到图（桌面 / 测试）。
 */
class CoverVectorScheduler(
    private val scope: CoroutineScope,
    private val library: Library,
    private val embedder: CoverEmbedder?,
    private val images: CoverImageSource?,
    /** 设置里的「记录浏览行为」；关着时不算、不存。 */
    private val enabled: () -> Boolean = { true },
    /** App 在不在前台（退到后台立刻停）。 */
    private val foreground: () -> Boolean = { true },
    /** 纯文字那条闲不闲（有批在算 / 队列里还有东西就不是）。 */
    private val textIdle: () -> Boolean = { true },
    private val windowSize: Int = 32,
    /** 队列上限（首页刷很久也只有这么多个待算，超了就丢最老的）。 */
    private val maxQueued: Int = 96,
    private val dim: Int = EMBEDDING_DIM,
    private val now: () -> Long = System::currentTimeMillis,
    private val log: (String) -> Unit = {},
    private val io: CoroutineContext = Dispatchers.IO,
    private val work: CoroutineContext = Dispatchers.Default,
    /** 一个窗口算完调一次（性能记录用：条数 / 两段耗时 / 编了几张图；**不含内容**）。 */
    private val onWindow: ((CoverWindowStats) -> Unit)? = null,
    /** 一轮最多盯多久（纯文字一直忙时）：到点就歇着，等下一次叫醒（默认半小时）。 */
    private val maxWaitMs: Long = 30 * 60_000L,
) {
    /** 队列 / 窗口 / 状态机（Rust）。 */
    private val state = RustEmbed.CoverSchedulerState(
        RustEmbed.coverConfig(
            windowSize = windowSize,
            maxQueued = maxQueued,
            dim = dim,
            maxWaitMs = maxWaitMs,
        ),
    )

    private val store = LibraryEmbedStore(library)

    /** 写进 `item_embedding.model` 的标识（`…-htp-l128-img8x8`）。 */
    val modelId: String get() = embedder?.modelId ?: ""

    /** 这个构建带没带两张图。 */
    val available: Boolean get() = embedder?.bundled == true

    /** 库里现在有多少条**封面**向量（设置页"已算 N 条"）。 */
    var stored: Int by mutableIntStateOf(0)
        private set

    /** 库里封面向量按来源的分布（和纯文字那张同一个口径）。 */
    var stats: List<EmbeddingStat> by mutableStateOf(emptyList())
        private set

    /** 队列里还有多少条待算（设置页"待算 N 条"）。 */
    var pending: Int by mutableIntStateOf(0)
        private set

    /** 这个进程算成了多少条。 */
    var computed: Int by mutableIntStateOf(0)
        private set

    /** 这个进程因为**取不到图**跳过了多少次（下次再试）。 */
    var skipped: Int by mutableIntStateOf(0)
        private set

    /** 最近一个窗口的账。 */
    var lastWindow: CoverWindowStats? by mutableStateOf(null)
        private set

    /** 最近平均每条多少毫秒（这个进程）。 */
    var avgMs: Long? by mutableStateOf(null)
        private set

    /** 现在在干什么（设置页那一行）。 */
    var status: CoverStatus by mutableStateOf(initialStatus())
        private set

    private var job: Job? = null

    /** 这个窗口要送进 `embed` 的输入（逐条取图时攒起来；窗口结束清空）。 */
    private val inputs = ArrayList<CoverInput>()

    /** 最近一次 `embed` 的返回（`null` = 整个窗口失败）。 */
    private var vectorsOut: List<FloatArray?>? = null

    /** 这个进程里的失败原因（null = 还好；**由 Rust 记**，这里只给 `ensureWorker` 挡一下）。 */
    private val broken: String? get() = if (state.isBroken()) "本次进程不可用" else null

    init {
        refreshAsync()
    }

    private fun initialStatus(): CoverStatus = when {
        embedder == null || !embedder.bundled -> CoverStatus.NoModel
        !enabled() -> CoverStatus.Off
        else -> CoverStatus.Idle
    }

    /** 重新读库里封面向量的条数 / 来源分布（设置页打开时调）。 */
    fun refreshAsync() {
        scope.launch {
            val id = modelId
            if (id.isEmpty()) return@launch
            val all = withContext(io) { library.embeddingStats() }
            stats = all.filter { it.model == id }
            stored = stats.sumOf { it.count }
        }
    }

    /**
     * 首页 / 关注来了新卡片：把够条件的排进队列（**只在内存里排队，不建会话、不取图**）。
     *
     * 条件在 Rust（[FeedItem.hasCoverVector]：平台在表里、有封面图、不是 18+、有文字）。
     */
    fun offer(items: List<FeedItem>) {
        if (items.isEmpty()) return
        if (!enabled()) return
        if (embedder == null || !embedder.bundled) return
        if (broken != null) return
        if (state.offer(items) > 0) {
            pending = state.pending()
            ensureWorker()
        }
    }

    /**
     * 前后台切换：退到后台**立刻**停（正在算的那个窗口结果丢掉、不写库），但**队列留着**。
     */
    fun onForeground(active: Boolean) {
        if (!active) {
            job?.cancel()
            job = null
            state.resetStatus(embedder?.bundled == true, enabled())
            status = state.status().toCoverStatus()
            pending = state.pending()
            return
        }
        if (state.pending() > 0) ensureWorker()
    }

    /** 开关打开 / 回前台：叫醒一次。 */
    fun poke() {
        if (!enabled() || !foreground()) return
        if (state.pending() == 0) return
        ensureWorker()
    }

    /** 取消当前、清空队列（回前台后由 [offer] 重新开始）。 */
    fun cancel() {
        state.cancel()
        pending = 0
        job?.cancel()
        job = null
        state.resetStatus(embedder?.bundled == true, enabled())
        status = state.status().toCoverStatus()
    }

    /** 清空统计（设置里"清除行为数据"之后调；库里的行由 `Library.clearExposure` 一起清）。 */
    fun resetStats() {
        state.resetStats()
        stored = 0
        computed = 0
        skipped = 0
        avgMs = null
        lastWindow = null
        stats = emptyList()
    }

    private fun ensureWorker() {
        if (job?.isActive == true) return
        if (!enabled() || !foreground()) return
        if (embedder == null || !embedder.bundled) return
        if (broken != null) return
        if (state.pending() == 0) return
        job = scope.launch {
            try {
                runWorker()
            } finally {
                job = null
            }
        }
    }

    /** 一轮的输入（每步现读：设置 / 前后台随时会变）。 */
    private fun flags(): RustEmbed.WindowFlags = RustEmbed.WindowFlags(
        enabled = enabled(),
        foreground = foreground(),
        textIdle = textIdle(),
        bundled = embedder?.bundled == true,
    )

    /**
     * 跑一轮（**顺序和分支都在 Rust**：`feedhub-core/src/embed/cover_schedule.rs` 的 `window_step`）。
     *
     * 这个循环只剩三件事：拉一步、执行那一步的框架调用、把结果报回去。判定（先看开关还是先看纯文字、
     * 什么时候 abort、图取不到怎么记账、窗口的账怎么摊）全在 Rust。
     */
    private suspend fun runWorker() {
        val e = embedder ?: return
        while (true) {
            when (val step = state.windowStep(flags())) {
                is RustEmbed.WindowStep.Stop -> {
                    // 开关关了：常驻的两张会话也放掉（退后台由 AppGraph 关）
                    if (!enabled()) runCatching { e.close() }
                    flushLogs()
                    publish()
                    return
                }
                is RustEmbed.WindowStep.Continue -> {
                    flushLogs()
                    publish()
                }
                is RustEmbed.WindowStep.Wait -> {
                    flushLogs()
                    publish()
                    if (step.stop) return
                    delay(step.delayMs)
                }
                is RustEmbed.WindowStep.BeginWindow -> {
                    withContext(io) { state.beginWindow(e.modelId, store) }
                    val next = state.reportPlan(e.modelId)
                    flushLogs()
                    publish()
                    if (next is RustEmbed.WindowStep.Continue) continue
                }
                is RustEmbed.WindowStep.LoadImage -> {
                    val urls = coverImageUrlsOf(step.coverUrl)
                    val bytes = if (urls.isEmpty()) null else images?.load(urls)
                    val found = bytes != null && bytes.isNotEmpty()
                    if (found) inputs += CoverInput(step.text, bytes)
                    val next = state.reportImage(flags(), step.key, found)
                    skipped = state.skipped()
                    flushLogs()
                    publish()
                    if (next is RustEmbed.WindowStep.Continue) {
                        inputs.clear()
                        continue
                    }
                }
                is RustEmbed.WindowStep.Prepare -> {
                    val failure = try {
                        e.prepare()
                    } catch (err: CancellationException) {
                        throw err
                    } catch (err: Throwable) {
                        "${err::class.simpleName}: ${err.message}"
                    }
                    val next = state.reportPrepare(failure)
                    flushLogs()
                    publish()
                    if (next is RustEmbed.WindowStep.Stop) return
                }
                is RustEmbed.WindowStep.Embed -> {
                    val vectors = try {
                        withContext(work) { e.embed(inputs.toList()) }
                    } catch (err: CancellationException) {
                        throw err
                    } catch (err: Throwable) {
                        log("[封面] 这个窗口算失败：${err::class.simpleName}: ${err.message}")
                        null
                    }
                    vectorsOut = vectors
                    val next = state.reportEmbed(vectors == null, vectors?.size ?: 0)
                    flushLogs()
                    publish()
                    if (next is RustEmbed.WindowStep.Stop) return
                }
                is RustEmbed.WindowStep.Finish -> {
                    finishWindow(e)
                    val next = state.reportFinished()
                    flushLogs()
                    publish()
                    if (next is RustEmbed.WindowStep.Stop) return
                }
            }
        }
    }

    /** 写库 + 记账（窗口结束；会话常驻，不在这里关——由实现自己按常驻 / 分时决定）。 */
    private suspend fun finishWindow(e: CoverEmbedder) {
        val vectors = vectorsOut
        vectorsOut = null
        val keys = state.windowKeys()
        inputs.clear()
        val results = ArrayList<Pair<String, FloatArray>>(keys.size)
        if (vectors != null && vectors.size == keys.size) {
            for (i in keys.indices) {
                val vec = vectors[i] ?: continue
                results += keys[i] to vec
            }
        }
        val written = withContext(io) { state.finishWindow(results, e.modelId, now(), store) }
        computed = state.computed()
        stored += written
        if (written > 0) bumpStats(e.modelId, written)
        status = state.status().toCoverStatus()
        pending = state.pending()
        val stats = e.lastWindow
        if (stats != null) {
            lastWindow = stats
            state.observeWindow(
                RustEmbed.CoverWindowStatsRaw(
                    stats.count,
                    stats.visionImages,
                    stats.visionMs,
                    stats.textBatches,
                    stats.textMs,
                    stats.totalMs,
                    stats.compiled,
                ),
            )
            avgMs = state.avgMs()
            onWindow?.invoke(stats)
            log(
                "[封面] 这个窗口 ${stats.count} 条：视觉 ${stats.visionImages} 张 ${stats.visionMs} ms、" +
                    "文本+图 ${stats.textBatches} 批 ${stats.textMs} ms，合计 ${stats.totalMs} ms" +
                    "（现编 ${stats.compiled} 张图）",
            )
        }
    }

    /** 把 Rust 那边的问题日志接到 App 的日志口（**不含内容**）。 */
    private fun flushLogs() {
        for (line in state.drainLogs()) log(line)
    }

    /** 把 Rust 的状态 / 计数刷到 Compose 字段。 */
    private fun publish() {
        status = state.status().toCoverStatus()
        pending = state.pending()
        skipped = state.skipped()
        computed = state.computed()
    }

    /** 封面图地址：Rust 给原地址，宽度档 → 缩略图地址的改写走图片层唯一那份（`CardThumbnails`，规则也在 Rust）。 */
    private fun coverImageUrlsOf(raw: String): List<String> =
        feedhub.app.feed.CardThumbnails.ofWidths(raw, CoverRules.CACHE_WIDTHS)

    private fun bumpStats(model: String, written: Int) {
        val k = model to EMBEDDING_SOURCE_FEED
        val current = stats.associateBy { it.model to it.source }.toMutableMap()
        current[k] = EmbeddingStat(model, EMBEDDING_SOURCE_FEED, (current[k]?.count ?: 0) + written)
        stats = current.values.sortedWith(compareBy({ it.model }, { it.source }))
    }
}

/** Rust 的状态 → Kotlin 的密封接口（设置页用）。 */
internal fun RustEmbed.CoverStatus.toCoverStatus(): CoverStatus = when (kind) {
    FfiCoverStatusKind.NO_MODEL -> CoverStatus.NoModel
    FfiCoverStatusKind.OFF -> CoverStatus.Off
    FfiCoverStatusKind.IDLE -> CoverStatus.Idle
    FfiCoverStatusKind.RUNNING -> CoverStatus.Running(done, total)
    FfiCoverStatusKind.UNAVAILABLE -> CoverStatus.Unavailable(reason ?: "本次不可用")
}
