//! 小红书**视频笔记的播放地址**（Kotlin `XhsVideo` 的逐行移植）。
//!
//! 边界和别的平台一样：这一层只做"`note_card` → 播放通用模型"
//! （[`crate::play::PlayInfo`]），**请求怎么发、缓存怎么复用、优先级怎么排**在
//! [`crate::xhs::source::XhsHostedSource::play`] 里，收发仍是宿主的 OkHttp。
//!
//! 实测口径：
//!
//! - 分组键名被混淆成 `EF4`~`EF7`，**每条流自己的 `video_codec` 也是 `EF4` / `EF5`**。
//!   EF4 = h264、EF5 = h265（有 Spider_XHS 的注释和 SSR 页面分组名 `h264/h265/h266/av1` 的顺序为证）；
//!   EF6、EF7 没见过有内容的流，按 [`VideoCodec::Other`] 列出（播放器排最后）。
//!   流里的 `video_codec` 要是以后改回 `h264` / `hevc` / `av1` 这样的名字也认。
//! - 每条流是 mp4 直链（`master_url` 加两个 `backup_urls`），带 `sign` 和 `t`（十六进制 Unix 秒）两个参数，
//!   **有效期约 3 天**；CDN 不要 Referer、不要 Cookie，支持 Range，https 可用。
//! - `stream_type` 259 和 309 是**带水印**的流，丢掉；只剩水印流时（旧视频可能只有它）还是返回它们，
//!   带水印的能放比报错好。
//! - 清晰度按**短边**：≥ 2000 是 4K（120）、1400~1999 是 2K（112，借 B站的 1080P+ 数字来区分）、
//!   其余照 [`crate::coolapk::video::quality_of_size`]（和 Kotlin `qualityOfSize` 同一份口径）。
//!
//! **已知差异**：Kotlin 的 `JsonObject` 保留 JSON 里的键顺序，
//! 这里的 `serde_json` 没开 `preserve_order`，分组按键名排序。`progressive` 最后按
//! `(清晰度 desc, 码率 desc)` 排，只有**两档清晰度和码率都相同**时才会看出输入顺序的差别

use crate::json;
use crate::json::JsonMap;
use crate::model::BiliError;
use crate::play::{PlayInfo, ProgressiveStream, Quality, VideoCodec};

/// [`crate::detail::VideoPart::id`]：整条笔记就是一个视频。
pub const MAIN: &str = "main";

/// 带水印的 `stream_type`。
const WATERMARKED: [i32; 2] = [259, 309];

/// 一条候选流（分组名 + 原始 JSON）。分组名是 [`crate::xhs::detail::streams`] 给的 `String`，
/// 所以这个结构**持有**它、只借用 `note` 里的流对象。
#[derive(Debug, Clone)]
pub struct Stream<'a> {
    pub group: String,
    pub o: &'a JsonMap,
}

/// 一档流（或 `media_v2.video`）**显示出来**的宽高：`rotate` 是 90 / 270 时把编码宽高对调。
///
/// 手机竖着拍、按横屏编码再标旋转的视频，编码尺寸是 1920×1080；不对调的话详情一回来就被当成横屏，
/// 等播放器解出真实画面又换回竖屏——用户看到的"先横后竖"。
pub(crate) fn display_size(o: &JsonMap) -> (Option<i32>, Option<i32>) {
    let (w, h) = (json::int(o, "width"), json::int(o, "height"));
    match json::int(o, "rotate").map(|r| r.rem_euclid(360)) {
        Some(90) | Some(270) => (h, w),
        _ => (w, h),
    }
}

impl<'a> Stream<'a> {
    fn stream_type(&self) -> Option<i32> {
        json::int(self.o, "stream_type")
    }

    pub fn width(&self) -> Option<i32> {
        display_size(self.o).0
    }

    pub fn height(&self) -> Option<i32> {
        display_size(self.o).1
    }

    fn watermarked(&self) -> bool {
        self.stream_type().is_some_and(|t| WATERMARKED.contains(&t))
    }
}

/// `note_card.video.media.stream.<分组>[]` 里带 `master_url` 的那些（`XhsVideo.streams`）。
///
/// 复用 [`crate::xhs::detail::streams`]（详情算 `media_v2` 尺寸时同一份，别再抄一遍）。
pub fn streams(note: &JsonMap) -> Vec<Stream<'_>> {
    crate::xhs::detail::streams(note)
        .into_iter()
        .map(|(group, o)| Stream { group, o })
        .collect()
}

/// 视频的像素尺寸（`XhsVideo.size`）：`media_v2` 里那一档优先，没有就取流里面积最大的。
///
/// 详情（[`crate::xhs::detail::detail_of`]）已经用它算 `VideoPart` 的宽高，所以这里只是转出去。
pub fn size(note: &JsonMap) -> (Option<i32>, Option<i32>) {
    crate::xhs::detail::video_size(note)
}

