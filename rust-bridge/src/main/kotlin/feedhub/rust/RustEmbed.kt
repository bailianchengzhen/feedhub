package feedhub.rust

import feedhub.core.FeedItem
import feedhub.core.Media
import uniffi.feedhub_ffi.FfiBatchAccount
import uniffi.feedhub_ffi.FfiCoverConfig
import uniffi.feedhub_ffi.FfiCoverPick
import uniffi.feedhub_ffi.FfiCoverScheduler
import uniffi.feedhub_ffi.FfiCoverStatus
import uniffi.feedhub_ffi.FfiCoverStatusKind
import uniffi.feedhub_ffi.FfiCoverVector
import uniffi.feedhub_ffi.FfiCoverWait
import uniffi.feedhub_ffi.FfiCoverWaitKind
import uniffi.feedhub_ffi.FfiCoverWindowStats
import uniffi.feedhub_ffi.FfiWindowFlags
import uniffi.feedhub_ffi.FfiEmbedBackfill
import uniffi.feedhub_ffi.FfiEmbedBlockedAuthor
import uniffi.feedhub_ffi.FfiEmbedConfig
import uniffi.feedhub_ffi.FfiEmbedExposure
import uniffi.feedhub_ffi.FfiEmbedFavorite
import uniffi.feedhub_ffi.FfiEmbedHistory
import uniffi.feedhub_ffi.FfiEmbedItem
import uniffi.feedhub_ffi.FfiEmbedNext
import uniffi.feedhub_ffi.FfiEmbedRow
import uniffi.feedhub_ffi.FfiEmbedScheduler
import uniffi.feedhub_ffi.FfiEmbedSeen
import uniffi.feedhub_ffi.FfiEmbedSignal
import uniffi.feedhub_ffi.FfiEmbedSnapshot
import uniffi.feedhub_ffi.FfiEmbedStep
import uniffi.feedhub_ffi.FfiEmbedStepKind
import uniffi.feedhub_ffi.FfiEmbedStore
import uniffi.feedhub_ffi.FfiEmbedRun
import uniffi.feedhub_ffi.FfiEmbedRunKind
import uniffi.feedhub_ffi.FfiExportExposure
import uniffi.feedhub_ffi.FfiExportFavorite
import uniffi.feedhub_ffi.FfiExportHistory
import uniffi.feedhub_ffi.FfiExportInput
import uniffi.feedhub_ffi.FfiExportOutput
import uniffi.feedhub_ffi.FfiExportSeen
import uniffi.feedhub_ffi.FfiNpuStatus
import uniffi.feedhub_ffi.FfiNpuStatusKind
import uniffi.feedhub_ffi.FfiPrepareKind
import uniffi.feedhub_ffi.FfiPrepareStep
import uniffi.feedhub_ffi.FfiRouter
import uniffi.feedhub_ffi.FfiRouterConfig
import uniffi.feedhub_ffi.FfiTokenSourceKind
import uniffi.feedhub_ffi.FfiTasteModelVectors
import uniffi.feedhub_ffi.FfiTasteProfile
import uniffi.feedhub_ffi.FfiTasteVector

/**
 * 本地向量那一层的 Rust 门面。
 *
 * `rust/crates/feedhub-core/src/embed/` 里是全部决策 / 数学 / 状态机 / 导出格式；这里只做**类型转换**：
 * - `FeedItem` → `FfiEmbedItem`（算向量只用到十几个小字段，一次 offer 一批卡片，别把整条 `FeedItem` 搬过去）；
 * - 过完边界再还原成业务代码认识的类型（枚举名 / `FloatArray` / `List<String>`）；
 * - 状态机对象（[RustEmbedScheduler] / [RustCoverScheduler]）包一层，`.so` 的加载只在这一层做（[RustNative]）。
 *
 * 和别的门面一样：**没有开关、没有 Kotlin 备用实现**。`.so` 加载不了就抛一句说得清的话（[RustNative]）。
 * Kotlin 原来的实现只在 `app` 的 desktopTest 里当对拍参照（`feedhub.app.embed.legacy`）。
 */
object RustEmbed {
    // ---------------------------------------------------------------- 卡片

    /**
     * 一条卡片的投影（Rust 只认这个）。
     *
     * `key` = `platform:id`；
     * 媒体只留两个"封面候选"：视频封面 + 正文第一张图（多图只取第一张，规则在 Rust）。
     */
    fun itemOf(item: FeedItem): FfiEmbedItem = FfiEmbedItem(
        key = item.key,
        id = item.id,
        platform = item.platform.key,
        kind = item.kind.name,
        publishedAt = item.publishedAt,
        language = item.language,
        authorName = item.author?.name,
        adult = item.adult,
        title = item.title,
        summary = item.summary,
        community = item.community,
        reason = item.reason,
        authorId = item.author?.id,
        repostTitle = item.repost?.title,
        repostSummary = item.repost?.summary,
        videoCover = item.media.filterIsInstance<Media.Video>().firstOrNull()?.cover,
        firstImage = item.media.filterIsInstance<Media.Image>().firstOrNull()?.url,
    )

    fun itemsOf(items: List<FeedItem>): List<FfiEmbedItem> = items.map(::itemOf)

