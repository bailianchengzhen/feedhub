//! **`FeedItem` 的 JSON 编解码**（手写，只给小红书关注流的存盘用）。
//!
//! 为什么不给 `crate::model::FeedItem` 加 `serde` derive：那是**全局**改动，12 家平台的模型都跟着变
//! （还有 `arbitrary_precision` 的数字字面量、sealed 枚举判别、`Platform` 的老 enum 名映射……），
//! 而当前只需要小红书关注流的作者缓冲能跟 Kotlin 互相读懂。
//!
//! 口径逐条对照 `core/src/main/kotlin/feedhub/core/Model.kt` + `XhsFollowStore.json`
//! （`Json { ignoreUnknownKeys = true; encodeDefaults = true; explicitNulls = false }`）：
//!
//! | 规则 | 是什么 |
//! |---|---|
//! | 字段顺序 | **按 Kotlin 的声明顺序**写（`platform id kind title summary language author publishedAt publishedAtPrecision media stats reason community communityIcon repost emotes adult url detail`） |
//! | `encodeDefaults = true` | 默认值也写：`media: []`、`stats: {}`、`emotes: {}`、`adult: false`、`publishedAtPrecision: "SECOND"` |
//! | `explicitNulls = false` | **`null` 不写**（`title` / `author` / `url` …… 是 null 就整个字段不出现） |
//! | 枚举 | 名字大写（`"VIDEO"` / `"SECOND"`） |
//! | sealed 类 | `"type"` 判别子在前（`DetailTarget`：`{"type":"native","id":…}`、`{"type":"web","url":…}`；`Media`：`{"type":"video",…}` / `{"type":"image",…}`） |
//! | `Platform` | 老的 11 家写**枚举名**（`"XHS"` / `"BILIBILI"`），新平台写 id（`Platform.toStored`）；读的时候两种都认 |
//! | `emotes` | 是一个 JSON **对象**（不是数组），**保持插入顺序**（Kotlin 那边是 `LinkedHashMap`） |
//! | 字符串转义 | kotlinx 口径（`hosted::push_json_string`：`\u00xx` **小写**、DEL / U+2028 / emoji 原样） |
//!
//! 读的时候宽容（`ignoreUnknownKeys = true` + `coerceInputValues = true` 的等价物）：不认识的字段忽略、
//! 类型不对的字段当没写（`publishedAtPrecision` 不认识就退回 `SECOND`，`kind` 不认识就是 `OTHER`）。

use crate::model::{DetailTarget, FeedItem, ItemKind, Media, PlatformId, Stats, TimePrecision, UserRef};
use crate::xhs::jsonord::{self, J};

// ===========================================================================
// 写
// ===========================================================================

/// 把一条 `FeedItem` 写成 Kotlin 能读回来的 JSON 对象文本。
pub fn encode(item: &FeedItem) -> String {
    let mut out = String::with_capacity(512);
    write_item(&mut out, item);
    out
}

