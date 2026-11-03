//! 小红书**笔记详情 / 一级评论 / 楼中楼**的解析。
//!
//! 逐条对照 Kotlin：
//!
//! | 这里 | Kotlin |
//! |---|---|
//! | [`parse_detail`] | `XhsSource.detail` 的解析那一半：`XhsNote.noteCard` / `texts` / `detail` / `image` / `videoInfo` + `XhsVideo.size` |
//! | [`parse_comments`] / [`parse_replies`] | `XhsSource.comments` / `replies` 的解析那一半：`XhsComments.page` / `comment` / `user` / `picture` |
//! | [`blocks`] / [`spans`] / [`plain`] | `XhsBody.blocks` / `spans` / `plain` / `topicUrl` |
//! | [`has_emote_token`] | `XhsBody.EMOTE_TOKEN.containsMatchIn`（Rust 这边自己扫，不引入 regex crate） |
//!
//! **富文本的口径**（这一家最容易踩的地方）：
//! - 正文是**纯文本 + 三种记号**：话题 `#日漫[话题]#`、@人 `@昵称`、表情 `[笑哭R]`；
//! - Kotlin 用一个正则 `#topic|@mention|[emote]` 的**交替**（左起、按顺序试、不重叠）切 Span。Rust 里没有
//!   regex crate（不新增依赖），所以按同样的语义手写扫描：**在每个位置先试话题、再试 @、最后试表情**，
//!   `#` 后面是**惰性**量词（最早的那个 `[话题]#` 才收），表情是 `{1,12}` **贪婪**（最长能凑到 `]` 的那个）；
//! - `@` 的分支是"名字按 UTF-16 长度**倒序**稳定排序后依次试"（Kotlin `sortedByDescending { it.name.length }`），
//!   `distinctBy { name }` 先出现的那份留下；
//! - 没认出表情的记号**留在文字里**（`buf.append(emote)`），所以相邻的文字最后要合并成一段
//!   （Kotlin 的 `fold`：只有两边都是不带粗体的 `Span.Text` 才合并）。
//!
//! **拿不到就是 `None`**：标题是空串时 `title = None`（`JsonObject.text` 的口径），摘要空就是 `None`。

use crate::detail::{Block, Comment, CommentPage, FeedDetail, RichText, Span, VideoInfo, VideoPart};
use crate::hosted;
use crate::json::{self, JsonMap};
use crate::model::{BiliError, DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision, UserRef};
use crate::xhs::{self, XHS};

// ===========================================================================
// 富文本（`XhsBody`）
// ===========================================================================

/// 话题页的网页地址（`XhsBody.topicUrl`）。
pub fn topic_url(id: &str) -> String {
    format!("{}/page/topics/{id}", xhs::WEB)
}

/// 表情记号里**不能**出现的字符：`[`、`]`、`\s`。
///
/// Java 正则的 `\s` 只是 ASCII 那六个（`[ \t\n\x0B\f\r]`），**不含** NBSP / 全角空格——
/// 这里照抄，别换成 Rust 的 `char::is_whitespace`（那是另一套集合）。
fn is_emote_break(c: char) -> bool {
    matches!(c, '[' | ']' | ' ' | '\t' | '\n' | '\u{0b}' | '\u{c}' | '\r')
}

/// 话题名里**不能**出现的字符：`#`、`[`、`]`、`\n`（`[^#\[\]\n]`）。注意 `\r` 是**允许**的。
fn is_topic_break(c: char) -> bool {
    matches!(c, '#' | '[' | ']' | '\n')
}