    /** 卡片 → 送去算向量的那段文字（`title: … | text: …`）。 */
    fun embeddingText(item: FeedItem, maxSummaryChars: Int = MAX_SUMMARY_CHARS): String = load {
        uniffi.feedhub_ffi.ffiEmbedText(itemOf(item), maxSummaryChars)
    }

    /** 详情正文的版本（正文折叠空白、按字符兜底砍到 [maxChars]，token 截断由分词器做）。 */
    fun detailEmbeddingText(item: FeedItem, body: String?, maxChars: Int = DETAIL_MAX_CHARS): String = load {
        uniffi.feedhub_ffi.ffiEmbedDetailText(itemOf(item), body, maxChars)
    }

    /** 这条卡片值不值得算向量（18+ / 没有文字都不算）。 */
    fun hasEmbeddingText(item: FeedItem): Boolean = load {
        uniffi.feedhub_ffi.ffiEmbedHasText(itemOf(item))
    }

    /** 空白折叠（口径和 Rust 一致；app 里只在需要"和向量同一份文字"时用）。 */
    fun collapseWhitespace(value: String): String = load { uniffi.feedhub_ffi.ffiEmbedCollapse(value) }

    // ---------------------------------------------------------------- 数学

    /** MRL：截前 [dim] 维 + 重新 L2 归一化。 */
    fun mrl(vec: FloatArray, dim: Int): FloatArray = load {
        uniffi.feedhub_ffi.ffiEmbedMrl(vec.toList(), dim).toFloatArray()
    }

    /** L2 归一化（返回新数组）。 */
    fun l2Normalize(vec: FloatArray): FloatArray = load {
        uniffi.feedhub_ffi.ffiEmbedL2Normalize(vec.toList()).toFloatArray()
    }

    /** 两个已归一化向量的余弦（= 点积，公共前缀）。 */
    fun cosine(a: FloatArray, b: FloatArray): Float = load {
        uniffi.feedhub_ffi.ffiEmbedCosine(a.toList(), b.toList())
    }

    /** 一整个向量 → f16 字节（小端）。 */
    fun halfBytes(vec: FloatArray): ByteArray = load {
        uniffi.feedhub_ffi.ffiEmbedHalfBytes(vec.toList())
    }

    /** f16 字节 → 一整个向量（长度不对返回 null）。 */
    fun vecFromHalf(bytes: ByteArray, dim: Int): FloatArray? = load {
        uniffi.feedhub_ffi.ffiEmbedVecFromHalf(bytes, dim)?.toFloatArray()
    }

    /** f16 字节里的第 [index] 个分量（单点读取；整条用 [vecFromHalf]）。 */
    fun halfAt(bytes: ByteArray, index: Int): Float? = load {
        uniffi.feedhub_ffi.ffiEmbedHalfAt(bytes, index)
    }

    // ---------------------------------------------------------------- 常量（Rust 是唯一真相）

    /** 存多少维（256）。 */
    val dim: Int by lazy { load { uniffi.feedhub_ffi.ffiEmbedDim() } }

    /** 当前写进 `item_embedding.model` 的默认标识（CPU 的 q4 图）。 */
    val defaultModel: String by lazy { load { uniffi.feedhub_ffi.ffiEmbedDefaultModel() } }

    /** 详情正文最多算到几个 token（128）。 */
    val detailMaxTokens: Int by lazy { load { uniffi.feedhub_ffi.ffiEmbedDetailMaxTokens() } }

    /** NPU 纯文字向量的空间标识。 */
    fun npuModelId(seqLen: Int): String = load { uniffi.feedhub_ffi.ffiEmbedNpuModelId(seqLen) }

    /** 封面向量的空间标识。 */
    fun coverModelId(seqLen: Int): String = load { uniffi.feedhub_ffi.ffiEmbedCoverModelId(seqLen) }

    /** 三种来源（`feed` / `favorite` / `history`）。 */
    val sourceFeed: String by lazy { load { uniffi.feedhub_ffi.ffiEmbedSourceFeed() } }
    val sourceFavorite: String by lazy { load { uniffi.feedhub_ffi.ffiEmbedSourceFavorite() } }
    val sourceHistory: String by lazy { load { uniffi.feedhub_ffi.ffiEmbedSourceHistory() } }

    const val MAX_SUMMARY_CHARS: Int = 200
    const val DETAIL_MAX_TOKENS: Int = 128
    const val DETAIL_MAX_CHARS: Int = 1200

    // ---------------------------------------------------------------- 行为信号

    /** 一种信号的定义（Rust 那份表：名字 / 权重 / 半衰期）。 */
    data class SignalKindDef(
        val name: String,
        val label: String,
        val baseWeight: Float,
        val halfLifeDays: Double,
    )

    fun signalKinds(): List<SignalKindDef> = load {
        uniffi.feedhub_ffi.ffiEmbedSignalKinds().map {
            SignalKindDef(it.name, it.label, it.baseWeight, it.halfLifeDays)
        }
    }

    /** 一条信号（`kind` 是 Kotlin 枚举名，`weight` 是**这次 `now`** 的权重）。 */
    data class Signal(
        val key: String,
        val kind: String,
        val at: Long,
        val strength: Float,
        val platform: String?,
        val weight: Float,
    )

