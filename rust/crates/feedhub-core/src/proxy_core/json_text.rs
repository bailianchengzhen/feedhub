//! **保序 JSON 的写出 + 数字口径**，专门为"`proxy-core.json` 逐字节和 Kotlin 一样"。
//!
//! 为什么不直接用 [`crate::xhs::jsonord`]：那个是**读 + 写**一套，写出来的是**紧凑**（无缩进）文本，
//! 而 `ProxyCoreStore` 用的是 `Json { prettyPrint = true; encodeDefaults = true }`——4 空格缩进、
//! `": "` 冒号后一个空格、数组元素各占一行。读进来再写出去要逐字节一样，缩进和数字格式都得照抄。
//!
//! 数字是这一层唯一的难点：`kotlinx.serialization` 的 `JsonPrimitive` 内部存的是**原样的字面量文本**
//! （`content`），但**写出**时走的是 `Long` / `Double` 两条路（`JsonPrimitive` 的 `Serializer`）：
//!
//! | 输入 | 写出来 | 走的哪条 |
//! |---|---|---|
//! | `1` / `-7` / `0` / `-0` | `1` / `-7` / `0` / `0` | 能装进 `Long` → `Long` 路径，`-0` 就是 `0` |
//! | `9223372036854775808` | `9223372036854775808` | `ULong` 路径（`0..=u64::MAX` 里装得下） |
//! | `18446744073709551616` | `1.8446744073709552E19` | 溢出 → `Double` 路径 |
//! | `1.0` / `1.50` / `1e3` / `1.` | `1.0` / `1.5` / `1000.0` / `1.0` | 带小数点 / 指数 → `Double` 路径 |
//! | `-0.0` | `-0.0` | `Double` 路径保留负零 |
//! | `123.456e2` | `12345.6` | `Double` 路径 |
//! | `2.5e-310` | `2.5E-310` | `Double` 路径（次正规数也照 `Double.toString`） |
//!
//! `Double` 路径的文本口径就是 Java 的 `Double.toString`（= Rust 的 `f64::to_string` 那套
//! "最短往返 + 科学计数法阈值"）：`[1e-3, 1e7)` 用定点、其余用 `d.dddE±x`（大写 `E`、指数不带 `+`、
//! 指数至少一位）。两份实现都是 Ryu / Grisu 的"最短往返"，实测逐位一致
//! （`proxy_core::json_text::tests::double_text_matches_java` 用一批边界值守着）。
//!
//! 只给代理核心用：别的地方（信息流 / 平台解析）没有"读进来再写出去要逐字节一样"的需求，
//! 用 `serde_json` 就够，也别为了这个模块改全局的 `serde_json` 特性（见 `xhs/jsonord.rs` 的说明）。

/// 一个**要按 kotlinx 口径写出去**的值。顺序即写出顺序（对象用 `Vec`，不用 `Map`）。
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    /// 我们自己算出来的整数（端口、条数……）：`Long` 路径，原样十进制。
    Int(i64),
    /// 我们自己算出来的、超出 `i64` 的正整数：`ULong` 路径（原样十进制）。
    UInt(u64),
    /// 我们自己算出来的浮点：Java `Double.toString` 口径。
    Double(f64),
    /// **原始字面量**里的数字（从订阅 / 存档读进来的）：内部就是那串文本，和 kotlinx 的
    /// `JsonPrimitive.content` 一样。**写出时**按 `Long` / `ULong` / `Double` 三条路归一化，
    /// **规范 JSON（算节点 id）时**用原始字面量（`JsonPrimitive.toString()` 就是它）。
    Num(String),
    Str(String),
    Arr(Vec<V>),
    Obj(Vec<(String, V)>),
}

