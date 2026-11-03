package feedhub.app.embed

import feedhub.app.library.BlockedAuthor
import feedhub.app.library.ExposureRow
import feedhub.app.library.ExposureSurface
import feedhub.app.library.Favorite
import feedhub.app.library.FavoriteQuery
import feedhub.app.library.HistoryEntry
import feedhub.app.library.HistoryQuery
import feedhub.app.library.Library
import feedhub.app.library.SeenItem
import feedhub.app.library.blockKey
import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.rust.RustEmbed

/**
 * 行为信号。
 *
 * 一条信号 = 「对哪条内容、什么动作、什么时候、有多强」。库里已有的六种行为折成八种信号：
 *
 * | 信号 | 来源 | 基础权重 | 半衰期 |
 * |---|---|---|---|
 * | [SignalKind.FAVORITE] 收藏 | `favorite.added_at` | **+1.00** | 30 天 |
 * | [SignalKind.VISIT] 点开 | `history.visited_at` | +0.35 | 21 天 |
 * | [SignalKind.WATCH_DONE] 看完 | 进度 ≥90% 且时长 ≥60 s | +0.65 | 21 天 |
 * | [SignalKind.DWELL_LONG] 看得久 | `surface=detail` 且停留 ≥60 s | +0.45 | 21 天 |
 * | [SignalKind.QUICK_BACK] 点开就退 | `surface=detail` 且 1 s ≤ 停留 < 5 s | **−0.25** | 14 天 |
 * | [SignalKind.SKIP] 划过没点 | 首页 / 关注曝光之后没打开过 | −0.08 | 7 天 |
 * | [SignalKind.BLOCKED_AUTHOR] 屏蔽作者 | `blocked_user.blocked_at` | **−1.00** | 120 天 |
 * | [SignalKind.BLOCKED_WORD] 命中屏蔽词 | 设置里的 `blockWords` | −0.50 | **不衰减** |
 *
 * 这里只剩**枚举**（业务代码要的类型）和"从库里读一份快照"（宿主 I/O）。
 * 枚举的 `label` / `baseWeight` / `halfLifeDays` 也是问 Rust 要的（[SignalKind.table]），Kotlin 不再抄一份数字。
 */
enum class SignalKind {
    FAVORITE,
    VISIT,
    WATCH_DONE,
    DWELL_LONG,
    QUICK_BACK,
    SKIP,
    BLOCKED_AUTHOR,
    BLOCKED_WORD,
    ;

    /** Rust 那份定义（名字 / 文案 / 权重 / 半衰期）。 */
    val def: RustEmbed.SignalKindDef get() = table.getValue(this)

    /** 设置页 / 导出里给人看的名字。 */
    val label: String get() = def.label

    /** 基础权重（带符号；正 = 喜欢，负 = 不喜欢）。 */
    val baseWeight: Float get() = def.baseWeight

    /** 多少天衰减一半；[SIGNAL_NO_DECAY] = 不衰减。 */
    val halfLifeDays: Double get() = def.halfLifeDays

    /** 正向（权重 > 0）吗——判定在 Rust（`SignalKind::is_positive`），Kotlin 不自己比大小。 */
    val isPositive: Boolean get() = RustEmbed.signalIsPositive(name)

    companion object {
        /** Rust 那张表（第一次用到时才加载 `.so`；顺序和枚举声明一致）。 */
        private val table: Map<SignalKind, RustEmbed.SignalKindDef> by lazy {
            RustEmbed.signalKinds().associate { def -> valueOf(def.name) to def }
        }
    }
}

/** [SignalKind.halfLifeDays] 用这个值表示**不衰减**。 */
const val SIGNAL_NO_DECAY: Double = 0.0

/** 详情页停留多久算"看得久"（毫秒）。 */
const val DWELL_LONG_MS: Long = 60_000L

/** 详情页停留少于多久算"点开就退"（毫秒）。 */
const val QUICK_BACK_MS: Long = 5_000L

/** 视频多长以上才判"看完"（毫秒）。 */
const val WATCH_DONE_MIN_DURATION_MS: Long = 60_000L

/** 视频看到多少比例算"看完"。 */
const val WATCH_DONE_RATIO: Float = 0.9f

/**
 * 时间衰减：`0.5 ^ (age / 半衰期)`（实现在 Rust）。
 *
 * [halfLifeDays] ≤ 0 = 不衰减；`age ≤ 0`（时钟倒退 / 未来的信号）= 1。
 */
fun signalDecay(ageMs: Long, halfLifeDays: Double): Float = RustEmbed.signalDecay(ageMs, halfLifeDays)

/**
 * 一条行为信号（纯数据）。
 *
 * [at] 是**动作发生的时刻**（收藏时间 / 打开时间 / 屏蔽时间 / 曝光结束时间），不是导出时间——
 * 时间衰减按它算。[strength] 是同类信号内部的强弱（0～1）。
 */
data class BehaviorSignal(
    val key: String,
    val kind: SignalKind,
    val at: Long,
    val strength: Float = 1f,
    val platform: Platform? = null,
) {
    /**
     * 到 [now] 这一刻的权重（带符号）：基础权重 × 强度 × 时间衰减（公式在 Rust）。
     *
     * 生产路径（导出 / 画像）不再走这里：`ffi_embed_signals` 一次就把权重算好带回来。
     * 留着它是给设置页 / 测试读单条信号（每次过一次边界，别放进循环里）。
     */
    fun effectiveWeight(now: Long): Float = RustEmbed.signalWeight(kind.name, strength, at, now)
}

