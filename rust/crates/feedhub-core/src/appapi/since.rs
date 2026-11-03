//! `since` 时间表达式解析。

use crate::hosted::kotlin_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinceError {
    Empty,
    InvalidDate(String),
    Unknown(String),
}

impl SinceError {
    pub fn message(&self) -> String {
        match self {
            SinceError::Empty => "since 不能为空".to_string(),
            SinceError::InvalidDate(text) => {
                format!("since 的时间看不懂：{text}（例：2026-10-01 或 2026-10-01 12:30）")
            }
            SinceError::Unknown(text) => {
                format!("since 看不懂：{text}（例：24h / 7d / 2026-10-01 / Unix 秒）")
            }
        }
    }
}

/// 解析 `since` 字符串。
/// `date_to_epoch` 用于将 `(year, month, day, Option<(hour, minute, second)>)` 转换成本地时区的毫秒时间戳（时区由宿主注入）。
pub fn parse_since<F>(text: &str, now_ms: i64, date_to_epoch: F) -> Result<i64, SinceError>
where
    F: Fn(i32, u32, u32, Option<(u32, u32, u32)>) -> Option<i64>,
{
    let t = kotlin_trim(text);
    if t.is_empty() {
        return Err(SinceError::Empty);
    }

    // 1) 相对时间：(\d+)([smhdw])
    if let Some((n, unit)) = parse_relative(&t) {
        let mult = match unit {
            's' => 1_000_i64,
            'm' => 60_000_i64,
            'h' => 3_600_000_i64,
            'd' => 86_400_000_i64,
            'w' => 7 * 86_400_000_i64,
            _ => 0_i64,
        };
        return Ok(now_ms - n * mult);
    }

    // 2) 日期时间：\d{4}-\d{2}-\d{2}([ T]\d{2}:\d{2}(:\d{2})?)?
    if is_date_pattern(&t) {
        if let Some((y, m, d, hms)) = parse_date_components(&t) {
            if let Some(epoch) = date_to_epoch(y, m, d, hms) {
                return Ok(epoch);
            }
        }
        return Err(SinceError::InvalidDate(text.to_string()));
    }

    // 3) Unix 秒（10 位）或毫秒（13 位）
    if let Ok(n) = t.parse::<i64>() {
        return Ok(if n > 100_000_000_000_i64 { n } else { n * 1000 });
    }

    Err(SinceError::Unknown(text.to_string()))
}

fn parse_relative(s: &str) -> Option<(i64, char)> {
    if s.is_empty() {
        return None;
    }
    let last = s.chars().last()?;
    if !matches!(last, 's' | 'm' | 'h' | 'd' | 'w') {
        return None;
    }
    let num_str = &s[..s.len() - last.len_utf8()];
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = num_str.parse::<i64>().ok()?;
    Some((n, last))
}

fn is_date_pattern(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    // \d{4}-\d{2}-\d{2}
    if !bytes[0..4].iter().all(u8::is_ascii_digit)
        || bytes[4] != b'-'
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || bytes[7] != b'-'
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    if bytes.len() == 10 {
        return true;
    }
    // [ T]\d{2}:\d{2}
    if bytes[10] != b' ' && bytes[10] != b'T' {
        return false;
    }
    if bytes.len() == 16 {
        return bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b':'
            && bytes[14..16].iter().all(u8::is_ascii_digit);
    }
    if bytes.len() == 19 {
        return bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b':'
            && bytes[14..16].iter().all(u8::is_ascii_digit)
            && bytes[16] == b':'
            && bytes[17..19].iter().all(u8::is_ascii_digit);
    }
    false
}

fn parse_date_components(s: &str) -> Option<(i32, u32, u32, Option<(u32, u32, u32)>)> {
    let y = s[0..4].parse::<i32>().ok()?;
    let m = s[5..7].parse::<u32>().ok()?;
    let d = s[8..10].parse::<u32>().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if s.len() == 10 {
        return Some((y, m, d, None));
    }
    let hh = s[11..13].parse::<u32>().ok()?;
    let mm = s[14..16].parse::<u32>().ok()?;
    if hh >= 24 || mm >= 60 {
        return None;
    }
    let ss = if s.len() == 19 {
        let sec = s[17..19].parse::<u32>().ok()?;
        if sec >= 60 {
            return None;
        }
        sec
    } else {
        0
    };
    Some((y, m, d, Some((hh, mm, ss))))
}

