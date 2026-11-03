package feedhub.brain

import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiMixBlocked
import uniffi.feedhub_ffi.FfiMixItem
import uniffi.feedhub_ffi.FfiMixOrder
import uniffi.feedhub_ffi.FfiMixReason
import uniffi.feedhub_ffi.FfiMixRules
import uniffi.feedhub_ffi.FfiMixText
import uniffi.feedhub_ffi.brainBlend
import uniffi.feedhub_ffi.brainBlendOrder
import uniffi.feedhub_ffi.brainCap
import uniffi.feedhub_ffi.brainFilterBlocked
import uniffi.feedhub_ffi.brainFilterHidden
import uniffi.feedhub_ffi.brainFilterShown
import uniffi.feedhub_ffi.brainFilterVisible
import uniffi.feedhub_ffi.brainNormalize
import uniffi.feedhub_ffi.brainNormalizeMany
import uniffi.feedhub_ffi.brainNormalizeRules
import uniffi.feedhub_ffi.brainOrder
import uniffi.feedhub_ffi.brainPrepare
import uniffi.feedhub_ffi.brainSearchQuota
import uniffi.feedhub_ffi.brainSearchRoundRobin

/**
 * 批内顺序（Rust `brain::OrderKind` 的 Kotlin 名字）。
 *
 * 不用 UniFFI 生成的 `FfiMixOrder`：门面**只暴露 `feedhub.core` 的类型**（和 `feedhub.rust.Rust*` 的约定一样），
 * `Ffi*` 是过河的形状，不该漏给业务代码。
 */
enum class MixOrder {
    /** 关注页：永远按发布时间倒序（关注流本身就是时间线，不受设置影响）。 */
    FOLLOWING,

    /** 首页设置成「按时间」。 */
    TIME,

    /** 首页默认：各平台穿插。 */
    INTERLEAVE,
}

/**
 * **首页大脑**的 Rust 门面（`rust/crates/feedhub-core/src/brain/`）。
 *
 * Kotlin 这边**只剩过河和状态管理**。
 * 和别的 `Rust*` 门面一样：**没有开关**、没有 Kotlin 备用实现，`.so` 不能加载就抛一句说得清的话，不悄悄换成另一种行为。
 *
 * 三条边界约定：
 * 1. **一批一次**：入参是整批，出参是**这批在输入里的下标**（`IntArray`），Kotlin 拿下标回自己的列表取，
 *    不复制 `FeedItem`、不逐条过 FFI；
 * 2. **两种入参**：排序走 `FfiMixItem`（key / 平台 / 发布时间），过滤走 `FfiMixText`（18+ / 标题 / 摘要 / 社区 / 转发）——
 *    排序不为几十个字的长文本付 JNA 的复制成本；
 * 3. **状态留在 Kotlin**：批 id、`seen` 集合、游标、预取、`origins` 都是 `FeedState` 的事；这里只算"这一批怎么排 / 留哪些"。
 *
 * 产品规则由 Rust 执行：首页各平台穿插、平台内保持平台顺序、
 * 关注页永远按时间、每平台一批最多 N 条（只截断不重排）、个性化只排序不删（权重 0）、18+ 整条隐藏、
 * 屏蔽词 / 外出模式命中整条隐藏。
 */
object Brain {
    /** 每平台封顶的结果（[ranks] 和 [kept] 一一对应）。 */
    class Capped(val kept: IntArray, val ranks: IntArray, val dropped: IntArray)

    /** 封顶 + 平台序号 + 排序，一次算完的结果（[ranks] 和 [order] 一一对应）。 */
    class Prepared(val order: IntArray, val ranks: IntArray, val dropped: IntArray)

    /** 一批过滤的结果：保留下来的下标 + 藏掉几条。 */
    class Shown(val kept: IntArray, val hidden: Int)

    /** 一条为什么不能显示：平台标的 18+，或者命中一条名单规则。 */
    sealed interface Blocked {
        /** 平台标的 18+（不是名单规则，界面 / 日志都按"平台标的"说）。 */
        data object Adult : Blocked

