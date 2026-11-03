//! **播放 / 弹幕的通用模型**（Kotlin `core/Video.kt` 的 `PlayInfo` / `Danmaku`）。
//!
//! 为什么单独一个模块：播放地址是**每家平台都要搬的一层**，B站是第一个。
//! 模型放这里、`FfiPlay*` / `FfiDanmaku*` 放 `feedhub-ffi/src/play.rs`、Kotlin 的映射只有一份
//! （`rust-bridge/.../RustModel.kt` 的 `toPlayInfo` / `toDanmaku`）——后面的抖音 / TikTok / 其余几家
//! **只写"这个响应怎么变成本模型"，不要再各造一套 record 和映射**。
//!
//! 两条口径（和 [`crate::model`] / [`crate::detail`] 一样）：
//!
//! 1. **拿不到就是 `None`，不要编默认值**（B站 没给 `timelength` 就不填 0）；
//! 2. [`PlayInfo::headers`] 是**有序的** `Vec<(String, String)>`，不是 `HashMap`：Kotlin 侧是 `Map`，
//!    但播放器要按顺序把请求头写到拉流的请求上，顺序变了请求字节就变了（B站 的 `Referer` 必须在
//!    `User-Agent` 前面——那是 `BilibiliSource.PLAY_HEADERS` 的插入顺序）。
//!
//! 播放地址**会过期**（B站 的 `upsig` / 小黑盒的 auth / 知乎的 `auth_key`）：这一层只负责"把平台刚给的
//! 响应解析成模型"，[`PlayInfo`] 不要缓存、不要复用详情里的旧地址（和 Kotlin 的注释同一条规矩）。
//!
//! 另外两个共用件：
//!
//! - [`quality_of_size`]：`core/Video.kt` 的 `qualityOfSize`（按画面**短边**定档，B站 的数字口径）；
//! - [`quality`] / [`quality_id`]：短视频平台（抖音 / TikTok / 小红书）在它前面多一档 **2K（112）**。
//!
//! 各家原来各写一份的相同实现已经并到这里：酷安（`coolapk/video.rs` 自己的 `quality_of_size`）和
//! 抖音 / TikTok 的详情（`douyin/detail.rs` / `tiktok/detail.rs` 里各一份 `quality_id`）现在都调
//! [`quality_of_size`] / [`quality`]，**只剩一份**（酷安 / B站 的数字口径一个字没动）。

/// 播放信息（Kotlin `PlayInfo`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayInfo {
    /// 视频总时长（毫秒）。DASH 的 `duration` 是秒，解析时已经乘 1000。
    pub duration_ms: Option<i64>,
    /// 这次返回的**实际**清晰度。DASH 时是"实际给到的最高的那条视频轨"，不是请求里那个 `qn`。
    pub quality: Option<i32>,
    /// 平台列出的全部清晰度（包括要登录 / 大会员、这次没给流的那几档）。
    pub qualities: Vec<Quality>,
    /// DASH 视频轨（无声），同一清晰度可能有几种编码。
    pub video: Vec<VideoStream>,
    /// DASH 音频轨。
    pub audio: Vec<AudioStream>,
    /// 音画一体的整段文件（老视频、非 DASH 的平台）。有 DASH 时一般为空。
    pub progressive: Vec<ProgressiveStream>,
    /// 拉流时必须带的请求头，**顺序有意义**（见模块说明）。
    pub headers: Vec<(String, String)>,
}

/// 一档清晰度（Kotlin `Quality`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quality {
    /// 平台自己的编号（B站 80 = 1080P、64 = 720P、32 = 480P、16 = 360P、120 = 4K）。
    pub id: i32,
    /// "1080P 高清"之类。
    pub label: String,
    /// 这次是否给了这个清晰度的流（没登录 / 不是大会员时高清晰度不给）。
    pub available: bool,
}

/// 视频编码（Kotlin `VideoCodec`）。播放器选流时优先选设备能硬解的那种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    Avc,
    Hevc,
    Av1,
    Other,
}

/// DASH 的 SegmentBase（`initialization` / `indexRange` 原样交给播放器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentBase {
    pub initialization: String,
    pub index_range: String,
}