/// 编码（`XhsVideo.codec`）：流自己的 `video_codec` 优先，没有就看分组名。
pub fn codec(s: &Stream<'_>) -> VideoCodec {
    let raw = json::text(s.o, "video_codec").unwrap_or_else(|| s.group.clone());
    match raw.to_lowercase().as_str() {
        "ef4" | "h264" | "avc" | "avc1" => VideoCodec::Avc,
        "ef5" | "h265" | "hevc" | "hvc1" | "hev1" => VideoCodec::Hevc,
        "av1" | "av01" => VideoCodec::Av1,
        _ => VideoCodec::Other,
    }
}

/// 清晰度档位（`XhsVideo.quality`）：短边 ≥ 2000 是 4K、1400~1999 是 2K、其余按短边分档。
///
/// `1400..=1999` 那一档借 B站 的 112（1080P+）把 2K 和 4K 分开——[`quality_of_size`] 只看短边，
/// 1440 会落进 1080P，和 Kotlin 不一样。
pub fn quality(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    let short = [width, height]
        .into_iter()
        .flatten()
        .filter(|v| *v > 0)
        .min()?;
    if (1400..2000).contains(&short) {
        return Some(Quality {
            id: 112,
            label: "2K".to_string(),
            available: true,
        });
    }
    crate::coolapk::video::quality_of_size(width, height)
}

/// 一条流 → [`ProgressiveStream`]（`XhsVideo.play` 里那个 `map`）。
fn progressive(s: &Stream<'_>) -> Option<ProgressiveStream> {
    let url = json::to_https(&json::text(s.o, "master_url")?);
    let backup_urls = json::arr(s.o, "backup_urls")
        .map(|list| {
            list.iter()
                .filter_map(json::primitive_content)
                .filter(|u| !u.trim().is_empty())
                .map(|u| json::to_https(&u))
                .collect()
        })
        .unwrap_or_default();
    Some(ProgressiveStream {
        quality: quality(s.width(), s.height()).map(|q| q.id),
        url,
        backup_urls,
        size_bytes: json::long(s.o, "size"),
        width: s.width(),
        height: s.height(),
        codec: Some(codec(s)),
        bandwidth: json::long(s.o, "avg_bitrate").or_else(|| json::long(s.o, "video_bitrate")),
    })
}

/// `note_card` → 播放信息（`XhsVideo.play`）。
///
/// 一条流都没有（或者全是不带 `master_url` 的）：`Api { code: "no_stream" }`，文案和 Kotlin 一样。
pub fn play(note: &JsonMap) -> Result<PlayInfo, BiliError> {
    let all = streams(note);
    let mut usable: Vec<Stream<'_>> = all.iter().filter(|s| !s.watermarked()).cloned().collect();
    if usable.is_empty() {
        // 只剩带水印的流：照样返回（带水印的能放，比报错好）
        usable = all;
    }
    if usable.is_empty() {
        return Err(BiliError::Api {
            code: "no_stream".to_string(),
            detail: "这条笔记没有可播放的视频流".to_string(),
        });
    }
    let mut items: Vec<ProgressiveStream> = usable.iter().filter_map(progressive).collect();
    // `compareByDescending { quality ?: 0 }.thenByDescending { bandwidth ?: 0 }`：稳定排序，并列保序。
    items.sort_by(|a, b| {
        (b.quality.unwrap_or(0), b.bandwidth.unwrap_or(0))
            .cmp(&(a.quality.unwrap_or(0), a.bandwidth.unwrap_or(0)))
    });
    let mut qualities: Vec<Quality> = Vec::new();
    for p in &items {
        let Some(q) = quality(p.width, p.height) else { continue };
        if qualities.iter().any(|seen| seen.id == q.id) {
            continue;
        }
        qualities.push(q);
    }
    qualities.sort_by(|a, b| b.id.cmp(&a.id));
    // 时长：流里第一个给了 `duration` 的（毫秒），没有就退回 `video.capa.duration`（秒）× 1000。
    let duration_ms = usable
        .iter()
        .find_map(|s| json::long(s.o, "duration"))
        .or_else(|| {
            json::obj(note, "video")
                .and_then(|v| json::obj(v, "capa"))
                .and_then(|c| json::int(c, "duration"))
                .map(|d| d as i64 * 1000)
        });
    Ok(PlayInfo {
        duration_ms,
        quality: qualities.first().map(|q| q.id),
        qualities,
        video: Vec::new(),
        audio: Vec::new(),
        progressive: items,
        headers: vec![(
            "Referer".to_string(),
            format!("{}/", crate::xhs::WEB),
        )],
    })
}

