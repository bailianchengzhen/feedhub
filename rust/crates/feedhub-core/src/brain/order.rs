//! 批内顺序 + 每平台封顶 + 打分钩子。
//!
//! 一一对应 Kotlin 的 `FeedMix.kt`（`capPerPlatform` / `InterleaveOrder` / `TimeOrder` / `ScoredOrder` /
//! `EmbeddingScore.blend`）和 `FeedState.kt` 的 `BatchMerge.prepare`。行为口径逐条照抄，**一条都没改**：
//!
//! - **穿插**：每个平台按它第一次出现的顺序建一条队列，队内保持平台自己的顺序；某平台这批有 n 条时，
//!   它第 i 条的位置分数 = `(i + 0.5) / n`，全批按位置分数升序；**同分**（不同平台位置分数相等）时
//!   按"这一批的种子打乱后的平台名次"排——打乱用的是 Kotlin `Random(seed)`（[`crate::brain::rng`] 逐位复刻）；
//! - **按时间**：整批按发布时间倒序，没有发布时间的排最后，时间相同的保持输入顺序（稳定排序）；
//! - **打分钩子**：位置分数和相似度加权合成（权重 0 = 原样返回位置分数，顺序逐个位置不变），
//!   合成分数相同时保持原来的先后（稳定排序 + 下标兜底）；
//! - **封顶**：只截断、不重排——每个平台取自己顺序里的前 N 条；被砍掉的条目由调用方记进"当前消费掉"；
//! - **平台序号**（`seen_item.platform_rank`）：这一批里这个平台能上屏的第几条（0 起），**在排序前数**。

use std::cmp::Ordering;
use std::collections::HashMap;

use super::rng;
use super::MixItem;

/// 批内顺序（`FeedOrder` + 关注页的硬规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    /// 关注页：永远按发布时间倒序（关注流本身就是时间线，不受设置影响）。
    Following,
    /// 首页设置成「按时间」。
    Time,
    /// 首页默认：各平台穿插。
    Interleave,
}

impl OrderKind {
    fn is_time(self) -> bool {
        matches!(self, OrderKind::Following | OrderKind::Time)
    }
}

/// 每平台封顶的结果：留下的下标（输入顺序）、对应的平台序号、被砍掉的下标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capped {
    /// 留下的条目：输入里的下标，顺序 = 输入顺序（封顶只截尾巴，不重排）。
    pub kept: Vec<u32>,
    /// 和 [`kept`](Self::kept) 一一对应：这一条在它自己平台里的序号（0 起，封顶之后从 0 数）。
    pub ranks: Vec<u32>,
    /// 被每平台上限砍掉的条目（输入下标，输入顺序）。调用方要把它们记进"当前消费掉"。
    pub dropped: Vec<u32>,
}

/// [`prepare`] 的结果：上屏顺序、每条的平台序号（和 `order` 一一对应）、被砍掉的下标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub order: Vec<u32>,
    pub ranks: Vec<u32>,
    pub dropped: Vec<u32>,
}

/// 每个平台最多留几条，同时数出平台序号（`capPerPlatform` + `BatchMerge.prepare` 的前两步）。
///
/// `max_per_platform <= 0` = 不限（原样全留，但序号照样数）；`ranks` 数的是**这一批里这个平台能上屏的**
/// 第几条，和后面怎么排无关。
pub fn cap(items: &[MixItem], max_per_platform: i32) -> Capped {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    let mut kept = Vec::with_capacity(items.len());
    let mut ranks = Vec::with_capacity(items.len());
    let mut dropped = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = counts.entry(item.platform.as_str()).or_insert(0);
        if max_per_platform <= 0 || *n < max_per_platform as u32 {
            kept.push(i as u32);
            ranks.push(*n);
            *n += 1;
        } else {
            dropped.push(i as u32);
        }
    }
    Capped {
        kept,
        ranks,
        dropped,
    }
}

/// 封顶 + 平台序号 + 排序，一次算完（`BatchMerge.prepare` 里"去重之后"的那一段）。
///
/// 去重还在 Kotlin：`seen` 是 `FeedState` 的状态（跨批、跨页），不搬过来。
pub fn prepare(items: &[MixItem], max_per_platform: i32, kind: OrderKind, seed: i64) -> Prepared {
    let Capped {
        kept,
        ranks,
        dropped,
    } = cap(items, max_per_platform);
    let kept_refs: Vec<&MixItem> = kept.iter().map(|&i| &items[i as usize]).collect();
    let ordered = order_refs(&kept_refs, kind, seed);
    Prepared {
        order: ordered.iter().map(|&j| kept[j as usize]).collect(),
        ranks: ordered.iter().map(|&j| ranks[j as usize]).collect(),
        dropped,
    }
}

/// 批量排序（输入是整批，输出是**输入里的下标**）。
pub fn order(items: &[MixItem], kind: OrderKind, seed: i64) -> Vec<u32> {
    let refs: Vec<&MixItem> = items.iter().collect();
    order_refs(&refs, kind, seed)
}

