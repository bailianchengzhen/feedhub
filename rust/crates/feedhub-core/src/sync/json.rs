//! **和 kotlinx.serialization 逐字节一致的 JSON 读写**（资料库同步的快照 / 报文就靠它）。
//!
//! 这一层不是通用 JSON 库，是**为一个验收门服务的**：同一个 `LibrarySnapshot` 在 Kotlin 和 Rust 两边
//! 编码出来的字节必须**完全相同**（字面量顺序、数字写法、`\u` 十六进制的大小写、要不要写默认值、
//! 字符串里的 `</`……每一样都要对上）。所以这里不派生 `serde` 结构体，而是：
//!
//! - **写**：照着 kotlinx 的 `Composer` / `printQuoted` 手工拼（`StringOps.printQuoted` 的转义表见
//!   [`push_quoted`]），字段顺序 = Kotlin `data class` 的声明顺序（kotlinx 按声明顺序写）；
//! - **读**：写成 [`Value`] 树（保留对象字段的**插入顺序**和数字的**原始字面量**——`1.0` 和 `1` 是两回事），
//!   由各自的编解码函数按键取值、按 Kotlin 的默认值补空。
//!
//! 为什么不用 `serde_json::Value`：它会把数字规范化（`1.0` → `1.0` 能保住，但 `1E2` → `100.0`）、
//! 又不保留"这个数字原来是字符串还是数字"以外的东西；更重要的是 `item_json` 这种**透传的子树**
//! 必须原样写回去（Kotlin 那边是 `JsonElement` 直接打印），`serde_json` 的 `arbitrary_precision`
//! 虽然能保原始字面量，但打印时会带上它自己的私有结构。自己写一份 [`Value`] 的代价只有 ~150 行。
//!
//! ## 和 kotlinx 对齐的几条口径（每一条都有单测）
//!
//! | 口径 | 值 |
//! |---|---|
//! | 转义 | `"` `\` `\b` `\t` `\n` `\f` `\r` 的短转义；其它 `< 0x20` 写成 `\u00XX`（**小写**十六进制） |
//! | 非 ASCII | **不转义**，直接写 UTF-8（`printQuoted` 的 `ESCAPE_STRINGS` 只覆盖 `< 0x20` 和三个特例） |
//! | `/` 和 `<` | 不转义（kotlinx 不做 HTML 安全转义） |
//! | `encodeDefaults = false` | 等于默认值的字段**不写**（快照编解码就是这个模式，见 `snapshot.rs`） |
//! | `explicitNulls = false` | `null` 字段**不写** |
//! | 枚举 | 写成枚举名本身（`"VIDEO"`），不是序号 |
//! | 长整型 | 十进制，没有指数、没有 `.0` |

use std::collections::BTreeMap;

/// 一个 JSON 值。**对象保留插入顺序**（`Vec` 而不是 `Map`），因为快照的字段顺序是编码契约的一部分。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// 数字的**原始字面量**。`1.0` / `1` / `1e5` 各是各的，打印时原样写回。
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            // 和 Kotlin 的 `JsonPrimitive.longOrNull` 差不多的宽松：整数、`1.0` 都认
            Value::Number(raw) => raw.parse::<i64>().ok().or_else(|| {
                raw.parse::<f64>().ok().map(|f| {
                    // Kotlin 的 `toLong()` 是**截断**（向零取整），不是四舍五入
                    f.trunc() as i64
                })
            }),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// 取对象的字段（`None` = 没有这个键）。**不区分"没有"和 `null`**：Kotlin 那边 `JsonObject[key]`
    /// 拿到 `JsonNull` 时的处理各字段不同，所以由调用方用 [`Value::is_null`] 自己判。
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// 取一个字符串字段；字段不存在 / 是 `null` 都返回 `None`。
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }

    pub fn i64_field(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.as_i64())
    }

    pub fn bool_field(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    /// 数组字段；不存在 / `null` 都是空（Kotlin 的默认值是 `emptyList()`）。
    pub fn array_field(&self, key: &str) -> &[Value] {
        self.get(key).and_then(|v| v.as_array()).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn number(n: i64) -> Value {
        Value::Number(n.to_string())
    }

    /// 按 kotlinx 的 `printQuoted` 口径写一个 JSON 字符串（含两边的引号）。
    pub fn quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        push_quoted(&mut out, s);
        out
    }
}

