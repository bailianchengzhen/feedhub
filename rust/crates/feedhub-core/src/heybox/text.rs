//! 小黑盒的**纯文本 / 行内片段**工具：逐条对照 `source-heybox/HeyboxBody.kt` 的
//! `plainBlocks` / `inline` / `expandEmotes` / `escapeHtml` / `containsAnyTag` / `hasHtmlTag`。
//!
//! 和 B站 的 `bili/text.rs` 是同一类东西，但口径是**小黑盒自己那几条正则**：
//!
//! | Kotlin | 这里 |
//! |---|---|
//! | `INLINE = (https?://[^\s<>"'\u3000-\u303f\uff00-\uffef]+)\|\[([A-Za-z0-9]+_[^\[\]\s]{1,30})]` | [`inline`]（手写扫描器，左到右不重叠） |
//! | `EMOTE_TOKEN` | `super::contains_emote_token` / `super::emote_match` |
//! | `ANY_TAG = </?[a-zA-Z][^>]*>` | [`contains_any_tag`] |
//! | `HTML_HINT = <(p\|div\|br\|h[1-6]\|img\|a\|b\|strong\|ul\|ol\|li\|blockquote\|span\|pre)\b` | [`has_html_tag`] |
//! | `EMOJI_SPAN` / `EMOJI_CLASS` | [`emoji_spans_to_images`] |
//! | `URLDecoder.decode(s, "UTF-8")` | [`url_decode`] |
//! | `escapeHtml` | [`escape_html`] |
//!
//! 两个"必须逐字节复刻"的地方：Java 正则的 `\s` 是 `[ \t\n\x0B\f\r]`（**不含** NBSP；多字节字符都算
//! 非空白），`trim()` / `isNotBlank()` 用 Kotlin 的 `Character.isWhitespace || isSpaceChar` 口径
//! （见 `crate::html::kotlin_trim`）。

use crate::detail::{Block, Span};
use crate::json::to_https;
use crate::model::UserRef;

use super::{emote_match, utf8_len, HEYBOX};

/// 按行分段，行内认 URL 和表情代码；空行、不间断空格都收掉（Kotlin `plainBlocks`）。
pub fn plain_blocks(text: &str, emotes: &[(String, String)]) -> Vec<Block> {
    text.replace('\u{a0}', " ")
        .split('\n')
        .map(|line| crate::html::kotlin_trim(line))
        .filter(|line| !line.is_empty())
        .map(|line| Block::Paragraph {
            spans: inline(line, emotes, true, false, false),
        })
        .collect()
}

/// 行内：URL 和表情代码一次扫出来（Kotlin 的 `inline`）。
///
/// `linkify` 为 false 时 URL 只当文字（`expandEmotes` 用）；`bold` / `italic` 是外层 `<b>` / `<i>` 的样式。
pub fn inline(
    text: &str,
    emotes: &[(String, String)],
    linkify: bool,
    bold: bool,
    italic: bool,
) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut pos = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        // URL 那一支（先试它，和正则的交替顺序一致）
        if let Some(len) = url_len(rest) {
            push_text(&mut out, &text[pos..i], bold, italic);
            let url = &rest[..len];
            if linkify {
                out.push(Span::Link {
                    text: url.to_string(),
                    url: url.to_string(),
                    target: None,
                });
            } else {
                push_text(&mut out, url, bold, italic);
            }
            i += len;
            pos = i;
            continue;
        }
        // 表情代码那一支
        if rest.as_bytes()[0] == b'[' {
            if let Some((end, name)) = emote_match(rest.as_bytes(), 0) {
                push_text(&mut out, &text[pos..i], bold, italic);
                // `INLINE` 正则的第二组是**带表情包前缀的整段**（`cube_并不简单`），
                // 和 `EMOTE_PREFIX`（第一组只到名字）不一样——`Span.Emote.text` 要的是前者。
                let full = &rest[1..name.1];
                match emotes.iter().find(|(c, _)| c == full) {
                    Some((_, img)) => out.push(Span::Emote {
                        text: format!("[{full}]"),
                        url: img.clone(),
                    }),
                    None => push_text(&mut out, &rest[..end], bold, italic),
                }
                i += end;
                pos = i;
                continue;
            }
        }
        i += char_len(rest);
    }
    push_text(&mut out, &text[pos..], bold, italic);
    out
}

