//! 验证码父子进程管道协议编解码（对齐 `HelperProtocol.kt`）。
//!
//! 大端二进制格式 + Java Modified UTF-8 兼容编解码。

pub const VERSION: i32 = 1;

pub const CMD_OPEN: u8 = 1;
pub const CMD_RESIZE: u8 = 2;
pub const CMD_MOUSE: u8 = 3;
pub const CMD_WHEEL: u8 = 4;
pub const CMD_KEY: u8 = 5;
pub const CMD_FOCUS: u8 = 6;
pub const CMD_CLOSE: u8 = 7;

pub const EVT_HELLO: u8 = 101;
pub const EVT_STATUS: u8 = 102;
pub const EVT_FRAME: u8 = 103;
pub const EVT_CURSOR: u8 = 104;
pub const EVT_LOADED: u8 = 105;
pub const EVT_FAILED: u8 = 106;
pub const EVT_BYE: u8 = 107;

pub const MAX_SIDE: i32 = 8192;
pub const MAX_RECTS: i32 = 4096;
pub const MAX_TEXT_LEN: usize = 500;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("数据不足：需要 {needed} 字节，剩余 {available} 字节")]
    UnexpectedEof { needed: usize, available: usize },
    #[error("未知命令类型：{0}")]
    UnknownCommand(u8),
    #[error("未知事件类型：{0}")]
    UnknownEvent(u8),
    #[error("坏帧头：{0}")]
    BadFrameHeader(String),
    #[error("坏脏区：{0}x{1}")]
    BadRect(i32, i32),
    #[error("Modified UTF-8 编码过长：字节数超过 65535")]
    UtfTooLong,
    #[error("Modified UTF-8 格式损坏：{0}")]
    MalformedUtf(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelperCommand {
    Open {
        url: String,
        width: i32,
        height: i32,
        scale: f32,
    },
    Resize {
        width: i32,
        height: i32,
        scale: f32,
    },
    Mouse {
        id: i32,
        x: i32,
        y: i32,
        modifiers: i32,
        button: i32,
        click_count: i32,
        stamp: i64,
    },
    Wheel {
        x: i32,
        y: i32,
        modifiers: i32,
        rotation: i32,
        precise: f64,
        stamp: i64,
    },
    Key {
        id: i32,
        key_code: i32,
        key_char: u16, // Java Char 16-bit
        modifiers: i32,
        location: i32,
        stamp: i64,
    },
    Focus {
        focused: bool,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelperEvent {
    Hello {
        version: i32,
        pid: i64,
    },
    Status {
        text: String,
    },
    Frame {
        width: i32,
        height: i32,
        rects: i32,
        paint_stamp: i64,
        input_stamp: i64,
    },
    Cursor {
        cursor_type: i32,
    },
    Loaded {
        http_status: i32,
    },
    Failed {
        message: String,
    },
    Bye,
}

/// 按 UTF-16 code unit 数截取字符串的前 `max_units` 个单元（对齐 Kotlin `String.take(n)`）。
/// 保证在合法标量边界切割，不切碎代理对。
pub fn truncate_utf16(s: &str, max_units: usize) -> &str {
    let mut count = 0;
    for (idx, c) in s.char_indices() {
        let units = c.len_utf16();
        if count + units > max_units {
            return &s[..idx];
        }
        count += units;
    }
    s
}

/// Java Modified UTF-8 编码：
/// 1. 2 字节无符号大端字节长度前缀；
/// 2. U+0000 编成两字节 0xC0 0x80；
/// 3. U+0001..=U+007F 编成单字节；
/// 4. U+0080..=U+07FF 编成两字节；
/// 5. U+0800..=U+FFFF 编成三字节；
/// 6. U+10000..=U+10FFFF（增补字符）拆成 UTF-16 代理对，各自编成 3 字节 CESU-8（共 6 字节）。
pub fn encode_java_utf(s: &str, out: &mut Vec<u8>) -> Result<(), CodecError> {
    let mut utflen: usize = 0;
    for c in s.chars() {
        let u = c as u32;
        if (0x0001..=0x007F).contains(&u) {
            utflen += 1;
        } else if u > 0x07FF {
            if u > 0xFFFF {
                utflen += 6;
            } else {
                utflen += 3;
            }
        } else {
            utflen += 2;
        }
    }
    if utflen > 65535 {
        return Err(CodecError::UtfTooLong);
    }
    out.extend_from_slice(&(utflen as u16).to_be_bytes());
    for c in s.chars() {
        let u = c as u32;
        if (0x0001..=0x007F).contains(&u) {
            out.push(u as u8);
        } else if u > 0x07FF {
            if u > 0xFFFF {
                let v = u - 0x10000;
                let high = 0xD800 + (v >> 10);
                let low = 0xDC00 + (v & 0x3FF);
                // 高代理项 (3 字节)
                out.push((0xE0 | (high >> 12)) as u8);
                out.push((0x80 | ((high >> 6) & 0x3F)) as u8);
                out.push((0x80 | (high & 0x3F)) as u8);
                // 低代理项 (3 字节)
                out.push((0xE0 | (low >> 12)) as u8);
                out.push((0x80 | ((low >> 6) & 0x3F)) as u8);
                out.push((0x80 | (low & 0x3F)) as u8);
            } else {
                out.push((0xE0 | (u >> 12)) as u8);
                out.push((0x80 | ((u >> 6) & 0x3F)) as u8);
                out.push((0x80 | (u & 0x3F)) as u8);
            }
        } else if u == 0 {
            out.push(0xC0);
            out.push(0x80);
        } else {
            out.push((0xC0 | (u >> 6)) as u8);
            out.push((0x80 | (u & 0x3F)) as u8);
        }
    }
    Ok(())
}

/// Java Modified UTF-8 解码：
/// 读取开头 2 字节大端长度，解码 Modified UTF-8 字节序列，返回 (解码文本, 消耗总字节数)。
pub fn decode_java_utf(buf: &[u8]) -> Result<(String, usize), CodecError> {
    if buf.len() < 2 {
        return Err(CodecError::UnexpectedEof {
            needed: 2,
            available: buf.len(),
        });
    }
    let utflen = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if buf.len() < 2 + utflen {
        return Err(CodecError::UnexpectedEof {
            needed: 2 + utflen,
            available: buf.len(),
        });
    }
    let bytes = &buf[2..2 + utflen];
    let mut s = String::with_capacity(utflen);
    let mut i = 0;
    while i < bytes.len() {
        let b1 = bytes[i];
        if b1 < 0x80 {
            if b1 == 0 {
                return Err(CodecError::MalformedUtf("null byte in modified utf-8"));
            }
            s.push(b1 as char);
            i += 1;
        } else if (b1 >> 5) == 0b110 {
            if i + 1 >= bytes.len() {
                return Err(CodecError::MalformedUtf("unexpected eof in 2-byte sequence"));
            }
            let b2 = bytes[i + 1];
            if (b2 >> 6) != 0b10 {
                return Err(CodecError::MalformedUtf("invalid continuation byte in 2-byte sequence"));
            }
            if b1 == 0xC0 && b2 == 0x80 {
                s.push('\0');
            } else {
                let code = (((b1 & 0x1F) as u32) << 6) | ((b2 & 0x3F) as u32);
                if let Some(ch) = char::from_u32(code) {
                    s.push(ch);
                } else {
                    return Err(CodecError::MalformedUtf("invalid codepoint in 2-byte sequence"));
                }
            }
            i += 2;
        } else if (b1 >> 4) == 0b1110 {
            if i + 2 >= bytes.len() {
                return Err(CodecError::MalformedUtf("unexpected eof in 3-byte sequence"));
            }
            let b2 = bytes[i + 1];
            let b3 = bytes[i + 2];
            if (b2 >> 6) != 0b10 || (b3 >> 6) != 0b10 {
                return Err(CodecError::MalformedUtf("invalid continuation byte in 3-byte sequence"));
            }
            let code = (((b1 & 0x0F) as u32) << 12) | (((b2 & 0x3F) as u32) << 6) | ((b3 & 0x3F) as u32);
            if (0xD800..=0xDBFF).contains(&code) {
                // 高代理项：后面必须跟低代理项 (3 字节)
                if i + 5 >= bytes.len() {
                    return Err(CodecError::MalformedUtf("missing low surrogate for high surrogate"));
                }
                let lb1 = bytes[i + 3];
                let lb2 = bytes[i + 4];
                let lb3 = bytes[i + 5];
                if (lb1 >> 4) != 0b1110 || (lb2 >> 6) != 0b10 || (lb3 >> 6) != 0b10 {
                    return Err(CodecError::MalformedUtf("invalid low surrogate continuation"));
                }
                let low = (((lb1 & 0x0F) as u32) << 12) | (((lb2 & 0x3F) as u32) << 6) | ((lb3 & 0x3F) as u32);
                if !(0xDC00..=0xDFFF).contains(&low) {
                    return Err(CodecError::MalformedUtf("invalid low surrogate range"));
                }
                let scalar = 0x10000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                if let Some(ch) = char::from_u32(scalar) {
                    s.push(ch);
                } else {
                    return Err(CodecError::MalformedUtf("invalid surrogate scalar"));
                }
                i += 6;
            } else if (0xDC00..=0xDFFF).contains(&code) {
                return Err(CodecError::MalformedUtf("orphan low surrogate"));
            } else if let Some(ch) = char::from_u32(code) {
                s.push(ch);
                i += 3;
            } else {
                return Err(CodecError::MalformedUtf("invalid codepoint in 3-byte sequence"));
            }
        } else {
            return Err(CodecError::MalformedUtf("invalid start byte"));
        }
    }
    Ok((s, 2 + utflen))
}

pub fn write_command(cmd: &HelperCommand, out: &mut Vec<u8>) -> Result<(), CodecError> {
    match cmd {
        HelperCommand::Open {
            url,
            width,
            height,
            scale,
        } => {
            out.push(CMD_OPEN);
            encode_java_utf(url, out)?;
            out.extend_from_slice(&width.to_be_bytes());
            out.extend_from_slice(&height.to_be_bytes());
            out.extend_from_slice(&scale.to_bits().to_be_bytes());
        }
        HelperCommand::Resize {
            width,
            height,
            scale,
        } => {
            out.push(CMD_RESIZE);
            out.extend_from_slice(&width.to_be_bytes());
            out.extend_from_slice(&height.to_be_bytes());
            out.extend_from_slice(&scale.to_bits().to_be_bytes());
        }
        HelperCommand::Mouse {
            id,
            x,
            y,
            modifiers,
            button,
            click_count,
            stamp,
        } => {
            out.push(CMD_MOUSE);
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
            out.extend_from_slice(&modifiers.to_be_bytes());
            out.extend_from_slice(&button.to_be_bytes());
            out.extend_from_slice(&click_count.to_be_bytes());
            out.extend_from_slice(&stamp.to_be_bytes());
        }
        HelperCommand::Wheel {
            x,
            y,
            modifiers,
            rotation,
            precise,
            stamp,
        } => {
            out.push(CMD_WHEEL);
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
            out.extend_from_slice(&modifiers.to_be_bytes());
            out.extend_from_slice(&rotation.to_be_bytes());
            out.extend_from_slice(&precise.to_bits().to_be_bytes());
            out.extend_from_slice(&stamp.to_be_bytes());
        }
        HelperCommand::Key {
            id,
            key_code,
            key_char,
            modifiers,
            location,
            stamp,
        } => {
            out.push(CMD_KEY);
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&key_code.to_be_bytes());
            out.extend_from_slice(&key_char.to_be_bytes());
            out.extend_from_slice(&modifiers.to_be_bytes());
            out.extend_from_slice(&location.to_be_bytes());
            out.extend_from_slice(&stamp.to_be_bytes());
        }
        HelperCommand::Focus { focused } => {
            out.push(CMD_FOCUS);
            out.push(if *focused { 1 } else { 0 });
        }
        HelperCommand::Close => {
            out.push(CMD_CLOSE);
        }
    }
    Ok(())
}

/// 读一条命令；如果输入为空返回 Ok(None) 表示正常 EOF。
pub fn read_command(buf: &[u8]) -> Result<Option<(HelperCommand, usize)>, CodecError> {
    if buf.is_empty() {
        return Ok(None);
    }
    let cmd_type = buf[0];
    let mut pos = 1;
    match cmd_type {
        CMD_OPEN => {
            let (url, utf_bytes) = decode_java_utf(&buf[pos..])?;
            pos += utf_bytes;
            if buf.len() < pos + 12 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 12,
                    available: buf.len(),
                });
            }
            let width = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let height = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let scale = f32::from_bits(u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()));
            pos += 4;
            Ok(Some((
                HelperCommand::Open {
                    url,
                    width,
                    height,
                    scale,
                },
                pos,
            )))
        }
        CMD_RESIZE => {
            if buf.len() < pos + 12 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 12,
                    available: buf.len(),
                });
            }
            let width = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let height = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let scale = f32::from_bits(u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()));
            pos += 4;
            Ok(Some((
                HelperCommand::Resize {
                    width,
                    height,
                    scale,
                },
                pos,
            )))
        }
        CMD_MOUSE => {
            if buf.len() < pos + 32 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 32,
                    available: buf.len(),
                });
            }
            let id = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let x = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let y = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let modifiers = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let button = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let click_count = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let stamp = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            Ok(Some((
                HelperCommand::Mouse {
                    id,
                    x,
                    y,
                    modifiers,
                    button,
                    click_count,
                    stamp,
                },
                pos,
            )))
        }
        CMD_WHEEL => {
            if buf.len() < pos + 32 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 32,
                    available: buf.len(),
                });
            }
            let x = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let y = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let modifiers = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let rotation = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let precise = f64::from_bits(u64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap()));
            pos += 8;
            let stamp = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            Ok(Some((
                HelperCommand::Wheel {
                    x,
                    y,
                    modifiers,
                    rotation,
                    precise,
                    stamp,
                },
                pos,
            )))
        }
        CMD_KEY => {
            if buf.len() < pos + 26 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 26,
                    available: buf.len(),
                });
            }
            let id = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let key_code = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let key_char = u16::from_be_bytes(buf[pos..pos + 2].try_into().unwrap());
            pos += 2;
            let modifiers = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let location = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let stamp = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            Ok(Some((
                HelperCommand::Key {
                    id,
                    key_code,
                    key_char,
                    modifiers,
                    location,
                    stamp,
                },
                pos,
            )))
        }
        CMD_FOCUS => {
            if buf.len() < pos + 1 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 1,
                    available: buf.len(),
                });
            }
            let focused = buf[pos] != 0;
            pos += 1;
            Ok(Some((HelperCommand::Focus { focused }, pos)))
        }
        CMD_CLOSE => Ok(Some((HelperCommand::Close, pos))),
        unknown => Err(CodecError::UnknownCommand(unknown)),
    }
}