/// 一条 DASH 视频轨（Kotlin `VideoStream`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStream {
    pub quality: i32,
    pub codec: VideoCodec,
    /// 原样的 codecs 字符串（`avc1.640033` / `hev1.1.6.L150.90`），给 MediaCodec 判 profile 用。
    pub codecs: Option<String>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub bandwidth: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<String>,
    pub segment_base: Option<SegmentBase>,
}

/// 一条 DASH 音频轨（Kotlin `AudioStream`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStream {
    /// B站 的 30280 / 30232 / 30216，杜比 / 无损是 30250 / 30251。
    pub id: i32,
    pub codecs: Option<String>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub bandwidth: Option<i64>,
    pub segment_base: Option<SegmentBase>,
}

/// 音画一体的整段文件（Kotlin `ProgressiveStream`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressiveStream {
    pub quality: Option<i32>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// 不知道就 `None`（播放器按 AVC 对待，最通用）。
    pub codec: Option<VideoCodec>,
    /// 码率（bit/s），同一清晰度有几条时用来挑。
    pub bandwidth: Option<i64>,
}

/// 弹幕的显示方式（Kotlin `DanmakuMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanmakuMode {
    /// 滚动（B站 mode 1 / 2 / 3）。
    Scroll,
    /// 顶部固定（B站 mode 5）。
    Top,
    /// 底部固定（B站 mode 4）。
    Bottom,
    /// 逆向 / 高级 / 代码 / BAS 等，前端可以直接不显示。
    Other,
}

/// 一条弹幕（Kotlin `Danmaku`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Danmaku {
    /// 平台内的弹幕 id（B站 是 dmid 的字符串形式）。
    pub id: String,
    /// 相对视频开头的毫秒数（负数已经夹成 0）。
    pub time_ms: i64,
    pub mode: DanmakuMode,
    pub text: String,
    /// `0xRRGGBB`（B站 的 `uint32` 只留低 24 位）。
    pub color: i32,
    /// 平台原值，B站 常见 25（标准）/ 18（小）；拿不到就是 25。
    pub font_size: i32,
    /// 平台给的权重 / 屏蔽等级（B站 0~10，越高越该显示）；0 / 没有就是 `None`。
    pub weight: Option<i32>,
}

// ===========================================================================
// 清晰度分档
// ===========================================================================

/// `core/Video.kt` 的 `qualityOfSize`：按画面**短边**定档（120 = 4K、80 = 1080P、64 = 720P、
/// 32 = 480P、16 = 360P）。宽高都没给 / 都不大于 0 就是 `None`。
///
/// 档位 id 沿用 B站 的数字，所以设置里那个"默认清晰度"对非 B站 的平台也一样有效。
pub fn quality_of_size(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    let short = short_side(width, height)?;
    let (id, label) = if short >= 2000 {
        (120, "4K")
    } else if short >= 1000 {
        (80, "1080P")
    } else if short >= 700 {
        (64, "720P")
    } else if short >= 460 {
        (32, "480P")
    } else {
        (16, "360P")
    };
    Some(Quality {
        id,
        label: label.to_string(),
        available: true,
    })
}

/// 短视频平台（抖音 / TikTok / 小红书）的档位：短边 `1400..=1999` 是 **2K（112）**，其余同
/// [`quality_of_size`]。抖音 / TikTok 的竖屏 1440×2560 / 1080×1920 都靠这一档分开。
pub fn quality(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    let short = short_side(width, height)?;
    if (1400..=1999).contains(&short) {
        Some(Quality {
            id: 112,
            label: "2K".to_string(),
            available: true,
        })
    } else {
        quality_of_size(width, height)
    }
}

/// 只要档位 id（各家的解析里到处用，省一次 `Option<Quality>` 的包装）。
pub fn quality_id(width: Option<i32>, height: Option<i32>) -> Option<i32> {
    quality(width, height).map(|q| q.id)
}

/// 短边：两边都给就取小的；只给一边就用那一边；不给或全是 0 就是 `None`。
fn short_side(width: Option<i32>, height: Option<i32>) -> Option<i32> {
    [width, height]
        .into_iter()
        .flatten()
        .filter(|v| *v > 0)
        .min()
}

