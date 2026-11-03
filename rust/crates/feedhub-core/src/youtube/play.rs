//! YouTube 的**播放取流**：`player` 响应里的 `adaptiveFormats` / `formats` → [`PlayInfo`]，
//! 外加 `signatureCipher` / 节流 `n` 的拼接（**JS 求解本身在宿主**，见 [`crate::host::ScriptHost`]）。
//!
//! 逐条对照 `source-youtube/YoutubeParser.playInfo` / `isMp4` / `qualityId` / `qualityLabel` / `codecOf`
//! 和 `YoutubeCipher.decode` / `needsSolving`（两个文件都在 Kotlin 那边，这一份是它们的合体）：
//!
//! - **只收 mp4 容器**：安卓的 DASH 播放器是我们自己拼 MPD（固定写
//!   `video/mp4` / `audio/mp4`），YouTube 同时给 `video/webm`（VP9 / AV1）和 `audio/webm`（Opus），
//!   混进来会拼出 mime 对不上的清单；
//! - 视频轨要有 `url` + `width` + `height` + `initRange` + `indexRange`，且没 DRM / 没 cipher / 不是 OTF；
//!   音频轨同条件但 `width == null`；整段（`formats`）只要 `url` + 没 cipher / DRM + mp4；
//! - 清晰度沿用 B站 的数字（120 = 4K、112 = 2K、80 = 1080P、64 = 720P、32 = 480P、16 = 360P），
//!   这样设置里那个"默认清晰度"对 YouTube 也有效；
//! - `signatureCipher` 的 `s` 要**先 URL 解码**再当挑战；答案再按 `URLEncoder` 编码回去拼在 `sp` 参数上；
//! - `n` 不解会被限速（很快 403 / 可用区间很短），挑战是 url 里的**原值**，答案编码回去替换掉它。
//!
//! **一次求解、整批过边界**：先把要解的 `s` / `n` 全扫出来（各自去重、保持首次出现顺序），
//! 按 `sig` 在前 `n` 在后交给宿主（和 Kotlin `YoutubeCipher.decode` 的 `requests` 顺序一致），
//! 拿回答案后一次拼完所有 URL——**不要一条格式过一次边界**。

use std::collections::HashMap;

use serde_json::Value;

use crate::json::{self, JsonMap};
use crate::play::{AudioStream, PlayInfo, ProgressiveStream, Quality, SegmentBase, VideoCodec, VideoStream};

use super::YoutubeError;

/// YouTube 只有一条流：播放 / 弹幕 / `VideoPart.id` 一律用它（Kotlin `YoutubeSource.MAIN_PART`）。
pub const MAIN_PART: &str = "main";

/// 容器：只收 mp4（理由见模块说明）。
const MP4_VIDEO: &str = "video/mp4";
const MP4_AUDIO: &str = "audio/mp4";

/// 一次交给宿主的求解请求（`kind` 是 `"sig"` 或 `"n"`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveRequest {
    pub kind: &'static str,
    pub challenges: Vec<String>,
}

impl SolveRequest {
    pub fn is_empty(&self) -> bool {
        self.challenges.is_empty()
    }
}

// ===========================================================================
// 格式扫描（`signatureCipher` 的拼接用）
// ===========================================================================

/// 一条 `signatureCipher`（Kotlin `YoutubeCipher.Cipher`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cipher {
    /// 挑战原文（**已经 URL 解码**；`None` = cipher 里没有 `s`）。
    s: Option<String>,
    /// 签名参数名（默认 `signature`）。
    sp: String,
    /// `url` 参数（已经 URL 解码）。
    url: String,
}

/// 扫描出来的一条格式（只有拼 URL 用得上的字段）。
#[derive(Debug, Clone)]
struct Scanned {
    key: &'static str,
    index: usize,
    cipher: Option<Cipher>,
    /// `cipher?.url ?: json.text("url")`
    base_url: Option<String>,
}

