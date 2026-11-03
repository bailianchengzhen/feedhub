//! 文本 / 时间文案：`core/Text.kt` + `core/TimeText.kt` + `app/ui/Format.kt` +
//! `app/detail/DetailFormat.kt` + `app/mine/DayLabel.kt` 的 Rust 版。
//!
//! 两条硬约束（见 [`super`] 的模块说明）：
//! - **时区由宿主给**：每个时间戳都带一个 `offset_sec`（`java.time` 在那一刻算出来的偏移）。
//!   Rust **不读系统时区**、不调 `now()`。
//! - **Kotlin 的空白口径**：`String.trim()` 用 `Char.isWhitespace`（含 NBSP 类），这里一律
//!   [`crate::html::kotlin_trim`]。

use crate::model::TimePrecision;

/// [`parse_relative_time_precise`] 的结果：Unix 秒 + 它精确到哪一档（Kotlin `RelativeTime`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativeTime {
    pub epoch_sec: i64,
    pub precision: TimePrecision,
}

// ===========================================================================
// 空白 / 正则口径
// ===========================================================================

/// Kotlin `String.trim()`（`Char.isWhitespace` = `Character.isWhitespace || isSpaceChar`）。
///
/// 用 [`crate::html::kotlin_trim`]：那一份是照 `Char.isWhitespace` 写的（**含 NBSP 类**，
/// 不含 `U+0085`，含 `U+001C..U+001F`）。`hosted::kotlin_trim` 是 Java `Character.isWhitespace`
/// 的口径（不含 NBSP），两者在"只由 NBSP 包着的文本"上不一样——搬展示层用的必须是前者。
pub fn kotlin_text_trim(s: &str) -> &str {
    crate::html::kotlin_trim(s)
}

/// Java / Kotlin 正则里 `\s` 的默认口径（**六个 ASCII 空白**，不含 Unicode 空白）。
fn is_java_regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
}

// ===========================================================================
// `Double.parseDouble` / `Math.round` / `"%.1f".format`
// ===========================================================================

/// Java `Double.parseDouble` / Kotlin `String.toDoubleOrNull` 的口径。
///
/// Rust 的 `f64::from_str` 在三个地方比 Java **宽**（`"inf"` / `"nan"` / `"INFINITY"` 大小写不敏感）、
/// 两个地方比 Java **严**（`"1d"` / `"1f"` 这种后缀、前导 `+`——Rust 两者其实都认，后缀不认）。
/// 这里照 Java 的完整语法做：
/// 1. 先 `trim()`（Java 的 `String.trim`：去掉 `<= ' '` 的字符，**不是** Kotlin 的 `isWhitespace`）；
/// 2. 去掉**一个** `[fFdD]` 后缀；
/// 3. `±NaN` / `±Infinity`（大小写敏感）；
/// 4. 十六进制浮点 `0x1.8p3`（Java 的 `HexFloatingPointLiteral`，二进制指数必填）；
/// 5. 其余只认 `[0-9+-.eE]`。
pub fn java_parse_double(text: &str) -> Option<f64> {
    let t = text.trim_matches(|c: char| c <= ' ');
    if t.is_empty() {
        return None;
    }
    let body = match t.as_bytes().last() {
        Some(b'f') | Some(b'F') | Some(b'd') | Some(b'D') => &t[..t.len() - 1],
        _ => t,
    };
    if body.is_empty() {
        return None;
    }
    let (negative, rest) = match body.as_bytes()[0] {
        b'-' => (true, &body[1..]),
        b'+' => (false, &body[1..]),
        _ => (false, body),
    };
    // Java 只认这两个写法，且大小写敏感（`"nan"` / `"inf"` 都不认）
    if rest == "NaN" {
        return Some(f64::NAN);
    }
    if rest == "Infinity" {
        return Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    // 十六进制浮点：Java 认、Rust 不认
    if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        let v = java_hex_float(hex)?;
        return Some(if negative { -v } else { v });
    }
    // Java 的十进制语法里没有别的字母（`e` / `E` 除外）；先挡掉 Rust 认的 `inf` / `nan` 变体
    if !rest
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
    {
        return None;
    }
    // 符号只许一个（`--5` / `+-5` Java 不认，Rust 的 parse 会再吃一个）
    if rest.starts_with(['+', '-']) {
        return None;
    }
    // 上面把符号拿掉了，这里要加回去
    let v = rest.parse::<f64>().ok()?;
    Some(if negative { -v } else { v })
}

