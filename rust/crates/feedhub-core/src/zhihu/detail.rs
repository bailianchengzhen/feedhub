//! 知乎**详情**（回答 / 文章 / 想法 / 视频 / 问题）的解析，逐条对照 `source-zhihu/ZhihuDetail.kt`。
//!
//! 请求由 [`super::source::ZhihuHostedSource`] 发（宿主回调），本模块一个 socket 都不碰。
//! 条目部分复用信息流的映射（[`super::target_item`]，和 Kotlin `ZhihuParser.target` 是同一份），
//! 再用详情里更全的字段补全（收藏数、浏览数、专栏名）：
//!
//! | 类型 | 详情接口 | 补的字段 |
//! |---|---|---|
//! | 回答 | `/api/v4/answers/{id}` | `community` = 问题标题；`favlists_count`（或 `reaction.statistics.favorites`）/ `visited_count` |
//! | 文章 | `/api/v4/articles/{id}` | `community` = 专栏名；题图（`title_image`）放正文最前 |
//! | 想法 | `/api/v4/pins/{id}` | `content` 数组（text / image / link_card / poll / video）逐个变成块 |
//! | 视频 | `/api/v4/zvideos/{id}` | 简介是正文，`video.video_id` 进 `FeedDetail.video`（拿不到就退回卡片） |
//! | 问题 | `/api/v4/questions/{id}` | 正文是问题描述；回答数 / 关注数拼进 `reason` |
//!
//! 类型和内容对不上（拿文章喂回答）也是 `ParseException`，文案和 Kotlin 一样（带 `id=` 的值）。

use super::html;
use super::links;
use crate::detail::{Block, FeedDetail, RichText, Span, VideoInfo, VideoPart};
use crate::json::{self, JsonMap, JsonValue};
use crate::model::{DetailTarget, FeedItem, ItemKind};

/// 一条详情。`besides` 是"别的类型"的文案（Kotlin 的 `ZhihuDetail.detail` 按类型分派）。
pub fn detail(
    kind: &str,
    json: &JsonMap,
    body_emotes: &[(String, String)],
    bundled_emotes: &[(String, String)],
) -> Result<FeedDetail, String> {
    match kind {
        "answer" => answer(json, bundled_emotes),
        "article" => article(json, bundled_emotes),
        "pin" => pin(json, body_emotes, bundled_emotes),
        "zvideo" => zvideo(json, bundled_emotes),
        "question" => question(json, bundled_emotes),
        _ => Err(format!("不认识的内容类型 {kind}")),
    }
}

/// 回答。标题和 community 都是问题标题；正文 `content` 是 HTML。
fn answer(json: &JsonMap, bundled: &[(String, String)]) -> Result<FeedDetail, String> {
    let mut item = expect(json, bundled, ItemKind::Answer, "回答缺少 id 或所属问题")?;
    item.community = item.title.clone();
    item.stats.favorites = json::long(json, "favlists_count")
        .or_else(|| json::obj(json, "reaction").and_then(|r| json::obj(r, "statistics")).and_then(|s| json::long(s, "favorites")));
    item.stats.views = json::long(json, "visited_count");
    let body = html::parse(&json::text(json, "content").unwrap_or_default(), &[], false);
    Ok(detail_of(item, body))
}

/// 文章。题图（`title_image`）放在正文最前面；community 是所属专栏名。
fn article(json: &JsonMap, bundled: &[(String, String)]) -> Result<FeedDetail, String> {
    let mut item = expect(json, bundled, ItemKind::Article, "文章缺少 id")?;
    item.community = json::obj(json, "column").and_then(|c| json::text(c, "title"));
    item.stats.favorites = json::long(json, "favlists_count")
        .or_else(|| json::obj(json, "reaction").and_then(|r| json::obj(r, "statistics")).and_then(|s| json::long(s, "favorites")));
    item.stats.views = json::long(json, "visited_count");
    let cover = json::text(json, "title_image")
        .or_else(|| json::text(json, "image_url"))
        .map(|url| Block::Image {
            url: json::to_https(&url),
            width: json::long(json, "image_width").map(|n| n as i32),
            height: json::long(json, "image_height").map(|n| n as i32),
            original: None,
            caption: None,
        });
    let mut blocks: Vec<Block> = cover.into_iter().collect();
    blocks.extend(html::parse(&json::text(json, "content").unwrap_or_default(), &[], false).blocks);
    Ok(detail_of(item, RichText { blocks }))
}

