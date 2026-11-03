//! `CoolapkHtml` 的 Rust 版本：酷安正文 HTML → 段落 / 纯文本（**信息流卡片要的那一部分**）。
//!
//! 酷安的 `message` 不是完整 HTML，而是"纯文本 + 少量标签"：换行就是 `\n`，标签只有
//! `<a class="feed-link-tag|feed-link-uname|feed-link-url">`、`<br/>`、偶尔的 `<img>`、`<!--break-->`，
//! 以及被截断正文末尾的 `<a href="">查看更多</a>`。`CoolapkParser.feed` 只用到两件事：
//! 标题 / 摘要的**纯文本**和里面**认得的表情**，所以这里只要一个忠实的分词器 + 段落归并，
//! 不需要 `blockquote` / 富文本那些（详情 / 评论没搬）。
//!
//! 分词口径：Kotlin 用一条正则
//! `<!--.*?-->|<a\b([^>]*)>(.*?)</a\s*>|<br\s*/?>|<img\b([^>]*)>|<[^>]*>|[^<]+|<`
//! 做分词（`DOT_MATCHES_ALL` + `IGNORE_CASE`）。这里手写同一套优先级：
//! 在同一个起点上按顺序试这 7 个分支，第一个命中就整段吃掉，然后从结尾继续。
//!
//! 两套输出（同一个分词器，两种"块"）：
//! - [`to_blocks`] / [`to_plain`]：**信息流卡片**要的那一部分（[`Span::Other`] 不区分链接 / @人）；
//! - [`to_detail_blocks`]：**详情 / 评论**用的 [`crate::detail::Block`] —— 链接、@人（带 uid 就是
//!   [`crate::detail::Span::Mention`]）、B站视频链接的目标、图片块，全都照 `CoolapkHtml.toBlocks` 的口径。
//!
//! 为什么不用 `html/dom.rs`（那个 HTML5 分词器 + 树构造）：酷安的正文**不是 HTML**，是"纯文本 + 少量标签"，
//! Kotlin 那条正则的分支顺序和"没闭合的 `<` 只吃一个字符"这些行为都是这个格式的一部分（比如 `<a href="">查看更多</a>`
//! 依赖 `<[^>]*>` 那一支的贪婪），换成完整 HTML5 解析器反而会得出不一样的结果。所以这里只复用**同一个**
//! 手写分词器，`html/dom.rs` 一个字没动。
//!
//! 和 Kotlin 的差别（写在这里免得以后误以为漏了）：
//! - 信息流那条路不需要 `Span.Mention` / `Span.Link` 的区分，都收成 [`Span::Other`]——卡片只取文字；
//! - 实体解码遇到**非法码点**（`&#x110000;` 这种）时 Kotlin 的 `Character.toChars` 会抛异常，这里当普通文字留着；
//! - `Span::Mention` 的"uid 全是数字"判定用 ASCII 数字（Kotlin 的 `Char.isDigit` 认 Unicode 数字，
//!   `/u/` 后面出现阿拉伯-印度数字的情况酷安不会产出）；
//! - `URLDecoder.decode` 遇到坏的 `%` 转义在 Kotlin 里会抛异常，这里宽松地原样留着。
//!
//! 表情（`#(笑眼)` 带 `#`、方括号不带）和"查看更多"的处理见 `CoolapkEmoji` / `CoolapkHtml` 的注释。

use super::emoji::{self, EmoteTable};
use crate::detail::{Block as DetailBlock, Span as DetailSpan};
use crate::hosted;
use crate::json::to_https;
use crate::model::UserRef;

/// 段落里的一个片段。信息流卡片只用 [`Span::text`]，表情另外收进 `FeedItem.emotes`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// 普通文字（相邻的文字已经合并、首尾空白已修整）。
    Text(String),
    /// 链接 / @人 / 图片占位这类 span 的文字部分（Kotlin 那边是 `Span.Link` / `Span.Mention`）。
    Other(String),
    /// 认得的表情（`text` 是原文里的代码，`url` 是随插件带的图片地址）。
    Emote { text: String, url: String },
}

