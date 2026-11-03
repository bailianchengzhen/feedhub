//! 小黑盒**一级评论 / 楼中楼**的解析，逐条对照 `source-heybox/HeyboxParser.kt` 的
//! `comments` / `replies` / `comment` 和 `HeyboxBody.comment`。
//!
//! 结构：
//! - `result.comments` 一级评论是"组"的数组，每组 `comment` = [根评论, 子评论…]（子评论只带前两条
//!   预览，总数在根评论的 `child_num`）；分页用 `page`，`has_more_floors=1`
//!   才有下一页，游标就是下一页的页码；
//! - 楼中楼是**平铺**的子评论，`lastval` 是下一页的游标；游标没推进（等于上一页的 `lastval`）就当到头；
//! - 置顶是根评论的 `top_comment=1`（`is_top` 只是热门排序下的"热评"标记，不是置顶）；
//! - `is_meaningless=1` 是被折叠的（无意义 / 广告）评论，丢掉；只有"插眼"标记的评论没有文字，
//!   显示成 `[插眼]`。
//!
//! 评论正文：纯文本里认 URL / 表情代码；带标签的走 [`crate::html`]（`newlineIsBreak = true`，
//! 和 Kotlin `HeyboxBody.comment` 一样把文本里的换行当分段）；配图是 `imgs[]`。

use crate::detail::{Comment, CommentPage, RichText};
use crate::json::{self, JsonMap};
use crate::model::TimePrecision;

use super::detail::{html_blocks, BodyEnv};
use super::text::{self, VideoSeq};
use super::{is_blank, user, HeyboxError, HEYBOX};

fn parse_err(detail: impl Into<String>) -> HeyboxError {
    HeyboxError::Parse {
        detail: detail.into(),
    }
}

/// 一级评论：`/bbs/app/link/tree` 的响应（`page` 是这一页的页码）。
pub fn parse_comments(
    body: &str,
    page: i32,
    emotes: &[(String, String)],
) -> Result<CommentPage, HeyboxError> {
    let root = json::parse_object(body).map_err(parse_err)?;
    parse_comments_object(&root, page, emotes)
}

/// 同 [`parse_comments`]，入参是已经解析好的树。
pub fn parse_comments_object(
    root: &JsonMap,
    page: i32,
    emotes: &[(String, String)],
) -> Result<CommentPage, HeyboxError> {
    parse_comment_page(root, page, emotes)
}

fn parse_comment_page(
    root: &JsonMap,
    page: i32,
    emotes: &[(String, String)],
) -> Result<CommentPage, HeyboxError> {
    let result = json::obj(root, "result").ok_or_else(|| parse_err("评论缺少 result"))?;
    let groups = json::arr(result, "comments").ok_or_else(|| parse_err("评论缺少 result.comments"))?;
    let env = BodyEnv {
        emotes: emotes.to_vec(),
        games: Vec::new(),
    };
    let mut items: Vec<Comment> = Vec::new();
    for group in groups {
        let Some(group) = group.as_object() else {
            continue;
        };
        let list: Vec<&JsonMap> = json::arr(group, "comment")
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_object())
            .collect();
        let Some(first) = list.first() else {
            continue;
        };
        let Some(root_comment) = comment(first, None, &env) else {
            continue;
        };
        let id = root_comment.id.clone();
        let previews = list[1..]
            .iter()
            .filter_map(|c| comment(c, Some(&id), &env))
            .collect();
        items.push(Comment {
            preview_replies: previews,
            ..root_comment
        });
    }
    let more = json::bool(result, "has_more_floors") == Some(true) && !groups.is_empty();
    Ok(CommentPage {
        items,
        next: if more {
            Some(page.wrapping_add(1).to_string())
        } else {
            None
        },
    })
}

/// 楼中楼：`/bbs/app/comment/sub/comments` 的响应。`from` 是上一页的游标（判"游标没推进"）。
pub fn parse_replies(
    body: &str,
    root_id: &str,
    emotes: &[(String, String)],
    from: Option<&str>,
) -> Result<CommentPage, HeyboxError> {
    let root = json::parse_object(body).map_err(parse_err)?;
    parse_replies_object(&root, root_id, emotes, from)
}

