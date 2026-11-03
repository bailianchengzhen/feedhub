//! Reddit 的 markdown → 富文本 / 纯文本。
//!
//! 逐条对照 `source-reddit/RedditMarkdown.kt` 的 `toBlocks` / `inline` / `toPlain`：
//!
//! - [`to_blocks`]：markdown → [`crate::detail::Block`] / [`crate::detail::Span`]；
//! - [`to_plain`]：正文 → 一行纯文字（**信息流卡片摘要**用）。口径是 Kotlin 的
//!   `RichText(toBlocks(md)).textOnly()` + 空白压成一个 + trim，所以现在**真的是同一个函数**：
//!   先出块、再按 `textOnly` 拼，不再各写一套扫描器（以前是"只算文字长什么样"的平行实现）。
//!
//! 块的分法、行内元素的识别顺序、转义与 HTML 实体的还原顺序**全部照 Kotlin 抄**，
//! `RedditMarkdownTest` 的用例在 `reddit::markdown::tests` 里有同样的一份（期望值同一批），另外
//! `RedditDetailDualRunTest` 用真实 fixture 逐字段比 `FeedDetail` / `Comment`。
//!
//! 和 Kotlin 的已知差别（理由写在代码里）：
//! - 下标按**字符**算（Kotlin 的 `String` 下标是 UTF-16 单元）。行内构造里如果出现 emoji 这类
//!   补充平面字符，匹配的边界会和 Kotlin 差一个单元；正文 / 摘要里目前没有这种输入。
//! - Kotlin 的 `String.take(n)` 也按 UTF-16 单元，这里用 [`take_utf16`] 对齐（调用方用）。
//! - Kotlin 的 `take` 还能切出半个代理对，Rust 的 `String` 表示不了，遇到时整字丢掉（见 `source.rs`）。

use crate::detail::{Block, Span};
use crate::model::UserRef;

use super::REDDIT;

/// 网页根（`/u/某人` 的链接和 Kotlin `RedditMarkdown` 里写死的域名一样）。
const WWW: &str = "https://www.reddit.com";

/// MessageFormat 之外的那套 `\s`（Java 正则默认只认这 6 个，不是 Unicode 空白）。
pub fn is_java_regex_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// Kotlin `Char.isWhitespace()` = `Character.isWhitespace(c) || Character.isSpaceChar(c)`。
///
/// Kotlin 的 `String.trim()` / `isBlank()` / `trimStart()` 全走它，所以它比 Java 的
/// `Character.isWhitespace` **多** NBSP / U+2007 / U+202F 这几个 `Zs` 字符。
///
/// 等价写法：Unicode `White_Space`（= Rust 的 `char::is_whitespace`）∪ `U+001C..U+001F` ∪ `U+2007`。
pub fn is_java_ws(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1C}'..='\u{1F}' | '\u{2007}')
}

/// Kotlin `String.trim()`。
pub fn trim_java(s: &str) -> &str {
    s.trim_matches(is_java_ws)
}

/// Kotlin `String.isBlank()`。
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_java_ws)
}

/// Kotlin `String.take(n)`：按 **UTF-16 单元**截断（emoji 占 2 个）。
/// 术语说明：Rust 的 `char` 是标量值，Kotlin 的 `Char` 是 UTF-16 单元，两者在补充平面不同。
pub fn take_utf16(s: &str, n: usize) -> &str {
    let mut units = 0usize;
    for (idx, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..idx];
        }
    }
    s
}

/// `RedditMarkdown.toPlain`：markdown 记号去掉、多行压成一行；图片不写 "[图片]"。
///
/// 口径 = Kotlin 的 `RichText(toBlocks(md)).textOnly()` → `\s+` 压成一个空格 → `trim()`。
/// `\s+` 用的是 Java 正则那 6 个 ASCII 空白（`is_java_regex_ws`），**不是** Unicode 空白。
pub fn to_plain(markdown: &str) -> String {
    let text = text_only(&to_blocks(markdown));
    // Kotlin: `.replace(Regex("\\s+"), " ").trim()`
    let mut collapsed = String::with_capacity(text.len());
    let mut in_ws = false;
    for c in text.chars() {
        if is_java_regex_ws(c) {
            if !in_ws {
                collapsed.push(' ');
                in_ws = true;
            }
        } else {
            collapsed.push(c);
            in_ws = false;
        }
    }
    trim_java(&collapsed).to_string()
}

