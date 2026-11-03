//! **平台层（"请求怎么发"那半边）的公共小工具**。
//!
//! 用的是"共用小工具照抄到自己的 `source.rs`"，
//! 现在合完了，口径**逐字节相同**的那几份收在这里；口径不同的留在各自模块，下面写清楚为什么不合。
//!
//! ## 合进来的
//!
//! | 名字 | 口径 | 原来在哪 |
//! |---|---|---|
//! | [`query_encode`] | OkHttp 4.12 `HttpUrl.addQueryParameter`（只留 `* - . 0-9 A-Z _ a-z`） | `bili` / `coolapk` / `heybox` / `reddit` / `x` 的 `source.rs` 各一份 |
//! | [`okhttp_canonical_query`] | OkHttp 4.12 解析完整 URL 时对 query 的规范化（签名输入） | `douyin::source::okhttp_canonical_query` / `zhihu::source::canonical_query` |
//! | [`json_string`] / [`push_json_string`] | kotlinx.serialization 的字符串转义（`\u00xx` **小写**） | `x::source::json_string` / `heybox::source::push_json_string` / `xhs::codec::json_string` |
//! | [`is_java_whitespace`] / [`is_blank`] / [`kotlin_trim`] | Java `Character.isWhitespace`（**不含** U+00A0 / U+2007 / U+202F / U+0085，**含** U+001C..U+001F） | `heybox::mod` / `tieba::mod` |
//! | [`md5_hex`] / [`md5_hex_bytes`] | MD5 的小写十六进制 | 七家的签名 / token 模块各一份 |
//! | [`header_value`] / [`has_header`] | 响应头查找（名字大小写不敏感） | `douyin` / `reddit` / `tiktok` / `zhihu` / `x` 里各写了一遍 `iter().find(...)` |
//! | [`civil_from_days`] | 纪元日 → (年, 月, 日) | `bili::source` / `tieba::source` |
//!
//! ## 故意**没**合的（口径不一样，合了就是行为变更）
//!
//! - `tiktok::sign::rfc3986_query`：TikTok 的 query 顺序和编码是抓包定的（RFC 3986 + 键名 ASCII 升序），
//!   和 OkHttp 的 `addQueryParameter` 不是一套集合（比如 `~` 一个留一个编）。
//! - `douyin::sign::canonical_query`：`a_bogus` 签名**自己**的规范化（先按 `&` 切、再逐段处理），
//!   和 [`okhttp_canonical_query`]（OkHttp 解析 URL 的口径）是两件事，只是名字像。
//! - `youtube::source::push_quoted`：`\u00XX` 用**大写**十六进制（InnerTube body 的既有字节），
//!   和 [`json_string`] 的小写不同；YouTube 的 body 里目前没有控制字符，但字节口径不同就不合。
//! - `reddit::markdown::is_blank` / `youtube::mod::is_blank`：这两份把 Rust 的 `char::is_whitespace`
//!   去掉了 NBSP 类，但**多了 U+0085**（`youtube` 那份还漏了 U+001C..U+001F），和 Java 的
//!   `Character.isWhitespace` 不完全一样。改成 [`is_blank`] 是行为变更（虽然只在含 U+0085 的文本上看得出来），
//!   这轮是纯重构，不动它们；哪天要统一，先各加一条 `\u{85}` 的断言再改。
//! - `coolapk::source::non_empty` / `douyin::source::pair`：只有一家在用，不算"照抄"。
//! - 各家的 `check_status` / `check_transport` / `From<HostError>`：错误码表和平台错误类型都不一样
//!   （B站 是 `{code}`、Reddit 有 `x-ratelimit-*`、YouTube 是 429/403/412……），只共用 [`header_value`] 这种零件。

use md5::{Digest, Md5};

// ===========================================================================
// 内部伪请求头：请求优先级
// ===========================================================================

