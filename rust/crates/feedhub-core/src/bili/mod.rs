//! B站：推荐流 + 关注流（动态）的解析，以及 WBI 签名 / AV-BV 换算。
//!
//! 逐条对照：
//! - 推荐流：`source-bilibili/BiliParser.recommend` + `BiliVideoParser.videoCard`
//! - 关注流：`BiliParser.dynamics` / `BiliParser.dynamic`（动态的 major 类型都搬了）
//! - 签名：`WbiSigner.kt`；id：`BiliIds.kt` / `BvAv.kt`
//!
//! 详情 / 评论（一级评论 + 楼中楼）在 `detail` / `comment`；播放地址和分段弹幕在
//! `play` / `danmaku`。扫码 / 搜索 / 关注管理继续用 Kotlin。

pub mod account;
pub mod comment;
pub mod danmaku;
pub mod detail;
pub mod entities;
pub mod ids;
// 播放地址（`/x/player/wbi/playurl` → 通用 `play::PlayInfo`）和分段弹幕（`seg.so` 的 protobuf）。
pub mod play;

pub mod qr;
pub mod search;
pub mod sign;
pub mod source;
pub mod text;

use crate::json::{self, JsonMap};
use crate::model::{
    BiliError, DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef, BILI,
};
use serde_json::Value;

/// 有原生详情的动态类型（新版图文 / 文字也是 DRAW，专栏是 ARTICLE）。
const NATIVE_DYNAMIC_TYPES: [&str; 4] = [
    "DYNAMIC_TYPE_DRAW",
    "DYNAMIC_TYPE_WORD",
    "DYNAMIC_TYPE_FORWARD",
    "DYNAMIC_TYPE_ARTICLE",
];

pub fn video_url(bvid: &str) -> String {
    format!("https://www.bilibili.com/video/{bvid}")
}

pub fn dynamic_url(id: &str) -> String {
    format!("https://t.bilibili.com/{id}")
}

pub fn space_url(mid: &str) -> String {
    format!("https://space.bilibili.com/{mid}")
}

/// 检查 B站统一的 `{code, message, data}` 外壳，非 0 返回对应错误（和 `BilibiliSource.check()` 一一对应）。
pub fn check_envelope(body: &str) -> Result<JsonMap, BiliError> {
    let root = json::parse_object(body).map_err(|detail| BiliError::Parse { detail })?;
    check_envelope_object(&root)?;
    Ok(root)
}

/// 同 [`check_envelope`]，只是入参已经是解析好的对象（平台层从宿主拿到 body 之后只解析一遍）。
pub fn check_envelope_object(root: &JsonMap) -> Result<(), BiliError> {
    let code = json::long(root, "code").ok_or_else(|| BiliError::Parse {
        detail: "响应缺少 code".to_string(),
    })?;
    let msg = json::text(&root, "message")
        .or_else(|| json::text(&root, "msg"))
        .unwrap_or_default();
    match code {
        0 => Ok(()),
        // -101 未登录；-111 csrf 校验失败（bili_jct 过期）
        -101 => Err(BiliError::NotLoggedIn { detail: None }),
        -111 => Err(BiliError::NotLoggedIn {
            detail: Some("csrf 校验失败，登录可能已过期".to_string()),
        }),
        // -352 风控校验失败；-412 请求被拦截；-509 / -799 请求过于频繁；22015 账号异常
        -352 | -412 | -509 | -799 | 22015 => Err(BiliError::RiskControl {
            code: Some(code.to_string()),
            detail: msg,
        }),
        // -403 签名被拒
        -403 => Err(BiliError::Signature {
            detail: format!("{code} {msg}"),
        }),
        _ => Err(BiliError::Api {
            code: code.to_string(),
            detail: msg,
        }),
    }
}