/// [`order`] 的本体：拿引用排，省掉 [`prepare`] 里那次克隆。
pub fn order_refs(items: &[&MixItem], kind: OrderKind, seed: i64) -> Vec<u32> {
    if items.len() <= 1 {
        return (0..items.len() as u32).collect();
    }
    if kind.is_time() {
        time_order(items)
    } else {
        interleave(items, seed)
    }
}

/// **整批按发布时间倒序**：没有发布时间的排最后，时间相同的保持输入顺序。
fn time_order(items: &[&MixItem]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..items.len() as u32).collect();
    // Rust 的 sort_by 是稳定排序（和 Kotlin 的 sortedWith 一样）：时间相同就保持输入顺序
    order.sort_by(|&a, &b| match (items[a as usize].published_at, items[b as usize].published_at) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => y.cmp(&x),
    });
    order
}

/// **各平台穿插**：位置分数 `(i + 0.5) / n` 升序，同分时按种子打乱后的平台名次。
fn interleave(items: &[&MixItem], seed: i64) -> Vec<u32> {
    // 平台按批内第一次出现的顺序（Kotlin 的 LinkedHashMap 语义）
    let mut platforms: Vec<&str> = Vec::new();
    let mut queues: Vec<Vec<u32>> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let slot = match platforms.iter().position(|p| *p == item.platform.as_str()) {
            Some(slot) => slot,
            None => {
                platforms.push(item.platform.as_str());
                queues.push(Vec::new());
                queues.len() - 1
            }
        };
        queues[slot].push(i as u32);
    }

    // 同分时的先后：用这一批的种子打乱平台顺序（Kotlin Random(seed) 的逐位复刻）
    let shuffled = rng::shuffled_indices(platforms.len(), seed);
    let mut platform_rank = vec![0usize; platforms.len()];
    for (rank, &slot) in shuffled.iter().enumerate() {
        platform_rank[slot] = rank;
    }

    struct Slot {
        index: u32,
        position: f64,
        platform_rank: usize,
    }
    let mut slots: Vec<Slot> = Vec::with_capacity(items.len());
    for (slot, queue) in queues.iter().enumerate() {
        let n = queue.len() as f64;
        for (i, &index) in queue.iter().enumerate() {
            slots.push(Slot {
                index,
                position: (i as f64 + 0.5) / n,
                platform_rank: platform_rank[slot],
            });
        }
    }
    // 位置分数不会出现 NaN（n >= 1、i >= 0），同分再比平台名次
    slots.sort_by(|a, b| {
        a.position
            .partial_cmp(&b.position)
            .unwrap_or(Ordering::Equal)
            .then(a.platform_rank.cmp(&b.platform_rank))
    });
    slots.into_iter().map(|slot| slot.index).collect()
}

/// **打分钩子的合成**（`EmbeddingScore.blend`）：位置分数和相似度加权。
///
/// - `similarity = None`（这条没有向量）→ 原样返回位置分数，不会因为它没向量就被挤到后面；
/// - `weight <= 0`→ 原样返回位置分数；
/// - 两个都夹在 0～1（`coerceIn` 的语义：NaN 原样穿过去，和 Kotlin 一样）。
pub fn blend(position: f32, similarity: Option<f32>, weight: f32) -> f32 {
    let Some(similarity) = similarity else {
        return position;
    };
    if weight <= 0.0 {
        return position;
    }
    let w = coerce_in(weight, 0.0, 1.0);
    position * (1.0 - w) + (1.0 - coerce_in(similarity, 0.0, 1.0)) * w
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

/// **在排好的顺序上叠一层相似度**（`ScoredOrder`）：`position = i / (n - 1)`，和相似度加权后
/// **稳定**排序（同分保持传入顺序）。
///
/// `similarities` 要和 `items` 一一对应（`None` = 这条没有向量）；比 `items` 短的部分当没有向量。
/// `weight <= 0` 或只有一条时直接返回原顺序（**权重 0 时顺序逐个位置不变**，这条是验收条件，
/// `ScoredOrderTest` 在 Kotlin 侧也守着）。
pub fn blend_order(items: &[MixItem], similarities: &[Option<f32>], weight: f32) -> Vec<u32> {
    let n = items.len();
    let order: Vec<u32> = (0..n as u32).collect();
    if weight <= 0.0 || n <= 1 {
        return order;
    }
    let last = (n - 1) as f32;
    let mut scored: Vec<(u32, f32, u32)> = (0..n)
        .map(|i| {
            let position = i as f32 / last;
            let similarity = similarities.get(i).copied().flatten();
            (i as u32, blend(position, similarity, weight), i as u32)
        })
        .collect();
    // 和 Kotlin 的 compareBy { score }.thenBy { 下标 } 一样：同分按下标，结果确定
    scored.sort_by(|a, b| kt_float_cmp(a.1, b.1).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|entry| entry.0).collect()
}

/// Kotlin `Float.compare`（`compareBy` 比 Float 时用的就是这个）：
/// 先比大小，相等再比 IEEE 位模式（`floatToIntBits` 会把所有 NaN 归一成同一个位模式，
/// 所以 NaN 比 `+Inf` 还大、彼此相等；`-0.0` 比 `0.0` 小）。
fn kt_float_cmp(a: f32, b: f32) -> Ordering {
    if a < b {
        Ordering::Less
    } else if a > b {
        Ordering::Greater
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
