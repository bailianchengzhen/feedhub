//! Reddit 的**播放取流**（`source-reddit/RedditVideo.kt` 的 Rust 版）＋一个够用的 **MPD（XML）解析器**。
//!
//! ## 结构
//!
//! 结构来自 Reddit 公开的 `.json`（`secure_media.reddit_video` / `media.reddit_video` /
//! `preview.reddit_video_preview`）和 `v.redd.it` 的 DASH 清单：
//! - `fallback_url` 是**只有画面**的 mp4（`DASH_720.mp4` 之类）；
//! - `hls_url` 是音画一体的 m3u8；
//! - `dash_url` 是 `DASHPlaylist.mpd`。
//!
//! 清单里的 `Representation` 带 `BaseURL` + `SegmentBase`（`indexRange` / `Initialization`），
//! 正好是 `VideoStream.segment_base` 要的形状，交给现有的中转 + `DashManifest` 播放。
//!
//! ## 回退链（和 Kotlin 一样写死）
//!
//! **HLS 也优先 DASH**：清单取得到就用 DASH；取不到（网络错误 / 结构变了 /
//! HTTP 非 2xx / body 空白）才退回 `hls_url` 的整段 m3u8，再不行退回 `fallback_url` 的无声 mp4。
//! 结果里如实标出用的是哪条（`video` 有内容 = DASH；`progressive` 里是 m3u8 或 mp4）。
//!
//! ## 为什么手写 XML
//!
//! 离线 cargo 缓存里**没有 XML crate**（`quick-xml` / `roxmltree` 都不在），而当前的规矩是
//! "不引入需要联网的新依赖"。所以 [`xml`] 是一个只认 MPD 用到的那一小撮语法的**极简解析器**：
//! 元素 / 属性 / 文本 / 注释 / CDATA / 自闭合 / XML 声明 / 字符引用；**不实现** DTD、实体定义、
//! 命名空间（`ns:tag` 当普通名字）、PI、条件段。边界写在 [`xml`] 模块的文件头。
//!
//! 口径上也和 Kotlin 的 `DocumentBuilderFactory` 对齐：**只看直接子元素**（Kotlin 的 `child` /
//! `childText` 也是直接子元素），`descendants` 是"所有后代里同名的最先那个"。

use super::RedditError;
use crate::play::{quality_of_size, AudioStream, PlayInfo, ProgressiveStream, Quality, SegmentBase, VideoCodec, VideoStream};

/// `partId`：整条帖子就是一个视频（Kotlin `RedditVideo.MAIN`）。
pub const MAIN: &str = "main";

/// 只有一路音轨时它的 id（Kotlin `RedditVideo.AUDIO_ID`）。
const AUDIO_ID: i32 = 1;

/// 从一条帖子（或它的 crosspost 原帖）里挑出来的 `reddit_video`（Kotlin `RedditVideo.Video`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub id: String,
    pub fallback_url: Option<String>,
    pub hls_url: Option<String>,
    pub dash_url: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_sec: Option<i32>,
    pub is_gif: bool,
    pub has_audio: bool,
}

/// 解析出来的 DASH 清单（Kotlin `RedditVideo.Mpd`）：时长 + 视频轨（按清晰度、码率降序）+ 音频轨。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mpd {
    pub duration_ms: Option<i64>,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
}

/// **只校验 `partId`**：Reddit 只有 `main`（Kotlin 那边不是 `main` 就抛 `ApiException("part")`）。
pub fn check_part(part_id: &str) -> Result<(), RedditError> {
    if part_id == MAIN {
        return Ok(());
    }
    Err(RedditError::Api {
        code: "part".to_string(),
        detail: format!("Reddit 的视频只有一段 main，没有 {part_id}"),
    })
}