/// **内部伪请求头**：表达"这次是用户正等着的请求"（Kotlin 的 `RequestPriority.INTERACTIVE`）。
///
/// 宿主（`rust-bridge` 的 `RustHttpHost`）读到它之后：**在发出去之前删掉**，转成 OkHttp 的
/// `RequestPriority` tag（`MinIntervalInterceptor` 靠这个 tag 让"用户正等着的请求"插队）。
/// 所以它**永远不会出现在线上请求里**（`XhsPriorityWireTest` / `XhsDetailRequestEquivalenceTest` 都断言了）。
///
/// 加一个字段会让那些字面量编不过（合并时要么加
/// `..Default::default()`、要么一行行补），而请求头这条路只动"要表达优先级的那一家"。
/// `HostHttp` 的签名也因此一个字节没改。
///
/// 用法：`headers.push((PRIORITY_HEADER.into(), PRIORITY_INTERACTIVE.into()))`，宿主负责剥掉。
pub const PRIORITY_HEADER: &str = "X-Feedhub-Priority";

/// [`PRIORITY_HEADER`] 的取值：用户正等着（详情 / 评论 / 楼中楼 / 播放）。
///
/// 别的值（或者没有这个头）都按后台排——和加优先级之前完全一样。
pub const PRIORITY_INTERACTIVE: &str = "interactive";

// ===========================================================================
// query 编码
// ===========================================================================

/// query 里的一项，**逐字节复刻 OkHttp 4.12 `HttpUrl.addQueryParameter` 的口径**
///
/// 只有 `* - . 0-9 A-Z _ a-z` 原样，**其余全部百分号编码**（大写十六进制、非 ASCII 按 UTF-8 字节）：
/// 空格是 `%20`（不是 `+`），`~ ! $ ' ( ) , : ; @ / ?` 也编（这一条最容易踩：`encodeURIComponent`
/// 会保留 `!'()*~`，用它拼 URL 就和 Kotlin 版不再逐字节相同）。
/// 别换成表单编码，也别换成 WBI / TikTok 签名用的那些编码器。
pub fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// **OkHttp 4.12 `FormBody.Builder.add` 的编码**（表单 body 里 `k=v&k=v` 的每一段）。
///
/// 口径和 [`query_encode`] **完全相同**（两处都是 OkHttp 的 `canonicalize` + 同一个保守集合：只留
/// `* - . 0-9 A-Z _ a-z`，空格 `%20`、非 ASCII 按 UTF-8 字节）——实测 `FormBody.writeTo` 把
/// `a b+c%d~!$'()*,/:;<=>?@[]^\`{|}\\"#&` 编成一模一样的 `%XX`。
/// 名字分开只是为了让调用处一眼看出"这是表单 body，不是 query"；**别**改用 `+` 表示空格。
pub fn okhttp_form_encode(value: &str) -> String {
    query_encode(value)
}

/// **OkHttp 4.12 解析 URL 时对 query 的规范化**（签名签的是规范化之后的串，所以必须一模一样）。
///
/// 实测口径（OkHttp 4.12 探针）：
/// - `\t` / `\n` / `\r` **直接丢掉**（不是编码）；整个 URL 末尾 `<= ' '` 的字符会被裁掉；
/// - `#` 是 fragment 起点，query 到此为止；
/// - 空格 → `%20`；`"` `'` `<` `>` → `%22` `%27` `%3C` `%3E`；`< 0x20`、`0x7f`、非 ASCII 的 UTF-8 字节 → `%XX`（大写）；
/// - 其余可打印 ASCII（含 `% & = + , : ; @ / ? [ ] { } | \ ^ ` ~ $ ! ( ) *`）**原样保留**，
///   连不成对的 `%2` / `%zz` 也原样保留。
///
/// 别换成 `encodeURIComponent`，也别用 [`query_encode`]（那是 `addQueryParameter` 的口径，连 `,` 都编）。
pub fn okhttp_canonical_query(query: &str) -> String {
    // 先按 OkHttp 的解析口径处理**原始**字符串（顺序很重要：`#` 之后是 fragment，
    // 所以只有"没有 fragment"时，query 末尾的空白才是整个 URL 的末尾、才被裁掉）。
    let (head, has_fragment) = match query.find('#') {
        Some(i) => (&query[..i], true),
        None => (query, false),
    };
    let raw: String = head
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let raw: &str = if has_fragment {
        raw.as_str()
    } else {
        raw.trim_end_matches(|c: char| c <= ' ')
    };

    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii() {
            let b = ch as u8;
            let keep = (0x20..=0x7e).contains(&b) && !matches!(b, b' ' | b'"' | b'\'' | b'<' | b'>');
            if keep {
                out.push(ch);
            } else {
                push_percent(&mut out, b);
            }
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                push_percent(&mut out, *b);
            }
        }
    }
    out
}

