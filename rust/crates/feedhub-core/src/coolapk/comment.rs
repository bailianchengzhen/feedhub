//! 酷安**评论 / 楼中楼**（`/v6/feed/replyList`）的解析，逐条对照 `source-coolapk/CoolapkParser.replyList`
//! / `CoolapkParser.comment`。
//!
//! 一级评论和楼中楼是**同一个接口、同一种实体**（`entityType=feed_reply`），区别只在请求参数：
//! 一级评论 `feedType=feed` + `discussMode=1`，楼中楼 `feedType=feed_reply` + `discussMode=0`
//! （参数在 [`super::source`] 里拼）。响应里：
//! - `data[]` 里 `entityType=feed_reply` 的才是评论，卡片（评论区推广位之类）丢掉；
//! - 一级评论可能带 `replyRows`（楼中楼预览，解析时 `preview=true` 只解析一层，不再展开预览里的预览）；
//! - 游标是 `FeedCursor`（`page` + `firstItem` / `lastItem`），算法和列表一样：`page+1` + 这一页最后一条的 id。
//!
//! 请求要不要人机验证**不在这里管**：那是 [`super::source`] 的事（`err_request_captcha_v2` → 专门的错误）。

use super::emoji::EmoteTable;
use super::html;
use super::{CoolapkError, Cursor, COOLAPK};
use crate::detail::{Block, Comment, CommentPage, RichText};
use crate::json::{self, JsonMap};
use crate::model::{TimePrecision, UserRef};

/// 评论列表 / 楼中楼（`CoolapkParser.replyList`）。
pub fn reply_list(
    json: &JsonMap,
    cursor: &Cursor,
    emotes: &[(String, String)],
) -> Result<CommentPage, CoolapkError> {
    let table = EmoteTable::new(emotes);
    let data = json::arr(json, "data").ok_or_else(|| CoolapkError::Parse {
        detail: "评论列表缺少 data 数组".to_string(),
    })?;
    let entities: Vec<&JsonMap> = data.iter().filter_map(|v| v.as_object()).collect();
    let items: Vec<Comment> = entities
        .iter()
        .filter(|o| json::string(o, "entityType").as_deref() == Some("feed_reply"))
        .filter_map(|o| comment(o, false, &table))
        .collect();
    let next = super::next_cursor(&entities, "feed_reply", cursor);
    Ok(CommentPage { items, next })
}

/// 一条评论（一级评论或楼中楼）。`preview` 为 true 时**不**解析内嵌的楼中楼预览（`replyRows`）。
pub fn comment(o: &JsonMap, preview: bool, emotes: &EmoteTable) -> Option<Comment> {
    let id = super::id(o)?;
    let mut blocks: Vec<Block> = html::to_detail_blocks(
        &json::string(o, "message").unwrap_or_default(),
        emotes,
    );
    blocks.extend(pictures(o));
    let ruid = json::string(o, "ruid").filter(|s| !s.trim().is_empty() && s != "0");
    let reply_to = ruid.map(|uid| {
        let name = json::text(o, "rusername").unwrap_or_else(|| uid.clone());
        UserRef {
            platform: COOLAPK.to_string(),
            url: Some(super::user_url(&uid)),
            id: uid,
            name,
            avatar: None,
        }
    });
    let preview_replies = if preview {
        Vec::new()
    } else {
        json::arr(o, "replyRows")
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.as_object())
                    .filter_map(|r| comment(r, true, emotes))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(Comment {
        platform: COOLAPK.to_string(),
        id,
        author: super::author(o),
        content: RichText { blocks },
        published_at: json::long(o, "dateline").filter(|n| *n > 0),
        published_at_precision: TimePrecision::Second,
        likes: json::long(o, "likenum"),
        reply_count: json::long(o, "replynum"),
        reply_to,
        ip_location: json::text(o, "ip_location"),
        floor: None,
        pinned: false,
        preview_replies,
        language: None,
    })
}

/// `CoolapkParser.pictures`：`picArr`（没有就退回 `pic`）→ 图片块，去重保序、统一 https、带尺寸。
fn pictures(o: &JsonMap) -> Vec<Block> {
    super::pictures(o)
        .into_iter()
        .map(|(url, width, height)| Block::Image {
            url,
            width,
            height,
            original: None,
            caption: None,
        })
        .collect()
}