impl Span {
    pub fn text(&self) -> &str {
        match self {
            Span::Text(t) | Span::Other(t) => t,
            Span::Emote { text, .. } => text,
        }
    }
}

/// 正文里的一个块。信息流卡片只关心 [`Block::Paragraph`] 的文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Image {
        url: String,
        width: Option<i32>,
        height: Option<i32>,
    },
}

/// `CoolapkHtml.toPlain` 的结果：纯文本 + 里面认得的表情（按出现顺序）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plain {
    pub text: String,
    pub emotes: Vec<(String, String)>,
}

// ===========================================================================
// 分词
// ===========================================================================

/// 一次匹配的结果。字段的形状和 Kotlin `MatchResult` 的用法对齐（`groupValues[1..3]`）。
struct Match<'a> {
    end: usize,
    tok: &'a str,
    /// `<a …>inner</a>` 的 (属性串, 内部 HTML)；不是 a 标签时 `None`（对应 Kotlin `groups[2] == null`）。
    anchor: Option<(&'a str, &'a str)>,
    /// `<img …>` 的属性串；别的分支是空串（对应 Kotlin `groupValues[3]`）。
    img_attrs: &'a str,
}

fn starts_with_ci(s: &str, i: usize, pat: &str) -> bool {
    let b = s.as_bytes();
    let end = i + pat.len();
    end <= b.len() && b[i..end].eq_ignore_ascii_case(pat.as_bytes())
}