/// `RichText.textOnly()`（实现只有一份，在 `crate::detail`）。
pub fn text_only(blocks: &[Block]) -> String {
    crate::detail::text_only(blocks)
}

// ===========================================================================
// 块
// ===========================================================================

/// `RedditMarkdown.toBlocks`：markdown → 块（顺序、边界、优先级逐条照 Kotlin）。
pub fn to_blocks(markdown: &str) -> Vec<Block> {
    let text = markdown.replace("\r\n", "\n").replace('\r', "\n");
    if is_blank(&text) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Block> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = trim_java(line);
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        // 围栏代码块（``` 或 ~~~），语言写在开头围栏后面
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fence: String = trimmed.chars().take(3).collect();
            let language = {
                let rest = trim_java(trimmed.strip_prefix(&fence).unwrap_or(""));
                if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                }
            };
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !trim_java(lines[i]).starts_with(&fence) {
                body.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // 吃掉收尾围栏
            }
            out.push(Block::Code {
                text: body.join("\n"),
                language,
            });
            continue;
        }
        if let Some(heading) = heading(trimmed) {
            out.push(Block::Heading {
                spans: inline(&heading),
            });
            i += 1;
            continue;
        }
        if trimmed.starts_with('>') {
            let mut quoted: Vec<String> = Vec::new();
            while i < lines.len() && trim_java(lines[i]).starts_with('>') {
                let without = trim_java(lines[i]).strip_prefix('>').unwrap_or("");
                quoted.push(without.strip_prefix(' ').unwrap_or(without).to_string());
                i += 1;
            }
            out.push(Block::Quote {
                spans: inline(&quoted.join("\n")),
            });
            continue;
        }
        if is_hr(trimmed) {
            out.push(Block::Divider);
            i += 1;
            continue;
        }
        // 表格：分隔行（|---|---|）丢掉，其余按 " | " 拼成一行文字
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            let mut rows: Vec<String> = Vec::new();
            while i < lines.len()
                && trim_java(lines[i]).starts_with('|')
                && trim_java(lines[i]).ends_with('|')
            {
                let row = trim_java(lines[i]);
                if !table_sep(row) {
                    let cells: Vec<String> = trim_pipes(row)
                        .split('|')
                        .map(|c| trim_java(c).to_string())
                        .collect();
                    rows.push(cells.join(" | "));
                }
                i += 1;
            }
            if !rows.is_empty() {
                out.push(Block::Paragraph {
                    spans: inline(&rows.join("\n")),
                });
            }
            continue;
        }
        if let Some(item) = list_item(line) {
            let prefix = if item.ordered.is_empty() {
                "• ".to_string()
            } else {
                format!("{} ", item.ordered)
            };
            let mut spans = vec![Span::Text {
                text: prefix,
                bold: false,
                italic: false,
            }];
            spans.extend(inline(&item.rest));
            out.push(Block::Paragraph { spans });
            i += 1;
            continue;
        }
        // 整段就是一张图
        if let Some(img) = image_only(trimmed) {
            out.push(Block::Image {
                url: img.url,
                width: None,
                height: None,
                original: None,
                caption: Some(img.label).filter(|l| !is_blank(l)),
            });
            i += 1;
            continue;
        }
        // 段落：一行行攒到空行 / 下一个结构为止（`t` 是 trim 过的行）
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() {
            let t = trim_java(lines[i]);
            if t.is_empty()
                || t.starts_with("```")
                || t.starts_with("~~~")
                || t.starts_with('>')
                || heading(t).is_some()
                || is_hr(t)
                || list_item(lines[i]).is_some()
                || image_only(t).is_some()
                || (t.starts_with('|') && t.ends_with('|'))
            {
                break;
            }
            para.push(t);
            i += 1;
        }
        if !para.is_empty() {
            out.push(Block::Paragraph {
                spans: inline(&para.join("\n")),
            });
        } else {
            // 上面所有分支都覆盖了 break 条件，走不到这里；防万一（死循环比少一块更糟）
            i += 1;
        }
    }
    out
}

