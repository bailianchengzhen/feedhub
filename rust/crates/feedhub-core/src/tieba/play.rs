//! 贴吧的**播放取流**（`source-tieba/TiebaVideo.kt` 的 Rust 版）。
//!
//! 贴吧不是 DASH：楼层内容段 `type 5`（`PbContent`）的 `link` 是 mp4 直链
//! （`tb-video.bdstatic.com/tieba-smallvideo-transcode-crf/….mp4?vt=1&…&tbau=<日期>_<签名>&ptid=<tid>`），
//! 只有一档清晰度，**不需要 Referer**，所以 `headers` 是空的。
//!
//! | `partId` | 是什么 | 地址从哪来 |
//! |---|---|---|
//! | [`MAIN`]（`main`） | 整条帖子的主视频（一楼的第一个视频） | `pb/page` 第一页（5 分钟内复用详情那份） |
//! | [`url:<mp4>`](inline_part) | 楼层 / 正文里夹着的视频 | 就是 `partId` 里那个地址，**不再请求** |
//!
//! 两个口径（照 Kotlin）：
//! 1. **只有第一个**内容段里的视频是主视频（后面的留在正文里原地播，`partId = url:…`）；
//! 2. 宽高优先用 `thread.video_info` 里**转码后**的那一份，
//!    清晰度按后者算。
//!
//! 地址里的 `tbau` 带到期日期，所以"什么时候重新取 `pb/page`"是
//! [`super::source::TiebaHostedSource::main_video`] 的事（5 分钟），这一层只做纯解析 / 纯拼装。

use crate::play::{quality_of_size, PlayInfo, ProgressiveStream};

use super::content::{self, MainVideo};
use super::detail;
use super::{Error, TIEBA};

/// 整条帖子的主视频（Kotlin `TiebaVideo.MAIN`）。
pub const MAIN: &str = "main";

/// 楼层 / 正文里夹着的视频前缀：`partId` 直接带着 mp4 地址（Kotlin `TiebaVideo.INLINE`）。
pub const INLINE: &str = "url:";

/// `partId = url:<mp4>`（Kotlin `TiebaVideo.inlinePart`）。
pub fn inline_part(mp4: &str) -> String {
    format!("{INLINE}{mp4}")
}

/// 地址里的 `ptid` 就是帖子 tid（Kotlin `TiebaVideo.threadIdOf`）；没有 / 空白就是 `None`。
pub fn thread_id_of(mp4: &str) -> Option<String> {
    content::thread_id_of(mp4)
}

/// 一楼的主视频（Kotlin `TiebaVideo.main`）：一楼内容段里的第一个视频；没有就退到
/// `thread.video_info.videoUrl`；都没有就是 `None`（调用方翻成"一楼里没有视频"）。
///
/// [`bytes`] 是 `pb/page` 第一页的原始响应。[`Error`] 只会在 protobuf 解不开时出现。
pub fn main(bytes: &[u8]) -> Result<Option<MainVideo>, Error> {
    let res = detail::decode_page(bytes)?;
    Ok(detail::page_main_video(&res))
}

/// 主视频 → 播放信息（Kotlin `TiebaVideo.play(video)`）。
pub fn play(video: &MainVideo) -> PlayInfo {
    play_of(&video.url, video.duration_sec, video.width, video.height)
}

/// `partId = url:<mp4>` 的播放信息（Kotlin `TiebaVideo.playInline`）：时长 / 宽高楼层里没有，
/// 播放器从视频里读。写法不对（不是 `url:` 开头、地址不是 http(s)）报 `不认识的视频 <partId>`。
pub fn play_inline(part_id: &str) -> Result<PlayInfo, Error> {
    let url = match part_id.strip_prefix(INLINE) {
        Some(rest) if rest.starts_with("http") => rest,
        _ => return Err(unknown_part(part_id)),
    };
    Ok(play_of(url, None, None, None))
}

/// **只校验 `partId` 的写法**，不解析、不发请求：`main` 或 `url:http…`。
///
/// 平台层在取 `pb/page` 之前先调它一次（参数不对就不该打接口）。
pub fn check_part(part_id: &str) -> Result<(), Error> {
    if part_id == MAIN {
        return Ok(());
    }
    match part_id.strip_prefix(INLINE) {
        Some(rest) if rest.starts_with("http") => Ok(()),
        _ => Err(unknown_part(part_id)),
    }
}

fn play_of(url: &str, duration_sec: Option<i32>, width: Option<i32>, height: Option<i32>) -> PlayInfo {
    let quality = quality_of_size(width, height);
    PlayInfo {
        duration_ms: duration_sec.map(|d| d as i64 * 1000),
        quality: quality.as_ref().map(|q| q.id),
        qualities: quality.clone().into_iter().collect(),
        progressive: vec![ProgressiveStream {
            quality: quality.map(|q| q.id),
            url: url.to_string(),
            backup_urls: Vec::new(),
            size_bytes: None,
            width,
            height,
            codec: None,
            bandwidth: None,
        }],
        // 贴吧的 mp4 直链不需要 Referer / UA，所以没有 headers——照 Kotlin 的空 map
        ..PlayInfo::default()
    }
}

fn unknown_part(part_id: &str) -> Error {
    Error::Parse {
        detail: format!("不认识的视频 {part_id}"),
    }
}

/// 平台 id（文案里要用，和别处一致）。
pub const PLATFORM: &str = TIEBA;