/// 从 `start` 开始试 `#([^#\[\]\n]+?)\[话题]#`（惰性量词）：`Some((结束字节下标, 话题名))`。
fn match_topic(line: &str, start: usize) -> Option<(usize, String)> {
    debug_assert!(line[start..].starts_with('#'));
    let mut j = start + 1;
    // `+?` 至少要一个字符，所以先"必须消费一个"再开始试终止符。
    loop {
        if j > start + 1 && line[j..].starts_with("[话题]#") {
            return Some((j + "[话题]#".len(), line[start + 1..j].to_string()));
        }
        if j >= line.len() {
            return None;
        }
        let c = line[j..].chars().next()?;
        if is_topic_break(c) {
            return None;
        }
        j += c.len_utf8();
    }
}

/// 从 `start` 开始试 `@(?:name1|name2|…)`：`Some((结束字节下标, 命中的名字下标))`。
///
/// 交替是**按顺序**试的（Kotlin 那边名字已经按长度倒序排过），先对上的那个算。
fn match_mention(line: &str, start: usize, named: &[&str]) -> Option<(usize, usize)> {
    debug_assert!(line[start..].starts_with('@'));
    let rest = &line[start + 1..];
    for (index, name) in named.iter().enumerate() {
        if rest.starts_with(name) {
            return Some((start + 1 + name.len(), index));
        }
    }
    None
}

/// 从 `start` 开始试 `\[[^\[\]\s]{1,12}]`（`{1,12}` **贪婪**）：`Some(结束字节下标)`。
fn match_emote(line: &str, start: usize) -> Option<usize> {
    debug_assert!(line[start..].starts_with('['));
    // 先尽量吃到 12 个字符，再一个一个往回退，第一个后面是 `]` 的位置就是答案。
    let mut ends: Vec<usize> = Vec::with_capacity(13);
    let mut j = start + 1;
    while ends.len() < 12 {
        if j >= line.len() {
            break;
        }
        let c = line[j..].chars().next()?;
        if is_emote_break(c) {
            break;
        }
        j += c.len_utf8();
        ends.push(j);
    }
    ends.into_iter().rev().find(|end| line[*end..].starts_with(']')).map(|end| end + 1)
}

/// 这段文字里有没有表情记号（`XhsBody.EMOTE_TOKEN.containsMatchIn`）。Kotlin 用它决定要不要去取表情表。
pub fn has_emote_token(text: &str) -> bool {
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap_or('\u{fffd}');
        if c == '[' && match_emote(text, i).is_some() {
            return true;
        }
        i += c.len_utf8();
    }
    false
}

/// 一棵 JSON 里有没有哪个**字符串值**带表情记号（Kotlin `json.anyString { EMOTE_TOKEN.containsMatchIn(it) }`）。
///
/// 只看字符串值、不看键——表情代码和正文都出现在值里。注释页几十到几百 KB，
/// 所以是"边走树边扫"，不把整棵树重新序列化。
pub fn tree_has_emote_token(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => has_emote_token(s),
        serde_json::Value::Array(list) => list.iter().any(tree_has_emote_token),
        serde_json::Value::Object(map) => map.values().any(tree_has_emote_token),
        _ => false,
    }
}

/// 一个切出来的记号。
enum Token {
    Topic { start: usize, end: usize, name: String },
    Mention { start: usize, end: usize, index: usize },
    Emote { start: usize, end: usize },
}

/// 在 `from` 之后找**最靠前**的那个记号（交替按 话题 → @ → 表情 的顺序试）。
fn next_token(line: &str, from: usize, named: &[&str]) -> Option<Token> {
    let mut i = from;
    while i < line.len() {
        let c = line[i..].chars().next()?;
        match c {
            '#' => {
                if let Some((end, name)) = match_topic(line, i) {
                    return Some(Token::Topic { start: i, end, name });
                }
            }
            '@' => {
                if let Some((end, index)) = match_mention(line, i, named) {
                    return Some(Token::Mention { start: i, end, index });
                }
            }
            '[' => {
                if let Some(end) = match_emote(line, i) {
                    return Some(Token::Emote { start: i, end });
                }
            }
            _ => {}
        }
        i += c.len_utf8();
    }
    None
}