pub fn write_hello(version: i32, pid: i64, out: &mut Vec<u8>) {
    out.push(EVT_HELLO);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&pid.to_be_bytes());
}

pub fn write_status(text: &str, out: &mut Vec<u8>) -> Result<(), CodecError> {
    out.push(EVT_STATUS);
    let truncated = truncate_utf16(text, MAX_TEXT_LEN);
    encode_java_utf(truncated, out)
}

pub fn write_cursor(cursor_type: i32, out: &mut Vec<u8>) {
    out.push(EVT_CURSOR);
    out.extend_from_slice(&cursor_type.to_be_bytes());
}

pub fn write_loaded(http_status: i32, out: &mut Vec<u8>) {
    out.push(EVT_LOADED);
    out.extend_from_slice(&http_status.to_be_bytes());
}

pub fn write_failed(message: &str, out: &mut Vec<u8>) -> Result<(), CodecError> {
    out.push(EVT_FAILED);
    let truncated = truncate_utf16(message, MAX_TEXT_LEN);
    encode_java_utf(truncated, out)
}

pub fn write_bye(out: &mut Vec<u8>) {
    out.push(EVT_BYE);
}

pub fn write_frame_header(
    width: i32,
    height: i32,
    rects: i32,
    paint_stamp: i64,
    input_stamp: i64,
    out: &mut Vec<u8>,
) {
    out.push(EVT_FRAME);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&paint_stamp.to_be_bytes());
    out.extend_from_slice(&input_stamp.to_be_bytes());
    out.extend_from_slice(&rects.to_be_bytes());
}