fn find_ci(s: &str, needle: &str, from: usize) -> Option<usize> {
    let b = s.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || b.len() < n.len() {
        return None;
    }
    let mut i = from.min(b.len());
    while i + n.len() <= b.len() {
        if b[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 扫描缓存：**只做加速，不改语义**。
///
/// 分词器每走到一个 `<` 都要往后找 `-->` / `</a`。找不到的时候（未闭合的注释 / 未闭合的 `<a>`），
/// 下一次还会从更后面重新找一遍——Kotlin 那条正则 `<!--.*?-->|<a\b([^>]*)>(.*?)</a\s*>` 在 Java 里
/// 也是这个形状，所以两边一样慢。但 Rust 这边可以**记一笔**
/// "从这个位置往后都没有 `-->` 了"，于是整段只在第一次扫一遍：语义完全一样（找不到的位置只会往后，
/// 前面找不到后面也不可能找到），复杂度从 O(n²) 掉到 O(n)。
///
/// 为什么要修：酷安的正文虽然只是"纯文本 + 少量标签"，但**截断的响应 / 畸形正文**里出现一堆
/// `<a href="` 是可能的。
#[derive(Default)]
struct ScanCache {
    /// `Some(p)` = 从 `p` 往后找不到 `-->`（含 `p`）。
    no_comment_close_from: Option<usize>,
    /// `Some(p)` = 从 `p` 往后找不到 `</a`（含 `p`）。
    no_anchor_close_from: Option<usize>,
}

impl ScanCache {
    fn comment_close(&mut self, s: &str, from: usize) -> Option<usize> {
        if let Some(bound) = self.no_comment_close_from {
            if from >= bound {
                return None;
            }
        }
        match find_ci(s, "-->", from) {
            Some(pos) => Some(pos),
            None => {
                self.no_comment_close_from = Some(match self.no_comment_close_from {
                    Some(b) => b.min(from),
                    None => from,
                });
                None
            }
        }
    }

    fn anchor_close(&mut self, s: &str, from: usize) -> Option<usize> {
        if let Some(bound) = self.no_anchor_close_from {
            if from >= bound {
                return None;
            }
        }
        match find_ci(s, "</a", from) {
            Some(pos) => Some(pos),
            None => {
                self.no_anchor_close_from = Some(match self.no_anchor_close_from {
                    Some(b) => b.min(from),
                    None => from,
                });
                None
            }
        }
    }
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// 在 `s[i..]` 上按 Kotlin 那条正则的分支顺序找**最左**的匹配。
fn match_token<'a>(s: &'a str, i: usize, cache: &mut ScanCache) -> Option<Match<'a>> {
    let b = s.as_bytes();
    // 1. <!-- … -->
    if starts_with_ci(s, i, "<!--") {
        if let Some(pos) = cache.comment_close(s, i + 4) {
            return Some(Match {
                end: pos + 3,
                tok: &s[i..pos + 3],
                anchor: None,
                img_attrs: "",
            });
        }
    }
    // 2. <a …>inner</a>
    if starts_with_ci(s, i, "<a") {
        let after = i + 2;
        let boundary = match b.get(after) {
            None => true,
            Some(&c) => !is_word_byte(c),
        };
        if boundary {
            if let Some(gt) = s[after..].find('>') {
                let gt = after + gt;
                if let Some(close) = cache.anchor_close(s, gt + 1) {
                    let mut j = close + 3;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'>') {
                        return Some(Match {
                            end: j + 1,
                            tok: &s[i..j + 1],
                            anchor: Some((&s[after..gt], &s[gt + 1..close])),
                            img_attrs: "",
                        });
                    }
                }
            }
        }
    }
    // 3. <br> / <br/> / <br />
    if starts_with_ci(s, i, "<br") {
        let mut j = i + 3;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b.get(j) == Some(&b'/') {
            j += 1;
        }
        if b.get(j) == Some(&b'>') {
            return Some(Match {
                end: j + 1,
                tok: &s[i..j + 1],
                anchor: None,
                img_attrs: "",
            });
        }
    }
    // 4. <img …>
    if starts_with_ci(s, i, "<img") {
        let after = i + 4;
        let boundary = match b.get(after) {
            None => true,
            Some(&c) => !is_word_byte(c),
        };
        if boundary {
            if let Some(gt) = s[after..].find('>') {
                let gt = after + gt;
                return Some(Match {
                    end: gt + 1,
                    tok: &s[i..gt + 1],
                    anchor: None,
                    img_attrs: &s[after..gt],
                });
            }
        }
    }
    // 5. 其他标签 <…>
    if b.get(i) == Some(&b'<') {
        return match s[i..].find('>') {
            Some(gt) => Some(Match {
                end: i + gt + 1,
                tok: &s[i..i + gt + 1],
                anchor: None,
                img_attrs: "",
            }),
            // 没有 '>'：正则的 `<` 分支只吃一个字符
            None => Some(Match {
                end: i + 1,
                tok: &s[i..i + 1],
                anchor: None,
                img_attrs: "",
            }),
        };
    }
    // 6. [^<]+
    let mut j = i;
    while j < b.len() && b[j] != b'<' {
        j += 1;
    }
    if j > i {
        return Some(Match {
            end: j,
            tok: &s[i..j],
            anchor: None,
            img_attrs: "",
        });
    }
    None
}

// ===========================================================================
// 正文 → 块
// ===========================================================================

/// 解析成段落（按 `\n` / `<br>` 分段，空段落丢掉）。正文里的 `<img>` 变成图片块。
pub fn to_blocks(html: &str, table: &EmoteTable) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0usize;
    let mut cache = ScanCache::default();
    while i < html.len() {
        let Some(m) = match_token(html, i, &mut cache) else {
            i += 1;
            continue;
        };
        if m.tok.starts_with("<!--") {
            // 注释整段丢掉
        } else if let Some((attrs, inner)) = m.anchor {
            // <a …>：链接 / @人 / "查看更多"（href 为空就整段丢掉）
            if let Some(span) = link(attrs, inner) {
                spans.push(span);
            }
        } else if m.tok.len() > 1 && starts_with_ci(m.tok, 0, "<br") {
            // <br>：分段
            flush(&mut out, &mut spans);
        } else if starts_with_ci(m.tok, 0, "<img") {
            // <img …>：图片块（src 为空就忽略）
            if let Some(src) = attr("src", m.img_attrs) {
                if !src.trim().is_empty() {
                    flush(&mut out, &mut spans);
                    out.push(image(&src));
                }
            }
        } else if m.tok.starts_with('<') && m.tok.len() > 1 {
            // 其他标签去掉，保留里面的文字
        } else {
            push_text(&mut out, &mut spans, m.tok, table);
        }
        i = m.end;
    }
    flush(&mut out, &mut spans);
    out
}

