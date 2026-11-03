//! B站文字 → `Span` / `Block` 的公共小工具，逐条对照 `source-bilibili/BiliText.kt`（还有它下面的
//! `BiliLinks`）：评论、视频简介、动态正文都用这一套。
//!
//! 做法和 Kotlin 一样："先找出要替换的片段（表情、@、链接、BV 号），再把剩下的当普通文字"，
//! 所以和 B站各接口的结构无关。
//!
//! 三处**必须逐字节复刻 Kotlin**的地方：
//!
//! 1. **下标是 UTF-16 码元**（Kotlin `String.indexOf` / `substring` 的口径）。emoji 这类补充平面字符各占两个
//!    单元，用 Rust 的 `char` 下标算长度 / 排序会差一位。这里把文本转成 `Vec<u16>` 再找子串；
//!    万一在代理对中间切开，用 `from_utf16_lossy` 收尾（Kotlin 会留一个孤立代理项，Rust 表示不了）。
//! 2. **没有 `regex` crate**（离线缓存里没有，加依赖要改 `Cargo.lock` 联网），两条正则
//!    （网址、BV 号）是手写扫描器，语义按 Kotlin `Regex.findAll` 对着写：左到右、不重叠。
//! 3. **Java 的空白口径**（`String.trim` / `Char.isWhitespace`）：用 [`crate::hosted::is_java_whitespace`]，
//!    不是 Rust 的 `char::is_whitespace`（后者含 NBSP 类）。

use crate::detail::{Block, Span};
use crate::hosted::{is_blank, is_java_whitespace};
use crate::json::to_https;
use crate::model::{DetailTarget, UserRef, BILI};

use super::ids;

/// 一个要换成特殊 [`Span`] 的片段。[`once`] 为 true 时只换第一处（评论关键词链接的 `match_once`）。
#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub span: Span,
    pub once: bool,
}

impl Token {
    pub fn new(text: impl Into<String>, span: Span) -> Token {
        Token {
            text: text.into(),
            span,
            once: false,
        }
    }

    pub fn once(text: impl Into<String>, span: Span) -> Token {
        Token {
            text: text.into(),
            span,
            once: true,
        }
    }
}

struct Match {
    start: usize,
    end: usize,
    span: Span,
}

/// `\r\n`、单独的 `\r` 都当 `\n`（评论里真有只用 `\r` 换行的）。
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 按 [`tokens`] 把 [`text`] 切成 Span。重叠时先出现的、长的优先（Kotlin 的
/// `compareBy { start }.thenByDescending { end - start }`，**稳定排序**）。
pub fn spans(text: &str, tokens: &[Token]) -> Vec<Span> {
    let hay: Vec<u16> = text.encode_utf16().collect();
    let mut matches: Vec<Match> = Vec::new();
    for token in tokens {
        if token.text.is_empty() {
            continue;
        }
        let needle: Vec<u16> = token.text.encode_utf16().collect();
        let mut from = 0usize;
        while from + needle.len() <= hay.len() {
            match find_from(&hay, &needle, from) {
                Some(i) => {
                    matches.push(Match {
                        start: i,
                        end: i + needle.len(),
                        span: token.span.clone(),
                    });
                    if token.once {
                        break;
                    }
                    from = i + needle.len();
                }
                None => break,
            }
        }
    }
    matches.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| (b.end - b.start).cmp(&(a.end - a.start)))
    });
    let mut out: Vec<Span> = Vec::new();
    let mut pos = 0usize;
    for m in matches {
        if m.start < pos {
            continue;
        }
        if m.start > pos {
            out.push(Span::Text {
                text: String::from_utf16_lossy(&hay[pos..m.start]),
                bold: false,
                italic: false,
            });
        }
        out.push(m.span);
        pos = m.end;
    }
    if pos < hay.len() {
        out.push(Span::Text {
            text: String::from_utf16_lossy(&hay[pos..]),
            bold: false,
            italic: false,
        });
    }
    out
}

