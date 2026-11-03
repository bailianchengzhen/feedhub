//! 「口味画像」：把一沓带权重的行为信号压成**两个向量**（正 / 负加权中心），再给卡片打分。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/TasteProfile.kt` 的 Rust 版。
//! 公式、`NEGATIVE_PENALTY`、归一化方式**一个数都没动**（理由见 Kotlin 那边的 KDoc：为什么用加权中心
//! 而不是 top-k、为什么两个向量空间不能混）。这一层只做数学，不碰库、不碰时钟。
//!
//! 逐位对齐的两处细节：
//! - 加权和用 **f64** 累加（Kotlin 是 `DoubleArray`），最后转 f32 再单位化；
//! - 打分里 `posCos - penalty * negCos`、`(raw / span + 1) / 2` 是 **f32** 运算（Kotlin 就是 Float）。

use std::collections::HashMap;

use super::signal::BehaviorSignal;
use super::text::l2_normalize_in_place;

/// 负向中心的排斥强度（固定的设计参数，不跟着数据调；改了要重跑回放并写进结果）。
pub const NEGATIVE_PENALTY: f32 = 0.5;

/// 一个模型的口味画像。
#[derive(Debug, Clone, PartialEq)]
pub struct TasteProfile {
    /// 这个画像是哪个模型的向量空间（`item_embedding.model`）。
    pub model: String,
    /// 正向加权中心（已 L2 归一化）；一个正样本都没有时是 `None`。
    pub positive: Option<Vec<f32>>,
    /// 负向加权中心（已 L2 归一化）；没有负样本时是 `None`。
    pub negative: Option<Vec<f32>>,
    pub positive_weight: f32,
    pub negative_weight: f32,
    pub positive_signals: i32,
    pub negative_signals: i32,
}

impl TasteProfile {
    /// 有没有可用的正向中心（只有负样本的话打不出分）。
    pub fn usable(&self) -> bool {
        self.positive.is_some()
    }

    /// 一条卡片对这个画像的相似度（0～1，0.5 = 中性）。
    ///
    /// `model` 给了就必须和画像的模型一致（两个向量空间不能混着比，不一致返回 `None`）。
    /// `vec` 是模型的**原始输出**（不必先归一化；全零 / 空 / 维数不对返回 `None`）。
    pub fn similarity(
        &self,
        vec: Option<&[f32]>,
        model: Option<&str>,
        negative_penalty: f32,
    ) -> Option<f32> {
        if let Some(model) = model {
            if model != self.model {
                return None;
            }
        }
        let pos = self.positive.as_deref()?;
        let vec = vec?;
        if vec.is_empty() || vec.len() != pos.len() {
            return None;
        }
        let v = unit(vec)?;
        let pos_cos = dot(&v, pos).clamp(-1.0, 1.0);
        let penalty = if self.negative.is_some() {
            negative_penalty.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let neg_cos = match self.negative.as_deref() {
            Some(neg) => dot(&v, neg).clamp(-1.0, 1.0),
            None => 0.0,
        };
        let raw = pos_cos - penalty * neg_cos;
        let span = 1.0 + penalty;
        Some(((raw / span + 1.0) / 2.0).clamp(0.0, 1.0))
    }
}

/// 从信号 + 向量造一个模型的口味画像。
///
/// `vectors` 是"这个模型下 key → 向量"（没算过 / 别的模型就别放进来，缺的会被跳过并计数）。
pub fn build_profile(
    signals: &[BehaviorSignal],
    model: &str,
    vectors: &HashMap<String, Vec<f32>>,
    now: i64,
) -> TasteProfile {
    let mut pos_sum: Option<Vec<f64>> = None;
    let mut neg_sum: Option<Vec<f64>> = None;
    let mut pos_w = 0.0f64;
    let mut neg_w = 0.0f64;
    let mut pos_count = 0i32;
    let mut neg_count = 0i32;

    for s in signals {
        let w = s.effective_weight(now);
        if w == 0.0 {
            continue;
        }
        let raw = match vectors.get(&s.key) {
            Some(v) => v,
            None => continue,
        };
        let v = match unit(raw) {
            Some(v) => v,
            None => continue,
        };
        let weight = (w as f64).abs();
        let sum = if w > 0.0 {
            pos_sum.get_or_insert_with(|| vec![0.0f64; v.len()])
        } else {
            neg_sum.get_or_insert_with(|| vec![0.0f64; v.len()])
        };
        if sum.len() != v.len() {
            // 维数对不上（换了模型 / 坏数据）：跳过，不混着加
            continue;
        }
        for i in 0..v.len() {
            sum[i] += weight * v[i] as f64;
        }
        if w > 0.0 {
            pos_w += weight;
            pos_count += 1;
        } else {
            neg_w += weight;
            neg_count += 1;
        }
    }

    TasteProfile {
        model: model.to_string(),
        positive: pos_sum.and_then(|sum| to_unit_f32(&sum)),
        negative: neg_sum.and_then(|sum| to_unit_f32(&sum)),
        positive_weight: pos_w as f32,
        negative_weight: neg_w as f32,
        positive_signals: pos_count,
        negative_signals: neg_count,
    }
}

/// 每个模型各造一份画像（CPU / NPU / 封面三个向量空间分开，绝不混着比）。
pub fn build_profiles(
    signals: &[BehaviorSignal],
    vectors_by_model: &HashMap<String, HashMap<String, Vec<f32>>>,
    now: i64,
) -> Vec<TasteProfile> {
    let mut models: Vec<&String> = vectors_by_model.keys().collect();
    models.sort_by(|a, b| super::text::cmp_utf16(a, b));
    models
        .into_iter()
        .map(|model| build_profile(signals, model, &vectors_by_model[model], now))
        .collect()
}

/// 单位化一份浮点向量（拷贝，不改入参）；全零 / 空返回 `None`。
fn unit(vec: &[f32]) -> Option<Vec<f32>> {
    let mut sum = 0.0f64;
    for v in vec {
        let v = *v as f64;
        sum += v * v;
    }
    if sum <= 0.0 {
        return None;
    }
    let inv = (1.0f64 / sum.sqrt()) as f32;
    Some(vec.iter().map(|v| *v * inv).collect())
}

fn to_unit_f32(vec: &[f64]) -> Option<Vec<f32>> {
    let floats: Vec<f32> = vec.iter().map(|v| *v as f32).collect();
    unit(&floats)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut s = 0.0f64;
    for i in 0..a.len().min(b.len()) {
        s += a[i] as f64 * b[i] as f64;
    }
    s as f32
}

/// 归一化一个向量，给调用方复用（`l2NormalizeInPlace` 的 Rust 入口）。
pub fn normalize_in_place(vec: &mut [f32]) {
    l2_normalize_in_place(vec);
}
