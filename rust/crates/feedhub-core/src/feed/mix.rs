//! `app/.../feed/FeedMix.kt` 里**还没进 Rust** 的那一部分：比例配额（`QuotaConfig` / `quotas` /
//! `applyQuota`）和个性化**过滤**原型（`PersonalizationConfig` / `filterByScore` / `mixBatch`）。
//!
//! 已经进 Rust 的是同一个文件里的另外三样，别在这里再写一遍：
//! - `capPerPlatform` → `brain::order::cap`；
//! - `InterleaveOrder` / `TimeOrder` → `brain::order::{interleave, time_order}`；
//! - `EmbeddingScore.blend` / `ScoredOrder` / `blendIndices` → `brain::order::{blend, blend_order}`。
//!
//! 这一部分的口径：
//! - 个性化**只排序、不删内容**；"分低的去掉"这一路（[`PersonalizationConfig`]）是**调研原型**，
//!   **没有调用方**，代码留着（有测试），别删，也别在没有用户新决定之前接进信息流；
//! - 比例配额（[`quotas`] / [`apply_quota`]）也是原型：`batch_target = 0`（现在）等于不限；
//! - 配额是**截断**，不是重排——每个平台取它自己推荐顺序里的前几条，平台内部顺序不变；
//!   最后 [mix_batch] 仍按发布时间整批倒序。
//!
//! **Rust 这张表里没有 Kotlin 的 `FeedItem`**：这里只看 `key` / 平台 / 发布时间（排序）和
//! 打分器给的分数（`Option<f32>`，`None` = 这个模型还没法给它打分 → 当"不过滤"处理）。
//! 上屏顺序用**输入下标**返回，宿主拿下标回自己的列表取（和 `brain` 的约定一样）。

use std::collections::HashMap;

/// 一条候选里这一层要看的字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixFeedItem {
    /// 跨平台唯一键（`FeedItem.key`）。
    pub key: String,
    /// 平台 id。
    pub platform: String,
    /// 发布时间（Unix 秒）；`None` = 没有发布时间（按时间排时**排在最后**）。
    pub published_at: Option<i64>,
}

/// 一批的目标条数与各平台权重（`QuotaConfig`）。
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaConfig {
    /// 一批的目标条数；0 = 不限（现在就是这个）。
    pub batch_target: i32,
    /// 各平台的相对权重（保持顺序）。没写的按 1.0；`<= 0` 表示这个平台不占这一批的位置。
    pub weights: Vec<(String, f32)>,
    /// 每个平台最少留几条（保底）。`batch_target` 比"保底 × 平台数"还小时，以保底为准（总量会超出目标）。
    pub floor_per_platform: i32,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            batch_target: 0,
            weights: Vec::new(),
            floor_per_platform: 3,
        }
    }
}

/// 个性化**过滤**的参数（默认关；见模块说明：这一路暂不接）。
#[derive(Debug, Clone, PartialEq)]
pub struct PersonalizationConfig {
    pub enabled: bool,
    pub min_positives: i32,
    pub min_positives_per_platform: i32,
    pub threshold: f32,
    pub exploration_ratio: f32,
    pub max_drop_ratio: f32,
}

impl Default for PersonalizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_positives: 150,
            min_positives_per_platform: 5,
            threshold: 0.35,
            exploration_ratio: 0.2,
            max_drop_ratio: 0.4,
        }
    }
}

/// 一批怎么筛（`MixConfig`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MixConfig {
    pub quota: QuotaConfig,
    pub personalization: PersonalizationConfig,
}

impl MixConfig {
    /// `quota` 缺省时用 [`QuotaConfig::default`]，但 `MixConfig::default()` 里 `quota` 是 `0`——
    /// 和 Kotlin 的 `MixConfig(quota = QuotaConfig(), ...)` 一样，默认 `batch_target = 0`（不限）。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 行为摘要（纯数据；查库在宿主那边）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorSummary {
    /// 正样本总数（点开 + 收藏 + 看完），用来判冷启动。
    pub positives: i32,
    /// 各平台的正样本数，用来判平台级冷启动。
    pub positives_by_platform: Vec<(String, i32)>,
}