        /** 命中一条名单规则。 */
        data class Rule(val hit: ContentFilter.Hit) : Blocked
    }

    /** 批内顺序：返回 [items] 里的下标。 */
    fun order(items: List<FeedItem>, kind: MixOrder, seed: Long): IntArray = load {
        brainOrder(items.map { it.mixItem() }, kind.toFfi(), seed).toIndices(items.size)
    }

    /** 每平台封顶 + 数平台序号（不排序）。 */
    fun cap(items: List<FeedItem>, maxPerPlatform: Int): Capped = load {
        val capped = brainCap(items.map { it.mixItem() }, maxPerPlatform)
        Capped(
            kept = capped.kept.toIndices(items.size),
            ranks = capped.ranks.toIndices(capped.kept.size),
            dropped = capped.dropped.toIndices(items.size),
        )
    }

    /** 封顶 + 平台序号 + 排序，一次算完（`BatchMerge.prepare` 里"去重之后"的那一段）。 */
    fun prepare(items: List<FeedItem>, maxPerPlatform: Int, kind: MixOrder, seed: Long): Prepared =
        load {
            val prepared = brainPrepare(items.map { it.mixItem() }, maxPerPlatform, kind.toFfi(), seed)
            Prepared(
                order = prepared.order.toIndices(items.size),
                ranks = prepared.ranks.toIndices(prepared.order.size),
                dropped = prepared.dropped.toIndices(items.size),
            )
        }

    /** 打分钩子的合成（`EmbeddingScore.blend`）：位置分数 + 相似度（权重 0 = 原样返回位置分数）。 */
    fun blend(position: Float, similarity: Float?, weight: Float): Float = load {
        brainBlend(position, similarity, weight)
    }

    /** 在排好的顺序上叠一层相似度（稳定排序）：返回输入里的下标。 */
    fun blendOrder(items: List<FeedItem>, similarities: List<Float?>, weight: Float): IntArray = load {
        brainBlendOrder(items.map { it.mixItem() }, similarities, weight).toIndices(items.size)
    }

    /** 一批里**按名单**能显示的那些（不看 18+，`ContentFilter.visible`）。 */
    fun visibleRules(items: List<FeedItem>, rules: ContentFilter.Rules): Shown = load {
        val visible = brainFilterVisible(items.map { it.mixText() }, rules.toFfi())
        Shown(visible.kept.toIndices(items.size), visible.hidden.toInt())
    }

    /** 一批里**能上屏**的那些：18+ 一直藏，再按名单（信息流 / 搜索的 `kept`）。 */
    fun shown(items: List<FeedItem>, rules: ContentFilter.Rules): Shown = load {
        val shown = brainFilterShown(items.map { it.mixText() }, rules.toFfi())
        Shown(shown.kept.toIndices(items.size), shown.hidden.toInt())
    }

    /** 单条按名单判断（不看 18+，`ContentFilter.hidden`）。 */
    fun hidden(item: FeedItem, rules: ContentFilter.Rules): ContentFilter.Hit? = load {
        brainFilterHidden(item.mixText(), rules.toFfi())?.let { hit ->
            ContentFilter.Hit(hit.reason.toReason(), hit.rule.orEmpty())
        }
    }

    /** 单条合判（18+ 或名单，`AdultFilter` + `ContentFilter` 合起来那条）。 */
    fun blocked(item: FeedItem, rules: ContentFilter.Rules): Blocked? = load {
        when (val blocked = brainFilterBlocked(item.mixText(), rules.toFfi())) {
            null -> null
            FfiMixBlocked.Adult -> Blocked.Adult
            is FfiMixBlocked.Rule -> Blocked.Rule(
                ContentFilter.Hit(blocked.hit.reason.toReason(), blocked.hit.rule.orEmpty()),
            )
        }
    }