    /** 把一份快照折成信号（Rust 里算完、排好序、**把权重也算好**）。 */
    fun signals(snapshot: FfiEmbedSnapshot, now: Long): List<Signal> = load {
        uniffi.feedhub_ffi.ffiEmbedSignals(snapshot, now).map {
            Signal(it.key, it.kind, it.at, it.strength, it.platform, it.weight)
        }
    }

    /** 每种信号各几条（固定 8 项，没出现的算 0）。 */
    fun signalCounts(snapshot: FfiEmbedSnapshot): Map<String, Int> = load {
        uniffi.feedhub_ffi.ffiEmbedSignalCounts(snapshot).associate { it.kind to it.count }
    }

    /**
     * **只数种类**：宿主已经有信号列表时用这个，别为了计数把整份快照再过一遍边界。
     *
     * 返回固定 8 项、按 Rust 枚举顺序（`FAVORITE` / `VISIT` / …）；不认识的种类名忽略。
     */
    fun countKinds(kinds: List<String>): List<Pair<String, Int>> = load {
        uniffi.feedhub_ffi.ffiEmbedCountKinds(kinds).map { it.kind to it.count }
    }

    /** 一条信号是不是正向（权重 > 0）——正负口径只有 Rust 一份。 */
    fun signalIsPositive(kind: String): Boolean = load {
        uniffi.feedhub_ffi.ffiEmbedSignalIsPositive(kind)
    }

    /** 全部信号种类（Rust 枚举顺序）。 */
    val signalNames: List<String> by lazy { load { uniffi.feedhub_ffi.ffiEmbedSignalNames() } }

    /** 一条信号的权重。 */
    fun signalWeight(kind: String, strength: Float, at: Long, now: Long): Float = load {
        uniffi.feedhub_ffi.ffiEmbedSignalWeight(kind, strength, at, now)
            ?: error("不认识的信号种类：$kind")
    }

    /** 时间衰减 `0.5 ^ (age / 半衰期)`。 */
    fun signalDecay(ageMs: Long, halfLifeDays: Double): Float = load {
        uniffi.feedhub_ffi.ffiEmbedSignalDecay(ageMs, halfLifeDays)
    }

    // ---------------------------------------------------------------- 口味画像

    /** 一个模型的口味画像（原始 `FloatArray`，不做 Kotlin 那边的包装）。 */
    data class Profile(
        val model: String,
        val positive: FloatArray?,
        val negative: FloatArray?,
        val positiveWeight: Float,
        val negativeWeight: Float,
        val positiveSignals: Int,
        val negativeSignals: Int,
    ) {
        val usable: Boolean get() = positive != null

        fun similarity(vec: FloatArray?, model: String?, negativePenalty: Float): Float? = load {
            val raw = FfiTasteProfile(
                this.model,
                this.positive?.toList(),
                this.negative?.toList(),
                this.positiveWeight,
                this.negativeWeight,
                this.positiveSignals,
                this.negativeSignals,
            )
            uniffi.feedhub_ffi.ffiTasteSimilarity(raw, vec?.toList(), model, negativePenalty)
        }
    }

    /** 从信号 + 向量造一个模型的口味画像。 */
    fun tasteProfile(signals: List<Signal>, model: String, vectors: Map<String, FloatArray>, now: Long): Profile =
        load {
            val rows = signals.mapNotNull { s -> vectors[s.key]?.let { FfiTasteVector(s.key, it.toList()) } }
            profileOf(uniffi.feedhub_ffi.ffiTasteProfile(signals.map(::rawSignal), model, rows, now))
        }

    /** 每个模型各造一份画像。 */
    fun tasteProfiles(signals: List<Signal>, vectorsByModel: Map<String, Map<String, FloatArray>>, now: Long): Map<String, Profile> =
        load {
            val rows = vectorsByModel.map { (model, vectors) ->
                FfiTasteModelVectors(model, vectors.map { (key, vec) -> FfiTasteVector(key, vec.toList()) })
            }
            uniffi.feedhub_ffi.ffiTasteProfiles(signals.map(::rawSignal), rows, now)
                .associate { it.model to profileOf(it) }
        }

    /** 负向中心的排斥强度（固定设计参数）。 */
    val negativePenalty: Float by lazy { load { uniffi.feedhub_ffi.ffiTasteNegativePenalty() } }

    private fun profileOf(p: FfiTasteProfile) = Profile(
        p.model,
        p.positive?.toFloatArray(),
        p.negative?.toFloatArray(),
        p.positiveWeight,
        p.negativeWeight,
        p.positiveSignals,
        p.negativeSignals,
    )

    private fun rawSignal(s: Signal) = FfiEmbedSignal(s.key, s.kind, s.at, s.strength, s.platform, s.weight)

    // ---------------------------------------------------------------- 封面

    /** 这个平台算不算封面向量。 */
    fun coverCounts(platform: String): Boolean = load { uniffi.feedhub_ffi.ffiCoverCounts(platform) }

    /** 表里所有"算"的平台 id。 */
    fun coverPlatforms(): List<String> = load { uniffi.feedhub_ffi.ffiCoverPlatforms() }