/// 推荐流：`/x/web-interface/wbi/index/top/feed/rcmd`。
///
/// [`fresh_idx`] 是这一页的序号；推荐流没有尽头，下一页永远是 `fresh_idx + 1`。
pub fn parse_recommend(body: &str, fresh_idx: i64) -> Result<Page, BiliError> {
    let root = check_envelope(body)?;
    let items = json::path(&root, &["data"])
        .and_then(|d| json::arr(d, "item"))
        .ok_or_else(|| BiliError::Parse {
            detail: "推荐流缺少 data.item".to_string(),
        })?;
    let items = items
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(recommend_item)
        .collect();
    Ok(Page {
        items,
        next: Some((fresh_idx + 1).to_string()),
    })
}

/// 只要普通视频（`goto == "av"`）；直播、广告、番剧卡片跳过。
fn recommend_item(o: &JsonMap) -> Option<FeedItem> {
    if json::string(o, "goto").as_deref() != Some("av") {
        return None;
    }
    video_card(o)
}

/// 一张视频卡片 → [`FeedItem`]。推荐流的 `item`、详情的 `View`、`Related` 字段基本一致，共用这一个函数。
/// 番剧（`is_ogv`）没有原生详情，返回 `None`。
pub fn video_card(o: &JsonMap) -> Option<FeedItem> {
    if json::bool(o, "is_ogv") == Some(true) {
        return None;
    }
    let bvid = json::text(o, "bvid")?;
    let stat = json::obj(o, "stat");
    let author = json::obj(o, "owner").and_then(|owner| {
        user(
            json::string(owner, "mid"),
            json::text(owner, "name"),
            json::text(owner, "face"),
        )
    });
    Some(FeedItem {
        platform: BILI.to_string(),
        id: bvid.clone(),
        kind: ItemKind::Video,
        title: json::text(o, "title"),
        summary: None,
        language: None,
        author,
        published_at: json::long(o, "pubdate").filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        media: vec![Media::Video {
            cover: json::text(o, "pic").map(|p| json::to_https(&p)),
            duration_sec: json::int(o, "duration"),
            width: None,
            height: None,
        }],
        stats: Stats {
            views: stat.and_then(|s| json::long(s, "view")),
            likes: stat.and_then(|s| json::long(s, "like")),
            comments: stat.and_then(|s| json::long(s, "reply")),
            shares: stat.and_then(|s| json::long(s, "share")),
            danmaku: stat.and_then(|s| json::long(s, "danmaku")),
            favorites: stat.and_then(|s| json::long(s, "favorite")),
            coins: stat.and_then(|s| json::long(s, "coin")),
        },
        // `rcmd_reason` 有时候是对象（`content` 在里面），有时候直接是字符串
        reason: json::obj(o, "rcmd_reason")
            .and_then(|r| json::text(r, "content"))
            .or_else(|| json::text(o, "rcmd_reason")),
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        url: Some(video_url(&bvid)),
        detail: DetailTarget::Native {
            id: bvid,
            platform: None,
        },
    })
}

/// 关注流：`/x/polymer/web-dynamic/v1/feed/all` 和 `feed/space`。
pub fn parse_dynamics(body: &str) -> Result<Page, BiliError> {
    // 外层 `{code}` 外壳也要查：关注流要登录，-101 / -352 都可能出现。
    let root = check_envelope(body)?;
    let data = json::obj(&root, "data").ok_or_else(|| BiliError::Parse {
        detail: "动态流缺少 data".to_string(),
    })?;
    let items = json::arr(data, "items").ok_or_else(|| BiliError::Parse {
        detail: "动态流缺少 data.items".to_string(),
    })?;
    let items: Vec<FeedItem> = items
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(dynamic)
        .collect();
    let offset = json::text(data, "offset");
    let has_more = json::bool(data, "has_more") == Some(true);
    Ok(Page {
        items,
        next: if has_more {
            offset
        } else {
            None
        },
    })
}

/// 单条动态（`data.item` 那种结构）。测试里用来逐条喂 fixture；`dyn_detail_*` 的 `data.item` 也能直接进来。
pub fn parse_dynamic_item(body: &str) -> Result<Option<FeedItem>, BiliError> {
    let root = check_envelope(body)?;
    Ok(json::path(&root, &["data", "item"]).and_then(dynamic))
}

