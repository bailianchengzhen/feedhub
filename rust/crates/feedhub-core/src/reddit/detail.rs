//! Reddit 的**详情 / 评论 / 楼中楼**解析。
//!
//! 逐条对照 `source-reddit/RedditSource.kt` 的 `detail` / `comments` / `replies` / `thread` /
//! `postOf` / `morePage` / `moreChildren` / `avatars` / `accountIds` 和 `RedditParser.kt` 的
//! `detail` / `body` / `comments` / `commentThread` / `moreChildren` / `comment` / `flatten` /
//! `collectComments` / `collectOne` / `idsOf`：**请求怎么拼在 `source.rs`，形状怎么解在这里**。
//!
//! 三张响应形状（都挂着 `raw_json=1`）：
//! - 详情：`[Listing(帖子), Listing(评论)]`（数组，两个元素）；
//! - 一级评论：上面那个数组的第二个 Listing；顶层 `more` 节点变成下一页游标 `more:<id1>,<id2>,…`；
//! - 楼中楼：`/comments/<postId>/_/<commentId>.json`（数组 / 单个 Listing 都认）或
//!   `/api/morechildren`（`json.data.things[]`，扁平、带 `parent_id`）。
//!
//! 作者头像：Reddit 的列表 / 评论 JSON **不带**头像，要另外用
//! `/api/user_data_by_account_ids.json?ids=t2_…` 批量取（一次最多 100 个）。这里只负责
//! "一份响应里有哪些 `author_fullname`"（[`account_ids`]，先收顶层再收楼中楼预览，照 Kotlin 的
//! 两趟扫描），真正发请求和缓存那张表在 [`crate::reddit::source`]。

use std::collections::{HashMap, HashSet};

use crate::detail::{Block, Comment, CommentPage, FeedDetail, RichText, VideoInfo, VideoPart};
use crate::json::{self, JsonMap, JsonValue};
use crate::model::{TimePrecision, UserRef};

use super::markdown;
use super::{preview_image, video_of, RedditError, VideoMeta};

/// 楼中楼预览最多几条（Kotlin `PREVIEW_MAX`，前端只画 3 条）。
pub const PREVIEW_MAX: usize = 3;

/// `more` 游标的前缀：`more:<id1>,<id2>,…`。base36 的 id 不含 `:` 和 `,`，不会歧义。
pub const MORE_PREFIX: &str = "more:";

/// `more:<id1>,<id2>`。
pub fn more_cursor(ids: &[String]) -> String {
    format!("{MORE_PREFIX}{}", ids.join(","))
}