    /** 封面向量规则表（平台 id → 算不算；`false` 的也在，测试要断言"每个平台都有一行"）。 */
    fun coverPlatformTable(): Map<String, Boolean> = load {
        uniffi.feedhub_ffi.ffiCoverPlatformTable().associate { it.platform to it.counts }
    }

    /** 这条内容的封面原地址（视频封面优先，然后正文第一张图）。 */
    fun coverUrl(item: FeedItem): String? = load { uniffi.feedhub_ffi.ffiCoverUrl(itemOf(item)) }

    /** 这条内容值不值得算封面向量。 */
    fun coverHas(item: FeedItem): Boolean = load { uniffi.feedhub_ffi.ffiCoverHas(itemOf(item)) }

    /** 封面向量要送的那段文字。 */
    fun coverText(item: FeedItem): String = load { uniffi.feedhub_ffi.ffiCoverText(itemOf(item)) }

    /** 缩略图宽度档（从大到小：640 起）。 */
    val coverCacheWidths: IntArray by lazy { load { uniffi.feedhub_ffi.ffiCoverCacheWidths().toIntArray() } }

    val coverInputSide: Int by lazy { load { uniffi.feedhub_ffi.ffiCoverInputSide() } }
    val coverImageTokens: Int by lazy { load { uniffi.feedhub_ffi.ffiCoverImageTokens() } }
    val coverTextTokens: Int by lazy { load { uniffi.feedhub_ffi.ffiCoverTextTokens() } }

    /** 384×384 的 ARGB 像素 → `pixel_values [1, 576, 768]`（尺寸不对抛 `IllegalArgumentException`）。 */
    fun coverPatchify(argb: IntArray, side: Int): FloatArray = load {
        try {
            uniffi.feedhub_ffi.ffiCoverPatchify(argb.toList(), side).toFloatArray()
        } catch (e: uniffi.feedhub_ffi.FfiEmbedException) {
            throw IllegalArgumentException(e.message, e)
        }
    }

    // ---------------------------------------------------------------- 后端选择（CPU / NPU 的判定）

    // 现在那五个判定都收进了
    // [Router] 的状态机（`feedhub-core/src/embed/router.rs`），宿主不再自己拼——所以门面也删了。

    /** 等"能编图"的上限。 */
    const val NPU_MAX_WAIT_MS: Long = 120_000L

    /** 等"能编图" / 等 NPU 就绪的间隔。 */
    const val NPU_POLL_MS: Long = 300L

    // ---------------------------------------------------------------- 后端路由状态机

