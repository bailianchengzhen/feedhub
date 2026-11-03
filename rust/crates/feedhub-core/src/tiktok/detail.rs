//! TikTok 详情 / 评论的解析。
//!
//! 对应 `source-tiktok/TiktokParser.kt` 的 `detail` / `comments` / `comment` 与 `TiktokVideo.info`
//! 的**详情那一小半**。
//!
//! ## 详情为什么是 HTML
//!
//! 详情不是接口：视频页 `/@<作者>/video/<id>` 的 HTML 里挂着
//! `__UNIVERSAL_DATA_FOR_REHYDRATION__` → `__DEFAULT_SCOPE__` → `webapp.video-detail.itemInfo.itemStruct`。
//! [`parse_video_page`] 把"抠 hydration + 取 itemStruct + 报状态码"三件事一起做（[`crate::tiktok::source`]
//! 里的请求路径用它），[`parse`] 只负责 itemStruct → `FeedDetail`。
//!
//! ## 几个实测修正过的坑
//!
//! - `video.duration` 是**秒**（1044 那条的 `size × 8 / Bitrate` 正好对上），不是毫秒；
//! - `video.cover` / `playAddr` 是**字符串**，`bitrateInfo[].PlayAddr` 是 `{UrlList:[…]}`，两种都认；
//! - `itemStruct` 里**没有** 18+ 字段；
//! - 评论的字段名同时接受 web 的 camelCase 与老接口的 snake_case。

use crate::detail::{Block, Comment, CommentPage, FeedDetail, RichText, Span, VideoInfo};
use crate::json::{self, JsonMap};
use crate::model::Media;
use crate::tiktok::{check_status, TiktokError, PLATFORM};

/// 当前登录者的主页地址（详情里不需要，`whoami` 用）。
pub(crate) fn text_only(text: String) -> RichText {
    RichText {
        blocks: vec![Block::Paragraph {
            spans: vec![Span::Text {
                text,
                bold: false,
                italic: false,
            }],
        }],
    }
}

// ===========================================================================
// 详情
// ===========================================================================

/// itemStruct → `FeedDetail`（`TiktokParser.detail`）。`id` 是 `DetailTarget::Native` 的 id
/// （`<作者>/<itemId>`）：条目本身的 `id` 会被换成裸 itemId。
pub fn parse(o: &JsonMap, id: &str) -> Result<FeedDetail, TiktokError> {
    let mut item = crate::tiktok::item(o).ok_or_else(|| TiktokError::Parse {
        detail: "这条作品没有 id，解析不出条目".to_string(),
    })?;
    let desc = item.summary.clone();
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(desc) = desc.filter(|d| !d.trim().is_empty()) {
        blocks.push(Block::Paragraph {
            spans: vec![Span::Text {
                text: desc,
                bold: false,
                italic: false,
            }],
        });
    }
    for image in crate::tiktok::image_post_images(o) {
        if let Media::Image { url, width, height } = image {
            blocks.push(Block::Image {
                url,
                width,
                height,
                original: None,
                caption: None,
            });
        }
    }
    item.id = crate::tiktok::item_id_of(id);
    Ok(FeedDetail {
        item,
        body: RichText { blocks },
        video: video_info(o),
        follow: None,
        related: Vec::new(),
    })
}

/// 从 itemStruct 的 JSON 文本解析。
pub fn parse_body(body: &str, id: &str) -> Result<FeedDetail, TiktokError> {
    let json = json::parse_object(body).map_err(|detail| TiktokError::Parse { detail })?;
    parse(&json, id)
}

/// 视频页 HTML → `FeedDetail`：抠 hydration、取 `itemStruct`，拿不到就把状态码带出来
/// （Kotlin `TiktokSource.detail` + `fetchVideoItem` 那几步）。
pub fn parse_video_page(html: &str, id: &str) -> Result<FeedDetail, TiktokError> {
    let scope = crate::tiktok::source::hydration(html).ok_or_else(|| TiktokError::Parse {
        detail: "视频页里没有 __UNIVERSAL_DATA_FOR_REHYDRATION__（可能是验证页 / 登录墙）"
            .to_string(),
    })?;
    let item = video_detail(&scope).ok_or_else(|| {
        let (status, message) = video_detail_status(&scope);
        TiktokError::Api {
            code: status.map(|s| s.to_string()).unwrap_or_else(|| "no_item".to_string()),
            detail: not_found_message(status, message.as_deref(), id),
        }
    })?;
    parse(&item, id)
}

/// 视频页 hydration 里的作品对象（`webapp.video-detail.itemInfo.itemStruct`）。
pub fn video_detail(scope: &JsonMap) -> Option<JsonMap> {
    json::obj(scope, "webapp.video-detail")
        .and_then(|d| json::obj(d, "itemInfo"))
        .and_then(|i| json::obj(i, "itemStruct"))
        .cloned()
}

