//! **播放 / 弹幕的 UniFFI 边界**（Kotlin `core/Video.kt` 的 `PlayInfo` / `Danmaku`）。
//!
//! 名字统一前缀 `FfiPlay*` / `FfiDanmaku*`（UniFFI 生成的 Kotlin 全在 `uniffi.feedhub_ffi` 一个包里，
//! 别家不许再占这几个名字）。**core → FFI 的转换只有一份**（[`to_ffi_play`] / [`to_ffi_danmaku`]），
//! Kotlin 侧的还原也只有一份（`rust-bridge/.../RustModel.kt` 的 `toPlayInfo` / `toDanmaku`）。
//!
//! 三条和别处一样的约定：
//!
//! - **拿不到就是 `None`**，不要编默认值；
//! - [`FfiPlayInfo::headers`] 用 record 数组而不是 `HashMap`：**顺序有意义**（播放器按顺序设请求头，
//!   `Referer` 要在 `User-Agent` 前面），`HashMap` 会把顺序丢掉；
//! - 一次调用换一整份结果：JVM ↔ Rust 每跨一次 JNA 都是固定成本，不要把 stream 一条条过边界。

use feedhub_core::play::{
    AudioStream, Danmaku, DanmakuMode, PlayInfo, ProgressiveStream, Quality, SegmentBase, VideoCodec,
    VideoStream,
};

/// 拉流时要带的一个请求头（**有序**，见模块说明）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayHeader {
    pub name: String,
    pub value: String,
}

/// 播放信息（Kotlin `PlayInfo`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayInfo {
    pub duration_ms: Option<i64>,
    pub quality: Option<i32>,
    pub qualities: Vec<FfiPlayQuality>,
    pub video: Vec<FfiPlayVideoStream>,
    pub audio: Vec<FfiPlayAudioStream>,
    pub progressive: Vec<FfiPlayProgressiveStream>,
    pub headers: Vec<FfiPlayHeader>,
}

/// 一档清晰度（Kotlin `Quality`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayQuality {
    pub id: i32,
    pub label: String,
    pub available: bool,
}

/// 视频编码（Kotlin `VideoCodec`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPlayCodec {
    Avc,
    Hevc,
    Av1,
    Other,
}

/// DASH 的 SegmentBase（Kotlin `SegmentBase`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlaySegmentBase {
    pub initialization: String,
    pub index_range: String,
}

/// 一条 DASH 视频轨（Kotlin `VideoStream`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayVideoStream {
    pub quality: i32,
    pub codec: FfiPlayCodec,
    pub codecs: Option<String>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub bandwidth: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<String>,
    pub segment_base: Option<FfiPlaySegmentBase>,
}

/// 一条 DASH 音频轨（Kotlin `AudioStream`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayAudioStream {
    pub id: i32,
    pub codecs: Option<String>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub bandwidth: Option<i64>,
    pub segment_base: Option<FfiPlaySegmentBase>,
}

/// 音画一体的整段文件（Kotlin `ProgressiveStream`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlayProgressiveStream {
    pub quality: Option<i32>,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub codec: Option<FfiPlayCodec>,
    pub bandwidth: Option<i64>,
}

/// 弹幕的显示方式（Kotlin `DanmakuMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDanmakuMode {
    Scroll,
    Top,
    Bottom,
    Other,
}

/// 一条弹幕（Kotlin `Danmaku`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDanmaku {
    pub id: String,
    pub time_ms: i64,
    pub mode: FfiDanmakuMode,
    pub text: String,
    pub color: i32,
    pub font_size: i32,
    pub weight: Option<i32>,
}

// ---------------------------------------------------------------------------
// core → FFI（只有这一份）
// ---------------------------------------------------------------------------

pub(crate) fn to_ffi_codec(codec: VideoCodec) -> FfiPlayCodec {
    match codec {
        VideoCodec::Avc => FfiPlayCodec::Avc,
        VideoCodec::Hevc => FfiPlayCodec::Hevc,
        VideoCodec::Av1 => FfiPlayCodec::Av1,
        VideoCodec::Other => FfiPlayCodec::Other,
    }
}

