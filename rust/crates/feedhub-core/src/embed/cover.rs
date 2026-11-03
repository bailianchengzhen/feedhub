//! 封面向量：**哪些内容算、取哪张图、送给模型的文字怎么拼、像素怎么摆**。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/CoverVectors.kt` 的 Rust 版。
//! 这一层只放规则和口径，调度在 [`super::cover_schedule`]，设备上的两张 HTP 图在宿主。
//!
//! 为什么封面向量和纯文字向量**并存、不替换**：同一条内容带不带封面，256 维余弦中位只有 0.9156
//! ——比"同长度下量化的差"（0.997）大一个数量级，
//! 所以按 `item_embedding.model` 分成两个空间，**绝不混着比**。

use super::item::EmbedItem;
use super::text::{embedding_text, MAX_SUMMARY_CHARS};

/// 平台 id → 算不算封面向量（**唯一的判定处**）。
///
/// 键是平台 id（插件 `plugin.toml` 的那一个），不是显示名、不是插件类型。
/// 表里没有的平台**不算**。
pub const PLATFORM_TABLE: [(&str, bool); 11] = [
    // 视频 / 短视频：封面就是视频画面，信息量最大
    ("bili", true),
    ("douyin", true),
    ("xhs", true),
    ("yt", true),
    ("tiktok", true),
    // 图文帖子：带配图的算（多图只取第一张）
    ("x", true),
    ("reddit", true),
    // 文字为主的平台：继续只有纯文字向量
    ("zhihu", false),
    ("coolapk", false),
    ("tieba", false),
    ("heybox", false),
];

/// 这个平台算不算封面向量。
pub fn counts(platform: &str) -> bool {
    PLATFORM_TABLE
        .iter()
        .find(|(id, _)| *id == platform)
        .map(|(_, on)| *on)
        .unwrap_or(false)
}

/// 表里所有"算"的平台 id（测试 / 设置页文案用）。
pub fn cover_platforms() -> Vec<&'static str> {
    PLATFORM_TABLE
        .iter()
        .filter(|(_, on)| *on)
        .map(|(id, _)| *id)
        .collect()
}

/// 封面取哪一档缩略图（首页卡片那一档）。
pub const COVER_THUMB_WIDTH: i32 = 640;

/// 试缓存时要试的宽度档（**从大到小**：先 640，再几档退路）。
pub const CACHE_WIDTHS: [i32; 8] = [640, 480, 800, 320, 960, 1280, 1600, 1920];

/// 视觉图的输入边长（384 = 8 个 soft token × 48 像素）。
pub const COVER_INPUT_SIDE: usize = 384;

/// 一个 patch 的边长（16 像素；48 = patch 16 × 池化 3）。
pub const COVER_PATCH: usize = 16;

/// 文本图里**图占多少个 token**（8×8 方阵 = 64 个 soft token，烘进图里的常量）。
pub const COVER_IMAGE_TOKENS: usize = 64;

/// 文本+图那张图里**正文**最多放几个 token（`[8,128]`：128 − 67 − 1 = 60）。
pub const COVER_TEXT_TOKENS: usize = 128 - (2 + COVER_IMAGE_TOKENS + 1) - 1;

/// 纯文字 NPU 向量的空间标识（`npuModelId(seqLen)`）。
pub fn npu_model_id(seq_len: usize) -> String {
    format!("embeddinggemma-2-w8a16-htp-l{seq_len}")
}

/// **封面向量**的空间标识（`npuCoverModelId(seqLen)`）。
pub fn npu_cover_model_id(seq_len: usize) -> String {
    format!("embeddinggemma-2-w8a16-htp-l{seq_len}-img8x8")
}

/// 这条内容的**第一张**封面 / 配图（原地址）；视频封面优先，然后正文里第一张图。
pub fn cover_image_url(item: &EmbedItem) -> Option<&str> {
    if let Some(cover) = item.video_cover.as_deref() {
        if !crate::hosted::is_kotlin_blank(cover) {
            return Some(cover);
        }
    }
    if let Some(url) = item.first_image.as_deref() {
        if !crate::hosted::is_kotlin_blank(url) {
            return Some(url);
        }
    }
    None
}