/// 从一条帖子（或它的 crosspost 原帖）里挑出 `reddit_video`。
///
/// 字段名见 Octonaut `videoMedia` / redlib `Media::parse`：优先 `secure_media`，其次 `media`，
/// 最后 `preview.reddit_video_preview`。三个地址一个都没有的候选是**跳过**（不是"有视频但没地址"）。
pub fn video_of(post: &crate::json::JsonMap) -> Option<Video> {
    use crate::json;
    let mut candidates: Vec<&crate::json::JsonMap> = vec![post];
    if let Some(p) = json::arr(post, "crosspost_parent_list")
        .and_then(|a| a.first())
        .and_then(|v| v.as_object())
    {
        candidates.push(p);
    }
    for o in candidates {
        let rv = json::obj(o, "secure_media")
            .and_then(|m| json::obj(m, "reddit_video"))
            .or_else(|| json::obj(o, "media").and_then(|m| json::obj(m, "reddit_video")))
            .or_else(|| {
                json::obj(o, "preview").and_then(|m| json::obj(m, "reddit_video_preview"))
            });
        let Some(rv) = rv else { continue };
        let fallback = json::text(rv, "fallback_url");
        let hls = json::text(rv, "hls_url");
        let dash = json::text(rv, "dash_url");
        let video_id = dash
            .as_deref()
            .and_then(video_id_of)
            .or_else(|| fallback.as_deref().and_then(video_id_of))
            .or_else(|| json::text(o, "id"));
        let Some(id) = video_id else { continue };
        if fallback.is_none() && hls.is_none() && dash.is_none() {
            continue;
        }
        return Some(Video {
            id,
            fallback_url: fallback,
            hls_url: hls,
            dash_url: dash,
            width: json::int(rv, "width"),
            height: json::int(rv, "height"),
            duration_sec: json::int(rv, "duration"),
            is_gif: json::bool(rv, "is_gif") == Some(true),
            has_audio: json::bool(rv, "has_audio") != Some(false),
        });
    }
    None
}

/// 取视频 id：`https://v.redd.it/<id>/...` 的第一段路径（Kotlin `RedditVideo.videoIdOf`）。
fn video_id_of(url: &str) -> Option<String> {
    let after = url.split_once("//").map(|(_, r)| r).unwrap_or(url);
    let path = after.split_once('/').map(|(_, r)| r).unwrap_or("");
    let first = path.split('/').next().unwrap_or("");
    if crate::reddit::markdown::is_blank(first) {
        None
    } else {
        Some(first.to_string())
    }
}

/// 要取回哪份清单：优先响应里的 `dash_url`，没有就从 `fallback_url` / `hls_url` 的同目录拼
/// `DASHPlaylist.mpd`（Octonaut 也是这么拼 `HLSPlaylist.m3u8` 的）。
pub fn dash_url_of(video: &Video) -> Option<String> {
    if let Some(url) = &video.dash_url {
        return Some(url.clone());
    }
    let any = video.fallback_url.as_ref().or(video.hls_url.as_ref())?;
    let idx = any.rfind('/')?;
    // Kotlin：`if (idx <= "https://x".length) return null`
    if idx <= "https://x".len() {
        return None;
    }
    Some(format!("{}DASHPlaylist.mpd", &any[..idx + 1]))
}

/// 清单请求要带的请求头：只有 `Accept: */*`（Kotlin `fetchText`）。
///
/// 其余默认头由宿主客户端的拦截器补（`isRedditHost` 只认 reddit / redd.it 那几个域名，`v.redd.it`
/// 命中 `.redd.it`），顺序因此和 Kotlin 一致：`Accept` 先写，拦截器再补 UA / Accept-Language / Referer。
pub fn manifest_headers() -> Vec<(String, String)> {
    vec![("Accept".to_string(), "*/*".to_string())]
}

