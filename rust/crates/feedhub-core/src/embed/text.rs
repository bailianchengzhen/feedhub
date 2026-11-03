//! 送进模型的**那段文字** + 向量数学（MRL 截断 / L2 归一化 / float16 存字节）。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/Embedder.kt` + `HalfFloat.kt` 的 Rust 版。口径逐条照抄 Kotlin，**逐位对齐**是硬要求（同一批卡片两边算出来的 f16 字节要一样）：
//!
//! | 规则 | 落点 |
//! |---|---|
//! | 空白折叠只认 6 个字符（空格 / `\n` / `\r` / `\t` / `\u000B` / `\u000C`） | [`collapse_whitespace`] |
//! | 收尾的 `trim()` 用 **Kotlin 的空白口径**（Java `isWhitespace` ∪ `isSpaceChar`，含 NBSP 三兄弟） | [`crate::hosted::is_kotlin_blank`] + [`kotlin_trim`] |
//! | `isBlank()` 同上（NBSP 也算空白） | [`crate::hosted::is_kotlin_blank`] |
//! | 摘要 / 正文的字符上限按 **UTF-16 单元**数（Kotlin `String.length`），切在代理对中间时补 U+FFFD | [`truncate_utf16`] |
//! | 字符串排序按 **UTF-16 码元**（Java `String.compareTo`），不是 UTF-8 字节序 | [`cmp_utf16`] |
//!
//! 为什么"切在代理对中间"要补 U+FFFD 而不是丢掉：Kotlin 的 `substring` 会切出半个代理对，那个字符串过
//! UniFFI 边界时 UTF-8 编码会抛异常，所以宿主在过边界**之前**会把它换成 U+FFFD（`RustGemmaTokenizer.sanitize`）；
//! 分词器看到的也是 U+FFFD。Rust 这边直接产出 U+FFFD，两边最终送进模型的是同一段文字。

use std::cmp::Ordering;

use crate::hosted::is_java_whitespace;
use crate::model::FeedItem;

use super::item::EmbedItem;

/// 存下来的维数。
pub const EMBEDDING_DIM: usize = 256;

/// 当前写进 `item_embedding.model` 的模型标识（CPU 的 q4 图）。
pub const EMBEDDING_MODEL_ID: &str = "embeddinggemma-2-q4-acc4";

/// 向量来源：首页 / 关注的新卡片。
pub const EMBEDDING_SOURCE_FEED: &str = "feed";
/// 向量来源：收藏里的条目补算。
pub const EMBEDDING_SOURCE_FAVORITE: &str = "favorite";
/// 向量来源：历史里的条目补算（详情正文覆盖也记这个）。
pub const EMBEDDING_SOURCE_HISTORY: &str = "history";

/// 卡片的摘要最多取多少个字（UTF-16 单元）。
pub const MAX_SUMMARY_CHARS: usize = 200;

/// 详情正文最多算到几个 token（`[8,128]` 那张图装得下的长度）。
pub const DETAIL_MAX_TOKENS: usize = 128;

/// 详情正文的字符上限（兜底：先按字符砍一刀，别让一篇几万字专栏在内存里来回分词）。
pub const DETAIL_MAX_CHARS: usize = 1200;

/// 卡片 → 送去算向量的那段文字（`title: … | text: …`）。
///
/// 口径见 `app/.../Embedder.kt` 的 `embeddingText` KDoc：没有标题时 `title: none`；
/// `text` 依次退到摘要 / 圈子 / 推荐理由，都没有就 `text: none`；摘要按 [`MAX_SUMMARY_CHARS`] 截。
pub fn embedding_text(item: &EmbedItem, max_summary_chars: usize) -> String {
    let title = item
        .title
        .as_deref()
        .map(collapse_whitespace)
        .filter(|t| !crate::hosted::is_kotlin_blank(t));
    let text = [&item.summary, &item.community, &item.reason]
        .into_iter()
        .filter_map(|v| v.as_deref())
        .map(collapse_whitespace)
        .find(|t| !crate::hosted::is_kotlin_blank(t))
        .map(|t| truncate_utf16(&t, max_summary_chars));
    format!(
        "title: {} | text: {}",
        title.as_deref().unwrap_or("none"),
        text.as_deref().unwrap_or("none"),
    )
}

/// 统一模型上的同一条（Rust 自己的测试 / 将来资料库直接喂进来时用）。
pub fn embedding_text_of(item: &FeedItem, max_summary_chars: usize) -> String {
    embedding_text(&EmbedItem::from_feed(item), max_summary_chars)
}

/// 一条卡片值不值得算向量：18+ 不算，标题和正文都没有的也不算（算出来也是噪声）。
pub fn has_embedding_text(item: &EmbedItem) -> bool {
    item.has_text()
}