/// `^(#{1,6})\s+(.*)$` → group 2。
fn heading(trimmed: &str) -> Option<String> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.starts_with(is_java_regex_ws) {
        return None;
    }
    Some(rest.trim_start_matches(is_java_regex_ws).to_string())
}

/// `^(-{3,}|\*{3,}|_{3,})$`
fn is_hr(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    trimmed.chars().count() >= 3 && trimmed.chars().all(|c| c == first)
}

/// `^\|[\s:|-]+\|$`
fn table_sep(trimmed: &str) -> bool {
    let inner = trimmed.strip_prefix('|').and_then(|s| s.strip_suffix('|'));
    let Some(inner) = inner else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|c| is_java_regex_ws(c) || matches!(c, ':' | '|' | '-'))
}

/// Kotlin `String.trim('|')`。
fn trim_pipes(row: &str) -> &str {
    row.trim_matches('|')
}

struct ListItem {
    /// 有序列表的记号（`1.` / `2)`），无序是空串。
    ordered: String,
    rest: String,
}

/// `^(\s*)(?:([-*+])|(\d+[.)]))\s+(.*)$`（作用在**没 trim** 的整行上）。
fn list_item(line: &str) -> Option<ListItem> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() && is_java_regex_ws(chars[i]) {
        i += 1;
    }
    let mut ordered = String::new();
    match chars.get(i) {
        Some('-') | Some('*') | Some('+') => i += 1,
        None => return None,
        _ => {
            // \d+[.)]
            let digits_start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i == digits_start {
                return None;
            }
            match chars.get(i) {
                Some('.') | Some(')') => i += 1,
                _ => return None,
            }
            ordered = chars[digits_start..i].iter().collect();
        }
    }
    if i >= chars.len() || !is_java_regex_ws(chars[i]) {
        return None;
    }
    while i < chars.len() && is_java_regex_ws(chars[i]) {
        i += 1;
    }
    Some(ListItem {
        ordered,
        rest: chars[i..].iter().collect(),
    })
}

// ===========================================================================
// 行内
// ===========================================================================

/// markdown 里 `\*` 这样的转义集合。
const ESCAPABLE: &str = "\\`*_{}[]()#+-.!~|>";

/// `RedditMarkdown.inline`：一段文字 → 行内片段（顺序、优先级、吃掉的字符数都照 Kotlin）。
fn inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if let Some((span, len)) = match_inline(&chars, i) {
            if !plain.is_empty() {
                out.push(Span::Text {
                    text: std::mem::take(&mut plain),
                    bold: false,
                    italic: false,
                });
            }
            out.push(span);
            i += len;
            continue;
        }
        // 反斜杠转义：\* 就是字面量 *
        if chars[i] == '\\' && i + 1 < chars.len() && ESCAPABLE.contains(chars[i + 1]) {
            plain.push(chars[i + 1]);
            i += 2;
            continue;
        }
        plain.push(chars[i]);
        i += 1;
    }
    if !plain.is_empty() {
        out.push(Span::Text {
            text: plain,
            bold: false,
            italic: false,
        });
    }
    // Kotlin: `decodeEntities(out)` —— 每个 span 的**文字**过实体还原，链接地址不动
    out.iter().map(decode_span_entities).collect()
}