fn write_item(out: &mut String, item: &FeedItem) {
    out.push('{');
    out.push_str("\"platform\":");
    jsonord::quote(out, &platform_to_stored(&item.platform));
    out.push_str(",\"id\":");
    jsonord::quote(out, &item.id);
    out.push_str(",\"kind\":");
    jsonord::quote(out, kind_name(item.kind));
    if let Some(v) = &item.title {
        out.push_str(",\"title\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.summary {
        out.push_str(",\"summary\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.language {
        out.push_str(",\"language\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.author {
        out.push_str(",\"author\":");
        write_user(out, v);
    }
    if let Some(v) = item.published_at {
        out.push_str(",\"publishedAt\":");
        out.push_str(&v.to_string());
    }
    out.push_str(",\"publishedAtPrecision\":");
    jsonord::quote(out, precision_name(item.published_at_precision));
    out.push_str(",\"media\":[");
    for (i, m) in item.media.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_media(out, m);
    }
    out.push_str("],\"stats\":");
    write_stats(out, &item.stats);
    if let Some(v) = &item.reason {
        out.push_str(",\"reason\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.community {
        out.push_str(",\"community\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.community_icon {
        out.push_str(",\"communityIcon\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &item.repost {
        out.push_str(",\"repost\":");
        write_item(out, v);
    }
    out.push_str(",\"emotes\":{");
    for (i, (code, url)) in item.emotes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        jsonord::quote(out, code);
        out.push(':');
        jsonord::quote(out, url);
    }
    out.push_str("},\"adult\":");
    out.push_str(if item.adult { "true" } else { "false" });
    if let Some(v) = &item.url {
        out.push_str(",\"url\":");
        jsonord::quote(out, v);
    }
    out.push_str(",\"detail\":");
    match &item.detail {
        DetailTarget::Web { url } => {
            out.push_str("{\"type\":\"web\",\"url\":");
            jsonord::quote(out, url);
            out.push('}');
        }
        DetailTarget::Native { id, platform } => {
            out.push_str("{\"type\":\"native\",\"id\":");
            jsonord::quote(out, id);
            if let Some(p) = platform {
                out.push_str(",\"platform\":");
                jsonord::quote(out, &platform_to_stored(p));
            }
            out.push('}');
        }
    }
    out.push('}');
}

fn write_user(out: &mut String, user: &UserRef) {
    out.push_str("{\"platform\":");
    jsonord::quote(out, &platform_to_stored(&user.platform));
    out.push_str(",\"id\":");
    jsonord::quote(out, &user.id);
    out.push_str(",\"name\":");
    jsonord::quote(out, &user.name);
    if let Some(v) = &user.avatar {
        out.push_str(",\"avatar\":");
        jsonord::quote(out, v);
    }
    if let Some(v) = &user.url {
        out.push_str(",\"url\":");
        jsonord::quote(out, v);
    }
    out.push('}');
}

fn write_media(out: &mut String, media: &Media) {
    match media {
        Media::Image {
            url,
            width,
            height,
        } => {
            out.push_str("{\"type\":\"image\",\"url\":");
            jsonord::quote(out, url);
            if let Some(w) = width {
                out.push_str(",\"width\":");
                out.push_str(&w.to_string());
            }
            if let Some(h) = height {
                out.push_str(",\"height\":");
                out.push_str(&h.to_string());
            }
            out.push('}');
        }
        Media::Video {
            cover,
            duration_sec,
            width,
            height,
        } => {
            // `type` 判别子在前，后面按 Kotlin 的声明顺序（cover → durationSec → width → height）
            out.push_str("{\"type\":\"video\"");
            if let Some(v) = cover {
                out.push_str(",\"cover\":");
                jsonord::quote(out, v);
            }
            if let Some(v) = duration_sec {
                out.push_str(",\"durationSec\":");
                out.push_str(&v.to_string());
            }
            if let Some(v) = width {
                out.push_str(",\"width\":");
                out.push_str(&v.to_string());
            }
            if let Some(v) = height {
                out.push_str(",\"height\":");
                out.push_str(&v.to_string());
            }
            out.push('}');
        }
    }
}

fn write_stats(out: &mut String, stats: &Stats) {
    out.push('{');
    let mut first = true;
    let mut field = |name: &str, value: Option<i64>| {
        if let Some(v) = value {
            if !first {
                out.push(',');
            }
            first = false;
            out.push('"');
            out.push_str(name);
            out.push_str("\":");
            out.push_str(&v.to_string());
        }
    };
    field("views", stats.views);
    field("likes", stats.likes);
    field("comments", stats.comments);
    field("shares", stats.shares);
    field("danmaku", stats.danmaku);
    field("favorites", stats.favorites);
    field("coins", stats.coins);
    out.push('}');
}

const fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Video => "VIDEO",
        ItemKind::Article => "ARTICLE",
        ItemKind::Answer => "ANSWER",
        ItemKind::Question => "QUESTION",
        ItemKind::Post => "POST",
        ItemKind::Moment => "MOMENT",
        ItemKind::Live => "LIVE",
        ItemKind::Other => "OTHER",
    }
}

const fn precision_name(p: TimePrecision) -> &'static str {
    match p {
        TimePrecision::Second => "SECOND",
        TimePrecision::Minute => "MINUTE",
        TimePrecision::Hour => "HOUR",
        TimePrecision::Day => "DAY",
        TimePrecision::Month => "MONTH",
        TimePrecision::Year => "YEAR",
    }
}

/// `Platform.toStored`：老的 11 家写**枚举名**（旧版本读得回），新的写 id。
///
/// 表只有一份（在 [`crate::hosted::platform_to_stored`]），这里只是转出去（老调用点不用改）。
pub fn platform_to_stored(platform: &str) -> String {
    crate::hosted::platform_to_stored(platform)
}

/// `Platform.fromStored`：老的枚举名换成 id，别的原样当 id。
pub fn platform_from_stored(text: &str) -> PlatformId {
    crate::hosted::platform_from_stored(text)
}

// ===========================================================================
// 读
// ===========================================================================

/// 读一条 `FeedItem`。`platform` / `id` / `detail` 缺一个就是 `None`（Kotlin 那边这三个没有默认值，
/// 缺了会让整个 store 解析失败——这里也一样，调用方当成"缓存坏了"）。
pub fn decode(value: &J) -> Option<FeedItem> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    let platform = platform_from_stored(field("platform")?.as_str()?);
    let id = field("id")?.as_str()?.to_string();
    let kind = match field("kind").and_then(|v| v.as_str()) {
        Some("VIDEO") => ItemKind::Video,
        Some("ARTICLE") => ItemKind::Article,
        Some("ANSWER") => ItemKind::Answer,
        Some("QUESTION") => ItemKind::Question,
        Some("POST") => ItemKind::Post,
        Some("MOMENT") => ItemKind::Moment,
        Some("LIVE") => ItemKind::Live,
        Some("OTHER") => ItemKind::Other,
        // 枚举名不认识 / 字段缺失：Kotlin 那边会让整份 store 解析失败；这里当 OTHER（宽容一点，
        // 反正只有"我们自己写出去、自己读回来"这一条路，格式对不上说明缓存已经坏了，上层会重建）
        _ => ItemKind::Other,
    };
    let precision = match field("publishedAtPrecision").and_then(|v| v.as_str()) {
        Some("MINUTE") => TimePrecision::Minute,
        Some("HOUR") => TimePrecision::Hour,
        Some("DAY") => TimePrecision::Day,
        Some("MONTH") => TimePrecision::Month,
        Some("YEAR") => TimePrecision::Year,
        // `SECOND` 是默认值：字段缺失、显式 null、不认识的值都退回它
        _ => TimePrecision::Second,
    };
    let media = field("media")
        .and_then(|v| v.as_arr())
        .map(|list| list.iter().filter_map(decode_media).collect())
        .unwrap_or_default();
    let stats = field("stats").map(decode_stats).unwrap_or_default();
    let emotes = field("emotes")
        .and_then(|v| v.as_obj())
        .map(|pairs| {
            pairs
                .iter()
                .map(|(code, url)| (code.clone(), url.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let detail = decode_detail(field("detail")?)?;
    Some(FeedItem {
        platform,
        id,
        kind,
        title: text(field("title")),
        summary: text(field("summary")),
        language: text(field("language")),
        author: field("author").and_then(decode_user),
        published_at: field("publishedAt").and_then(|v| v.as_i64()),
        published_at_precision: precision,
        media,
        stats,
        reason: text(field("reason")),
        community: text(field("community")),
        community_icon: text(field("communityIcon")),
        repost: field("repost").and_then(decode).map(Box::new),
        emotes,
        adult: field("adult").and_then(|v| v.as_bool()).unwrap_or(false),
        url: text(field("url")),
        detail,
    })
}

/// `JsonObject.text(key)` 的口径：字符串且**去掉空白之后非空**；null / 不是字符串 → `None`。
fn text(value: Option<&J>) -> Option<String> {
    let s = value?.as_str()?;
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn decode_user(value: &J) -> Option<UserRef> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    Some(UserRef {
        platform: platform_from_stored(field("platform")?.as_str()?),
        id: field("id")?.as_str()?.to_string(),
        name: field("name")?.as_str()?.to_string(),
        avatar: field("avatar").and_then(|v| v.as_str()).map(|s| s.to_string()),
        url: field("url").and_then(|v| v.as_str()).map(|s| s.to_string()),
    })
}

fn decode_media(value: &J) -> Option<Media> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    match field("type")?.as_str()? {
        "image" => Some(Media::Image {
            url: field("url")?.as_str()?.to_string(),
            width: field("width").and_then(|v| v.as_i64()).map(|n| n as i32),
            height: field("height").and_then(|v| v.as_i64()).map(|n| n as i32),
        }),
        "video" => Some(Media::Video {
            cover: field("cover").and_then(|v| v.as_str()).map(|s| s.to_string()),
            duration_sec: field("durationSec").and_then(|v| v.as_i64()).map(|n| n as i32),
            width: field("width").and_then(|v| v.as_i64()).map(|n| n as i32),
            height: field("height").and_then(|v| v.as_i64()).map(|n| n as i32),
        }),
        // 不认识的媒体类型：跳过这一项（Kotlin 那边 sealed 类会解析失败；这里宽容）
        _ => None,
    }
}

fn decode_stats(value: &J) -> Stats {
    let Some(obj) = value.as_obj() else {
        return Stats::default();
    };
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_i64());
    Stats {
        views: field("views"),
        likes: field("likes"),
        comments: field("comments"),
        shares: field("shares"),
        danmaku: field("danmaku"),
        favorites: field("favorites"),
        coins: field("coins"),
    }
}

fn decode_detail(value: &J) -> Option<DetailTarget> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    match field("type")?.as_str()? {
        "web" => Some(DetailTarget::Web {
            url: field("url")?.as_str()?.to_string(),
        }),
        "native" => Some(DetailTarget::Native {
            id: field("id")?.as_str()?.to_string(),
            platform: field("platform")
                .and_then(|v| v.as_str())
                .map(platform_from_stored),
        }),
        _ => None,
    }
}