    /**
     * **CPU / NPU 的后端路由状态机**（`RoutingEmbedder` 的判定与状态）。
     *
     * 全部判定在 Rust `embed::router::Router`；这个类只把 `Ffi*` 翻成 Kotlin 认的密封类型，
     * 生产的 `NpuBackendTest` 一条不改就是它的行为契约。
     */
    class Router(
        npuLengths: List<Int>,
        spaceLen: Int,
        hasCpu: Boolean,
        cpuPresent: Boolean,
        cpuModel: String,
        npuSpaceModel: String,
    ) {
        private val native: FfiRouter = load {
            FfiRouter(
                FfiRouterConfig(
                    npuLengths = npuLengths,
                    spaceLen = spaceLen,
                    hasCpu = hasCpu,
                    cpuPresent = cpuPresent,
                    cpuModel = cpuModel,
                    npuSpaceModel = npuSpaceModel,
                    waitStepMs = NPU_POLL_MS,
                    maxWaitMs = NPU_MAX_WAIT_MS,
                    readyPollMs = NPU_POLL_MS,
                ),
            )
        }

        // ---------------- 只读

        fun status(): RouterStatus = load {
            val raw: FfiNpuStatus = native.status()
            RouterStatus(
                kind = when (raw.kind) {
                    FfiNpuStatusKind.NO_MODEL -> RouterStatusKind.NoModel
                    FfiNpuStatusKind.OFF -> RouterStatusKind.Off
                    FfiNpuStatusKind.DISABLED -> RouterStatusKind.Disabled
                    FfiNpuStatusKind.UNAVAILABLE -> RouterStatusKind.Unavailable
                    FfiNpuStatusKind.PREPARING -> RouterStatusKind.Preparing
                    FfiNpuStatusKind.ACTIVE -> RouterStatusKind.Active
                    FfiNpuStatusKind.FALLBACK -> RouterStatusKind.Fallback
                },
                detail = raw.detail,
                lens = raw.lens,
                reason = raw.reason,
            )
        }

        fun npuBundled(): Boolean = load { native.npuBundled() }

        fun cpuBundled(): Boolean = load { native.cpuBundled() }

        fun npuLengths(): List<Int> = load { native.npuLengths() }

        fun readyLengths(): List<Int> = load { native.readyLengths() }

        fun available(): Boolean = load { native.available() }

        fun modelId(): String = load { native.modelId() }

        fun isBroken(): Boolean = load { native.isBroken() }

        fun requested(): Boolean = load { native.requested() }

        fun lastBatch(): BatchAccount = load {
            val raw: FfiBatchAccount = native.lastBatch()
            BatchAccount(raw.backend, raw.model, raw.seqLen)
        }

        fun accountCpu(): BatchAccount = load {
            val raw: FfiBatchAccount = native.accountCpu()
            BatchAccount(raw.backend, raw.model, raw.seqLen)
        }

        fun drainLogs(): List<String> = load { native.drainLogs() }

        fun tokenSources(): List<TokenSource> = load {
            native.tokenSources().map { TokenSource(it.kind, it.len) }
        }

        // ---------------- 开关 / 前后台 / 会话

        fun setRequested(on: Boolean): Boolean = load { native.setRequested(on) }

        /** 返回 `true` = 该把准备协程拉起来（回前台、并且还有图没就绪）。 */
        fun onForeground(active: Boolean): Boolean = load { native.onForeground(active) }

        fun closeEngines() = load { native.closeEngines() }

        fun releaseSessions() = load { native.releaseSessions() }

        fun closeSessions() = load { native.closeSessions() }

        // ---------------- 准备

        fun tryStartPrepare(): Boolean = load { native.tryStartPrepare() }

        fun cancelPrepare() = load { native.cancelPrepare() }

        fun prepareCancelled() = load { native.prepareCancelled() }

        fun prepareStep(charging: Boolean): PrepareStep = load {
            val raw: FfiPrepareStep = native.prepareStep(charging)
            when (raw.kind) {
                FfiPrepareKind.WAIT -> PrepareStep.Wait(raw.delayMs)
                FfiPrepareKind.PROBE -> PrepareStep.Probe(raw.len, raw.which)
                FfiPrepareKind.COMPILE -> PrepareStep.Compile(raw.len, raw.which)
                FfiPrepareKind.COMMIT -> PrepareStep.Commit(raw.len)
                FfiPrepareKind.DISCARD -> PrepareStep.Discard(raw.len)
                FfiPrepareKind.FINISHED -> PrepareStep.Finished
            }
        }

        fun reportProbe(reason: String?) = load { native.reportProbe(reason) }

        /** 报完编译结果，返回下一步（通常是 `Commit` / `Discard` / `Finished`）。 */
        fun reportCompile(failure: String?, detail: String): PrepareStep = load {
            val raw: FfiPrepareStep = native.reportCompile(failure, detail)
            when (raw.kind) {
                FfiPrepareKind.WAIT -> PrepareStep.Wait(raw.delayMs)
                FfiPrepareKind.PROBE -> PrepareStep.Probe(raw.len, raw.which)
                FfiPrepareKind.COMPILE -> PrepareStep.Compile(raw.len, raw.which)
                FfiPrepareKind.COMMIT -> PrepareStep.Commit(raw.len)
                FfiPrepareKind.DISCARD -> PrepareStep.Discard(raw.len)
                FfiPrepareKind.FINISHED -> PrepareStep.Finished
            }
        }

        fun commit(len: Int) = load { native.commit(len) }

        // ---------------- 推理

        fun embedStep(maxTokens: Int): EmbedStep = load {
            val raw: FfiEmbedRun = native.embedStep(maxTokens)
            when (raw.kind) {
                FfiEmbedRunKind.EMPTY -> EmbedStep.Empty
                FfiEmbedRunKind.CPU -> EmbedStep.UseCpu(raw.model, raw.seqLen)
                FfiEmbedRunKind.NPU -> EmbedStep.RunNpu(raw.len)
                FfiEmbedRunKind.WAIT_READY -> EmbedStep.WaitReady(raw.delayMs, raw.startPrepare)
            }
        }

        fun reportEmbed(len: Int, ranOk: Boolean) = load { native.reportEmbed(len, ranOk) }

        fun beginCpu() = load { native.beginCpu() }

        fun endCpu() = load { native.endCpu() }
    }

    /** 准备阶段的下一步。 */
    sealed interface PrepareStep {
        data class Wait(val delayMs: Long) : PrepareStep
        data class Probe(val len: Int, val which: String) : PrepareStep
        data class Compile(val len: Int, val which: String) : PrepareStep
        data class Commit(val len: Int) : PrepareStep
        data class Discard(val len: Int) : PrepareStep
        data object Finished : PrepareStep
    }

    /** 一批的下一步。 */
    sealed interface EmbedStep {
        data object Empty : EmbedStep
        data class UseCpu(val model: String, val seqLen: Int) : EmbedStep
        data class RunNpu(val len: Int) : EmbedStep
        data class WaitReady(val delayMs: Long, val startPrepare: Boolean) : EmbedStep
    }

    /** 分词 / 截断该问谁。 */
    data class TokenSource(val kind: FfiTokenSourceKind, val len: Int)

    /** 这一批的账。 */
    data class BatchAccount(val backend: String, val model: String, val seqLen: Int)

    /** 后端路由状态的种类（和 `feedhub.app.embed.NpuStatus` 一一对应；**这个模块不反向依赖 app**）。 */
    enum class RouterStatusKind { NoModel, Off, Disabled, Unavailable, Preparing, Active, Fallback }