/// 纯文本：`#日漫[话题]#` → `#日漫`，别的原样（表情代码、@昵称都留着）。信息流摘要用。
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap_or('\u{fffd}');
        if c == '#' {
            if let Some((end, name)) = match_topic(text, i) {
                out.push_str(&text[last..i]);
                out.push('#');
                out.push_str(&name);
                last = end;
                i = end;
                continue;
            }
        }
        i += c.len_utf8();
    }
    out.push_str(&text[last..]);
    out
}

/// @ 到的人：去空名、按名字去重（保留先出现的）、按 **UTF-16 长度**倒序（稳定排序）。
///
/// 为什么是 UTF-16：Kotlin 的 `String.length` 数的是码元，emoji 名字算 2。顺序会决定
/// "前缀重叠的两个名字谁先试"，所以必须一样。
fn named_mentions(mentions: &[UserRef]) -> Vec<&UserRef> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out: Vec<&UserRef> = Vec::new();
    for m in mentions {
        if m.name.is_empty() || seen.contains(&m.name.as_str()) {
            continue;
        }
        seen.push(m.name.as_str());
        out.push(m);
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.name.encode_utf16().count()));
    out
}

/// 把一行切成 Span（`XhsBody.spans`）。
pub fn spans(
    line: &str,
    emotes: &[(String, String)],
    topics: &[(String, String)],
    mentions: &[UserRef],
) -> Vec<Span> {
    let named = named_mentions(mentions);
    let names: Vec<&str> = named.iter().map(|m| m.name.as_str()).collect();

    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<Span>| {
        if !buf.is_empty() {
            out.push(Span::Text {
                text: std::mem::take(buf),
                bold: false,
                italic: false,
            });
        }
    };

    let mut last = 0;
    let mut cursor = 0;
    while let Some(token) = next_token(line, cursor, &names) {
        match token {
            Token::Topic { start, end, name } => {
                buf.push_str(&line[last..start]);
                last = end;
                cursor = end;
                flush(&mut buf, &mut out);
                match topics.iter().find(|(n, _)| *n == name) {
                    Some((_, id)) => out.push(Span::Link {
                        text: format!("#{name}"),
                        url: topic_url(id),
                        target: None,
                    }),
                    None => out.push(Span::Text {
                        text: format!("#{name}"),
                        bold: false,
                        italic: false,
                    }),
                }
            }
            Token::Mention { start, end, index } => {
                buf.push_str(&line[last..start]);
                last = end;
                cursor = end;
                flush(&mut buf, &mut out);
                let user = named[index];
                out.push(Span::Mention {
                    text: line[start..end].to_string(),
                    user: user.clone(),
                });
            }
            Token::Emote { start, end } => {
                buf.push_str(&line[last..start]);
                last = end;
                cursor = end;
                let code = &line[start..end];
                match emotes.iter().find(|(c, _)| c == code) {
                    Some((_, url)) => {
                        flush(&mut buf, &mut out);
                        out.push(Span::Emote {
                            text: code.to_string(),
                            url: url.clone(),
                        });
                    }
                    // 表里没有（或者这次没带表）：记号原样留在文字里，等后面合并
                    None => buf.push_str(code),
                }
            }
        }
    }
    buf.push_str(&line[last..]);
    flush(&mut buf, &mut out);

    // 连续的文字合并，免得被没认出来的记号切碎（Kotlin 的 fold）
    let mut merged: Vec<Span> = Vec::with_capacity(out.len());
    for span in out {
        match (merged.last_mut(), &span) {
            (
                Some(Span::Text {
                    text,
                    bold: false,
                    italic: prev_italic,
                }),
                Span::Text {
                    text: next,
                    bold: false,
                    italic,
                },
            ) if !*prev_italic && !*italic => text.push_str(next),
            _ => merged.push(span),
        }
    }
    merged
}