/// kotlinx `StringOps.printQuoted`：`"` `\` `\b` `\t` `\n` `\f` `\r` 用短转义，其余 `< 0x20` 用
/// `\u00xx`（**小写**十六进制）；`/` `<` 和所有非 ASCII 字符原样写（UTF-8）。
///
/// 为什么这必须和 kotlinx 一字不差：同步文件的 AAD 覆盖整段文件头，快照又整体被 gzip + 加密；
/// 只要有一处转义不同，两边算出来的字节就不同，"两边编码逐字节相同"那条门就过不去。
pub fn push_quoted(out: &mut String, s: &str) {
    out.push('"');
    let mut run_start = 0usize; // 还没被转义的那段起点（按字节）
    for (i, ch) in s.char_indices() {
        let escaped: Option<&str> = match ch {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{8}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{c}' => Some("\\f"),
            '\r' => Some("\\r"),
            c if (c as u32) < 0x20 => {
                // 和其它分支一样：先把之前那段原样刷出去，再写 \u00xx
                out.push_str(&s[run_start..i]);
                out.push_str(&format!("\\u{:04x}", c as u32));
                run_start = i + c.len_utf8();
                continue;
            }
            _ => None,
        };
        if let Some(esc) = escaped {
            out.push_str(&s[run_start..i]);
            out.push_str(esc);
            run_start = i + ch.len_utf8();
        }
    }
    out.push_str(&s[run_start..]);
    out.push('"');
}

// ===========================================================================
// 写：手工拼（字段顺序由调用方决定，和 Kotlin data class 的声明顺序一致）
// ===========================================================================

/// 一个小的 JSON 对象构造器：只做"按顺序追加字段"，并且实现 kotlinx 的
/// `encodeDefaults = false` + `explicitNulls = false`（默认值 / `null` 不写）。
///
/// 为什么不做成通用的 `Value::to_json`：**默认值不写**这条规则要知道每个字段的默认值是什么，
/// 而那只有各 record 的编解码函数知道；硬塞进通用层反而会把"哪个字段没写"藏起来。
pub struct ObjectWriter {
    out: String,
    /// 已经写过字段吗（决定要不要先加逗号）。
    first: bool,
}

impl Default for ObjectWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectWriter {
    pub fn new() -> Self {
        ObjectWriter {
            out: String::from("{"),
            first: true,
        }
    }

    fn comma(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.out.push(',');
        }
    }

    /// 写一个名字（会被引号包起来）+ 已经拼好的值。
    pub fn field_raw(&mut self, name: &str, value: &str) {
        self.comma();
        push_quoted(&mut self.out, name);
        self.out.push(':');
        self.out.push_str(value);
    }

    pub fn field_str(&mut self, name: &str, value: &str) {
        self.comma();
        push_quoted(&mut self.out, name);
        self.out.push(':');
        push_quoted(&mut self.out, value);
    }

    pub fn field_bool(&mut self, name: &str, value: bool) {
        self.field_raw(name, if value { "true" } else { "false" });
    }

    pub fn field_i64(&mut self, name: &str, value: i64) {
        self.comma();
        push_quoted(&mut self.out, name);
        self.out.push(':');
        self.out.push_str(&value.to_string());
    }

    /// 写一个 `Option<...>`：`None` 不写（`explicitNulls = false`）。
    pub fn field_opt_str(&mut self, name: &str, value: Option<&str>) {
        if let Some(v) = value {
            self.field_str(name, v);
        }
    }

    pub fn field_opt_i64(&mut self, name: &str, value: Option<i64>) {
        if let Some(v) = value {
            self.field_i64(name, v);
        }
    }

    /// 写一个"默认值不写"的标量：只有 `value != default` 才写。
    pub fn field_i64_non_default(&mut self, name: &str, value: i64, default: i64) {
        if value != default {
            self.field_i64(name, value);
        }
    }

    pub fn field_bool_non_default(&mut self, name: &str, value: bool, default: bool) {
        if value != default {
            self.field_bool(name, value);
        }
    }

    pub fn field_str_non_default(&mut self, name: &str, value: &str, default: &str) {
        if value != default {
            self.field_str(name, value);
        }
    }

    /// 写一串已经拼好的值（数组的元素）。空数组写成 `[]`（Kotlin 的 `emptyList()` 是**非默认值**，
    /// 总是写出来——四个 list 字段都没有默认值以外的"省略"规则）。
    pub fn field_array(&mut self, name: &str, items: &[String]) {
        self.comma();
        push_quoted(&mut self.out, name);
        self.out.push(':');
        self.out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            self.out.push_str(item);
        }
        self.out.push(']');
    }

    pub fn finish(self) -> String {
        let mut out = self.out;
        out.push('}');
        out
    }
}

/// 把一个 [`Value`] 子树的**规范化**写法拼出来（`item_json` 这种透传字段用）。
///
/// 注意：这和"原样打印原始字节"**不是**一回事。Kotlin 那边 `item` 是 `JsonElement`，
/// 由 kotlinx 的 `JsonElement.toString()` 写出来（无缩进、保留数字字面量、对象按解析时的顺序），
/// 这里逐条复刻：
/// - 对象的键按**解析时的顺序**（[`Value::Object`] 是 `Vec`）；
/// - 数字按原始字面量；
/// - 字符串按同一套转义。
///
/// 和 Kotlin 的差别只在"同一份 JSON 有两种等价写法"上（`1.0` vs `1`、`\u00e9` vs `é`）：
/// 快照里的 `item_json` 是**我们自己写进库**的那份（`FeedJson.encodeToString(FeedJson.decode(...))`），
/// 所以两边看到的是同一串字节，不会漂。
pub fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(raw) => out.push_str(raw),
        Value::String(s) => push_quoted(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_quoted(out, k);
                out.push(':');
                write_value(out, v);
            }
            out.push('}');
        }
    }
}