/// 游标 → 还没展开的评论 id（`moreIds`：去掉前缀、按 `,` 切、trim、丢空）。
pub fn more_ids(cursor: &str) -> Vec<String> {
    let rest = cursor.strip_prefix(MORE_PREFIX).unwrap_or(cursor);
    rest.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ===========================================================================
// 详情
// ===========================================================================

/// `/comments/<id>.json` 的响应（数组）：`[0]` 是帖子、`[1]` 是评论。
///
/// 找不到帖子时抛 `Parse`（和 Kotlin `postOf` 的 "详情里没有帖子" 一字不差）。
pub fn post_of_thread(thread: &[JsonValue]) -> Result<&JsonMap, RedditError> {
    for el in thread {
        let Some(listing) = el.as_object() else { continue };
        let Some(child) = json::obj(listing, "data")
            .and_then(|d| json::arr(d, "children"))
            .and_then(|a| a.first())
            .and_then(|v| v.as_object())
        else {
            continue;
        };
        if json::text(child, "kind").as_deref() != Some("t3") {
            continue;
        }
        if let Some(data) = json::obj(child, "data") {
            return Ok(data);
        }
    }
    Err(RedditError::Parse {
        detail: "详情里没有帖子".to_string(),
    })
}

/// 帖子详情：`item` + 正文（selftext 的 markdown、图片 / 相册、站外链接卡片）+ 视频分 P。
///
/// [avatars] 是 `author_fullname` → 头像地址（这一页批量取到的那些；没有就留空）。
pub fn parse_detail(
    post: &JsonMap,
    avatars: &HashMap<String, String>,
) -> Result<FeedDetail, RedditError> {
    let item = super::post_with_avatars(post, Some(avatars)).ok_or_else(|| RedditError::Parse {
        detail: "帖子缺少 id".to_string(),
    })?;
    let video = video_of(post);
    let blocks = body(post, video.as_ref());
    Ok(FeedDetail {
        item,
        body: RichText { blocks },
        video: video.as_ref().map(|v| VideoInfo {
            parts: vec![VideoPart {
                id: "main".to_string(),
                title: None,
                duration_sec: v.duration_sec,
                width: v.width,
                height: v.height,
            }],
            danmaku_segment_ms: None,
        }),
        follow: None,
        related: Vec::new(),
    })
}

/// 正文：selftext（markdown）+ 图片 / 相册 + 站外链接卡片（Kotlin `RedditParser.body`）。
fn body(o: &JsonMap, video: Option<&VideoMeta>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let selftext = json::string(o, "selftext").unwrap_or_default();
    if !markdown::is_blank(&selftext) {
        blocks.extend(markdown::to_blocks(&selftext));
    }
    // 整条就是视频的帖子：视频在 FeedDetail.video，封面是它的海报，正文不放图片
    let images = if video.is_none() {
        image_blocks(o)
    } else {
        Vec::new()
    };
    blocks.extend(images.iter().cloned());
    let dest = json::text(o, "url_overridden_by_dest").or_else(|| json::string(o, "url"));
    let link_only = video.is_none()
        && images.is_empty()
        && json::bool(o, "is_self") != Some(true)
        && dest.is_some();
    if link_only {
        let dest = dest.unwrap();
        blocks.push(Block::Card {
            title: json::text(o, "title").unwrap_or_else(|| dest.clone()),
            subtitle: json::text(o, "domain"),
            cover: preview_image_relay(o),
            url: Some(dest),
            target: None,
        });
    }
    blocks
}

/// 链接卡片封面：Kotlin `previewImage(o)?.url`。
fn preview_image_relay(o: &JsonMap) -> Option<String> {
    preview_image(o).map(|i| i.url)
}

/// 正文里的图片：有相册就用相册，否则单图（Kotlin `imageBlocks`）。
fn image_blocks(o: &JsonMap) -> Vec<Block> {
    let imgs = match super::gallery_images(o) {
        Some(gallery) => gallery,
        None => preview_image(o).into_iter().collect(),
    };
    imgs.into_iter()
        .map(|i| Block::Image {
            url: i.url,
            width: i.width,
            height: i.height,
            original: i.original,
            caption: i.caption,
        })
        .collect()
}

// ===========================================================================
// 评论
// ===========================================================================

/// 一页一级评论（`RedditParser.comments`）：`t1` 进列表，`more` 的 children 进下一页游标。
pub fn parse_comments(
    listing: &JsonMap,
    avatars: &HashMap<String, String>,
) -> CommentPage {
    let Some(data) = json::obj(listing, "data") else {
        return CommentPage::default();
    };
    let mut items: Vec<Comment> = Vec::new();
    let mut more: Vec<String> = Vec::new();
    for child in json::arr(data, "children").map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(o) = child.as_object() else { continue };
        let Some(d) = json::obj(o, "data") else { continue };
        match json::text(o, "kind").as_deref() {
            Some("t1") => {
                if let Some(c) = one_comment(d, avatars) {
                    items.push(c);
                }
            }
            Some("more") => more.extend(ids_of(d)),
            _ => {}
        }
    }
    CommentPage {
        items,
        next: if more.is_empty() {
            None
        } else {
            Some(more_cursor(&more))
        },
    }
}

