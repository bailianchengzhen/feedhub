package feedhub.feed

import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiMixBehavior
import uniffi.feedhub_ffi.FfiMixConfig
import uniffi.feedhub_ffi.FfiMixCount
import uniffi.feedhub_ffi.FfiMixFeedItem
import uniffi.feedhub_ffi.FfiMixPersonalization
import uniffi.feedhub_ffi.FfiMixQuotaConfig
import uniffi.feedhub_ffi.FfiMixWeight
import uniffi.feedhub_ffi.feedMixBatch
import uniffi.feedhub_ffi.feedMixQuotas

/**
 * **混排原型**（比例配额 + 个性化**过滤**）的门面：`rust/crates/feedhub-core/src/feed/mix.rs`。
 *
 * 所以"分低的去掉"不接进信息流。
 * 代码留着、有测试，别删也别接。
 *
 * **已经接进界面的是另一个东西**：每平台封顶 / 穿插 / 按时间 / 打分钩子的合成在 `humhub.brain.Brain`
 * （`brain::order`），C 块就搬完了；这里只是同一个 `FeedMix.kt` 里**还没搬的那两样**。
 *
 * **没有开关也不需要 `.so` 才能看设置**：只有真的要算配额 / 原型时才加载。
 */
object MixEngine {

    /** 一条候选（只看 key / 平台 / 发布时间）。 */
    data class Candidate(val key: String, val platform: String, val publishedAt: Long?)

    /** 比例配额的参数。 */
    data class Quota(
        val batchTarget: Int = 0,
        val weights: Map<Platform, Float> = emptyMap(),
        val floorPerPlatform: Int = 3,
    )

    /** 个性化**过滤**原型的参数（默认关）。 */
    data class Personalization(
        val enabled: Boolean = false,
        val minPositives: Int = 150,
        val minPositivesPerPlatform: Int = 5,
        val threshold: Float = 0.35f,
        val explorationRatio: Float = 0.2f,
        val maxDropRatio: Float = 0.4f,
    )

    /** 一批怎么筛。 */
    data class Config(
        val quota: Quota = Quota(),
        val personalization: Personalization = Personalization(),
    )

    /** 行为摘要（纯数据）。 */
    data class Behavior(
        val positives: Int = 0,
        val positivesByPlatform: Map<Platform, Int> = emptyMap(),
    )

    /** 一批筛完的结果（`order` 是输入下标；`perPlatform` 保持平台第一次出现的顺序）。 */
    data class Result(
        val order: IntArray,
        val droppedLowScore: Int,
        val droppedByQuota: Int,
        val perPlatform: Map<Platform, Int>,
        val shortOfTarget: Boolean,
    )

    /**
     * 每个平台留几条（水位法）。
     *
     * [sizes] 的顺序就是返回值的顺序（**保持调用方给的平台顺序**：Rust 那边照这个顺序算）。
     */
    fun quotas(
        sizes: List<Pair<Platform, Int>>,
        target: Int,
        weights: Map<Platform, Float> = emptyMap(),
        floorPerPlatform: Int = 0,
    ): Map<Platform, Int> {
        RustNative.ensureLoaded()
        val out = LinkedHashMap<Platform, Int>(sizes.size)
        val picked = feedMixQuotas(
            sizes.map { FfiMixCount(it.first.key, it.second) },
            target,
            weights.map { FfiMixWeight(it.key.key, it.value) },
            floorPerPlatform,
        )
        for (row in picked) out[Platform(row.platform)] = row.count
        return out
    }

    /**
     * 把一批候选筛成上屏的一批（`mixBatch` 原型）。
     *
     * [scores] 和 [candidates] 一一对应（`null` = 这个模型还没法给它打分，当"不过滤"处理）。
     */
    fun mix(
        candidates: List<FeedItem>,
        behavior: Behavior = Behavior(),
        config: Config = Config(),
        scores: List<Float?> = candidates.map { null },
    ): Result {
        RustNative.ensureLoaded()
        val picked = feedMixBatch(
            candidates.map { FfiMixFeedItem(it.key, it.platform.key, it.publishedAt) },
            FfiMixBehavior(
                behavior.positives,
                behavior.positivesByPlatform.map { FfiMixCount(it.key.key, it.value) },
            ),
            FfiMixConfig(
                FfiMixQuotaConfig(
                    config.quota.batchTarget,
                    config.quota.weights.map { FfiMixWeight(it.key.key, it.value) },
                    config.quota.floorPerPlatform,
                ),
                FfiMixPersonalization(
                    config.personalization.enabled,
                    config.personalization.minPositives,
                    config.personalization.minPositivesPerPlatform,
                    config.personalization.threshold,
                    config.personalization.explorationRatio,
                    config.personalization.maxDropRatio,
                ),
            ),
            scores,
        )
        val order = IntArray(picked.order.size) { picked.order[it].toInt() }
        val perPlatform = LinkedHashMap<Platform, Int>(picked.perPlatform.size)
        for (row in picked.perPlatform) perPlatform[Platform(row.platform)] = row.count
        return Result(
            order = order,
            droppedLowScore = picked.droppedLowScore.toInt(),
            droppedByQuota = picked.droppedByQuota.toInt(),
            perPlatform = perPlatform,
            shortOfTarget = picked.shortOfTarget,
        )
    }
}