/// 一段文字（可以有换行）→ 段落块：一行一个段落，空行丢掉（`XhsBody.blocks`）。
pub fn blocks(
    text: &str,
    emotes: &[(String, String)],
    topics: &[(String, String)],
    mentions: &[UserRef],
) -> Vec<Block> {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !hosted::is_blank(line))
        .map(|line| Block::Paragraph {
            spans: spans(line, emotes, topics, mentions),
        })
        .collect()
}

// ===========================================================================
// 笔记详情（`XhsNote` / `XhsVideo.size`）
// ===========================================================================

/// 用户主页地址（`XhsParser.userUrl`）。
fn user_url(id: &str) -> String {
    xhs::user_url(id)
}

/// 卡片 / 详情里的作者：`user_id`、`nickname`（= `nick_name`）、`avatar`。
fn user_of(o: &JsonMap) -> Option<UserRef> {
    let id = json::text(o, "user_id")?;
    let name = json::text(o, "nickname")
        .or_else(|| json::text(o, "nick_name"))
        .unwrap_or_else(|| id.clone());
    let avatar = json::text(o, "avatar").map(|a| json::to_https(&a));
    Some(UserRef {
        platform: XHS.to_string(),
        id: id.clone(),
        name,
        avatar,
        url: Some(user_url(&id)),
    })
}

/// 详情里的作者：只认 `nickname` / `avatar`（**没有** `nick_name` 那一档，和 `XhsNote` 用的是同一个
/// `XhsParser.user`）。评论里是 `nickname` / `image`，见 [`comment_user`]。
fn note_user(o: &JsonMap) -> Option<UserRef> {
    user_of(o)
}

/// 评论里的作者：`user_id`、`nickname`、`image`（`XhsComments.user`）。
fn comment_user(o: &JsonMap) -> Option<UserRef> {
    let id = json::text(o, "user_id")?;
    let avatar = json::text(o, "image")
        .or_else(|| json::text(o, "avatar"))
        .map(|a| json::to_https(&a));
    Some(UserRef {
        platform: XHS.to_string(),
        id: id.clone(),
        name: json::text(o, "nickname").unwrap_or_else(|| id.clone()),
        avatar,
        url: Some(user_url(&id)),
    })
}

/// 一张图（`XhsNote.image`）：`url_default` → `url_pre` → `url`，`file_id` 是原图。
fn note_image(o: &JsonMap) -> Option<Block> {
    let url = json::text(o, "url_default")
        .or_else(|| json::text(o, "url_pre"))
        .or_else(|| json::text(o, "url"))
        .map(|u| json::to_https(&u))?;
    let original = json::text(o, "file_id").map(|id| format!("https://sns-img-bd.xhscdn.com/{id}"));
    Some(Block::Image {
        url: url.clone(),
        width: json::int(o, "width"),
        height: json::int(o, "height"),
        original: original.filter(|o| *o != url),
        caption: None,
    })
}

/// 评论里配的图（`XhsComments.picture`）：没有 `file_id`（原图），也没有 `caption`。
fn comment_picture(o: &JsonMap) -> Option<Block> {
    let url = json::text(o, "url_default")
        .or_else(|| json::text(o, "url_pre"))
        .or_else(|| json::text(o, "url"))
        .map(|u| json::to_https(&u))?;
    Some(Block::Image {
        url,
        width: json::int(o, "width"),
        height: json::int(o, "height"),
        original: None,
        caption: None,
    })
}

