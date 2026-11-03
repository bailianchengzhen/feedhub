//! 抖音的**文字 → Span**（正文 / 评论里的文字表情）。
//!
//! 逐行照抄 Kotlin `core/Emotes.kt` 的 `split(text, emotes)`：
//!
//! - 空文本 → 空列表；表里没有可用代码 → 一个 `Span::Text`；
//! - 代码按 **UTF-16 长度从长到短**稳定排序（Kotlin 的 `sortedByDescending { it.length }`，
//!   `String.length` 是 UTF-16 码元数），同一位置上多个代码都对得上时取最长的（Java 正则的
//!   选择分支是"最左位置 + 分支顺序优先"，不是 POSIX 的最长匹配，排序保证了两者一致）；
//! - 普通文本按 UTF-16 下标切；`String` 里可能出现落单代理项，Rust 的 `&str` 表示不了，
//!   这里按**字符边界**切（真实样本里没有落单代理项）。
//!
//! 只有抖音用得上（表情表是抖音自己的），所以住在 `douyin/`。以后别家也要"文字代码 → 表情"时，
//! 把这一份提到 `crate::detail`，别各抄一份。

use crate::detail::Span;

/// `Emotes.split` 的移植。
pub fn split_emotes(text: &str, emotes: &[(String, String)]) -> Vec<Span> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut codes: Vec<(&str, &str)> = emotes
        .iter()
        .filter(|(code, _)| !code.is_empty())
        .map(|(code, url)| (code.as_str(), url.as_str()))
        .collect();
    if codes.is_empty() {
        return vec![text_span(text)];
    }
    // 稳定排序（Rust 的 sort_by_key 是稳定的）：长度相同就保持表里的先后
    codes.sort_by_key(|(code, _)| std::cmp::Reverse(code.encode_utf16().count()));

    let mut out: Vec<Span> = Vec::new();
    let mut last = 0usize; // 上一次匹配结束的字节下标
    let mut i = 0usize;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        match codes.iter().find(|(code, _)| rest.starts_with(*code)) {
            Some((code, url)) => {
                if i > last {
                    out.push(text_span(&text[last..i]));
                }
                out.push(Span::Emote {
                    text: (*code).to_string(),
                    url: (*url).to_string(),
                });
                i += code.len();
                last = i;
            }
            None => {
                i += rest.chars().next().map(char::len_utf8).unwrap_or(1);
            }
        }
    }
    if out.is_empty() {
        return vec![text_span(text)];
    }
    if last < text.len() {
        out.push(text_span(&text[last..]));
    }
    out
}

fn text_span(text: &str) -> Span {
    Span::Text {
        text: text.to_string(),
        bold: false,
        italic: false,
    }
}

