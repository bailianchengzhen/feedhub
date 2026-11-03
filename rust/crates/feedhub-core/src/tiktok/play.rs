//! TikTok 的**播放地址**（视频页 hydration 的 `itemStruct.video` → [`PlayInfo`]）。
//!
//! 逐行照抄 `source-tiktok/TiktokVideo.kt`（`streams` / `size` / `quality` / `codecOf` / `play` /
//! `addrUrls` / `urlList` / `coverUrl`），`FeedDetail.video`（`info`）也在这里——详情和播放必须挑
//! 同一批流。
//!
//! ## 地址从哪来
//!
//! - `video.playAddr`：**字符串**（不是对象），同一源（`v16-webapp-prime.tiktok.com` 之类）的 mp4；
//! - `video.downloadAddr`：带水印的下载地址，只在前面的都没有时兜底；
//! - `video.bitrateInfo[]`：每档清晰度一项，字段名是**大驼峰**：`Bitrate`、`CodecType`、`GearName`、
//!   `QualityType`、`PlayAddr{UrlList, Width, Height, DataSize, FileHash, Uri}`。
//!
//! 这些地址**都带时效签名**（`VExpiration` / `VSignature` 或 `l` / `ft` 那类），所以
//! [`super::source::TiktokHostedSource::play`] **每次重新取一次视频页**，不复用详情里那份。
//!
//! ## 编码
//!
//! 每档清晰度按 `CodecType` 分 AVC / HEVC / AV1，全部列进 [`PlayInfo::progressive`]，由前端按硬件解码
//! 能力挑。TikTok web 给的是**音画一体的 mp4**（`video_encoding=dash` 只是 query 里的一个开关，返回的
//! 仍是整段 mp4），所以走 progressive、没有 DASH 轨、也没有弹幕。
//!
//! ## 两个实测修正过的坑
//!
//! - `video.duration` **是秒**（1044 那条的 `size × 8 / Bitrate` 正好对上），不是毫秒；
//! - `bitrateInfo[].CodecType` 的真实取值是 `h264` 与 **`h265_hvc1`**，
//!   所以按子串判，别只认字面量。

use crate::detail::{VideoInfo, VideoPart};
use crate::json::{self, JsonMap};
use crate::play::{self, PlayInfo, ProgressiveStream, Quality};
use crate::tiktok::TiktokError;

/// [`VideoPart::id`]：整条作品就是一个视频（和抖音 / 小黑盒 / 贴子 / 酷安的 `main` 一致）。
pub const MAIN: &str = "main";

/// 一条可播的流。
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub quality: Option<i32>,
    pub codec: play::VideoCodec,
    pub url: String,
    pub backup_urls: Vec<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub bandwidth: Option<i64>,
    pub size_bytes: Option<i64>,
}

/// `video.duration` **是秒**。
pub fn duration_sec(video: &JsonMap) -> Option<i32> {
    json::long(video, "duration")
        .filter(|v| *v > 0)
        .map(|v| v as i32)
}

/// 毫秒（`PlayInfo.durationMs`）。
pub fn duration_ms(video: &JsonMap) -> Option<i64> {
    duration_sec(video).map(|s| i64::from(s) * 1000)
}

/// 作品的像素尺寸：`video.width/height` → 面积最大的一档 bitrateInfo。
pub fn size(video: &JsonMap) -> (Option<i32>, Option<i32>) {
    let w = json::int(video, "width");
    let h = json::int(video, "height");
    if let (Some(w), Some(h)) = (w, h) {
        if w > 0 && h > 0 {
            return (Some(w), Some(h));
        }
    }
    let sorted = sorted(streams(video));
    // Kotlin 的 `maxByOrNull` 返回**第一个**最大值（`<` 才换）
    let mut best: Option<&Stream> = None;
    for stream in &sorted {
        let area =
            |s: &Stream| i64::from(s.width.unwrap_or(0)) * i64::from(s.height.unwrap_or(0));
        if best.is_none_or(|b| area(stream) > area(b)) {
            best = Some(stream);
        }
    }
    match best {
        Some(stream) => (stream.width, stream.height),
        None => (None, None),
    }
}