    /** 后端路由的状态 + 它带的数字 / 原因。 */
    data class RouterStatus(
        val kind: RouterStatusKind,
        /** `Preparing` / `Active` 的那句说明。 */
        val detail: String,
        /** `Active` 时现在有几张图就绪（升序）。 */
        val lens: List<Int>,
        /** `Disabled` / `Unavailable` / `Fallback` 的原因。 */
        val reason: String,
    )

    // ---------------------------------------------------------------- 行为导出

    /** 导出的结果（行**不带换行**；字节数算上了每行的 `\n`）。 */
    data class Export(val lines: List<String>, val tables: Map<String, Int>, val bytes: Long)

    /** 把库里这几张表拼成 JSON Lines 的全部行。 */
    fun export(input: FfiExportInput): Export = load {
        val out: FfiExportOutput = uniffi.feedhub_ffi.ffiExportBehavior(input)
        Export(out.lines, out.tables.associate { it.table to it.rows }, out.bytes)
    }

    val exportSchemaVersion: Int by lazy { load { uniffi.feedhub_ffi.ffiExportSchemaVersion() } }
    val exportSummaryMaxChars: Int by lazy { load { uniffi.feedhub_ffi.ffiExportSummaryMaxChars() } }

    /** 标准 base64（带 `=` 补齐）。 */
    fun base64(bytes: ByteArray): String = load { uniffi.feedhub_ffi.ffiExportBase64(bytes) }

    // ---------------------------------------------------------------- 导出输入的构造

    fun exportSeen(item: FeedItem, firstSeenAt: Long, feedKind: String, batchId: Long, platformRank: Int) =
        FfiExportSeen(itemOf(item), firstSeenAt, feedKind, batchId, platformRank)

    fun exportExposure(key: String, platform: String, surface: String, exposedAt: Long, lastSeenAt: Long, dwellMs: Long) =
        FfiExportExposure(key, platform, surface, exposedAt, lastSeenAt, dwellMs)

    fun exportHistory(item: FeedItem, visitedAt: Long, progressMs: Long?, durationMs: Long?) =
        FfiExportHistory(itemOf(item), visitedAt, progressMs, durationMs)

    fun exportFavorite(item: FeedItem, folder: String?, addedAt: Long) =
        FfiExportFavorite(itemOf(item), folder, addedAt)

    fun exportBlocked(authorKey: String, blockedAt: Long) = FfiEmbedBlockedAuthor(authorKey, blockedAt)

    /** 一条补算候选（调度器 `NeedBackfill` 的回话）。 */
    fun backfillOf(item: FeedItem, source: String, at: Long) = FfiEmbedBackfill(itemOf(item), source, at)

    fun embeddingRow(key: String, model: String, dim: Int, vec: ByteArray, createdAt: Long, source: String) =
        FfiEmbedRow(key, model, dim, vec, createdAt, source)

    fun exportInput(
        seen: List<FfiExportSeen>,
        exposures: List<FfiExportExposure>,
        history: List<FfiExportHistory>,
        favorites: List<FfiExportFavorite>,
        embeddings: List<FfiEmbedRow>,
        blocked: List<FfiEmbedBlockedAuthor>,
        blockWords: List<String>,
        now: Long,
    ) = FfiExportInput(seen, exposures, history, favorites, embeddings, blocked, blockWords, now)

    // ---------------------------------------------------------------- 快照的构造

    fun snapshot(
        favorites: List<FfiEmbedFavorite>,
        history: List<FfiEmbedHistory>,
        exposures: List<FfiEmbedExposure>,
        seen: List<FfiEmbedSeen>,
        blocked: List<FfiEmbedBlockedAuthor>,
        blockWords: List<String>,
    ) = FfiEmbedSnapshot(favorites, history, exposures, seen, blocked, blockWords)

    fun favorite(item: FeedItem, addedAt: Long) = FfiEmbedFavorite(itemOf(item), addedAt)

    fun history(item: FeedItem, visitedAt: Long, progressMs: Long?, durationMs: Long?) =
        FfiEmbedHistory(itemOf(item), visitedAt, progressMs, durationMs)

    fun exposure(key: String, platform: String, surface: String, exposedAt: Long, lastSeenAt: Long, dwellMs: Long) =
        FfiEmbedExposure(key, platform, surface, exposedAt, lastSeenAt, dwellMs)

    fun seen(item: FeedItem, firstSeenAt: Long) = FfiEmbedSeen(itemOf(item), firstSeenAt)

    // ---------------------------------------------------------------- 调度 / 封面状态机

    /**
     * 纯文字向量的调度状态机（`FfiEmbedScheduler` 的薄包装）。
     *
     * 所有决策都在 Rust；这里只是把 `FeedItem` 转成投影、把 `FfiEmbedStep` 翻成 Kotlin 认的类型。
     */
    class Scheduler(config: FfiEmbedConfig) {
        /**
         * `newWithMemory` 只是"这个对象自己有一份内存假库"的构造入口（Rust 侧不建真库）；
         * 真库靠每个 [nextStep] / [finishBatch] 传进来的 [FfiEmbedStore]。
         */
        private val native: FfiEmbedScheduler = load { FfiEmbedScheduler.newWithMemory(config) }

        fun offer(items: List<FeedItem>): Int = load { native.offer(itemsOf(items)) }

