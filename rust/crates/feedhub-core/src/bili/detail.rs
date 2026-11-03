//! B站 **详情**（视频 / 动态 / 图文 / 专栏）的解析，逐条对照
//! `source-bilibili/BiliVideoParser.kt` 和 `BiliDynamicParser.kt`。
//!
//! 只解析：请求由 `bili::source` 的平台对象发（宿主回调），本模块一个 socket 都不碰。
//! 两个响应的分工：
//!
//! - 视频：`/x/web-interface/wbi/view/detail` 一次给 `View`（标题 / 分 P / 计数）、`Card`（UP 主 + 我是否已关注）、
//!   `Related`（相关推荐）；
//! - 动态：`v1/detail` 给条目（和关注流里的动态同一个结构）+ 评论区位置（`basic.comment_type` / `comment_id_str`），
//!   图文 / 专栏的**全文**另从 `opus/detail` 取（信息流里的 `opus.summary` 是截断的）。
//!
//! 三处踩过的坑：
//!
//! 1. **商品 / 抽奖节点要丢掉**（`RICH_TEXT_NODE_TYPE_GOODS` / `LOTTERY`），链接卡片同理
//!    （`*_GOODS` / `*_ITEM_NULL`）——这些是购物卡，不是正文。
//! 2. **图文的图片有两种放法**：夹在段落里（`para_type` 2），或者在顶部的相册 `MODULE_TYPE_TOP`
//!    （这时正文里就没有图了）。相册的图片排在正文**前面**。
//! 3. **专栏代码块被 HTML 转义过两遍**（`&amp;#39;`），要解到不再变化（[`super::text::unescape_entities`]，
//!    命名实体只收了常见集，见那里的说明）。

use crate::detail::{Block, FeedDetail, FollowKind, FollowState, FollowTarget, RichText, Span, VideoInfo, VideoPart};
use crate::json::{self, JsonMap, JsonValue};
use crate::model::{DetailTarget, BILI};
use crate::bili::text as bili_text;
use crate::hosted::kotlin_trim;

use super::ids;

/// 弹幕每段 6 分钟（Kotlin `BiliVideoParser.DANMAKU_SEGMENT_MS`）。
pub const DANMAKU_SEGMENT_MS: i64 = 6 * 60 * 1000;

/// 评论区在哪：`type` 是 B站评论区类型（1 视频、11 图文、12 专栏、17 动态……），`oid` 是该类型下的资源 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentTarget {
    pub comment_type: i32,
    pub oid: String,
}

// ===========================================================================
// 视频详情（`/x/web-interface/wbi/view/detail`）
// ===========================================================================

