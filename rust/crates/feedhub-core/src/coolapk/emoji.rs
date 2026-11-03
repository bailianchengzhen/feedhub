//! 酷安表情（`CoolapkEmoji`）：文字代码 → 随插件带的图片地址。**表由 Kotlin 传进来**
//!
//! 传进来的键是**正文里的原文**：方括号 `[受虐滑稽]`、圆括号要带 `#`（`#(笑眼)`）——也就是
//! Kotlin 插件描述里 `CoolapkPlugin.emotes.codes` 的那张表（`CoolapkEmoji.url(code)` 的结果）。
//! Kotlin 内部那张表存的是不带 `#` 的 `(笑眼)`，`CoolapkEmoji.url` 会自己剥掉，这里不重复那一步。
//!
//! 认得的代码切成 [`Span::Emote`]，其余原样留在文字里；`[链接]` / `[1]` 这种表里没有的方括号文字不是表情。

use std::collections::HashMap;

use super::html::Span;

/// "文字代码 → 图片地址"的表（借用调用方给的那一份，不复制字符串）。
pub struct EmoteTable<'a> {
    map: HashMap<&'a str, &'a str>,
}

impl<'a> EmoteTable<'a> {
    pub fn new(pairs: &'a [(String, String)]) -> Self {
        EmoteTable {
            map: pairs
                .iter()
                .map(|(code, url)| (code.as_str(), url.as_str()))
                .collect(),
        }
    }

    pub fn url(&self, code: &str) -> Option<&'a str> {
        self.map.get(code).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 代码在文本里的形状（`CoolapkEmoji.CODE`）：`\[[^\[\]\s]+\]` 或 `#\([^()\s]+\)`——
/// 里面不许有空白和同种括号，至少要有一个字符。`from` 是括号**里面**第一个字符的下标。
fn token_end(chars: &[(usize, char)], from: usize, open: char, close: char) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        let c = chars[i].1;
        if c == close {
            return if i > from { Some(i) } else { None };
        }
        if c == open || c.is_whitespace() {
            return None;
        }
        i += 1;
    }
    None
}

/// 一段纯文字（已经解过实体）切成文字和表情。没有表情就是一个 [`Span::Text`]；空串是空列表。
///
/// 和 Kotlin 的 `CoolapkEmoji.spans` 一一对应：从左到右找"认得的表情代码"，认得就切成
/// [`Span::Emote`]（`text` 是原文里的代码，**带 `#`**），不认得的代码当文字。
pub fn spans(text: &str, table: &EmoteTable) -> Vec<Span> {
    if text.is_empty() {
        return Vec::new();
    }
    if table.is_empty() {
        return vec![Span::Text(text.to_string())];
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut last_byte = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let (byte, c) = chars[i];
        let (end_char, token_start) = if c == '[' {
            match token_end(&chars, i + 1, '[', ']') {
                Some(end) => (end, byte),
                None => {
                    i += 1;
                    continue;
                }
            }
        } else if c == '#' && chars.get(i + 1).map(|(_, c)| *c) == Some('(') {
            // `#(` 两个字符都算代码的一部分，里面从 i+2 开始扫
            match token_end(&chars, i + 2, '(', ')') {
                Some(end) => (end, byte),
                None => {
                    i += 1;
                    continue;
                }
            }
        } else {
            i += 1;
            continue;
        };
        let end_byte = chars[end_char].0 + chars[end_char].1.len_utf8();
        let token = &text[token_start..end_byte];
        match table.url(token) {
            Some(url) => {
                if token_start > last_byte {
                    out.push(Span::Text(text[last_byte..token_start].to_string()));
                }
                out.push(Span::Emote {
                    text: token.to_string(),
                    url: url.to_string(),
                });
                last_byte = end_byte;
            }
            None => {}
        }
        i = end_char + 1;
    }
    if out.is_empty() {
        return vec![Span::Text(text.to_string())];
    }
    if last_byte < text.len() {
        out.push(Span::Text(text[last_byte..].to_string()));
    }
    out
}