/// 跳过文本里的一个字符（返回它的字节长度）。
fn char_len(s: &str) -> usize {
    s.chars().next().map(char::len_utf8).unwrap_or(1)
}

/// 相邻同样式的文字合并，空白不重复（Kotlin `addText`）。
fn push_text(out: &mut Vec<Span>, t: &str, bold: bool, italic: bool) {
    if t.is_empty() {
        return;
    }
    if let Some(Span::Text {
        text,
        bold: b,
        italic: it,
    }) = out.last_mut()
    {
        if *b == bold && *it == italic {
            let merged = if text.ends_with(' ') && t.starts_with(' ') {
                format!("{text}{}", crate::html::kotlin_trim_start(t))
            } else {
                format!("{text}{t}")
            };
            *text = merged;
            return;
        }
    }
    out.push(Span::Text {
        text: t.to_string(),
        bold,
        italic,
    });
}

/// `https?://[^\s<>"'\u3000-\u303f\uff00-\uffef]+` 的长度（不匹配返回 `None`）。
fn url_len(s: &str) -> Option<usize> {
    let rest = if let Some(r) = s.strip_prefix("http://") {
        r
    } else if let Some(r) = s.strip_prefix("https://") {
        r
    } else {
        return None;
    };
    let prefix = s.len() - rest.len();
    let mut len = prefix;
    for c in rest.chars() {
        if is_url_stop(c) {
            break;
        }
        len += c.len_utf8();
    }
    // `+` 至少要有一个字符
    if len == prefix {
        None
    } else {
        Some(len)
    }
}

fn is_url_stop(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '<' | '>' | '"' | '\''
    ) || ('\u{3000}'..='\u{303f}').contains(&c)
        || ('\u{ff00}'..='\u{ffef}').contains(&c)
}

/// `</?[a-zA-Z][^>]*>`（Kotlin `ANY_TAG`）：文本里有没有标签。
pub fn contains_any_tag(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let mut j = i + 1;
            if j < b.len() && b[j] == b'/' {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_alphabetic() && s[j..].contains('>') {
                return true;
            }
        }
        i += utf8_len(b[i]).max(1);
    }
    false
}

/// Kotlin `HTML_HINT`：`<(p|div|br|h[1-6]|img|a|b|strong|ul|ol|li|blockquote|span|pre)\b`。
pub fn has_html_tag(s: &str) -> bool {
    const TAGS: [&str; 19] = [
        "p", "div", "br", "h1", "h2", "h3", "h4", "h5", "h6", "img", "a", "b", "strong", "ul",
        "ol", "li", "blockquote", "span", "pre",
    ];
    let lower = s.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let rest = &lower[i + 1..];
            let matched = TAGS.iter().any(|tag| {
                rest.starts_with(tag)
                    && !rest
                        .as_bytes()
                        .get(tag.len())
                        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            });
            if matched {
                return true;
            }
        }
        i += utf8_len(b[i]).max(1);
    }
    false
}

/// HTML 里转出来的文字也可能带 `[cube_喜欢]` 代码，有表情表时换成表情（Kotlin `expandEmotes`）。
pub fn expand_emotes(blocks: Vec<Block>, emotes: &[(String, String)]) -> Vec<Block> {
    if emotes.is_empty() {
        return blocks;
    }
    fn spans(list: Vec<Span>, emotes: &[(String, String)]) -> Vec<Span> {
        let mut out = Vec::with_capacity(list.len());
        for span in list {
            match span {
                Span::Text {
                    text,
                    bold,
                    italic,
                } if super::contains_emote_token(&text) => {
                    out.extend(inline(&text, emotes, false, bold, italic));
                }
                other => out.push(other),
            }
        }
        out
    }
    blocks
        .into_iter()
        .map(|b| match b {
            Block::Paragraph { spans: list } => Block::Paragraph {
                spans: spans(list, emotes),
            },
            Block::Heading { spans: list } => Block::Heading {
                spans: spans(list, emotes),
            },
            Block::Quote { spans: list } => Block::Quote {
                spans: spans(list, emotes),
            },
            other => other,
        })
        .collect()
}