/// 楼中楼：`/comments/<postId>/_/<commentId>.json` 的响应（数组 / 单个 Listing 都认）。
///
/// 返回**铺平的一层**：楼中楼里内联的嵌套回复也逐条列出，`reply_to` 用 `parent_id` 在本份响应里查。
pub fn parse_comment_thread(
    element: &JsonValue,
    post_id: &str,
    target_id: &str,
    avatars: &HashMap<String, String>,
) -> CommentPage {
    let _ = post_id; // Kotlin 里也只是签名的一部分（`/comments/<postId>/_/<commentId>.json`）
    let listings: Vec<&JsonMap> = match element {
        JsonValue::Array(items) => items.iter().filter_map(|v| v.as_object()).collect(),
        JsonValue::Object(map) => vec![map],
        _ => Vec::new(),
    };
    let mut all: Vec<&JsonMap> = Vec::new();
    for listing in &listings {
        collect_comments(listing, &mut all);
    }
    let by_id = index_by_id(&all);
    let target = by_id.get(target_id).copied();
    let source: Vec<&JsonMap> = match target
        .and_then(|t| json::obj(t, "replies"))
        .and_then(|r| json::obj(r, "data"))
        .and_then(|d| json::arr(d, "children"))
    {
        Some(children) => children.iter().filter_map(|v| v.as_object()).collect(),
        None => match listings
            .last()
            .and_then(|l| json::obj(l, "data"))
            .and_then(|d| json::arr(d, "children"))
        {
            Some(children) => children.iter().filter_map(|v| v.as_object()).collect(),
            None => return CommentPage::default(),
        },
    };

    let mut out: Vec<Comment> = Vec::new();
    let mut more: Vec<String> = Vec::new();
    for o in source {
        let Some(d) = json::obj(o, "data") else { continue };
        match json::text(o, "kind").as_deref() {
            Some("t1") => flatten(d, &by_id, target_id, avatars, &mut out),
            Some("more") => more.extend(ids_of(d)),
            _ => {}
        }
    }
    CommentPage {
        items: out,
        next: if more.is_empty() {
            None
        } else {
            Some(more_cursor(&more))
        },
    }
}

/// 任意 JSON（详情 / 楼中楼的响应是数组或对象）。坏 JSON 的文案照 Kotlin `parseJsonElement`。
pub fn parse_element(body: &str) -> Result<JsonValue, RedditError> {
    serde_json::from_str::<JsonValue>(body).map_err(|_| RedditError::Parse {
        detail: format!("响应不是合法 JSON：{}", json::take_chars(body, 120)),
    })
}

/// `/api/morechildren` 的响应：`json.data.things[]`（扁平，带 `depth` / `parent_id`）。
pub fn parse_more_children(json: &JsonMap, avatars: &HashMap<String, String>) -> CommentPage {
    let Some(things) = json::obj(json, "json")
        .and_then(|j| json::obj(j, "data"))
        .and_then(|d| json::arr(d, "things"))
    else {
        return CommentPage::default();
    };
    let mut all: Vec<&JsonMap> = Vec::new();
    for thing in things {
        let Some(d) = thing.as_object().and_then(|o| json::obj(o, "data")) else {
            continue;
        };
        collect_one(d, &mut all);
    }
    let by_id = index_by_id(&all);
    let mut items: Vec<Comment> = Vec::new();
    for thing in things {
        let Some(o) = thing.as_object() else { continue };
        if json::text(o, "kind").as_deref() != Some("t1") {
            continue;
        }
        let Some(d) = json::obj(o, "data") else { continue };
        if let Some(c) = comment(d, &by_id, true, avatars, None) {
            items.push(c);
        }
    }
    CommentPage { items, next: None }
}

/// `id → data`。同一份响应里同 id 出现两次时**后出现的赢**（Kotlin 的 `toMap()`）。
fn index_by_id<'a>(all: &[&'a JsonMap]) -> HashMap<String, &'a JsonMap> {
    let mut out: HashMap<String, &'a JsonMap> = HashMap::new();
    for d in all {
        if let Some(id) = json::text(d, "id") {
            out.insert(id, d);
        }
    }
    out
}