/// 这条内容值不值得算封面向量：平台在表里 + 有图 + 不是 18+ + 有文字。
///
/// **不在这里挡"图片取不取得到"**：那是运行时的事（缓存里没有、又不在 Wi-Fi 上就先跳过，下次再试）。
pub fn has_cover_vector(item: &EmbedItem) -> bool {
    !item.adult && counts(&item.platform) && cover_image_url(item).is_some() && item.has_text()
}

/// 封面向量要送的那段文字（和纯文字向量同一个口径：`title: … | text: …`）。
pub fn cover_embedding_text(item: &EmbedItem) -> String {
    embedding_text(item, MAX_SUMMARY_CHARS)
}

/// 384×384 的 **ARGB 像素** → `pixel_values [1, 576, 768]`（视觉图唯一的输入）。
///
/// 口径照 `tools/embed-vision/egv_common.py` 逐行对齐：
/// - 像素值**除以 255**、不归一化（`do_normalize=false`）；
/// - patch 是 16×16×3，**行优先**：先按 (patch 行, patch 列) 排 patch，patch 内部再按 (y, x) 排像素，
///   每个像素按 R、G、B；
/// - 8×8 方阵这条路不保持长宽比（拉伸到 384×384）。
///
/// `argb` 的长度必须是 `side * side`（安卓 `Bitmap.getPixels` 的格式：`A<<24|R<<16|G<<8|B`）。
pub fn patchify(argb: &[i32], side: usize) -> Result<Vec<f32>, String> {
    if side == 0 || side % COVER_PATCH != 0 {
        return Err(format!("side={side} 不是 {} 的整数倍", COVER_PATCH));
    }
    if argb.len() != side * side {
        return Err(format!("像素数 {} 对不上 {side}×{side}", argb.len()));
    }
    let grid = side / COVER_PATCH;
    let mut out = vec![0.0f32; grid * grid * 3 * COVER_PATCH * COVER_PATCH];
    let mut at = 0usize;
    for gy in 0..grid {
        for gx in 0..grid {
            for y in 0..COVER_PATCH {
                let row_base = (gy * COVER_PATCH + y) * side + gx * COVER_PATCH;
                for x in 0..COVER_PATCH {
                    let v = argb[row_base + x];
                    out[at] = (((v >> 16) & 0xFF) as f32) / 255.0;
                    at += 1;
                    out[at] = (((v >> 8) & 0xFF) as f32) / 255.0;
                    at += 1;
                    out[at] = ((v & 0xFF) as f32) / 255.0;
                    at += 1;
                }
            }
        }
    }
    Ok(out)
}

/// 一个封面窗口的账（性能记录 / 设置页 / 日志；**不含任何内容**）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoverWindowStats {
    pub count: i32,
    pub vision_images: i32,
    pub vision_ms: i64,
    pub text_batches: i32,
    pub text_ms: i64,
    pub total_ms: i64,
    /// 这个窗口里**现编**了几张图（0 = 都是 ctx 加载；1 = 只有视觉；2 = 两张都编了）。
    pub compiled: i32,
}

impl CoverWindowStats {
    /// 一条摊销多少毫秒。
    pub fn ms_per_item(&self) -> i64 {
        if self.count > 0 {
            self.total_ms / self.count as i64
        } else {
            0
        }
    }
}

/// 封面向量的状态（设置页那行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverStatus {
    /// 这个构建没带两张图（普通包 / 桌面）。
    NoModel,
    /// 「记录浏览行为」关着。
    Off,
    /// 队列空。
    Idle,
    /// 在算：这个窗口第几条 / 一共几条。
    Running { done: i32, total: i32 },
    /// 当前里用不了了（QNN 6001 / 编图失败 / 这个包没带图）。
    Unavailable { reason: String },
}

impl CoverStatus {
    /// 设置页那一行的短标签。
    pub fn label(&self) -> &'static str {
        match self {
            CoverStatus::NoModel => "没带图",
            CoverStatus::Off => "关着",
            CoverStatus::Idle => "空闲",
            CoverStatus::Running { .. } => "算封面",
            CoverStatus::Unavailable { .. } => "本次不可用",
        }
    }
}