/// `HeyboxBody.escapeHtml`：`&` → `&amp;`、`"` → `&quot;`、`<` / `>`（**顺序不能换**）。
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `java.net.URLDecoder.decode(s, "UTF-8")`：`+` → 空格、`%hh` → 字节；坏转义返回 `None`
/// （Kotlin 那边是 `runCatching { … }.getOrNull()`）。
pub fn url_decode(s: &str) -> Option<String> {
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
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = hex_val(bytes[i + 1])?;
                let lo = hex_val(bytes[i + 2])?;
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

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `HeyboxBody.emojiSpansToImages`：把雪碧图表情 `<span class="hb-emoji hb-emoji-cube_103" data-emoji="cube_电牛"></span>`
/// 换成单张 PNG 的 `<img class="hb-emote" alt="[cube_电牛]" src="…">`，这样能走 HTML 转换器的行内图片钩子。
///
/// 正则 `EMOJI_SPAN` 是 `<span\b[^>]*?\bdata-emoji="([^"]*)"[^>]*>\s*</span>`（忽略大小写），
/// `EMOJI_CLASS` 是 `hb-emoji-([A-Za-z0-9]+)_(\d+)`（在整段匹配里找第一个）。都在这里手写扫描。
pub fn emoji_spans_to_images(html: &str, emotes: &[(String, String)]) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    let mut copied = 0usize;
    while i < b.len() {
        if b[i] == b'<' && starts_with_ci(&html[i..], "<span") {
            if let Some((end, name)) = emoji_span_at(html, i) {
                out.push_str(&html[copied..i]);
                out.push_str(&emoji_replacement(&name, &html[i..end], emotes));
                i = end;
                copied = end;
                continue;
            }
        }
        i += char_len(&html[i..]);
    }
    out.push_str(&html[copied..]);
    out
}