fn flush(out: &mut Vec<Block>, spans: &mut Vec<Span>) {
    let taken = std::mem::take(spans);
    if let Some(merged) = normalize(taken) {
        out.push(Block::Paragraph(merged));
    }
}

/// 文字先解实体（Kotlin `text(decodeEntities(tok))`），再按 `\n` 切成多段（每段单独成段）。
fn push_text(out: &mut Vec<Block>, spans: &mut Vec<Span>, text: &str, table: &EmoteTable) {
    let text = decode_entities(text);
    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            flush(out, spans);
        }
        if !line.is_empty() {
            spans.extend(emoji::spans(line, table));
        }
    }
}

/// 合并相邻文字、去掉首尾空白；全是空白就返回 `None`（整段丢掉）。
fn normalize(spans: Vec<Span>) -> Option<Vec<Span>> {
    let mut merged: Vec<Span> = Vec::new();
    for s in spans {
        if let Span::Text(t) = &s {
            if let Some(Span::Text(prev)) = merged.last_mut() {
                prev.push_str(t);
                continue;
            }
        }
        merged.push(s);
    }
    if let Some(Span::Text(t)) = merged.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(Span::Text(t)) = merged.last_mut() {
        *t = t.trim_end().to_string();
    }
    merged.retain(|s| !matches!(s, Span::Text(t) if t.is_empty()));
    if merged.is_empty() {
        return None;
    }
    if merged
        .iter()
        .all(|s| matches!(s, Span::Text(t) if t.trim().is_empty()))
    {
        return None;
    }
    Some(merged)
}

/// `<a …>inner</a>` → 一个 span。`href` 为空、文字为空 / 全是空白就返回 `None`
/// （信息流里截断正文后面的"查看更多"就是 href 为空那种）。
fn link(attrs: &str, inner: &str) -> Option<Span> {
    let text = decode_entities(&strip_tags(inner)).replace('\n', " ");
    let href = attr("href", attrs).map(|h| h.trim().to_string()).unwrap_or_default();
    if href.is_empty() {
        return None;
    }
    if text.trim().is_empty() {
        return None;
    }
    // Kotlin 这里还分 Mention / Link（带 uid 的 @人 vs 普通链接）；卡片只取文字，所以都收成 Other。
    Some(Span::Other(text))
}

// ===========================================================================
// 纯文本
// ===========================================================================

/// 同 [`to_plain_text`]，另外返回文字里的表情（信息流卡片把它们画成图片，见 `FeedItem.emotes`）。
pub fn to_plain(html: &str, table: &EmoteTable) -> Plain {
    let blocks = to_blocks(html, table);
    let mut parts: Vec<String> = Vec::new();
    let mut emotes: Vec<(String, String)> = Vec::new();
    for block in &blocks {
        if let Block::Paragraph(spans) = block {
            let mut line = String::new();
            for span in spans {
                line.push_str(span.text());
                if let Span::Emote { text, url } = span {
                    if !emotes.iter().any(|(code, _)| code == text) {
                        emotes.push((text.clone(), url.clone()));
                    }
                }
            }
            parts.push(line);
        }
    }
    Plain {
        text: parts.join("\n").trim().to_string(),
        emotes,
    }
}

/// 纯文本（摘要用）：段落之间换行，链接只留文字，"查看更多"去掉。表情还是文字代码。
pub fn to_plain_text(html: &str, table: &EmoteTable) -> String {
    to_plain(html, table).text
}

// ===========================================================================
// 小工具（和 `CoolapkHtml` 的私有函数一一对应）
// ===========================================================================

/// 图片块；地址里带尺寸（`..._866@1280x2772.jpg`）时顺便填上宽高。
pub fn image(url: &str) -> Block {
    let (width, height) = image_size(url);
    Block::Image {
        url: to_https(url),
        width,
        height,
    }
}

