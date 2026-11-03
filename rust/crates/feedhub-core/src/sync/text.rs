//! Kotlin 语义的**字符串小工具**（`isBlank` / `take` / `isISOControl` / `Regex("\\s+")`）。
//!
//! 这些看着是"随便写写"的东西，但同步里的每一个判断都靠它们，而且**两边必须一致**：
//! 设备名清不清洗、uid 空不空、快照是不是"没有设备标识"——差一个 NBSP 就是两种行为。
//!
//! Kotlin 的口径（`kotlin.text`）：
//! - `String.isBlank()` → 所有 `Char.isWhitespace()`；
//! - `Char.isWhitespace()` → `Character.isWhitespace(c) || Character.isSpaceChar(c)`；
//! - `String.trim()` → 去掉两端的 `Char.isWhitespace()`（同上）；
//! - `String.take(n)` → 按 **UTF-16 码元**数截，不是码点；
//! - `Char.isISOControl()` → `0x00..0x1F` 和 `0x7F..0x9F`；
//! - `Regex("\\s")`（Java）→ 只有 `[ \t\n\x0B\f\r]`，**不是** Unicode 空白。
//!
//! Rust 的 `char::is_whitespace`（Unicode `White_Space`）和这几条都不一样：它**多**了 U+0085，
//! **少**了 U+001C..U+001F。所以这里全部显式写出来。

/// Java `Character.isSpaceChar`：Unicode 的 Zs / Zl / Zp（**包含**不换行空格 NBSP）。
pub fn is_space_char(c: char) -> bool {
    matches!(
        c,
        '\u{20}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Kotlin `Char.isWhitespace()` = Java `isWhitespace` **或** `isSpaceChar`。
pub fn is_whitespace(c: char) -> bool {
    is_space_char(c)
        || matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{1c}'..='\u{1f}'
        )
}

/// Kotlin `String.isBlank()`。
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_whitespace)
}

/// Kotlin `String.trim()`（两端去掉 `Char.isWhitespace`）。
pub fn trim(s: &str) -> &str {
    let start = s
        .char_indices()
        .find(|(_, c)| !is_whitespace(*c))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| !is_whitespace(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start);
    &s[start..end]
}

/// Kotlin `Char.isISOControl()`。
pub fn is_iso_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// Kotlin `String.take(n)`：数的是 **UTF-16 码元**。
///
/// 一个补充平面的字符（emoji）算 2 个。截断点正好落在代理对中间时，Kotlin 得到的字符串带着一个
/// **孤立的高位代理**；这样的字符串之后按 UTF-8 编码时，那个代理会变成 `?`（JDK 的
/// `String.getBytes(UTF_8)` 用 REPLACE 动作）。这里照样复刻，否则设备名里带 emoji 时
/// 配对报文的字节两边就对不上。
pub fn take_utf16(s: &str, n: usize) -> String {
    let mut units = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let width = c.len_utf16();
        if units + width > n {
            // 还剩一个码元，但这个是 2 码元的字符：Kotlin 会留下一个孤立的高位代理 → UTF-8 里是 '?'
            if units < n {
                out.push('?');
            }
            return out;
        }
        out.push(c);
        units += width;
    }
    out
}

/// Kotlin `String.length`（UTF-16 码元数）。
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 把 Java 正则 `\s+` 的运行压成一个空格（`PairingCodec.cleanName` 用）。
///
/// 注意 Java 的 `\s` **只是** `[ \t\n\x0B\f\r]`（没有 `Pattern.UNICODE_CHARACTER_CLASS` 时），
/// 和 Unicode 空白、和 Kotlin 的 `isWhitespace` 都不是一回事——照抄。
pub fn collapse_java_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

