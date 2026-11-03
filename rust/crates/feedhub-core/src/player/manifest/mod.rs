//! **清单生成与改写**（`app/.../video/DashManifest.kt` + `app/desktopMain/.../video/ManifestRewrite.kt`）。
//!
//! 三件事，**一件一个入口、一次一整份**：
//!
//! 1. [`dash_manifest`]：把"视频轨 + 音频轨"（各是一个带 sidx 的分片 mp4）拼成一份只含 `SegmentBase` 的
//!    静态 MPD，交给播放器的 dashdemux。为什么不用两路 souphttpsrc + qtdemux：分片 mp4 在推模式下 qtdemux
//!    不支持跳转（"ignoring seek in push mode"），用 queue2 环形缓冲凑拉模式又要把中间每个 moof 读一遍
//!    （跳到 400 秒要 16 秒）；dashdemux 靠 sidx 直接定位，跳转不到 1 秒；
//! 2. [`rewrite_dash`]：把清单里**所有指向远端的分片地址**换成本机中转地址（[`crate::player::relay`]）。
//!    覆盖 `BaseURL`（任何一层）、`SegmentTemplate` 的 `media` / `initialization` / `index` /
//!    `bitstreamSwitching`、`SegmentList` 的 `SegmentURL@media` / `@index`、`Initialization@sourceURL`；
//!    相对地址按清单自己的地址和**逐层继承**的 `BaseURL` 解析；
//! 3. [`rewrite_hls`]：m3u8 里的分片 URI、子清单、`#EXT-X-KEY` / `#EXT-X-MAP` 这些带 `URI="…"` 的标签
//!    都换成中转地址。
//!
//! ## XML 怎么处理
//!
//! 离线 cargo 缓存里**没有 XML crate**（`quick-xml` / `roxmltree` 都不在），规矩是"不引入需要联网的新依赖"。
//! 所以复用 `crate::reddit::play::xml` 那个**够用的解析器**（元素 / 属性 / 文本 / 注释 / CDATA / 自闭合 /
//! XML 声明 / 字符引用），**不写第三份**。
//!
//! 和 Kotlin 的 `DocumentBuilderFactory` + `Transformer` 的口径差（都写了测试盯着）：
//! - Kotlin 序列化时**保留原来的缩进文本节点**，只是把属性值换掉；这里走的是"**只在原文上替换属性值 /
//!   文本**"的路子（[`rewrite_dash`] 自己扫标签、原地改值），所以空白、注释、属性顺序、`<?xml …?>`
//!   声明**一个字节都不动**——比走 DOM 再序列化更接近 Kotlin 的输出，也不会把 `ns:` 前缀改掉；
//! - 不处理的：`Location`（清单更新地址，我们的清单是静态的）、`EventStream` / `EssentialProperty` 里
//!   那种"内容里有 URL"的扩展——现在没有平台会走到。
//!
//! ## 为什么改写是"原地替换"而不是"解析 → 改树 → 打印"
//!
//! 播放器（dashdemux / hlsdemux）对清单里别的字段一个字节都不该变（`codecs` / `frameRate` 的写法、
//! 属性的先后、注释都有用），而这些字段的书写方式平台随时会改。原地替换只碰"我们真的要改的那几个值"，
//! 其余原样——这是最小改动面。

use crate::play::{AudioStream, VideoStream};
use crate::player::relay::RelayUrls;

// ===========================================================================
// 1. 拼 MPD（Kotlin `DashManifest`）
// ===========================================================================

/// DASH 里一条轨的描述（Kotlin `DashTrack`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DashTrack {
    /// `SegmentBase` 的 `initialization`（字节范围，如 `0-831`）。
    pub initialization: String,
    /// `SegmentBase` 的 `indexRange`。
    pub index_range: String,
    pub codecs: Option<String>,
    pub bandwidth: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<String>,
}

impl DashTrack {
    /// 从一条 `VideoStream` 的 `segment_base` 取（没有 `segment_base` 就是 `None`）。
    pub fn from_video(stream: &VideoStream) -> Option<Self> {
        let sb = stream.segment_base.as_ref()?;
        Some(Self {
            initialization: sb.initialization.clone(),
            index_range: sb.index_range.clone(),
            codecs: stream.codecs.clone(),
            bandwidth: stream.bandwidth,
            width: stream.width,
            height: stream.height,
            frame_rate: stream.frame_rate.clone(),
        })
    }