/// `note_card.video.media.stream.<分组>[]` 里带 `master_url` 的那些（`XhsVideo.streams`）。
///
/// **已知差异**：Kotlin 的 `JsonObject` 保留 JSON 里的键顺序，Rust 这边（`serde_json` 没开
/// `preserve_order`）是按键名排序的。只有 [`video_size`] 的"面积并列"那一档会看出来
/// （`maxByOrNull` 取**第一个**最大值）；真实响应里各档面积都不相同，对拍样本也没造这个并列。
pub(crate) fn streams(note: &JsonMap) -> Vec<(String, &JsonMap)> {
    let groups = match json::obj(note, "video").and_then(|v| json::obj(v, "media")).and_then(|m| json::obj(m, "stream")) {
        Some(g) => g,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for (group, list) in groups.iter() {
        let Some(list) = list.as_array() else { continue };
        for item in list {
            let Some(o) = item.as_object() else { continue };
            if json::text(o, "master_url").is_some() {
                out.push((group.clone(), o));
            }
        }
    }
    out
}

/// 视频的像素尺寸（`XhsVideo.size`）：`media_v2` 里那一档优先，没有就取流里面积最大的。
pub(crate) fn video_size(note: &JsonMap) -> (Option<i32>, Option<i32>) {
    let v2 = json::obj(note, "video")
        .and_then(|v| json::text(v, "media_v2"))
        .and_then(|text| json::parse_object(&text).ok());
    if let Some(vv) = v2.as_ref().and_then(|m| json::obj(m, "video")) {
        let (w, h) = super::play::display_size(vv);
        if w.is_some() && h.is_some() {
            return (w, h);
        }
    }
    // `maxByOrNull`：只有**严格大于**才换，所以并列时留第一个。
    let mut best: Option<(Option<i32>, Option<i32>)> = None;
    let mut best_area = 0i64;
    for (_, o) in streams(note) {
        let (w, h) = super::play::display_size(o);
        let area = (w.unwrap_or(0) as i64) * (h.unwrap_or(0) as i64);
        if best.is_none() || area > best_area {
            best = Some((w, h));
            best_area = area;
        }
    }
    best.unwrap_or((None, None))
}

/// 详情响应里的 `note_card`；没有就报"入口的 xsec_token 失效或缺失"（和 Kotlin 同一句文案）。
pub fn note_card(root: &JsonMap) -> Result<JsonMap, BiliError> {
    let found = json::obj(root, "data")
        .and_then(|d| json::arr(d, "items"))
        .and_then(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object())
                .find_map(|o| json::obj(o, "note_card").cloned())
        });
    found.ok_or_else(|| BiliError::Api {
        code: "xsec_token".to_string(),
        detail: "笔记打不开：入口里的 xsec_token 已失效或缺失（很久以前收藏的笔记会这样），请从信息流或作者主页重新打开"
            .to_string(),
    })
}

/// 标题 + 正文拼在一起（`XhsNote.texts`）：检查表情记号用。
pub fn note_texts(note: &JsonMap) -> String {
    let title = json::string(note, "title").unwrap_or_default();
    let desc = json::string(note, "desc").unwrap_or_default();
    format!("{title}\n{desc}")
}