impl BehaviorSummary {
    fn positives_of(&self, platform: &str) -> i32 {
        self.positives_by_platform
            .iter()
            .find(|(p, _)| p == platform)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }
}

/// 一批筛完的结果（顺序 = 上屏顺序，用输入下标表示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixResult {
    /// 上屏顺序（输入里的下标）。
    pub order: Vec<u32>,
    pub dropped_low_score: u32,
    pub dropped_by_quota: u32,
    /// 每个平台留了几条。
    pub per_platform: Vec<(String, u32)>,
    /// 筛完不够 `batch_target`（调用方可以据此拉下一页；平台到底了就别拉）。
    pub short_of_target: bool,
}

/// 平台顺序的下标：**保持输入里第一次出现的顺序**（Kotlin 的 `LinkedHashMap`）。
fn platform_order(items: &[MixFeedItem]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.iter().any(|p| *p == item.platform) {
            out.push(item.platform.clone());
        }
    }
    out
}

/// Kotlin `Float.coerceIn(min, max)`：两个比较都不成立就原样返回（NaN 会原样穿过去）。
fn coerce_in(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Kotlin `Int.coerceAtLeast(min)`。
fn at_least(value: i32, min: i32) -> i32 {
    if value < min {
        min
    } else {
        value
    }
}

/// Kotlin `ceil(x).toInt()`（NaN → 0，和 JVM 的 `Math.ceil` + `toInt()` 一样）。
fn ceil_to_int(value: f32) -> i32 {
    let c = value.ceil();
    if c.is_nan() {
        0
    } else if c >= i32::MAX as f32 {
        i32::MAX
    } else if c <= i32::MIN as f32 {
        i32::MIN
    } else {
        c as i32
    }
}

/// 按发布时间倒序、**没有发布时间的排最后**，时间相同的保持输入顺序（稳定排序）。
///
/// 就是 Kotlin 的 `sortedWith(compareByDescending<FeedItem, Long?>(nullsFirst()) { it.publishedAt })`。
fn by_time_desc(indices: &mut [u32], items: &[MixFeedItem]) {
    indices.sort_by(|&a, &b| {
        let (x, y) = (items[a as usize].published_at, items[b as usize].published_at);
        match (x, y) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(p), Some(q)) => q.cmp(&p),
        }
    });
}