fn starts_with_ci(s: &str, needle: &str) -> bool {
    s.len() >= needle.len() && s.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

/// 在 `html[start]` 处匹配 `EMOJI_SPAN`，返回 `(整段结束, data-emoji 的值)`。
fn emoji_span_at(html: &str, start: usize) -> Option<(usize, String)> {
    let after_span = start + "<span".len();
    // `\b`：`<span` 后面不能紧跟名字字符
    if html[after_span..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let tag_end = html[after_span..].find('>').map(|i| after_span + i + 1)?;
    let tag = &html[after_span..tag_end];
    // `[^>]*?\bdata-emoji="`：第一个满足"前面是边界"的
    let mut search = 0usize;
    let mut found: Option<(usize, usize)> = None;
    while let Some(rel) = tag[search..].find("data-emoji=\"") {
        let at = search + rel;
        let before = tag[..at].chars().next_back();
        let boundary = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if boundary {
            found = Some((at, at + "data-emoji=\"".len()));
            break;
        }
        search = at + "data-emoji=\"".len();
    }
    let (_, value_start) = found?;
    let value_end = tag[value_start..].find('"').map(|i| value_start + i)?;
    let name = tag[value_start..value_end].to_string();
    // `>[^>]*\s*</span>`：标签结束后只允许空白，然后是 `</span>`
    let mut pos = tag_end;
    while pos < html.len() {
        let c = html[pos..].chars().next()?;
        if is_java_space_char(c) {
            pos += c.len_utf8();
        } else {
            break;
        }
    }
    if starts_with_ci(&html[pos..], "</span>") {
        return Some((pos + "</span>".len(), name));
    }
    None
}

fn is_java_space_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n' || c == '\u{0b}' || c == '\u{0c}' || c == '\r'
}

/// 正则替换的产物：能找到图就换成 `<img>`，找不到就只剩文字代码。
fn emoji_replacement(name: &str, whole: &str, emotes: &[(String, String)]) -> String {
    let url = emoji_class_url(whole).or_else(|| {
        emotes
            .iter()
            .find(|(code, _)| code == name)
            .map(|(_, url)| url.clone())
    });
    let token = escape_html(&format!("[{name}]"));
    match url {
        None => token,
        Some(url) => format!(
            "<img class=\"hb-emote\" alt=\"{token}\" src=\"{}\">",
            escape_html(&url)
        ),
    }
}

/// `hb-emoji-([A-Za-z0-9]+)_(\d+)` 的第一个匹配 → 单张 PNG 地址。
fn emoji_class_url(whole: &str) -> Option<String> {
    let b = whole.as_bytes();
    let needle = b"hb-emoji-";
    let mut i = 0usize;
    while i + needle.len() <= b.len() {
        if b[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            let mut j = i + needle.len();
            let group_start = j;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j > group_start && j < b.len() && b[j] == b'_' {
                let group = &whole[group_start..j];
                j += 1;
                let digits_start = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digits_start {
                    return Some(format!(
                        "https://imgheybox.max-c.com/heybox/emoji/{group}_{}.png",
                        &whole[digits_start..j]
                    ));
                }
            }
        }
        i += 1;
    }
    None
}

/// 帖子正文里那个"视频帖自己的视频"：`has_video=1` + `video_url` → (url, duration, width, height)。
pub fn main_video(link: &crate::json::JsonMap) -> Option<(String, Option<i32>, Option<i32>, Option<i32>)> {
    use crate::json;
    if json::bool(link, "has_video") != Some(true) {
        return None;
    }
    let url = json::text(link, "video_url").map(|u| to_https(&u))?;
    Some((
        url,
        json::int(link, "video_duration").filter(|d| *d > 0),
        json::int(link, "video_width").filter(|w| *w > 0),
        json::int(link, "video_height").filter(|h| *h > 0),
    ))
}

/// 正文里的视频片段（图文段数组 / HTML 里的 `<video>`）：`seg:n` 的第 n 个。
#[derive(Debug, Clone, PartialEq)]
pub struct VideoRef {
    pub url: String,
    pub cover: Option<String>,
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 视频片段编号：按出现顺序，第 n 个是 `seg:n`。没有帖子 id（评论等）时不产生 `Block.Video`，降级成卡片。
pub struct VideoSeq {
    pub link_id: Option<String>,
    pub refs: Vec<VideoRef>,
}

impl VideoSeq {
    pub fn new(link_id: Option<String>) -> Self {
        VideoSeq {
            link_id,
            refs: Vec::new(),
        }
    }

    /// 记一个视频并给出 `Block.Video`；没有帖子 id 时返回 `None`（调用方降级成卡片）。
    pub fn add(&mut self, reference: VideoRef) -> Option<Block> {
        let id = self.link_id.clone()?;
        self.refs.push(reference.clone());
        let n = self.refs.len();
        Some(Block::Video {
            id: id.clone(),
            part_id: format!("seg:{n}"),
            cover: reference.cover,
            duration_sec: reference.duration_sec,
            width: reference.width,
            height: reference.height,
            title: None,
            url: Some(super::link_url(&id)),
        })
    }
}

/// 视频卡片（不是视频帖自己的视频、也不带地址时用）。
pub fn video_card(link: &crate::json::JsonMap) -> Option<Block> {
    use crate::json;
    if json::bool(link, "has_video") != Some(true) {
        return None;
    }
    let url = json::text(link, "video_url").map(|u| to_https(&u));
    let cover = json::text(link, "video_thumb").map(|u| to_https(&u));
    if url.is_none() && cover.is_none() {
        return None;
    }
    let dur = json::int(link, "video_duration")
        .filter(|d| *d > 0)
        .map(|d| format!("{}:{:02}", d / 60, d % 60));
    let size = json::int(link, "video_width")
        .and_then(|w| json::int(link, "video_height").map(|h| format!("{w}x{h}")));
    let subtitle = [dur, size]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    Some(Block::Card {
        title: "视频".to_string(),
        subtitle: if subtitle.is_empty() {
            None
        } else {
            Some(subtitle)
        },
        cover,
        url,
        target: None,
    })
}

/// 评论作者 / 帖子里 `<a heybox://…>` 指向的用户：只给 id 和文字（网页个人主页地址没核实过）。
pub fn mention_user(id: String, text: &str) -> UserRef {
    UserRef {
        platform: HEYBOX.to_string(),
        id,
        name: text.strip_prefix('@').unwrap_or(text).to_string(),
        avatar: None,
        url: None,
    }
}