pub fn write_event_header(event: &HelperEvent, out: &mut Vec<u8>) -> Result<(), CodecError> {
    match event {
        HelperEvent::Hello { version, pid } => write_hello(*version, *pid, out),
        HelperEvent::Status { text } => write_status(text, out)?,
        HelperEvent::Frame {
            width,
            height,
            rects,
            paint_stamp,
            input_stamp,
        } => write_frame_header(*width, *height, *rects, *paint_stamp, *input_stamp, out),
        HelperEvent::Cursor { cursor_type } => write_cursor(*cursor_type, out),
        HelperEvent::Loaded { http_status } => write_loaded(*http_status, out),
        HelperEvent::Failed { message } => write_failed(message, out)?,
        HelperEvent::Bye => write_bye(out),
    }
    Ok(())
}

/// 把 [src]（整幅 [src_width] 宽的 BGRA）里的一块写出去
pub fn write_rect(
    src_bgra: &[u8],
    src_width: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    if w < 0
        || h < 0
        || w > MAX_SIDE
        || h > MAX_SIDE
        || src_width < 0
        || x < 0
        || y < 0
        || (x as i64) + (w as i64) > (src_width as i64)
    {
        return Err(CodecError::BadRect(w, h));
    }
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&y.to_be_bytes());
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    let bytes_per_row = (w as usize) * 4;
    let stride = (src_width as usize) * 4;
    for r in 0..h {
        let row_y = (y as usize).checked_add(r as usize).ok_or(CodecError::BadRect(w, h))?;
        let row_offset = row_y
            .checked_mul(stride)
            .and_then(|v| v.checked_add((x as usize) * 4))
            .ok_or(CodecError::BadRect(w, h))?;
        let end = row_offset
            .checked_add(bytes_per_row)
            .ok_or(CodecError::BadRect(w, h))?;
        if end > src_bgra.len() {
            return Err(CodecError::UnexpectedEof {
                needed: end,
                available: src_bgra.len(),
            });
        }
        out.extend_from_slice(&src_bgra[row_offset..end]);
    }
    Ok(())
}

