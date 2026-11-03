//! 知乎的**播放取流**（`source-zhihu/ZhihuVideo.kt` 的 Rust 版）。
//!
//! 两条路：
//! - **zvideo**：`partId` 就是 `video.video_id`（详情里 `FeedDetail.video.parts[].id`）；
//! - **回答 / 文章 / 想法正文里嵌的视频**：`<a class="video-box" data-lens-id=…>` →
//!   `Block.Video { id: lens, part_id: lens }`——`partId` 还是那个 lens 视频 id。
//!
//! 所以两者其实是一个接口：`GET {lens}/api/v4/videos/{video_id}`，**匿名可用**、不需要也不该带 Cookie，
//! 不需要 Referer（Zhihu++ 用的是 `POST /api/v4/video/play_info`，是 POST 就不用）。
//!
//! 响应里有两套 mp4 列表：
//! - `playlist`：`LD` / `SD` / `HD`（偶尔 `FHD`），H.264，`vdn3.vzuu.com/…mp4?auth_key=…`；
//! - `playlist_v2`：`FHD` / `HD` / `SD`，H.265（地址里 `c=hevc.8.0`，同一清晰度码率更低），
//!   `vdn5.vzuu.com/…mp4?pkey=…`；
//!
//! 每条有 `play_url`、`width` / `height`、`bitrate`（kbps）、`size`、`duration`（秒）。
//! **地址带签名和 `expiration`，会过期**，所以每次播放都现取，不复用详情里缓存的
//! `video.playlist`（那是同一形状，但可能已经过期）。
//!
//! 两个口径（照 Kotlin）：
//! 1. 清晰度按**档位名**定（FHD = 1080P / HD = 720P / SD = 480P / LD = 360P），id 沿用 B站的数字，
//!    设置里的"默认清晰度"对知乎一样有效；
//! 2. 两套列表里相同地址（LD 和 SD 常常是同一个文件）**只留较高的一档**；同一档有 AVC / HEVC 两条时
//!    都给，播放器按本机硬解能力挑。

use crate::json::{self, JsonMap};
use crate::model::BiliError as ZhihuError;
use crate::play::{quality_of_size, PlayInfo, ProgressiveStream, Quality, VideoCodec};

/// lens（视频地址接口）的域名（Kotlin `ZhihuSource.LENS`）。
pub const LENS: &str = "https://lens.zhihu.com";

/// 档位名 → 清晰度（id 沿用 B站；顺序就是"从高到低"的遍历顺序）。
const TIERS: [(&str, i32, &str); 4] = [
    ("FHD", 80, "1080P"),
    ("HD", 64, "720P"),
    ("SD", 32, "480P"),
    ("LD", 16, "360P"),
];

/// zvideo 详情里的 `video.video_id`（Kotlin `ZhihuVideo.videoId`）：非空、全是数字。
pub fn video_id(zvideo: &JsonMap) -> Option<String> {
    json::obj(zvideo, "video")
        .and_then(|v| json::text(v, "video_id"))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

/// **只校验 `partId` 的写法**（非空、全是数字），不解析、不发请求。
///
/// 平台层在请求 lens 之前先调它一次（Kotlin 是 `if (partId.isEmpty() || !partId.all { it.isDigit() })`
/// 就地抛 `ApiException("bad_id")`，本来就在请求之前）。错误码 / 文案照 Kotlin 的 `badId`。
pub fn check_part(part_id: &str) -> Result<(), ZhihuError> {
    if part_id.is_empty() || !part_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ZhihuError::Api {
            code: "bad_id".to_string(),
            detail: format!("不认识的内容 id：{part_id}（应该像 answer:123，类型有 answer/article/pin/zvideo/question）"),
        });
    }
    Ok(())
}

/// lens 接口的完整地址（`{lens}/api/v4/videos/{video_id}`）。
pub fn url(part_id: &str) -> String {
    url_at(LENS, part_id)
}

/// 同 [`url`]，根由调用方给（测试指向 MockWebServer；和 Kotlin 的 `lensBase` 构造参数对应）。
pub fn url_at(lens_base: &str, part_id: &str) -> String {
    format!("{}/api/v4/videos/{part_id}", lens_base.trim_end_matches('/'))
}