/// `0x` 之后的部分：`HexDigits? [. HexDigits?] [pP] [+-]? Digits`（至少一位十六进制数字、`p` 指数必填）。
/// 尾数超过 64 位时截断低位（会差最后一个 ULP；计数文本里不会出现这么长的十六进制）。
fn java_hex_float(hex: &str) -> Option<f64> {
    let (mantissa, exp) = hex.split_once(['p', 'P'])?;
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
    if exp_digits.is_empty() || !exp_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // 指数特别大时饱和（Java 给 ∞ 或 0），不会因为 parse 溢出而变成"解析不了"
    let exp: i64 = exp.parse().unwrap_or(if exp.starts_with('-') { -100_000 } else { 100_000 });
    let mut value: u64 = 0;
    let mut shift: i64 = 0;
    for (i, c) in int_part.chars().chain(frac_part.chars()).enumerate() {
        let d = c.to_digit(16)? as u64;
        if value >> 60 == 0 {
            value = value * 16 + d;
            if i >= int_part.len() {
                shift -= 4;
            }
        } else if i < int_part.len() {
            shift += 4;
        }
    }
    let e = (exp + shift).clamp(-2_000, 2_000) as i32;
    Some(value as f64 * 2f64.powi(e))
}

/// Java `Math.round(double)`：`(long) Math.floor(a + 0.5d)`，NaN → 0，溢出饱和。
fn java_round(a: f64) -> i64 {
    // Rust 的 `as i64` 对 NaN 给 0、对 ±∞ 饱和，和 Java 的 Math.round 一致
    (a + 0.5).floor() as i64
}