/// 在 `at` 位置匹配一个行内元素，返回（Span，吃掉的字符数）。顺序和 Kotlin 一样。
fn match_inline(chars: &[char], at: usize) -> Option<(Span, usize)> {
    if let Some(m) = bracket_link(chars, at, true) {
        let text = if is_blank(&m.label) {
            "图片".to_string()
        } else {
            m.label.clone()
        };
        return Some((
            Span::Link {
                text,
                url: m.url.clone(),
                target: None,
            },
            m.len,
        ));
    }
    if let Some(m) = bracket_link(chars, at, false) {
        return Some((
            Span::Link {
                text: m.label.clone(),
                url: m.url.clone(),
                target: None,
            },
            m.len,
        ));
    }
    if let Some((url, len)) = autolink(chars, at) {
        return Some((
            Span::Link {
                text: url.clone(),
                url,
                target: None,
            },
            len,
        ));
    }
    if let Some((inner, len)) = delimited(chars, at, "**", "**", true) {
        return Some((
            Span::Text {
                text: unescape(&inner),
                bold: true,
                italic: false,
            },
            len,
        ));
    }
    if let Some((inner, len)) = delimited(chars, at, "*", "*", true) {
        return Some((
            Span::Text {
                text: unescape(&inner),
                bold: false,
                italic: true,
            },
            len,
        ));
    }
    if let Some((inner, len)) = italic_underscore(chars, at) {
        return Some((
            Span::Text {
                text: unescape(&inner),
                bold: false,
                italic: true,
            },
            len,
        ));
    }
    if let Some((inner, len)) = delimited(chars, at, "~~", "~~", true) {
        // 模型里没有删除线样式 → 普通文字
        return Some((
            Span::Text {
                text: unescape(&inner),
                bold: false,
                italic: false,
            },
            len,
        ));
    }
    if let Some((inner, len)) = delimited(chars, at, "`", "`", true) {
        return Some((
            Span::Text {
                text: inner,
                bold: false,
                italic: false,
            },
            len,
        ));
    }
    let prev = if at == 0 { ' ' } else { chars[at - 1] };
    if !prev.is_alphanumeric() && prev != '_' {
        if let Some((name, len)) = prefixed_name(chars, at, 'u', true) {
            return Some((
                Span::Mention {
                    text: format!("/u/{name}"),
                    user: UserRef {
                        platform: REDDIT.to_string(),
                        id: name.clone(),
                        name: name.clone(),
                        avatar: None,
                        url: Some(format!("{WWW}/user/{name}")),
                    },
                },
                len,
            ));
        }
        if let Some((name, len)) = prefixed_name(chars, at, 'r', false) {
            return Some((
                Span::Link {
                    text: format!("/r/{name}"),
                    url: format!("{WWW}/r/{name}"),
                    target: None,
                },
                len,
            ));
        }
    }
    if let Some((url, len)) = bare_url(chars, at) {
        return Some((
            Span::Link {
                text: url.clone(),
                url,
                target: None,
            },
            len,
        ));
    }
    None
}

/// `!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)$`：整段（trim 之后）只有一张图。
fn image_only(trimmed: &str) -> Option<Bracket> {
    let chars: Vec<char> = trimmed.chars().collect();
    match bracket_link(&chars, 0, true) {
        Some(b) if b.len == chars.len() => Some(b),
        _ => None,
    }
}

struct Bracket {
    label: String,
    url: String,
    len: usize,
}