/// 同 [`parse_replies`]，入参是已经解析好的树。
pub fn parse_replies_object(
    root: &JsonMap,
    root_id: &str,
    emotes: &[(String, String)],
    from: Option<&str>,
) -> Result<CommentPage, HeyboxError> {
    let result = json::obj(root, "result").ok_or_else(|| parse_err("楼中楼缺少 result"))?;
    let list =
        json::arr(result, "comments").ok_or_else(|| parse_err("楼中楼缺少 result.comments"))?;
    let env = BodyEnv {
        emotes: emotes.to_vec(),
        games: Vec::new(),
    };
    let items: Vec<Comment> = list
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|c| comment(c, Some(root_id), &env))
        .collect();
    let last = json::string(result, "lastval")
        .or_else(|| {
            list.last()
                .and_then(|v| v.as_object())
                .and_then(|o| json::string(o, "commentid"))
        })
        .filter(|s| !is_blank(s));
    // 游标没推进就当到头，不要原地打转
    let next = if json::bool(result, "has_more") == Some(true)
        && last.is_some()
        && last.as_deref() != from
    {
        last
    } else {
        None
    };
    Ok(CommentPage { items, next })
}

/// 一条评论。[`root_id`] 为 `None` 表示根评论（带楼层、置顶、楼中楼总数），否则是它下面的子评论。
fn comment(o: &JsonMap, root_id: Option<&str>, env: &BodyEnv) -> Option<Comment> {
    if json::bool(o, "is_meaningless") == Some(true) {
        return None;
    }
    let id = json::string(o, "commentid").filter(|s| !is_blank(s) && s != "0")?;
    let root = root_id.is_none();
    Some(Comment {
        platform: HEYBOX.to_string(),
        id,
        author: json::obj(o, "user").and_then(user),
        content: content(o, env),
        published_at: json::long(o, "create_at").filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        likes: json::long(o, "up"),
        reply_count: if root { json::long(o, "child_num") } else { None },
        reply_to: json::obj(o, "replyuser").and_then(user),
        ip_location: json::text(o, "ip_location"),
        floor: if root {
            json::int(o, "floor_num").filter(|f| *f > 0)
        } else {
            None
        },
        pinned: root && json::int(o, "top_comment") == Some(1),
        preview_replies: Vec::new(),
        language: None,
    })
}

/// 评论正文（Kotlin `HeyboxBody.comment`）：文字（可能有表情代码和 `<a>`）+ 配图。
fn content(o: &JsonMap, env: &BodyEnv) -> RichText {
    let text = json::string(o, "text").unwrap_or_default();
    let mut out = Vec::new();
    if !is_blank(&text) {
        if text::contains_any_tag(&text) {
            let mut seq = VideoSeq::new(None);
            out.extend(html_blocks(
                &text,
                env,
                &[],
                &mut Vec::new(),
                true,
                &mut seq,
            ));
        } else {
            out.extend(text::plain_blocks(&text, &env.emotes));
        }
    }
    for img in json::arr(o, "imgs").map(|v| v.as_slice()).unwrap_or_default() {
        let Some(img) = img.as_object() else {
            continue;
        };
        let Some(url) = json::text(img, "thumb")
            .or_else(|| json::text(img, "url"))
            .map(|u| super::keep_animation(&json::to_https(&u)))
        else {
            continue;
        };
        out.push(crate::detail::Block::Image {
            original: original_of(&url),
            url,
            width: json::int(img, "width"),
            height: json::int(img, "height"),
            caption: None,
        });
    }
    if out.is_empty() && json::bool(o, "is_cy") == Some(true) {
        out.push(crate::detail::Block::Paragraph {
            spans: vec![crate::detail::Span::Text {
                text: "[插眼]".to_string(),
                bold: false,
                italic: false,
            }],
        });
    }
    RichText { blocks: out }
}

/// 缩略图地址带 `?imageMogr2/…` 处理参数，去掉就是原图（Kotlin `originalOf`）。
fn original_of(url: &str) -> Option<String> {
    let base = url.split('?').next().unwrap_or("");
    if url.contains("imageMogr2") && base != url {
        Some(base.to_string())
    } else {
        None
    }
}
