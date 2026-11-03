//! B站 **评论 / 楼中楼** 的解析，逐条对照 `source-bilibili/BiliCommentParser.kt`。
//!
//! 评论条目的结构（视频、动态、专栏的评论区都一样）：
//! - `content.message` 是纯文本：里面的 `[doge]` 对应 `content.emote` 的键、`@名字` 对应
//!   `content.members` / `content.at_name_to_mid`、`content.jump_url` 的键是文本里要变成链接的关键词，
//!   `content.pictures` 是评论配图；
//! - 楼中楼里回复别人的评论，`message` 开头自带 `回复 @某某 :`（`parent` ≠ `root` 时），这里拆成
//!   [`Comment::reply_to`]；
//! - `replies` 是楼中楼预览（一般两条），`rcount` 是楼中楼总数，`reply_control.location` 是 `IP属地：xx`；
//! - 置顶评论在第一页的 `top_replies` 里（其余页没有），放在最前面并标 [`Comment::pinned`]。
//!
//! 两个和 Kotlin 对齐的口径：
//! 1. `users` 表是"名字 → 用户"：`at_name_to_mid` 先填、`members` 后覆盖（同名时后面的赢，位置不变）；
//! 2. `回复 @某某 :` 的拆分按**名字长度倒序**试（长的先试），然后 `trimStart` → 去一个 `:` 或 `：` → `trimStart`。

use std::cmp::Ordering;

use crate::detail::{Block, Comment, CommentPage, RichText, Span};
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, UserRef, BILI};

use super::ids;
use super::text as bili_text;
use crate::bili::text::Token;

const IP_PREFIX: &str = "IP属地：";

/// 一页一级评论。置顶评论（`top_replies`）放在最前面并标 `pinned`。
/// 游标是 `cursor.pagination_reply.next_offset`，`cursor.is_end` 为 true 或没有 offset 就是到头了。
pub fn main_list(json: &JsonMap) -> Result<CommentPage, String> {
    let data = json::obj(json, "data").ok_or_else(|| "评论缺少 data".to_string())?;
    let pinned: Vec<Comment> = json::arr(data, "top_replies")
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| comment(o, true))
        .collect();
    let normal: Vec<Comment> = json::arr(data, "replies")
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| comment(o, false))
        .filter(|c| !pinned.iter().any(|p| p.id == c.id))
        .collect();
    let cursor = json::obj(data, "cursor");
    let next_offset = cursor
        .and_then(|c| json::obj(c, "pagination_reply"))
        .and_then(|p| json::text(p, "next_offset"));
    let end = cursor.and_then(|c| json::bool(c, "is_end")) == Some(true);
    let next = if end || next_offset.is_none() || (normal.is_empty() && pinned.is_empty()) {
        None
    } else {
        next_offset
    };
    let mut items = pinned;
    items.extend(normal);
    Ok(CommentPage { items, next })
}

/// 楼中楼的一页。页码从 1 开始，`data.page.count` 是总数。
pub fn sub_replies(json: &JsonMap, pn: i32, ps: i32) -> Result<CommentPage, String> {
    let data = json::obj(json, "data").ok_or_else(|| "楼中楼缺少 data".to_string())?;
    let items: Vec<Comment> = json::arr(data, "replies")
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| comment(o, false))
        .collect();
    let total = json::obj(data, "page")
        .and_then(|p| json::long(p, "count"))
        .unwrap_or(0);
    let next = if !items.is_empty() && (pn as i64) * (ps as i64) < total {
        Some((pn + 1).to_string())
    } else {
        None
    };
    Ok(CommentPage { items, next })
}

/// 一条评论（一级或楼中楼）。不可见的（`invisible`）返回 `None`。
pub fn comment(o: &JsonMap, pinned: bool) -> Option<Comment> {
    if json::bool(o, "invisible") == Some(true) {
        return None;
    }
    let id = json::string(o, "rpid_str")
        .or_else(|| json::string(o, "rpid"))
        .filter(|s| !s.is_empty() && s != "0")?;
    let root = json::long(o, "root").unwrap_or(0);
    let parent = json::long(o, "parent").unwrap_or(0);
    let reply_prefix = root != 0 && parent != root && parent != 0;
    let (content, reply_to) = content(json::obj(o, "content"), reply_prefix);
    let author = json::obj(o, "member")
        .and_then(|m| {
            super::user(
                json::string(m, "mid"),
                json::text(m, "uname"),
                json::text(m, "avatar"),
            )
        })
        .or_else(|| {
            super::user(
                json::string(o, "mid_str").or_else(|| json::string(o, "mid")),
                None,
                None,
            )
        });
    let reply_control = json::obj(o, "reply_control");
    let ip_location = reply_control
        .and_then(|c| json::text(c, "location"))
        .map(|l| l.strip_prefix(IP_PREFIX).unwrap_or(&l).to_string())
        .map(|l| bili_text::trim_start(&bili_text::trim_end(&l)).to_string())
        .filter(|l| !l.is_empty());
    Some(Comment {
        platform: BILI.to_string(),
        id,
        author,
        content,
        published_at: json::long(o, "ctime").filter(|t| *t > 0),
        published_at_precision: crate::model::TimePrecision::Second,
        likes: json::long(o, "like"),
        reply_count: json::long(o, "rcount"),
        reply_to,
        ip_location,
        floor: None,
        pinned: pinned || reply_control.and_then(|c| json::bool(c, "is_up_top")) == Some(true),
        preview_replies: json::arr(o, "replies")
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_object())
            .filter_map(|r| comment(r, false))
            .collect(),
        language: None,
    })
}

