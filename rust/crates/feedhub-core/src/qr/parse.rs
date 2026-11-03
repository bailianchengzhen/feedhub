//! 二维码接口响应的**解析 + 判错**。
//!
//! 规则只有两条（和 Kotlin 一样）：
//! 1. 响应必须是**一个 JSON 对象**（数组 / 标量 / 坏 JSON 都不行）；
//! 2. 出错信息里**不带响应内容**——成功的扫码响应里有凭据（`login_info.session` 这种），
//!    而 `parseJsonObject` 的文案会贴 body 前 200 字，所以扫码头不能用它。
//!
//! 用仓库自己的保序 JSON 解析器（[`crate::xhs::jsonord`]）：数字保留原始字面量、对象保持文档顺序，
//! 写成紧凑 JSON 之后交给宿主的 kotlinx 变成 `JsonObject`——宿主只做"模型表示"，判错在这里。
//! 写出来的串和原串**语义等价**（键顺序不变、数字字面量不变、字符串转义后的值不变，重复键两边都是"后写的赢"）。

use crate::xhs::jsonord::{self, J};

/// 解析失败的两档（Kotlin `parseQrJson` 的两个 catch 分支，文案不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// 不是合法 JSON（`FeedJson.parseToJsonElement` 抛了）。
    Invalid,
    /// 是 JSON，但不是**对象**（数组 / 标量）。
    NotObject,
}

/// 解析二维码接口的响应：是 JSON 对象就返回**紧凑 JSON**，否则带上是哪一档错
/// （调用方抛 `ParseException`，**文案里不带 body**）。
pub fn parse(body: &str) -> Result<String, ParseError> {
    let value = jsonord::parse(body).map_err(|_| ParseError::Invalid)?;
    if !matches!(value, J::Obj(_)) {
        return Err(ParseError::NotObject);
    }
    let mut out = String::with_capacity(body.len());
    write_compact(&value, &mut out);
    Ok(out)
}

/// [`parse`] 的 Option 版本（Rust 侧单测 / 内部调用方便）。
pub fn parse_object(body: &str) -> Option<String> {
    parse(body).ok()
}

/// 保序 JSON → 紧凑文本（不换行、不缩进，数字用原来的字面量）。
pub fn write_compact(value: &J, out: &mut String) {
    match value {
        J::Null => out.push_str("null"),
        J::Bool(true) => out.push_str("true"),
        J::Bool(false) => out.push_str("false"),
        // 数字保留原始字面量：`1.50` 还是 `1.50`（kotlinx 读出来都是同一个 `JsonPrimitive` 内容）
        J::Num(raw) => out.push_str(raw),
        J::Str(text) => jsonord::quote(out, text),
        J::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        J::Obj(fields) => {
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jsonord::quote(out, key);
                out.push(':');
                write_compact(item, out);
            }
            out.push('}');
        }
    }
}