/**
 * 从库里读出来的**一份死数据**：算信号要的全部原始行为。
 *
 * 为什么要有这个中间层：规则（谁正谁负、怎么衰减、怎么去重）是产品决定，在 Rust 里单独离线测；
 * 读写库的格式变化不该牵动规则。测试直接构造这个 data class，不碰 SQLite。
 */
data class BehaviorSnapshot(
    val favorites: List<Favorite> = emptyList(),
    val history: List<HistoryEntry> = emptyList(),
    val exposures: List<ExposureRow> = emptyList(),
    val seen: List<SeenItem> = emptyList(),
    val blockedAuthors: List<BlockedAuthor> = emptyList(),
    /** 只放"屏蔽词"（一直生效的那份）；外出模式的关键词是临时状态，**不算口味**。 */
    val blockWords: List<String> = emptyList(),
)

/**
 * 把一份快照折成信号列表（规则全在 Rust，这里只做类型转换）。
 *
 * - 收藏 / 点开 / 看完：来自 `favorite` / `history`；18+ 的不算；
 * - 详情停留：`surface=detail`，同一条取停留最久的那次；
 * - 划过没点：首页 / 关注面的曝光合并成一条，曝光之后打开过就不算；
 * - 屏蔽作者：落到这个作者出现过的每张卡片上；
 * - 命中屏蔽词：用 Rust 的 `ContentFilter` 匹配（不分大小写 / 全半角），不衰减。
 *
 * 输出按 `(at, key, kind)` 排序，测试可以直接比对整份列表。
 */
fun buildBehaviorSignals(snapshot: BehaviorSnapshot, now: Long = System.currentTimeMillis()): List<BehaviorSignal> =
    RustEmbed.signals(snapshot.toRust(), now).map { s ->
        BehaviorSignal(
            key = s.key,
            kind = SignalKind.valueOf(s.kind),
            at = s.at,
            strength = s.strength,
            platform = s.platform?.let(::Platform),
        )
    }

/** 把 Kotlin 的快照翻成过边界的形状（`FeedItem` 只留算信号用得到的字段）。 */
private fun BehaviorSnapshot.toRust(): uniffi.feedhub_ffi.FfiEmbedSnapshot = RustEmbed.snapshot(
    favorites = favorites.map { RustEmbed.favorite(it.item, it.addedAt) },
    history = history.map { RustEmbed.history(it.item, it.visitedAt, it.progressMs, it.durationMs) },
    exposures = exposures.map {
        RustEmbed.exposure(it.key, it.platform.key, it.surface.key, it.exposedAt, it.lastSeenAt, it.dwellMs)
    },
    seen = seen.map { RustEmbed.seen(it.item, it.firstSeenAt) },
    blocked = blockedAuthors.map { RustEmbed.exportBlocked(it.user.blockKey(), it.blockedAt) },
    blockWords = blockWords,
)

/**
 * 每种信号各几条。
 *
 * 一次过边界（[RustEmbed.countKinds]），
 * 顺序 / 补零 / 正负口径都跟着 Rust 那份枚举走——Kotlin 不再自己数、也不再自己排。
 */
fun signalCounts(signals: List<BehaviorSignal>): Map<SignalKind, Int> =
    signalCountsOfKinds(signals.map { it.kind.name })

/** 同上，但输入是"种类名"（`FAVORITE` / `WATCH_DONE` / …）；不认识的忽略，固定 8 项。 */
fun signalCountsOfKinds(kinds: List<String>): Map<SignalKind, Int> {
    val out = LinkedHashMap<SignalKind, Int>()
    for (kind in SignalKind.entries) out[kind] = 0
    for ((name, count) in RustEmbed.countKinds(kinds)) {
        out[SignalKind.valueOf(name)] = count
    }
    return out
}

/**
 * 从库里读一份 [BehaviorSnapshot]（**只有读，不写**）。
 *
 * 读的条数都有上限（保留期内够用）：设置页那一行状态只数"最近的"，
 * 导出那边自己要完整数据、传大一点的 [limits]。
 */
suspend fun collectBehaviorSnapshot(
    library: Library,
    blockWords: List<String> = emptyList(),
    limits: BehaviorSnapshotLimits = BehaviorSnapshotLimits.SETTINGS,
): BehaviorSnapshot = BehaviorSnapshot(
    favorites = library.favorites(FavoriteQuery(limit = limits.favorites)),
    history = library.history(HistoryQuery(limit = limits.history)),
    exposures = library.exposureRows(limit = limits.exposures),
    seen = library.seenItems(limit = limits.seen),
    blockedAuthors = library.blockedAuthors(),
    blockWords = blockWords,
)

/**
 * [collectBehaviorSnapshot] 的条数上限。
 *
 * 设置页那一份故意小一号：它只是为了显示"各信号几条"，没必要把 30 天的 `item_json` 全读进来；
 * 导出 / 回放要完整数据，用 [EXPORT]。
 */
data class BehaviorSnapshotLimits(
    val favorites: Int,
    val history: Int,
    val exposures: Int,
    val seen: Int,
) {
    companion object {
        /** 设置页：最近的几千条足够看出信号分布（页面上也写了"最多看最近的…"）。 */
        val SETTINGS = BehaviorSnapshotLimits(favorites = 2_000, history = 5_000, exposures = 20_000, seen = 5_000)

        /** 导出 / 离线回放：和导出其它表一样放到 10 万条。 */
        val EXPORT = BehaviorSnapshotLimits(favorites = 100_000, history = 100_000, exposures = 100_000, seen = 100_000)
    }
}