/// 读一条事件的头部（Frame 的像素块由调用方根据 Frame.rects 逐块读）。
pub fn read_event_header(buf: &[u8]) -> Result<Option<(HelperEvent, usize)>, CodecError> {
    if buf.is_empty() {
        return Ok(None);
    }
    let evt_type = buf[0];
    let mut pos = 1;
    match evt_type {
        EVT_HELLO => {
            if buf.len() < pos + 12 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 12,
                    available: buf.len(),
                });
            }
            let version = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let pid = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            Ok(Some((HelperEvent::Hello { version, pid }, pos)))
        }
        EVT_STATUS => {
            let (text, utf_bytes) = decode_java_utf(&buf[pos..])?;
            pos += utf_bytes;
            Ok(Some((HelperEvent::Status { text }, pos)))
        }
        EVT_FRAME => {
            if buf.len() < pos + 28 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 28,
                    available: buf.len(),
                });
            }
            let width = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let height = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let paint_stamp = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let input_stamp = i64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let rects = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            if !(1..=MAX_SIDE).contains(&width)
                || !(1..=MAX_SIDE).contains(&height)
                || !(0..=MAX_RECTS).contains(&rects)
            {
                return Err(CodecError::BadFrameHeader(format!(
                    "坏帧头 {width}x{height} n={rects}"
                )));
            }
            Ok(Some((
                HelperEvent::Frame {
                    width,
                    height,
                    rects,
                    paint_stamp,
                    input_stamp,
                },
                pos,
            )))
        }
        EVT_CURSOR => {
            if buf.len() < pos + 4 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 4,
                    available: buf.len(),
                });
            }
            let cursor_type = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            Ok(Some((HelperEvent::Cursor { cursor_type }, pos)))
        }
        EVT_LOADED => {
            if buf.len() < pos + 4 {
                return Err(CodecError::UnexpectedEof {
                    needed: pos + 4,
                    available: buf.len(),
                });
            }
            let http_status = i32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
            pos += 4;
            Ok(Some((HelperEvent::Loaded { http_status }, pos)))
        }
        EVT_FAILED => {
            let (message, utf_bytes) = decode_java_utf(&buf[pos..])?;
            pos += utf_bytes;
            Ok(Some((HelperEvent::Failed { message }, pos)))
        }
        EVT_BYE => Ok(Some((HelperEvent::Bye, pos))),
        unknown => Err(CodecError::UnknownEvent(unknown)),
    }
}

