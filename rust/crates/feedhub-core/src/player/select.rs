//! **选流**：从一份 `PlayInfo` 里挑"这次播哪一路"（`app/.../video/StreamSelection.kt` 的 Rust 版）。
//!
//! 这一层的输入只有"平台给了什么"（[`crate::play::PlayInfo`]）+ "设置里选的编码偏好 / 默认清晰度" +
//! "这台机器能解什么"（[`crate::player::DecodeCapability`]）。**不发请求、不读时钟、不碰 IO**，
//! 同一份输入两次调用的结果一定一样。
//!
//! 口径逐条照抄 Kotlin（改之前先读 `StreamSelection.kt` 的注释，那里写着每条规则的出处）：
//!
//! 1. **自动顺序**：只把"确认能硬解 **且** 这个负载下实测能实时"的编码排前面，内部按
//!    AV1 > HEVC > AVC；其余跟在后面按 AVC / HEVC / AV1（软解最省的在前）。
//!    **负载未知就不额外限制**（不猜）；
//! 2. **清晰度**：自动最高 = 能给的最高一档，但 **HDR / 杜比视界（125 / 126）不当"最高"**——
//!    它们要专门的显示链路；手动选了才用。要的那一档没给流就**先往低找、再往高找**；
//! 3. **同一档里**按编码顺序挑，编码相同取码率高的；
//! 4. **音频**只挑一条：AAC 里码率最高的（兼容最好），没有 AAC 才按码率，挑不出就 `None`；
//! 5. **音画一体**（无 DASH）：先定清晰度，再按编码顺序（没标编码的按 AVC 对待），最后按码率。
//!
//! 和 Kotlin 的一处**故意的行为对齐**：`select` 在"没有视频轨"时才走音画一体那条路；有视频轨但
//! 挑不出清晰度（`pick_quality` 返回 `None`）就是 `None`，不会掉回 `progressive`。

use crate::play::{AudioStream, PlayInfo, ProgressiveStream, Quality, VideoCodec, VideoStream};
use crate::player::ledger::{pixel_rate_of, DecodeCapability};

/// 没有硬解信息（或一个都不能硬解）时"自动"的编码顺序：软解的话 AVC 最省（兼容最好、最容易解）；
/// HEVC 其次；AV1 软解最吃力，放最后。
pub const DESKTOP_AUTO: [VideoCodec; 3] = [VideoCodec::Avc, VideoCodec::Hevc, VideoCodec::Av1];

/// 能硬解的编码之间的偏好：AV1 > HEVC > AVC。同一档清晰度 AV1 压缩效率最高、
/// 码率最低，硬解时三者 CPU 差不多。
///
/// 所以 [`auto_order`] 会按 [`DecodeCapability`] 里的实测上限把跟不上的编码挪到后面。
const HARDWARE_PREFERENCE: [VideoCodec; 3] = [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::Avc];

/// 1080P 的清晰度 id（B站 / 各家共用的数字口径）。
pub const Q_1080: i32 = 80;

/// 720P 的清晰度 id。
pub const Q_720: i32 = 64;

/// 杜比视界（125）、HDR（126）：**不当成"自动最高"**（要专门的色彩处理），手动选了才用。
pub const SPECIAL_QUALITIES: [i32; 2] = [125, 126];

fn is_special(q: i32) -> bool {
    SPECIAL_QUALITIES.contains(&q)
}

/// 设置里的编码偏好（Kotlin `CodecPreference`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecPreference {
    Auto,
    Avc,
    Hevc,
    Av1,
}

/// 设置里的默认清晰度（Kotlin `DefaultQuality`）。枚举顺序就是"从高到低"
/// （`limitQuality` 用 `maxOf(setting.ordinal, limit.ordinal)` 取"更低"的那一档，靠的就是这个顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DefaultQuality {
    Highest,
    P1080,
    P720,
}