/// `streamingData` 里的两条数组（`adaptiveFormats` 在前，`formats` 在后）。
const FORMAT_KEYS: [&str; 2] = ["adaptiveFormats", "formats"];

fn format_values<'a>(root: &'a JsonMap, key: &str) -> Vec<&'a Value> {
    json::obj(root, "streamingData")
        .and_then(|s| json::arr(s, key))
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

// ===========================================================================
// Java 的 URL 编解码（`URLDecoder` / `URLEncoder`，和 OkHttp 的表单编码**不是**一回事）
// ===========================================================================

/// `java.net.URLDecoder.decode(s, "UTF-8")`：`+` → 空格、`%XX` → 字节；坏转义返回 `None`
/// （Kotlin 那边会抛 `IllegalArgumentException`，调用处用 `runCatching` 兜成原值 / null）。
fn url_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hi = hex_of(*bytes.get(i + 1)?)?;
                let lo = hex_of(*bytes.get(i + 2)?)?;
                out.push(hi * 16 + lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex_of(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `java.net.URLEncoder.encode(s, "UTF-8")`：字母数字和 `- . _ *` 原样，**空格是 `+`**，其余 `%XX`（大写）。
///
/// 和 `hosted::query_encode`（OkHttp 的 `addQueryParameter`，空格 `%20`）**不是一套**，别换。
/// 贴吧那边有一份一模一样的（`tieba::source::form_encode`），先各留一份。
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ===========================================================================
// `signatureCipher` / `n`
// ===========================================================================

/// `query.split('&')` + `indexOf('=')`（Kotlin `YoutubeCipher.paramsOf`：**key 不参与解码**，
/// 只有 `s` / `url` 各自再解一次；`i <= 0` 的段（空段、没有 `=`、`=x`）丢掉）。
fn params_of(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in query.split('&') {
        if let Some(at) = part.find('=') {
            if at > 0 {
                map.insert(part[..at].to_string(), part[at + 1..].to_string());
            }
        }
    }
    map
}

/// `parseCipher`：`url` 解不出来就算没有 cipher（`None`），`s` 解不出来就用原值。
fn parse_cipher(raw: &str) -> Option<Cipher> {
    let params = params_of(raw);
    let url = url_decode(params.get("url")?)?;
    let s = params
        .get("s")
        .map(|v| url_decode(v).unwrap_or_else(|| v.clone()));
    Some(Cipher {
        s,
        sp: params.get("sp").cloned().unwrap_or_else(|| "signature".to_string()),
        url,
    })
}

/// 一条格式里的 `n` 挑战（`url.substringAfter('?')` 之后 `n` 的值，空串当没有）。
fn n_of(url: Option<&str>) -> Option<String> {
    let query = url?.split_once('?').map(|(_, q)| q).unwrap_or("");
    params_of(query)
        .get("n")
        .filter(|v| !v.is_empty())
        .cloned()
}

fn scan(root: &JsonMap) -> Vec<Scanned> {
    let mut out = Vec::new();
    for key in FORMAT_KEYS {
        for (index, json) in format_values(root, key).into_iter().enumerate() {
            let Some(json) = json.as_object() else { continue };
            let raw = json::text(json, "signatureCipher").or_else(|| json::text(json, "cipher"));
            let cipher = raw.as_deref().and_then(parse_cipher);
            let base_url = cipher
                .as_ref()
                .map(|c| c.url.clone())
                .or_else(|| json::text(json, "url"));
            out.push(Scanned {
                key,
                index,
                cipher,
                base_url,
            });
        }
    }
    out
}

/// 这个响应里有没有要跑 JS 的东西（`needsSolving`）：有 `signatureCipher`，或者直链里带 `n`。
pub fn needs_solving(root: &JsonMap) -> bool {
    scan(root)
        .iter()
        .any(|f| f.cipher.is_some() || n_of(f.base_url.as_deref()).is_some())
}

/// 要把哪些挑战交给宿主（**`sig` 在前、`n` 在后**，各自去重并保持首次出现顺序；
/// 某一组为空就整组不出现——和 Kotlin 的 `buildList` 一样）。
pub fn solve_requests(root: &JsonMap) -> Vec<SolveRequest> {
    let formats = scan(root);
    let mut sig: Vec<String> = Vec::new();
    let mut n: Vec<String> = Vec::new();
    for f in &formats {
        if let Some(s) = f.cipher.as_ref().and_then(|c| c.s.as_ref()) {
            if !s.is_empty() && !sig.iter().any(|x| x == s) {
                sig.push(s.clone());
            }
        }
    }
    for f in &formats {
        if let Some(value) = n_of(f.base_url.as_deref()) {
            if !n.iter().any(|x| *x == value) {
                n.push(value);
            }
        }
    }
    let mut out = Vec::new();
    if !sig.is_empty() {
        out.push(SolveRequest {
            kind: "sig",
            challenges: sig,
        });
    }
    if !n.is_empty() {
        out.push(SolveRequest {
            kind: "n",
            challenges: n,
        });
    }
    out
}

/// 把答案拼回所有格式的 `url`（Kotlin `YoutubeCipher.decode` 的后半段）：
/// `cipher` 的格式变成 `url&sp=sig`（再换掉里面的 `n`）、删掉 `signatureCipher` / `cipher`；
/// 直链的格式只换 `n`。**解不出答案的 cipher 格式原样留着**（和 Kotlin 一样：`return@associateWith null`）。
pub fn apply_answers(root: &mut JsonMap, answers: &HashMap<String, String>) {
    let formats = scan(root);
    let mut updates: Vec<(&'static str, usize, String)> = Vec::new();
    for f in &formats {
        let with_sig = match (&f.cipher, &f.base_url) {
            (None, Some(base)) => Some(base.clone()),
            (Some(_), None) => None,
            (Some(cipher), Some(base)) => {
                let challenge = cipher.s.clone().unwrap_or_default();
                match answers.get(&challenge) {
                    Some(sig) => Some(append_param(base, &cipher.sp, sig)),
                    // 没解出来：这条格式保持原样（不删 cipher、不改 url）
                    None => None,
                }
            }
            (None, None) => None,
        };
        if let Some(url) = with_sig.map(|u| with_solved_n(&u, answers)) {
            updates.push((f.key, f.index, url));
        }
    }
    let Some(streaming) = root.get_mut("streamingData").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, index, url) in updates {
        let Some(array) = streaming.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let Some(Value::Object(object)) = array.get_mut(index) else {
            continue;
        };
        object.remove("signatureCipher");
        object.remove("cipher");
        object.insert("url".to_string(), Value::String(url));
    }
}

/// `url&sp=sig`（`?` 还是 `&` 看 url 里有没有 `?`）。
fn append_param(url: &str, name: &str, value: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{name}={}", url_encode(value))
}

/// 把 URL 里 `n` 参数的值换成解出来的（没有 `n` 或没解出来就原样返回）。
///
/// Kotlin 用正则 `([?&]n=)([^&]*)` 全量替换（`replace` 会替换**每一处**），这里照做。
fn with_solved_n(url: &str, answers: &HashMap<String, String>) -> String {
    if !url.contains("?n=") && !url.contains("&n=") {
        return url.to_string();
    }
    let bytes = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let marker = i == 0 || bytes[i - 1] == b'?' || bytes[i - 1] == b'&';
        if marker && bytes[i] == b'n' && bytes.get(i + 1) == Some(&b'=') {
            // 挑战 = 到下一个 `&` 之前的那一段（原值，不解码）
            let start = i + 2;
            let end = bytes[start..]
                .iter()
                .position(|b| *b == b'&')
                .map(|p| start + p)
                .unwrap_or(bytes.len());
            let challenge = &url[start..end];
            let answer = answers.get(challenge).map(String::as_str).unwrap_or(challenge);
            out.push_str(&url[i..start]);
            out.push_str(&url_encode(answer));
            i = end;
            continue;
        }
        // 按字符推进（非 ASCII 的字节直接照抄）
        let ch = url[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ===========================================================================
// 流挑选（`YoutubeParser.playInfo`）
// ===========================================================================

/// 一条格式（播放只用得上这些字段）。
#[derive(Debug, Clone)]
struct Parsed {
    itag: i32,
    url: Option<String>,
    mime_type: Option<String>,
    codecs: Option<String>,
    bitrate: Option<i64>,
    average_bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    fps: Option<i32>,
    content_length: Option<i64>,
    initialization: Option<String>,
    index_range: Option<String>,
    cipher: bool,
    drm: bool,
    otf: bool,
}

fn long_of(el: Option<&Value>, key: &str) -> Option<i64> {
    el.and_then(Value::as_object).and_then(|m| json::long(m, key))
}

/// `{start: 0, end: 700}` → `"0-700"`（两个都得有，缺一个就是 `None`）。
fn range_of(el: Option<&Value>) -> Option<String> {
    let start = long_of(el, "start")?;
    let end = long_of(el, "end")?;
    Some(format!("{start}-{end}"))
}

/// `YoutubeParser.format`：没有 `itag` 的格式直接跳过。
fn parse_format(element: &Value) -> Option<Parsed> {
    let object = element.as_object()?;
    let itag = json::int(object, "itag")?;
    let mime_type = json::text(object, "mimeType");
    Some(Parsed {
        itag,
        url: json::text(object, "url"),
        codecs: codecs_of(mime_type.as_deref(), itag),
        bitrate: json::long(object, "bitrate"),
        average_bitrate: json::long(object, "averageBitrate"),
        width: json::int(object, "width"),
        height: json::int(object, "height"),
        // `r.int("fps") ?: r.text("fps")?.toDoubleOrNull()?.toInt()`
        fps: json::int(object, "fps").or_else(|| {
            json::text(object, "fps")
                .and_then(|t| t.parse::<f64>().ok())
                .map(|f| f as i32)
        }),
        content_length: json::long(object, "contentLength"),
        initialization: range_of(object.get("initRange")),
        index_range: range_of(object.get("indexRange")),
        cipher: json::text(object, "signatureCipher").is_some()
            || json::text(object, "cipher").is_some(),
        drm: json::obj(object, "drmFamilies").is_some(),
        otf: json::text(object, "type").as_deref() == Some("FORMAT_STREAM_TYPE_OTF"),
        mime_type,
    })
}

/// codec 字符串：优先 `mimeType` 里的 `codecs="…"`；没有就按 itag 补（音频的 itag 140/141 = AAC、
/// 249/250/251 = Opus，`StreamSelection.pickAudio` 靠它优先挑 AAC）。
fn codecs_of(mime: Option<&str>, itag: i32) -> Option<String> {
    if let Some(found) = extract_quoted(mime.unwrap_or(""), "codecs=\"") {
        return Some(found);
    }
    match itag {
        140 | 141 => Some("mp4a.40.2".to_string()),
        249 | 250 | 251 => Some("opus".to_string()),
        _ => None,
    }
}

/// `Regex("codecs=\"([^\"]+)\"")` 的第一个捕获组。
fn extract_quoted(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// 编码 → 播放器认的四大类（VP9 归 `Other`，解码器照 codecs 字符串选）。
pub fn codec_of(codecs: Option<&str>) -> VideoCodec {
    let Some(codecs) = codecs else {
        return VideoCodec::Other;
    };
    if codecs.starts_with("avc") || codecs.starts_with("mp4a") {
        if codecs.starts_with("mp4a") {
            VideoCodec::Other
        } else {
            VideoCodec::Avc
        }
    } else if codecs.starts_with("hev") || codecs.starts_with("hvc") {
        VideoCodec::Hevc
    } else if codecs.starts_with("av01") {
        VideoCodec::Av1
    } else {
        VideoCodec::Other
    }
}

/// `mimeType` 是不是 **mp4 容器**（`video/mp4` / `audio/mp4`，带不带 `; codecs="…"` 都算）。
/// 没写 `mimeType` 的当 mp4（InnerTube 一直带这个字段，真缺了按老行为"全部收"比"一条流都没有"安全）。
pub fn is_mp4(mime_type: Option<&str>) -> bool {
    let Some(mime) = mime_type else {
        return true;
    };
    let container = mime.split(';').next().unwrap_or("").trim().to_lowercase();
    container == MP4_VIDEO || container == MP4_AUDIO
}

/// 清晰度 id（`YoutubeParser.qualityId`）：**按 height，且多一档 2K（112）**。
pub fn quality_id(height: Option<i32>) -> Option<i32> {
    let height = height?;
    if height <= 0 {
        return None;
    }
    Some(if height >= 2000 {
        120
    } else if height >= 1400 {
        112
    } else if height >= 1000 {
        80
    } else if height >= 700 {
        64
    } else if height >= 460 {
        32
    } else {
        16
    })
}

/// 清晰度标签（`YoutubeParser.qualityLabel`）：高帧率的补一句" 60帧"。
pub fn quality_label(height: Option<i32>, fps: Option<i32>) -> String {
    let base = match quality_id(height) {
        Some(120) => "4K",
        Some(112) => "2K",
        Some(80) => "1080P",
        Some(64) => "720P",
        Some(32) => "480P",
        _ => "360P",
    };
    match fps {
        Some(fps) if fps > 30 => format!("{base} {fps}帧"),
        _ => base.to_string(),
    }
}

/// `piece.copy(available = …)`：同一档可能列两次，`distinctBy { it.id }` 只留第一个。
fn push_quality(out: &mut Vec<Quality>, quality: Quality) {
    if !out.iter().any(|q| q.id == quality.id) {
        out.push(quality);
    }
}

/// `player` 响应 → [`PlayInfo`]（`YoutubeParser.playInfo`）。
///
/// `requested` 只用来标"这次选的是哪一档"（真正的挑选在 `StreamSelection`）：它在可用档位里就照用，
/// 否则用最高的一档。`headers` 是空的——拉流要带的头由平台 OkHttp 客户端加。
pub fn play_info(root: &JsonMap, requested: Option<i32>) -> Result<PlayInfo, YoutubeError> {
    let adaptive: Vec<Parsed> = format_values(root, "adaptiveFormats")
        .iter()
        .filter_map(|v| parse_format(v))
        .collect();
    let muxed: Vec<Parsed> = format_values(root, "formats")
        .iter()
        .filter_map(|v| parse_format(v))
        .collect();

    let usable = |f: &Parsed, audio: bool| -> bool {
        f.url.is_some()
            && if audio { f.width.is_none() } else { f.width.is_some() && f.height.is_some() }
            && f.initialization.is_some()
            && f.index_range.is_some()
            && !f.cipher
            && !f.drm
            && !f.otf
            && is_mp4(f.mime_type.as_deref())
    };
    let dash_videos: Vec<&Parsed> = adaptive.iter().filter(|f| usable(f, false)).collect();
    let dash_audios: Vec<&Parsed> = adaptive.iter().filter(|f| usable(f, true)).collect();
    let progressive_sources: Vec<&Parsed> = muxed
        .iter()
        .filter(|f| {
            f.url.is_some() && !f.cipher && !f.drm && is_mp4(f.mime_type.as_deref())
        })
        .collect();

    let videos: Vec<VideoStream> = dash_videos
        .iter()
        .map(|f| VideoStream {
            quality: quality_id(f.height).unwrap_or(0),
            codec: codec_of(f.codecs.as_deref()),
            codecs: f.codecs.clone(),
            url: f.url.clone().unwrap_or_default(),
            backup_urls: Vec::new(),
            bandwidth: f.bitrate.or(f.average_bitrate),
            width: f.width,
            height: f.height,
            frame_rate: f.fps.map(|v| v.to_string()),
            segment_base: Some(SegmentBase {
                initialization: f.initialization.clone().unwrap_or_default(),
                index_range: f.index_range.clone().unwrap_or_default(),
            }),
        })
        .collect();

    let audio: Vec<AudioStream> = dash_audios
        .iter()
        .map(|f| AudioStream {
            id: f.itag,
            codecs: f.codecs.clone(),
            url: f.url.clone().unwrap_or_default(),
            backup_urls: Vec::new(),
            bandwidth: f.bitrate.or(f.average_bitrate),
            segment_base: Some(SegmentBase {
                initialization: f.initialization.clone().unwrap_or_default(),
                index_range: f.index_range.clone().unwrap_or_default(),
            }),
        })
        .collect();

    let progressive: Vec<ProgressiveStream> = progressive_sources
        .iter()
        .map(|f| ProgressiveStream {
            quality: crate::play::quality_of_size(f.width, f.height).map(|q| q.id),
            url: f.url.clone().unwrap_or_default(),
            backup_urls: Vec::new(),
            size_bytes: f.content_length,
            width: f.width,
            height: f.height,
            codec: Some(codec_of(f.codecs.as_deref())),
            bandwidth: f.bitrate.or(f.average_bitrate),
        })
        .collect();

    // `heights.distinct().sortedDescending()`：先按原顺序去重，再降序。
    let mut heights: Vec<i32> = Vec::new();
    for f in &dash_videos {
        if let Some(height) = f.height {
            if !heights.contains(&height) {
                heights.push(height);
            }
        }
    }
    heights.sort_by(|a, b| b.cmp(a));
    let mut qualities: Vec<Quality> = Vec::new();
    for height in heights {
        if let Some(id) = quality_id(Some(height)) {
            let fps = dash_videos
                .iter()
                .find(|f| f.height == Some(height))
                .and_then(|f| f.fps);
            push_quality(
                &mut qualities,
                Quality {
                    id,
                    label: quality_label(Some(height), fps),
                    available: true,
                },
            );
        }
    }

    let available: Vec<i32> = {
        let mut out: Vec<i32> = Vec::new();
        for stream in &videos {
            if !out.contains(&stream.quality) {
                out.push(stream.quality);
            }
        }
        out
    };
    let current = requested
        .filter(|q| available.contains(q))
        .or_else(|| available.iter().copied().max());

    let duration_sec = json::obj(root, "videoDetails")
        .and_then(|details| json::long(details, "lengthSeconds"));
    let reason = json::obj(root, "playabilityStatus")
        .and_then(|ps| json::text(ps, "reason"));

    // `cipherOnly`：一条能放的流都没有、格式里全是 cipher、而且平台没给 reason。
    let cipher_only = videos.is_empty()
        && progressive.is_empty()
        && adaptive.iter().any(|f| f.cipher)
        && reason.as_deref().map(|r| r.trim().is_empty()).unwrap_or(true);

    let info = PlayInfo {
        duration_ms: duration_sec.map(|sec| sec * 1000),
        quality: current,
        qualities: qualities
            .into_iter()
            .map(|q| Quality {
                available: available.contains(&q.id),
                ..q
            })
            .collect(),
        video: videos,
        audio,
        progressive,
        headers: Vec::new(),
    };
    if cipher_only {
        return Err(YoutubeError::Parse {
            detail: "这个视频只给了 signatureCipher（要跑 JS 解 n/sig），第二阶段才会支持".to_string(),
        });
    }
    Ok(info)
}