/// 想法。`content` 是数组：`text`（HTML，可能带"标题 | "前缀）、`image`、`link_card`（引用的内容）；
/// 投票（`poll`）和视频（`video`）按卡片处理。
fn pin(
    json: &JsonMap,
    body_emotes: &[(String, String)],
    bundled: &[(String, String)],
) -> Result<FeedDetail, String> {
    let mut item = expect(json, bundled, ItemKind::Moment, "想法缺少 id")?;
    item.stats.favorites = json::long(json, "favorite_count").or_else(|| json::long(json, "favlists_count"));
    item.stats.views = json::long(json, "page_view_count");
    let mut blocks: Vec<Block> = Vec::new();
    for entry in json::arr(json, "content").map(Vec::as_slice).unwrap_or_default() {
        let Some(o) = entry.as_object() else {
            continue;
        };
        blocks.extend(pin_block(o, item.url.as_deref(), body_emotes));
    }
    Ok(detail_of(item, RichText { blocks }))
}

/// 想法文字开头的"标题 | "：标题和正文用 `|` 连在一起，标题是纯文字，正文是 HTML。
///
/// Kotlin 用的是 `^([^<]+?)\s*\|\s*(?=<)`。手写：第一个 `|`（前面不能出现 `<`），
/// 它后面跳过空白紧跟 `<` 就算命中；正文从那个 `<` 开始（和惰性匹配 + 贪婪 `\s*` 的结果一致）。
fn split_pin_title(html: &str) -> Option<(&str, &str)> {
    for (index, c) in html.char_indices() {
        if c == '<' {
            return None;
        }
        if c != '|' {
            continue;
        }
        let title = html[..index].trim_end_matches(crate::hosted::is_java_whitespace);
        if title.is_empty() {
            continue;
        }
        let rest = &html[index + 1..];
        let trimmed = rest.trim_start_matches(crate::hosted::is_java_whitespace);
        if trimmed.starts_with('<') {
            return Some((title, trimmed));
        }
    }
    None
}

fn pin_block(o: &JsonMap, pin_url: Option<&str>, emotes: &[(String, String)]) -> Vec<Block> {
    match json::string(o, "type").as_deref() {
        Some("text") => {
            let raw = json::string(o, "content").unwrap_or_default();
            match split_pin_title(&raw) {
                None => html::parse(&raw, emotes, true).blocks,
                Some((title, rest)) => {
                    let mut blocks = vec![Block::Heading {
                        spans: vec![Span::Text {
                            text: title.trim_matches(crate::hosted::is_java_whitespace).to_string(),
                            bold: false,
                            italic: false,
                        }],
                    }];
                    blocks.extend(html::parse(rest, emotes, true).blocks);
                    blocks
                }
            }
        }
        Some("image") => {
            let Some(url) = json::text(o, "url") else {
                return Vec::new();
            };
            let url = json::to_https(&url);
            let original = json::text(o, "original_url").map(|u| json::to_https(&u)).filter(|o| *o != url);
            vec![Block::Image {
                url,
                width: json::long(o, "width").map(|n| n as i32),
                height: json::long(o, "height").map(|n| n as i32),
                original,
                caption: None,
            }]
        }
        Some("link_card") => {
            let url = json::text(o, "url").map(|u| json::to_https(&u));
            let id = json::text(o, "data_content_type")
                .and_then(|t| json::text(o, "data_content_id").map(|c| format!("{t}:{c}")));
            let target = url
                .as_deref()
                .and_then(links::target)
                .or_else(|| id.filter(|i| links::split_id(i).is_some()).map(|id| DetailTarget::Native { id, platform: None }));
            vec![Block::Card {
                title: json::text(o, "data_draft_title").or(url.clone()).unwrap_or_else(|| "链接".to_string()),
                subtitle: None,
                cover: json::text(o, "data_draft_cover").map(|c| json::to_https(&c)),
                url,
                target,
            }]
        }
        Some("poll") => vec![Block::Card {
            title: "投票".to_string(),
            subtitle: None,
            cover: None,
            url: pin_url.map(str::to_string),
            target: None,
        }],
        Some("video") => vec![Block::Card {
            title: "视频".to_string(),
            subtitle: None,
            cover: json::text(o, "thumbnail")
                .or_else(|| json::obj(o, "video_info").and_then(|v| json::text(v, "thumbnail")))
                .map(|u| json::to_https(&u)),
            url: pin_url.map(str::to_string),
            target: None,
        }],
        _ => Vec::new(),
    }
}