/// 一条一级评论（`RedditParser.comment(d, avatars)`）。
fn one_comment(d: &JsonMap, avatars: &HashMap<String, String>) -> Option<Comment> {
    comment(d, &HashMap::new(), false, avatars, None)
}

/// 一条评论（含楼中楼预览、回复总数、置顶）。没有 id 返回 `None`。
///
/// `parent_is_batch_root` = "这批 id 是平铺展开的"，此时 `reply_to` 才按 `parent_id` 查——
/// 和 Kotlin `comment(d, byId, parentIsBatchRoot, avatars, transform)` 一样。
/// `reply_to_override` = `Some(Some(x))` 强制用 x、`Some(None)` 强制清空（`flatten` 用的那两条规矩）。
fn comment(
    d: &JsonMap,
    by_id: &HashMap<String, &JsonMap>,
    parent_is_batch_root: bool,
    avatars: &HashMap<String, String>,
    reply_to_override: Option<Option<UserRef>>,
) -> Option<Comment> {
    let id = json::text(d, "id")?;
    let author = super::author_with_avatars(d, Some(avatars));
    let children: &[JsonValue] = json::obj(d, "replies")
        .and_then(|r| json::obj(r, "data"))
        .and_then(|dd| json::arr(dd, "children"))
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let mut previews: Vec<Comment> = Vec::new();
    let mut children_count: i64 = 0;
    let mut more_count: i64 = 0;
    for child in children {
        let Some(o) = child.as_object() else { continue };
        let Some(cd) = json::obj(o, "data") else { continue };
        match json::text(o, "kind").as_deref() {
            Some("t1") => {
                children_count += 1;
                if previews.len() < PREVIEW_MAX {
                    if let Some(p) = comment(cd, by_id, false, avatars, None) {
                        previews.push(p);
                    }
                }
            }
            Some("more") => more_count += json::long(cd, "count").unwrap_or(0),
            _ => {}
        }
    }
    let parent_comment_id = json::text(d, "parent_id")
        .filter(|p| p.starts_with("t1_"))
        .map(|p| p["t1_".len()..].to_string());
    let reply_to = match reply_to_override {
        Some(value) => value,
        None => {
            if parent_is_batch_root {
                parent_comment_id
                    .as_deref()
                    .and_then(|pid| by_id.get(pid).copied())
                    .and_then(|parent| super::author_with_avatars(parent, Some(avatars)))
            } else {
                None
            }
        }
    };
    let total = children_count + more_count;
    Some(Comment {
        platform: super::REDDIT.to_string(),
        id,
        author,
        content: RichText {
            blocks: markdown::to_blocks(&json::string(d, "body").unwrap_or_default()),
        },
        published_at: json::long(d, "created_utc"),
        published_at_precision: TimePrecision::Second,
        likes: json::long(d, "score"),
        reply_count: if total > 0 { Some(total) } else { None },
        reply_to,
        ip_location: None,
        floor: None,
        pinned: json::bool(d, "stickied") == Some(true),
        preview_replies: previews,
        language: None,
    })
}

/// 把 `d` 及其嵌套回复逐条铺平（楼中楼面板用），`reply_to` 在 `by_id` 里查。
fn flatten(
    d: &JsonMap,
    by_id: &HashMap<String, &JsonMap>,
    target_id: &str,
    avatars: &HashMap<String, String>,
    out: &mut Vec<Comment>,
) {
    let Some(id) = json::text(d, "id") else { return };
    if out.iter().any(|c| c.id == id) {
        return;
    }
    let parent_id = json::text(d, "parent_id")
        .unwrap_or_default()
        .strip_prefix("t1_")
        .unwrap_or("")
        .to_string();
    let parent = by_id.get(&parent_id).copied();
    // 直接回目标评论的不标"回复 xxx"（面板上那条就是它）；回复别人的才标
    let reply_to = if parent_id == target_id || parent_id.is_empty() {
        Some(None)
    } else {
        Some(parent.and_then(|p| super::author_with_avatars(p, Some(avatars))))
    };
    let Some(mut c) = comment(d, by_id, true, avatars, reply_to) else {
        return;
    };
    c.preview_replies = Vec::new();
    out.push(c);
    // 嵌套的子回复（同一份响应里内联的那些）跟着铺平
    for child in json::obj(d, "replies")
        .and_then(|r| json::obj(r, "data"))
        .and_then(|dd| json::arr(dd, "children"))
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let Some(o) = child.as_object() else { continue };
        if json::text(o, "kind").as_deref() != Some("t1") {
            continue;
        }
        if let Some(cd) = json::obj(o, "data") {
            flatten(cd, by_id, target_id, avatars, out);
        }
    }
}