/// 单条动态对象的 JSON（不带 `{code}` 外壳）。
pub fn parse_dynamic_object_json(body: &str) -> Result<Option<FeedItem>, BiliError> {
    let m = json::parse_object(body).map_err(|detail| BiliError::Parse { detail })?;
    Ok(dynamic(&m))
}

/// 一条动态。字段结构见 bilby `data/DynamicCardMapper.kt`。
/// 视频类动态的 id 用 bvid，这样和推荐流里同一个视频的 `FeedItem.key` 一致、能去重。
pub fn dynamic(o: &JsonMap) -> Option<FeedItem> {
    let id_str = json::text(o, "id_str");
    let ty = json::string(o, "type");
    let modules = json::obj(o, "modules")?;
    let author = json::obj(modules, "module_author");
    let dynm = json::obj(modules, "module_dynamic");
    let stat = json::obj(modules, "module_stat");
    let major = dynm.and_then(|d| json::obj(d, "major"));
    let desc = dynm
        .and_then(|d| json::obj(d, "desc"))
        .and_then(|d| json::text(d, "text"));

    let Some(id_str) = id_str else {
        return deleted_orig(major);
    };
    let native = ty
        .as_deref()
        .map(|t| NATIVE_DYNAMIC_TYPES.contains(&t))
        .unwrap_or(false);
    let url = dynamic_url(&id_str);

    let base = FeedItem {
        platform: BILI.to_string(),
        id: id_str.clone(),
        kind: ItemKind::Moment,
        title: None,
        summary: desc.clone(),
        language: None,
        author: author.and_then(|a| {
            user(
                json::string(a, "mid"),
                json::text(a, "name"),
                json::text(a, "face"),
            )
        }),
        published_at: author
            .and_then(|a| json::long(a, "pub_ts"))
            .filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        media: Vec::new(),
        stats: Stats {
            likes: stat
                .and_then(|s| json::obj(s, "like"))
                .and_then(|l| json::long(l, "count")),
            comments: stat
                .and_then(|s| json::obj(s, "comment"))
                .and_then(|c| json::long(c, "count")),
            shares: stat
                .and_then(|s| json::obj(s, "forward"))
                .and_then(|f| json::long(f, "count")),
            ..Stats::default()
        },
        reason: author.and_then(|a| json::text(a, "pub_action")),
        community: dynm
            .and_then(|d| json::obj(d, "topic"))
            .and_then(|t| json::text(t, "name")),
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        url: Some(url.clone()),
        detail: if native {
            DetailTarget::Native {
                id: ids::dynamic_native_id(&id_str),
                platform: None,
            }
        } else {
            DetailTarget::Web { url }
        },
    };

    // major：视频 / 合集 / 图文 / 图片 / 专栏 / 番剧 / 直播 / 通用卡片
    let mut item = base.clone();
    if let Some(m) = major {
        let handled: Option<FeedItem> = match json::string(m, "type").as_deref() {
            Some("MAJOR_TYPE_ARCHIVE") => json::obj(m, "archive").map(|a| archive(&base, a)),
            Some("MAJOR_TYPE_UGC_SEASON") => json::obj(m, "ugc_season").map(|a| archive(&base, a)),
            Some("MAJOR_TYPE_OPUS") => json::obj(m, "opus").map(|op| {
                opus(&base, op, ty.as_deref() == Some("DYNAMIC_TYPE_ARTICLE"))
            }),
            Some("MAJOR_TYPE_DRAW") => json::obj(m, "draw").map(|draw| {
                with(&base, |b| {
                    b.media = json::arr(draw, "items")
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_object())
                                .filter_map(draw_image)
                                .collect()
                        })
                        .unwrap_or_default();
                })
            }),
            Some("MAJOR_TYPE_ARTICLE") => json::obj(m, "article").map(|a| {
                let url = json::text(a, "jump_url")
                    .map(|u| json::to_https(&u))
                    .or_else(|| base.url.clone())
                    .unwrap_or_default();
                with(&base, |b| {
                    b.kind = ItemKind::Article;
                    b.title = json::text(a, "title");
                    b.summary = json::text(a, "desc").or_else(|| b.summary.clone());
                    b.media = json::arr(a, "covers")
                        .map(|covers| {
                            covers
                                .iter()
                                .filter_map(json::as_string)
                                .map(|u| Media::Image {
                                    url: json::to_https(&u),
                                    width: None,
                                    height: None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    b.url = Some(url);
                })
            }),
            Some("MAJOR_TYPE_PGC") => json::obj(m, "pgc").map(|pgc| {
                let url = json::text(pgc, "jump_url")
                    .map(|u| json::to_https(&u))
                    .or_else(|| base.url.clone())
                    .unwrap_or_default();
                let s = json::obj(pgc, "stat");
                with(&base, |b| {
                    b.kind = ItemKind::Video;
                    b.title = json::text(pgc, "title");
                    b.media = vec![Media::Video {
                        cover: json::text(pgc, "cover").map(|c| json::to_https(&c)),
                        duration_sec: None,
                        width: None,
                        height: None,
                    }];
                    b.stats.views = json::parse_count(s.and_then(|s| json::string(s, "play")).as_deref());
                    b.stats.danmaku =
                        json::parse_count(s.and_then(|s| json::string(s, "danmaku")).as_deref());
                    b.url = Some(url.clone());
                    b.detail = DetailTarget::Web { url };
                })
            }),
            Some("MAJOR_TYPE_LIVE_RCMD") => json::obj(m, "live_rcmd").map(|live| {
                // content 是一段 JSON 字符串
                let info = json::text(live, "content")
                    .and_then(|s| json::parse_object(&s).ok())
                    .and_then(|m| json::obj(&m, "live_play_info").cloned());
                match info {
                    Some(info) => live_item(
                        &base,
                        json::text(&info, "title"),
                        json::text(&info, "cover"),
                        json::text(&info, "link"),
                    ),
                    None => live_item(&base, None, None, None),
                }
            }),
            Some("MAJOR_TYPE_LIVE") => json::obj(m, "live").map(|live| {
                live_item(
                    &base,
                    json::text(live, "title"),
                    json::text(live, "cover"),
                    json::text(live, "jump_url"),
                )
            }),
            Some("MAJOR_TYPE_COMMON") => json::obj(m, "common").map(|c| {
                let joined = [desc.clone(), json::text(c, "desc")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                let summary = if joined.is_empty() {
                    None
                } else {
                    Some(joined)
                };
                with(&base, |b| {
                    b.title = json::text(c, "title");
                    b.summary = summary;
                    b.media = json::text(c, "cover")
                        .map(|cover| {
                            vec![Media::Image {
                                url: json::to_https(&cover),
                                width: None,
                                height: None,
                            }]
                        })
                        .unwrap_or_default();
                })
            }),
            _ => None,
        };
        if let Some(handled) = handled {
            item = handled;
        }
    }

    let repost = if ty.as_deref() == Some("DYNAMIC_TYPE_FORWARD") {
        json::obj(o, "orig").and_then(dynamic).map(Box::new)
    } else {
        None
    };

    // 摘要是纯文本，里面的 [打call] 对应的图片在 rich_text_nodes 里（动态文字在 desc，图文在 opus.summary）；
    // 转发的原动态有自己的 emotes
    let mut table: Vec<(String, String)> = Vec::new();
    if let Some(desc_node) = dynm.and_then(|d| json::obj(d, "desc")) {
        merge_emotes(&mut table, emote_table(json::arr(desc_node, "rich_text_nodes")));
    }
    if let Some(summary) = major
        .and_then(|m| json::obj(m, "opus"))
        .and_then(|o| json::obj(o, "summary"))
    {
        merge_emotes(&mut table, emote_table(json::arr(summary, "rich_text_nodes")));
    }
    let emotes = emotes_used(&table, &[item.title.as_deref(), item.summary.as_deref()]);

    item.repost = repost;
    item.emotes = emotes;
    Some(item)
}

/// 被转发的原动态被删了：没有 id_str，major 是 MAJOR_TYPE_NONE。
fn deleted_orig(major: Option<&JsonMap>) -> Option<FeedItem> {
    let tips = major
        .and_then(|m| json::obj(m, "none"))
        .and_then(|n| json::text(n, "tips"))?;
    Some(FeedItem {
        platform: BILI.to_string(),
        id: "deleted".to_string(),
        kind: ItemKind::Other,
        title: None,
        summary: Some(tips),
        language: None,
        author: None,
        published_at: None,
        published_at_precision: TimePrecision::Second,
        media: Vec::new(),
        stats: Stats::default(),
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        url: None,
        detail: DetailTarget::Web {
            url: "https://t.bilibili.com/".to_string(),
        },
    })
}

fn archive(base: &FeedItem, a: &JsonMap) -> FeedItem {
    let Some(bvid) = json::text(a, "bvid") else {
        return with(base, |b| {
            b.kind = ItemKind::Video;
            b.title = json::text(a, "title");
        });
    };
    let stat = json::obj(a, "stat");
    let url = video_url(&bvid);
    with(base, |b| {
        b.id = bvid.clone();
        b.kind = ItemKind::Video;
        b.title = json::text(a, "title");
        // 视频简介比"投稿了视频"那句动态文字有用；动态文字为空时才用它
        if b.summary.is_none() {
            b.summary = json::text(a, "desc").filter(|d| d != "-");
        }
        b.media = vec![Media::Video {
            cover: json::text(a, "cover").map(|c| json::to_https(&c)),
            duration_sec: json::parse_duration(json::text(a, "duration_text").as_deref()),
            width: None,
            height: None,
        }];
        b.stats.views = json::parse_count(stat.and_then(|s| json::string(s, "play")).as_deref());
        b.stats.danmaku =
            json::parse_count(stat.and_then(|s| json::string(s, "danmaku")).as_deref());
        b.url = Some(url.clone());
        b.detail = DetailTarget::Native {
            id: bvid.clone(),
            platform: None,
        };
    })
}

fn opus(base: &FeedItem, op: &JsonMap, is_article: bool) -> FeedItem {
    let url = json::text(op, "jump_url")
        .map(|u| json::to_https(&u))
        .or_else(|| base.url.clone())
        .unwrap_or_default();
    with(base, |b| {
        b.kind = if is_article {
            ItemKind::Article
        } else {
            ItemKind::Moment
        };
        b.title = json::text(op, "title");
        b.summary = json::obj(op, "summary")
            .and_then(|s| json::text(s, "text"))
            .or_else(|| b.summary.clone());
        b.media = json::arr(op, "pics")
            .map(|pics| {
                pics.iter()
                    .filter_map(|v| v.as_object())
                    .filter_map(opus_image)
                    .collect()
            })
            .unwrap_or_default();
        b.url = Some(url);
    })
}

fn live_item(
    base: &FeedItem,
    title: Option<String>,
    cover: Option<String>,
    link: Option<String>,
) -> FeedItem {
    let url = link
        .map(|u| json::to_https(&u))
        .or_else(|| base.url.clone())
        .unwrap_or_default();
    with(base, |b| {
        b.kind = ItemKind::Live;
        b.title = title;
        b.media = cover
            .map(|c| {
                vec![Media::Image {
                    url: json::to_https(&c),
                    width: None,
                    height: None,
                }]
            })
            .unwrap_or_default();
        b.url = Some(url.clone());
        b.detail = DetailTarget::Web { url };
    })
}

fn opus_image(p: &JsonMap) -> Option<Media> {
    json::text(p, "url").map(|url| Media::Image {
        url: json::to_https(&url),
        width: json::int(p, "width"),
        height: json::int(p, "height"),
    })
}

fn draw_image(p: &JsonMap) -> Option<Media> {
    json::text(p, "src").map(|url| Media::Image {
        url: json::to_https(&url),
        width: json::int(p, "width"),
        height: json::int(p, "height"),
    })
}

/// 动态表情表：`text`（文字代码，如 `[打call]`）→ 图片地址。
/// 动态表情包的表情（`gif_url` / `webp_url` 非空）是会动的，用它；普通表情只有静态的 `icon_url`。
fn emote_table(nodes: Option<&Vec<Value>>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for n in nodes.map(|v| v.as_slice()).unwrap_or_default() {
        let Some(o) = n.as_object() else { continue };
        if json::string(o, "type").as_deref() != Some("RICH_TEXT_NODE_TYPE_EMOJI") {
            continue;
        }
        let emoji = json::obj(o, "emoji");
        let Some(code) = emoji
            .and_then(|e| json::text(e, "text"))
            .or_else(|| json::text(o, "text"))
        else {
            continue;
        };
        if let Some(url) = emoji_url(emoji) {
            if !out.iter().any(|(c, _)| *c == code) {
                out.push((code, url));
            }
        }
    }
    out
}

fn emoji_url(e: Option<&JsonMap>) -> Option<String> {
    e.and_then(|e| {
        json::text(e, "gif_url")
            .or_else(|| json::text(e, "webp_url"))
            .or_else(|| json::text(e, "icon_url"))
    })
    .map(|u| json::to_https(&u))
}

/// Kotlin 的 `map + map`：后面那张表里重复的键改值但保持第一次出现的位置，新键追加。
fn merge_emotes(table: &mut Vec<(String, String)>, other: Vec<(String, String)>) {
    for (code, url) in other {
        let mut found = false;
        for slot in table.iter_mut() {
            if slot.0 == code {
                slot.1 = url.clone();
                found = true;
                break;
            }
        }
        if !found {
            table.push((code, url));
        }
    }
}

/// `Emotes.used`：挑出标题 / 摘要里**真的出现过**的表情代码，保持表里的顺序。
fn emotes_used(table: &[(String, String)], texts: &[Option<&str>]) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let present: Vec<&str> = texts
        .iter()
        .flatten()
        .copied()
        .filter(|t| !t.is_empty())
        .collect();
    if present.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (code, url) in table {
        if code.is_empty() || url.is_empty() {
            continue;
        }
        if present.iter().any(|t| t.contains(code.as_str())) {
            out.push((code.clone(), url.clone()));
        }
    }
    out
}

fn user(mid: Option<String>, name: Option<String>, face: Option<String>) -> Option<UserRef> {
    let mid = mid?;
    if mid.is_empty() || mid == "0" {
        return None;
    }
    Some(UserRef {
        platform: BILI.to_string(),
        id: mid.clone(),
        name: name.unwrap_or_else(|| mid.clone()),
        avatar: face.map(|f| json::to_https(&f)),
        url: Some(space_url(&mid)),
    })
}

/// `base.copy { … }` 的 Rust 写法：克隆一份再改几个字段。
fn with(base: &FeedItem, f: impl FnOnce(&mut FeedItem)) -> FeedItem {
    let mut copy = base.clone();
    f(&mut copy);
    copy
}

/// 从 nav 的响应里取 WBI 的两个 key（img / sub）。未登录时 `code=-101` 也照样有 `wbi_img`，
/// 所以这里**不检查**外壳（和 Kotlin `BiliParser.wbiKeys` 一样）。
pub fn wbi_keys_from_nav(body: &str) -> Option<(String, String)> {
    wbi_keys_from_nav_object(&json::parse_object(body).ok()?)
}

/// 同 [`wbi_keys_from_nav`]，入参已经是解析好的 nav 对象。
pub fn wbi_keys_from_nav_object(root: &JsonMap) -> Option<(String, String)> {
    let img = json::path(root, &["data", "wbi_img"])?;
    let img_url = json::text(img, "img_url")?;
    let sub_url = json::text(img, "sub_url")?;
    Some((
        sign::key_from_url(&img_url),
        sign::key_from_url(&sub_url),
    ))
}
