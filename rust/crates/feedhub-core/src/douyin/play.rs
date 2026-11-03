//! 抖音的**播放地址**（`/aweme/v1/web/aweme/detail/` 响应里的 `video` → [`PlayInfo`]）。
//!
//! 逐行照抄 `source-douyin/DouyinVideo.kt`（`streams` / `size` / `quality` / `play` / `urlList` /
//! `miscDownloadAddrs`），以及 `DouyinVideo.info` 的详情那一小半（`FeedDetail.video` 也在这里，
//! 详情和播放必须挑同一批流，别各写一份）。
//!
//! ## 地址从哪来
//!
//! 响应里能播的地址有好几处，`video` 对象里就有三重：
//!
//! - `video.bit_rate[]`：每档清晰度一项，带 `bit_rate`（码率）、`is_h265`（0 = AVC、1 = HEVC）、
//!   `play_addr`（这一档的地址）、`play_addr_265` / `play_addr_h264`（同一档的另一种编码）、`FPS`、`format`。
//!   **这一层最全**：分辨率写在各自的 `play_addr.width` / `height` 上，所以清晰度按每条流自己的尺寸算
//!   （高分辨率的 H.265 码率可能比低分辨率的 H.264 还低，只取第一条会漏档）。
//!   `format: "dash"` 的那些是**只有画面**的 DASH 轨（声音在 `bit_rate_audio` 里），**不收**。
//! - `video.play_addr` / `play_addr_265` / `play_addr_h264`：作品的主地址。
//! - `video.download_addr` / `video.misc_download_addrs`：带水印的下载地址，**只在别的都没有时兜底**。
//!
//! ## 每个 `url_list` 里是两种形态，要挑跳转那种
//!
//! `url_list` 是 `[CDN 直链, CDN 直链（另一台）, https://www.douyin.com/aweme/v1/play/?video_id=…]`：
//!
//! | 形态 | 实测 |
//! | CDN 直链 `https://v26-webf.douyinvod.com/…` | **403 Forbidden**（补齐全套头也 403） |
//! | 跳转 `/aweme/v1/play/?…` | **302 → CDN → 206 `video/mp4`**，只要 `Referer` + 浏览器 UA |
//!
//! 所以 [`url_list`] 把"跳转"那种排到最前，CDN 直链留在 `backup_urls` 里。每档清晰度的跳转地址各不
//! 相同（`file_id` / `sign` / `biz_sign` 一档一套），所以清晰度阶梯不会塌掉。
//!
//! ## 时效
//!
//! 两种形态都带时效参数（直链的 `l` / `ft`，跳转的 `sign` / `biz_sign`），**会过期**；复用策略在
//! [`super::source::DouyinHostedSource`]（60 秒内 `detail` 取过的同一条作品，`play` 不再打一次
//! secsdk 保护路径）。

use crate::detail::{VideoInfo, VideoPart};
use crate::douyin::DouyinError;
use crate::json::{self, JsonMap};
use crate::play::{self, PlayInfo, ProgressiveStream, Quality};

/// [`VideoPart::id`]：整条作品就是一个视频（和小黑盒 / 贴子 / 酷安的 `main` 一致）。
pub const MAIN: &str = "main";

/// 一条可播的流。`quality` 是档位 id（拿不到尺寸是 `None`）。
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

/// `video.duration` 是**毫秒**（真实录制：`149676` = 149 秒）。
pub fn duration_sec(video: &JsonMap) -> Option<i32> {
    json::long(video, "duration")
        .filter(|v| *v > 0)
        .map(|v| (v / 1000) as i32)
}

/// 毫秒（`PlayInfo.durationMs` 用；Kotlin 是 `long("duration")?.takeIf { it > 0 }`）。
pub fn duration_ms(video: &JsonMap) -> Option<i64> {
    json::long(video, "duration").filter(|v| *v > 0)
}

/// 作品的像素尺寸：`video.play_addr` 优先，没有就取 [`streams`] 里面积最大的一档。
pub fn size(video: &JsonMap) -> (Option<i32>, Option<i32>) {
    for key in ["play_addr", "play_addr_h264", "play_addr_265"] {
        let Some(addr) = json::obj(video, key) else {
            continue;
        };
        let w = json::int(addr, "width");
        let h = json::int(addr, "height");
        if let (Some(w), Some(h)) = (w, h) {
            if w > 0 && h > 0 {
                return (Some(w), Some(h));
            }
        }
    }
    let sorted = sorted(streams(video));
    largest(&sorted)
}