/// `@(\d+)x(\d+)(?=\.[A-Za-z]+(?:[?#].*)?$)`：从地址里读尺寸（不是就 null）。
pub fn image_size(url: &str) -> (Option<i32>, Option<i32>) {
    let b = url.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'@' {
            let w_start = i + 1;
            let mut j = w_start;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let w_len = j - w_start;
            if w_len > 0 && b.get(j) == Some(&b'x') {
                let h_start = j + 1;
                let mut k = h_start;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                let h_len = k - h_start;
                if h_len > 0 && size_lookahead_ok(b, k) {
                    let w = url[w_start..j].parse::<i32>().ok();
                    let h = url[h_start..k].parse::<i32>().ok();
                    return (w, h);
                }
            }
        }
        i += 1;
    }
    (None, None)
}

/// `(?=\.[A-Za-z]+(?:[?#].*)?$)`：尺寸后面必须紧跟扩展名（可以再带 query / fragment）。
fn size_lookahead_ok(b: &[u8], mut i: usize) -> bool {
    if b.get(i) != Some(&b'.') {
        return false;
    }
    i += 1;
    let start = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == start {
        return false;
    }
    match b.get(i) {
        None => true,
        Some(&b'?') | Some(&b'#') => true,
        _ => false,
    }
}

/// 去掉 HTML 标签（`CoolapkHtml` 的 `TAG = Regex("<[^>]*>")`）。
fn strip_tags(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'<' {
            if let Some(gt) = s[i..].find('>') {
                i += gt + 1;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("非空");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 取属性值（`href` / `src` / `class`）：`name\s*=\s*("…"|'…')`，大小写不敏感。
fn attr(name: &str, attrs: &str) -> Option<String> {
    if attrs.is_empty() {
        return None;
    }
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find(name) {
        let p = from + pos;
        let mut j = p + name.len();
        while j < attrs.len() && attrs.as_bytes()[j].is_ascii_whitespace() {
            j += 1;
        }
        if attrs.as_bytes().get(j) == Some(&b'=') {
            j += 1;
            while j < attrs.len() && attrs.as_bytes()[j].is_ascii_whitespace() {
                j += 1;
            }
            match attrs.as_bytes().get(j) {
                Some(&b'"') => {
                    if let Some(end) = attrs[j + 1..].find('"') {
                        return Some(decode_entities(&attrs[j + 1..j + 1 + end]));
                    }
                }
                Some(&b'\'') => {
                    if let Some(end) = attrs[j + 1..].find('\'') {
                        return Some(decode_entities(&attrs[j + 1..j + 1 + end]));
                    }
                }
                _ => {}
            }
        }
        from = p + name.len();
        if from >= lower.len() {
            break;
        }
    }
    None
}

/// `&(#\d+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);` 的还原（表外的名字原样留着）。
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'&' {
            if let Some((end, text)) = decode_entity_at(s, i) {
                out.push_str(&text);
                i = end;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("非空");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 从 `s[i] == '&'` 处认一个实体；认得就返回 (结束位置, 替换文字)。
fn decode_entity_at(s: &str, i: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let mut j = i + 1;
    let body_start = j;
    if b.get(j) == Some(&b'#') {
        j += 1;
        let hex = matches!(b.get(j), Some(&b'x') | Some(&b'X'));
        if hex {
            j += 1;
            let start = j;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j == start || b.get(j) != Some(&b';') {
                return None;
            }
            let value = u32::from_str_radix(&s[start..j], 16).ok()?;
            let ch = char::from_u32(value)?;
            return Some((j + 1, ch.to_string()));
        }
        let start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == start || b.get(j) != Some(&b';') {
            return None;
        }
        let value = s[start..j].parse::<u32>().ok()?;
        let ch = char::from_u32(value)?;
        return Some((j + 1, ch.to_string()));
    }
    while j < b.len() && b[j].is_ascii_alphabetic() {
        j += 1;
    }
    if j == body_start || b.get(j) != Some(&b';') {
        return None;
    }
    let name = &s[body_start..j];
    let text = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => return None,
    };
    Some((j + 1, text.to_string()))
}

// ===========================================================================
// 详情 / 评论的正文 → `crate::detail::Block`（Kotlin `CoolapkHtml.toBlocks`）
// ===========================================================================

/// `CoolapkHtml.toBlocks`：正文 → 通用详情块（详情和评论共用）。
///
/// 和 [`to_blocks`] 用**同一个分词器**，区别只在 `<a>` 和文字的处理：
/// - `<a>` → [`DetailSpan::Mention`]（`/u/<数字>` 或 `feed-link-uname` 且目标是纯数字）/ [`DetailSpan::Link`]
///   （带 `target`，B站视频链接指向 B站 Source 的原生详情）；
/// - 文字里的表情 → [`DetailSpan::Emote`]。
pub fn to_detail_blocks(html: &str, table: &EmoteTable) -> Vec<DetailBlock> {
    let mut out: Vec<DetailBlock> = Vec::new();
    let mut spans: Vec<DetailSpan> = Vec::new();
    let mut i = 0usize;
    let mut cache = ScanCache::default();
    while i < html.len() {
        let Some(m) = match_token(html, i, &mut cache) else {
            i += 1;
            continue;
        };
        if m.tok.starts_with("<!--") {
            // 注释整段丢掉
        } else if let Some((attrs, inner)) = m.anchor {
            if let Some(span) = detail_link(attrs, inner) {
                spans.push(span);
            }
        } else if m.tok.len() > 1 && starts_with_ci(m.tok, 0, "<br") {
            flush_detail(&mut out, &mut spans);
        } else if starts_with_ci(m.tok, 0, "<img") {
            if let Some(src) = attr("src", m.img_attrs) {
                if !hosted::is_blank(&src) {
                    flush_detail(&mut out, &mut spans);
                    out.push(detail_image(&src));
                }
            }
        } else if m.tok.starts_with('<') && m.tok.len() > 1 {
            // 其他标签去掉，保留里面的文字
        } else {
            let text = decode_entities(m.tok);
            for (idx, line) in text.split('\n').enumerate() {
                if idx > 0 {
                    flush_detail(&mut out, &mut spans);
                }
                if !line.is_empty() {
                    spans.extend(emoji::spans(line, table).into_iter().map(detail_span));
                }
            }
        }
        i = m.end;
    }
    flush_detail(&mut out, &mut spans);
    out
}

/// 图片块（`CoolapkHtml.image`）：地址统一 https，顺便从地址里读宽高。
pub fn detail_image(url: &str) -> DetailBlock {
    let (width, height) = image_size(url);
    DetailBlock::Image {
        url: to_https(url),
        width,
        height,
        original: None,
        caption: None,
    }
}

fn detail_span(span: Span) -> DetailSpan {
    match span {
        Span::Text(text) | Span::Other(text) => DetailSpan::Text {
            text,
            bold: false,
            italic: false,
        },
        Span::Emote { text, url } => DetailSpan::Emote { text, url },
    }
}

fn flush_detail(out: &mut Vec<DetailBlock>, spans: &mut Vec<DetailSpan>) {
    let taken = std::mem::take(spans);
    if let Some(merged) = normalize_detail(taken) {
        out.push(DetailBlock::Paragraph { spans: merged });
    }
}

/// `CoolapkHtml.normalize`：合并相邻文字、去掉首尾空白；全是空白就整段丢掉。
fn normalize_detail(spans: Vec<DetailSpan>) -> Option<Vec<DetailSpan>> {
    let mut merged: Vec<DetailSpan> = Vec::new();
    for s in spans {
        match &s {
            DetailSpan::Text { text, .. } => {
                if let Some(DetailSpan::Text { text: prev, .. }) = merged.last_mut() {
                    prev.push_str(text);
                    continue;
                }
            }
            _ => {}
        }
        merged.push(s);
    }
    if let Some(DetailSpan::Text { text, .. }) = merged.first_mut() {
        *text = trim_java_start(text).to_string();
    }
    if let Some(DetailSpan::Text { text, .. }) = merged.last_mut() {
        *text = trim_java_end(text).to_string();
    }
    merged.retain(|s| !matches!(s, DetailSpan::Text { text, .. } if text.is_empty()));
    if merged.is_empty() {
        return None;
    }
    if merged
        .iter()
        .all(|s| matches!(s, DetailSpan::Text { text, .. } if hosted::is_blank(text)))
    {
        return None;
    }
    Some(merged)
}

fn trim_java_start(s: &str) -> &str {
    match s.char_indices().find(|(_, c)| !hosted::is_java_whitespace(*c)) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

fn trim_java_end(s: &str) -> &str {
    match s
        .char_indices()
        .rev()
        .find(|(_, c)| !hosted::is_java_whitespace(*c))
    {
        Some((i, c)) => &s[..i + c.len_utf8()],
        None => "",
    }
}

/// `<a …>inner</a>` → 一个 span。`CoolapkHtml.link` 的逐行移植：
/// `href` 为空（截断正文后面的"查看更多"）或文字全空白就丢掉。
fn detail_link(attrs: &str, inner: &str) -> Option<DetailSpan> {
    let text = decode_entities(&strip_tags(inner)).replace('\n', " ");
    let href = attr("href", attrs)
        .map(|h| hosted::kotlin_trim(&h).to_string())
        .unwrap_or_default();
    if href.is_empty() {
        return None;
    }
    if hosted::is_blank(&text) {
        return None;
    }
    let class = attr("class", attrs).unwrap_or_default();
    let user_path = user_path(&href);
    if user_path.is_some() || class.contains("feed-link-uname") {
        let target = match user_path {
            Some(p) => url_decode(&p),
            None => strip_at_prefix(&text).to_string(),
        };
        let name = strip_at_prefix(&text).to_string();
        if !target.is_empty() && target.bytes().all(|b| b.is_ascii_digit()) {
            return Some(DetailSpan::Mention {
                text,
                user: UserRef {
                    platform: super::COOLAPK.to_string(),
                    url: Some(format!("{}/u/{target}", super::WEB)),
                    id: target,
                    name,
                    avatar: None,
                },
            });
        }
        // 很多 @ 链接只给用户名、不给 uid：退化成普通链接（酷安网页 /u/ 后面也接受用户名）
        return Some(DetailSpan::Link {
            text,
            url: format!("{}/u/{}", super::WEB, url_encode(&target)),
            target: None,
        });
    }
    let url = absolute(&href);
    let target = super::video::bilibili_target(&url);
    Some(DetailSpan::Link { text, url, target })
}

/// `Regex("^(?:https?://(?:www\\.)?coolapk\\.com)?/u/([^/?#]+)")` 的第一个捕获组。
fn user_path(href: &str) -> Option<String> {
    const PREFIXES: [&str; 4] = [
        "https://coolapk.com",
        "https://www.coolapk.com",
        "http://coolapk.com",
        "http://www.coolapk.com",
    ];
    let rest = match href.strip_prefix("/u/") {
        Some(rest) => rest,
        None => {
            let tail = PREFIXES.iter().find_map(|p| href.strip_prefix(p))?;
            tail.strip_prefix("/u/")?
        }
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

/// `String.removePrefix("@")`。
fn strip_at_prefix(text: &str) -> &str {
    text.strip_prefix('@').unwrap_or(text)
}

/// `CoolapkHtml.absolute`：`//x` → `https://x`，`/x` → 网页根 + `/x`，其余原样。
fn absolute(href: &str) -> String {
    if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("{}{href}", super::WEB)
    } else {
        href.to_string()
    }
}

/// Java `URLDecoder.decode(s, UTF-8)`：`+` → 空格，`%XX` → 字节；坏的转义原样留着（见模块说明）。
pub fn url_decode(s: &str) -> String {
    if !s.contains('%') && !s.contains('+') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                match (hex_value(b[i + 1]), hex_value(b[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Java `URLEncoder.encode(s, UTF-8)` 再 `.replace("+", "%20")`：
/// 只留 `a-z A-Z 0-9 . - * _`，空格（Java 给 `+`）变成 `%20`，其余按 UTF-8 字节 `%XX`（大写）。
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        let c = byte as char;
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'*' | b'_') {
            out.push(c);
        } else if byte == b' ' {
            out.push_str("%20");
        } else {
            out.push('%');
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0f));
        }
    }
    out
}

fn hex_digit(v: u8) -> char {
    match v {
        0..=9 => (b'0' + v) as char,
        _ => (b'A' + (v - 10)) as char,
    }
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

