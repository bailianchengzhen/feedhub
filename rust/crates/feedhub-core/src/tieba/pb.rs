//! 极小的 protobuf 读取器：只做"遍历字段 + 按字段号取标量 / 字节"。
//!
//! 为什么自己写而不引入 prost：当前**一个 crate 都不新增**（`cargo build --offline` 要能过，机器上的
//! registry 里也没有 protobuf 相关的包）；而贴吧这几个响应我们只读十来个字段，手写一个 reader 比生成代码轻。
//! 语义对齐 Kotlin 侧用的 Square Wire（proto3）：
//!
//! - 未知字段号 / 未知 wire type **跳过**（proto3 的约定，也是 Wire 的行为）；
//! - 同一个标量字段出现多次时**后出现的赢**（Wire 也是最后覆盖）；
//! - `int32` / `uint32` 按 varint 的低 32 位截断（负数在 protobuf 里是 10 字节 varint 的补码）；
//! - 字符串按 UTF-8 解码，非法字节按替换字符处理（Wire 的 `toStringUtf8()` 也是这样）；
//! - **`START_GROUP` / `END_GROUP`（wire type 3 / 4）按 proto2 的规则整段跳过**（可嵌套；不配对的
//!   `END_GROUP` 报错）——Wire 的处理也是这样：`0x0B 0x0C`（一个空 group）它当成"没有字段"。
//! 对拍发现它对 group 宽容、原来这里报错，两边就分叉了。
//!
//! 不做的事：packed repeated（贴吧 / 弹幕这几个消息里没有 repeated 标量）。

use std::fmt;

/// 读 protobuf 出错（截断、非法 varint、字段号 0……）。上层统一翻成 `ParseException`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PbError(String);

impl PbError {
    fn new(msg: impl Into<String>) -> Self {
        PbError(msg.into())
    }