/// 选出来的一路流：DASH（视频轨 + 音频轨）或音画一体的整段文件。
///
/// 字段是**下标**而不是拷贝：Kotlin 侧拿它回自己的 `PlayInfo` 取对象，省一次跨 FFI 的深拷贝
/// （一份 B站 的 `PlayInfo` 可能有几十条流）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    /// 被选中的清晰度（`DASH` / 音画一体都有；音画一体那几档可能没标清晰度，就是 `None`）。
    pub quality: Option<i32>,
    /// `info.video` 里的下标（音画一体是 `None`）。
    pub video: Option<u32>,
    /// `info.audio` 里的下标（没有音频轨 / 没挑出来就是 `None`）。
    pub audio: Option<u32>,
    /// `info.progressive` 里的下标（DASH 是 `None`）。
    pub progressive: Option<u32>,
}

impl Selection {
    fn dash(quality: i32, video: u32, audio: Option<u32>) -> Self {
        Self {
            quality: Some(quality),
            video: Some(video),
            audio,
            progressive: None,
        }
    }

    fn progressive(quality: Option<i32>, index: u32) -> Self {
        Self {
            quality,
            video: None,
            audio: None,
            progressive: Some(index),
        }
    }
}

/// 清晰度菜单里的一项：`enabled == false`（这次没给流，比如没登录 / 不是大会员）就灰掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityOption {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
}

/// "自动"的编码顺序（Kotlin `StreamSelection.autoOrder`）。
///
/// `pixels_per_second` 是这一路流的负载（宽 × 高 × 帧率），`None` = 不知道，那就只按"能不能硬解"排
/// （不额外限制）。用户手动指定的编码不受影响（见 [`codec_order`]）。
pub fn auto_order(capability: &DecodeCapability, pixels_per_second: Option<u64>) -> Vec<VideoCodec> {
    let mut front: Vec<VideoCodec> = HARDWARE_PREFERENCE
        .iter()
        .copied()
        .filter(|c| capability.hardware.contains(c) && capability.smooth_at(*c, pixels_per_second))
        .collect();
    for c in DESKTOP_AUTO {
        if !front.contains(&c) {
            front.push(c);
        }
    }
    front
}