        fun offerDetail(item: FeedItem, body: String?): Boolean = load { native.offerDetail(itemOf(item), body) }

        fun requestBackfill() = load { native.requestBackfill() }

        fun cancel() = load { native.cancel() }

        fun resetStats() = load { native.resetStats() }

        fun nextStep(now: Long, model: String, graphLengths: List<Int>, store: FfiEmbedStore): Step = load {
            val raw: FfiEmbedStep = native.nextStep(FfiEmbedNext(now, model, graphLengths), store)
            Step.from(raw)
        }

        fun supplyPrep(texts: List<String>, tokens: IntArray) = load {
            native.supplyPrep(texts, tokens.toList())
        }

        fun supplyBackfill(candidates: List<FfiEmbedBackfill>) = load { native.supplyBackfill(candidates) }

        fun finishBatch(
            vectors: FloatArray,
            vecDim: Int,
            storedModel: String,
            now: Long,
            store: FfiEmbedStore,
        ): Done? = load {
            val raw = native.finishBatch(vectors.toList(), vecDim, storedModel, now, store) ?: return@load null
            Done(raw.count, raw.fromDetail, raw.fromBackfill, raw.kind, raw.model, raw.sources)
        }

        fun failBatch() = load { native.failBatch() }

        fun busy(): Boolean = load { native.busy() }

        fun queuedCount(): Int = load { native.queuedCount() }

        fun computed(): Int = load { native.computed() }

        fun backfilled(): Int = load { native.backfilled() }

        fun detailed(): Int = load { native.detailed() }

        fun lastBatchSize(): Int = load { native.lastBatchSize() }
    }

    /** 一步（`Empty` / `NeedBackfill` / `Prepare` / `Batch`）。 */
    data class Step(
        val kind: FfiEmbedStepKind,
        val texts: List<String>,
        val truncateTo: List<Int>,
        val maxTokens: Int,
        val count: Int,
        val model: String,
        val sinceMs: Long,
        val limit: Int,
    ) {
        companion object {
            fun from(raw: FfiEmbedStep) = Step(
                raw.kind,
                raw.texts,
                raw.truncateTo,
                raw.maxTokens,
                raw.count,
                raw.model,
                raw.sinceMs,
                raw.limit,
            )
        }
    }

    /** 一批算完的账（`model` / `sources` 是**写进库的**那批，设置页按它们累计分布）。 */
    data class Done(
        val count: Int,
        val fromDetail: Int,
        val fromBackfill: Int,
        val kind: String,
        val model: String,
        val sources: List<String>,
    )

    /** 封面调度参数（默认 = Rust 的 `CoverConfig::default()`）。 */
    fun coverConfig(
        windowSize: Int = 32,
        maxQueued: Int = 96,
        dim: Int = this.dim,
        maxImageAttempts: Int = 4,
        textBusyPollMs: Long = 5_000L,
        maxWaitMs: Long = 30 * 60_000L,
    ): FfiCoverConfig = FfiCoverConfig(
        windowSize,
        maxQueued,
        dim,
        maxImageAttempts,
        textBusyPollMs,
        maxWaitMs,
    )

    /** 纯文字调度的参数（默认 = 线上那一套）。 */
    fun embedConfig(
        batchSize: Int = 8,
        dim: Int = this.dim,
        backfillChunk: Int = 24,
        backfillWindowMs: Long = 30 * 86_400_000L,
        detailMaxTokens: Int = DETAIL_MAX_TOKENS,
        detailMaxChars: Int = DETAIL_MAX_CHARS,
        maxSummaryChars: Int = MAX_SUMMARY_CHARS,
    ): FfiEmbedConfig = FfiEmbedConfig(
        batchSize,
        dim,
        backfillChunk,
        backfillWindowMs,
        detailMaxTokens,
        detailMaxChars,
        maxSummaryChars,
    )

    /** 封面向量的调度状态机（`FfiCoverScheduler` 的薄包装）。 */
    class CoverSchedulerState(config: FfiCoverConfig) {
        private val native: FfiCoverScheduler = load { FfiCoverScheduler(config) }

        fun offer(items: List<FeedItem>): Int = load { native.offer(itemsOf(items)) }

        fun beginWindow(model: String, store: FfiEmbedStore): List<CoverPick> = load {
            native.beginWindow(model, store).map { CoverPick(it.key, it.text, it.coverUrl) }
        }

        fun imageMissing(key: String): Boolean = load { native.imageMissing(key) }

        fun abortWindow(): Int = load { native.abortWindow() }

        fun windowFailed(reason: String) = load { native.windowFailed(reason) }

        fun finishWindow(results: List<Pair<String, FloatArray>>, storedModel: String, now: Long, store: FfiEmbedStore): Int =
            load {
                val rows = results.map { FfiCoverVector(it.first, it.second.toList()) }
                native.finishWindow(rows, storedModel, now, store)
            }

        fun waitTick(): CoverWait = load {
            val wait: FfiCoverWait = native.waitTick(FfiCoverWaitKind.TEXT_BUSY)
            CoverWait(wait.delayMs, wait.stop)
        }

        fun clearWait() = load { native.clearWait() }

        fun cancel() = load { native.cancel() }