/// Java `"%.1f".format(value)`（`Locale.ROOT` 的小数点，调用方按需换字符）。
///
/// **不是** Rust 的 `format!("{:.1}")`：Java 的 Formatter 对 double 的**精确十进制展开**做
/// HALF_UP 舍入，Rust 的是 half-to-even。差值只在"恰好 `x.x5`"上：`1.25` Java 给 `"1.3"`、
/// Rust 给 `"1.2"`；`1.05`（double 是 1.0500000000000000444…）两边都给 `"1.1"`；
/// `2.675`（double 是 2.67499999999999982…）两边都给 `"2.7"`。
pub fn java_fixed_1(value: f64, decimal_sep: char) -> String {
    if value.is_nan() {
        // Java 给 "NaN"（没有小数位）
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let negative = value.is_sign_negative();
    let a = value.abs();
    // 30 位小数足够区分"恰好 x.x5"和"只差 1 ULP"：Java 的舍入依据就是 double 的精确展开
    let exact = format!("{a:.30}");
    let (int_part, frac) = exact.split_once('.').unwrap_or((exact.as_str(), ""));
    let frac: Vec<u8> = frac.bytes().collect();
    let first = frac.first().copied().unwrap_or(b'0');
    // 第 1 位之后的部分决定进位
    let rest = &frac[1.min(frac.len())..];
    // 第 2 位是 5 就进位：**恰好 `x.x5` 按 HALF_UP 进位**，`x.x500…001` 这种比它大的自然也进位
    let round_up = match rest.first().copied() {
        Some(b'5') => true,
        Some(b) => b > b'5',
        None => false,
    };
    let mut int_digits: Vec<u8> = int_part.as_bytes().to_vec();
    let mut digit = first;
    if round_up {
        if digit == b'9' {
            digit = b'0';
            // 整数部分 +1
            let mut i = int_digits.len();
            loop {
                if i == 0 {
                    int_digits.insert(0, b'1');
                    break;
                }
                i -= 1;
                if int_digits[i] == b'9' {
                    int_digits[i] = b'0';
                } else {
                    int_digits[i] += 1;
                    break;
                }
            }
        } else {
            digit += 1;
        }
    }
    let mut out = String::with_capacity(int_digits.len() + 4);
    // Java 对 `-0.4` 也印负号（`-0.0` 同理），所以按符号位来
    if negative {
        out.push('-');
    }
    out.push_str(std::str::from_utf8(&int_digits).unwrap_or("0"));
    out.push(decimal_sep);
    out.push(digit as char);
    out
}

// ===========================================================================
// `parseCount` / `parseDuration` / `toHttps` / `stripHtml`
// ===========================================================================

/// 把平台给的计数文本转成数字："1.2万" → 12000，"3亿" → 300000000，"--" → None。
///
/// `text` 先按 **Kotlin 的 trim** 去空白、去掉所有半角逗号，再看后缀（万 / 亿 / w / k，后两个
/// 忽略 ASCII 大小写），数字部分走 [`java_parse_double`]，结果按 `Math.round`（`floor(x + 0.5)`）。
pub fn parse_count(text: Option<&str>) -> Option<i64> {
    let raw = kotlin_text_trim(text?).replace(',', "");
    if raw.is_empty() {
        return None;
    }
    let (num, mul) = if let Some(rest) = raw.strip_suffix('万') {
        (rest.to_string(), 10_000.0)
    } else if let Some(rest) = raw.strip_suffix('亿') {
        (rest.to_string(), 100_000_000.0)
    } else if raw.ends_with('w') || raw.ends_with('W') {
        (raw[..raw.len() - 1].to_string(), 10_000.0)
    } else if raw.ends_with('k') || raw.ends_with('K') {
        (raw[..raw.len() - 1].to_string(), 1_000.0)
    } else {
        (raw, 1.0)
    };
    let d = java_parse_double(&num)?;
    Some(java_round(d * mul))
}

/// "12:34" / "1:02:03" → 秒数（Kotlin `parseDuration`）。
///
/// 口径：只对整个文本 trim；按 `:` 切；每段必须能 `toIntOrNull`；最多三段；累加用 Int（溢出按
/// Kotlin 的环绕）。
pub fn parse_duration(text: Option<&str>) -> Option<i32> {
    let raw = kotlin_text_trim(text?);
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut acc: i32 = 0;
    for p in parts {
        let n: i32 = java_parse_int(p)?;
        acc = acc.wrapping_mul(60).wrapping_add(n);
    }
    Some(acc)
}

/// Kotlin `String.toIntOrNull()`：可选 `+` / `-`、至少一位 ASCII 数字、溢出 → `None`。
fn java_parse_int(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    let v = if negative { -v } else { v };
    if v < i32::MIN as i64 || v > i32::MAX as i64 {
        return None;
    }
    Some(v as i32)
}

/// 图片地址统一成 https（B站等会给 `//i0.hdslb.com/...` 或 `http://`）。
pub fn to_https(u: &str) -> String {
    if u.starts_with("//") {
        format!("https:{u}")
    } else if let Some(rest) = u.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        u.to_string()
    }
}

/// 去掉 HTML 标签并还原常见实体（`core/Text.kt` 的 `stripHtml`）。
///
/// **全仓唯一一份**：`html/mod.rs` 的规则（那一份本来就是照 Kotlin 的正则序列逐条写的），
/// 小黑盒 / 知乎以前各有一份拷贝，
pub fn strip_html(html: &str) -> String {
    crate::html::strip_html(html)
}

/// Kotlin `String.lowercase()`（`Locale.ROOT` 口径）。
fn lowercase_root(s: &str) -> String {
    s.to_lowercase()
}

