//! 知乎**评论 / 楼中楼**的解析，逐条对照 `source-zhihu/ZhihuDetail.comments` / `comment` /
//! `nextCursor` / `stickers` / `hasEmoteCandidates`。
//!
//! `comment_v5` 的响应是 `{data: [评论], paging: {is_end, next}}`，每条评论带 `child_comments`
//! （楼中楼预览，只有一级评论有）。**游标就是 `paging.next` 的 query**（里面已经带排序和 offset），
//! 路径和域名由平台对象固定——所以游标被改成别的地址也带不走 Cookie。
//!
//! 游标这里有个坑：**最后一页 `is_end` 为 true 时 `next` 仍然有值**（指向 offset 回到起点），
//! 所以不能只看有没有 `next`。Kotlin 那个 `substringAfter('?', "")` 在没有 `?` 时给空串 → 没有下一页。

use super::html;
use crate::detail::{Comment, CommentPage};
use crate::json::{self, JsonMap};
use crate::model::TimePrecision;

/// 一级评论或楼中楼列表（`root_comment` / `child_comment`）。
pub fn comments(json: &JsonMap, emotes: &[(String, String)]) -> Result<CommentPage, String> {
    let data = json::arr(json, "data").ok_or_else(|| "评论列表缺少 data".to_string())?;
    let mut items = Vec::new();
    for entry in data {
        if let Some(o) = entry.as_object() {
            if let Some(comment) = comment(o, emotes, false) {
                items.push(comment);
            }
        }
    }
    Ok(CommentPage {
        items,
        next: next_cursor(json)?,
    })
}

/// 游标是 `paging.next` 的 query 部分。`is_end` 为 true 就是最后一页。
pub(crate) fn next_cursor(json: &JsonMap) -> Result<Option<String>, String> {
    let paging = json::obj(json, "paging").ok_or_else(|| "评论列表缺少 paging".to_string())?;
    if super::bool_of(paging, "is_end") == Some(true) {
        return Ok(None);
    }
    Ok(json::text(paging, "next")
        .and_then(|next| next.split_once('?').map(|(_, query)| query.to_string()))
        .filter(|query| !query.is_empty()))
}

/// 一条评论。楼中楼预览 `child_comments` 放 `preview_replies`，总数 `child_comment_count` 放 `reply_count`；
/// `reply_to_author` 只有回复的不是一级评论作者时才有。置顶：`is_author_top` 或 `top`。
/// IP 属地在 `comment_tag` 里 `type == "ip_info"` 那一项。
pub fn comment(o: &JsonMap, emotes: &[(String, String)], nested: bool) -> Option<Comment> {
    let id = json::text(o, "id")?;
    let content = html::parse(&json::string(o, "content").unwrap_or_default(), emotes, false);
    let mut out = Comment::new(super::ZHIHU, id, content);
    out.author = json::obj(o, "author").and_then(super::user);
    out.published_at = json::long(o, "created_time").filter(|v| *v > 0);
    out.published_at_precision = TimePrecision::Second;
    out.likes = json::long(o, "like_count");
    out.reply_count = json::long(o, "child_comment_count");
    out.reply_to = json::obj(o, "reply_to_author").and_then(super::user);
    out.ip_location = json::arr(o, "comment_tag")
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|tag| tag.as_object())
        .find(|tag| json::string(tag, "type").as_deref() == Some("ip_info"))
        .and_then(|tag| json::text(tag, "text"));
    out.pinned = super::bool_of(o, "is_author_top") == Some(true) || super::bool_of(o, "top") == Some(true);
    if !nested {
        out.preview_replies = json::arr(o, "child_comments")
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|child| child.as_object())
            .filter_map(|child| comment(child, emotes, true))
            .collect();
    }
    Some(out)
}

/// 评论列表（含楼中楼预览）或想法详情的文字里有没有 `[xxx]` 形式的表情占位符。
pub fn has_emote_candidates(json: &JsonMap) -> bool {
    fn texts(o: &JsonMap, out: &mut Vec<String>) {
        if let Some(content) = json::string(o, "content") {
            out.push(content);
        }
        for child in json::arr(o, "child_comments").map(Vec::as_slice).unwrap_or_default() {
            if let Some(child) = child.as_object() {
                texts(child, out);
            }
        }
    }
    let mut all: Vec<String> = Vec::new();
    for entry in json::arr(json, "data").map(Vec::as_slice).unwrap_or_default() {
        if let Some(o) = entry.as_object() {
            texts(o, &mut all);
        }
    }
    for entry in json::arr(json, "content").map(Vec::as_slice).unwrap_or_default() {
        let Some(o) = entry.as_object() else { continue };
        if json::string(o, "type").as_deref() != Some("text") {
            continue;
        }
        if let Some(content) = json::string(o, "content") {
            all.push(content);
        }
    }
    all.iter().any(|text| html::has_emote_candidate(text))
}

/// `/api/v4/sticker-groups/{id}`：`data.stickers[]` 的 `placeholder`（`[感谢]`）→
/// `static_image_url`（data URI），没有静态图就用 `dynamic_image_url`。同名后面的覆盖前面的（Kotlin `toMap()`）。
pub fn stickers(json: &JsonMap) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let entries = json::obj(json, "data")
        .and_then(|data| json::arr(data, "stickers"))
        .map(Vec::as_slice)
        .unwrap_or_default();
    for entry in entries {
        let Some(sticker) = entry.as_object() else { continue };
        let Some(placeholder) = json::text(sticker, "placeholder") else {
            continue;
        };
        let Some(url) = json::text(sticker, "static_image_url").or_else(|| json::text(sticker, "dynamic_image_url")) else {
            continue;
        };
        if let Some(slot) = out.iter_mut().find(|(code, _)| *code == placeholder) {
            slot.1 = url;
        } else {
            out.push((placeholder, url));
        }
    }
    out
}