pub fn to_json_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

// ===========================================================================
// 读：一个够用的递归下降解析器
// ===========================================================================

/// 解析失败（消息里有位置，方便对着一份坏文件排障；**不含文件内容**）。
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(format!("JSON 末尾有多余内容（偏移 {}）", p.pos));
    }
    Ok(value)
}

/// 一个 UTF-8 字符占几个字节（看首字节；非法首字节返回 1，交给 `from_utf8` 去报错）。
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("JSON 在偏移 {} 处有意外字符 {:?}", self.pos, c as char)),
            None => Err("JSON 提前结束".to_string()),
        }
    }

    fn literal(&mut self, text: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(value)
        } else {
            Err(format!("JSON 在偏移 {} 处的字面量不对", self.pos))
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => self.pos += 1,
                _ => break,
            }
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "JSON 数字不是合法 UTF-8".to_string())?
            .to_string();
        if raw.is_empty() || raw == "-" {
            return Err(format!("JSON 在偏移 {start} 处的数字不完整"));
        }
        Ok(Value::Number(raw))
    }

    fn string(&mut self) -> Result<String, String> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("JSON 字符串没有结束引号".to_string());
            };
            match c {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        return Err("JSON 转义没有后续字符".to_string());
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.hex4()?;
                            // 代理对：高位后面必须跟 `\uXXXX` 低位
                            if (0xD800..0xDC00).contains(&code) {
                                if self.peek() != Some(b'\\') {
                                    return Err("JSON 代理对不完整".to_string());
                                }
                                self.pos += 1;
                                if self.peek() != Some(b'u') {
                                    return Err("JSON 代理对不完整".to_string());
                                }
                                self.pos += 1;
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err("JSON 代理对不完整".to_string());
                                }
                                let combined =
                                    0x10000u32 + ((code - 0xD800) as u32) * 0x400 + (low - 0xDC00) as u32;
                                out.push(
                                    char::from_u32(combined)
                                        .ok_or_else(|| "JSON 有非法码点".to_string())?,
                                );
                            } else {
                                out.push(
                                    char::from_u32(code as u32)
                                        .ok_or_else(|| "JSON 有非法码点".to_string())?,
                                );
                            }
                        }
                        other => {
                            return Err(format!(
                                "JSON 在偏移 {} 处有不认识的转义 \\{}",
                                self.pos - 1,
                                other as char
                            ))
                        }
                    }
                }
                _ => {
                    // 取一个完整的 UTF-8 字符。**不要**对 `&bytes[pos..]` 整体做 `from_utf8`
                    // 这里按首字节算出这个字符占几个字节，只解码那一段。
                    let width = utf8_width(c);
                    if self.pos + width > self.bytes.len() {
                        return Err("JSON 字符串里有被截断的 UTF-8 字符".to_string());
                    }
                    let slice = &self.bytes[self.pos..self.pos + width];
                    let text = std::str::from_utf8(slice)
                        .map_err(|_| "JSON 字符串里有非法 UTF-8".to_string())?;
                    let ch = text
                        .chars()
                        .next()
                        .ok_or_else(|| "JSON 字符串提前结束".to_string())?;
                    out.push(ch);
                    self.pos += width;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        if self.pos + 4 > self.bytes.len() {
            return Err("JSON 的 \\u 转义不完整".to_string());
        }
        let mut value = 0u16;
        for _ in 0..4 {
            let c = self.bytes[self.pos];
            self.pos += 1;
            let digit = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err("JSON 的 \\u 转义里有非十六进制字符".to_string()),
            };
            value = (value << 4) | digit as u16;
        }
        Ok(value)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(format!("JSON 数组在偏移 {} 处没有结束", self.pos)),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.pos += 1; // '{'
        let mut fields: Vec<(String, Value)> = Vec::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(fields));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("JSON 对象的键在偏移 {} 处不是字符串", self.pos));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("JSON 对象在偏移 {} 处缺冒号", self.pos));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            // 重复键：**后一个覆盖前一个，但位置留在第一次出现的地方**（kotlinx 的
            // LinkedHashMap 语义：put 已存在的键不改变迭代顺序）。真实快照里不会出现，
            // 但"云盘里被塞了坏文件"时会碰上，行为要和 Kotlin 一致。
            if let Some(&index) = seen.get(&key) {
                fields[index].1 = value;
            } else {
                seen.insert(key.clone(), fields.len());
                fields.push((key, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(fields));
                }
                _ => return Err(format!("JSON 对象在偏移 {} 处没有结束", self.pos)),
            }
        }
    }
}