/// `!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)`（image=true）/ `\[([^\]]+)\]\(…`（image=false）。
fn bracket_link(chars: &[char], at: usize, image: bool) -> Option<Bracket> {
    let mut j = at;
    if image {
        if chars.get(j) != Some(&'!') {
            return None;
        }
        j += 1;
    }
    if chars.get(j) != Some(&'[') {
        return None;
    }
    j += 1;
    let label_start = j;
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    // IMAGE 的 alt 是 `[^\]]*`（可以空），LINK 的文字是 `[^\]]+`（至少一个）
    if !image && j == label_start {
        return None;
    }
    let label: String = chars[label_start..j].iter().collect();
    j += 1; // ']'
    if chars.get(j) != Some(&'(') {
        return None;
    }
    j += 1;
    let url_start = j;
    while j < chars.len() && chars[j] != ')' && !is_java_regex_ws(chars[j]) {
        j += 1;
    }
    if j == url_start {
        return None;
    }
    let url: String = chars[url_start..j].iter().collect();
    if j < chars.len() && is_java_regex_ws(chars[j]) {
        // 可选 `\s+"[^"]*"` 再接 ')'
        let mut k = j;
        while k < chars.len() && is_java_regex_ws(chars[k]) {
            k += 1;
        }
        if chars.get(k) != Some(&'"') {
            return None;
        }
        k += 1;
        while k < chars.len() && chars[k] != '"' {
            k += 1;
        }
        if k >= chars.len() {
            return None;
        }
        k += 1;
        if chars.get(k) != Some(&')') {
            return None;
        }
        return Some(Bracket {
            label,
            url,
            len: k + 1 - at,
        });
    }
    if chars.get(j) != Some(&')') {
        return None;
    }
    Some(Bracket {
        label,
        url,
        len: j + 1 - at,
    })
}

/// `<(https?://[^>\s]+)>`
fn autolink(chars: &[char], at: usize) -> Option<(String, usize)> {
    if chars.get(at) != Some(&'<') {
        return None;
    }
    // 组 1 是**带 scheme** 的整段（`<(https?://[^>\s]+)>`）
    let start = at + 1;
    let scheme_len = if starts_with(chars, start, "https://") {
        8
    } else if starts_with(chars, start, "http://") {
        7
    } else {
        return None;
    };
    let mut j = start + scheme_len;
    while j < chars.len() && chars[j] != '>' && !is_java_regex_ws(chars[j]) {
        j += 1;
    }
    if j == start || chars.get(j) != Some(&'>') {
        return None;
    }
    Some((chars[start..j].iter().collect(), j + 1 - at))
}

fn starts_with(chars: &[char], at: usize, pat: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    at + pat.len() <= chars.len() && chars[at..at + pat.len()] == pat[..]
}

/// `open(.+?)close`：内容里的 `.` 不跨行（Kotlin 的 `.` 不匹配 `\n`）。
fn delimited(chars: &[char], at: usize, open: &str, close: &str, _nonempty: bool) -> Option<(String, usize)> {
    if !starts_with(chars, at, open) {
        return None;
    }
    let open_len = open.chars().count();
    let close: Vec<char> = close.chars().collect();
    let content_start = at + open_len;
    let mut k = content_start;
    while k < chars.len() {
        if chars[k] == '\n' {
            return None;
        }
        if chars[k..].starts_with(&close) && k > content_start {
            return Some((chars[content_start..k].iter().collect(), k + close.len() - at));
        }
        k += 1;
    }
    None
}

/// `(?<![A-Za-z0-9_])_([^_\n]+?)_(?![A-Za-z0-9_])`
fn italic_underscore(chars: &[char], at: usize) -> Option<(String, usize)> {
    if chars.get(at) != Some(&'_') {
        return None;
    }
    if at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_') {
        return None;
    }
    let start = at + 1;
    let mut k = start;
    while k < chars.len() && chars[k] != '_' && chars[k] != '\n' {
        k += 1;
    }
    if k >= chars.len() || chars[k] != '_' || k == start {
        return None;
    }
    if k + 1 < chars.len() && (chars[k + 1].is_alphanumeric() || chars[k + 1] == '_') {
        return None;
    }
    Some((chars[start..k].iter().collect(), k + 1 - at))
}

/// `/?u/([A-Za-z0-9_-]{1,32})` / `/?r/([A-Za-z0-9_]{1,32})`：返回（名字，吃掉的字符数）。
fn prefixed_name(chars: &[char], at: usize, kind: char, dash: bool) -> Option<(String, usize)> {
    let slash = chars.get(at) == Some(&'/');
    let mut j = if slash { at + 1 } else { at };
    if chars.get(j) != Some(&kind) || chars.get(j + 1) != Some(&'/') {
        return None;
    }
    j += 2;
    let start = j;
    while j < chars.len()
        && j - start < 32
        && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || (dash && chars[j] == '-'))
    {
        j += 1;
    }
    if j == start {
        return None;
    }
    Some((chars[start..j].iter().collect(), j - at))
}