/// 解析 `DASHPlaylist.mpd`。[`mpd_url`] 用来把相对的 `BaseURL` 拼成绝对地址。
///
/// 结构不对（或没有视频轨）返回 `None`，调用方回退 HLS / fallback（Kotlin 也是 `runCatching{}` 吞掉）。
pub fn decode_mpd(xml_text: &str, mpd_url: &str) -> Option<Mpd> {
    let root = xml::parse(xml_text)?;
    if !root.name.eq_ignore_ascii_case("MPD") {
        return None;
    }
    let duration_ms = parse_iso_duration(&root.attr("mediaPresentationDuration"));
    let mut video: Vec<VideoStream> = Vec::new();
    let mut audio: Vec<AudioStream> = Vec::new();
    for adaptation in root.descendants("AdaptationSet") {
        let mime = {
            let m = adaptation.attr("mimeType");
            if m.is_empty() {
                adaptation.attr("contentType").to_string()
            } else {
                m.to_string()
            }
        };
        let audio_set = mime.to_ascii_lowercase().contains("audio");
        for rep in adaptation.descendants("Representation") {
            let Some(base) = rep.child_text("BaseURL").or_else(|| adaptation.child_text("BaseURL")) else {
                continue;
            };
            let url = resolve(mpd_url, base.trim());
            let sb = rep.child("SegmentBase").map(|el| SegmentBase {
                initialization: el
                    .child("Initialization")
                    .map(|i| i.attr("range").to_string())
                    .unwrap_or_default(),
                index_range: el.attr("indexRange").to_string(),
            });
            // 没 SegmentBase、或者两边缺一个 → 跳过（DashManifest 拼不出来）
            if let Some(sb) = &sb {
                if sb.index_range.is_empty() || sb.initialization.is_empty() {
                    continue;
                }
            }
            let codecs = {
                let c = rep.attr("codecs");
                if c.is_empty() {
                    None
                } else {
                    Some(c.to_string())
                }
            };
            let bandwidth = rep.attr("bandwidth").parse::<i64>().ok();
            let width = rep.attr("width").parse::<i32>().ok();
            let height = rep.attr("height").parse::<i32>().ok();
            if audio_set {
                let id = audio.len() as i32 + AUDIO_ID;
                audio.push(AudioStream {
                    id,
                    codecs,
                    url,
                    backup_urls: Vec::new(),
                    bandwidth,
                    segment_base: sb,
                });
            } else {
                let quality = quality_of_size(width, height).map(|q| q.id).unwrap_or(0);
                let frame_rate = {
                    let f = rep.attr("frameRate");
                    if f.is_empty() {
                        None
                    } else {
                        Some(f.to_string())
                    }
                };
                video.push(VideoStream {
                    quality,
                    codec: codec_of(codecs.as_deref()),
                    codecs,
                    url,
                    backup_urls: Vec::new(),
                    bandwidth,
                    width,
                    height,
                    frame_rate,
                    segment_base: sb,
                });
            }
        }
    }
    if video.is_empty() {
        return None;
    }
    video.sort_by(|a, b| b.quality.cmp(&a.quality).then((b.bandwidth.unwrap_or(0)).cmp(&a.bandwidth.unwrap_or(0))));
    Some(Mpd {
        duration_ms,
        video,
        audio,
    })
}

/// 组成播放信息（Kotlin `RedditVideo.play`）：有 DASH 就用它，否则退回 HLS / fallback mp4。
///
/// [`mpd`] 是**已经取回来并解析好**的清单（取清单本身在平台对象里，见
/// [`super::source::RedditHostedSource::play`]）；`None` = 那条回退链走到了 HLS 那一步。
pub fn play(video: &Video, mpd: Option<&Mpd>) -> Result<PlayInfo, RedditError> {
    let headers = manifest_headers();
    if let Some(mpd) = mpd.filter(|m| !m.video.is_empty()) {
        let mut sorted = mpd.video.clone();
        sorted.sort_by(|a, b| {
            b.quality
                .cmp(&a.quality)
                .then((b.bandwidth.unwrap_or(0)).cmp(&a.bandwidth.unwrap_or(0)))
        });
        let mut qualities: Vec<Quality> = Vec::new();
        for stream in &sorted {
            let q = stream.quality;
            if !qualities.iter().any(|x| x.id == q) {
                qualities.push(Quality {
                    id: q,
                    label: quality_label(stream.width, stream.height),
                    available: true,
                });
            }
        }
        return Ok(PlayInfo {
            duration_ms: mpd
                .duration_ms
                .or_else(|| video.duration_sec.map(|d| d as i64 * 1000)),
            quality: qualities.first().map(|q| q.id),
            qualities,
            video: sorted,
            audio: mpd.audio.clone(),
            headers,
            ..PlayInfo::default()
        });
    }
    let url = video
        .hls_url
        .clone()
        .or_else(|| video.fallback_url.clone())
        .ok_or_else(|| RedditError::Api {
            code: "no_video".to_string(),
            detail: "这条帖子没有可播放的地址（fallback_url / hls_url / dash_url 都没有）".to_string(),
        })?;
    let quality = quality_of_size(video.width, video.height);
    Ok(PlayInfo {
        duration_ms: video.duration_sec.map(|d| d as i64 * 1000),
        quality: quality.as_ref().map(|q| q.id),
        qualities: quality.clone().into_iter().collect(),
        progressive: vec![ProgressiveStream {
            quality: quality.map(|q| q.id),
            url,
            backup_urls: Vec::new(),
            size_bytes: None,
            width: video.width,
            height: video.height,
            // HLS 是音画一体；退回 fallback_url 时只有画面（服务端说 has_audio 也拿不到音轨）
            codec: None,
            bandwidth: None,
        }],
        headers,
        ..PlayInfo::default()
    })
}

