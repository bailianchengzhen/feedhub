//! 抖音评论解析（`/aweme/v1/web/comment/list/` 一级、`/aweme/v1/web/comment/list/reply/` 楼中楼）。
//!
//! 两个接口的评论项结构一样，共用一个 [`comment`]。逐行照抄 `source-douyin/DouyinComments.kt`：
//!
//! 见过的字段：`comments[]`，每项 `{cid text user{uid sec_uid nickname avatar_thumb}
//! create_time(秒) digg_count reply_comment_total ip_label stick_position level is_hot reply_comment
//! text_extra image_list video_list}`；顶层 `{comments cursor has_more total status_code}`。
//! 这一页里 `reply_comment`（楼中楼预览）都是 null、`image_list` / `video_list` 也都是 null。
//!
//! 几个口径：
//! - **置顶**：`stick_position` 是真实字段，但录到的一页 19 条全是 0，**没有见过非 0 的样本**；
//!   `pinned` 按 "> 0 = 置顶" 处理（按字段名的理解，未经证实）。
//! - **游标**：`cursor` 且 `has_more` 为真**且这一页有内容**才给下一页（Kotlin 的 `takeIf`）。
//! - **评论配图** `image_list[]` 的形状没有在真实响应里见过（录到的那页全是 null），所以两种写法都认：
//!   `{thumb_url:{url_list},origin_url:{url_list},width,height}` 与平铺的 `{url_list,width,height}`。
//! - `video_list` 仍然没见样本，不做。

use crate::detail::{Block, Comment, CommentPage, RichText};
use crate::douyin::{DouyinError, DOUYIN};
use crate::model::UserRef;
use crate::json::{self, JsonMap};

/// 一页评论正文（含楼中楼预览）的拼串：取表情表之前先扫它，避免拿整段 JSON 去匹配 `[xxx]`
/// （JSON 的方括号会误报，白花一个请求）。每条后面跟一个 `\n`（和 Kotlin 的 `append('\n')` 一致）。
pub fn texts(json: &JsonMap) -> String {
    fn collect(o: &JsonMap, out: &mut String) {
        if let Some(text) = json::text(o, "text") {
            out.push_str(&text);
            out.push('\n');
        }
        for value in json::arr(o, "reply_comment").into_iter().flatten() {
            if let Some(reply) = value.as_object() {
                collect(reply, out);
            }
        }
    }
    let mut out = String::new();
    for value in json::arr(json, "comments").into_iter().flatten() {
        if let Some(o) = value.as_object() {
            collect(o, &mut out);
        }
    }
    out
}

/// 一页评论。`next` 为 `None` 表示没有下一页。
pub fn page(json: &JsonMap, emotes: &[(String, String)]) -> CommentPage {
    let items: Vec<Comment> = json::arr(json, "comments")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|o| comment(o, emotes))
                .collect()
        })
        .unwrap_or_default();
    let cursor = json::long(json, "cursor").map(|n| n.to_string());
    let has_more = json::bool(json, "has_more") == Some(true);
    let next = cursor.filter(|_| has_more && !items.is_empty());
    CommentPage { items, next }
}

/// 一条评论（一级和楼中楼同一个类型）。没有 id 的丢掉。
pub fn comment(o: &JsonMap, emotes: &[(String, String)]) -> Option<Comment> {
    // 一级评论是 cid，楼中楼里有的是 reply_id
    let id = json::text(o, "cid").or_else(|| json::text(o, "reply_id"))?;
    let text = json::text(o, "text").unwrap_or_default();
    let mut blocks: Vec<Block> = Vec::new();
    if !text.is_empty() {
        blocks.push(Block::Paragraph {
            spans: super::text::split_emotes(&text, emotes),
        });
    }
    // 评论配图
    for value in json::arr(o, "image_list").into_iter().flatten() {
        if let Some(image) = value.as_object().and_then(picture) {
            blocks.push(image);
        }
    }
    let mut out = Comment::new(DOUYIN, id, RichText { blocks });
    out.author = json::obj(o, "user").and_then(user);
    out.published_at = json::long(o, "create_time").filter(|t| *t > 0);
    out.likes = json::long(o, "digg_count");
    out.reply_count = json::long(o, "reply_comment_total");
    out.reply_to = reply_to(o);
    out.ip_location = json::text(o, "ip_label");
    // 置顶：真实响应里有 `stick_position`（录到的 19 条全是 0 = 不置顶），"> 0 就是置顶"没见非 0 样本
    out.pinned = json::long(o, "stick_position").unwrap_or(0) > 0;
    out.preview_replies = json::arr(o, "reply_comment")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|s| comment(s, emotes))
                .collect()
        })
        .unwrap_or_default();
    Some(out)
}