        fun resetStats() = load { native.resetStats() }

        fun resetStatus(bundled: Boolean, enabled: Boolean) = load { native.resetStatus(bundled, enabled) }

        fun status(): CoverStatus = load {
            val raw: FfiCoverStatus = native.status()
            CoverStatus(raw.kind, raw.label, raw.done, raw.total, raw.reason)
        }

        fun pending(): Int = load { native.pending() }

        fun computed(): Int = load { native.computed() }

        fun skipped(): Int = load { native.skipped() }

        fun isBroken(): Boolean = load { native.isBroken() }

        // ---------------- 窗口循环

        fun windowStep(flags: WindowFlags): WindowStep = load { native.windowStep(flags.raw()).toStep() }

        fun reportPlan(model: String): WindowStep = load { native.reportPlan(model).toStep() }

        fun reportImage(flags: WindowFlags, key: String, found: Boolean): WindowStep = load {
            native.reportImage(flags.raw(), key, found).toStep()
        }

        fun reportPrepare(failure: String?): WindowStep = load { native.reportPrepare(failure).toStep() }

        fun reportEmbed(outerFailed: Boolean, returned: Int): WindowStep = load {
            native.reportEmbed(outerFailed, returned).toStep()
        }

        fun reportFinished(): WindowStep = load { native.reportFinished().toStep() }

        fun windowKeys(): List<String> = load { native.windowKeys() }

        fun observeWindow(stats: CoverWindowStatsRaw): Long? = load {
            native.observeWindow(
                FfiCoverWindowStats(
                    stats.count,
                    stats.visionImages,
                    stats.visionMs,
                    stats.textBatches,
                    stats.textMs,
                    stats.totalMs,
                    stats.compiled,
                ),
            )
        }

        fun lastWindow(): CoverWindowStatsRaw? = load {
            native.lastWindow()?.let {
                CoverWindowStatsRaw(it.count, it.visionImages, it.visionMs, it.textBatches, it.textMs, it.totalMs, it.compiled)
            }
        }

        fun avgMs(): Long? = load { native.avgMs() }

        fun drainLogs(): List<String> = load { native.drainLogs() }
    }

    /** 一轮的输入（宿主每轮回一次）。 */
    data class WindowFlags(
        val enabled: Boolean,
        val foreground: Boolean,
        val textIdle: Boolean,
        val bundled: Boolean,
    ) {
        fun raw() = uniffi.feedhub_ffi.FfiWindowFlags(enabled, foreground, textIdle, bundled)
    }

    /** 窗口循环的下一步。 */
    sealed interface WindowStep {
        /** 回循环开头再拉一步。 */
        data object Continue : WindowStep

        /** 等一等。 */
        data class Wait(val delayMs: Long, val stop: Boolean, val kind: String) : WindowStep

        /** `begin_window(model, store)` 然后 `report_plan`。 */
        data object BeginWindow : WindowStep

        /** 去取这一条的封面图。 */
        data class LoadImage(val key: String, val text: String, val coverUrl: String) : WindowStep

        /** `prepare()` 两张图。 */
        data object Prepare : WindowStep

        /** `embed(inputs)`。 */
        data object Embed : WindowStep

        /** 写库 / 关会话 / 记账。 */
        data object Finish : WindowStep

        /** 当前结束。 */
        data object Stop : WindowStep
    }

    private fun uniffi.feedhub_ffi.FfiWindowStep.toStep(): WindowStep = when (kind) {
        uniffi.feedhub_ffi.FfiWindowStepKind.CONTINUE -> WindowStep.Continue
        uniffi.feedhub_ffi.FfiWindowStepKind.WAIT -> WindowStep.Wait(delayMs, stop, waitKind)
        uniffi.feedhub_ffi.FfiWindowStepKind.BEGIN_WINDOW -> WindowStep.BeginWindow
        uniffi.feedhub_ffi.FfiWindowStepKind.LOAD_IMAGE -> WindowStep.LoadImage(key, text, coverUrl)
        uniffi.feedhub_ffi.FfiWindowStepKind.PREPARE -> WindowStep.Prepare
        uniffi.feedhub_ffi.FfiWindowStepKind.EMBED -> WindowStep.Embed
        uniffi.feedhub_ffi.FfiWindowStepKind.FINISH -> WindowStep.Finish
        uniffi.feedhub_ffi.FfiWindowStepKind.STOP -> WindowStep.Stop
    }

    /** 一个窗口的账（过边界的形状）。 */
    data class CoverWindowStatsRaw(
        val count: Int,
        val visionImages: Int,
        val visionMs: Long,
        val textBatches: Int,
        val textMs: Long,
        val totalMs: Long,
        val compiled: Int,
    )

    data class CoverPick(val key: String, val text: String, val coverUrl: String)
    data class CoverWait(val delayMs: Long, val stop: Boolean)
    data class CoverStatus(
        val kind: FfiCoverStatusKind,
        val label: String,
        val done: Int,
        val total: Int,
        val reason: String?,
    )

    // ---------------------------------------------------------------- 内部

    /** 每次过边界前确保 `.so` 在（[RustNative] 自己会记住"已经加载过"）。 */
    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