fn to_ffi_quality(q: &Quality) -> FfiPlayQuality {
    FfiPlayQuality {
        id: q.id,
        label: q.label.clone(),
        available: q.available,
    }
}

fn to_ffi_segment_base(sb: &SegmentBase) -> FfiPlaySegmentBase {
    FfiPlaySegmentBase {
        initialization: sb.initialization.clone(),
        index_range: sb.index_range.clone(),
    }
}

fn to_ffi_video_stream(s: &VideoStream) -> FfiPlayVideoStream {
    FfiPlayVideoStream {
        quality: s.quality,
        codec: to_ffi_codec(s.codec),
        codecs: s.codecs.clone(),
        url: s.url.clone(),
        backup_urls: s.backup_urls.clone(),
        bandwidth: s.bandwidth,
        width: s.width,
        height: s.height,
        frame_rate: s.frame_rate.clone(),
        segment_base: s.segment_base.as_ref().map(to_ffi_segment_base),
    }
}

fn to_ffi_audio_stream(s: &AudioStream) -> FfiPlayAudioStream {
    FfiPlayAudioStream {
        id: s.id,
        codecs: s.codecs.clone(),
        url: s.url.clone(),
        backup_urls: s.backup_urls.clone(),
        bandwidth: s.bandwidth,
        segment_base: s.segment_base.as_ref().map(to_ffi_segment_base),
    }
}

fn to_ffi_progressive_stream(s: &ProgressiveStream) -> FfiPlayProgressiveStream {
    FfiPlayProgressiveStream {
        quality: s.quality,
        url: s.url.clone(),
        backup_urls: s.backup_urls.clone(),
        size_bytes: s.size_bytes,
        width: s.width,
        height: s.height,
        codec: s.codec.map(to_ffi_codec),
        bandwidth: s.bandwidth,
    }
}

/// [`feedhub_core::play::PlayInfo`] → `FfiPlayInfo`（11 家共用这一份）。
pub(crate) fn to_ffi_play(play: &PlayInfo) -> FfiPlayInfo {
    FfiPlayInfo {
        duration_ms: play.duration_ms,
        quality: play.quality,
        qualities: play.qualities.iter().map(to_ffi_quality).collect(),
        video: play.video.iter().map(to_ffi_video_stream).collect(),
        audio: play.audio.iter().map(to_ffi_audio_stream).collect(),
        progressive: play.progressive.iter().map(to_ffi_progressive_stream).collect(),
        headers: play
            .headers
            .iter()
            .map(|(name, value)| FfiPlayHeader {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
    }
}

pub(crate) fn to_ffi_danmaku_mode(mode: DanmakuMode) -> FfiDanmakuMode {
    match mode {
        DanmakuMode::Scroll => FfiDanmakuMode::Scroll,
        DanmakuMode::Top => FfiDanmakuMode::Top,
        DanmakuMode::Bottom => FfiDanmakuMode::Bottom,
        DanmakuMode::Other => FfiDanmakuMode::Other,
    }
}

/// 一条弹幕 → `FfiDanmaku`。
pub(crate) fn to_ffi_danmaku_one(d: &Danmaku) -> FfiDanmaku {
    FfiDanmaku {
        id: d.id.clone(),
        time_ms: d.time_ms,
        mode: to_ffi_danmaku_mode(d.mode),
        text: d.text.clone(),
        color: d.color,
        font_size: d.font_size,
        weight: d.weight,
    }
}

/// 一段弹幕 → `Vec<FfiDanmaku>`（**一次调用换一整段**，不要一条条过边界）。
pub(crate) fn to_ffi_danmaku(items: &[Danmaku]) -> Vec<FfiDanmaku> {
    items.iter().map(to_ffi_danmaku_one).collect()
}