/// 相对时间文本 → Unix 秒 + 精度档（Kotlin `parseRelativeTimePrecise`）。
///
/// 认的字样和 Kotlin 那张正则**逐条一样**（`just now` / `刚刚` / `moments ago`、`12 minutes ago`、
/// `3小时前`、`昨天` …），认不出来就是 `None`（宁可没有这个时间，也不编一个）。`now_sec` 由宿主给。
pub fn parse_relative_time_precise(text: Option<&str>, now_sec: i64) -> Option<RelativeTime> {
    let trimmed = kotlin_text_trim(text?);
    let lower = lowercase_root(trimmed);
    // `removePrefix("streamed")?.removePrefix("premiered")`：**依次**去，各去一次
    let after_streamed = lower.strip_prefix("streamed").unwrap_or(&lower);
    let after_premiered = after_streamed.strip_prefix("premiered").unwrap_or(after_streamed);
    let t = kotlin_text_trim(after_premiered);
    if t.is_empty() {
        return None;
    }
    if t == "just now" || t == "刚刚" || t == "moments ago" {
        return Some(RelativeTime {
            epoch_sec: now_sec,
            precision: TimePrecision::Second,
        });
    }
    if let Some(r) = scan_relative(t, now_sec) {
        return Some(r);
    }
    if t == "yesterday" || t == "昨天" {
        return Some(RelativeTime {
            epoch_sec: now_sec.wrapping_sub(86_400),
            precision: TimePrecision::Day,
        });
    }
    None
}

/// 正则 `(\d+)\s*(second|minute|hour|day|week|month|year|秒|分钟|小时|天|日|周|星期|个月|月|年)s?\s*(ago|前)?`
/// 的 `find`（最左匹配）。
fn scan_relative(t: &str, now_sec: i64) -> Option<RelativeTime> {
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let digits: String = chars[i..j].iter().collect();
        let mut k = j;
        while k < chars.len() && is_java_regex_space(chars[k]) {
            k += 1;
        }
        if let Some(unit) = match_unit(&chars, k) {
            // Kotlin：`m.groupValues[1].toLongOrNull() ?: return null`（溢出就是 null，不再往后找）
            let n: i64 = digits.parse().ok()?;
            let (secs, precision) = unit_secs_precision(unit)?;
            return Some(RelativeTime {
                epoch_sec: now_sec.wrapping_sub(n.wrapping_mul(secs)),
                precision,
            });
        }
        i += 1;
    }
    None
}

/// 位置 `i` 上匹配到哪个单位（**照正则里的交替顺序**，第一个匹配的算）。
fn match_unit(chars: &[char], i: usize) -> Option<&'static str> {
    const UNITS: [&str; 17] = [
        "second", "minute", "hour", "day", "week", "month", "year", "秒", "分钟", "小时", "天",
        "日", "周", "星期", "个月", "月", "年",
    ];
    for unit in UNITS {
        if starts_with_at(chars, i, unit) {
            return Some(unit);
        }
    }
    None
}

fn starts_with_at(chars: &[char], i: usize, needle: &str) -> bool {
    let mut k = i;
    for c in needle.chars() {
        if k >= chars.len() || chars[k] != c {
            return false;
        }
        k += 1;
    }
    true
}

fn unit_secs_precision(unit: &str) -> Option<(i64, TimePrecision)> {
    Some(match unit {
        "second" | "秒" => (1, TimePrecision::Second),
        "minute" | "分钟" => (60, TimePrecision::Minute),
        "hour" | "小时" => (3_600, TimePrecision::Hour),
        "day" | "天" | "日" => (86_400, TimePrecision::Day),
        "week" | "周" | "星期" => (604_800, TimePrecision::Day),
        "month" | "个月" | "月" => (2_592_000, TimePrecision::Month),
        "year" | "年" => (31_536_000, TimePrecision::Year),
        _ => return None,
    })
}

// ===========================================================================
// 民用日期 / 时间（epoch 秒 + 时区偏移 → 年月日时分秒）
// ===========================================================================

/// 一个"本地时刻"（`Instant.atZone(zone)` 的拆解结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = 周一 … 6 = 周日。
    pub weekday: u32,
    /// 本地日期的天数（1970-01-01 是 0），判"今天 / 昨天"用。
    pub day_number: i64,
}

/// `Instant.ofEpochSecond(epoch_sec).atZone(zone)`（`offset_sec` 是宿主在**那一刻**算出来的偏移）。
pub fn civil(epoch_sec: i64, offset_sec: i64) -> Civil {
    let local = epoch_sec.wrapping_add(offset_sec);
    let day_number = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(day_number);
    Civil {
        year,
        month,
        day,
        hour: (secs / 3_600) as u32,
        minute: ((secs % 3_600) / 60) as u32,
        second: (secs % 60) as u32,
        weekday: weekday_from_days(day_number),
        day_number,
    }
}

