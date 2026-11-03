//! B站 的**分段弹幕**：`GET /x/v2/dm/wbi/web/seg.so`（WBI 签名）的响应是**裸 protobuf**
//! （`DmSegMobileReply`，见 `source-bilibili/src/main/proto/dm.proto`）→ [`crate::play::Danmaku`]。
//!
//! 逐条对照 `source-bilibili/BiliDanmakuParser.kt`：
//!
//! - 每段 6 分钟（`danmaku_segment_ms` 在详情里给），`segment_index` 从 1 开始；
//! - 正文空白的弹幕直接丢掉（B站 的脏数据里有；空白口径是 **Kotlin 的 `isBlank`**，含 NBSP 类——
//!   真的有一条"正文只有一个 U+00A0"的弹幕，Java 的 `Character.isWhitespace` 不认它，要按 Kotlin 来）；
//! - `mode`：1 / 2 / 3 滚动、4 底部、5 顶部，其余（6 逆向、7 高级、8 代码、9 BAS）归 `Other`；
//! - `id` 优先用 `id_str`（`int64` 的十进制字符串，超过 2^53 也不会被浮点吃掉），没有就用 `id`；
//! - `color` 是 `uint32`，只留低 24 位；`fontsize <= 0` 当 25；`weight <= 0` 当没有。
//!
//! protobuf 读取器用的是**贴吧那份**（[`crate::pb`]，`tieba/pb.rs` 的手写实现），语义对齐 Kotlin 侧的
//! Square Wire（未知字段跳过、同一标量后出现的赢、`int32` 取 varint 低 32 位、字符串 UTF-8 lossy）。
//!
//! 已知的口径差（只有手工构造的坏字节才看得出来）：字段的 wire type 和 `.proto` 不符时 Wire 会抛异常、
//! 这里读成默认值（当字段不存在）。真实的 `seg.so` 不会出现这种形态。

use crate::hosted::is_kotlin_blank;
use crate::model::BiliError;
use crate::pb;
use crate::play::{Danmaku, DanmakuMode};

/// 一段弹幕的响应 → 弹幕列表（**顺序就是响应里的顺序**，前端自己按时间排）。
///
/// 空响应（超出视频长度的段 B站 回 304 + 空 body）是**空列表，不是错误**。
/// 不是合法 protobuf 时是 [`BiliError::Parse`]，文案和 Kotlin 的 `ParseException` 一字不差。
pub fn segment(bytes: &[u8]) -> Result<Vec<Danmaku>, BiliError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    decode_reply(bytes).map_err(|_| BiliError::Parse {
        detail: format!("弹幕分段不是合法的 protobuf（{} 字节）", bytes.len()),
    })
}

/// `DmSegMobileReply { repeated DanmakuElem elems = 1; int32 state = 2; }`。
fn decode_reply(bytes: &[u8]) -> Result<Vec<Danmaku>, pb::PbError> {
    let mut out = Vec::new();
    pb::walk(bytes, |field, value| {
        // 只认 elems；`state`（是否已关闭弹幕）和未知字段跳过。
        if field == 1 {
            let elem = pb::require_bytes(value)?;
            if let Some(danmaku) = decode_elem(elem)? {
                out.push(danmaku);
            }
        }
        Ok(())
    })?;
    Ok(out)
}

/// `DanmakuElem`（字段号见 `dm.proto`；没用到的字段跳过）。
fn decode_elem(bytes: &[u8]) -> Result<Option<Danmaku>, pb::PbError> {
    let mut id: i64 = 0;
    let mut progress: i32 = 0;
    let mut mode: i32 = 0;
    let mut font_size: i32 = 0;
    let mut color: u32 = 0;
    let mut content = String::new();
    let mut weight: i32 = 0;
    let mut id_str = String::new();
    pb::walk(bytes, |field, value| {
        match field {
            1 => id = pb::as_i64(value),
            2 => progress = pb::as_i32(value),
            3 => mode = pb::as_i32(value),
            4 => font_size = pb::as_i32(value),
            5 => color = pb::as_u32(value),
            7 => content = pb::as_string(value),
            9 => weight = pb::as_i32(value),
            12 => id_str = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    if is_kotlin_blank(&content) {
        return Ok(None);
    }
    Ok(Some(Danmaku {
        // `id_str.ifEmpty { id.toString() }`
        id: if id_str.is_empty() { id.to_string() } else { id_str },
        // `progress.toLong().coerceAtLeast(0)`
        time_ms: (progress as i64).max(0),
        mode: mode_of(mode),
        text: content,
        // `color and 0xFFFFFF`（uint32 → Int 的低 32 位，再取低 24 位）
        color: (color & 0x00FF_FFFF) as i32,
        font_size: if font_size > 0 { font_size } else { 25 },
        weight: if weight > 0 { Some(weight) } else { None },
    }))
}

/// `BiliDanmakuParser.mode`：1 / 2 / 3 滚动，4 底部，5 顶部，其余 OTHER。
pub fn mode_of(mode: i32) -> DanmakuMode {
    match mode {
        1 | 2 | 3 => DanmakuMode::Scroll,
        4 => DanmakuMode::Bottom,
        5 => DanmakuMode::Top,
        _ => DanmakuMode::Other,
    }
}

