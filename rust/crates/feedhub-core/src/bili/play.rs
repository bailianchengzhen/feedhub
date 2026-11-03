//! B站 的**播放地址**：`GET /x/player/wbi/playurl`（WBI 签名）的响应 → [`crate::play::PlayInfo`]。
//!
//! 逐条对照 `source-bilibili/BiliPlayParser.kt`：
//!
//! - 请求带 `fnval=4048`（DASH + HDR + 4K + 杜比 / 视界 + 8K + AV1）时响应里有 `data.dash`：
//!   `video[]`（每个清晰度 × 每种编码 `codecid` 7 AVC / 12 HEVC / 13 AV1）、`audio[]`、`dolby.audio[]`、
//!   `flac.audio`；老视频和不给 DASH 的情况给 `data.durl[]`（整段文件）。
//! - `data.support_formats` 是这个视频的**全部**清晰度（包括没登录 / 不是大会员拿不到的），
//!   哪些真给了流看 `dash.video[].id`；老接口没有 `support_formats`，退回到 `accept_quality` / `accept_description`。
//! - 同一路流的几个地址里，**不稳定的 CDN 主机往后排**（见 [`order`] / [`is_unstable`]）。
//!
//! 说话直说：`is_unstable` 的判据来自 BiliPai 的 `CdnOptimizationPolicy`（GPL-3.0，只读了规则没拷代码），
//! 另加一条"带非默认端口的也算"——真实响应里 `*.edge.mountaintoys.cn:4483` 这种边缘节点出现在备用地址里，
//! BiliPai 的规则没覆盖到。

use std::collections::HashSet;

use crate::bili::source::USER_AGENT;
use crate::hosted::is_kotlin_blank;
use crate::json::{self, JsonMap};
use crate::model::BiliError;
use crate::play::{AudioStream, PlayInfo, ProgressiveStream, Quality, SegmentBase, VideoCodec, VideoStream};

/// 拉流必须带的请求头（B站 CDN 校验 Referer，缺了 403）。
///
/// **顺序 = Kotlin `BilibiliSource.PLAY_HEADERS` 的插入顺序**（`Referer` 在前、`User-Agent` 在后）：
/// 播放器按顺序把它们写到拉流请求上，顺序变了请求字节就变了。
pub fn play_headers() -> Vec<(String, String)> {
    vec![
        ("Referer".to_string(), "https://www.bilibili.com/".to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
    ]
}

/// 播放地址响应（**已经过 `{code}` 外壳检查**）→ [`PlayInfo`]。
///
/// [`headers`] 是原样放进结果里的拉流请求头。
pub fn parse(root: &JsonMap, headers: Vec<(String, String)>) -> Result<PlayInfo, BiliError> {
    let data = json::obj(root, "data").ok_or_else(|| BiliError::Parse {
        detail: "播放地址缺少 data".to_string(),
    })?;
    let dash = json::obj(data, "dash");
    let durl: Vec<&JsonMap> = json::arr(data, "durl")
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    if dash.is_none() && durl.is_empty() {
        return Err(BiliError::Parse {
            detail: "播放地址既没有 dash 也没有 durl".to_string(),
        });
    }

    let mut video: Vec<VideoStream> = dash
        .and_then(|d| json::arr(d, "video"))
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object())
                .filter_map(video_stream)
                .collect()
        })
        .unwrap_or_default();
    // Kotlin `sortedByDescending { it.quality }`：稳定排序，同清晰度保持 B站 给的顺序（AVC / HEVC / AV1）。
    video.sort_by(|a, b| b.quality.cmp(&a.quality));

    let audio = dash.map(audio_streams).unwrap_or_default();
    let current = json::int(data, "quality");
    let progressive: Vec<ProgressiveStream> = durl
        .iter()
        .filter_map(|o| progressive(o, current))
        .collect();

    // DASH：这次给了流的清晰度集合；durl：只有 data.quality 那一档。
    let available: HashSet<i32> = if dash.is_some() {
        video.iter().map(|s| s.quality).collect()
    } else {
        current.into_iter().collect()
    };

    // Kotlin：`data.long("timelength")?.takeIf { it > 0 } ?: dash?.long("duration")?.takeIf { it > 0 }?.times(1000)`
    let duration_ms = match json::long(data, "timelength").filter(|n| *n > 0) {
        Some(ms) => Some(ms),
        None => dash
            .and_then(|d| json::long(d, "duration"))
            .filter(|n| *n > 0)
            .map(|sec| sec.saturating_mul(1000)),
    };

    Ok(PlayInfo {
        duration_ms,
        // DASH 时 `data.quality` 是"请求的"清晰度，实际给到的看视频轨里最高的那条。
        quality: if dash.is_some() {
            video.iter().map(|s| s.quality).max()
        } else {
            current
        },
        qualities: qualities(data, &available),
        video,
        audio,
        progressive,
        headers,
    })
}

