//! **YouTube 评论里的表情短码**（照 `source-youtube/YoutubeEmotes.kt` 逐行移植）。
//!
//! 为什么要有这一层：TV `next` 的评论 `contentText.runs[]` 一般只有纯文字（unicode 表情原样留在里面），
//! 但 InnerTube 的形状本来可以带图：
//! `{emoji:{emojiId, shortcuts:[":face-blue-smiling:"], image:{thumbnails:[…]}, isCustomEmoji:true}}`。
//! 所以两条路都接（和 Kotlin 一样）：
//!
//! 1. **run 里有图**（`emoji.image.thumbnails[]`）→ 用响应给的地址（最大那一档）→ [`Span::Emote`]；
//! 2. **只有短码** → 查两张随插件带的表：`yt_emojis/youtube.tsv`（YouTube 自家的图片表情，短码 → `yt3.ggpht.com`
//!    的 PNG）和 `yt_emojis/unicode.tsv`（标准 unicode 表情的短码 → 字符）。
//!
//! 两张表就是 `source-youtube/src/main/resources/yt_emojis/` 下那两份（`tools/yt-emojis/gen.py` 生成），
//! **这里用 `include_str!` 直接读 Kotlin 模块里那份，不另抄一份**（和 `tiktok/source.rs` 读 catbus 对拍数据同一个做法）。
//! 表变了这边自动跟着变；反过来改这里不用动 Kotlin。
//!
//! 两张表都查不到的短码**原样保留**（`10:30:45`、`:not_a_thing:` 这种可能真是文字）。
//! ASCII 颜文字（`:D`、`:P`、`:)`）连正则都匹配不上，生成表时也没收。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::Value;

use crate::detail::Span;
use crate::json;

/// YouTube 自家的图片表情（短码 → 图片地址，约 100 条）。
const YOUTUBE_TSV: &str =
    include_str!("../../../../../source-youtube/src/main/resources/yt_emojis/youtube.tsv");

/// 标准 unicode 表情的短码（短码 → 字符，约 1980 条）。
const UNICODE_TSV: &str =
    include_str!("../../../../../source-youtube/src/main/resources/yt_emojis/unicode.tsv");

/// YouTube 自家图片表情表（懒加载一次，之后只读）。
fn images() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| load(YOUTUBE_TSV))
}

/// 标准 unicode 表情表（懒加载一次，之后只读）。
fn unicode() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| load(UNICODE_TSV))
}

/// 读 TSV（第一列 = 短码，第二列 = 图片地址 / 字符；空行和 `#` 开头的注释跳过）。
///
/// 照 Kotlin `YoutubeEmotes.load`：`bufferedReader().useLines` 那一套（空行跳过、`#` 注释跳过、
/// 没有 tab 的跳过、第一列空的跳过）。重复的键**后出现的赢**（Kotlin 是 `LinkedHashMap`，同 key 覆盖）。
fn load(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(tab) = line.find('\t') else { continue };
        if tab == 0 {
            continue;
        }
        out.insert(line[..tab].to_string(), line[tab + 1..].to_string());
    }
    out
}

/// 短码：`:[A-Za-z0-9_+-]{2,40}:`（照 Kotlin 的 `Regex(":[A-Za-z0-9_+-]{2,40}:")`）。
///
/// 没有 regex crate，这里手写扫描器，语义按 Java 正则来：从左边起找第一个匹配；
/// 冒号后面那段"名字"**只能由那 5 类字符组成**，所以名字天然停在下个冒号处——
/// 名字长度落在 2..=40 且后面紧跟冒号才算，否则从这个冒号往后接着找。
fn is_code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')
}

/// 一段**纯文字**里的短码 → [`Span`]（Kotlin `YoutubeEmotes.spans`）。
///
/// 没有任何短码时返回单个 [`Span::Text`]（**不做扫描**，省一屏评论的开销，和 Kotlin 一样先看有没有 `:`）。
/// 认出来的图片表情变成 [`Span::Emote`]（`text` 是短码原文），unicode 表情变成它那个字符。
pub fn spans(text: &str) -> Vec<Span> {
    if text.is_empty() || !text.contains(':') {
        return vec![text_span(text)];
    }
    let mut out: Vec<Span> = Vec::new();
    let bytes = text.as_bytes();
    let mut last = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        // 名字只能由 ASCII 类字符组成，所以按字节走就是按字符走
        let mut end = i + 1;
        while end < bytes.len() && is_code_char(bytes[end] as char) {
            end += 1;
        }
        let len = end - (i + 1);
        if !(2..=40).contains(&len) || bytes.get(end) != Some(&b':') {
            i += 1;
            continue;
        }
        let code = &text[i..end + 1];
        let lower = code.to_ascii_lowercase();
        let image = images().get(code).or_else(|| images().get(&lower));
        let char = if image.is_some() {
            None
        } else {
            unicode().get(code).or_else(|| unicode().get(&lower))
        };
        if image.is_none() && char.is_none() {
            // 认不得：当普通文字，连着后面一起输出（Kotlin `continue`）
            i = end + 1;
            continue;
        }
        if i > last {
            out.push(text_span(&text[last..i]));
        }
        match (image, char) {
            (Some(url), _) => out.push(emote_span(code, url)),
            (None, Some(c)) => out.push(text_span(c)),
            (None, None) => unreachable!("上面已经排除了"),
        }
        last = end + 1;
        i = end + 1;
    }
    if out.is_empty() {
        return vec![text_span(text)];
    }
    if last < text.len() {
        out.push(text_span(&text[last..]));
    }
    out
}

/// 一个带 `emoji` 对象的 run → [`Span`]（Kotlin `YoutubeEmotes.span`，不是表情 run 返回 `None`）。
///
/// 顺序照 YouTube.js 的 `EmojiRun.text`：`shortcuts[0]` → `emojiId` → run 自己的 `text`。
/// 有 `image.thumbnails` 就用**最大的一档**（实录是 24 / 48 两档），没有就走短码表。
pub fn span(run: &Value) -> Option<Span> {
    let emoji = run.get("emoji")?.as_object()?;
    let code = json::arr(emoji, "shortcuts")
        .and_then(|list| {
            list.iter()
                .filter_map(json::primitive_content)
                .find(|s| !s.is_empty())
        })
        .or_else(|| json::text(emoji, "emojiId"))
        .or_else(|| json::text(run.as_object()?, "text"))?;
    let url = json::obj(emoji, "image")
        .and_then(|image| json::arr(image, "thumbnails"))
        .and_then(|thumbs| {
            thumbs
                .iter()
                .filter_map(|t| t.as_object())
                .filter_map(|t| json::text(t, "url"))
                .last()
        });
    if let Some(url) = url {
        return Some(emote_span(&code, &json::to_https(&url)));
    }
    // 响应里没给图：能查表就查表，查不到留短码原文
    let resolved = spans(&code);
    Some(match resolved.as_slice() {
        [only] => only.clone(),
        _ => text_span(&code),
    })
}

fn text_span(text: &str) -> Span {
    Span::Text {
        text: text.to_string(),
        bold: false,
        italic: false,
    }
}

fn emote_span(text: &str, url: &str) -> Span {
    Span::Emote {
        text: text.to_string(),
        url: url.to_string(),
    }
}