/// 笔记详情（`XhsNote.detail`）。
pub fn detail_of(
    note: &JsonMap,
    reference: &xhs::NoteRef,
    emotes: &[(String, String)],
    now_sec: i64,
) -> FeedDetail {
    let id = json::text(note, "note_id").unwrap_or_else(|| reference.note_id.clone());
    let is_video = json::text(note, "type").as_deref() == Some("video");
    let title = json::text(note, "title");
    let desc = json::string(note, "desc").unwrap_or_default();
    let topics: Vec<(String, String)> = json::arr(note, "tag_list")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter(|o| json::text(o, "type").as_deref() == Some("topic"))
                .filter_map(|o| {
                    let name = json::text(o, "name")?;
                    let topic_id = json::text(o, "id")?;
                    Some((name, topic_id))
                })
                .collect()
        })
        .unwrap_or_default();
    let mentions: Vec<UserRef> = json::arr(note, "at_user_list")
        .map(|list| list.iter().filter_map(|v| v.as_object()).filter_map(note_user).collect())
        .unwrap_or_default();
    let images: Vec<Block> = json::arr(note, "image_list")
        .map(|list| list.iter().filter_map(|v| v.as_object()).filter_map(note_image).collect())
        .unwrap_or_default();
    let cover = images.first().and_then(|b| match b {
        Block::Image { url, .. } => Some(url.clone()),
        _ => None,
    });
    let duration = json::obj(note, "video")
        .and_then(|v| json::obj(v, "capa"))
        .and_then(|c| json::int(c, "duration"));
    let interact = json::obj(note, "interact_info");
    let summary = {
        let plain = plain(&desc);
        let trimmed = hosted::kotlin_trim(&plain);
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };

    let media = if is_video {
        let first = images.first();
        let (w, h) = match first {
            Some(Block::Image { width, height, .. }) => (*width, *height),
            _ => (None, None),
        };
        vec![Media::Video {
            cover,
            duration_sec: duration,
            width: w,
            height: h,
        }]
    } else {
        images
            .iter()
            .filter_map(|b| match b {
                Block::Image {
                    url, width, height, ..
                } => Some(Media::Image {
                    url: url.clone(),
                    width: *width,
                    height: *height,
                }),
                _ => None,
            })
            .collect()
    };

    let xsec_source = reference.xsec_source.clone().unwrap_or_else(|| xhs::SOURCE_FEED.to_string());
    let item = FeedItem {
        platform: XHS.to_string(),
        id: id.clone(),
        kind: if is_video { ItemKind::Video } else { ItemKind::Post },
        title: title.clone(),
        summary: summary.clone(),
        language: None,
        author: json::obj(note, "user").and_then(note_user),
        published_at: json::long(note, "time")
            .map(|t| t / 1000)
            .or_else(|| xhs::id_time(&id, now_sec)),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: interact
                .and_then(|i| json::string(i, "liked_count"))
                .and_then(|t| json::parse_count(Some(&t))),
            favorites: interact
                .and_then(|i| json::string(i, "collected_count"))
                .and_then(|t| json::parse_count(Some(&t))),
            comments: interact
                .and_then(|i| json::string(i, "comment_count"))
                .and_then(|t| json::parse_count(Some(&t))),
            shares: interact
                .and_then(|i| json::string(i, "share_count"))
                .and_then(|t| json::parse_count(Some(&t))),
            ..Default::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: xhs::emotes_used(emotes, &[title.as_deref(), summary.as_deref()]),
        adult: false,
        url: Some(xhs::note_url(&id, reference.xsec_token.as_deref(), &xsec_source)),
        detail: DetailTarget::Native {
            id: xhs::native_id(&id, &xsec_source, reference.xsec_token.as_deref()),
            platform: None,
        },
    };

    // 图文笔记：图片先（轮播图的顺序），再正文；视频笔记：正文就是简介，视频放 FeedDetail.video
    let mut body = Vec::new();
    if !is_video {
        body.extend(images.iter().cloned());
    }
    body.extend(blocks(&desc, emotes, &topics, &mentions));

    let video = if is_video {
        let (width, height) = video_size(note);
        Some(VideoInfo {
            parts: vec![VideoPart {
                id: "main".to_string(),
                title: None,
                duration_sec: duration,
                width,
                height,
            }],
            danmaku_segment_ms: None,
        })
    } else {
        None
    };

    FeedDetail {
        item,
        body: RichText { blocks: body },
        video,
        follow: None,
        related: Vec::new(),
    }
}

/// 详情接口的响应 → `FeedDetail`。`emotes` 是 Kotlin 手上已有的表情表（没有就是空表，**不在这里取**）。
pub fn parse_detail(
    body: &str,
    http_code: i32,
    reference: &xhs::NoteRef,
    emotes: &[(String, String)],
    now_sec: i64,
) -> Result<FeedDetail, BiliError> {
    let root = xhs::check_http(body, http_code)?;
    let note = note_card(&root)?;
    Ok(detail_of(&note, reference, emotes, now_sec))
}

// ===========================================================================
// 评论 / 楼中楼（`XhsComments`）
// ===========================================================================