/// 脏区排版与边界计算结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRectPlan {
    pub valid: bool,
    pub inside: bool,
    pub bytes_per_row: i32,
    pub rows: i32,
    pub row_stride_bytes: i32,
    pub initial_offset_bytes: i32,
    pub total_bytes: i64,
}

pub fn plan_rect(frame_w: i32, frame_h: i32, x: i32, y: i32, w: i32, h: i32) -> FrameRectPlan {
    if w < 0 || h < 0 || w > MAX_SIDE || h > MAX_SIDE {
        return FrameRectPlan {
            valid: false,
            inside: false,
            bytes_per_row: 0,
            rows: 0,
            row_stride_bytes: 0,
            initial_offset_bytes: 0,
            total_bytes: 0,
        };
    }
    let inside_raw = frame_w > 0
        && frame_h > 0
        && x >= 0
        && y >= 0
        && (x as i64) + (w as i64) <= (frame_w as i64)
        && (y as i64) + (h as i64) <= (frame_h as i64);
    let bytes_per_row = w * 4;
    let rows = h;
    let stride_i64 = (frame_w.max(0) as i64) * 4;
    let init_offset_i64 = if inside_raw {
        ((y as i64) * (frame_w as i64) + (x as i64)) * 4
    } else {
        0
    };
    let (inside, row_stride_bytes, initial_offset_bytes) = match (
        i32::try_from(stride_i64),
        i32::try_from(init_offset_i64),
    ) {
        (Ok(stride), Ok(off)) => (inside_raw, stride, off),
        _ => (false, 0, 0),
    };
    let total_bytes = (w as i64) * 4 * (h as i64);
    FrameRectPlan {
        valid: true,
        inside,
        bytes_per_row,
        rows,
        row_stride_bytes,
        initial_offset_bytes,
        total_bytes,
    }
}