    pub fn detail(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一个字段的值。长度分隔的字段（字符串 / 嵌套消息）给的是原始字节。
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

/// 按顺序遍历 [buf] 里的所有字段。闭包收到的字段号一定是非 0；闭包里解码嵌套消息时可以直接用 `?`。
pub fn walk<'a>(
    buf: &'a [u8],
    mut visit: impl FnMut(u32, Value<'a>) -> Result<(), PbError>,
) -> Result<(), PbError> {
    let mut pos = 0usize;
    while pos < buf.len() {
        let (tag, next) = read_varint(buf, pos)?;
        pos = next;
        let field = (tag >> 3) as u32;
        if field == 0 {
            return Err(PbError::new("字段号是 0"));
        }
        let wire = (tag & 7) as u8;
        // group 的两个 wire type 不产出值：START_GROUP 整段跳过，END_GROUP 没有配对就是坏消息。
        match wire {
            3 => {
                pos = skip_group(buf, pos)?;
                continue;
            }
            4 => return Err(PbError::new("不配对的 END_GROUP")),
            _ => {}
        }
        let value = match wire {
            0 => {
                let (n, next) = read_varint(buf, pos)?;
                pos = next;
                Value::Varint(n)
            }
            1 => {
                let end = pos + 8;
                if end > buf.len() {
                    return Err(PbError::new("固定 64 位字段被截断"));
                }
                let n = u64::from_le_bytes(buf[pos..end].try_into().expect("8 字节"));
                pos = end;
                Value::Fixed64(n)
            }
            2 => {
                let (len, next) = read_varint(buf, pos)?;
                pos = next;
                let end = pos
                    .checked_add(len as usize)
                    .ok_or_else(|| PbError::new("长度字段溢出"))?;
                if end > buf.len() {
                    return Err(PbError::new("长度分隔字段被截断"));
                }
                let bytes = &buf[pos..end];
                pos = end;
                Value::Bytes(bytes)
            }
            5 => {
                let end = pos + 4;
                if end > buf.len() {
                    return Err(PbError::new("固定 32 位字段被截断"));
                }
                let n = u32::from_le_bytes(buf[pos..end].try_into().expect("4 字节"));
                pos = end;
                Value::Fixed32(n)
            }
            // 3 / 4 在上面处理掉了；6 / 7 是保留值：出现就说明不是我们认识的消息。
            other => return Err(PbError::new(format!("不支持的 wire type {other}"))),
        };
        visit(field, value)?;
    }
    Ok(())
}

/// 从 `START_GROUP` 之后跳过一整个 group（可嵌套），返回 `END_GROUP` 之后的位置。
///
/// 只数层级、不比字段号：Wire 的 `skipGroup` 也是"读到对应的 END_GROUP 为止"，
/// 畸形输入下两边的差别只是错误文案（外层统一翻成 `ParseException`）。
fn skip_group(buf: &[u8], mut pos: usize) -> Result<usize, PbError> {
    let mut depth = 1usize;
    while pos < buf.len() {
        let (tag, next) = read_varint(buf, pos)?;
        pos = next;
        match (tag & 7) as u8 {
            0 => {
                let (_, next) = read_varint(buf, pos)?;
                pos = next;
            }
            1 => pos = pos.checked_add(8).ok_or_else(|| PbError::new("固定 64 位字段溢出"))?,
            2 => {
                let (len, next) = read_varint(buf, pos)?;
                pos = next
                    .checked_add(len as usize)
                    .ok_or_else(|| PbError::new("长度字段溢出"))?;
            }
            5 => pos = pos.checked_add(4).ok_or_else(|| PbError::new("固定 32 位字段溢出"))?,
            3 => depth += 1,
            4 => {
                depth -= 1;
                if depth == 0 {
                    return if pos > buf.len() {
                        Err(PbError::new("group 被截断"))
                    } else {
                        Ok(pos)
                    };
                }
            }
            other => return Err(PbError::new(format!("不支持的 wire type {other}"))),
        }
        if pos > buf.len() {
            return Err(PbError::new("group 被截断"));
        }
    }
    Err(PbError::new("group 没有闭合"))
}

fn read_varint(buf: &[u8], mut pos: usize) -> Result<(u64, usize), PbError> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let byte = *buf.get(pos).ok_or_else(|| PbError::new("varint 被截断"))?;
        pos += 1;
        if shift >= 64 {
            return Err(PbError::new("varint 太长"));
        }
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok((result, pos));
        }
        shift += 7;
    }
}

/// 有符号 64 位（`int64` / `uint64` 都按位解释）。字段被写成了长度分隔（类型不对）时算 0——
/// 真实响应不会有这种形态，对拍样本里也不造它（Kotlin 的 Wire 在那种情况下读到的是一段无意义的字节）。
pub fn as_i64(v: Value) -> i64 {
    match v {
        Value::Varint(n) | Value::Fixed64(n) => n as i64,
        Value::Fixed32(n) => n as i32 as i64,
        Value::Bytes(_) => 0,
    }
}

/// `int32`：varint 的低 32 位（负数就是这样带过来的）。
pub fn as_i32(v: Value) -> i32 {
    as_i64(v) as i32
}

/// `uint32`。
pub fn as_u32(v: Value) -> u32 {
    as_i64(v) as u32
}

/// `uint64`（Wire 在 Kotlin 侧给的是 `ULong` / `Long` 两种，贴吧这边只用来拼 id 字符串）。
pub fn as_u64(v: Value) -> u64 {
    as_i64(v) as u64
}

/// 字符串 / 嵌套消息的原始字节。标量出现在字符串字段上时按 Wire 的行为当作空串。
pub fn as_bytes<'a>(v: Value<'a>) -> &'a [u8] {
    match v {
        Value::Bytes(b) => b,
        _ => &[],
    }
}

/// 字符串：
pub fn as_string(v: Value) -> String {
    String::from_utf8_lossy(as_bytes(v)).into_owned()
}

/// 只有"长度分隔"才算字符串；别的 wire type 说明消息坏了。
pub fn require_bytes<'a>(v: Value<'a>) -> Result<&'a [u8], PbError> {
    match v {
        Value::Bytes(b) => Ok(b),
        _ => Err(PbError::new("字符串 / 消息字段不是长度分隔的")),
    }
}
