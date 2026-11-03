//! `CoolapkVideo` 的 Rust 版本：视频字段的解析（信息流卡片 + 详情）。
//!
//! 两块：
//! - [`video`]：信息流卡片要的那一小块（`media_type` / `media_url` / `media_pic` / `media_info`）；
//! - [`hosted`] / [`video_info`] / [`external_card`] / [`bilibili_target`] / [`is_short_link`]：**详情**要的
//!   ——酷安自己托管的视频（进 `FeedDetail.video`，`partId=main`）、站外视频卡片、B站链接的目标。
//!
//! 第三块：
//! - [`play`]：酷安托管的整段 mp4 → **播放通用模型** [`PlayInfo`]（`feedhub-core/src/play.rs`，11 家共用）；
//! - [`play_from_detail`]：`/v6/feed/detail` 的响应 → `(托管视频, PlayInfo)`，错误映射走
//!   [`crate::coolapk::decode_json`]（和 Kotlin `getJson` 的尾巴一字不差），没有托管视频就是 `no_video`。
//!
//! **Range 探针不在这一层**：Kotlin `CoolapkSource.play` 拿到地址后要用**另一个客户端**（不带 Cookie、
//! 不加设备头、不占 600ms 限速的 `mediaHttp`）发一次 `Range: bytes=0-0`——那是"请求怎么发"，
//! 在平台对象 [`crate::coolapk::source::CoolapkHostedSource::play`] 里，走第三个宿主端口。
//! 酷安托管的视频有 URL 签名防盗链（`403 need sign info`），签名我们算不出来，所以只能探一下如实报错。

use crate::detail::{Block, VideoInfo, VideoPart};
use crate::json;
use crate::model::{DetailTarget, Media};
use crate::play::{PlayInfo, ProgressiveStream, Quality};

/// 拉流时要带的 UA（Kotlin `UserAgents.DESKTOP_CHROME`，和 [`crate::coolapk::source`] 那份逐字一样）。
const DESKTOP_CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 酷安托管视频的 `partId`（`CoolapkVideo.MAIN`）。
pub const MAIN: &str = "main";

/// B站 的 BV 号（`BV1[1-9A-HJ-NP-Za-km-z]{9}`）。
const BVID_LEN: usize = 12;

/// 认的视频扩展名（`CoolapkVideo.VIDEO_EXT`）。
const VIDEO_EXT: [&str; 5] = ["mp4", "m4v", "mov", "webm", "m3u8"];


/// `media_info`（一个**字符串形式的 JSON**）里解析出来的东西。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub duration_ms: Option<i64>,
    pub cover: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

impl Info {
    /// `durationSec = (durationMs / 1000).toInt()`，`<= 0` 就当没有。
    pub fn duration_sec(&self) -> Option<i32> {
        self.duration_ms
            .map(|ms| (ms / 1000) as i32)
            .filter(|s| *s > 0)
    }
}

/// `media_info` 是 JSON 字符串；解析不了就当没有（Kotlin 用 `runCatching` 兜住）。
pub fn info(o: &json::JsonMap) -> Info {
    let parsed = json::text(o, "media_info")
        .and_then(|raw| json::parse_object(&raw).ok())
        .unwrap_or_default();
    Info {
        duration_ms: json::long(&parsed, "duration").filter(|n| *n > 0),
        cover: json::text(&parsed, "cover").map(|c| json::to_https(&c)),
        width: json::int(&parsed, "width").filter(|n| *n > 0),
        height: json::int(&parsed, "height").filter(|n| *n > 0),
    }
}

/// 一条动态的视频（对应 `CoolapkParser.video`，注意**不是** `CoolapkVideo.hosted`）：
/// `media_type != 0` 且（有 `media_pic` 或 `media_info.cover`）或有 `media_url` 时算视频，
/// 封面用 `media_pic`（没有就用 `media_info.cover`），时长在 `media_info` 里。
///
/// 宽高从 `media_info` 里带上（有的条目有、有的没有）：卡片和沉浸界面靠它一开始就按竖屏 / 横屏摆，
/// 不用等播放器报尺寸再变一次。
/// Kotlin 原实现这里是不带的。
pub fn video(o: &json::JsonMap) -> Option<Media> {
    let media_type = json::long(o, "media_type").unwrap_or(0);
    if media_type == 0 {
        return None;
    }
    let info = info(o);
    let cover = json::text(o, "media_pic").or_else(|| info.cover.clone());
    if cover.is_none() && json::text(o, "media_url").is_none() {
        return None;
    }
    Some(Media::Video {
        cover: cover.map(|c| json::to_https(&c)),
        duration_sec: info.duration_sec(),
        width: info.width,
        height: info.height,
    })
}