/// 纯 Rust 帧像素组装器
#[derive(Debug, Clone, Default)]
pub struct FrameAssembler {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self, w: i32, h: i32) {
        if w == self.width && h == self.height {
            return;
        }
        self.width = w;
        self.height = h;
        let total = (w as usize) * (h as usize) * 4;
        self.pixels.clear();
        self.pixels.resize(total, 0);
    }

    pub fn read_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        rect_pixels: &[u8],
    ) -> Result<(), CodecError> {
        let plan = plan_rect(self.width, self.height, x, y, w, h);
        if !plan.valid {
            return Err(CodecError::BadRect(w, h));
        }
        if rect_pixels.len() < plan.total_bytes as usize {
            return Err(CodecError::UnexpectedEof {
                needed: plan.total_bytes as usize,
                available: rect_pixels.len(),
            });
        }
        if plan.inside {
            let row_bytes = plan.bytes_per_row as usize;
            for r in 0..plan.rows {
                let dst_offset = (((y + r) * self.width + x) * 4) as usize;
                let src_offset = (r as usize) * row_bytes;
                self.pixels[dst_offset..dst_offset + row_bytes]
                    .copy_from_slice(&rect_pixels[src_offset..src_offset + row_bytes]);
            }
        }
        Ok(())
    }
}