/// 知乎视频：简介是正文，视频是 [`FeedDetail::video`]（`partId` = `video.video_id`）。
/// 拿不到 video_id 才退回一张带封面的卡片。
fn zvideo(json: &JsonMap, bundled: &[(String, String)]) -> Result<FeedDetail, String> {
    let mut item = expect(json, bundled, ItemKind::Video, "视频缺少 id")?;
    item.stats.favorites = json::long(json, "favlists_count");
    let description = json::text(json, "description").or_else(|| json::text(json, "excerpt"));
    let desc = description
        .map(|d| html::parse(&d, &[], true).blocks)
        .unwrap_or_default();
    if let Some(video) = video_info(json) {
        return Ok(FeedDetail {
            item,
            body: RichText { blocks: desc },
            video: Some(video),
            follow: None,
            related: Vec::new(),
        });
    }
    let video = json::obj(json, "video");
    let seconds = video.and_then(|v| json::long(v, "duration")).map(|n| n as i32);
    let subtitle = match seconds {
        None => "视频".to_string(),
        Some(s) => format!("视频 · {}:{:02}", s / 60, s % 60),
    };
    let cover = video
        .and_then(|v| json::text(v, "thumbnail"))
        .or_else(|| json::text(json, "image_url"))
        .map(|u| json::to_https(&u));
    let card = Block::Card {
        title: item.title.clone().unwrap_or_else(|| "知乎视频".to_string()),
        subtitle: Some(subtitle),
        cover,
        url: item.url.clone(),
        target: None,
    };
    let mut blocks = desc;
    blocks.push(card);
    Ok(detail_of(item, RichText { blocks }))
}

/// 问题：正文是问题描述（`detail` HTML），不列回答。
fn question(json: &JsonMap, bundled: &[(String, String)]) -> Result<FeedDetail, String> {
    let mut item = expect(json, bundled, ItemKind::Question, "问题缺少 id")?;
    let parts: Vec<String> = [
        json::long(json, "answer_count").map(|n| format!("{n} 个回答")),
        json::long(json, "follower_count").map(|n| format!("{n} 人关注")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let reason = parts.join(" · ");
    item.reason = if reason.is_empty() { None } else { Some(reason) };
    item.stats.views = json::long(json, "visit_count");
    item.stats.likes = json::long(json, "voteup_count");
    let body = html::parse(&json::text(json, "detail").unwrap_or_default(), &[], false);
    Ok(detail_of(item, body))
}

/// `ZhihuVideo.info`：zvideo 的 `FeedDetail.video`（一个分 P，`partId` = `video.video_id`）。
fn video_info(json: &JsonMap) -> Option<VideoInfo> {
    let video = json::obj(json, "video")?;
    let id = json::text(video, "video_id").filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?;
    Some(VideoInfo {
        parts: vec![VideoPart {
            id,
            title: None,
            duration_sec: json::double(video, "duration").map(|d| d as i32).filter(|v| *v > 0),
            width: json::long(video, "width").map(|n| n as i32).filter(|v| *v > 0),
            height: json::long(video, "height").map(|n| n as i32).filter(|v| *v > 0),
        }],
        danmaku_segment_ms: None,
    })
}

// ---------------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------------

fn detail_of(item: FeedItem, body: RichText) -> FeedDetail {
    FeedDetail {
        item,
        body,
        video: None,
        follow: None,
        related: Vec::new(),
    }
}

/// `ZhihuParser.target(json)?.takeIf { it.kind == expected } ?: ParseException(what)`。
fn expect(
    json: &JsonMap,
    bundled: &[(String, String)],
    expected: ItemKind,
    what: &str,
) -> Result<FeedItem, String> {
    let item = super::target_item(json, bundled).filter(|item| item.kind == expected);
    match item {
        Some(item) => Ok(item),
        None => Err(format!("{what}（id={}）", display_string(json.get("id")))),
    }
}

/// Kotlin 字符串模板里 `${json.string("id")}` 的样子：`null` 或它的文本。
fn display_string(value: Option<&JsonValue>) -> String {
    match value {
        None | Some(JsonValue::Null) => "null".to_string(),
        Some(JsonValue::Object(_)) | Some(JsonValue::Array(_)) => "null".to_string(),
        Some(other) => json::as_string(other).unwrap_or_else(|| "null".to_string()),
    }
}