/// `/x/web-interface/wbi/view/detail` 的响应 → [`FeedDetail`]。
///
/// `logged_in` 为 false 时 `Card.following` 一律是 false（不是真的没关注），所以此时
/// `follow.following` 给 `None`（和 Kotlin 一样）。
pub fn video_detail(json: &JsonMap, logged_in: bool) -> Result<FeedDetail, String> {
    let data = json::obj(json, "data").ok_or_else(|| "视频详情缺少 data".to_string())?;
    let view = json::obj(data, "View").ok_or_else(|| "视频详情缺少 data.View".to_string())?;
    let mut item = super::video_card(view).ok_or_else(|| "视频详情缺少 bvid".to_string())?;
    item.summary = description(view);
    Ok(FeedDetail {
        item: item.clone(),
        body: RichText {
            blocks: description_blocks(view),
        },
        video: Some(VideoInfo {
            parts: parts(view),
            danmaku_segment_ms: Some(DANMAKU_SEGMENT_MS),
        }),
        follow: follow(json::obj(data, "Card"), item.author.as_ref(), logged_in),
        related: json::arr(data, "Related")
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object())
                    .filter_map(super::video_card)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 简介：`desc`；`"-"` 是没写简介时的占位。
fn description(view: &JsonMap) -> Option<String> {
    json::text(view, "desc").filter(|d| kotlin_trim(d) != "-")
}

/// 简介 → 段落。`desc_v2` 是一串 `{raw_text, type, biz_id}`：type 1 文字，type 2 是 @。
/// 老视频 `desc_v2` 可能是 null / 空，那就用 `desc`；文字里的网址和 BV 号变成链接。
fn description_blocks(view: &JsonMap) -> Vec<Block> {
    let nodes: Vec<&JsonMap> = json::arr(view, "desc_v2")
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    let spans: Vec<Span> = if nodes.is_empty() {
        let Some(text) = description(view).map(|d| bili_text::normalize(&d)) else {
            return Vec::new();
        };
        bili_text::spans(&text, &bili_text::link_tokens(&text))
    } else {
        let mut out: Vec<Span> = Vec::new();
        for n in nodes {
            let raw = bili_text::normalize(&json::string(n, "raw_text").unwrap_or_default());
            let mid = json::string(n, "biz_id").filter(|m| m != "0");
            match (json::int(n, "type"), mid) {
                (Some(2), Some(mid)) if !raw.is_empty() => {
                    let name = raw.strip_prefix('@').unwrap_or(&raw).to_string();
                    out.push(Span::Mention {
                        text: format!("@{name}"),
                        user: bili_text::mention_user(mid, name),
                    });
                }
                _ => out.extend(bili_text::spans(&raw, &bili_text::link_tokens(&raw))),
            }
        }
        out
    };
    bili_text::paragraphs(&spans)
}

/// 分 P。没有 `pages` 时（极少见）退回只有 `cid` 的单 P。
fn parts(view: &JsonMap) -> Vec<VideoPart> {
    let pages: Vec<&JsonMap> = json::arr(view, "pages")
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    if pages.is_empty() {
        let Some(cid) = json::string(view, "cid").filter(|c| c != "0") else {
            return Vec::new();
        };
        return vec![part(
            cid,
            json::text(view, "title"),
            json::int(view, "duration"),
            json::obj(view, "dimension"),
        )];
    }
    pages
        .into_iter()
        .filter_map(|p| {
            let cid = json::string(p, "cid").filter(|c| !c.is_empty() && c != "0")?;
            Some(part(
                cid,
                json::text(p, "part"),
                json::int(p, "duration"),
                json::obj(p, "dimension"),
            ))
        })
        .collect()
}

/// `rotate` = 1 表示宽高要对调（竖屏视频按横屏编码）。
fn part(cid: String, title: Option<String>, duration: Option<i32>, dim: Option<&JsonMap>) -> VideoPart {
    let mut w = dim.and_then(|d| json::int(d, "width")).filter(|w| *w > 0);
    let mut h = dim.and_then(|d| json::int(d, "height")).filter(|h| *h > 0);
    if dim.and_then(|d| json::int(d, "rotate")) == Some(1) {
        std::mem::swap(&mut w, &mut h);
    }
    VideoPart {
        id: cid,
        title,
        duration_sec: duration,
        width: w,
        height: h,
    }
}

/// UP 主 + "我是否已关注"。
fn follow(card: Option<&JsonMap>, author: Option<&crate::model::UserRef>, logged_in: bool) -> Option<FollowState> {
    let c = card.and_then(|card| json::obj(card, "card"));
    let mid = c
        .and_then(|c| json::string(c, "mid"))
        .filter(|m| m != "0" && !m.is_empty())
        .or_else(|| author.map(|a| a.id.clone()))?;
    let name = c
        .and_then(|c| json::text(c, "name"))
        .or_else(|| author.map(|a| a.name.clone()))
        .unwrap_or_else(|| mid.clone());
    let target = FollowTarget {
        platform: BILI.to_string(),
        kind: FollowKind::User,
        id: mid.clone(),
        name,
        avatar: c
            .and_then(|c| json::text(c, "face"))
            .or_else(|| author.and_then(|a| a.avatar.clone()))
            .map(|u| json::to_https(&u)),
        description: c.and_then(|c| json::text(c, "sign")),
        url: Some(super::space_url(&mid)),
    };
    Some(FollowState {
        target,
        following: if logged_in {
            card.and_then(|card| json::bool(card, "following"))
        } else {
            None
        },
    })
}

// ===========================================================================
// 动态详情（`v1/detail` + `opus/detail`）
// ===========================================================================

/// 这条动态的正文在 `opus/detail` 里（图文 / 专栏）。
pub fn is_opus(detail: &JsonMap) -> bool {
    json::path(detail, &["data", "item", "modules", "module_dynamic", "major"])
        .and_then(|major| json::string(major, "type"))
        .as_deref()
        == Some("MAJOR_TYPE_OPUS")
}

/// `v1/detail` 的响应里取评论区（`basic.comment_type` / `comment_id_str`，没有就退 `rid_str`）。
pub fn comment_target(json: &JsonMap) -> Option<CommentTarget> {
    let item = json::path(json, &["data", "item"])?;
    let basic = json::obj(item, "basic")?;
    let comment_type = json::int(basic, "comment_type")?;
    let oid = json::string(basic, "comment_id_str")
        .or_else(|| json::string(basic, "rid_str"))
        .filter(|s| !s.is_empty() && s != "0")?;
    Some(CommentTarget { comment_type, oid })
}

/// 详情。[`detail`] 是 `v1/detail` 的响应；[`opus`] 是同一条的 `opus/detail` 响应，可以为
/// `None` 或 `item: null`（这时正文退回动态自带的摘要和图片）。
pub fn dynamic_detail(detail: &JsonMap, opus: Option<&JsonMap>) -> Result<FeedDetail, String> {
    let raw = json::path(detail, &["data", "item"])
        .ok_or_else(|| "动态详情缺少 data.item".to_string())?;
    let item = super::dynamic(raw).ok_or_else(|| "动态详情里没有可用的内容".to_string())?;
    let modules: Vec<&JsonMap> = opus
        .and_then(|o| json::path(o, &["data", "item"]))
        .and_then(|i| json::arr(i, "modules"))
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    let paragraphs = modules
        .iter()
        .find(|m| json::string(m, "module_type").as_deref() == Some("MODULE_TYPE_CONTENT"))
        .and_then(|m| json::obj(m, "module_content"))
        .and_then(|c| json::arr(c, "paragraphs"));
    let blocks = match paragraphs {
        Some(paragraphs) if !paragraphs.is_empty() => {
            // 图文的图片有两种放法：夹在段落里（para_type 2），或者放在顶部的相册 MODULE_TYPE_TOP
            let album = modules
                .iter()
                .find(|m| json::string(m, "module_type").as_deref() == Some("MODULE_TYPE_TOP"))
                .and_then(|m| json::obj(m, "module_top"))
                .and_then(|t| json::obj(t, "display"))
                .and_then(|d| json::obj(d, "album"))
                .and_then(|a| json::arr(a, "pics"));
            let mut out = pictures(album);
            out.extend(opus_blocks(paragraphs));
            out
        }
        _ => summary_blocks(raw),
    };
    Ok(FeedDetail {
        item,
        body: RichText { blocks },
        video: None,
        follow: None,
        related: Vec::new(),
    })
}

/// 动态自带的文字 + 图片：转发 / 文字动态在 `desc`，图文在 `major.opus.summary` + `pics`，
/// 老式图片动态在 `major.draw.items`。
fn summary_blocks(item: &JsonMap) -> Vec<Block> {
    let Some(dynm) = json::path(item, &["modules", "module_dynamic"]) else {
        return Vec::new();
    };
    let major = json::obj(dynm, "major");
    let nodes = json::obj(dynm, "desc")
        .and_then(|d| json::arr(d, "rich_text_nodes"))
        .or_else(|| {
            major
                .and_then(|m| json::obj(m, "opus"))
                .and_then(|o| json::obj(o, "summary"))
                .and_then(|s| json::arr(s, "rich_text_nodes"))
        });
    let (spans, images) = rich_nodes(nodes);
    let mut blocks = bili_text::paragraphs(&spans);
    blocks.extend(images);
    if let Some(opus) = major.and_then(|m| json::obj(m, "opus")) {
        for p in json::arr(opus, "pics").map(|v| v.as_slice()).unwrap_or_default() {
            let Some(o) = p.as_object() else { continue };
            if let Some(url) = json::text(o, "url") {
                blocks.push(image(url, json::int(o, "width"), json::int(o, "height")));
            }
        }
    }
    if let Some(draw) = major.and_then(|m| json::obj(m, "draw")) {
        for p in json::arr(draw, "items").map(|v| v.as_slice()).unwrap_or_default() {
            let Some(o) = p.as_object() else { continue };
            if let Some(url) = json::text(o, "src") {
                blocks.push(image(url, json::int(o, "width"), json::int(o, "height")));
            }
        }
    }
    blocks
}

// ===========================================================================
// 富文本节点（信息流 / 详情的 rich_text_nodes，以及段落里的 rich）
// ===========================================================================

/// `rich_text_nodes` → Span，另把"查看图片"节点带的图片单独返回。
/// 节点类型见 `BiliDynamicParser.richNodes` 的注释（TEXT / EMOJI / AT / TOPIC / WEB / BV / CV / OGV / VOTE …）。
/// `VIEW_PICTURE` 的图片进第二个返回值；`GOODS` / `LOTTERY` 丢掉。
pub fn rich_nodes(nodes: Option<&Vec<JsonValue>>) -> (Vec<Span>, Vec<Block>) {
    let mut spans: Vec<Span> = Vec::new();
    let mut images: Vec<Block> = Vec::new();
    for n in nodes.map(|v| v.as_slice()).unwrap_or_default() {
        let Some(o) = n.as_object() else { continue };
        if json::string(o, "type").as_deref() == Some("RICH_TEXT_NODE_TYPE_VIEW_PICTURE") {
            images.extend(pictures(json::arr(o, "pics")));
            continue;
        }
        if let Some(span) = rich_span(o) {
            spans.push(span);
        }
    }
    (spans, images)
}

/// EMOJI 节点的图片：动态表情包的表情（`gif_url` / `webp_url` 非空）是会动的，用它；普通表情只有静态的 `icon_url`。
fn emoji_url(e: Option<&JsonMap>) -> Option<String> {
    e.and_then(|e| {
        json::text(e, "gif_url")
            .or_else(|| json::text(e, "webp_url"))
            .or_else(|| json::text(e, "icon_url"))
    })
    .map(|u| json::to_https(&u))
}

fn pictures(pics: Option<&Vec<JsonValue>>) -> Vec<Block> {
    pics.map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.as_object())
        .filter_map(|o| {
            let url = json::text(o, "src").or_else(|| json::text(o, "url"))?;
            Some(image(
                url,
                json::int(o, "width"),
                json::int(o, "height"),
            ))
        })
        .collect()
}

fn image(url: String, width: Option<i32>, height: Option<i32>) -> Block {
    Block::Image {
        url: json::to_https(&url),
        width: width.filter(|w| *w > 0),
        height: height.filter(|h| *h > 0),
        original: None,
        caption: None,
    }
}

/// 一个 rich 节点 → Span；丢弃的类型返回 `None`。
pub fn rich_span(o: &JsonMap) -> Option<Span> {
    let text = json::string(o, "text")
        .or_else(|| json::string(o, "orig_text"))
        .unwrap_or_default();
    let url = json::text(o, "jump_url")
        .map(|u| json::to_https(&u))
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    Some(match json::string(o, "type").as_deref() {
        Some("RICH_TEXT_NODE_TYPE_TEXT") => {
            if text.is_empty() {
                return None;
            }
            Span::Text {
                text,
                bold: false,
                italic: false,
            }
        }
        Some("RICH_TEXT_NODE_TYPE_EMOJI") => {
            let e = json::obj(o, "emoji");
            let icon = emoji_url(e);
            match icon {
                Some(icon) => Span::Emote {
                    text: e
                        .and_then(|e| json::text(e, "text"))
                        .unwrap_or_else(|| text.clone()),
                    url: icon,
                },
                None => {
                    if text.is_empty() {
                        return None;
                    }
                    Span::Text {
                        text,
                        bold: false,
                        italic: false,
                    }
                }
            }
        }
        Some("RICH_TEXT_NODE_TYPE_AT") => {
            let mid = json::text(o, "rid").filter(|m| m != "0");
            match mid {
                Some(mid) => Span::Mention {
                    text: text.clone(),
                    user: bili_text::mention_user(
                        mid,
                        text.strip_prefix('@').unwrap_or(&text).to_string(),
                    ),
                },
                None => {
                    if text.is_empty() {
                        return None;
                    }
                    Span::Text {
                        text,
                        bold: false,
                        italic: false,
                    }
                }
            }
        }
        Some("RICH_TEXT_NODE_TYPE_GOODS") | Some("RICH_TEXT_NODE_TYPE_LOTTERY") => return None,
        Some("RICH_TEXT_NODE_TYPE_BV") => {
            let bvid = json::text(o, "rid").filter(|r| ids::is_bvid(r));
            let link = url
                .clone()
                .or_else(|| bvid.as_deref().map(super::video_url));
            match (link, text.is_empty()) {
                (Some(link), false) => Span::Link {
                    text,
                    url: link.clone(),
                    target: bvid
                        .map(|b| DetailTarget::Native {
                            id: b,
                            platform: None,
                        })
                        .or_else(|| bili_text::target(&link)),
                },
                _ => {
                    if text.is_empty() {
                        return None;
                    }
                    Span::Text {
                        text,
                        bold: false,
                        italic: false,
                    }
                }
            }
        }
        _ => match (url, text.is_empty()) {
            // 其余带链接的（网址、专栏、番剧、投票、话题……）都做成链接
            (Some(url), false) => Span::Link {
                text,
                target: bili_text::target(&url),
                url,
            },
            (_, false) => Span::Text {
                text,
                bold: false,
                italic: false,
            },
            _ => return None,
        },
    })
}

// ===========================================================================
// opus 段落
// ===========================================================================

pub fn opus_blocks(paragraphs: &Vec<JsonValue>) -> Vec<Block> {
    paragraphs
        .iter()
        .filter_map(|p| p.as_object())
        .flat_map(|p| paragraph(p))
        .collect()
}

const HEADING_LEVELS: [&str; 4] = ["large", "xLarge", "xxLarge", "xxxLarge"];

fn paragraph(p: &JsonMap) -> Vec<Block> {
    match json::int(p, "para_type") {
        Some(1) => {
            let nodes = json::obj(p, "text").and_then(|t| json::arr(t, "nodes"));
            let heading = nodes
                .map(|v| v.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|n| n.as_object())
                .filter_map(|o| json::obj(o, "word"))
                .filter_map(|w| json::string(w, "font_level"))
                .any(|level| HEADING_LEVELS.contains(&level.as_str()));
            let (spans, images) = text_nodes(nodes);
            let paras = bili_text::paragraphs(&spans);
            let mut out: Vec<Block> = if heading {
                paras
                    .into_iter()
                    .map(|b| match b {
                        Block::Paragraph { spans } => Block::Heading { spans },
                        other => other,
                    })
                    .collect()
            } else {
                paras
            };
            out.extend(images);
            out
        }
        Some(4) => bili_text::paragraphs(
            &text_nodes(json::obj(p, "text").and_then(|t| json::arr(t, "nodes"))).0,
        )
        .into_iter()
        .map(|b| match b {
            Block::Paragraph { spans } => Block::Quote { spans },
            other => other,
        })
        .collect(),
        Some(8) => bili_text::paragraphs(
            &text_nodes(json::obj(p, "heading").and_then(|t| json::arr(t, "nodes"))).0,
        )
        .into_iter()
        .map(|b| match b {
            Block::Paragraph { spans } => Block::Heading { spans },
            other => other,
        })
        .collect(),
        Some(2) => {
            // `p.pic.pics`，没有 `pics` 时把 `pic` 自己当成一项
            let pic = json::obj(p, "pic");
            match pic.and_then(|pic| json::arr(pic, "pics")) {
                Some(pics) => pictures(Some(pics)),
                None => pic
                    .and_then(|pic| {
                        let url = json::text(pic, "src").or_else(|| json::text(pic, "url"))?;
                        Some(image(url, json::int(pic, "width"), json::int(pic, "height")))
                    })
                    .into_iter()
                    .collect(),
            }
        }
        Some(3) => vec![Block::Divider],
        Some(5) => list(json::obj(p, "list")),
        Some(6) => json::obj(p, "link_card")
            .and_then(|lc| json::obj(lc, "card"))
            .and_then(link_card)
            .into_iter()
            .collect(),
        Some(7) => match json::obj(p, "code") {
            Some(code) => match json::text(code, "content") {
                Some(content) => vec![Block::Code {
                    text: bili_text::unescape_entities(&content),
                    language: json::text(code, "lang")
                        .map(|l| l.strip_prefix("language-").unwrap_or(&l).to_string()),
                }],
                None => Vec::new(),
            },
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// 段落里的节点：WORD 是带样式的文字，RICH 同信息流的 rich 节点，FORMULA 是 LaTeX（保留源码）。
fn text_nodes(nodes: Option<&Vec<JsonValue>>) -> (Vec<Span>, Vec<Block>) {
    let mut spans: Vec<Span> = Vec::new();
    let mut images: Vec<Block> = Vec::new();
    for n in nodes.map(|v| v.as_slice()).unwrap_or_default() {
        let Some(o) = n.as_object() else { continue };
        match json::string(o, "type").as_deref() {
            Some("TEXT_NODE_TYPE_WORD") => {
                if let Some(w) = json::obj(o, "word") {
                    let words = json::string(w, "words").unwrap_or_default();
                    if !words.is_empty() {
                        let style = json::obj(w, "style");
                        spans.push(Span::Text {
                            text: words,
                            bold: style.and_then(|s| json::bool(s, "bold")) == Some(true),
                            italic: style.and_then(|s| json::bool(s, "italic")) == Some(true),
                        });
                    }
                }
            }
            Some("TEXT_NODE_TYPE_RICH") => {
                if let Some(r) = json::obj(o, "rich") {
                    if json::string(r, "type").as_deref() == Some("RICH_TEXT_NODE_TYPE_VIEW_PICTURE") {
                        images.extend(pictures(json::arr(r, "pics")));
                    } else if let Some(span) = rich_span(r) {
                        spans.push(span);
                    }
                }
            }
            Some("TEXT_NODE_TYPE_FORMULA") => {
                if let Some(latex) = json::obj(o, "formula").and_then(|f| json::text(f, "latex_content")) {
                    spans.push(Span::Text {
                        text: latex,
                        bold: false,
                        italic: false,
                    });
                }
            }
            _ => {}
        }
    }
    (spans, images)
}

/// 列表项变成带 "• " / "1. " 前缀的段落，嵌套层级用两个空格缩进。
fn list(l: Option<&JsonMap>) -> Vec<Block> {
    let Some(l) = l else { return Vec::new() };
    let ordered = json::string(l, "theme").as_deref() == Some("number") || json::int(l, "style") == Some(1);
    let mut out: Vec<Block> = Vec::new();
    for (i, item) in json::arr(l, "items")
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let Some(item) = item.as_object() else { continue };
        let (spans, _) = text_nodes(json::arr(item, "nodes"));
        if spans.is_empty() {
            continue;
        }
        let level = json::int(item, "level").unwrap_or(1).max(1);
        let indent = "  ".repeat((level - 1) as usize);
        let prefix = if ordered {
            format!("{}. ", json::int(item, "order").unwrap_or((i + 1) as i32))
        } else {
            "• ".to_string()
        };
        let mut all = vec![Span::Text {
            text: format!("{indent}{prefix}"),
            bold: false,
            italic: false,
        }];
        all.extend(spans);
        out.push(Block::Paragraph { spans: all });
    }
    out
}

/// 链接卡片：视频（UGC）、通用卡片（COMMON）、直播、图文等变成 [`Block::Card`]；
/// 商品（GOODS）和打不开的（ITEM_NULL）丢掉。字段名照 PiliPlus：
/// 卡片对象在以类型命名的子对象里（`ugc` / `common` / `live` / `opus` ……）。
fn link_card(card: &JsonMap) -> Option<Block> {
    let ty = json::string(card, "type")?;
    if ty.ends_with("GOODS") || ty.ends_with("ITEM_NULL") {
        return None;
    }
    let key = ty.replace("LINK_CARD_TYPE_", "").to_ascii_lowercase();
    let sub = json::obj(card, &key)?;
    let title = json::text(sub, "title")?;
    let url = json::text(sub, "jump_url")
        .map(|u| json::to_https(&u))
        .filter(|u| u.starts_with("http"));
    Some(Block::Card {
        title,
        subtitle: json::text(sub, "desc_second")
            .or_else(|| json::text(sub, "desc"))
            .or_else(|| json::text(sub, "desc1")),
        cover: json::text(sub, "cover").map(|c| json::to_https(&c)),
        url: url.clone(),
        target: url.as_deref().and_then(bili_text::target),
    })
}