/// 编码优先级：用户指定了就先试它，再按 `auto` 的顺序退；最后是 OTHER。
pub fn codec_order(preference: CodecPreference, auto: &[VideoCodec]) -> Vec<VideoCodec> {
    let mut out: Vec<VideoCodec> = Vec::with_capacity(auto.len() + 5);
    match preference {
        CodecPreference::Auto => {}
        CodecPreference::Avc => out.push(VideoCodec::Avc),
        CodecPreference::Hevc => out.push(VideoCodec::Hevc),
        CodecPreference::Av1 => out.push(VideoCodec::Av1),
    }
    for c in auto {
        if !out.contains(c) {
            out.push(*c);
        }
    }
    // `VideoCodec.entries`（Kotlin）的顺序：AVC / HEVC / AV1 / OTHER
    for c in [
        VideoCodec::Avc,
        VideoCodec::Hevc,
        VideoCodec::Av1,
        VideoCodec::Other,
    ] {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// 默认清晰度想要哪一档。
///
/// 自动最高 = 能给的最高一档（跳过 HDR / 杜比视界）；一档都没有时退回"全部里的最高"（此时连
/// 125 / 126 也算——`available` 只有那两档时没有别的可选，Kotlin 的 `?: available.maxOrNull()` 就是这个意思）。
pub fn target_quality(preference: DefaultQuality, available: &[i32]) -> Option<i32> {
    match preference {
        DefaultQuality::Highest => {
            let normal = available.iter().copied().filter(|q| !is_special(*q)).max();
            normal.or_else(|| available.iter().copied().max())
        }
        DefaultQuality::P1080 => Some(Q_1080),
        DefaultQuality::P720 => Some(Q_720),
    }
}

/// 在 `available` 里找最接近 `target` 的一档：先找**不高于**它的最高一档，没有就找**高于**它的最低一档。
pub fn pick_quality(available: &[i32], target: Option<i32>) -> Option<i32> {
    if available.is_empty() {
        return None;
    }
    let Some(target) = target else {
        return available.iter().copied().max();
    };
    if available.contains(&target) {
        return Some(target);
    }
    let pool: Vec<i32> = {
        let normal: Vec<i32> = available.iter().copied().filter(|q| !is_special(*q)).collect();
        if normal.is_empty() {
            available.to_vec()
        } else {
            normal
        }
    };
    pool.iter().copied().filter(|q| *q <= target).max().or_else(|| {
        pool.iter()
            .copied()
            .filter(|q| *q > target)
            .min()
    })
}

/// 音频：AAC（`codecs` 以 `mp4a` 开头）里码率最高的；没有 AAC 就取码率最高的。
///
/// 返回下标。Kotlin 的 `maxByOrNull` 在**码率相同**时返回**第一个**，这里也一样
/// （`>` 而不是 `>=`）。
pub fn pick_audio(audio: &[AudioStream]) -> Option<u32> {
    let mut best_aac: Option<(u32, i64)> = None;
    let mut best_any: Option<(u32, i64)> = None;
    for (i, a) in audio.iter().enumerate() {
        let bw = a.bandwidth.unwrap_or(0);
        let i = i as u32;
        if best_any.map(|(_, b)| bw > b).unwrap_or(true) {
            best_any = Some((i, bw));
        }
        let is_aac = a
            .codecs
            .as_deref()
            .map(|c| c.starts_with("mp4a"))
            .unwrap_or(false);
        if is_aac && best_aac.map(|(_, b)| bw > b).unwrap_or(true) {
            best_aac = Some((i, bw));
        }
    }
    best_aac.or(best_any).map(|(i, _)| i)
}

/// 这一档清晰度该用的自动顺序：有 `capability` 就按它的实测上限（负载取这一档里**最重**的一条），
/// 否则用调用方给的 `auto`。
fn order_for(
    candidates: &[&VideoStream],
    auto: &[VideoCodec],
    capability: Option<&DecodeCapability>,
) -> Vec<VideoCodec> {
    let Some(capability) = capability else {
        return auto.to_vec();
    };
    let load = candidates
        .iter()
        .filter_map(|s| pixel_rate_of(s.width, s.height, s.frame_rate.as_deref()))
        .max();
    auto_order(capability, load)
}

/// 音画一体：先定清晰度，同一档有几条时按编码顺序挑（本机能硬解的靠前），同编码取码率高的；
/// 没标编码的按 AVC 对待。
fn pick_progressive(
    list: &[ProgressiveStream],
    target: Option<i32>,
    order: &[VideoCodec],
) -> Option<u32> {
    if list.is_empty() {
        return None;
    }
    let available: Vec<i32> = list.iter().filter_map(|p| p.quality).collect();
    let q = pick_quality(&available, target);
    let candidates: Vec<(u32, &ProgressiveStream)> = list
        .iter()
        .enumerate()
        .map(|(i, p)| (i as u32, p))
        .filter(|(_, p)| match q {
            None => true,
            Some(q) => p.quality == Some(q),
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // `compareBy { order.indexOf(codec ?: AVC) }.thenByDescending { bandwidth ?: 0 }`
    let rank = |codec: Option<VideoCodec>| -> usize {
        let c = codec.unwrap_or(VideoCodec::Avc);
        order.iter().position(|x| *x == c).unwrap_or(usize::MAX)
    };
    let mut best: Option<(u32, usize, i64)> = None;
    for (i, p) in candidates {
        let key = (rank(p.codec), p.bandwidth.unwrap_or(0));
        let better = match best {
            None => true,
            Some((_, r, b)) => key.0 < r || (key.0 == r && key.1 > b),
        };
        if better {
            best = Some((i, key.0, key.1));
        }
    }
    best.map(|(i, _, _)| i)
}

/// 选一路流。`quality` 是用户在菜单里手动选的档位（没选就 `None`，用 `default_quality`）。
///
/// - `auto`：没有 `capability` 时用的编码顺序（安卓 = 查 MediaCodec 得到的"硬解优先"顺序，
///   桌面 = [`DESKTOP_AUTO`]）；
/// - `capability`：给了它就按**这一档清晰度的实际负载**算自动顺序；
/// - 选不出（一路流都没有）就是 `None`。
pub fn select(
    info: &PlayInfo,
    preference: CodecPreference,
    default_quality: DefaultQuality,
    quality: Option<i32>,
    auto: &[VideoCodec],
    capability: Option<&DecodeCapability>,
) -> Option<Selection> {
    if info.video.is_empty() {
        let available: Vec<i32> = info.progressive.iter().filter_map(|p| p.quality).collect();
        // `quality ?: targetQuality(...)`：手动选了就一定用它（哪怕平台列出的档位里没有）
        let target = match quality {
            Some(q) => Some(q),
            None => target_quality(default_quality, &available),
        };
        let order = codec_order(preference, auto);
        let index = pick_progressive(&info.progressive, target, &order)?;
        return Some(Selection::progressive(
            info.progressive[index as usize].quality,
            index,
        ));
    }
    let available: Vec<i32> = info.video.iter().map(|v| v.quality).collect();
    let distinct: Vec<i32> = {
        let mut seen: Vec<i32> = Vec::with_capacity(available.len());
        for q in &available {
            if !seen.contains(q) {
                seen.push(*q);
            }
        }
        seen
    };
    let wanted = quality.or_else(|| target_quality(default_quality, &distinct));
    let q = pick_quality(&distinct, wanted)?;
    let candidates: Vec<(u32, &VideoStream)> = info
        .video
        .iter()
        .enumerate()
        .map(|(i, v)| (i as u32, v))
        .filter(|(_, v)| v.quality == q)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let refs: Vec<&VideoStream> = candidates.iter().map(|(_, v)| *v).collect();
    let order = codec_order(preference, &order_for(&refs, auto, capability));
    // `sortedWith(compareBy { order.indexOf(codec) }.thenByDescending { bandwidth ?: 0 })`
    let mut best: Option<(u32, usize, i64)> = None;
    for (i, v) in candidates {
        let rank = order
            .iter()
            .position(|c| *c == v.codec)
            .unwrap_or(usize::MAX);
        let bw = v.bandwidth.unwrap_or(0);
        let better = match best {
            None => true,
            Some((_, r, b)) => rank < r || (rank == r && bw > b),
        };
        if better {
            best = Some((i, rank, bw));
        }
    }
    let (video, _, _) = best?;
    Some(Selection::dash(q, video, pick_audio(&info.audio)))
}

/// 清晰度菜单：平台列出的全部档位（高的在前），这次有流的可选，没流的灰掉；
/// 流里有但平台没列的档位也补上（标签就是数字本身）。
pub fn quality_options(info: &PlayInfo) -> Vec<QualityOption> {
    let mut have: Vec<i32> = Vec::new();
    for v in &info.video {
        if !have.contains(&v.quality) {
            have.push(v.quality);
        }
    }
    for p in &info.progressive {
        if let Some(q) = p.quality {
            if !have.contains(&q) {
                have.push(q);
            }
        }
    }
    let mut out: Vec<QualityOption> = Vec::new();
    for q in &info.qualities {
        if out.iter().any(|o| o.id == q.id) {
            continue;
        }
        out.push(QualityOption {
            id: q.id,
            label: q.label.clone(),
            enabled: have.contains(&q.id),
        });
    }
    for q in &have {
        if out.iter().any(|o| o.id == *q) {
            continue;
        }
        out.push(QualityOption {
            id: *q,
            label: format!("{q}"),
            enabled: true,
        });
    }
    // `sortedByDescending { it.id }`（稳定：同 id 已经去重过）
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// 编码在界面上的名字。
pub fn codec_label(codec: Option<VideoCodec>) -> String {
    match codec {
        Some(VideoCodec::Avc) => "AVC".to_string(),
        Some(VideoCodec::Hevc) => "HEVC".to_string(),
        Some(VideoCodec::Av1) => "AV1".to_string(),
        Some(VideoCodec::Other) | None => String::new(),
    }
}

/// 控件里编码标签后面的"硬解 / 软解"。
pub fn decode_label(hardware: Option<bool>) -> String {
    match hardware {
        Some(true) => "硬解".to_string(),
        Some(false) => "软解".to_string(),
        None => String::new(),
    }
}

/// 控件里那个小标签的文字，如 `AVC · 硬解`；编码和解码方式都不知道就空。
pub fn chip_label(codec: Option<VideoCodec>, hardware: Option<bool>) -> String {
    let parts: Vec<String> = [codec_label(codec), decode_label(hardware)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    parts.join(" · ")
}

/// 倍速的档位和显示（`StreamSelection.kt` 末尾的 `PlaybackSpeed`）。
pub struct PlaybackSpeed;

impl PlaybackSpeed {
    /// 可选档位。
    pub const STEPS: [f32; 8] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0];

    /// 默认（正常速度）。
    pub const DEFAULT: f32 = 1.0;

    /// 按钮 / 菜单上的字：`1.0x`、`0.75x`。
    ///
    /// 和 Kotlin 一样用 `round(speed * 100)`：`0.75` 是 `75` → 小数位是 `75`（不是 `7`）；
    /// `1.25` 是 `125` → `25`。整十的小数位只留一位（`1.5x` 而不是 `1.50x`）。
    pub fn label(speed: f32) -> String {
        let hundredths = round_half_up(speed * 100.0) as i32;
        let frac = hundredths % 100;
        let frac_text = if frac % 10 == 0 {
            format!("{}", frac / 10)
        } else {
            format!("{frac:02}")
        };
        format!("{}.{}x", hundredths / 100, frac_text)
    }

    /// 往快 / 慢走一档（`delta` 为 +1 / -1），到头不动；当前值不在档位上就取**最近**的档位再走
    /// （距离相同时取靠前的那个档位，和 Kotlin `minByOrNull` 一样）。
    pub fn step(current: f32, delta: i32) -> f32 {
        let mut best: Option<(usize, f32)> = None;
        for (i, s) in Self::STEPS.iter().enumerate() {
            let d = (s - current).abs();
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        let Some((idx, _)) = best else {
            return Self::DEFAULT;
        };
        let next = (idx as i32 + delta).clamp(0, Self::STEPS.len() as i32 - 1) as usize;
        Self::STEPS[next]
    }
}

/// Kotlin 的 `kotlin.math.round(Float)`：`floor(x + 0.5)`（往正无穷方向取半）。
fn round_half_up(v: f32) -> f64 {
    (v as f64 + 0.5).floor()
}

/// 这个视频的默认清晰度被封顶之后是哪一档：设置里选的和上限里**低**的那个
/// （[`DefaultQuality`] 按从高到低排，所以 `ordinal` 大的更低）。
pub fn limit_quality(setting: DefaultQuality, limit: Option<DefaultQuality>) -> DefaultQuality {
    match limit {
        None => setting,
        Some(limit) => {
            if setting >= limit {
                setting
            } else {
                limit
            }
        }
    }
}

/// 清晰度档位里"每一档里有哪几种编码"（检查工具 / 选流调试用）：按 [`QualityOption`] 的顺序。
pub fn codecs_by_quality(info: &PlayInfo) -> Vec<(i32, Vec<VideoCodec>)> {
    let mut out: Vec<(i32, Vec<VideoCodec>)> = Vec::new();
    for v in &info.video {
        match out.iter_mut().find(|(q, _)| *q == v.quality) {
            Some((_, list)) => {
                if !list.contains(&v.codec) {
                    list.push(v.codec);
                }
            }
            None => out.push((v.quality, vec![v.codec])),
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// 一档清晰度（`Quality`）从 `PlayInfo` 里取出来的一份拷贝（Kotlin 侧要显示标签时用）。
pub fn qualities(info: &PlayInfo) -> Vec<Quality> {
    info.qualities.clone()
}