/// `video` 对象里所有能播的流，同一个地址只留一条；按清晰度、再按码率从高到低排。
///
/// `bitrateInfo` 里的每条本身就是一档（带自己的 `PlayAddr.Width/Height`），所以清晰度按**每条流自己
/// 的短边**算（高分辨率的 H.265 码率可能比低分辨率的 H.264 还低，只取第一条会漏档）。
pub fn streams(video: &JsonMap) -> Vec<Stream> {
    let mut out: Vec<Stream> = Vec::new();

    fn add(
        out: &mut Vec<Stream>,
        urls: Vec<String>,
        codec: play::VideoCodec,
        bandwidth: Option<i64>,
        size_bytes: Option<i64>,
        w: Option<i32>,
        h: Option<i32>,
    ) {
        let Some(primary) = urls.first().cloned() else {
            return;
        };
        if out.iter().any(|s| s.url == primary) {
            return;
        }
        out.push(Stream {
            quality: play::quality_id(w, h),
            codec,
            url: primary,
            backup_urls: urls.into_iter().skip(1).collect(),
            width: w,
            height: h,
            bandwidth,
            size_bytes,
        });
    }

    for value in json::arr(video, "bitrateInfo").into_iter().flatten() {
        let Some(o) = value.as_object() else { continue };
        let Some(addr) = json::obj(o, "PlayAddr").or_else(|| json::obj(o, "playAddr")) else {
            continue;
        };
        let codec_type = json::text(o, "CodecType").or_else(|| json::text(o, "codecType"));
        add(
            &mut out,
            url_list(addr),
            codec_of(codec_type.as_deref()),
            json::long(o, "Bitrate").or_else(|| json::long(o, "bitrate")),
            json::long(addr, "DataSize").filter(|v| *v > 0),
            json::int(addr, "Width").or_else(|| json::int(addr, "width")),
            json::int(addr, "Height").or_else(|| json::int(addr, "height")),
        );
    }

    // 作品主地址：web 给的是**字符串**（有些版本给 `{UrlList:[…]}`），两种形态都认。
    // 它没有自己的尺寸，用 `video.width/height`。
    let main_w = json::int(video, "width");
    let main_h = json::int(video, "height");
    for url in addr_urls(video, "playAddr") {
        add(
            &mut out,
            vec![url],
            play::VideoCodec::Avc,
            None,
            None,
            main_w,
            main_h,
        );
    }
    if out.is_empty() {
        for url in addr_urls(video, "downloadAddr") {
            add(
                &mut out,
                vec![url],
                play::VideoCodec::Avc,
                None,
                None,
                main_w,
                main_h,
            );
        }
    }

    sorted(out)
}

/// 稳定排序：清晰度（拿不到按 0）降序，再按码率降序。
fn sorted(mut streams: Vec<Stream>) -> Vec<Stream> {
    streams.sort_by(|a, b| {
        let qa = a.quality.unwrap_or(0);
        let qb = b.quality.unwrap_or(0);
        qb.cmp(&qa)
            .then_with(|| b.bandwidth.unwrap_or(0).cmp(&a.bandwidth.unwrap_or(0)))
    });
    streams
}

/// `CodecType` → 统一编码。真实取值：`h264` 与 **`h265_hvc1`**，所以按子串判。
pub fn codec_of(codec_type: Option<&str>) -> play::VideoCodec {
    let Some(raw) = codec_type else {
        return play::VideoCodec::Avc;
    };
    let t = raw.to_lowercase();
    if t.contains("av1") || t.contains("av01") {
        play::VideoCodec::Av1
    } else if t.contains("hvc1")
        || t.contains("h265")
        || t.contains("hevc")
        || t.contains("bytevc1")
    {
        play::VideoCodec::Hevc
    } else if t.contains("h264") || t.contains("avc") {
        play::VideoCodec::Avc
    } else {
        play::VideoCodec::Other
    }
}

/// 清晰度档位：短边 ≥ 2000 是 4K（120），1400~1999 是 2K（112），其余按短边分档。
pub fn quality_of(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    play::quality(width, height)
}