/// 朴素的 UTF-16 子串查找（从 [`from`] 开始）。
fn find_from(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

/// 一串 Span 按 `\n` 分段；只有空白的段丢掉（Kotlin `BiliText.paragraphs`）。
pub fn paragraphs(spans: &[Span]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();

    fn flush(cur: &mut Vec<Span>, out: &mut Vec<Block>) {
        let keep = cur.iter().any(|s| match s {
            Span::Text { text, .. } => !is_blank(text),
            _ => true,
        });
        if keep {
            out.push(Block::Paragraph {
                spans: std::mem::take(cur),
            });
        } else {
            cur.clear();
        }
    }

    for span in spans {
        match span {
            Span::Text { text, .. } if text.contains('\n') => {
                let pieces: Vec<&str> = text.split('\n').collect();
                for (i, piece) in pieces.iter().enumerate() {
                    if i > 0 {
                        flush(&mut cur, &mut out);
                    }
                    if !piece.is_empty() {
                        cur.push(Span::Text {
                            text: (*piece).to_string(),
                            bold: match span {
                                Span::Text { bold, .. } => *bold,
                                _ => false,
                            },
                            italic: match span {
                                Span::Text { italic, .. } => *italic,
                                _ => false,
                            },
                        });
                    }
                }
            }
            other => cur.push(other.clone()),
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// 段落块里的 spans（Kotlin 里是 `it.spans`）。
pub fn block_spans(block: &Block) -> Option<&[Span]> {
    match block {
        Block::Paragraph { spans } | Block::Heading { spans } | Block::Quote { spans } => Some(spans),
        _ => None,
    }
}

/// 网址正则 `https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+` 的字符集。
fn is_url_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

/// 网址结尾要剪掉的标点（Kotlin `trimEnd('.', ',', ';', ':', '!', '?', ')', ']', '\'')`）。
fn is_url_tail(b: u8) -> bool {
    matches!(b, b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'\'')
}

/// `BV1[1-9A-HJ-NP-Za-km-z]{9}` 里第 4 个字符起的字符集。
fn is_bv_char(b: u8) -> bool {
    matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
}

/// 简介 / 正文里自动识别的链接：**先找网址、再找 BV 号**，按文本去重（Kotlin `linkTokens`）。
pub fn link_tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let prefix = if bytes[i..].starts_with(b"https://") {
            8
        } else if bytes[i..].starts_with(b"http://") {
            7
        } else {
            0
        };
        if prefix == 0 {
            i += 1;
            continue;
        }
        let mut j = i + prefix;
        while j < bytes.len() && is_url_char(bytes[j]) {
            j += 1;
        }
        if j == i + prefix {
            // `http://` 后面紧跟不能出现在网址里的字符：正则引擎会从 i+1 继续找
            i += 1;
            continue;
        }
        // 多字节字符不会出现在 ASCII 网址里：j 一定落在字符边界上
        let raw = &text[i..j];
        let url = raw.trim_end_matches(|c: char| is_url_tail(c as u8) && c.is_ascii());
        out.push(Token::new(
            url,
            Span::Link {
                text: url.to_string(),
                url: url.to_string(),
                target: target(url),
            },
        ));
        i = j;
    }
    // BV 号
    let mut i = 0usize;
    while i + 12 <= bytes.len() {
        let window = &bytes[i..i + 12];
        if window.starts_with(b"BV1") && window[3..].iter().all(|b| is_bv_char(*b)) {
            let bvid = &text[i..i + 12];
            let url = super::video_url(bvid);
            out.push(Token::new(
                bvid,
                Span::Link {
                    text: bvid.to_string(),
                    url,
                    target: Some(DetailTarget::Native {
                        id: bvid.to_string(),
                        platform: None,
                    }),
                },
            ));
            i += 12;
        } else {
            i += 1;
        }
    }
    // Kotlin `distinctBy { it.text }`：同一个片段只留第一次出现
    let mut seen: Vec<String> = Vec::new();
    out.retain(|t| {
        if seen.iter().any(|s| s == &t.text) {
            false
        } else {
            seen.push(t.text.clone());
            true
        }
    });
    out
}

// ===========================================================================
// 站内链接 → 能原生打开的目标（`BiliText.kt` 下面的 `BiliLinks`）
// ===========================================================================

/// 一个够用的 `HttpUrl` 子集：host 小写、`encoded_path`、query 原样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub host: String,
    pub encoded_path: String,
    pub query: Option<String>,
}

/// OkHttp `toHttpUrlOrNull()` 的近似（只认 http / https；scheme / host 大小写不敏感；
/// 没有 path 时是 `/`；`#` 之后是 fragment）。B站 这几个域名用不到 userinfo / IPv6 / 端口之外的怪写法。
pub fn parse_http_url(raw: &str) -> Option<HttpUrl> {
    let lower = raw.to_ascii_lowercase();
    let rest = if lower.starts_with("http://") {
        &raw["http://".len()..]
    } else if lower.starts_with("https://") {
        &raw["https://".len()..]
    } else {
        return None;
    };
    let end_authority = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..end_authority];
    let after = &rest[end_authority..];
    // userinfo 丢掉（OkHttp 有，但 B站 链接里没有）
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        // IPv6
        let close = stripped.find(']')?;
        stripped[..close].to_string()
    } else {
        match host_port.rsplit_once(':') {
            Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
            _ => host_port.to_string(),
        }
    };
    if host.is_empty() || host.chars().any(|c| c == ' ' || c == '\t') {
        return None;
    }
    let (path_and_query, _fragment) = match after.find('#') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    let (path, query) = match path_and_query.find('?') {
        Some(i) => (&path_and_query[..i], Some(path_and_query[i + 1..].to_string())),
        None => (path_and_query, None),
    };
    Some(HttpUrl {
        host: host.to_ascii_lowercase(),
        encoded_path: if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        },
        query,
    })
}

