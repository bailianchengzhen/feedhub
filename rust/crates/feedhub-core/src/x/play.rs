//! X 的**播放取流**（`source-x/XVideoPlay.kt` + `XTweet.kt` 里 `selected` / `mp4ByBitrateDesc` 的 Rust 版）。
//!
//! X 不是 DASH：每个视频只有一组 mp4（同一分辨率的不同码率）+ 少数情况下的 HLS（直播 / 部分老视频）。
//! **全部放进 `PlayInfo.progressive`**，由播放器按硬解能力 /
//! 码率挑（`StreamSelection.pickProgressive` 同清晰度取码率最高的那条，正好等于 [`selected`]）。
//!
//! 清晰度没有档位名，按画面**短边**用 [`quality_of_size`] 算（和酷安 / 小红书一样）；HLS 也照样报这一档。
//!
//! **地址会过期**（也带时效签名），所以 [`super::source::XHostedSource::play`] 每次都重新拉一次推文详情，
//! 不复用信息流 / 详情里的旧地址。
//!
//! ## `partId`
//!
//! `media:<媒体下标>`（和 `Block.Video.partId` / `VideoPart.id` 一致，见 [`XVideoMeta::part_id`]）。
//! 认不出来的写法在**发请求之前**就报错（"推文 <id> 里没有 <partId>"那一类文案由平台层补齐）。

use crate::play::{quality_of_size, PlayInfo, ProgressiveStream};
use crate::x::{XError, XVideoMeta, XVideoVariant};

/// 拉流时带的请求头：视频 CDN（`video.twimg.com`）不校验 Referer，但照网页带上更稳。
///
/// 顺序和 Kotlin 的 `mapOf` 一致（`Referer` 在前、`User-Agent` 在后）。
pub fn headers() -> Vec<(String, String)> {
    vec![
        ("Referer".to_string(), "https://x.com/".to_string()),
        (
            "User-Agent".to_string(),
            super::source::USER_AGENT.to_string(),
        ),
    ]
}

/// `media:<下标>` → 下标（Kotlin `XVideoPlay.mediaIndexOf`）：别的写法、负数、非数字都是 `None`。
pub fn media_index_of(part_id: &str) -> Option<usize> {
    let rest = part_id.strip_prefix("media:")?;
    rest.parse::<i32>().ok().filter(|n| *n >= 0).map(|n| n as usize)
}

/// 选哪个 variant（Kotlin `XVideo.selected`）：
/// 1. `content_type == "video/mp4"`、`bitrate` 非空、`url` 非空 → 按码率**降序**的第一条；
/// 2. 一个 mp4 都没有时，退回第一个有 url 的 variant（一般是直播的 HLS，播放器自己走 hlsdemux）。
pub fn selected(video: &XVideoMeta) -> Option<&XVideoVariant> {
    // `mp4_by_bitrate_desc` 返回 `Vec<&XVideoVariant>`，`first()` 是 `Option<&&…>`，要解一层
    mp4_by_bitrate_desc(video)
        .first()
        .copied()
        .or_else(|| video.variants.iter().find(|v| !v.url.is_empty()))
}

/// 按码率降序的全部 mp4（Kotlin `XVideo.mp4ByBitrateDesc`）：Kotlin 用 `sortedByDescending`（稳定排序），
/// 所以同码率的几条**保持原顺序**（Rust 的 `sort_by` 也是稳定排序）。
pub fn mp4_by_bitrate_desc(video: &XVideoMeta) -> Vec<&XVideoVariant> {
    let mut out: Vec<&XVideoVariant> = video
        .variants
        .iter()
        .filter(|v| v.content_type == "video/mp4" && v.bitrate.is_some() && !v.url.is_empty())
        .collect();
    out.sort_by(|a, b| b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0)));
    out
}

/// 一个视频的播放地址（Kotlin `XVideoPlay.playInfo`）。没有可用地址（variant 全空）返回 `None`，
/// 调用方报"播不了"。
pub fn play_info(video: &XVideoMeta) -> Option<PlayInfo> {
    let quality = quality_of_size(video.width, video.height);
    let mp4 = mp4_by_bitrate_desc(video);
    let progressive: Vec<ProgressiveStream> = if !mp4.is_empty() {
        // 同一分辨率的不同码率各给一条，bandwidth 让播放器挑（默认挑最高的那条）
        mp4.iter()
            .map(|v| ProgressiveStream {
                quality: quality.as_ref().map(|q| q.id),
                url: v.url.clone(),
                backup_urls: Vec::new(),
                size_bytes: None,
                width: video.width,
                height: video.height,
                codec: None,
                bandwidth: v.bitrate,
            })
            .collect()
    } else {
        // 一个 mp4 都没有：退回第一个有 url 的（一般是直播的 HLS，播放器自己走 hlsdemux）
        let fallback = selected(video)?;
        vec![ProgressiveStream {
            quality: quality.as_ref().map(|q| q.id),
            url: fallback.url.clone(),
            backup_urls: Vec::new(),
            size_bytes: None,
            width: video.width,
            height: video.height,
            codec: None,
            bandwidth: None,
        }]
    };
    Some(PlayInfo {
        duration_ms: video.duration_ms,
        quality: quality.as_ref().map(|q| q.id),
        qualities: quality.into_iter().collect(),
        progressive,
        headers: headers(),
        ..PlayInfo::default()
    })
}

/// **只校验 `partId` 的写法**（`media:<非负整数>`），不解析、不发请求。
///
/// 平台层在拉 `TweetDetail` 之前先调它一次（参数不对就不该打接口——X 的详情是一条带签名的 GraphQL）。
pub fn check_part(part_id: &str) -> Result<(), XError> {
    match media_index_of(part_id) {
        Some(_) => Ok(()),
        None => Err(XError::Parse {
            detail: format!("推文里没有 {part_id}（partId 应该像 media:0）"),
        }),
    }
}