/// 一份 Listing 里所有 `t1` 的 `data`（含嵌套回复），按文档顺序、同 id 只留第一次。
fn collect_comments<'a>(listing: &'a JsonMap, out: &mut Vec<&'a JsonMap>) {
    for child in json::obj(listing, "data")
        .and_then(|d| json::arr(d, "children"))
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let Some(o) = child.as_object() else { continue };
        if json::text(o, "kind").as_deref() != Some("t1") {
            continue;
        }
        if let Some(d) = json::obj(o, "data") {
            collect_one(d, out);
        }
    }
}

/// 一条评论 + 它下面所有内联回复，按文档顺序铺进 `out`（同 id 只留第一次）。
fn collect_one<'a>(d: &'a JsonMap, out: &mut Vec<&'a JsonMap>) {
    let id = json::text(d, "id");
    if out.iter().any(|e| json::text(e, "id") == id) {
        return;
    }
    out.push(d);
    for child in json::obj(d, "replies")
        .and_then(|r| json::obj(r, "data"))
        .and_then(|dd| json::arr(dd, "children"))
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let Some(o) = child.as_object() else { continue };
        if json::text(o, "kind").as_deref() != Some("t1") {
            continue;
        }
        if let Some(cd) = json::obj(o, "data") {
            collect_one(cd, out);
        }
    }
}

/// `more` 节点的 `children`（还没加载的评论 id）。
fn ids_of(d: &JsonMap) -> Vec<String> {
    json::arr(d, "children")
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(json::primitive_content)
        .filter(|s| !markdown::is_blank(s))
        .collect()
}

// ===========================================================================
// 作者头像的 id 收集
// ===========================================================================

/// 一份响应里所有 `author_fullname`（去重、保序）。
///
/// **先收顶层**（帖子 + 这一页列出来的一级评论），再收楼中楼预览里的（嵌套 `replies`）：
/// 一次只有 100 个名额，按文档顺序走会把名额耗在嵌套回复上，只有前 ~40 条一级评论有头像；
/// 先顶层就能覆盖整屏（Kotlin `RedditSource.accountIds` 的两趟扫描）。
pub fn account_ids(element: &JsonValue) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    collect_account_ids(element, &mut out, &mut seen, true);
    if out.len() < super::source::MAX_AVATAR_IDS {
        collect_account_ids(element, &mut out, &mut seen, false);
    }
    out
}

fn collect_account_ids(
    element: &JsonValue,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
    skip_replies: bool,
) {
    match element {
        JsonValue::Object(map) => {
            for (k, v) in map {
                if skip_replies && k == "replies" {
                    continue;
                }
                if k == "author_fullname" {
                    if let Some(s) = json::as_string(v).filter(|s| !markdown::is_blank(s)) {
                        if seen.insert(s.clone()) {
                            out.push(s);
                        }
                    }
                } else {
                    collect_account_ids(v, out, seen, skip_replies);
                }
            }
        }
        JsonValue::Array(items) => {
            for v in items {
                collect_account_ids(v, out, seen, skip_replies);
            }
        }
        _ => {}
    }
}

/// 从作者 `author_fullname` 取账号 id（去掉 `t2_`）；**没登录 / 匿名 / 已删号**时保留原样。
/// 只有测试和排障用（生产里 `author()` 自己算）。
pub fn account_id_of(full_name: &str) -> &str {
    full_name.strip_prefix("t2_").unwrap_or(full_name)
}