/// 点开过的条目用它自己的正文重新拼一段文字（格式一样，`text` 换成正文开头）。
///
/// - 正文先折叠空白、按 [`DETAIL_MAX_CHARS`] 砍一刀（`trimEnd` 用 Kotlin 口径）；
/// - 正文是空的（纯图片帖 / 视频没简介）就退回 [`embedding_text`]；
/// - 按 token 截到 [`DETAIL_MAX_TOKENS`] 由调用方（宿主的分词器）做——这里不碰词表。
pub fn detail_embedding_text(item: &EmbedItem, body: Option<&str>, max_chars: usize) -> String {
    let collapsed = match body {
        None => None,
        Some(b) => Some(collapse_whitespace(b)),
    };
    let text = match collapsed {
        None => return embedding_text(item, MAX_SUMMARY_CHARS),
        Some(t) if crate::hosted::is_kotlin_blank(&t) => return embedding_text(item, MAX_SUMMARY_CHARS),
        Some(t) => t,
    };
    let title = item
        .title
        .as_deref()
        .map(collapse_whitespace)
        .filter(|t| !crate::hosted::is_kotlin_blank(t));
    let cut = if text.chars().map(char::len_utf16).sum::<usize>() > max_chars {
        trim_end_owned(truncate_utf16(&text, max_chars))
    } else {
        text
    };
    format!(
        "title: {} | text: {}",
        title.as_deref().unwrap_or("none"),
        cut,
    )
}

/// 空白折叠：连续空白压成一个空格，去掉首尾空白。
///
/// 和 Kotlin 一样只把 6 个字符当"折叠目标"（U+000B / U+000C 也在内，NBSP **不在**内——
/// NBSP 在 Kotlin 那边只是 `isWhitespace`，不会被折叠成空格）。收尾的 `trim()` 用 Kotlin 的空白口径。
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\n' | '\r' | '\t' | '\u{b}' | '\u{c}') {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    trim_end_owned(trim_start_owned(out))
}

/// Kotlin `String.trim()`（两端都去，口径是 `Char.isWhitespace` = Java `isWhitespace` ∪ `isSpaceChar`）。
pub fn kotlin_trim(s: &str) -> &str {
    trim_end(trim_start(s))
}

fn is_kotlin_ws(c: char) -> bool {
    is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
}