fn quality_label(width: Option<i32>, height: Option<i32>) -> String {
    let q = quality_of_size(width, height);
    let size = match (width, height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => String::new(),
    };
    [q.map(|q| q.label), Some(size).filter(|s| !s.is_empty())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

fn codec_of(codecs: Option<&str>) -> VideoCodec {
    match codecs {
        None => VideoCodec::Other,
        Some(c) if c.starts_with("avc") || c.starts_with("avc1") => VideoCodec::Avc,
        Some(c) if c.starts_with("hev") || c.starts_with("hvc") => VideoCodec::Hevc,
        Some(c) if c.starts_with("av01") => VideoCodec::Av1,
        Some(_) => VideoCodec::Other,
    }
}

/// `PT1M30.5S` / `PT12S` / `PT1H2M3S` → 毫秒（Kotlin `RedditVideo.parseIsoDuration`）。
///
/// 手写扫描，语义照 Kotlin 那条正则：
/// `^P(?:(\d+)D)?T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+(?:\.\d+)?)S)?$`（`trim()` 之后整体匹配）。
pub fn parse_iso_duration(text: &str) -> Option<i64> {
    let s = text.trim();
    let rest = s.strip_prefix('P')?;
    let (days, rest) = take_number(rest, 'D');
    let rest = rest.strip_prefix('T')?;
    let (hours, rest) = take_number(rest, 'H');
    let (minutes, rest) = take_number(rest, 'M');
    let (seconds, rest) = take_decimal(rest, 'S');
    if !rest.is_empty() {
        return None;
    }
    let days = days.unwrap_or(0);
    let hours = hours.unwrap_or(0);
    let minutes = minutes.unwrap_or(0);
    let seconds = seconds.unwrap_or(0.0);
    Some(
        (((days * 24 + hours) * 60 + minutes) * 60) * 1000 + (seconds * 1000.0) as i64,
    )
}

/// 取一串数字 + 可选的结束字符（大小写敏感，照 Kotlin 的正则）。
fn take_number(input: &str, suffix: char) -> (Option<i64>, &str) {
    let digits: String = input.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return (None, input);
    }
    let rest = &input[digits.len()..];
    match rest.strip_prefix(suffix) {
        Some(after) => (digits.parse().ok(), after),
        // 没有这个后缀：不算命中，原样返回（让后面的步骤继续试）
        None => (None, input),
    }
}

/// 取一个十进制数（可带小数）+ 结束字符。
fn take_decimal(input: &str, suffix: char) -> (Option<f64>, &str) {
    let end = input
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let number = &input[..end];
    if number.is_empty() || number == "." {
        return (None, input);
    }
    let rest = &input[end..];
    match rest.strip_prefix(suffix) {
        Some(after) => (number.parse().ok(), after),
        None => (None, input),
    }
}

/// 相对地址按 [`base`] 拼成绝对地址（`BaseURL` 一般是 `DASH_720.mp4` 这样的同目录文件名）。
fn resolve(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix("//") {
        return format!("https://{rest}");
    }
    if let Some(rest) = path.strip_prefix('/') {
        let host = base
            .split_once("//")
            .map(|(_, r)| r)
            .unwrap_or(base)
            .split_once('/')
            .map(|(h, _)| h)
            .unwrap_or("");
        return format!("https://{host}/{rest}");
    }
    // `base.substringBeforeLast('/') + "/" + path`
    match base.rfind('/') {
        Some(idx) => format!("{}/{}", &base[..idx], path),
        None => path.to_string(),
    }
}

/// **够用的 XML 解析器**（只认 MPD 用到的那一小撮语法）。
///
/// 支持的：XML 声明、注释、CDATA、自闭合标签、属性（单 / 双引号）、文本、字符引用（`&lt;` 那五个 +
/// 数字引用）、`<![…]]>` 之外的 DOCTYPE 跳过（不解析实体定义）。
///
/// **不支持的**（MPD 里不会出现，遇到就跳过 / 当文本）：DTD 内部实体定义、命名空间前缀（`ns:tag` 当普通
/// 名字比较，所以 `cenc:pssh` 这种带前缀的元素名要按全名写）、处理指令（`<?…?>` 跳过）、
/// 条件段（`<![if …]>`）。**不做**任何规范化（不折空白、不改大小写）。
pub mod xml {
    /// 一个元素：名字（原样）+ 属性（原样，先出现的赢）+ 直接子元素 + 拼接后的文本。
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Element {
        pub name: String,
        pub attrs: Vec<(String, String)>,
        pub children: Vec<Element>,
        pub text: String,
    }

    impl Element {
        /// 属性值；没有就是 `""`（和 Kotlin `Element.getAttribute` 一样：**不 trim**）。
        pub fn attr(&self, name: &str) -> &str {
            self.attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        }

        /// 直接子元素里第一个叫 [`name`] 的。
        pub fn child(&self, name: &str) -> Option<&Element> {
            self.children.iter().find(|c| c.name == name)
        }

        /// 直接子元素的文本，`trim()` 之后非空才返回（Kotlin `childText`）。
        pub fn child_text(&self, name: &str) -> Option<&str> {
            let text = self.child(name)?.text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }

        /// 所有后代里名字等于 [`name`] 的元素（**不含自己**，顺序为先序）。
        ///
        /// Kotlin 的 `descendants` 是手写 walk：遇到同名就收下、**不再往里走**（所以嵌套同名的内层不算）。
        pub fn descendants(&self, name: &str) -> Vec<&Element> {
            let mut out = Vec::new();
            fn walk<'a>(node: &'a Element, name: &str, out: &mut Vec<&'a Element>) {
                for child in &node.children {
                    if child.name == name {
                        out.push(child);
                    } else {
                        walk(child, name, out);
                    }
                }
            }
            walk(self, name, &mut out);
            out
        }
    }

    /// 解析一份 XML，返回根元素。空文档 / 没有根元素 / 标签没闭合都是 `None`（调用方走回退链）。
    pub fn parse(input: &str) -> Option<Element> {
        let mut parser = Parser {
            input,
            pos: 0,
            stack: Vec::new(),
            root: None,
        };
        parser.run();
        parser.root
    }

    struct Parser<'a> {
        input: &'a str,
        pos: usize,
        stack: Vec<Element>,
        root: Option<Element>,
    }

    impl<'a> Parser<'a> {
        fn run(&mut self) {
            while self.pos < self.input.len() {
                let rest = &self.input[self.pos..];
                if rest.starts_with("<?") {
                    // XML 声明 / 处理指令：跳到 `?>`
                    match rest.find("?>") {
                        Some(i) => self.pos += i + 2,
                        None => return,
                    }
                } else if rest.starts_with("<!--") {
                    match rest.find("-->") {
                        Some(i) => self.pos += i + 3,
                        None => return,
                    }
                } else if rest.starts_with("<![CDATA[") {
                    let Some(end) = rest.find("]]>") else {
                        return;
                    };
                    let data = &rest[9..end];
                    if let Some(top) = self.stack.last_mut() {
                        top.text.push_str(data);
                    }
                    self.pos += end + 3;
                } else if rest.starts_with("<!") {
                    // DOCTYPE / 条件段：跳到下一个 `>`（不解析内部子集）
                    match rest.find('>') {
                        Some(i) => self.pos += i + 1,
                        None => return,
                    }
                } else if rest.starts_with("</") {
                    let Some(end) = rest.find('>') else { return };
                    let name = rest[2..end].trim();
                    self.close(name);
                    self.pos += end + 1;
                } else if rest.starts_with('<') {
                    let Some(end) = tag_end(rest) else { return };
                    let raw = &rest[1..end];
                    self.pos += end + 1;
                    let self_closing = raw.ends_with('/');
                    let raw = raw.trim_end_matches('/');
                    let (name, attrs) = parse_tag(raw);
                    if name.is_empty() {
                        continue;
                    }
                    let element = Element {
                        name,
                        attrs,
                        children: Vec::new(),
                        text: String::new(),
                    };
                    if self_closing {
                        self.push(element);
                    } else {
                        self.stack.push(element);
                    }
                } else {
                    // 文本：到下一个 `<`
                    let end = rest.find('<').unwrap_or(rest.len());
                    let decoded = decode_entities(&rest[..end]);
                    if let Some(top) = self.stack.last_mut() {
                        top.text.push_str(&decoded);
                    }
                    self.pos += end;
                }
            }
            // 没闭合的标签：从里到外收
            while !self.stack.is_empty() {
                let element = self.stack.pop();
                if let Some(element) = element {
                    self.push(element);
                }
            }
        }

        fn close(&mut self, name: &str) {
            // 找最近的同名元素；找到就把它（连同上面没闭合的）收掉
            let Some(index) = self.stack.iter().rposition(|e| e.name == name) else {
                return;
            };
            loop {
                let Some(element) = self.stack.pop() else { return };
                let matched = element.name == name;
                self.push(element);
                if matched {
                    break;
                }
                if self.stack.len() <= index {
                    break;
                }
            }
        }

        fn push(&mut self, element: Element) {
            match self.stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => {
                    if self.root.is_none() {
                        self.root = Some(element);
                    }
                }
            }
        }
    }

    /// 标签结束的 `>` 位置（属性值里的 `>` 不算，引号里的内容整体跳过）。
    fn tag_end(rest: &str) -> Option<usize> {
        let mut quote: Option<char> = None;
        for (i, c) in rest.char_indices() {
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    '"' | '\'' => quote = Some(c),
                    '>' => return Some(i),
                    _ => {}
                },
            }
        }
        None
    }

    /// `name attr="v" attr2='v2'` → (名字, 有序属性)。
    fn parse_tag(raw: &str) -> (String, Vec<(String, String)>) {
        let mut chars = raw.char_indices().peekable();
        let name_end = raw
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(raw.len());
        let name = raw[..name_end].to_string();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut pos = name_end;
        while pos < raw.len() {
            // 跳过空白
            let rest = &raw[pos..];
            let trimmed = rest.trim_start();
            pos += rest.len() - trimmed.len();
            if pos >= raw.len() {
                break;
            }
            // 属性名：到 `=` 或空白
            let name_len = raw[pos..]
                .char_indices()
                .find(|(_, c)| *c == '=' || c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(raw.len() - pos);
            if name_len == 0 {
                pos += 1;
                continue;
            }
            let attr_name = raw[pos..pos + name_len].to_string();
            pos += name_len;
            // 跳过空白 + `=`
            let rest = &raw[pos..];
            let trimmed = rest.trim_start();
            pos += rest.len() - trimmed.len();
            let mut value = String::new();
            if raw[pos..].starts_with('=') {
                pos += 1;
                let rest = &raw[pos..];
                let trimmed = rest.trim_start();
                pos += rest.len() - trimmed.len();
                if pos < raw.len() {
                    let quote = raw[pos..].chars().next().unwrap_or(' ');
                    if quote == '"' || quote == '\'' {
                        pos += quote.len_utf8();
                        let end = raw[pos..].find(quote).map(|i| pos + i).unwrap_or(raw.len());
                        value = decode_entities(&raw[pos..end]);
                        pos = (end + quote.len_utf8()).min(raw.len());
                    } else {
                        let end = raw[pos..]
                            .char_indices()
                            .find(|(_, c)| c.is_whitespace())
                            .map(|(i, _)| pos + i)
                            .unwrap_or(raw.len());
                        value = decode_entities(&raw[pos..end]);
                        pos = end;
                    }
                }
            }
            if !attrs.iter().any(|(k, _)| *k == attr_name) {
                attrs.push((attr_name, value));
            }
            // `chars` 只用来保持借用检查器安静（peekable 没用上）
            let _ = chars.peek();
        }
        (name, attrs)
    }

    /// 字符引用：`&lt;` `&gt;` `&amp;` `&quot;` `&apos;` + `&#N;` / `&#xN;`；认不出的原样保留。
    pub fn decode_entities(input: &str) -> String {
        if !input.contains('&') {
            return input.to_string();
        }
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(idx) = rest.find('&') {
            out.push_str(&rest[..idx]);
            let after = &rest[idx + 1..];
            let Some(semi) = after.find(';') else {
                out.push('&');
                rest = after;
                continue;
            };
            let entity = &after[..semi];
            match entity {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ => match numeric_entity(entity) {
                    Some(c) => out.push(c),
                    None => {
                        // 认不出：原样（连同 `&` 和 `;`）
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                    }
                },
            }
            rest = &after[semi + 1..];
        }
        out.push_str(rest);
        out
    }

    fn numeric_entity(entity: &str) -> Option<char> {
        let number = entity.strip_prefix('#')?;
        let value = if let Some(hex) = number.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            number.parse::<u32>().ok()?
        };
        char::from_u32(value)
    }
}

