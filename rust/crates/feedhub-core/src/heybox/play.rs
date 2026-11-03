//! 小黑盒的**播放取流**（`source-heybox/HeyboxVideo.kt` 的 Rust 版）。
//!
//! 小黑盒不是 DASH：帖子自己的视频（视频帖，`has_video=1`）和正文里夹着的视频各是一段 mp4 直链
//! （地址是 `.m3u8` 的话照样放进 `progressive`，播放器按 HLS 播），只有一档清晰度。
//!
//! | `partId` | 是什么 | 地址从哪来 |
//! |---|---|---|
//! | [`MAIN`]（`main`） | 视频帖自己的视频（`link.video_url`） | `link/tree` 的 `result.link` |
//! | [`seg:<n>`](seg_part) | 正文里夹着的第 n 个视频 | 正文扫描器的 `VideoSeq.refs[n-1]` |
//!
//! **地址里的 `auth` 是临时签名**，
//! 所以"什么时候重新取 `link/tree`"是 [`super::source::HeyboxHostedSource::play`] 的事（60 秒内复用详情那份，
//! 再旧就现取），这一层只做纯解析——没有任何缓存。
//!
//! 参数（`partId`）不认识的写法照 Kotlin 抛 `ParseException`，**不发请求**。

use crate::detail::{VideoInfo, VideoPart};
use super::detail::{self, BodyEnv, VIDEO_MAIN};
use super::text::{self, VideoRef};
use super::{HeyboxError, HEYBOX};
use crate::json::{self, JsonMap};
use crate::play::{quality_of_size, PlayInfo, ProgressiveStream};

/// 视频帖自己的视频（Kotlin `HeyboxVideo.MAIN`）。
pub const MAIN: &str = VIDEO_MAIN;

/// 播放时要带的请求头（网页的）：`Referer` 在前、`User-Agent` 在后，和 Kotlin 的 `mapOf` 顺序一致。
///
/// 不带 Referer 也能拉，但照网页带上更稳；顺序是 `PlayInfo.headers` 的顺序，
/// 播放器只按名查，所以这里保持和 Kotlin 一样只是为了"两份实现看起来一样"。
pub fn headers() -> Vec<(String, String)> {
    vec![
        ("Referer".to_string(), "https://www.xiaoheihe.cn/".to_string()),
        (
            "User-Agent".to_string(),
            super::source::USER_AGENT.to_string(),
        ),
    ]
}

/// 正文里第 n 个视频的 `partId`（Kotlin `HeyboxVideo.segPart`）。
pub fn seg_part(n: usize) -> String {
    format!("seg:{n}")
}

/// 视频帖自己的那个视频（Kotlin `HeyboxVideo.Ref`）：没有 `has_video`、或只有封面没地址就是 `None`。
pub fn main(link: &JsonMap) -> Option<VideoRef> {
    let (url, duration_sec, width, height) = text::main_video(link)?;
    Some(VideoRef {
        url,
        cover: json::text(link, "video_thumb").map(|c| json::to_https(&c)),
        duration_sec,
        width,
        height,
    })
}

/// **只校验 `partId` 的写法**，不解析、不发请求。
///
/// 平台层在取 `link/tree` 之前先调它一次：不认识的写法（`main2` / `seg:x` / 空串）
/// **一个请求都不发**（和 Kotlin 的"参数不对就不该打接口"规矩一致，请求等价门守着）。
/// `seg:0` / `seg:9` 这种"写法对、帖子里没有"留给 [`play`] 报"找不到"。
pub fn check_part(part_id: &str) -> Result<(), HeyboxError> {
    if part_id == MAIN {
        return Ok(());
    }
    let Some(rest) = part_id.strip_prefix("seg:") else {
        return Err(unknown_part(part_id));
    };
    match rest.parse::<i32>() {
        Ok(n) if n >= 0 => Ok(()),
        _ => Err(unknown_part(part_id)),
    }
}

/// `link/tree` 的 `result.link` → 播放地址（Kotlin `HeyboxVideo.play`）。
///
/// [`partId`] 见模块说明；不认识的写法抛 `ParseException`（`不认识的视频 <partId>`），
/// 认得出但帖子里没有那个视频抛 `帖子里找不到视频 <partId>（可能已删除）`。
pub fn play(link: &JsonMap, part_id: &str) -> Result<PlayInfo, HeyboxError> {
    let r#ref = match part_id {
        MAIN => main(link),
        other => match other.strip_prefix("seg:") {
            Some(rest) => {
                let n: usize = rest
                    .parse::<i32>()
                    .ok()
                    .filter(|n| *n >= 0)
                    .map(|n| n as usize)
                    .ok_or_else(|| unknown_part(part_id))?;
                // Kotlin `getOrNull(n - 1)`：n = 0 时 `-1` 越界 → None → "找不到"
                let (_, refs) = detail::body_with_videos(link, &BodyEnv::default());
                n.checked_sub(1).and_then(|i| refs.get(i).cloned())
            }
            None => return Err(unknown_part(part_id)),
        },
    };
    let Some(r#ref) = r#ref else {
        return Err(HeyboxError::Parse {
            detail: format!("帖子里找不到视频 {part_id}（可能已删除）"),
        });
    };
    let quality = quality_of_size(r#ref.width, r#ref.height);
    Ok(PlayInfo {
        duration_ms: r#ref.duration_sec.map(|d| d as i64 * 1000),
        quality: quality.as_ref().map(|q| q.id),
        qualities: quality.clone().into_iter().collect(),
        progressive: vec![ProgressiveStream {
            quality: quality.map(|q| q.id),
            url: r#ref.url,
            backup_urls: Vec::new(),
            size_bytes: None,
            width: r#ref.width,
            height: r#ref.height,
            codec: None,
            bandwidth: None,
        }],
        headers: headers(),
        ..PlayInfo::default()
    })
}

/// [`FeedDetail.video`]：视频帖才有，一个分 P（`id = main`）。弹幕没有，所以不给 `danmakuSegmentMs`。
pub fn info(link: &JsonMap) -> Option<VideoInfo> {
    main(link).map(|r#ref| VideoInfo {
        parts: vec![VideoPart {
            id: MAIN.to_string(),
            title: None,
            duration_sec: r#ref.duration_sec,
            width: r#ref.width,
            height: r#ref.height,
        }],
        danmaku_segment_ms: None,
    })
}

fn unknown_part(part_id: &str) -> HeyboxError {
    HeyboxError::Parse {
        detail: format!("不认识的视频 {part_id}"),
    }
}

/// 平台 id（文案里要用，和 `HeyboxError` 的其它分支一致）。
pub const PLATFORM: &str = HEYBOX;