fn trim_start(s: &str) -> &str {
    match s.char_indices().find(|(_, c)| !is_kotlin_ws(*c)) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

fn trim_end(s: &str) -> &str {
    match s.char_indices().rev().find(|(_, c)| !is_kotlin_ws(*c)) {
        Some((i, c)) => &s[..i + c.len_utf8()],
        None => "",
    }
}

fn trim_start_owned(mut s: String) -> String {
    let cut = s.len() - trim_start(&s).len();
    if cut > 0 {
        s.drain(..cut);
    }
    s
}

fn trim_end_owned(mut s: String) -> String {
    let keep = trim_end(&s).len();
    s.truncate(keep);
    s
}

/// Kotlin `String.substring(0, max)` 的等价物：**按 UTF-16 单元**数，切在代理对中间时补 U+FFFD。
///
/// 不截断时原样返回（一次分配）；`max_units` 比整串还大时也是原样。
pub fn truncate_utf16(s: &str, max_units: usize) -> String {
    let mut units = 0usize;
    for (i, c) in s.char_indices() {
        let w = c.len_utf16();
        if units + w > max_units {
            // 切在代理对中间：Kotlin 会留下半个代理对，宿主过边界前换成 U+FFFD，所以这里也补一个。
            let mut out = String::with_capacity(i + 3);
            out.push_str(&s[..i]);
            if w == 2 && units < max_units {
                out.push('\u{fffd}');
            }
            return out;
        }
        units += w;
    }
    s.to_string()
}

/// Kotlin `String.compareTo`：按 **UTF-16 码元**逐位比。
pub fn cmp_utf16(a: &str, b: &str) -> Ordering {
    let mut ai = a.encode_utf16();
    let mut bi = b.encode_utf16();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

/// MRL（Matryoshka）：截前 `dim` 维（不够就补 0）再**重新 L2 归一化**。
pub fn mrl(vec: &[f32], dim: usize) -> Vec<f32> {
    assert!(dim > 0, "dim 必须 > 0（收到 {dim}）");
    let mut out = vec![0.0f32; dim];
    let n = vec.len().min(dim);
    out[..n].copy_from_slice(&vec[..n]);
    l2_normalize_in_place(&mut out);
    out
}

/// L2 归一化（原地）；全零向量原样返回（不产生 NaN）。
///
/// 和 Kotlin 一样用 **f64 累加**、`1/sqrt` 之后转 f32 再逐位乘——逐字节对拍要靠这个。
pub fn l2_normalize_in_place(vec: &mut [f32]) {
    let mut sum = 0.0f64;
    for v in vec.iter() {
        let v = *v as f64;
        sum += v * v;
    }
    if sum <= 0.0 {
        return;
    }
    let inv = (1.0f64 / sum.sqrt()) as f32;
    for v in vec.iter_mut() {
        *v *= inv;
    }
}

/// 两个**已归一化**向量的余弦（= 点积）；长度不一致时取公共前缀。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut dot = 0.0f64;
    for i in 0..n {
        dot += a[i] as f64 * b[i] as f64;
    }
    dot as f32
}

/// float32 → float16 的 16 位位型（就近舍入到偶数，和 Kotlin `HalfFloat.fromFloatBits` 逐位一致）。
pub fn f16_bits_from_f32_bits(bits: u32) -> u16 {
    let sign = (bits >> 16) & 0x8000;
    let exponent = (bits >> 23) & 0xFF;
    let mantissa = bits & 0x7F_FFFF;
    if exponent == 0xFF {
        // Inf / NaN：保留符号；NaN 保留 mantissa 的高位（非零即可）
        let tail = if mantissa == 0 { 0 } else { (mantissa >> 13).max(1) };
        return (sign | 0x7C00 | tail) as u16;
    }
    let exp = exponent as i32 - 127 + 15;
    if exp >= 0x1F {
        return (sign | 0x7C00) as u16;
    }
    if exp <= 0 {
        if exp < -10 {
            return sign as u16;
        }
        let m = mantissa | 0x80_0000;
        let shift = (14 - exp) as u32;
        let mut half = m >> shift;
        let round_bit = (m >> (shift - 1)) & 1;
        let sticky = (m & ((1u32 << (shift - 1)) - 1)) != 0;
        if round_bit == 1 && (sticky || (half & 1) == 1) {
            half += 1;
        }
        return (sign | half) as u16;
    }
    let mut half = ((exp as u32) << 10) | (mantissa >> 13);
    let round_bit = (mantissa >> 12) & 1;
    let sticky = (mantissa & 0xFFF) != 0;
    if round_bit == 1 && (sticky || (half & 1) == 1) {
        half += 1;
        if half & 0x7C00 == 0x7C00 {
            return (sign | 0x7C00) as u16;
        }
    }
    (sign | half) as u16
}

/// float16 的 16 位位型 → float32（和 Kotlin `HalfFloat.toFloatBits` 逐位一致）。
pub fn f32_bits_from_f16_bits(half: u16) -> u32 {
    let sign = ((half as u64) & 0x8000) << 16;
    let exp = (half >> 10) & 0x1F;
    let mantissa = (half & 0x3FF) as u64;
    let bits = match exp {
        0 => {
            if mantissa == 0 {
                sign
            } else {
                // subnormal：规格化
                let mut m = mantissa;
                let mut e = 0u64;
                while m & 0x400 == 0 {
                    m <<= 1;
                    e += 1;
                }
                let norm_mantissa = (m & 0x3FF) << 13;
                let norm_exp = 127 - 15 - e as i64;
                sign | ((norm_exp as u64) << 23) | norm_mantissa
            }
        }
        0x1F => sign | (0xFFu64 << 23) | (mantissa << 13),
        _ => sign | (((exp as u64) + 127 - 15) << 23) | (mantissa << 13),
    };
    bits as u32
}

/// 一整个向量 → f16 字节（小端，2 字节一个）。
pub fn float_vec_to_half_bytes(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 2);
    for v in vec {
        let bits = f16_bits_from_f32_bits(v.to_bits());
        out.push((bits & 0xFF) as u8);
        out.push((bits >> 8) as u8);
    }
    out
}

/// f16 字节里的第 `i` 个分量（小端）→ float。
pub fn half_byte_to_float(bytes: &[u8], i: usize) -> f32 {
    let at = i * 2;
    let bits = (bytes[at] as u16) | ((bytes[at + 1] as u16) << 8);
    f32::from_bits(f32_bits_from_f16_bits(bits))
}

/// 一整条 f16 向量 → f32（长度不对返回 `None`，和 Kotlin `ItemEmbedding.toFloats` 一个口径）。
pub fn vec_from_half_bytes(bytes: &[u8], dim: usize) -> Option<Vec<f32>> {
    if dim == 0 || bytes.len() != dim * 2 {
        return None;
    }
    Some((0..dim).map(|i| half_byte_to_float(bytes, i)).collect())
}