/// lens 请求要带的请求头：只有 `Accept`（Kotlin 是 `.header("Accept", "application/json")`）。
///
/// **不带 Cookie**（匿名接口）、不带 `x-zse-96`、不带 Referer——单独的那个匿名客户端把 Cookie 罐关了，
/// 其余默认头（UA）由宿主客户端的拦截器补；这里把 UA 也写全（和 Kotlin 最终落到线上的顺序一致：
/// `Accept` 在前、`User-Agent` 在后）。
pub fn headers() -> Vec<(String, String)> {
    vec![
        ("Accept".to_string(), "application/json".to_string()),
        (
            "User-Agent".to_string(),
            super::source::USER_AGENT.to_string(),
        ),
    ]
}

/// lens 的响应 → 播放信息（Kotlin `ZhihuVideo.play`）。
///
/// 一条流都没有时抛 `ParseException("视频没有可播放的地址（可能还在转码或已删除）")`（照 Kotlin）。
pub fn play(json: &JsonMap) -> Result<PlayInfo, ZhihuError> {
    // 地址 → 流：同一个文件只留一次（先出现的档位更高）
    let mut streams: Vec<ProgressiveStream> = Vec::new();
    let mut duration: Option<f64> = None;
    for (key, hevc_by_default) in [("playlist_v2", true), ("playlist", false)] {
        let Some(list) = json::obj(json, key) else {
            continue;
        };
        for (tier, quality, _label) in TIERS {
            // Kotlin：`list.obj(tier) ?: list.obj(tier.lowercase())`
            let Some(o) = json::obj(list, tier).or_else(|| json::obj(list, &tier.to_ascii_lowercase())) else {
                continue;
            };
            let Some(url) = json::text(o, "play_url").or_else(|| json::text(o, "url")) else {
                continue;
            };
            if duration.is_none() {
                duration = json::double(o, "duration");
            }
            if streams.iter().any(|s| s.url == url) {
                continue;
            }
            streams.push(ProgressiveStream {
                quality: Some(quality),
                url: url.clone(),
                backup_urls: Vec::new(),
                size_bytes: json::long(o, "size").filter(|v| *v > 0),
                width: json::long(o, "width").map(|v| v as i32).filter(|v| *v > 0),
                height: json::long(o, "height").map(|v| v as i32).filter(|v| *v > 0),
                codec: Some(codec_of(&url, hevc_by_default)),
                bandwidth: json::double(o, "bitrate")
                    .map(|b| (b * 1000.0) as i64)
                    .filter(|b| *b > 0),
            });
        }
    }
    if streams.is_empty() {
        return Err(ZhihuError::Parse {
            detail: "视频没有可播放的地址（可能还在转码或已删除）".to_string(),
        });
    }
    // `TIERS.values.filter { q -> list.any { it.quality == q.id } }`：按档位从高到低的顺序列出给了流的那些
    let qualities: Vec<Quality> = TIERS
        .iter()
        .filter(|(_, id, _)| streams.iter().any(|s| s.quality == Some(*id)))
        .map(|(_, id, label)| Quality {
            id: *id,
            label: label.to_string(),
            available: true,
        })
        .collect();
    let quality = streams.iter().filter_map(|s| s.quality).max();
    Ok(PlayInfo {
        duration_ms: duration
            .map(|d| (d * 1000.0) as i64)
            .filter(|d| *d > 0),
        quality,
        qualities,
        progressive: streams,
        ..PlayInfo::default()
    })
}

/// 地址里 `c=avc.4.0` / `c=hevc.8.0` 标了编码；没标的看地址里有没有 `h265` / `hevc`，
/// 再没有就按所在列表的默认（`playlist_v2` 是 HEVC）。
fn codec_of(url: &str, hevc_by_default: bool) -> VideoCodec {
    let c = super::html::url::parse(url)
        .and_then(|u| u.query_param("c"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if c.starts_with("hevc") || c.starts_with("hev") || c.starts_with("h265") {
        return VideoCodec::Hevc;
    }
    if c.starts_with("avc") || c.starts_with("h264") {
        return VideoCodec::Avc;
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("h265") || lower.contains("hevc") {
        return VideoCodec::Hevc;
    }
    if hevc_by_default {
        VideoCodec::Hevc
    } else {
        VideoCodec::Avc
    }
}

/// 没有档位名的视频（理论上不会有：知乎都带档位）按画面短边给一档——留给将来别的地方用。
pub fn quality_for(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    quality_of_size(width, height)
}

