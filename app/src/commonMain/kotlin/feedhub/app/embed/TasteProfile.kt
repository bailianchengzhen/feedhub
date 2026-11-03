package feedhub.app.embed

import feedhub.rust.RustEmbed

/**
 * 「口味画像」：把一堆带权重的行为信号压成**两个向量**
 * （正向中心 + 负向中心），再给卡片打分。
 *
 * ## 为什么是"加权中心"而不是"top-k 近邻"
 *
 * 两种口径都试过（离线回放的合成数据上两种都算了 AUC，见 `tools/embed/replay.py` 的输出）。选中心的理由：
 *
 * 1. **正样本很少**：用户 5 天只有约 45 次点开 / 天、收藏个位数。top-k 在这种量级下 k 取几都别扭
 *    （k=5 基本等于"最近 5 次"，k=50 又把半年前的也算进来）；加权中心只用一次加权平均，不需要调 k。
 * 2. **便宜**：中心是**每个模型一个向量**（256 维），打分是一次点积。
 * 3. **负信号有地方放**：中心可以再算一个"负向中心"，用 [NEGATIVE_PENALTY] 当排斥项。
 * 4. **可解释 / 可回放**：导出里全是信号（key + 时间 + 权重），任何人用同一套公式都能复算出一个中心。
 *
 * 代价（如实写）：**多峰兴趣会被平均掉**。"一半看手机、一半看 AI"的人，中心落在两者中间。
 * 当前**权重是 0**（[feedhub.app.feed.EmbeddingScore.WEIGHT]），画像只用于导出和离线回放。
 *
 * ## 两个向量空间不能混
 *
 * CPU（q4）、NPU（w8a16）、封面（w8a16 + 图）是不同的向量空间，所以画像带 [model]，
 * [similarity] 要求调用方给的向量属于同一个模型（[ItemEmbedding.model]），不一致直接返回 null，
 * 绝不跨空间比。
 *
 * 这个文件只是数据载体。
 */
data class TasteProfile(
    /** 这个画像是哪个模型的向量空间（`item_embedding.model`）。 */
    val model: String,
    /** 正向加权中心（已 L2 归一化）；一个正样本向量都没有时是 null。 */
    val positive: FloatArray?,
    /** 负向加权中心（已 L2 归一化）；没有负样本时是 null（这时打分只有正向项）。 */
    val negative: FloatArray?,
    /** 正向权重的绝对值和（给导出 / 调试看）。 */
    val positiveWeight: Float,
    /** 负向权重的绝对值和。 */
    val negativeWeight: Float,
    val positiveSignals: Int,
    val negativeSignals: Int,
) {
    /** 有没有可用的正向中心（只有负样本的话打不出分）。 */
    val usable: Boolean get() = positive != null

    /**
     * 一条卡片对这个画像的相似度（0～1，0.5 = 中性；越大越像"我喜欢的东西"）。
     *
     * [model] 不为 null 时必须和 [model] 一致，否则返回 null（两个向量空间不能混着比）。
     * 公式在 Rust（`TasteProfile::similarity`）；**不在首页热路径上**（权重是 0），调用方一次一条。
     */
    fun similarity(vec: FloatArray?, model: String? = null, negativePenalty: Float = NEGATIVE_PENALTY): Float? {
        if (model != null && model != this.model) return null
        return toRust().similarity(vec, null, negativePenalty)
    }

    /** 自己 → 过边界的形状（Rust 的 `ffi_taste_similarity` 要它）。 */
    private fun toRust(): RustEmbed.Profile = RustEmbed.Profile(
        model,
        positive,
        negative,
        positiveWeight,
        negativeWeight,
        positiveSignals,
        negativeSignals,
    )
}

/**
 * 负向中心的排斥强度（固定的设计参数，不跟着数据调；改了要重跑回放并把前后对比写进结果）。
 *
 * 具体的数值在 Rust（`taste::NEGATIVE_PENALTY`），这里保留常量是为了给默认参数用；
 * 对拍会断言两份相等。
 */
const val NEGATIVE_PENALTY: Float = 0.5f

/**
 * 从信号 + 向量造一个模型的口味画像。
 *
 * @param vectorOf 这个模型下 key → 向量（没算过 / 别的模型返回 null，会被跳过并计数）。
 * @param now 算时间衰减的"现在"。
 */
fun buildTasteProfile(
    signals: List<BehaviorSignal>,
    model: String,
    vectorOf: (String) -> FloatArray?,
    now: Long,
): TasteProfile {
    // 同一个 key 只问一次（老实现是每条信号问一次，惯常返回同一份数组）
    val vectors = HashMap<String, FloatArray>()
    for (s in signals) {
        if (s.key in vectors) continue
        vectorOf(s.key)?.let { vectors[s.key] = it }
    }
    return RustEmbed.tasteProfile(signals.toRustSignals(), model, vectors, now).toTasteProfile()
}

/**
 * 每个模型各造一份画像（CPU / NPU / 封面三个向量空间分开）。
 *
 * [vectorsByModel] 是 `model → (key → 向量)`；缺的模型不会出现在返回值里（没有向量就算不出画像）。
 */
fun buildTasteProfiles(
    signals: List<BehaviorSignal>,
    vectorsByModel: Map<String, Map<String, FloatArray>>,
    now: Long,
): Map<String, TasteProfile> =
    RustEmbed.tasteProfiles(signals.toRustSignals(), vectorsByModel, now)
        .mapValues { (_, profile) -> profile.toTasteProfile() }

/** 过边界用的信号形状（权重由 Rust 现算，这里给 0 占位，Rust 那边不看它）。 */
private fun List<BehaviorSignal>.toRustSignals(): List<RustEmbed.Signal> =
    map { RustEmbed.Signal(it.key, it.kind.name, it.at, it.strength, it.platform?.key, 0f) }

private fun RustEmbed.Profile.toTasteProfile(): TasteProfile = TasteProfile(
    model = model,
    positive = positive,
    negative = negative,
    positiveWeight = positiveWeight,
    negativeWeight = negativeWeight,
    positiveSignals = positiveSignals,
    negativeSignals = negativeSignals,
)