    /// 从一条 `AudioStream` 的 `segment_base` 取（没有就是 `None`）。
    pub fn from_audio(stream: &AudioStream) -> Option<Self> {
        let sb = stream.segment_base.as_ref()?;
        Some(Self {
            initialization: sb.initialization.clone(),
            index_range: sb.index_range.clone(),
            codecs: stream.codecs.clone(),
            bandwidth: stream.bandwidth,
            width: None,
            height: None,
            frame_rate: None,
        })
    }
}

/// 拼一份只含 `SegmentBase` 的静态 MPD（Kotlin `DashManifest.build`，**逐字节一致**）。
///
/// `audio_url` 是 `None`（或 `audio` 是 `None`）时只有视频那一条 `AdaptationSet`。
pub fn dash_manifest(
    video_url: &str,
    audio_url: Option<&str>,
    video: &DashTrack,
    audio: Option<&DashTrack>,
    duration_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\" type=\"static\" minBufferTime=\"PT2S\"",
    );
    if let Some(ms) = duration_ms.filter(|ms| *ms > 0) {
        out.push_str(&format!(" mediaPresentationDuration=\"PT{}S\"", seconds(ms)));
    }
    out.push_str(">\n<Period>\n");
    push_track(&mut out, "video", "v", video_url, video);
    if let (Some(url), Some(audio)) = (audio_url, audio) {
        push_track(&mut out, "audio", "a", url, audio);
    }
    out.push_str("</Period>\n</MPD>\n");
    out
}

fn push_track(out: &mut String, kind: &str, id: &str, url: &str, t: &DashTrack) {
    out.push_str(&format!(
        "<AdaptationSet mimeType=\"{kind}/mp4\" segmentAlignment=\"true\" startWithSAP=\"1\">\n"
    ));
    let bandwidth = t.bandwidth.filter(|b| *b > 0).unwrap_or(1_000_000);
    out.push_str(&format!("<Representation id=\"{id}\" bandwidth=\"{bandwidth}\""));
    if let Some(codecs) = &t.codecs {
        out.push_str(&format!(" codecs=\"{}\"", xml_escape(codecs)));
    }
    if kind == "video" {
        if let Some(w) = t.width {
            out.push_str(&format!(" width=\"{w}\""));
        }
        if let Some(h) = t.height {
            out.push_str(&format!(" height=\"{h}\""));
        }
        if let Some(fr) = t.frame_rate.as_deref().filter(|s| !crate::hosted::is_kotlin_blank(s)) {
            out.push_str(&format!(" frameRate=\"{}\"", xml_escape(fr)));
        }
    }
    out.push_str(">\n");
    out.push_str("<BaseURL>");
    out.push_str(&xml_escape(url));
    out.push_str("</BaseURL>\n");
    out.push_str(&format!(
        "<SegmentBase indexRange=\"{}\"><Initialization range=\"{}\"/></SegmentBase>\n",
        xml_escape(&t.index_range),
        xml_escape(&t.initialization)
    ));
    out.push_str("</Representation>\n</AdaptationSet>\n");
}