/// 1970-01-01 起的天数 → 年月日（Howard Hinnant 的 `civil_from_days`，和 `java.time` 同解）。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 年月日 → 1970-01-01 起的天数（`days_from_civil`）。
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = 周一 … 6 = 周日（1970-01-01 是周四）。
pub fn weekday_from_days(day_number: i64) -> u32 {
    (day_number + 3).rem_euclid(7) as u32
}

fn pad2(v: u32) -> String {
    format!("{v:02}")
}

fn pad4(v: i64) -> String {
    format!("{v:04}")
}

// ===========================================================================
// `TimeText` 的三个入口
// ===========================================================================

/// 按精度档显示**相对**时间（Kotlin `TimeText.relative`）。
///
/// * `epoch_sec` 是发布时刻，`precision` 是平台给的精度档；
/// * `now_ms` 是宿主时钟（毫秒）；`offset_at_epoch` / `offset_at_now` 是两处的时区偏移（秒）。
pub fn relative(
    epoch_sec: i64,
    precision: TimePrecision,
    now_ms: i64,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    // Kotlin：`nowMs / 1000 - epochSec`（整数除法截断，这里同样是截断）
    let diff = now_ms / 1_000 - epoch_sec;
    let t = civil(epoch_sec, offset_at_epoch);
    let today = civil(now_ms.div_euclid(1_000), offset_at_now);
    let today_day = today.day_number;
    match precision {
        TimePrecision::Second | TimePrecision::Minute => {
            if diff < 60 {
                "刚刚".to_string()
            } else if diff < 3_600 {
                format!("{}分钟前", diff / 60)
            } else if t.day_number == today_day {
                format!("{}小时前", diff / 3_600)
            } else if t.day_number == today_day - 1 {
                format!("昨天 {}:{}", pad2(t.hour), pad2(t.minute))
            } else if t.year == today.year {
                format!("{}-{}", pad2(t.month), pad2(t.day))
            } else {
                format!("{}-{}-{}", pad4(t.year), pad2(t.month), pad2(t.day))
            }
        }
        TimePrecision::Hour => {
            if diff < 86_400 {
                // 只到小时：不足一小时按 1 小时算，绝不退回"X分钟前"
                format!("{}小时前", (diff / 3_600).max(1))
            } else if diff < 7 * 86_400 {
                format!("{}天前", diff / 86_400)
            } else if t.year == today.year {
                format!("{}-{}", pad2(t.month), pad2(t.day))
            } else {
                format!("{}-{}-{}", pad4(t.year), pad2(t.month), pad2(t.day))
            }
        }
        TimePrecision::Day => {
            if t.day_number == today_day {
                "今天".to_string()
            } else if t.day_number == today_day - 1 {
                "昨天".to_string()
            } else if diff < 30 * 86_400 {
                format!("{}天前", diff / 86_400)
            } else if t.year == today.year {
                format!("{}-{}", pad2(t.month), pad2(t.day))
            } else {
                format!("{}-{}-{}", pad4(t.year), pad2(t.month), pad2(t.day))
            }
        }
        TimePrecision::Month => {
            let months = diff / (30 * 86_400);
            if months < 12 {
                format!("{}个月前", months.max(1))
            } else {
                format!("{}-{}", pad4(t.year), pad2(t.month))
            }
        }
        TimePrecision::Year => {
            format!("{}年前", (diff / (365 * 86_400)).max(1))
        }
    }
}