impl V {
    /// 把一个数字字面量包成值。
    ///
    /// **已经归一化过的字面量**（`443` / `1.5` / `1000.0`，也就是 kotlinx 自己写出来的那些）会变成
    /// `Int` / `UInt` / `Double`——这样"读进来 → 写出去 → 再读回来"两边**逐字段相等**，不会出现
    /// `Int(443)` 和 `Num("443")` 这种假差异。
    ///
    /// **不是归一化的**（`1.50` / `1e3` / 超长整数）留成 `Num`：算节点 id 的规范 JSON 要用**原始字面量**
    /// （Kotlin 的 `JsonPrimitive.toString()` 就是它），写出去时按 kotlinx 的口径归一化。
    pub fn number(literal: &str) -> V {
        let raw = literal.trim();
        let normalized = V::number_text(raw);
        if normalized != raw {
            return V::Num(raw.to_string());
        }
        let plain = !raw.contains('.') && !raw.contains('e') && !raw.contains('E');
        if plain {
            let (negative, digits) = match raw.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, raw.strip_prefix('+').unwrap_or(raw)),
            };
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                if !negative {
                    if let Ok(u) = digits.parse::<u64>() {
                        return if u <= i64::MAX as u64 {
                            V::Int(u as i64)
                        } else {
                            V::UInt(u)
                        };
                    }
                } else if let Ok(n) = raw.parse::<i64>() {
                    return V::Int(n);
                }
            }
        }
        match raw.parse::<f64>() {
            Ok(f) if double_text(f) == raw => V::Double(f),
            _ => V::Num(raw.to_string()),
        }
    }

    /// 按 kotlinx 的口径把一个数字字面量写成文本。
    ///
    /// 规则见模块说明：不带小数点 / 指数的先试 `i64`（`-0` 归成 `0`），再试 `u64`，都装不下就退化成
    /// `Double`；带小数点 / 指数的直接走 `Double`。
    pub fn number_text(literal: &str) -> String {
        let t = literal.trim();
        let plain = !t.contains('.') && !t.contains('e') && !t.contains('E');
        if plain {
            // `Long.parseLong` 先吃掉符号：`-0` → 0，`+0` → 0（Kotlin 的 `toLongOrNull` 认 `+`）
            let (negative, digits) = match t.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                if !negative {
                    if let Ok(u) = digits.parse::<u64>() {
                        // `0..=i64::MAX` 走 Long，上面那段走 ULong（和 kotlinx 一样：先 Long 再 ULong）
                        return u.to_string();
                    }
                } else if let Ok(n) = t.parse::<i64>() {
                    return n.to_string();
                }
            }
        }
        match t.parse::<f64>() {
            Ok(f) => double_text(f),
            // 连 f64 都解析不了的（不该出现：来源是已经解析过的 JSON）：原样留着，别把文件写坏
            Err(_) => t.to_string(),
        }
    }

    /// 字符串值（`JsonPrimitive` 里的字符串）。别的类型是 `None`（和 Kotlin 那边 `decodeFromString`
    /// 遇到类型不对就整份失败不同——这里只丢那一条，见 `model::ProxyCoreState::from_json_text` 的说明）。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            V::Str(s) => Some(s),
            _ => None,
        }
    }

    /// 整数值：整数原样、浮点截断、数字字面量 / 数字字符串都认（和 `JsonObject.long` 一个口径）。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            V::Int(n) => Some(*n),
            V::UInt(n) => Some(*n as i64),
            V::Double(f) => Some(*f as i64),
            V::Num(raw) => raw
                .parse::<i64>()
                .ok()
                .or_else(|| raw.parse::<f64>().ok().map(|f| f as i64))
                .or_else(|| raw.trim().parse::<i64>().ok()),
            V::Str(s) => s
                .parse::<i64>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
                .or_else(|| s.trim().parse::<i64>().ok()),
            _ => None,
        }
    }

    /// 布尔值（`true` / `"true"` / 数字非 0）。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            V::Bool(b) => Some(*b),
            V::Str(s) => match s.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => s.parse::<i64>().ok().map(|n| n != 0),
            },
            V::Int(n) => Some(*n != 0),
            _ => None,
        }
    }

    /// 写成一个 JSON 字符串字面量（kotlinx 口径：`"` `\` `\n` `\r` `\t` `\b` `\f` 短转义，
    /// 其余 `< 0x20` 写 `\u00xx`（**小写**），DEL / U+2028 / 非 ASCII 一律原样）。
    pub fn write_string(out: &mut String, s: &str) {
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
}