/// 站内链接 → 能原生打开的目标：视频页（BV / av）、图文页 `/opus/ID`、动态页 `t.bilibili.com/ID`。
pub fn target(url: &str) -> Option<DetailTarget> {
    let u = parse_http_url(url)?;
    let path = u.encoded_path.as_str();
    if u.host == "t.bilibili.com"
        || (u.host == "m.bilibili.com" && is_dynamic_path(path))
    {
        return dynamic_id_from(path).map(native_dynamic);
    }
    if u.host != "www.bilibili.com" && u.host != "bilibili.com" && u.host != "m.bilibili.com" {
        return None;
    }
    if let Some(id) = opus_id_from(path) {
        return Some(native_dynamic(id));
    }
    if let Some(bvid) = video_id_from(path) {
        return Some(DetailTarget::Native {
            id: bvid,
            platform: None,
        });
    }
    None
}

fn native_dynamic(id: String) -> DetailTarget {
    DetailTarget::Native {
        id: ids::dynamic_native_id(&id),
        platform: None,
    }
}

/// `/(\d{10,})` 整串匹配。
fn is_dynamic_path(path: &str) -> bool {
    path.strip_prefix('/')
        .map(|rest| rest.len() >= 10 && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn dynamic_id_from(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/')?;
    if rest.len() >= 10 && rest.chars().all(|c| c.is_ascii_digit()) {
        Some(rest.to_string())
    } else {
        None
    }
}

/// `/opus/(\d+)` 的 `find`（不要求整串）。
fn opus_id_from(path: &str) -> Option<String> {
    let mut rest = path;
    while let Some(i) = rest.find("/opus/") {
        let tail = &rest[i + "/opus/".len()..];
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return Some(digits);
        }
        rest = &rest[i + "/opus/".len()..];
    }
    None
}

/// `/video/(BV1[1-9A-HJ-NP-Za-km-z]{9}|av(\d+))`，`IGNORE_CASE`；av 号换算成 BV。
fn video_id_from(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let i = lower.find("/video/")?;
    let tail = &path[i + "/video/".len()..];
    // BV 号
    if tail.len() >= 12 && ids::is_bvid(&tail[..12]) {
        return Some(tail[..12].to_string());
    }
    // av 号（大小写不敏感）
    let tail_lower = &tail.to_ascii_lowercase();
    if let Some(rest) = tail_lower.strip_prefix("av") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse::<i64>().ok().and_then(ids::to_bvid);
        }
    }
    None
}

// ===========================================================================
// 文字工具（Java 口径）
// ===========================================================================

/// Kotlin `String.trimStart()`（Java 空白口径）。
pub fn trim_start(s: &str) -> &str {
    s.char_indices()
        .find(|(_, c)| !is_java_whitespace(*c))
        .map(|(i, _)| &s[i..])
        .unwrap_or("")
}

/// Kotlin `String.trimEnd()`（Java 空白口径）。
pub fn trim_end(s: &str) -> &str {
    match s
        .char_indices()
        .rev()
        .find(|(_, c)| !is_java_whitespace(*c))
    {
        Some((i, c)) => &s[..i + c.len_utf8()],
        None => "",
    }
}

/// Java `URLEncoder.encode(s, "UTF-8")`：只留 `A-Z a-z 0-9 . - * _`，空格是 `+`，其余 `%XX`（大写）。
pub fn java_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'*' | b'_');
        if keep {
            out.push(*b as char);
        } else if *b == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// OkHttp `HttpUrl.queryParameter(name)`：解码后的名字比较，取**第一个**；`+` 是字面加号（不是空格）。
pub fn query_parameter(url: &str, name: &str) -> Option<String> {
    let u = parse_http_url(url)?;
    let query = u.query?;
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        if percent_decode(k) == name {
            return Some(percent_decode(v));
        }
    }
    None
}

/// OkHttp 的百分号解码（UTF-8；`+` 原样，不是空格）。坏转义原样保留。
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// B站 把专栏正文里的代码 **HTML 转义过**，引号还转了两道（`&amp;#39;`），所以 Kotlin 那边
/// `repeat(3) { jsoup Parser.unescapeEntities(...) }` 一直解到不再变化。
///
/// 实体解码
/// 搬到共用的 [`crate::html::unescape_once`]（那里多了"属性值"的规则），这里只剩"解三轮"这一层。
/// 对拍（`BiliTextEntityDualRunTest` 的全表 2231 条）守着语义没变。
pub fn unescape_entities(s: &str) -> String {
    crate::html::unescape_entities_looped(s)
}

/// 评论里被 @ 到的用户表（`content.at_name_to_mid` + `content.members`）。
/// 名字 → 用户；同名时**后面的覆盖前面的**（Kotlin 的 `users[name] = it`），保持第一次出现的位置。
pub fn merge_user(table: &mut Vec<(String, UserRef)>, name: &str, user: Option<UserRef>) {
    let Some(user) = user else { return };
    if let Some(slot) = table.iter_mut().find(|(n, _)| n == name) {
        slot.1 = user;
    } else {
        table.push((name.to_string(), user));
    }
}

/// 评论里 `@名字` 用的 UserRef（头像给不了）。
pub fn mention_user(mid: String, name: String) -> UserRef {
    UserRef {
        platform: BILI.to_string(),
        id: mid.clone(),
        name,
        avatar: None,
        url: Some(super::space_url(&mid)),
    }
}

/// `toHttps` 的别名，让解析代码读起来和 Kotlin 一样。
pub fn https(u: &str) -> String {
    to_https(u)
}