/// 每个平台留几条：先按权重分（`target × w / Σw`），再夹在保底和"这个平台这一批有几条"之间；
/// 总量超了就从拿得最多的平台往下砍（不砍到保底以下），不够就按"离份额还差多少"从大到小往上加（水位法）。
///
/// `sizes` 的顺序就是返回值的顺序（平台第一次出现的顺序）。注意：这是**截断**，不是重排。
pub fn quotas(
    sizes: &[(String, i32)],
    target: i32,
    weights: &[(String, f32)],
    floor_per_platform: i32,
) -> Vec<(String, i32)> {
    if sizes.is_empty() {
        return Vec::new();
    }
    if target <= 0 {
        return sizes.to_vec();
    }
    // 权重表：只留 sizes 里有的平台、权重 > 0 的（没写的按 1.0）
    let weight_of = |platform: &str| -> f32 {
        weights
            .iter()
            .find(|(p, _)| p == platform)
            .map(|(_, w)| *w)
            .unwrap_or(1.0)
            .max(0.0)
    };
    let effective: Vec<(String, f32)> = sizes
        .iter()
        .map(|(p, _)| (p.clone(), weight_of(p)))
        .filter(|(_, w)| *w > 0.0)
        .collect();
    // 一个权重都没给（配置写错了）：退回"不限"，不要因此把一批清空
    if effective.is_empty() {
        return sizes.to_vec();
    }
    let total_weight: f32 = effective.iter().map(|(_, w)| *w).sum();
    let floor = at_least(floor_per_platform, 0);
    let size_of = |platform: &str| sizes.iter().find(|(p, _)| p == platform).map(|(_, n)| *n).unwrap_or(0);
    let mut out: Vec<(String, i32)> = sizes.iter().map(|(p, _)| (p.clone(), 0)).collect();
    fn get(out: &[(String, i32)], p: &str) -> i32 {
        out.iter().find(|(k, _)| k == p).map(|(_, v)| *v).unwrap_or(0)
    }
    fn set(out: &mut [(String, i32)], p: &str, v: i32) {
        if let Some(slot) = out.iter_mut().find(|(k, _)| k == p) {
            slot.1 = v;
        }
    }
    // 保底先给（给不起的按这个平台有几条给）
    for (platform, size) in sizes {
        if effective.iter().any(|(p, _)| p == platform) {
            set(&mut out, platform, floor.min(*size));
        }
    }
    // 再按份额补齐（份额已经低于保底的平台先不动它，后面还能被"水位"加）
    for (platform, weight) in &effective {
        let share = target as f32 * (weight / total_weight);
        let rounded = (share + 0.5).floor();
        let rounded = if rounded.is_nan() { 0 } else { rounded as i32 };
        let want = size_of(platform).min(floor.max(rounded));
        let current = get(&out, platform);
        set(&mut out, platform, current.max(want));
    }
    // 超了：从拿得最多的开始砍（不砍到保底以下）；同分砍平台 id 更小的（Kotlin 的 thenByDescending）
    let mut sum: i32 = out.iter().map(|(_, v)| *v).sum();
    while sum > target {
        let mut cand: Option<(String, i32)> = None;
        for (platform, value) in out.iter() {
            if *value <= floor || !effective.iter().any(|(p, _)| p == platform) {
                continue;
            }
            cand = match cand {
                None => Some((platform.clone(), *value)),
                Some((best_p, best_v)) => {
                    if *value > best_v || (*value == best_v && *platform < best_p) {
                        Some((platform.clone(), *value))
                    } else {
                        Some((best_p, best_v))
                    }
                }
            };
        }
        let Some((platform, value)) = cand else { break };
        set(&mut out, &platform, value - 1);
        sum -= 1;
    }
    // 不够：按"离份额还差多少"从大到小往上加（还要有位置）；同分选平台 id 更小的
    while sum < target {
        let mut cand: Option<(String, f32)> = None;
        for (platform, weight) in effective.iter() {
            if get(&out, platform) >= size_of(platform) {
                continue;
            }
            let share = target as f32 * (weight / total_weight);
            let gap = share - get(&out, platform) as f32;
            cand = match cand {
                None => Some((platform.clone(), gap)),
                Some((best_p, best_gap)) => {
                    if kt_float_cmp(gap, best_gap) == std::cmp::Ordering::Greater
                        || (kt_float_cmp(gap, best_gap) == std::cmp::Ordering::Equal && *platform < best_p)
                    {
                        Some((platform.clone(), gap))
                    } else {
                        Some((best_p, best_gap))
                    }
                }
            };
        }
        let Some((platform, _)) = cand else { break };
        let next = get(&out, &platform) + 1;
        set(&mut out, &platform, next);
        sum += 1;
    }
    out
}

/// Kotlin `Float.compare`（`compareBy` 比 Float 时用的就是这个）：先比大小、相等再比 IEEE 位模式。
fn kt_float_cmp(a: f32, b: f32) -> std::cmp::Ordering {
    if a < b {
        std::cmp::Ordering::Less
    } else if a > b {
        std::cmp::Ordering::Greater
    } else {
        let bits = |v: f32| {
            if v.is_nan() {
                0x7fc0_0000u32 as i32
            } else {
                v.to_bits() as i32
            }
        };
        bits(a).cmp(&bits(b))
    }
}

/// 按 [`quotas`] 截断：每个平台取自己顺序里的前几条（**保持输入顺序**：先出现的平台先出）。
pub fn apply_quota(items: &[MixFeedItem], config: &QuotaConfig) -> Vec<u32> {
    let order = platform_order(items);
    let mut groups: Vec<Vec<u32>> = vec![Vec::new(); order.len()];
    for (i, item) in items.iter().enumerate() {
        if let Some(slot) = order.iter().position(|p| *p == item.platform) {
            groups[slot].push(i as u32);
        }
    }
    let sizes: Vec<(String, i32)> = order
        .iter()
        .zip(groups.iter())
        .map(|(p, g)| (p.clone(), g.len() as i32))
        .collect();
    let q = quotas(&sizes, config.batch_target, &config.weights, config.floor_per_platform);
    let mut out = Vec::with_capacity(items.len());
    for (slot, group) in groups.iter().enumerate() {
        let n = q
            .iter()
            .find(|(p, _)| *p == order[slot])
            .map(|(_, n)| *n)
            .unwrap_or(group.len() as i32);
        let n = if n < 0 { 0 } else { n as usize };
        out.extend(group.iter().take(n).copied());
    }
    out
}