/// `ms` → `秒.毫秒三位`（Kotlin `seconds`：`"${ms / 1000}.${(ms % 1000).toString().padStart(3, '0')}"`）。
fn seconds(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// XML 属性 / 文本里的转义（Kotlin `xml()`：`&` `<` `>` `"` `'` 五个都转，顺序先是 `&`）。
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

// ===========================================================================
// 2. 地址解析（Kotlin `DashRewrite.resolveUrl`）
// ===========================================================================

/// `ref` 相对 `base` 解析成绝对地址（`java.net.URI(base).resolve(ref)` 的口径）。
///
/// - `ref` 空 → 返回 `base`；
/// - `base` 空 → 返回 `ref` 自己（`URI(ref).toString()`：解析不出来就原样）；
/// - 认不出来的（`URI.resolve` 抛异常的边角）→ **原样返回 `ref`**（宁可不改，也不要写出错的地址）。
pub fn resolve_url(base: &str, reference: &str) -> String {
    if reference.is_empty() {
        return base.to_string();
    }
    if base.is_empty() {
        return reference.to_string();
    }
    resolve(base, reference).unwrap_or_else(|| reference.to_string())
}

/// `Uri.resolve` 的简化版：只处理清单里会出现的那几种（绝对地址、`//host/path`、`/path`、
/// `path`、`?query`、`#frag`、`.` / `..`）。
///
/// 返回 `None` = 认不出来（调用方原样保留）。Kotlin 用 `java.net.URI`，这里手写是因为
/// **不能引 url crate**（体积 + 离线缓存里没有）；两边口径由 `ManifestRewriteTest` 的用例盯着。
fn resolve(base: &str, reference: &str) -> Option<String> {
    // 带 scheme 的绝对地址：原样
    if let Some(scheme) = scheme_of(reference) {
        if scheme.len() > 1 {
            return Some(reference.to_string());
        }
    }
    let (base_scheme, base_authority, base_path, _base_query) = split_uri(base)?;
    // `//host/path`：只换 authority
    if let Some(rest) = reference.strip_prefix("//") {
        let (auth, path) = match rest.find(['/', '?', '#', '\\']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        return Some(format!("{base_scheme}://{auth}{}", strip_fragment(path)));
    }
    // `?query`：只换查询串，路径原样
    if reference.starts_with('?') {
        return Some(format!(
            "{base_scheme}://{base_authority}{base_path}{reference}"
        ));
    }
    if reference.starts_with('#') {
        return Some(format!(
            "{base_scheme}://{base_authority}{}{}",
            strip_query(base_path),
            reference
        ));
    }
    let ref_path = strip_fragment(strip_query(reference));
    let merged = if ref_path.starts_with('/') {
        ref_path.to_string()
    } else {
        // base 的最后一段是文件名：去掉
        let dir = match base_path.rfind('/') {
            Some(i) => &base_path[..=i],
            None => "/",
        };
        format!("{dir}{ref_path}")
    };
    let normalized = remove_dot_segments(&merged);
    let mut out = format!("{base_scheme}://{base_authority}{normalized}");
    if let Some(q) = query_of(reference) {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

fn scheme_of(value: &str) -> Option<&str> {
    let colon = value.find(':')?;
    let scheme = &value[..colon];
    if scheme.is_empty() {
        return None;
    }
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme)
}

/// `scheme://authority/path?query#frag` → 四段（path 至少是空串）。
fn split_uri(uri: &str) -> Option<(&str, &str, &str, Option<&str>)> {
    let scheme = scheme_of(uri)?;
    let rest = &uri[scheme.len() + 1..];
    let rest = rest.strip_prefix("//")?;
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let tail = strip_fragment(tail);
    let (path, query) = match tail.find('?') {
        Some(i) => (&tail[..i], Some(&tail[i + 1..])),
        None => (tail, None),
    };
    Some((scheme, authority, path, query))
}

fn strip_fragment(value: &str) -> &str {
    match value.find('#') {
        Some(i) => &value[..i],
        None => value,
    }
}

fn strip_query(value: &str) -> &str {
    match value.find('?') {
        Some(i) => &value[..i],
        None => value,
    }
}

fn query_of(value: &str) -> Option<&str> {
    let no_frag = strip_fragment(value);
    no_frag.find('?').map(|i| &no_frag[i + 1..])
}

/// RFC 3986 的 `remove_dot_segments`（只做 `//` 折叠、去掉 `/./` 和 `/../`；**不**动 `%2e`）。
fn remove_dot_segments(path: &str) -> String {
    if !path.contains("/.") && !path.contains("//") {
        return path.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    for seg in path.split('/') {
        match seg {
            "." => {}
            ".." => {
                // 绝对路径的根（`/`）不能弹掉；相对路径弹到空就停
                if !out.is_empty() {
                    out.pop();
                }
            }
            "" => {}
            other => out.push(other),
        }
    }
    let mut joined = String::new();
    if absolute {
        joined.push('/');
    }
    joined.push_str(&out.join("/"));
    if path.ends_with('/') && !joined.ends_with('/') {
        joined.push('/');
    }
    if joined.is_empty() {
        joined.push('/');
    }
    joined
}

// ===========================================================================
// 3. DASH 清单改写（Kotlin `DashRewrite`）
// ===========================================================================

/// 改写时的一层元素：这一层的**远端基准**（继承来的，或者本层 `BaseURL` 解析出来的）。
struct Frame {
    base: String,
}

/// 需要改写属性值的元素名（按**本地名**比较：`mpd:SegmentURL` 也算 `SegmentURL`）。
fn local_name(tag: &str) -> &str {
    match tag.rfind(':') {
        Some(i) => &tag[i + 1..],
        None => tag,
    }
}

/// 把清单里所有指向远端的分片地址换成本机中转地址，返回改写后的正文。
///
/// `base_url` 是这份清单自己的地址（`None` / 空 = 不知道，那就只对**绝对地址**登记，相对地址原样留着——
/// 和 Kotlin 的 `resolveUrl("" , ref)` 一致：`URI.resolve` 在没有 base 时把 `ref` 当相对地址原样返回）。
///
/// 解析不出来（不是 XML）就**原样返回**，不抛异常（Kotlin `runCatching { parse(mpd) }.getOrNull() ?: return mpd`）。
pub fn rewrite_dash(mpd: &str, base_url: Option<&str>, urls: &mut dyn RelayUrls) -> String {
    // 先确认它至少像一份 XML（Kotlin 那边解析失败也原样返回）：没有 `<` 就不动
    if !mpd.contains('<') {
        return mpd.to_string();
    }
    let mut out = String::with_capacity(mpd.len() + 256);
    let mut stack: Vec<Frame> = vec![Frame {
        base: base_url.unwrap_or("").to_string(),
    }];
    let mut pos = 0usize;
    let bytes = mpd.as_bytes();
    while pos < bytes.len() {
        let rest = &mpd[pos..];
        if let Some(stripped) = rest.strip_prefix("<!--") {
            let end = stripped
                .find("-->")
                .map(|i| pos + 4 + i + 3)
                .unwrap_or(mpd.len());
            out.push_str(&mpd[pos..end]);
            pos = end;
            continue;
        }
        if let Some(stripped) = rest.strip_prefix("<![CDATA[") {
            let end = stripped
                .find("]]>")
                .map(|i| pos + 9 + i + 3)
                .unwrap_or(mpd.len());
            out.push_str(&mpd[pos..end]);
            pos = end;
            continue;
        }
        if let Some(stripped) = rest.strip_prefix("<?") {
            let end = stripped
                .find("?>")
                .map(|i| pos + 2 + i + 2)
                .unwrap_or(mpd.len());
            out.push_str(&mpd[pos..end]);
            pos = end;
            continue;
        }
        if let Some(stripped) = rest.strip_prefix("<!") {
            let end = stripped
                .find('>')
                .map(|i| pos + i + 1)
                .unwrap_or(mpd.len());
            out.push_str(&mpd[pos..end]);
            pos = end;
            continue;
        }
        if !rest.starts_with('<') {
            // 标签之间的文本（缩进 / BaseURL 之外的内容）：原样搬，直到下一个 `<`
            let end = rest.find('<').unwrap_or(rest.len());
            out.push_str(&rest[..end]);
            pos += end;
            continue;
        }
        let Some(gt) = tag_end(rest) else {
            out.push_str(rest);
            break;
        };
        let raw = &rest[1..gt];
        let closing = raw.starts_with('/');
        if closing {
            out.push_str(&mpd[pos..pos + gt + 1]);
            pos += gt + 1;
            if stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let self_closing = raw.trim_end().ends_with('/');
        let body = raw.trim_end().trim_end_matches('/').trim_end();
        let name = body.split_whitespace().next().unwrap_or("");
        let local = local_name(name);
        let rewritten = rewrite_open_tag(name, local, raw, &stack, urls);
        out.push_str(&rewritten);
        pos += gt + 1;
        if self_closing {
            continue;
        }
        // `<BaseURL>…</BaseURL>`：把这段文本换成本机地址，并让这一层（以及它的子元素）用新的远端基准
        if local == "BaseURL" {
            let end = mpd[pos..].find("</").map(|i| pos + i).unwrap_or(mpd.len());
            let text_raw = &mpd[pos..end];
            let text = text_raw.trim();
            let current = stack.last().map(|f| f.base.clone()).unwrap_or_default();
            let next_base = if text.is_empty() {
                current.clone()
            } else {
                let abs = resolve_url(&current, text);
                let local_url = urls.resource(&abs);
                // 只替换那段文本，前后的空白原样保留
                match text_raw.find(text) {
                    Some(at) => {
                        out.push_str(&text_raw[..at]);
                        out.push_str(&xml_escape(&local_url));
                        out.push_str(&text_raw[at + text.len()..]);
                    }
                    None => out.push_str(text_raw),
                }
                abs
            };
            // BaseURL 的语义是"给**这一层元素的子元素**用"：所以更新的是它的父层
            // （Kotlin `walk` 里的 `base` 就是这个元素自己的局部变量）。
            let target = if stack.len() >= 2 {
                stack.len() - 2
            } else {
                stack.len() - 1
            };
            stack[target].base = next_base;
            pos = end;
            continue;
        }
        let inherited = stack.last().map(|f| f.base.clone()).unwrap_or_default();
        stack.push(Frame { base: inherited });
    }
    out
}

/// 重写一个开标签（只动要动的属性值，其余字节原样）。
fn rewrite_open_tag(
    name: &str,
    local: &str,
    raw: &str,
    stack: &[Frame],
    urls: &mut dyn RelayUrls,
) -> String {
    let base = stack.last().map(|f| f.base.clone()).unwrap_or_default();
    // SegmentTemplate：media / initialization / index / bitstreamSwitching → 模板口子。
    // **登记顺序按 TEMPLATE_ATTRS 这个固定列表**（不是文档里的属性顺序）：Kotlin 也是这么做的，
    // 而"先登记哪个"决定本机模板地址里的 id，对拍要逐字节比就得一致。
    if local == "SegmentTemplate" {
        return rewrite_attrs_in_order(raw, &TEMPLATE_ATTRS, &base, urls, true);
    }
    // SegmentList 的 SegmentURL@media / @index、Initialization@sourceURL → 资源口子
    if local == "SegmentURL" {
        return rewrite_attrs(raw, &["media", "index"], &base, urls, false);
    }
    if local == "Initialization" {
        return rewrite_attrs(raw, &["sourceURL"], &base, urls, false);
    }
    let _ = name;
    raw_with_brackets(raw)
}

const TEMPLATE_ATTRS: [&str; 4] = ["media", "initialization", "index", "bitstreamSwitching"];

/// 找下一个**属性名**后面的 `=`：跳过引号里的内容（属性值里的 `=` 不算），并且要求 `=` 前面是一个
/// 完整的属性名（`=` 前是属性名、再前是空白或标签开头）。
fn find_attr_equals(raw: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut attr_start = 0usize;
    for (i, c) in raw.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '=' => {
                    let name = raw[attr_start..i].trim();
                    if !name.is_empty() && !name.contains(['<', '>', '/']) {
                        return Some(i);
                    }
                    attr_start = i + 1;
                }
                c if c.is_whitespace() => attr_start = i + c.len_utf8(),
                _ => {}
            },
        }
    }
    None
}

/// 和 [`rewrite_attrs`] 一样，但**登记顺序按 `attrs` 这个列表**（不是文档顺序）。
///
/// 只有 `SegmentTemplate` 用它：那几个属性的登记顺序会决定本机地址里的 id，而 Kotlin 是按固定列表走的。
fn rewrite_attrs_in_order(
    raw: &str,
    attrs: &[&str],
    base: &str,
    urls: &mut dyn RelayUrls,
    template: bool,
) -> String {
    // 先按固定顺序算出每个属性换成本机地址后的值
    let mut replaced: Vec<(&str, String)> = Vec::new();
    for attr in attrs {
        if let Some(value) = attr_value(raw, attr) {
            if !value.is_empty() {
                let abs = resolve_url(base, &crate::reddit::play::xml::decode_entities(&value));
                let local = if template {
                    urls.template(&abs)
                } else {
                    urls.resource(&abs)
                };
                replaced.push((attr, local));
            }
        }
    }
    if replaced.is_empty() {
        return raw_with_brackets(raw);
    }
    // 再按**文档里的属性顺序**把值换掉（其余字节原样）
    replace_attr_values(raw, &replaced)
}

/// 标签里所有属性（原文顺序）：`(名字, 值, 值在 raw 里的起止)`。
fn attributes(raw: &str) -> Vec<(String, String, usize, usize)> {
    let mut out: Vec<(String, String, usize, usize)> = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let Some(eq_rel) = find_attr_equals(&raw[pos..]) else {
            break;
        };
        let eq = pos + eq_rel;
        let before = &raw[pos..eq];
        let name_start = before
            .rfind(|c: char| c.is_whitespace() || c == '<')
            .map(|i| i + 1)
            .unwrap_or(0);
        let name = before[name_start..].trim().to_string();
        let after = &raw[eq + 1..];
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
        let value_start = eq + 1 + quote.map(|q| q.len_utf8()).unwrap_or(0);
        let (value_end, next) = match quote {
            Some(q) => match raw[value_start..].find(q) {
                Some(i) => (value_start + i, value_start + i + 1),
                None => (raw.len(), raw.len()),
            },
            None => match raw[value_start..].find(|c: char| c.is_whitespace() || c == '>' || c == '/') {
                Some(i) => (value_start + i, value_start + i),
                None => (raw.len(), raw.len()),
            },
        };
        if !name.is_empty() {
            out.push((name, raw[value_start..value_end].to_string(), value_start, value_end));
        }
        pos = next.max(eq + 1);
    }
    out
}

/// 取一个属性的原值（不 trim）。
fn attr_value(raw: &str, attr: &str) -> Option<String> {
    attributes(raw)
        .into_iter()
        .find(|(name, _, _, _)| name == attr)
        .map(|(_, value, _, _)| value)
}

/// 把属性值换成新值（按文档顺序写一遍，别的字节原样）。
fn replace_attr_values(raw: &str, replaced: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(raw.len() + 64);
    out.push('<');
    let mut pos = 0usize;
    for (name, _, value_start, value_end) in attributes(raw) {
        out.push_str(&raw[pos..value_start]);
        match replaced.iter().find(|(a, _)| *a == name) {
            Some((_, local)) => out.push_str(&xml_escape(local)),
            None => out.push_str(&raw[value_start..value_end]),
        }
        pos = value_end;
    }
    out.push_str(&raw[pos..]);
    out.push('>');
    out
}

/// 把 `raw`（不含 `<>`）里列出的属性值改写掉。
fn rewrite_attrs(
    raw: &str,
    attrs: &[&str],
    base: &str,
    urls: &mut dyn RelayUrls,
    template: bool,
) -> String {
    let mut out = String::with_capacity(raw.len() + 64);
    out.push('<');
    let mut pos = 0usize;
    let bytes = raw.as_bytes();
    while pos < bytes.len() {
        // 找下一个 `name=`（在标签内）
        let rest = &raw[pos..];
        let Some(eq) = find_attr_equals(rest) else {
            out.push_str(rest);
            break;
        };
        // 属性名：从 eq 往前回退到空白 / 开头
        let before = &rest[..eq];
        let name_start = before
            .rfind(|c: char| c.is_whitespace() || c == '<')
            .map(|i| i + 1)
            .unwrap_or(0);
        let attr = &before[name_start..];
        out.push_str(&rest[..name_start]);
        out.push_str(attr);
        out.push('=');
        // 值：单 / 双引号
        let after = &rest[eq + 1..];
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
        let (value, consumed) = match quote {
            Some(q) => {
                let body = &after[q.len_utf8()..];
                // 闭引号的位置（找不到就当整段都是值）
                let end = body.find(q).unwrap_or(body.len());
                // 从 `rest` 开头的偏移量：`=` + 开引号 + 值 + 闭引号（没有闭引号就到头）
                let consumed = if end < body.len() {
                    eq + 1 + q.len_utf8() + end + q.len_utf8()
                } else {
                    eq + 1 + q.len_utf8() + end
                };
                (&body[..end], consumed)
            }
            None => {
                let end = after
                    .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                    .unwrap_or(after.len());
                (&after[..end], eq + 1 + end)
            }
        };
        if let Some(q) = quote {
            out.push(q);
        }
        if attrs.iter().any(|a| *a == attr) && !value.is_empty() {
            let abs = resolve_url(base, &crate::reddit::play::xml::decode_entities(value));
            let local = if template {
                urls.template(&abs)
            } else {
                urls.resource(&abs)
            };
            out.push_str(&xml_escape(&local));
        } else {
            out.push_str(value);
        }
        if let Some(q) = quote {
            out.push(q);
        }
        pos += consumed;
    }
    out.push('>');
    out
}

fn raw_with_brackets(raw: &str) -> String {
    format!("<{raw}>")
}

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

// ===========================================================================
// 4. HLS 播放列表改写（Kotlin `HlsRewrite`）
// ===========================================================================

/// m3u8 里的分片 URI、子清单、`#EXT-X-KEY` / `#EXT-X-MAP` 这些带 `URI="…"` 的标签都换成中转地址。
///
/// - 直接写成一行 URI 的，按"上一行是不是 `#EXT-X-STREAM-INF`"和扩展名判断是子清单还是分片；
/// - 带 `URI="…"` 属性的标签全部改写，属性之外的内容原样保留；
/// - 相对地址按这份播放列表自己的地址解析。
pub fn rewrite_hls(playlist_url: &str, text: &str, urls: &mut dyn RelayUrls) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    let mut expect_playlist = false;
    // `split('\n')` 会多出一个"最后一个换行之后的空串"：那一段要原样接上，不能凭空多出一行
    let mut lines = text.split('\n').peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim_end_matches('\r');
        if crate::hosted::is_kotlin_blank(line) {
            out.push_str(raw);
        } else if line.starts_with('#') {
            out.push_str(&rewrite_hls_tag(playlist_url, line, urls));
            expect_playlist = line.starts_with("#EXT-X-STREAM-INF");
        } else {
            let abs = resolve_url(playlist_url, crate::hosted::kotlin_trim(line));
            if expect_playlist || is_playlist_url(&abs) {
                out.push_str(&urls.playlist(&abs));
            } else {
                out.push_str(&urls.resource(&abs));
            }
            expect_playlist = false;
        }
        if lines.peek().is_some() {
            out.push('\n');
        }
    }
    out
}

