//! 知乎**问题 + 回答楼层**的版面：点开一条回答，上面是**问题**（标题 + 描述 +
//! 提问者），评论区放**这个问题下的回答**，推过来的那条置顶；每条回答自己的评论当楼中楼。
//!
//! 只是解析，请求在 [`super::source::ZhihuHostedSource::qa_detail`] / `qa_answers` / `qa_replies`。
//!
//! | 东西 | 接口 | 变成什么 |
//! |---|---|---|
//! | 问题 | `/api/v4/questions/{qid}?include=…` | 详情的标题 / 正文 / 作者（提问者）/ 计数 |
//! | 回答列表 | `/api/v4/questions/{qid}/feeds?include=…&order=default\|updated` | 一条回答 = 一"楼"（[`Comment`]，id `answer:<id>`） |
//! | 回答的评论 | `/api/v4/comment_v5/answers/{id}/root_comment` | 楼中楼（一级评论后面跟着它的子评论预览，摊平） |
//!
//! 详情里的条目**身份不变**（id / key / 链接 / 类型还是这条回答）：收藏、历史、分享、"在知乎 App 打开"都还是回答，
//! 只是显示的字段换成问题的。

use super::detail;
use super::html;
use crate::detail::{Comment, CommentPage, FeedDetail};
use crate::json::{self, JsonMap};
use crate::model::{FeedItem, Stats, TimePrecision, UserRef};

/// 一条回答 → 评论区里的一"楼"。内容是整篇回答（界面上长的先折叠）；赞同数当点赞，评论数当楼中楼总数。
pub fn answer_floor(t: &JsonMap) -> Option<Comment> {
    let id = json::string(t, "id")?;
    let content = html::parse(&json::text(t, "content").unwrap_or_default(), &[], false);
    let mut out = Comment::new(super::ZHIHU, format!("answer:{id}"), content);
    out.author = json::obj(t, "author").and_then(super::user);
    out.published_at = json::long(t, "created_time").filter(|v| *v > 0);
    out.published_at_precision = TimePrecision::Second;
    out.likes = json::long(t, "voteup_count");
    out.reply_count = json::long(t, "comment_count");
    Some(out)
}

/// 回答列表（`feeds`）：`data[].target` 是回答（`target_type == "answer"`，广告之类的别的类型跳过）。
/// [`skip`] 是置顶那条回答的楼 id（`answer:<id>`），在列表里再出现就去掉。
pub fn answers_page(json: &JsonMap, skip: Option<&str>) -> Result<CommentPage, String> {
    let data = json::arr(json, "data").ok_or_else(|| "回答列表缺少 data".to_string())?;
    let mut items = Vec::new();
    for entry in data {
        let Some(e) = entry.as_object() else { continue };
        let target = json::obj(e, "target").unwrap_or(e);
        let kind = json::string(e, "target_type").or_else(|| json::string(target, "type"));
        if kind.as_deref() != Some("answer") {
            continue;
        }
        if let Some(floor) = answer_floor(target) {
            if Some(floor.id.as_str()) != skip {
                items.push(floor);
            }
        }
    }
    Ok(CommentPage {
        items,
        next: super::comment::next_cursor(json)?,
    })
}

/// 提问者。**拿不到就是 `None`**（匿名提问 id 是 `"0"`、或者干脆没有 `author`），界面上就不显示那一行。
pub fn asker(q: &JsonMap) -> Option<UserRef> {
    json::obj(q, "author").and_then(super::user).filter(|u| u.id != "0")
}

/// 问题详情 → 版面上面那块。[`answer`] 是推过来的那条回答（点回答进来时给；点问题进来时 `None`）：
/// 给了就**保留回答的身份**，只换显示的字段。
///
/// - 标题 / 正文：问题标题、问题描述；
/// - 作者：提问者（[`asker`]，拿不到就没有）；时间：提问时间；
/// - 计数：浏览数、**回答数放在 `comments`**（评论区放的就是回答，标题上的总数跟着它）；
/// - `reason`：`N 个回答 · N 人关注`（问题详情原来就这么拼）。
pub fn question_view(q: &JsonMap, bundled: &[(String, String)], answer: Option<&FeedItem>) -> Result<FeedDetail, String> {
    let mut d = detail::detail("question", q, &[], bundled)?;
    let stats = Stats {
        views: json::long(q, "visit_count"),
        comments: json::long(q, "answer_count"),
        ..Stats::default()
    };
    let author = asker(q);
    let created = json::long(q, "created").or_else(|| json::long(q, "created_time")).filter(|v| *v > 0);
    match answer {
        Some(a) => {
            let mut item = a.clone();
            item.title = d.item.title.clone();
            item.summary = d.item.summary.clone();
            item.author = author;
            item.published_at = created;
            item.published_at_precision = TimePrecision::Second;
            item.reason = d.item.reason.clone();
            item.community = None;
            item.media = Vec::new();
            item.stats = stats;
            d.item = item;
        }
        None => {
            d.item.author = author;
            if created.is_some() {
                d.item.published_at = created;
            }
            d.item.stats = stats;
        }
    }
    Ok(d)
}

/// 回答的一级评论摊平成一列（楼中楼面板只画一层）：每条一级评论后面紧跟它的子评论预览；
/// 子评论没写回复谁的，就记成回复这条一级评论的作者（读起来是"回复 @某某"）。
pub fn flatten(page: CommentPage) -> CommentPage {
    let mut items = Vec::new();
    for mut root in page.items {
        let children = std::mem::take(&mut root.preview_replies);
        let root_author = root.author.clone();
        items.push(root);
        for mut child in children {
            if child.reply_to.is_none() {
                child.reply_to = root_author.clone();
            }
            items.push(child);
        }
    }
    CommentPage { items, next: page.next }
}