/// 评论正文 → 富文本，以及 `回复 @某某 :` 开头拆出来的被回复的人。
pub fn content(c: Option<&JsonMap>, reply_prefix: bool) -> (RichText, Option<UserRef>) {
    let Some(c) = c else {
        return (RichText::EMPTY, None);
    };
    let mut text = bili_text::normalize(&json::string(c, "message").unwrap_or_default());

    // @：名字 → 用户。members 里有头像，at_name_to_mid 只有 mid
    let mut users: Vec<(String, UserRef)> = Vec::new();
    if let Some(map) = c.get("at_name_to_mid").and_then(|v| v.as_object()) {
        for (name, mid) in map {
            let user = json::primitive_content(mid).and_then(|m| super::user(Some(m), Some(name.clone()), None));
            bili_text::merge_user(&mut users, name, user);
        }
    }
    for m in json::arr(c, "members").map(|v| v.as_slice()).unwrap_or_default() {
        let Some(o) = m.as_object() else { continue };
        let Some(name) = json::text(o, "uname") else { continue };
        let user = super::user(
            json::string(o, "mid"),
            Some(name.clone()),
            json::text(o, "avatar"),
        );
        bili_text::merge_user(&mut users, &name, user);
    }

    let mut reply_to: Option<UserRef> = None;
    if reply_prefix {
        // Kotlin `sortedByDescending { it.key.length }`：稳定的、按名字长度倒序
        let mut order: Vec<usize> = (0..users.len()).collect();
        order.sort_by(|a, b| {
            let by_len = users[*b].0.len().cmp(&users[*a].0.len());
            if by_len == Ordering::Equal {
                a.cmp(b)
            } else {
                by_len
            }
        });
        for i in order {
            let (name, user) = &users[i];
            let head = format!("回复 @{name}");
            if !text.starts_with(&head) {
                continue;
            }
            reply_to = Some(user.clone());
            let rest = &text[head.len()..];
            let rest = bili_text::trim_start(rest);
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            let rest = rest.strip_prefix('：').unwrap_or(rest);
            text = bili_text::trim_start(rest).to_string();
            break;
        }
    }

    let mut tokens: Vec<Token> = Vec::new();
    if let Some(emotes) = c.get("emote").and_then(|v| v.as_object()) {
        for (code, e) in emotes {
            let url = e
                .as_object()
                .and_then(|e| json::text(e, "gif_url").or_else(|| json::text(e, "url")));
            let Some(url) = url else { continue };
            tokens.push(Token::new(
                code.clone(),
                Span::Emote {
                    text: code.clone(),
                    url: json::to_https(&url),
                },
            ));
        }
    }
    for (name, user) in &users {
        tokens.push(Token::new(
            format!("@{name}"),
            Span::Mention {
                text: format!("@{name}"),
                user: user.clone(),
            },
        ));
    }
    if let Some(jumps) = c.get("jump_url").and_then(|v| v.as_object()) {
        for (keyword, j) in jumps {
            let Some(span) = link_span(keyword, j.as_object()) else { continue };
            let once = j.as_object().and_then(|j| json::bool(j, "match_once")) == Some(true);
            tokens.push(Token {
                text: keyword.clone(),
                span,
                once,
            });
        }
    }

    let mut blocks = bili_text::paragraphs(&bili_text::spans(&text, &tokens));
    for p in json::arr(c, "pictures").map(|v| v.as_slice()).unwrap_or_default() {
        let Some(o) = p.as_object() else { continue };
        let Some(url) = json::text(o, "img_src") else { continue };
        blocks.push(Block::Image {
            url: json::to_https(&url),
            width: json::int(o, "img_width").filter(|w| *w > 0),
            height: json::int(o, "img_height").filter(|h| *h > 0),
            original: None,
            caption: None,
        });
    }
    (RichText { blocks }, reply_to)
}

/// `jump_url` 的一项 → 链接。键是评论里的原文，`title` 是该显示的文字（BV 号、短网址会换成视频标题 / "网页链接"）。
/// 搜索关键词的 `pc_url` 带一长串埋点参数，只留 `keyword`；没有 `pc_url` 的从键本身推（BV 号、网址）。
fn link_span(keyword: &str, j: Option<&JsonMap>) -> Option<Span> {
    let title = j.and_then(|j| json::text(j, "title")).unwrap_or_else(|| keyword.to_string());
    let pc = j.and_then(|j| json::text(j, "pc_url")).map(|u| json::to_https(&u));
    let url: String = match pc {
        Some(pc) => match bili_text::parse_http_url(&pc) {
            Some(u) if u.host == "search.bilibili.com" => {
                let raw = bili_text::query_parameter(&pc, "keyword").unwrap_or_else(|| keyword.to_string());
                // Java `URLEncoder.encode(s, "UTF-8")` 把空格编成 `+`，Kotlin 再把 `+` 换回 `%20`
                format!(
                    "https://search.bilibili.com/all?keyword={}",
                    bili_text::java_url_encode(&raw).replace('+', "%20")
                )
            }
            _ => pc,
        },
        None if ids::is_bvid(keyword) => super::video_url(keyword),
        None if keyword.starts_with("http://") || keyword.starts_with("https://") => {
            keyword.to_string()
        }
        None => return None,
    };
    let target = if ids::is_bvid(keyword) {
        Some(DetailTarget::Native {
            id: keyword.to_string(),
            platform: None,
        })
    } else {
        bili_text::target(&url)
    };
    Some(Span::Link {
        text: title,
        url,
        target,
    })
}