/// `UrlList` / `urlList` 里像地址的那些，去重后保留顺序。
pub fn url_list(o: &JsonMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let list = json::arr(o, "UrlList").or_else(|| json::arr(o, "urlList"));
    for value in list.into_iter().flatten() {
        let Some(raw) = json::primitive_content(value) else {
            continue;
        };
        let raw = raw.trim().to_string();
        if !(raw.contains("://") || raw.starts_with("//")) {
            continue;
        }
        let url = json::to_https(&raw);
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// `video.<key>` 的地址列表，两种形态都认（字符串 / `{UrlList:[…]}`）。
pub fn addr_urls(video: &JsonMap, key: &str) -> Vec<String> {
    if let Some(o) = json::obj(video, key) {
        return url_list(o);
    }
    match json::text(video, key) {
        Some(text) if text.contains("://") || text.starts_with("//") => vec![json::to_https(&text)],
        _ => Vec::new(),
    }
}

/// 封面：`video.cover` / `originCover` / `dynamicCover` 也是**字符串**，两种形态都认。
pub fn cover_url(video: &JsonMap) -> Option<String> {
    addr_urls(video, "cover")
        .into_iter()
        .next()
        .or_else(|| addr_urls(video, "originCover").into_iter().next())
        .or_else(|| addr_urls(video, "dynamicCover").into_iter().next())
}

/// `FeedDetail.video`：整条就是视频的作品才有。TikTok 没有弹幕，`danmaku_segment_ms` 留 `None`。
pub fn info(o: &JsonMap) -> Option<VideoInfo> {
    let video = json::obj(o, "video")?;
    if json::obj(o, "imagePost")
        .and_then(|p| json::arr(p, "images"))
        .is_some_and(|images| !images.is_empty())
    {
        return None;
    }
    if streams(video).is_empty()
        && cover_url(video).is_none()
        && json::long(video, "duration").is_none()
    {
        return None;
    }
    let (width, height) = size(video);
    Some(VideoInfo {
        parts: vec![VideoPart {
            id: MAIN.to_string(),
            title: json::text(o, "desc"),
            // `video.duration` **是秒**（不是毫秒）
            duration_sec: duration_sec(video),
            width,
            height,
        }],
        danmaku_segment_ms: None,
    })
}

/// 播放地址。TikTok 只有"音画一体的 mp4"，所以所有档位、各种编码都放进 [`PlayInfo::progressive`]，
/// 由前端按设置和硬解能力挑。[`wanted`] 是前端要的那一档，有就给这一档当默认值。
pub fn play(
    o: &JsonMap,
    part_id: &str,
    wanted: Option<i32>,
    headers: Vec<(String, String)>,
) -> Result<PlayInfo, TiktokError> {
    if part_id != MAIN {
        return Err(TiktokError::Api {
            code: "part".to_string(),
            detail: format!("TikTok 的作品只有一个分段 `{MAIN}`，没有 `{part_id}`"),
        });
    }
    let video = json::obj(o, "video").ok_or_else(|| TiktokError::Api {
        code: "no_video".to_string(),
        detail: "这条作品没有 video 对象".to_string(),
    })?;
    let all = streams(video);
    if all.is_empty() {
        return Err(TiktokError::Api {
            code: "no_stream".to_string(),
            detail: "这条作品没有可播放的地址（可能已删除或只对 App 开放）".to_string(),
        });
    }
    let picked = wanted
        .and_then(|q| all.iter().find(|s| s.quality == Some(q)))
        .unwrap_or(&all[0]);
    let mut qualities: Vec<Quality> = Vec::new();
    for stream in &all {
        if let Some(q) = quality_of(stream.width, stream.height) {
            if !qualities.iter().any(|seen| seen.id == q.id) {
                qualities.push(q);
            }
        }
    }
    qualities.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(PlayInfo {
        duration_ms: duration_ms(video),
        quality: picked.quality,
        qualities,
        video: Vec::new(),
        audio: Vec::new(),
        progressive: all
            .iter()
            .map(|s| ProgressiveStream {
                quality: s.quality,
                url: s.url.clone(),
                backup_urls: s.backup_urls.clone(),
                size_bytes: s.size_bytes,
                width: s.width,
                height: s.height,
                codec: Some(s.codec),
                bandwidth: s.bandwidth,
            })
            .collect(),
        headers,
    })
}