/// 紧凑的绝对时间（cli 用；
pub fn time_absolute(
    epoch_sec: i64,
    precision: TimePrecision,
    now_ms: i64,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    let t = civil(epoch_sec, offset_at_epoch);
    let now = civil(now_ms.div_euclid(1_000), offset_at_now);
    let this_year = t.year == now.year;
    match precision {
        TimePrecision::Second | TimePrecision::Minute => {
            if this_year {
                format!("{}-{} {}:{}", pad2(t.month), pad2(t.day), pad2(t.hour), pad2(t.minute))
            } else {
                format!(
                    "{}-{}-{} {}:{}",
                    pad4(t.year),
                    pad2(t.month),
                    pad2(t.day),
                    pad2(t.hour),
                    pad2(t.minute)
                )
            }
        }
        // 平台只给到小时：分钟写 00（不给就是不给）
        TimePrecision::Hour => {
            let s = if this_year {
                format!("{}-{} {}:{}", pad2(t.month), pad2(t.day), pad2(t.hour), pad2(t.minute))
            } else {
                format!(
                    "{}-{}-{} {}:{}",
                    pad4(t.year),
                    pad2(t.month),
                    pad2(t.day),
                    pad2(t.hour),
                    pad2(t.minute)
                )
            };
            format!("{}00", &s[..s.len() - 2])
        }
        TimePrecision::Day => {
            if this_year {
                format!("{}-{}", pad2(t.month), pad2(t.day))
            } else {
                format!("{}-{}-{}", pad4(t.year), pad2(t.month), pad2(t.day))
            }
        }
        TimePrecision::Month => format!("{}-{}", pad4(t.year), pad2(t.month)),
        TimePrecision::Year => format!("{}年", pad4(t.year)),
    }
}

pub fn time_full(epoch_sec: i64, precision: TimePrecision, offset_at_epoch: i64) -> String {
    let t = civil(epoch_sec, offset_at_epoch);
    match precision {
        TimePrecision::Second | TimePrecision::Minute => format!(
            "{}-{}-{} {}:{}",
            pad4(t.year),
            pad2(t.month),
            pad2(t.day),
            pad2(t.hour),
            pad2(t.minute)
        ),
        TimePrecision::Hour => {
            let s = format!(
                "{}-{}-{} {}:{}",
                pad4(t.year),
                pad2(t.month),
                pad2(t.day),
                pad2(t.hour),
                pad2(t.minute)
            );
            format!("{}00", &s[..s.len() - 2])
        }
        TimePrecision::Day => format!("{}-{}-{}", pad4(t.year), pad2(t.month), pad2(t.day)),
        TimePrecision::Month => format!("{}-{}", pad4(t.year), pad2(t.month)),
        TimePrecision::Year => pad4(t.year),
    }
}

/// 评论的时间（`detail/DetailFormat.kt` 的 `commentTime`）：今年的 `10-08 15:17`，
/// 平台只给到天 / 月 / 年时退回相对说法（别断言具体哪一年）。
pub fn comment_time(
    epoch_sec: i64,
    now_ms: i64,
    precision: TimePrecision,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    let same_year = civil(epoch_sec, offset_at_epoch).year
        == civil(now_ms.div_euclid(1_000), offset_at_now).year;
    if precision != TimePrecision::Second && precision != TimePrecision::Minute {
        return relative(epoch_sec, precision, now_ms, offset_at_epoch, offset_at_now);
    }
    time_absolute(
        epoch_sec,
        if same_year {
            TimePrecision::Minute
        } else {
            TimePrecision::Day
        },
        now_ms,
        offset_at_epoch,
        offset_at_now,
    )
}

/// `app/ui/Format.kt` 的 `relativeTime`：系统时区的相对时间（宿主自己算两个偏移，见 [`relative`]）。
pub fn relative_time_of(
    epoch_sec: i64,
    precision: TimePrecision,
    now_ms: i64,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    relative(epoch_sec, precision, now_ms, offset_at_epoch, offset_at_now)
}

// ===========================================================================
// 计数 / 时长 / 日期标题
// ===========================================================================