/// 楼中楼里"回复 @xxx"：`reply_to_userid` / `reply_to_username` 是两个平铺字段，没有头像也没有 sec_uid，
/// 所以 `url` 留 `None`（不要去猜主页地址）。
fn reply_to(o: &JsonMap) -> Option<UserRef> {
    let uid = json::text(o, "reply_to_userid")?;
    Some(UserRef {
        platform: DOUYIN.to_string(),
        id: uid.clone(),
        name: json::text(o, "reply_to_username").unwrap_or_else(|| uid.clone()),
        avatar: None,
        url: None,
    })
}

fn user(o: &JsonMap) -> Option<UserRef> {
    let sec_uid = json::text(o, "sec_uid");
    let uid = json::text(o, "uid");
    let id = sec_uid.clone().or(uid)?;
    let name = json::text(o, "nickname").unwrap_or_else(|| id.clone());
    let avatar = json::obj(o, "avatar_thumb")
        .and_then(|a| first_primitive_content(a, "url_list"))
        .map(|u| json::to_https(&u));
    let url = sec_uid.map(|s| crate::douyin::user_url(&s));
    Some(UserRef {
        platform: DOUYIN.to_string(),
        id,
        name,
        avatar,
        url,
    })
}

/// `url_list` 里第一个 JSON 原始值（**包括** `null` → 字符串 `"null"`，Kotlin 的 `JsonNull` 也是
/// `JsonPrimitive`）；头像地址用的是这个口径，和 `DouyinComments.avatar` 一样。
fn first_primitive_content(o: &JsonMap, key: &str) -> Option<String> {
    json::arr(o, key)?
        .iter()
        .find_map(json::primitive_content)
}

/// `url_list` 里第一个**非空白**的原始值（`DouyinComments.urlList`）。
fn url_list(o: Option<&JsonMap>) -> Option<String> {
    o.and_then(|o| json::arr(o, "url_list"))
        .map(|list| {
            list.iter()
                .filter_map(json::primitive_content)
                .filter(|u| !u.trim().is_empty())
                .next()
        })
        .flatten()
}

/// 评论配图（`image_list[]`）。显示用缩略图（`thumb_url` 优先，平铺的 `url_list` 次之），
/// `original` 放原图（大图查看层用）；两种都没有就跳过，不猜地址。
fn picture(o: &JsonMap) -> Option<Block> {
    let flat = url_list(Some(o));
    let small = url_list(json::obj(o, "thumb_url")).or_else(|| flat.clone());
    let full = url_list(json::obj(o, "origin_url"))
        .or_else(|| url_list(json::obj(o, "download_url")))
        .or_else(|| flat.clone());
    let url = json::to_https(&small.or(full.clone())?);
    let original = full
        .map(|f| json::to_https(&f))
        .filter(|f| *f != url);
    Some(Block::Image {
        url,
        width: json::int(o, "width").filter(|v| *v > 0),
        height: json::int(o, "height").filter(|v| *v > 0),
        original,
        caption: None,
    })
}

/// `decode_response` 之后的便捷入口（`check_code` 由调用方做；这里只把类型收一下）。
pub fn page_from_body(
    body: &str,
    emotes: &[(String, String)],
) -> Result<CommentPage, DouyinError> {
    let json = json::parse_object(body).map_err(|detail| DouyinError::Parse { detail })?;
    Ok(page(&json, emotes))
}

