//! `core/src/main/kotlin/feedhub/core/JsonExt.kt` 的 Rust 版本。
//!
//! 平台响应里数字经常写成字符串（反过来也有），所以 `long()` / `string()` 两种都兼容；
//! 类型不对、字段不存在、显式 null 一律返回 `None`。语义逐条对着 Kotlin 实现抄，
//! 对拍（`BiliDualRunTest`）守着两边一致。
//!
//! 用 `serde_json::Value` 而不是派生结构体，是为了保住 `JsonExt` 这套"什么都能缺、类型能混"的容错语义；
//! 代价是解析时建一棵树。这是**有意**的取舍：
//! 当前的验收标准是"和 Kotlin 解析结果逐字段相等"，不是快。

use serde_json::{Map, Value};

pub type JsonMap = Map<String, Value>;
/// JSON 值（数组元素、对象字段的通用形态）。解析器里到处要用，给个短名字。
pub type JsonValue = Value;

/// 解析成 JSON 对象，对应 Kotlin 的 `parseJsonObject`。错误文案也照抄（含响应前 200 个字符）。
pub fn parse_object(body: &str) -> Result<JsonMap, String> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("响应不是 JSON 对象：{}", take_chars(body, 200))),
        Err(_) => Err(format!("响应不是合法 JSON：{}", take_chars(body, 200))),
    }
}

/// Kotlin `String.take(200)`：按字符（不是字节）截断。
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn obj<'a>(m: &'a JsonMap, key: &str) -> Option<&'a JsonMap> {
    m.get(key)?.as_object()
}

pub fn arr<'a>(m: &'a JsonMap, key: &str) -> Option<&'a Vec<Value>> {
    m.get(key)?.as_array()
}

/// 对应 `JsonObject.string(key)`：字符串 / 数字 / 布尔都转成文本，null、数组、对象是 `None`。
pub fn string(m: &JsonMap, key: &str) -> Option<String> {
    m.get(key).and_then(as_string)
}

/// 对应 `JsonObject.text(key)`：非空白字符串，否则 `None`。
pub fn text(m: &JsonMap, key: &str) -> Option<String> {
    string(m, key).filter(|s| !s.trim().is_empty())
}

/// 对应 `JsonObject.long(key)`：数字、数字字符串、`1.5`（截断）都认。
pub fn long(m: &JsonMap, key: &str) -> Option<i64> {
    m.get(key).and_then(as_long)
}

/// 对应 `JsonObject.int(key)`：`long` 再 `toInt()`（溢出按低 32 位截断，和 Kotlin 一样）。
pub fn int(m: &JsonMap, key: &str) -> Option<i32> {
    long(m, key).map(|n| n as i32)
}

/// 对应 `JsonObject.double(key)`。
pub fn double(m: &JsonMap, key: &str) -> Option<f64> {
    m.get(key).and_then(as_double)
}

/// 对应 `JsonObject.bool(key)`：布尔、数字非 0、`"true"` / `"false"`、数字字符串。
pub fn bool(m: &JsonMap, key: &str) -> Option<bool> {
    m.get(key).and_then(as_bool)
}

/// Kotlin `JsonPrimitive.content`：字符串原样、数字保留字面量、布尔 `true` / `false`、`null` 是字符串 `"null"`。
/// 解析器里 `(mid as? JsonPrimitive)?.content` 这种写法靠它。
pub fn primitive_content(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".to_string()),
        _ => None,
    }
}

pub fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn as_long(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        // Kotlin: content.toLongOrNull() ?: content.toDoubleOrNull()?.toLong() ?: content.trim().toLongOrNull()
        Value::String(s) => s
            .parse::<i64>()
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
            .or_else(|| s.trim().parse::<i64>().ok()),
        _ => None,
    }
}

pub fn as_double(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        // Kotlin: content.toDoubleOrNull() ?: content.trim().toDoubleOrNull()
        Value::String(s) => s.parse::<f64>().ok().or_else(|| s.trim().parse::<f64>().ok()),
        _ => None,
    }
}

pub fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        // Kotlin: content.toBooleanStrictOrNull() ?: content.toLongOrNull()?.let { it != 0L }
        Value::String(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => s.parse::<i64>().ok().map(|n| n != 0),
        },
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        _ => None,
    }
}

/// 对应 `JsonObject.path(vararg keys)`：按路径取对象。
pub fn path<'a>(m: &'a JsonMap, keys: &[&str]) -> Option<&'a JsonMap> {
    let mut cur = m;
    for k in keys {
        cur = cur.get(*k)?.as_object()?;
    }
    Some(cur)
}

/// 图片地址统一成 https（B站等会给 `//i0.hdslb.com/...` 或 `http://`）。
///
/// 这里留着只是因为平台解析层一直按 `json::to_https` 引用。
pub fn to_https(u: &str) -> String {
    crate::present::text::to_https(u)
}

/// "1.2万" → 12000，"3亿" → 300000000，"--" → None。
///
pub fn parse_count(text: Option<&str>) -> Option<i64> {
    crate::present::text::parse_count(text)
}

/// "12:34" / "1:02:03" → 秒数。对应 Kotlin 的 `parseDuration`。
///
pub fn parse_duration(text: Option<&str>) -> Option<i32> {
    crate::present::text::parse_duration(text)
}