pub fn push_percent(out: &mut String, b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 0x0f) as usize] as char);
}

// ===========================================================================
// JSON 字符串字面量
// ===========================================================================

/// JSON 字符串字面量：kotlinx.serialization 的转义口径
/// （`"` `\` `\n` `\r` `\t` `\b` `\f`，其余 < 0x20 写成 `\u00xx`（**小写**），非 ASCII 原样）。
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    push_json_string(&mut out, value);
    out
}

/// [`json_string`] 的追加版（少一次分配）。
pub fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ===========================================================================
// Java / Kotlin 的空白口径
// ===========================================================================

/// Java `Character.isWhitespace`：Unicode 的 Zs / Zl / Zp（**不含**不换行空格），
/// 外加 `\t \n \u000B \f \r` 和 `\u001C..\u001F`。
///
/// Rust 的 `char::is_whitespace` 用的是 Unicode White_Space 属性：多了 U+0085、含 NBSP / U+2007 / U+202F、
/// 又少了 U+001C..U+001F，两边正好错开，所以自己写。
pub fn is_java_whitespace(c: char) -> bool {
    match c {
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{1f}' => true,
        _ => c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{85}'),
    }
}

/// Kotlin `String.isBlank()`（空串也算）。
///
/// **注意**：这是 Java `Character.isWhitespace` 的口径，比 Kotlin 的 `Char.isWhitespace` 少了
/// NBSP 类的三个字符（U+00A0 / U+2007 / U+202F，它们是 `Character.isSpaceChar` 但不是 `isWhitespace`）。
/// 信息流 / 贴吧那几层一直是这个口径，
/// 要**逐字节对齐 Kotlin** 的调用方用 [`is_kotlin_blank`]。
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_java_whitespace)
}

/// Kotlin `String.isBlank()` 的**真口径**：`Char.isWhitespace` 在 JVM 上是
/// `Character.isWhitespace(c) || Character.isSpaceChar(c)`。
///
/// 和 [`is_blank`] 的差别只有三个字符（U+00A0 / U+2007 / U+202F，`isSpaceChar` 有、`isWhitespace` 没有）：
/// Kotlin 认为它们是空白、Java 的 `Character.isWhitespace` 不认。
/// 所以用它（B站 的弹幕分段里真的会有一条只有 NBSP 的脏数据）；**改动 [`is_blank`] 本身是行为变更**
/// （信息流 / 贴吧 会跟着变），
pub fn is_kotlin_blank(s: &str) -> bool {
    s.chars()
        .all(|c| is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}'))
}