// ===========================================================================
// 详情：托管视频 / 站外卡片 / B站链接（`CoolapkVideo.hosted` / `externalCard` / `bilibiliTarget`）
// ===========================================================================

/// 酷安自己托管的视频（`media_url` 指向 `*.coolapk.com` 下的视频文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosted {
    pub url: String,
    pub info: Info,
}

/// `CoolapkVideo.hasMedia`：`media_type != 0` 且有 `media_url`。
fn has_media(o: &json::JsonMap) -> bool {
    json::long(o, "media_type").unwrap_or(0) != 0 && json::text(o, "media_url").is_some()
}

/// 封面：`media_pic` 优先，没有用 `media_info.cover`（都是 https）。
pub fn cover(o: &json::JsonMap) -> Option<String> {
    json::text(o, "media_pic")
        .or_else(|| info(o).cover.clone())
        .map(|c| json::to_https(&c))
}

/// 酷安自己托管的视频（`*.coolapk.com` 下的视频文件）；站外链接是 `None`。
pub fn hosted(o: &json::JsonMap) -> Option<Hosted> {
    if !has_media(o) {
        return None;
    }
    let url = json::to_https(&json::text(o, "media_url")?);
    let parsed = parse_http_url(&url)?;
    let ext = parsed
        .path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if !(parsed.host == "coolapk.com" || parsed.host.ends_with(".coolapk.com"))
        || !VIDEO_EXT.contains(&ext.as_str())
    {
        return None;
    }
    Some(Hosted {
        info: info(o),
        url,
    })
}

/// `FeedDetail.video`：酷安托管的视频才有（一个分 P，`partId=main`），没有弹幕。
pub fn video_info(hosted: Option<&Hosted>) -> Option<VideoInfo> {
    let h = hosted?;
    Some(VideoInfo {
        parts: vec![VideoPart {
            id: MAIN.to_string(),
            title: None,
            duration_sec: h.info.duration_sec(),
            width: h.info.width,
            height: h.info.height,
        }],
        danmaku_segment_ms: None,
    })
}

/// 站外视频的卡片（酷安托管的返回 `None`）。`resolved` 是 `b23.tv` 短链跟完重定向后的地址。
pub fn external_card(o: &json::JsonMap, resolved: Option<&str>) -> Option<Block> {
    if !has_media(o) || hosted(o).is_some() {
        return None;
    }
    let url = json::to_https(&json::text(o, "media_url")?);
    let target = bilibili_target(resolved.unwrap_or(&url));
    let host = parse_http_url(&url)
        .map(|u| u.host)
        .unwrap_or_else(|| url.clone());
    Some(Block::Card {
        title: if target.is_some() {
            "B站视频".to_string()
        } else {
            "站外视频".to_string()
        },
        subtitle: Some(host),
        cover: cover(o),
        url: Some(url),
        target,
    })
}

/// B站视频链接（`bilibili.com/video/BV…`）→ 指向 B站 Source 的原生详情；别的链接是 `None`。
pub fn bilibili_target(url: &str) -> Option<DetailTarget> {
    let parsed = parse_http_url(url)?;
    if !(parsed.host == "bilibili.com" || parsed.host.ends_with(".bilibili.com")) {
        return None;
    }
    let bv = find_bvid(&parsed.path)?;
    Some(DetailTarget::Native {
        id: bv,
        platform: Some("bili".to_string()),
    })
}

/// `b23.tv` 短链：要跟一次重定向才知道是哪个视频。
pub fn is_short_link(url: &str) -> bool {
    parse_http_url(url)
        .map(|u| u.host == "b23.tv")
        .unwrap_or(false)
}

// ===========================================================================
// 播放：整段 mp4 → 播放通用模型
// ===========================================================================