/// 按面积取最大的一档。Kotlin 的 `maxByOrNull` 返回**第一个**最大值（`<` 才换），这里用严格大于模拟。
fn largest(sorted: &[Stream]) -> (Option<i32>, Option<i32>) {
    let mut best: Option<&Stream> = None;
    for stream in sorted {
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
pub fn streams(video: &JsonMap) -> Vec<Stream> {
    let mut out: Vec<Stream> = Vec::new();
    let main_w = json::int(video, "width");
    let main_h = json::int(video, "height");

    fn add(
        out: &mut Vec<Stream>,
        addr: Option<&JsonMap>,
        codec: play::VideoCodec,
        bandwidth: Option<i64>,
        fallback_h: Option<i32>,
        fallback_w: Option<i32>,
    ) {
        let Some(addr) = addr else { return };
        let urls = url_list(addr);
        let Some(primary) = urls.first().cloned() else {
            return;
        };
        if out.iter().any(|s| s.url == primary) {
            return;
        }
        let w = json::int(addr, "width").filter(|v| *v > 0).or(fallback_w);
        let h = json::int(addr, "height").filter(|v| *v > 0).or(fallback_h);
        out.push(Stream {
            quality: play::quality_id(w, h),
            codec,
            url: primary,
            backup_urls: urls.into_iter().skip(1).collect(),
            width: w,
            height: h,
            bandwidth,
            size_bytes: json::long(addr, "data_size").filter(|v| *v > 0),
        });
    }

    // 1) bit_rate[]：每档清晰度一条，编码按 is_h265 分；同一档的 play_addr_265 / play_addr_h264 是另一种编码
    for value in json::arr(video, "bit_rate").into_iter().flatten() {
        let Some(o) = value.as_object() else { continue };
        if json::long(o, "bit_rate").is_none() && json::obj(o, "play_addr").is_none() {
            continue;
        }
        // `format: "dash"` 的那些是**只有画面、没有声音**的 DASH 轨，放进 progressive 会放出没声音的片子
        if json::text(o, "format").as_deref() == Some("dash") {
            continue;
        }
        let bw = json::long(o, "bit_rate");
        let h265 = json::int(o, "is_h265") == Some(1);
        add(
            &mut out,
            json::obj(o, "play_addr"),
            if h265 {
                play::VideoCodec::Hevc
            } else {
                play::VideoCodec::Avc
            },
            bw,
            main_h,
            main_w,
        );
        let other = if h265 {
            json::obj(o, "play_addr_h264")
        } else {
            json::obj(o, "play_addr_265")
        };
        add(
            &mut out,
            other,
            if h265 {
                play::VideoCodec::Avc
            } else {
                play::VideoCodec::Hevc
            },
            bw,
            main_h,
            main_w,
        );
    }

    // 2) 作品主地址
    add(
        &mut out,
        json::obj(video, "play_addr"),
        play::VideoCodec::Avc,
        None,
        main_h,
        main_w,
    );
    add(
        &mut out,
        json::obj(video, "play_addr_h264"),
        play::VideoCodec::Avc,
        None,
        main_h,
        main_w,
    );
    add(
        &mut out,
        json::obj(video, "play_addr_265"),
        play::VideoCodec::Hevc,
        None,
        main_h,
        main_w,
    );

    // 3) 兜底：带水印的下载地址，只有别的都没有时才用得上
    if out.is_empty() {
        add(
            &mut out,
            json::obj(video, "download_addr"),
            play::VideoCodec::Avc,
            None,
            main_h,
            main_w,
        );
    }
    if out.is_empty() {
        for addr in misc_download_addrs(video) {
            add(
                &mut out,
                Some(&addr),
                play::VideoCodec::Avc,
                None,
                main_h,
                main_w,
            );
        }
    }

    sorted(out)
}

/// 稳定排序：清晰度（拿不到按 0）降序，再按码率降序（和 `compareByDescending{}.thenByDescending{}` 一样）。
fn sorted(mut streams: Vec<Stream>) -> Vec<Stream> {
    streams.sort_by(|a, b| {
        let qa = a.quality.unwrap_or(0);
        let qb = b.quality.unwrap_or(0);
        qb.cmp(&qa)
            .then_with(|| b.bandwidth.unwrap_or(0).cmp(&a.bandwidth.unwrap_or(0)))
    });
    streams
}

/// 清晰度档位：短边 ≥ 2000 是 4K（120），1400~1999 是 2K（112），其余按短边分档
/// （这样设置里的"默认清晰度"对所有平台一样有效）。
pub fn quality_of(width: Option<i32>, height: Option<i32>) -> Option<Quality> {
    play::quality(width, height)
}

/// `url_list` 里像地址的那些，**把 `/aweme/v1/play/` 那种跳转排到最前**。
///
/// "像地址"= 带 scheme（`xxx://`）或协议相对（`//`）。脱敏夹具里那些 `redacted-token-1120` 这样的
/// 不透明占位串因此被跳过（不猜地址）；演示环境里的 `file://` 测试视频也能通过。
fn url_list(o: &JsonMap) -> Vec<String> {
    let mut urls: Vec<String> = json::arr(o, "url_list")
        .map(|list| {
            list.iter()
                .filter_map(json::primitive_content)
                .map(|u| u.trim().to_string())
                .filter(|u| u.contains("://") || u.starts_with("//"))
                .map(|u| json::to_https(&u))
                .collect()
        })
        .unwrap_or_default();
    // 稳定排序：跳转地址（`/aweme/v1/play/`）排前面
    urls.sort_by_key(|u| u32::from(!u.contains(PLAY_REDIRECT_PATH)));
    urls
}

/// 是不是 `https://www.douyin.com/aweme/v1/play/…` 那种跳转地址（不是 CDN 直链）。
pub fn is_play_redirect(url: &str) -> bool {
    url.contains(PLAY_REDIRECT_PATH)
}

const PLAY_REDIRECT_PATH: &str = "/aweme/v1/play/";

/// `misc_download_addrs` 是一段 **JSON 字符串**（`{"suffix_scene":{"url_list":[…]}}`）。
/// 只在解析得出来时才用，坏数据直接跳过（不抛异常）。
fn misc_download_addrs(video: &JsonMap) -> Vec<JsonMap> {
    let Some(raw) = json::text(video, "misc_download_addrs") else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(root) = value.as_object() else {
        return Vec::new();
    };
    root.values()
        .filter_map(|v| v.as_object())
        .filter(|o| json::arr(o, "url_list").is_some())
        .cloned()
        .collect()
}

/// `FeedDetail.video`：整条就是视频的作品才有。抖音没有弹幕，`danmaku_segment_ms` 留 `None`。
pub fn info(aweme: &JsonMap) -> Option<VideoInfo> {
    let video = json::obj(aweme, "video")?;
    if json::arr(aweme, "images").is_some_and(|images| !images.is_empty()) {
        return None;
    }
    if streams(video).is_empty()
        && json::obj(video, "cover").is_none()
        && json::long(video, "duration").is_none()
    {
        return None;
    }
    let (width, height) = size(video);
    Some(VideoInfo {
        parts: vec![VideoPart {
            id: MAIN.to_string(),
            title: json::text(aweme, "desc"),
            duration_sec: duration_sec(video),
            width,
            height,
        }],
        danmaku_segment_ms: None,
    })
}

/// 播放地址。抖音只有"音画一体的 mp4"，所以把所有档位、两种编码都放进 [`PlayInfo::progressive`]，
/// 由前端按设置和硬解能力挑。[`quality`] 是前端要的那一档，有就给这一档当默认值。
pub fn play(
    aweme: &JsonMap,
    part_id: &str,
    wanted: Option<i32>,
    headers: Vec<(String, String)>,
) -> Result<PlayInfo, DouyinError> {
    if part_id != MAIN {
        return Err(DouyinError::Api {
            code: "part".to_string(),
            detail: format!("抖音的作品只有一个分段 `{MAIN}`，没有 `{part_id}`"),
        });
    }
    let video = json::obj(aweme, "video").ok_or_else(|| DouyinError::Api {
        code: "no_video".to_string(),
        detail: "这条作品没有 video 对象".to_string(),
    })?;
    let all = streams(video);
    if all.is_empty() {
        return Err(DouyinError::Api {
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