/// 个性化过滤的结果：留下的下标（输入顺序）+ 去掉几条。
struct Scored {
    kept: Vec<u32>,
    dropped: u32,
}

/// 个性化过滤（[`PersonalizationConfig`]）。几个"不删"的例外（都是防误杀 / 防越推越窄）：
/// 1. 正样本总数不到 `min_positives`、没有打分器、或者开关关着 —— 整段跳过（冷启动）；
/// 2. 某个平台的正样本不到 `min_positives_per_platform` —— **这个平台的条目全留**（平台冷启动）；
/// 3. 探索位：按发布时间最新的 `ceil(条数 × exploration_ratio)` 条无条件留下；
/// 4. 打分器返回 `None` 的留下（模型还没覆盖到，不能当低分）；
/// 5. 丢的上限：留下的少于 `ceil(条数 × (1 - max_drop_ratio))` 时，按分数从高到低把去掉的补回来。
///
/// 分数相同的时候**不按分数重排**：输入顺序原样保留（最终排序由 [`mix_batch`] 按时间做）。
fn filter_by_score(
    items: &[MixFeedItem],
    behavior: &BehaviorSummary,
    config: &PersonalizationConfig,
    scores: Option<&[Option<f32>]>,
) -> Scored {
    let cold = !config.enabled
        || scores.is_none()
        || behavior.positives < config.min_positives;
    if cold || items.is_empty() {
        return Scored {
            kept: (0..items.len() as u32).collect(),
            dropped: 0,
        };
    }
    let scores = scores.unwrap();
    let score_of = |index: usize| scores.get(index).copied().flatten();

    // 探索位：按发布时间取最新的那些（没有时间的排在最后，不会被当成"新"）
    let explore_count = ceil_to_int(
        items.len() as f32 * coerce_in(config.exploration_ratio, 0.0, 1.0),
    );
    let mut by_time: Vec<u32> = (0..items.len() as u32).collect();
    by_time_desc(&mut by_time, items);
    let explore: Vec<&str> = by_time
        .iter()
        .take(if explore_count < 0 { 0 } else { explore_count as usize })
        .map(|&i| items[i as usize].key.as_str())
        .collect();

    let keep_by_score = |index: usize| -> bool {
        if behavior.positives_of(&items[index].platform) < config.min_positives_per_platform {
            return true;
        }
        match score_of(index) {
            None => true,
            Some(s) => s >= config.threshold,
        }
    };

    let mut kept: Vec<u32> = Vec::with_capacity(items.len());
    for i in 0..items.len() {
        if explore.iter().any(|k| *k == items[i].key) || keep_by_score(i) {
            kept.push(i as u32);
        }
    }
    let least_keep = ceil_to_int(
        items.len() as f32 * (1.0 - coerce_in(config.max_drop_ratio, 0.0, 1.0)),
    );
    let least_keep = if least_keep < 0 { 0 } else { least_keep as usize };
    if kept.len() >= least_keep {
        return Scored {
            dropped: (items.len() - kept.len()) as u32,
            kept,
        };
    }

    // 丢太多了：把去掉的按分数从高到低补回来（同分按输入顺序，稳定）
    let kept_keys: Vec<&str> = kept.iter().map(|&i| items[i as usize].key.as_str()).collect();
    let mut backfill: Vec<u32> = (0..items.len() as u32)
        .filter(|i| !kept_keys.contains(&items[*i as usize].key.as_str()))
        .collect();
    // Kotlin: compareByDescending { scores[key] }（nullsFirst → 没有分数的排最后），稳定排序
    backfill.sort_by(|&a, &b| match (score_of(a as usize), score_of(b as usize)) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => kt_float_cmp(y, x),
    });
    let need = least_keep.saturating_sub(kept.len());
    let revived: Vec<&str> = backfill
        .iter()
        .take(need)
        .map(|&i| items[i as usize].key.as_str())
        .collect();
    // 结果顺序仍按输入顺序（不按分数排）
    let kept_now: Vec<u32> = (0..items.len() as u32)
        .filter(|i| {
            let key = items[*i as usize].key.as_str();
            kept_keys.contains(&key) || revived.contains(&key)
        })
        .collect();
    let dropped = (items.len() - kept_now.len()) as u32;
    Scored {
        kept: kept_now,
        dropped,
    }
}