/// Kotlin `String.trim()` 的**逐字节口径**：`CharSequence.trim()` 用的是 `Char.isWhitespace()`，
/// 而它在 JVM 上是 `Character.isWhitespace(c) || Character.isSpaceChar(c)`——比 [`is_java_whitespace`]
/// 多认 NBSP 类的三个字符（U+00A0 / U+2007 / U+202F）。
///
/// 和 [`kotlin_trim`] 的差别就是那三个字符：`kotlin_trim` 一直用的是 `isWhitespace` 的口径
/// （信息流 / 贴吧 那几层的历史口径，改动它是行为变更）。**要逐字节对齐 Kotlin `trim()` 的调用方用这个**
pub fn kotlin_trim_blank(s: &str) -> &str {
    fn is_blank_char(c: char) -> bool {
        is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
    }
    let start = s
        .char_indices()
        .find(|(_, ch)| !is_blank_char(*ch))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_blank_char(*ch))
        .map(|(i, ch)| i + ch.len_utf8())
        .unwrap_or(start);
    &s[start..end]
}

/// Kotlin `String.trim()`：去掉两端的空白字符（上面那个口径，不是 Rust 的 `trim`）。
pub fn kotlin_trim(s: &str) -> &str {
    let start = s
        .char_indices()
        .find(|(_, ch)| !is_java_whitespace(*ch))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_java_whitespace(*ch))
        .map(|(i, ch)| i + ch.len_utf8())
        .unwrap_or(start);
    &s[start..end]
}

// ===========================================================================
// 摘要 / 头 / 日期
// ===========================================================================

/// MD5 的小写十六进制（B站 WBI、酷安 token、抖音签名、小黑盒 `hkey`、TikTok 签名、小红书、知乎都用）。
pub fn md5_hex(text: &str) -> String {
    md5_hex_bytes(text.as_bytes())
}

/// 同上，输入是字节。
pub fn md5_hex_bytes(bytes: &[u8]) -> String {
    let digest = Md5::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 响应头里按名字取值（**名字大小写不敏感**，和 Kotlin 的 `equals(..., true)` 一样）。
///
/// 同名多次时取**第一个**（Kotlin 的 `headers[name]` 是 `get()`，OkHttp 的 `Headers` 也是第一个）。
pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// 响应头里有没有这个名字（同样大小写不敏感）。
pub fn has_header(headers: &[(String, String)], name: &str) -> bool {
    header_value(headers, name).is_some()
}

/// 纪元日（1970-01-01 起的**天数**）→ (年, 月, 日)。Howard Hinnant 的 `civil_from_days`。
///
/// B站 的"北京时间哪一天"（WBI key 的跨天刷新）和贴吧的 `event_day` 用它。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ===========================================================================
// `Platform` 的存盘写法（老的 11 家写枚举名、新的写 id）
// ===========================================================================

/// `Platform.toStored`：老的 11 家写**枚举名**（旧版本读得回），新的写 id。
///
/// 表就是 Kotlin `Platform.LEGACY_NAMES` 的逆映射（那边是"枚举名 → id"，这里反过来）。
/// 两份调用方：`xhs::feeditem_json`（收藏 / 关注流存盘）和 `settings`（设置里的平台集合 / 按平台代理）。
pub fn platform_to_stored(platform: &str) -> String {
    let legacy = match platform {
        "bili" => "BILIBILI",
        "zhihu" => "ZHIHU",
        "tieba" => "TIEBA",
        "heybox" => "HEYBOX",
        "coolapk" => "COOLAPK",
        "xhs" => "XHS",
        "douyin" => "DOUYIN",
        "x" => "X",
        "yt" => "YOUTUBE",
        "reddit" => "REDDIT",
        "tiktok" => "TIKTOK",
        _ => return platform.to_string(),
    };
    legacy.to_string()
}

/// `Platform.fromStored`：老的枚举名换成 id，别的原样当 id。
pub fn platform_from_stored(text: &str) -> String {
    let id = match text {
        "BILIBILI" => "bili",
        "ZHIHU" => "zhihu",
        "TIEBA" => "tieba",
        "HEYBOX" => "heybox",
        "COOLAPK" => "coolapk",
        "XHS" => "xhs",
        "DOUYIN" => "douyin",
        "X" => "x",
        "YOUTUBE" => "yt",
        "REDDIT" => "reddit",
        "TIKTOK" => "tiktok",
        other => other,
    };
    id.to_string()
}