/// 视频页 hydration 的错误码：10204 / 10216 = 作品不存在或不可见。
pub fn video_detail_status(scope: &JsonMap) -> (Option<i64>, Option<String>) {
    let Some(detail) = json::obj(scope, "webapp.video-detail") else {
        return (None, None);
    };
    (
        json::long(detail, "statusCode"),
        json::text(detail, "statusMsg"),
    )
}

/// 作品不存在 / 不可见时的说明（Kotlin `notFoundMessage`）。
pub fn not_found_message(status: Option<i64>, message: Option<&str>, id: &str) -> String {
    match status {
        Some(status @ (10204 | 10216)) => format!(
            "作品不存在或不可见（statusCode={status}{}）",
            message.map(|m| format!("，{m}")).unwrap_or_default()
        ),
        None => format!("视频页里没有作品对象：{id}"),
        Some(status) => format!(
            "视频页返回 statusCode={status}{}",
            message.map(|m| format!("（{m}）")).unwrap_or_default()
        ),
    }
}

/// `FeedDetail.video`：**和播放共用一份**流挑选（[`super::play::info`]）。
///
/// `info` 和 `play` 必须挑同一批流（否则"详情说有视频、
/// 点播放却说没有地址"这种漂移会出现），所以实现搬到了 [`super::play::info`]。
pub fn video_info(o: &JsonMap) -> Option<VideoInfo> {
    super::play::info(o)
}

// ===========================================================================
// 评论
// ===========================================================================

/// 一页评论（一级 / 楼中楼共用）。`statusCode` / `status_code` 不是 0 就按 [`check_status`] 抛。
pub fn comments(json: &JsonMap) -> Result<CommentPage, TiktokError> {
    check_status(json)?;
    let list = json::arr(json, "comments").ok_or_else(|| TiktokError::Parse {
        detail: "评论响应缺少 comments".to_string(),
    })?;
    let items: Vec<Comment> = list
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(comment)
        .collect();
    let has_more = json::bool(json, "hasMore").or_else(|| json::bool(json, "has_more")) == Some(true);
    let next = json::long(json, "cursor")
        .filter(|_| has_more)
        .map(|n| n.to_string());
    Ok(CommentPage { items, next })
}

/// 从响应体解析一页评论。
pub fn comments_body(body: &str) -> Result<CommentPage, TiktokError> {
    let json = json::parse_object(body).map_err(|detail| TiktokError::Parse { detail })?;
    comments(&json)
}

/// 一条评论（`reply_comment` / `replyComment` 里的预览也解析，取前两条）。
pub fn comment(o: &JsonMap) -> Option<Comment> {
    let id = json::text(o, "cid")
        .or_else(|| json::text(o, "commentId"))
        .or_else(|| json::text(o, "id"))?;
    let user = json::obj(o, "user").or_else(|| json::obj(o, "author"));
    let text = json::text(o, "text")
        .or_else(|| json::text(o, "content"))
        .unwrap_or_default();
    let mut previews: Vec<Comment> = json::arr(o, "reply_comment")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(comment)
                .collect()
        })
        .unwrap_or_default();
    if previews.is_empty() {
        previews = json::arr(o, "replyComment")
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_object())
                    .filter_map(comment)
                    .collect()
            })
            .unwrap_or_default();
    }
    let reply_to = json::obj(o, "reply_to_user")
        .and_then(|u| crate::tiktok::author(Some(u)))
        .or_else(|| {
            json::obj(o, "replyToUser").and_then(|u| crate::tiktok::author(Some(u)))
        });
    let mut comment = Comment::new(PLATFORM, id, text_only(text));
    comment.author = crate::tiktok::author(user);
    comment.published_at = json::long(o, "create_time")
        .or_else(|| json::long(o, "createTime"))
        .filter(|t| *t > 0);
    comment.likes = json::long(o, "digg_count").or_else(|| json::long(o, "diggCount"));
    comment.reply_count = json::long(o, "reply_comment_total")
        .or_else(|| json::long(o, "replyCommentTotal"));
    comment.reply_to = reply_to;
    // `stick_position` / `isPinned` 只见过 0 / false，照实映射
    comment.pinned = json::long(o, "stick_position")
        .or_else(|| json::long(o, "stickPosition"))
        .map(|v| v > 0)
        .unwrap_or_else(|| {
            json::bool(o, "isPinned")
                .or_else(|| json::bool(o, "is_pinned"))
                .unwrap_or(false)
        });
    comment.preview_replies = previews.into_iter().take(2).collect();
    Some(comment)
}