/// 置顶评论的标记（`XhsComments.PINNED_TAGS`）：录到的样本里只见过 `is_author`，
/// 置顶的写法没见到，这几个是按常见写法猜的。
const PINNED_TAGS: [&str; 3] = ["top", "is_top", "pinned"];

/// 一条评论（`XhsComments.comment`）。`invalid` 或没有 `id` 的返回 `None`（调用方丢掉）。
pub fn comment_of(o: &JsonMap, emotes: &[(String, String)]) -> Option<Comment> {
    if json::bool(o, "invalid") == Some(true) {
        return None;
    }
    let id = json::text(o, "id")?;
    let mentions: Vec<UserRef> = json::arr(o, "at_users")
        .map(|list| list.iter().filter_map(|v| v.as_object()).filter_map(comment_user).collect())
        .unwrap_or_default();
    let mut blocks = blocks(
        &json::string(o, "content").unwrap_or_default(),
        emotes,
        &[],
        &mentions,
    );
    if let Some(list) = json::arr(o, "pictures") {
        blocks.extend(list.iter().filter_map(|v| v.as_object()).filter_map(comment_picture));
    }
    let tags: Vec<String> = json::arr(o, "show_tags")
        .map(|list| list.iter().filter_map(json::primitive_content).collect())
        .unwrap_or_default();
    let preview: Vec<Comment> = json::arr(o, "sub_comments")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|s| comment_of(s, emotes))
                .collect()
        })
        .unwrap_or_default();

    let mut comment = Comment::new(XHS, id, RichText { blocks });
    comment.author = json::obj(o, "user_info").and_then(comment_user);
    comment.published_at = json::long(o, "create_time").map(|t| t / 1000);
    comment.likes = json::string(o, "like_count").and_then(|t| json::parse_count(Some(&t)));
    comment.reply_count = json::string(o, "sub_comment_count").and_then(|t| json::parse_count(Some(&t)));
    comment.reply_to = json::obj(o, "target_comment")
        .and_then(|t| json::obj(t, "user_info"))
        .and_then(comment_user);
    comment.ip_location = json::text(o, "ip_location");
    comment.pinned = tags.iter().any(|t| PINNED_TAGS.contains(&t.as_str()));
    comment.preview_replies = preview;
    Some(comment)
}

/// 一页评论（一级评论 / 楼中楼同一个形状）：`data.{comments[], cursor, has_more}`。
///
/// 没有 `data` 时给空页（和 Kotlin 一样，不报错）；`next` 只在 `has_more` 且这一页有条目时给。
pub fn parse_comment_page(body: &str, http_code: i32, emotes: &[(String, String)]) -> Result<CommentPage, BiliError> {
    let root = xhs::check_http(body, http_code)?;
    let Some(data) = json::obj(&root, "data") else {
        return Ok(CommentPage::default());
    };
    let items: Vec<Comment> = json::arr(data, "comments")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|o| comment_of(o, emotes))
                .collect()
        })
        .unwrap_or_default();
    let has_more = json::bool(data, "has_more") == Some(true);
    let next = json::text(data, "cursor").filter(|_| has_more && !items.is_empty());
    Ok(CommentPage { items, next })
}

/// 一级评论（`GET /api/sns/web/v2/comment/page` 的响应）。和 [`parse_comment_page`] 同一份实现，
/// 分成两个名字只是为了和 Kotlin 的 `comments` / `replies` 一一对上。
pub fn parse_comments(body: &str, http_code: i32, emotes: &[(String, String)]) -> Result<CommentPage, BiliError> {
    parse_comment_page(body, http_code, emotes)
}

/// 楼中楼（`GET /api/sns/web/v2/comment/sub/page` 的响应）。
pub fn parse_replies(body: &str, http_code: i32, emotes: &[(String, String)]) -> Result<CommentPage, BiliError> {
    parse_comment_page(body, http_code, emotes)
}