/// `https?://[^\s<>\[\]()]+`，末尾的 `.,;:!?)]` 去掉（吃掉的长度按去掉之后的算）。
fn bare_url(chars: &[char], at: usize) -> Option<(String, usize)> {
    if !starts_with(chars, at, "http://") && !starts_with(chars, at, "https://") {
        return None;
    }
    let scheme_len = if starts_with(chars, at, "https://") { 8 } else { 7 };
    let mut j = at + scheme_len;
    let start = at;
    while j < chars.len()
        && !is_java_regex_ws(chars[j])
        && !matches!(chars[j], '<' | '>' | '[' | ']' | '(' | ')')
    {
        j += 1;
    }
    let mut end = j;
    while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']') {
        end -= 1;
    }
    // `[^\s<>\[\]()]+` 至少一个字符，所以 "http://" 本身不算链接
    if end <= at + scheme_len {
        return None;
    }
    Some((chars[start..end].iter().collect(), end - start))
}

/// markdown 里 `\*` 这样的转义（`Regex("\\\\([\\\\`*_{}\\[\\]()#+\\-.!~|>])")`）。
fn unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && ESCAPABLE.contains(chars[i + 1]) {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// HTML 实体：命名实体**按 Kotlin 的插入顺序**替换，再还原数字实体（顺序会改变结果：
/// `&amp;lt;` 先还原 `&amp;` 就停在 `&lt;`，不会变成 `<`）。
fn decode_entities(text: &str) -> String {
    const NAMED: [(&str, &str); 9] = [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&amp;", "&"),
    ];
    let mut out = text.to_string();
    for (from, to) in NAMED {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    decode_numeric_entities(&out)
}

/// `decodeEntities(spans)`：每个 span 的**文字**过实体还原（链接地址 / 表情地址不动）。
fn decode_span_entities(span: &Span) -> Span {
    match span {
        Span::Text {
            text,
            bold,
            italic,
        } => Span::Text {
            text: decode_entities(text),
            bold: *bold,
            italic: *italic,
        },
        Span::Link { text, url, target } => Span::Link {
            text: decode_entities(text),
            url: url.clone(),
            target: target.clone(),
        },
        Span::Mention { text, user } => Span::Mention {
            text: decode_entities(text),
            user: user.clone(),
        },
        Span::Emote { text, url } => Span::Emote {
            text: decode_entities(text),
            url: url.clone(),
        },
    }
}

/// `&#(x?[0-9A-Fa-f]+);` → 对应字符；非法码点（0 / 越界 / 代理区）变成 U+FFFD。
fn decode_numeric_entities(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '#' && i > 0 && chars[i - 1] == '&' {
            // 从 '&#" 开始找 ';'
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
            // `x?` 前缀：`&#x2F;` / `&#X2F;`
            let mut hex = false;
            let mut digits_start = i + 1;
            if digits_start < chars.len() && (chars[digits_start] == 'x' || chars[digits_start] == 'X') {
                hex = true;
                digits_start += 1;
                j = digits_start;
                while j < chars.len() && chars[j].is_ascii_hexdigit() {
                    j += 1;
                }
            }
            if j > digits_start && chars.get(j) == Some(&';') {
                let code: String = chars[digits_start..j].iter().collect();
                let cp = if hex {
                    u32::from_str_radix(&code, 16).ok()
                } else {
                    code.parse::<u32>().ok()
                };
                // 前一个字符的 '&' 已经推进 out 了，这里要收回来（整个 `&#…;` 才算一个整体）
                out.pop();
                match cp {
                    Some(cp) if cp > 0 && cp <= 0x10FFFF && !(0xD800..=0xDFFF).contains(&cp) => {
                        if let Some(c) = char::from_u32(cp) {
                            out.push(c);
                        } else {
                            out.push('\u{FFFD}');
                        }
                    }
                    _ => out.push('\u{FFFD}'),
                }
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