    /** 名单 / 文本的归一化口径。 */
    fun normalize(text: String): String = load { brainNormalize(text) }

    /**
     * 一批文本的归一化（`ContentFilter.normalize` 的批量版）：设置页的"加一条"一次要比
     * 输入框 + 已有条目（最多 200 条），`remember(text, words)` 里**一次 FFI** 算完，
     * 不在每次重组时逐条过边界。顺序和输入一一对应。
     */
    fun normalizeMany(texts: List<String>): List<String> = if (texts.isEmpty()) {
        emptyList()
    } else {
        load { brainNormalizeMany(texts) }
    }

    /** 名单收干净（`ContentFilter.normalizeRules`）：去空白 / 丢空的和过长的 / 去重 / 最多 200 条。 */
    fun normalizeRules(words: List<String>): List<String> = if (words.isEmpty()) {
        emptyList()
    } else {
        load { brainNormalizeRules(words) }
    }

    /** 跨平台搜索：每家在**一页里**取几条进桶。 */
    fun quota(platforms: Int, pageSize: Int, target: Int): Int = load {
        brainSearchQuota(platforms, pageSize, target)
    }

    /** 跨平台搜索轮转：返回 [items] 里的下标（平台顺序、去重都在 Rust 里）。 */
    fun roundRobin(platforms: List<Platform>, items: List<FeedItem>): IntArray = load {
        brainSearchRoundRobin(platforms.map { it.key }, items.map { it.mixItem() })
            .toIndices(items.size)
    }

    /**
     * 加载 `.so` 再说。这里**不 catch 具体错误**（没有网络、没有平台错误码，能失败的只有"库没编出来 /
     * ABI 不对"），[RustNative] 已经会给一句说得清的话；业务代码不接受"悄悄退回 Kotlin"。
     */
    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    /** `List<UInt>`（UniFFI 的 `u32`）→ `IntArray`，顺便挡一下越界（Rust 不该返回越界下标）。 */
    private fun List<UInt>.toIndices(size: Int): IntArray {
        val out = IntArray(this.size) { i ->
            val value = this[i].toLong()
            require(value < size) { "首页大脑（Rust）返回的下标越界：$value / $size" }
            value.toInt()
        }
        return out
    }

    /**
     * 排序要的那一条（`FeedItem` → `FfiMixItem`）。
     *
     * 注意 `FeedItem.key` 是 `platform:id`（core 里算好的），和 Rust 侧一致；平台用 [Platform.key]。
     */
    private fun FeedItem.mixItem(): FfiMixItem = FfiMixItem(
        key = key,
        platform = platform.key,
        publishedAt = publishedAt,
    )

    /** 过滤要的那一条（只搬过滤看的字段；18+ 是平台标的那个布尔）。 */
    private fun FeedItem.mixText(): FfiMixText = FfiMixText(
        adult = adult,
        title = title,
        summary = summary,
        community = community,
        repostTitle = repost?.title,
        repostSummary = repost?.summary,
        repostCommunity = repost?.community,
    )

    private fun ContentFilter.Rules.toFfi(): FfiMixRules = FfiMixRules(
        blockWords = blockWords,
        outingMode = outingMode,
        outingKeywords = outingKeywords,
        outingCommunities = outingCommunities,
    )

    private fun MixOrder.toFfi(): FfiMixOrder = when (this) {
        MixOrder.FOLLOWING -> FfiMixOrder.FOLLOWING
        MixOrder.TIME -> FfiMixOrder.TIME
        MixOrder.INTERLEAVE -> FfiMixOrder.INTERLEAVE
    }

    private fun FfiMixReason.toReason(): ContentFilter.Reason = when (this) {
        FfiMixReason.BLOCK_WORD -> ContentFilter.Reason.BLOCK_WORD
        FfiMixReason.OUTING_KEYWORD -> ContentFilter.Reason.OUTING_KEYWORD
        FfiMixReason.OUTING_COMMUNITY -> ContentFilter.Reason.OUTING_COMMUNITY
    }
}