/// Java `Double.toString` 的口径（`parseFloat` 的逆）。
///
/// 规则（JDK 的 `Double.toString` 文档 + `sun.misc.FloatingDecimal`）：
/// - `NaN` / `Infinity` / `-Infinity` 各写这三个词（kotlinx 默认**拒绝**非有限值，这里只是不 panic）；
/// - `0.0` → `0.0`，`-0.0` → `-0.0`；
/// - 绝对值落在 `[1e-3, 1e7)`：**定点**，至少一位小数（`1.0` / `0.001` / `12345.6`）；
/// - 其余：`d.dddE±x`，尾数一位整数、至少一位小数、指数**不带 `+`**（`1.0E-4` / `1.8446744073709552E19`）。
///
/// Rust 的 `f64::to_string` 用的是同一套"最短往返"算法（Ryu），**指数阈值也一样**（`1e-3`…`1e7` 定点），
/// 差别只在指数字母小写、带 `+` 号、以及"整数值要不要补 `.0`"。这里把这几处抹平。
pub fn double_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        // 负零要靠 `is_sign_negative`（`== 0.0` 对 +0 和 -0 都为真）
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let abs = value.abs();
    if (1e-3..1e7).contains(&abs) {
        // 定点：Rust 会给 `1` 而不是 `1.0`，补上小数位
        let mut s = format!("{value}");
        if !s.contains('.') {
            s.push_str(".0");
        }
        // `1e-3` 这种刚好在边界上的值 Rust 也可能给科学计数法；这里兜一层
        if s.contains('e') || s.contains('E') {
            return scientific(value);
        }
        return s;
    }
    scientific(value)
}

/// 科学计数法：`d.dddE±x`（尾数至少一位小数，指数不带 `+`、至少一位）。
fn scientific(value: f64) -> String {
    // `{:e}` 给的是 `1.5e7` / `-1.5e-7`：尾数最短往返、指数不带前导零（和 Java 一样）
    let raw = format!("{value:e}");
    let (mantissa, exp) = match raw.split_once('e') {
        Some((m, e)) => (m, e),
        None => (raw.as_str(), "0"),
    };
    let mut mantissa = mantissa.to_string();
    if !mantissa.contains('.') {
        mantissa.push_str(".0");
    }
    // Rust 的 `{:e}` 已经去掉了 `+`；只为稳妥再处理一次（Java 只在负指数时写 `-`）
    let exp = exp.strip_prefix('+').unwrap_or(exp);
    format!("{mantissa}E{exp}")
}

/// 按 kotlinx 的 `prettyPrint` 口径写一份 JSON：4 空格缩进、`": "`、数组元素各占一行、
/// 空对象 `{}` / 空数组 `[]` 写在一行、**末尾不带换行**（`encodeToString` 的结论）。
pub fn write_pretty(value: &V) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
}

fn write_value(out: &mut String, value: &V, depth: usize) {
    match value {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Int(n) => out.push_str(&n.to_string()),
        V::UInt(n) => out.push_str(&n.to_string()),
        V::Double(f) => out.push_str(&double_text(*f)),
        V::Num(raw) => out.push_str(&V::number_text(raw)),
        V::Str(s) => V::write_string(out, s),
        V::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                push_indent(out, depth + 1);
                write_value(out, item, depth + 1);
            }
            out.push('\n');
            push_indent(out, depth);
            out.push(']');
        }
        V::Obj(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                push_indent(out, depth + 1);
                V::write_string(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1);
            }
            out.push('\n');
            push_indent(out, depth);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