fn rewrite_hls_tag(playlist_url: &str, line: &str, urls: &mut dyn RelayUrls) -> String {
    let tag = line.split(':').next().unwrap_or(line);
    if !URI_TAGS.contains(&tag) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 64);
    let mut pos = 0usize;
    while let Some(found) = line[pos..].find("URI=\"") {
        let start = pos + found;
        out.push_str(&line[pos..start]);
        let body_start = start + 5;
        let Some(end_rel) = line[body_start..].find('"') else {
            out.push_str(&line[start..]);
            return out;
        };
        let end = body_start + end_rel;
        let value = &line[body_start..end];
        let abs = resolve_url(playlist_url, value);
        let local = if PLAYLIST_TAGS.contains(&tag) || is_playlist_url(&abs) {
            urls.playlist(&abs)
        } else {
            urls.resource(&abs)
        };
        out.push_str("URI=\"");
        out.push_str(&local);
        out.push('"');
        pos = end + 1;
    }
    out.push_str(&line[pos..]);
    out
}

fn is_playlist_url(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".m3u8")
}

/// 带 `URI="…"` 的标签。
const URI_TAGS: [&str; 9] = [
    "#EXT-X-KEY",
    "#EXT-X-SESSION-KEY",
    "#EXT-X-MAP",
    "#EXT-X-MEDIA",
    "#EXT-X-I-FRAME-STREAM-INF",
    "#EXT-X-PART",
    "#EXT-X-PRELOAD-HINT",
    "#EXT-X-RENDITION-REPORT",
    "#EXT-X-SESSION-DATA",
];

/// 属性里那个 URI 指向**另一份播放列表**的标签。
const PLAYLIST_TAGS: [&str; 3] = [
    "#EXT-X-MEDIA",
    "#EXT-X-I-FRAME-STREAM-INF",
    "#EXT-X-RENDITION-REPORT",
];