/// 把一批候选筛成上屏的一批（`mixBatch`）。
///
/// `candidates` 的顺序有讲究：**同一个平台内部要保留平台自己的推荐顺序**（配额从前面截），
/// 跨平台先来后到无所谓。`scores` 是每条的分数（0～1，和 `candidates` 一一对应；`None` = 没有打分器）；
/// 元素为 `None` 表示"这个模型还没法给它打分"，当"不过滤"处理（宁可放过）。
pub fn mix_batch(
    candidates: &[MixFeedItem],
    behavior: &BehaviorSummary,
    config: &MixConfig,
    scores: Option<&[Option<f32>]>,
) -> MixResult {
    // 批内去重（同一个 key 只留第一次出现的，和 BatchMerge.prepare 一样；跨批去重不在这里管）
    let mut seen: Vec<&str> = Vec::with_capacity(candidates.len());
    let mut unique: Vec<usize> = Vec::with_capacity(candidates.len());
    for (i, item) in candidates.iter().enumerate() {
        if seen.iter().any(|k| *k == item.key) {
            continue;
        }
        seen.push(item.key.as_str());
        unique.push(i);
    }
    let unique_items: Vec<MixFeedItem> = unique.iter().map(|&i| candidates[i].clone()).collect();
    let unique_scores: Option<Vec<Option<f32>>> =
        scores.map(|list| unique.iter().map(|&i| list.get(i).copied().flatten()).collect());

    // 1) 个性化（分低的不进；冷启动 / 平台冷启动 / 探索位 / 丢的上限都在 filter_by_score 里）
    let scored = filter_by_score(
        &unique_items,
        behavior,
        &config.personalization,
        unique_scores.as_deref(),
    );
    let scored_items: Vec<MixFeedItem> = scored
        .kept
        .iter()
        .map(|&i| unique_items[i as usize].clone())
        .collect();

    // 2) 比例配额（从每个平台自己的顺序里截前几条），然后按发布时间整批倒序
    let quota_indices: Vec<u32> = if config.quota.batch_target > 0 {
        apply_quota(&scored_items, &config.quota)
    } else {
        (0..scored_items.len() as u32).collect()
    };
    let quota_len = quota_indices.len();
    let mut sorted = quota_indices;
    by_time_desc(&mut sorted, &scored_items);

    let mut per_platform: Vec<(String, u32)> = Vec::new();
    for &i in &sorted {
        let platform = &scored_items[i as usize].platform;
        match per_platform.iter_mut().find(|(p, _)| p == platform) {
            Some((_, n)) => *n += 1,
            None => per_platform.push((platform.clone(), 1)),
        }
    }
    // 上屏顺序是 `scored_items` 里的下标 → 换回 `candidates` 的下标
    let order: Vec<u32> = sorted
        .iter()
        .map(|&i| unique[scored.kept[i as usize] as usize] as u32)
        .collect();
    MixResult {
        dropped_low_score: scored.dropped,
        dropped_by_quota: (scored_items.len() - quota_len) as u32,
        per_platform,
        short_of_target: config.quota.batch_target > 0
            && (sorted.len() as i32) < config.quota.batch_target,
        order,
    }
}

/// 平台 id → 出现次数（宿主有时候要 `Map<Platform, Int>`；顺序 = 第一次出现）。
pub fn counts_in_order(order: &[(String, u32)]) -> HashMap<String, u32> {
    order.iter().cloned().collect()
}