/// `app/ui/Format.kt` 的 `compactCount`：12345 → "1.2万"，123456789 → "1.2亿"。
///
/// `decimal_sep` 是宿主给的小数点（Java 的 `"%.1f".format` 用 `Locale.getDefault`）；
/// **去尾 `.0` 用的是 ASCII 句点**——和 Kotlin 的 `removeSuffix(".0")` 一样，
/// 所以小数点不是 `.` 时 `"1,0"` 不会被去掉（原样保留，这就是原实现的字面行为）。
pub fn compact_count(n: i64, decimal_sep: char) -> String {
    if n >= 100_000_000 {
        format!("{}亿", trim_zero(java_fixed_1(n as f64 / 1e8, decimal_sep)))
    } else if n >= 10_000 {
        format!("{}万", trim_zero(java_fixed_1(n as f64 / 1e4, decimal_sep)))
    } else {
        n.to_string()
    }
}

fn trim_zero(s: String) -> String {
    match s.strip_suffix(".0") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

/// `app/ui/Format.kt` 的 `duration`：`12:34` / `1:02:03`（`%d:%02d[:%02d]` 的口径，负号算在宽度里）。
pub fn duration_text(sec: i32) -> String {
    if sec >= 3_600 {
        format!(
            "{}:{}:{}",
            sec / 3_600,
            java_pad2(sec / 60 % 60),
            java_pad2(sec % 60)
        )
    } else {
        format!("{}:{}", sec / 60, java_pad2(sec % 60))
    }
}

/// Java `"%02d".format(v)`：补零到宽度 2，**负号也算一位**（-5 → `"-5"`，-123 → `"-123"`）。
fn java_pad2(v: i32) -> String {
    let s = v.to_string();
    if s.len() >= 2 {
        s
    } else {
        format!("0{s}")
    }
}

/// 星期几的中文（`DayLabel` 用的那张表）。
pub fn week_label(weekday: u32) -> &'static str {
    match weekday % 7 {
        0 => "周一",
        1 => "周二",
        2 => "周三",
        3 => "周四",
        4 => "周五",
        5 => "周六",
        _ => "周日",
    }
}

/// `mine/DayLabel.kt` 的 `dayLabel`：今天 / 昨天 / "10月12日 周日"（今年）/ "2025年10月12日 周日"（往年）。
pub fn day_label(
    year: i64,
    month: u32,
    day: u32,
    today_year: i64,
    today_month: u32,
    today_day: u32,
) -> String {
    let date = days_from_civil(year, month, day);
    let today = days_from_civil(today_year, today_month, today_day);
    let week = week_label(weekday_from_days(date));
    if date == today {
        "今天".to_string()
    } else if date == today - 1 {
        "昨天".to_string()
    } else if year == today_year {
        format!("{month}月{day}日 {week}")
    } else {
        format!("{year}年{month}月{day}日 {week}")
    }
}

/// `mine/DayLabel.kt` 的 `checkedAtLabel`：「上次查到：…」那行（日期照 [`day_label`]，再带时分）。
pub fn checked_at_label(at_ms: i64, now_ms: i64, offset_at_at: i64, offset_at_now: i64) -> String {
    let m = civil(at_ms.div_euclid(1_000), offset_at_at);
    let today = civil(now_ms.div_euclid(1_000), offset_at_now);
    format!(
        "{} {}:{}",
        day_label(m.year, m.month, m.day, today.year, today.month, today.day),
        pad2(m.hour),
        pad2(m.minute)
    )
}

// ===========================================================================
// 一批（界面一屏一次）
// ===========================================================================

/// [`relative`] 的一批入参（界面一屏一次过边界）。
#[derive(Debug, Clone, Copy)]
pub struct RelativeRequest {
    pub epoch_sec: i64,
    pub precision: TimePrecision,
    pub now_ms: i64,
    pub offset_at_epoch: i64,
    pub offset_at_now: i64,
}

/// 一批 [`relative`]（等长等序）。
pub fn relative_times(requests: &[RelativeRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| {
            relative(
                r.epoch_sec,
                r.precision,
                r.now_ms,
                r.offset_at_epoch,
                r.offset_at_now,
            )
        })
        .collect()
}

/// 一批 [`comment_time`]（等长等序）：评论列表一屏一次。
pub fn comment_times(requests: &[RelativeRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| {
            comment_time(
                r.epoch_sec,
                r.now_ms,
                r.precision,
                r.offset_at_epoch,
                r.offset_at_now,
            )
        })
        .collect()
}