/// 酷安托管的视频的播放信息（`CoolapkVideo.play`）。
///
/// 地址不变、不需要现取：一条 `ProgressiveStream`（整段 mp4），清晰度按画面短边算
/// （[`quality_of_size`]），DASH 的 video / audio 都是空的（酷安没有 DASH）。
/// 两个请求头**有顺序**（`Referer` 在 `User-Agent` 前面，和 Kotlin 的 `mapOf` 插入顺序一样）。
pub fn play(h: &Hosted) -> PlayInfo {
    let q = quality_of_size(h.info.width, h.info.height);
    PlayInfo {
        duration_ms: h.info.duration_ms,
        quality: q.as_ref().map(|q| q.id),
        qualities: q.iter().cloned().collect(),
        video: Vec::new(),
        audio: Vec::new(),
        progressive: vec![ProgressiveStream {
            quality: q.as_ref().map(|q| q.id),
            url: h.url.clone(),
            backup_urls: Vec::new(),
            size_bytes: None,
            width: h.info.width,
            height: h.info.height,
            codec: None,
            bandwidth: None,
        }],
        headers: vec![
            ("Referer".to_string(), format!("{}/", crate::coolapk::WEB)),
            ("User-Agent".to_string(), DESKTOP_CHROME.to_string()),
        ],
    }
}

/// `/v6/feed/detail` 的响应 → `(托管视频, 播放信息)`。
///
/// `http_code` 一起传进来：错误映射（业务码 / 状态码）在 [`crate::coolapk::decode_json`] 里，
/// 和 Kotlin `CoolapkSource.getJson` 的尾巴一字不差。没有酷安托管的视频（站外视频 / 图文动态）是
/// `Api { code: "no_video" }`，文案照 Kotlin。
pub fn play_from_detail(body: &str, http_code: i32) -> Result<(Hosted, PlayInfo), crate::coolapk::CoolapkError> {
    let json = crate::coolapk::decode_json(body, http_code)?;
    let hosted = json::obj(&json, "data")
        .and_then(hosted)
        .ok_or_else(|| crate::coolapk::CoolapkError::Api {
            code: "no_video".to_string(),
            detail: "这条动态里没有酷安自己托管的视频（站外视频在详情里是卡片）".to_string(),
        })?;
    let info = play(&hosted);
    Ok((hosted, info))
}

/// Kotlin `core/Video.kt` 的 `qualityOfSize`：按画面**短边**定档（120 = 4K、80 = 1080P、64 = 720P、
/// 32 = 480P、16 = 360P）。宽高都没给 / 都不大于 0 就是 `None`。
///
/// 在 [`crate::play::quality_of_size`]（抖音 / TikTok 那条分支并进来时
/// 逐行比对过，口径一模一样）；这里留着同名薄壳，好让 `coolapk::video` 的调用点和老测试不用改。
pub fn quality_of_size(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    crate::play::quality_of_size(width, height)
}

/// `Regex("BV1[1-9A-HJ-NP-Za-km-z]{9}")` 的第一次命中。
fn find_bvid(path: &str) -> Option<String> {
    let b = path.as_bytes();
    if b.len() < BVID_LEN {
        return None;
    }
    for start in 0..=b.len() - BVID_LEN {
        if b[start] != b'B' || b[start + 1] != b'V' || b[start + 2] != b'1' {
            continue;
        }
        let tail = &b[start + 3..start + BVID_LEN];
        let ok = tail
            .iter()
            .all(|c| matches!(c, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z'));
        if ok {
            return Some(String::from_utf8_lossy(&b[start..start + BVID_LEN]).into_owned());
        }
    }
    None
}

/// 一个 http(s) URL 的三段（够判断 host / 扩展名 / BV 号就收）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// 小写的 host（不含端口）。
    pub host: String,
    /// `:` 之后的路径（原样，不做百分号解码）。
    pub path: String,
}

/// 宽松版的 `toHttpUrlOrNull()`：scheme 必须是 http / https，host 非空；别的返回 `None`。
///
/// OkHttp 还会做 path 规范化和百分号编解码，这里不需要：调用方只看 host、扩展名和 BV 号。
pub fn parse_http_url(url: &str) -> Option<Url> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    let tail = &rest[authority_end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_end];
    Some(Url {
        host: host.to_ascii_lowercase(),
        path: if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        },
    })
}