/// 清晰度列表：优先 `support_formats`，老接口退回 `accept_quality` + `accept_description`。
fn qualities(data: &JsonMap, available: &HashSet<i32>) -> Vec<Quality> {
    let formats: Vec<&JsonMap> = json::arr(data, "support_formats")
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    if !formats.is_empty() {
        return formats
            .into_iter()
            .filter_map(|f| {
                let id = json::int(f, "quality")?;
                let label = json::text(f, "new_description")
                    .or_else(|| json::text(f, "display_desc"))
                    .unwrap_or_else(|| id.to_string());
                Some(Quality {
                    id,
                    label,
                    available: available.contains(&id),
                })
            })
            .collect();
    }
    let ids: Vec<i32> = json::arr(data, "accept_quality")
        .map(|items| {
            items
                .iter()
                .filter_map(json::primitive_content)
                .filter_map(|s| s.parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default();
    let names: Vec<Option<String>> = json::arr(data, "accept_description")
        .map(|items| items.iter().map(json::primitive_content).collect())
        .unwrap_or_default();
    ids.iter()
        .enumerate()
        .map(|(i, id)| Quality {
            id: *id,
            label: names
                .get(i)
                .cloned()
                .flatten()
                .unwrap_or_else(|| id.to_string()),
            available: available.contains(id),
        })
        .collect()
}

/// 一条 DASH 视频轨。没有 `base_url` 或 `id` 的条目直接丢掉（Kotlin `mapNotNull`）。
fn video_stream(o: &JsonMap) -> Option<VideoStream> {
    let (url, backup_urls) = urls(o)?;
    Some(VideoStream {
        quality: json::int(o, "id")?,
        codec: match json::int(o, "codecid") {
            Some(7) => VideoCodec::Avc,
            Some(12) => VideoCodec::Hevc,
            Some(13) => VideoCodec::Av1,
            _ => VideoCodec::Other,
        },
        codecs: json::text(o, "codecs"),
        url,
        backup_urls,
        bandwidth: json::long(o, "bandwidth"),
        width: json::int(o, "width").filter(|w| *w > 0),
        height: json::int(o, "height").filter(|h| *h > 0),
        frame_rate: json::text(o, "frameRate").or_else(|| json::text(o, "frame_rate")),
        segment_base: segment_base(o),
    })
}

/// 普通音轨、杜比音轨（`dolby.audio[]`）、无损音轨（`flac.audio`，是个对象）都收进来，`id` 区分。
fn audio_streams(dash: &JsonMap) -> Vec<AudioStream> {
    let mut all: Vec<&JsonMap> = Vec::new();
    if let Some(items) = json::arr(dash, "audio") {
        all.extend(items.iter().filter_map(|v| v.as_object()));
    }
    if let Some(items) = json::obj(dash, "dolby").and_then(|d| json::arr(d, "audio")) {
        all.extend(items.iter().filter_map(|v| v.as_object()));
    }
    if let Some(o) = json::obj(dash, "flac").and_then(|f| json::obj(f, "audio")) {
        all.push(o);
    }
    let mut streams: Vec<AudioStream> = all
        .into_iter()
        .filter_map(|o| {
            let (url, backup_urls) = urls(o)?;
            Some(AudioStream {
                id: json::int(o, "id")?,
                codecs: json::text(o, "codecs"),
                url,
                backup_urls,
                bandwidth: json::long(o, "bandwidth"),
                segment_base: segment_base(o),
            })
        })
        .collect();
    // Kotlin `sortedByDescending { it.bandwidth ?: 0 }`：稳定排序，缺码率的当 0。
    streams.sort_by(|a, b| b.bandwidth.unwrap_or(0).cmp(&a.bandwidth.unwrap_or(0)));
    streams
}

/// `durl[]` 里的一段整文件。
///
/// 和 Kotlin 的**唯一**差别：`url` 是空白串时 Kotlin 的 `order()` 会把它过滤掉、然后 `first()` 抛
/// `NoSuchElementException`；这里返回 `None`（丢掉这一条）。真实响应里没有这种形态，
/// 对拍样本也不造它——这是一条"不崩"的兜底，不是行为变更。
fn progressive(o: &JsonMap, quality: Option<i32>) -> Option<ProgressiveStream> {
    let primary = json::text(o, "url")?;
    let mut candidates = vec![primary];
    if let Some(items) = json::arr(o, "backup_url") {
        candidates.extend(
            items
                .iter()
                .filter_map(json::primitive_content)
                .filter(|s| !is_kotlin_blank(s)),
        );
    }
    let ordered = order(&candidates);
    let url = ordered.first()?.clone();
    Some(ProgressiveStream {
        quality,
        url,
        backup_urls: ordered[1..].to_vec(),
        size_bytes: json::long(o, "size"),
        width: None,
        height: None,
        codec: None,
        bandwidth: None,
    })
}

/// DASH 的 `segment_base`（两种驼峰都认）。
fn segment_base(o: &JsonMap) -> Option<SegmentBase> {
    let sb = json::obj(o, "segment_base").or_else(|| json::obj(o, "SegmentBase"))?;
    let initialization = json::text(sb, "initialization").or_else(|| json::text(sb, "Initialization"))?;
    let index_range = json::text(sb, "index_range").or_else(|| json::text(sb, "indexRange"))?;
    Some(SegmentBase {
        initialization,
        index_range,
    })
}

/// 主地址 + 备用地址。主地址取 `order()` 之后的第一个（不稳定的主机被换下去）。
///
/// 和 [`progressive`] 一样：全被过滤掉时返回 `None`（Kotlin 那边是 `first()` 抛异常）。
fn urls(o: &JsonMap) -> Option<(String, Vec<String>)> {
    let primary = json::text(o, "base_url").or_else(|| json::text(o, "baseUrl"))?;
    let mut candidates = vec![primary];
    if let Some(items) = json::arr(o, "backup_url").or_else(|| json::arr(o, "backupUrl")) {
        candidates.extend(
            items
                .iter()
                .filter_map(json::primitive_content)
                .filter(|s| !is_kotlin_blank(s)),
        );
    }
    let ordered = order(&candidates);
    let url = ordered.first()?.clone();
    Some((url, ordered[1..].to_vec()))
}

/// 同一路流的几个地址：**不稳定的主机往后排**，其余保持 B站 给的顺序（去重、丢空白）。
///
/// 这正是 Kotlin `BiliPlayParser.order` 的两步：先 `filter { it.isNotBlank() }.distinct()`（保序），
/// 再 `filterNot(isUnstable) + filter(isUnstable)`（稳定的先、不稳定的后，各自保序）。
pub fn order(urls: &[String]) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::with_capacity(urls.len());
    for url in urls {
        if !is_kotlin_blank(url) && !distinct.iter().any(|seen| seen == url) {
            distinct.push(url.clone());
        }
    }
    let (stable, unstable): (Vec<String>, Vec<String>) = distinct
        .into_iter()
        .partition(|url| !is_unstable(url));
    stable.into_iter().chain(unstable).collect()
}

/// 这个地址是不是"不稳定的 CDN 主机"（判断口径见文件头）。
///
/// 解析不出 URL（OkHttp `toHttpUrlOrNull()` 返回 null 的那种）算**稳定**——和 Kotlin 一样返回 false，
/// 别把一个奇怪的字符串当成"要往后排"。
pub fn is_unstable(url: &str) -> bool {
    let Some(parsed) = parse_url(url) else {
        return false;
    };
    let host = parsed.host.as_str();
    if host.contains("mcdn") || host.contains("pcdn") || host.contains("gotcha") {
        return true;
    }
    if is_ipv4(host) {
        return true;
    }
    parsed.port != if parsed.https { 443 } else { 80 }
}

/// OkHttp `toHttpUrlOrNull()` 的最小替身：只取 `scheme` / `host`（小写）/ `port` 三样。
///
/// 不做的：IDN（punycode）、百分号解码、IPv4 的八进制 / 十六进制形态、路径的规范化——B站 的播放地址
/// 全是 ASCII 的 `https://<host>[:port]/...`。判"非法 host"的字符集照 OkHttp 的 `HOST_INVALID_CHARS`：
/// 控制字符、空格、`" ' < > ( ) \ ^ ` { | }`、`0x7f`。
struct ParsedUrl {
    https: bool,
    host: String,
    port: u16,
}

fn parse_url(url: &str) -> Option<ParsedUrl> {
    // scheme：OkHttp 用的是大小写不敏感的 "http:" / "https:" 前缀，没有就 null。
    let (https, rest) = if starts_with_ignore_case(url, "https:") {
        (true, &url[6..])
    } else if starts_with_ignore_case(url, "http:") {
        (false, &url[5..])
    } else {
        return None;
    };
    // `http://host`；OkHttp 把 "//" 当可选前缀。
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let authority_end = rest
        .find(['/', '?', '#'])
        .unwrap_or(rest.len());
    let mut authority = &rest[..authority_end];
    if let Some(at) = authority.rfind('@') {
        authority = &authority[at + 1..];
    }
    let (host_part, port_part) = match authority.strip_prefix('[') {
        // IPv6 字面量：[::1]:8080
        Some(after) => {
            let close = after.find(']')?;
            let host = &authority[..close + 2];
            let tail = &after[close + 1..];
            (host, tail.strip_prefix(':'))
        }
        None => match authority.rfind(':') {
            Some(i) => (&authority[..i], Some(&authority[i + 1..])),
            None => (authority, None),
        },
    };
    let mut host = host_part.to_ascii_lowercase();
    if let Some(stripped) = host.strip_suffix('.') {
        // OkHttp 会去掉一个末尾的点；去掉之后还是点结尾就是空域名。
        if stripped.ends_with('.') {
            return None;
        }
        host = stripped.to_string();
    }
    if host.is_empty() || host.chars().any(is_invalid_host_char) {
        return None;
    }
    let default_port = if https { 443 } else { 80 };
    let port = match port_part {
        // 端口段是空的时候 OkHttp 用默认端口（"https://host:/x" 也是 443）。
        Some(p) if !p.is_empty() => {
            let port = p.parse::<u16>().ok()?;
            if port == 0 {
                return None;
            }
            port
        }
        _ => default_port,
    };
    Some(ParsedUrl { https, host, port })
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// OkHttp `HOST_INVALID_CHARS` 的那一批（非 ASCII 放行：那可能是 IDN）。
fn is_invalid_host_char(c: char) -> bool {
    match c {
        '\u{0}' | '\t' | '\n' | '\u{c}' | '\r' | ' ' | '"' | '\'' | '<' | '>' | '(' | ')' | '\\'
        | '^' | '`' | '{' | '|' | '}' | '\u{7f}' => true,
        _ => false,
    }
}

/// `\d{1,3}(\.\d{1,3}){3}` 的整串匹配（Kotlin 的 `Regex.matches`）。
fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

