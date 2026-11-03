//! 贴吧**个人表情包**（收藏 / DIY）的正文文本格式：`#(meme,{idPart},{w},{h},{md5})`。
//!
//! 逐条对照 `source-tieba/TiebaMeme.kt`：
//!
//! ```text
//! 收藏 / DIY 的本地标识：#(meme,{collect_|diy_}{pkgId}_{picId},{w},{h},{md5})
//! 发帖时服务端只存不带前缀的：#(meme,{pkgId}_{picId},{w},{h},{md5})
//! md5 = MD5("meme,{pkgId}_{picId},{w},{h},7S6wbXjEKL9N").lowercase()
//! ```
//!
//! 形状对得上就认（md5 只用 [`MemeRef::md5_ok`] 标一下对不对，对不上也照样画 `[表情包]`）。
//!
//! **为什么手写扫描而不是正则**：当前一个 crate 都不新增（`cargo build --offline`），机器上也本来没有
//! `regex`；Kotlin 那两个正则的口径（`#\(meme,[^)\s]{1,200}\)` 和整串匹配的形状）在这里逐条手写，
//! 本文件末尾那几条单测（`scan_*` / `parses_*` / `rejects_*`）守着。
//!
//! **下标是字节偏移**（Kotlin 是 UTF-16 码元）：记号本身全是 ASCII，边界一定落在字符边界上，
//! 所以按字节切出来的前后文字和 Kotlin 的 `substring` 完全一样。

use crate::hosted::md5_hex;

/// md5 的盐（FluxTie `MemeRepository.SIGN_SALT`，格式事实，不是我们的密钥）。
pub const SALT: &str = "7S6wbXjEKL9N";

/// 认出来以后画成图片（或名字）的文字：不是给人读的机器码，显示和复制都用这个名字。
pub const LABEL: &str = "[表情包]";

/// 记号的开头（`#(meme,`）。
const MARKER: &str = "#(meme,";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Collect,
    Diy,
}

/// 一个认下来的表情包记号。[`text`](MemeRef::text) 是原文，`pck_id` / `pic_id` 是表情包和图片的 id，
/// [`md5_ok`](MemeRef::md5_ok) = md5 和 FluxTie 记的算法对得上（对不上也认，只是标一下）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemeRef {
    pub text: String,
    pub kind: Option<Kind>,
    pub pck_id: i64,
    pub pic_id: i64,
    pub width: i32,
    pub height: i32,
    pub md5_ok: bool,
}

/// 在文本里扫出来的一处：`start` 到 `end`（不含，都是**字节**偏移）是那个记号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub raw: String,
    pub start: usize,
    pub end: usize,
    pub reference: MemeRef,
}

/// `#(meme,…)` 记号的 md5：`MD5("meme,{pkgId}_{picId},{w},{h},{salt}")`，小写十六进制。
pub fn md5(pck_id: i64, pic_id: i64, width: i32, height: i32) -> String {
    md5_hex(&format!("meme,{pck_id}_{pic_id},{width},{height},{SALT}"))
}

/// 扫 [text] 里的 `#(meme,…)`。形状都对得上才算命中（认不出、md5 不对的都原样留着）。
///
/// 口径照 Kotlin `TOKEN = Regex("#\\(meme,[^)\\s]{1,200}\\)")` + `findAll`：
/// 记号内部是 1..=200 个"不是 `)`、不是 Java `\s`（空格 / `\t\n\x0B\f\r`）"的字符，后面紧跟 `)`；
/// 命中一次之后从它后面接着扫，没命中就从 `#(meme,` 后面接着扫。
pub fn scan(text: &str) -> Vec<Hit> {
    if !text.contains(MARKER) {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut out: Vec<Hit> = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(MARKER) {
        let start = from + rel;
        let mut end = start + MARKER.len();
        let mut count = 0usize;
        while end < bytes.len()
            && count < 200
            && bytes[end] != b')'
            && !is_java_space(bytes[end])
        {
            end += 1;
            count += 1;
        }
        let matched = count >= 1 && end < bytes.len() && bytes[end] == b')';
        if matched {
            let token = &text[start..=end];
            if let Some(reference) = parse(token) {
                out.push(Hit {
                    raw: token.to_string(),
                    start,
                    end: end + 1,
                    reference,
                });
            }
            from = end + 1;
        } else {
            from = start + MARKER.len();
        }
    }
    out
}

/// 单个记号 → [`MemeRef`]；形状不对就是 `None`（Kotlin 的 `SHAPE.matchEntire`）。
///
/// `#\(meme,(?:(collect|diy)_)?(\d{1,20})_(\d{1,20}),(\d{1,5}),(\d{1,5}),([0-9a-fA-F]{32})\)`
/// —— 每段数字都是"1..=上限个数字、紧接着必须是分隔符"（正则贪心 + 不能回溯到非数字边界）。
pub fn parse(token: &str) -> Option<MemeRef> {
    let inner = token.strip_prefix(MARKER)?.strip_suffix(')')?;
    let (kind, rest) = if let Some(rest) = inner.strip_prefix("collect_") {
        (Some(Kind::Collect), rest)
    } else if let Some(rest) = inner.strip_prefix("diy_") {
        (Some(Kind::Diy), rest)
    } else {
        (None, inner)
    };
    let (pck, rest) = digits(rest, 1, 20)?;
    let rest = rest.strip_prefix('_')?;
    let (pic, rest) = digits(rest, 1, 20)?;
    let rest = rest.strip_prefix(',')?;
    let (width, rest) = digits(rest, 1, 5)?;
    let rest = rest.strip_prefix(',')?;
    let (height, rest) = digits(rest, 1, 5)?;
    let rest = rest.strip_prefix(',')?;
    if rest.len() != 32 || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pck_id = pck.parse::<i64>().ok()?;
    let pic_id = pic.parse::<i64>().ok()?;
    let width = width.parse::<i32>().ok()?;
    let height = height.parse::<i32>().ok()?;
    // Kotlin: `if (w !in 1..9999 || h !in 1..9999) return null`
    if !(1..=9999).contains(&width) || !(1..=9999).contains(&height) {
        return None;
    }
    let md5_ok = rest.eq_ignore_ascii_case(&md5(pck_id, pic_id, width, height));
    Some(MemeRef {
        text: token.to_string(),
        kind,
        pck_id,
        pic_id,
        width,
        height,
        md5_ok,
    })
}

/// 取 1..=max 个 ASCII 数字；多于 max 个就整段失败（正则没法回溯到非数字边界）。
fn digits(s: &str, min: usize, max: usize) -> Option<(&str, &str)> {
    let n = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if n < min || n > max {
        return None;
    }
    Some((&s[..n], &s[n..]))
}

/// Java `\s`（也就是 Kotlin 正则里的 `\s`，**没有**开 UNICODE_CHARACTER_CLASS）：` \t\n\x0B\f\r`。
fn is_java_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

