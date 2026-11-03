//! TikTok（网页版）的解析与签名，加上**平台层**（请求怎么发，见 [`source`]）。
//!
//! 对应 Kotlin 的 `source-tiktok` 三个文件：
//! - [`sign`]：`X-Dynosaur` / `X-Gnarly`（`TiktokSign.kt`，那一份是 f2 的逐行移植）；
//! - 本文件：推荐流的解析（`TiktokParser.kt` 里信息流用到的那部分）；
//! - [`source`]：`TiktokSource.kt` 的 `recommend` / `ensureDevice` / 请求头 / 错误映射
//!   （整条"请求怎么发"都在那里）。
//!
//! 详情是视频页 HTML 的 hydration（不是接口），评论 / 楼中楼走 `comment/list*` 并带 `X-Dynosaur` 签名。
//! 播放 / 用户信息 / `raw` 还在 Kotlin（`RustTiktokSource` 逐方法委托）。TikTok **没有关注流**
//!
//! ## 实测过的几个坑（解析这一侧）
//!
//! - `video.duration` 是**秒**（不是毫秒）；
//! - `video.cover` / `playAddr` 是**字符串**（有些版本是 `{UrlList:[…]}`，两种都认）；
//! - 推荐流的响应 `hasMore: true` 但**没有 `cursor`**（浏览器就是反复 GET，每回一批新的），
//!   所以翻页游标是调用方给的**占位值**（`TiktokSource` 用当前毫秒时间戳）；
//! - `itemStruct` 里**没有** 18+ 字段。

pub mod detail;
// 播放地址（`TiktokVideo.kt` 的移植）：详情里的 `FeedDetail.video` 也走它。
pub mod play;
pub mod sign;
pub mod source;

use serde_json::Value;

use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

/// TikTok 的平台 id（和 Kotlin `TIKTOK` 同值，定了就别改）。
pub const PLATFORM: &str = "tiktok";

/// Rust 侧的业务错误。和 `BiliError` / `CoolapkError` 同一套形状（FFI 再拆成带类型的 enum）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TiktokError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：`tt_orcas_res`、403 / 429、空 body、JS 挑战页、业务码 10000 / 10001 / 2154。
    /// **调用方不许自动重试**（风控红线）。
    #[error("风控：{detail}")]
    RiskControl { code: Option<String>, detail: String },

    #[error("签名被拒：{detail}")]
    Signature { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 宿主执行请求时失败了（连不上 / 超时 / 被重置）。上层映射回 `java.io.IOException`。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外 / Rust 内部错误（别变成一次崩溃）。
    #[error("内部错误：{detail}")]
    Internal { detail: String },
}

impl From<crate::host::HostError> for TiktokError {
    fn from(e: crate::host::HostError) -> Self {
        match e {
            crate::host::HostError::Network { detail } => TiktokError::Network { detail },
            crate::host::HostError::ProxyUnavailable { detail } => {
                TiktokError::ProxyUnavailable { detail }
            }
            crate::host::HostError::Other { detail } => TiktokError::Internal { detail },
        }
    }
}

// ---------- id 形态 ----------

/// 详情 id：`<uniqueId>/<itemId>`（详情页地址是 `/@<作者>/video/<id>`）。
pub fn detail_id(unique_id: Option<&str>, item_id: &str) -> String {
    match unique_id {
        Some(u) if !u.trim().is_empty() => format!("{u}/{item_id}"),
        _ => item_id.to_string(),
    }
}

/// 详情 id → 裸 itemId（最后一段）。
pub fn item_id_of(id: &str) -> String {
    match id.rsplit_once('/') {
        Some((_, last)) if !last.is_empty() => last.to_string(),
        _ => id.to_string(),
    }
}

/// 详情 id → 作者 uniqueId（没有就 `None`）。
pub fn unique_id_of(id: &str) -> Option<String> {
    match id.rsplit_once('/') {
        Some((head, _)) if !head.trim().is_empty() && head != id => Some(head.to_string()),
        _ => None,
    }
}

/// 没有识别出登录者时给用户看的地址形态（TikTok 网页的分享地址）。
pub fn video_url(id: &str) -> String {
    let unique_id = unique_id_of(id);
    let item_id = item_id_of(id);
    match unique_id {
        Some(u) => format!("https://www.tiktok.com/@{u}/video/{item_id}"),
        None => format!("https://www.tiktok.com/video/{item_id}"),
    }
}

pub fn user_url(unique_id: &str) -> String {
    format!("https://www.tiktok.com/@{unique_id}")
}

/// 首页 hydration 的 `__DEFAULT_SCOPE__` → 当前登录的账号。
///
/// TikTok 不同版本的首页把账号挂在几个不同的 scope 里（`webapp.user-detail.userInfo.user`、
/// `webapp.app-context.user`、`webapp.app-context.userInfo.user`、`webapp.user-detail.user`），
/// 按"最可能"的顺序找一遍，**找不到就 `None`**（宁可显示"登录已失效"，也不编一个账号）。
///
/// 昵称 / 头像的键名两边不一致：首页 `app-context` 那份是 **`nickName`** / `avatarUri`
/// ，大驼峰那份是 `nickname` /
/// `avatarThumb` / `avatarLarger`。
pub fn self_user(scope: &JsonMap) -> Option<UserRef> {
    let user_detail = || json::obj(scope, "webapp.user-detail");
    let app_context = || json::obj(scope, "webapp.app-context");
    let candidates = [
        user_detail()
            .and_then(|o| json::obj(o, "userInfo"))
            .and_then(|o| json::obj(o, "user")),
        app_context().and_then(|o| json::obj(o, "user")),
        app_context()
            .and_then(|o| json::obj(o, "userInfo"))
            .and_then(|o| json::obj(o, "user")),
        user_detail().and_then(|o| json::obj(o, "user")),
    ];
    candidates.into_iter().flatten().find_map(self_user_of)
}

fn self_user_of(user: &JsonMap) -> Option<UserRef> {
    let unique_id = json::text(user, "uniqueId").or_else(|| json::text(user, "unique_id"));
    let uid = json::text(user, "id").or_else(|| json::text(user, "uid"));
    let id = unique_id.clone().or(uid)?;
    let name = json::text(user, "nickname")
        .or_else(|| json::text(user, "nickName"))
        .or_else(|| unique_id.clone())
        .unwrap_or_else(|| id.clone());
    let avatar = json::obj(user, "avatarThumb")
        .or_else(|| json::obj(user, "avatarLarger"))
        .or_else(|| json::obj(user, "avatarUri"))
        .and_then(url_list)
        .map(|u| json::to_https(&u));
    Some(UserRef {
        platform: PLATFORM.to_string(),
        id,
        name,
        avatar,
        url: unique_id.map(|u| user_url(&u)),
    })
}

// ---------- 一页推荐流 ----------

/// 一页 `item_list`。[`fallback_cursor`] 在没有 `cursor` 但 `hasMore` 时当"还有下一页"的占位
/// （Kotlin `TiktokParser.feed` 的 `fallbackCursor`）。
pub fn parse_feed(body: &str, fallback_cursor: Option<&str>) -> Result<Page, TiktokError> {
    let json = json::parse_object(body).map_err(|detail| TiktokError::Parse { detail })?;
    parse_feed_object(&json, fallback_cursor)
}

/// 已经解析成对象的那一份（平台层只解析一遍）。
pub fn parse_feed_object(
    json: &JsonMap,
    fallback_cursor: Option<&str>,
) -> Result<Page, TiktokError> {
    check_status(json)?;
    let list = json::arr(json, "itemList")
        .or_else(|| json::arr(json, "item_list"))
        .ok_or_else(|| TiktokError::Parse {
            detail: "推荐流缺少 itemList".to_string(),
        })?;
    let items = list
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| item(o))
        .collect();
    let has_more = json::bool(json, "hasMore").or_else(|| json::bool(json, "has_more")) == Some(true);
    // 所以没有时就退回调用方给的占位值。
    let cursor = json::long(json, "cursor")
        .map(|n| n.to_string())
        .or_else(|| if has_more { fallback_cursor.map(|s| s.to_string()) } else { None });
    Ok(Page { items, next: cursor })
}

/// 一条 itemStruct → [`FeedItem`]。没有 id 的丢掉（广告 / 非内容卡片返回 `None`）。
pub fn item(o: &JsonMap) -> Option<FeedItem> {
    // 广告不是内容：丢
    if json::bool(o, "isAd") == Some(true) || json::bool(o, "is_ad") == Some(true) {
        return None;
    }
    let id = json::text(o, "id")?;
    let author = author(o.get("author").and_then(|v| v.as_object()));
    let desc = json::text(o, "desc");
    let images = image_post_images(o);
    let video = if images.is_empty() {
        json::obj(o, "video").and_then(video_media)
    } else {
        None
    };
    let kind = if video.is_some() {
        ItemKind::Video
    } else if !images.is_empty() {
        ItemKind::Post
    } else {
        ItemKind::Other
    };
    let stats = json::obj(o, "stats").or_else(|| json::obj(o, "statsV2"));
    let detail = detail_id(author.as_ref().map(|a| a.id.as_str()), &id);
    let mut media = Vec::new();
    media.extend(video);
    media.extend(images);
    Some(FeedItem {
        platform: PLATFORM.to_string(),
        id,
        kind,
        // TikTok 没有标题字段，`desc` 是文案：放 summary，title 留 None（不编字段）
        title: None,
        summary: desc,
        language: None,
        author,
        // createTime 是秒（f2 的 filter 直接 timestamp_2_str）
        published_at: json::long(o, "createTime").filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            views: stats.and_then(|s| json::long(s, "playCount")).filter(|n| *n > 0),
            likes: stats.and_then(|s| json::long(s, "diggCount")),
            comments: stats.and_then(|s| json::long(s, "commentCount")),
            shares: stats.and_then(|s| json::long(s, "shareCount")),
            favorites: stats.and_then(|s| json::long(s, "collectCount")),
            ..Stats::default()
        },
        reason: None,
        // 话题当"圈子"（textExtra[].hashtagName）
        community: first_hashtag(o),
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        // 18+ 标记：**实测没有可映射的字段**，一直 false
        adult: false,
        url: Some(video_url(&detail)),
        detail: DetailTarget::Native {
            id: detail,
            platform: None,
        },
    })
}

/// 作者：`uniqueId` 是详情 URL 要用的，`secUid` 只用来去重（都没有就退回数字 id）。
pub fn author(a: Option<&JsonMap>) -> Option<UserRef> {
    let a = a?;
    let unique_id = json::text(a, "uniqueId").or_else(|| json::text(a, "unique_id"));
    let sec_uid = json::text(a, "secUid").or_else(|| json::text(a, "sec_uid"));
    let uid = json::text(a, "id").or_else(|| json::text(a, "uid"));
    let id = unique_id
        .clone()
        .or(sec_uid)
        .or(uid)?;
    let name = json::text(a, "nickname")
        .or_else(|| unique_id.clone())
        .unwrap_or_else(|| id.clone());
    let avatar = json::obj(a, "avatarThumb")
        .or_else(|| json::obj(a, "avatarMedium"))
        .or_else(|| json::obj(a, "avatarLarger"))
        .or_else(|| json::obj(a, "avatar_thumb"))
        .or_else(|| json::obj(a, "avatar_medium"))
        .or_else(|| json::obj(a, "avatar_larger"))
        .and_then(url_list)
        .map(|u| json::to_https(&u));
    Some(UserRef {
        platform: PLATFORM.to_string(),
        id,
        name,
        avatar,
        url: unique_id.map(|u| user_url(&u)),
    })
}

fn first_hashtag(o: &JsonMap) -> Option<String> {
    for e in json::arr(o, "textExtra").into_iter().flatten() {
        let Some(e) = e.as_object() else { continue };
        let name = json::text(e, "hashtagName").or_else(|| json::text(e, "hashtag_name"));
        if let Some(name) = name {
            return Some(name);
        }
    }
    None
}

pub(crate) fn image_post_images(o: &JsonMap) -> Vec<Media> {
    let mut out = Vec::new();
    let images = json::obj(o, "imagePost").and_then(|p| json::arr(p, "images"));
    for e in images.into_iter().flatten() {
        let Some(img) = e.as_object() else { continue };
        let url = json::obj(img, "imageURL")
            .and_then(url_list)
            .or_else(|| url_list(img));
        let Some(url) = url else { continue };
        out.push(Media::Image {
            url: json::to_https(&url),
            width: json::int(img, "imageWidth"),
            height: json::int(img, "imageHeight"),
        });
    }
    out
}

/// `urlList` / `url_list` 的**第一个**元素（Kotlin `TiktokParser` 的 `urlList()`）。
///
/// 注意：Kotlin 的 `(it as? JsonPrimitive)?.content` 对 `JsonNull` 也给字符串 `"null"`
/// （`JsonNull` 也是 `JsonPrimitive`），这里照抄（和抖音那一期同一个怪癖）。
fn url_list(o: &JsonMap) -> Option<String> {
    let arr = json::arr(o, "urlList").or_else(|| json::arr(o, "url_list"))?;
    arr.iter().find_map(primitive_content)
}

/// `JsonPrimitive.content`：字符串 / 数字 / 布尔原样，`null` 给 `"null"`，数组 / 对象没有。
fn primitive_content(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".to_string()),
        _ => None,
    }
}

/// 卡片上的 `Media.Video`：封面（`cover` → `originCover` → `dynamicCover`）+ 时长 + 尺寸。
fn video_media(v: &JsonMap) -> Option<Media> {
    let cover = cover_url(v);
    // `video.duration` 是**秒**
    let duration_sec = json::long(v, "duration").filter(|n| *n > 0).map(|n| n as i32);
    if cover.is_none() && duration_sec.is_none() {
        return None;
    }
    Some(Media::Video {
        cover,
        duration_sec,
        width: json::int(v, "width"),
        height: json::int(v, "height"),
    })
}

/// 封面：web 的 `video.cover` / `originCover` / `dynamicCover` 是**字符串**，也认 `{UrlList:[…]}`。
///
/// 实现和播放那边**是同一份**（[`play::cover_url`]）：卡片封面和播放地址必须挑同一张图 / 同一个地址。
pub fn cover_url(video: &JsonMap) -> Option<String> {
    play::cover_url(video)
}

/// `video.<key>` 的地址列表，两种形态都认（Kotlin `TiktokVideo.addrUrls`）。
pub fn addr_urls(video: &JsonMap, key: &str) -> Vec<String> {
    if let Some(o) = json::obj(video, key) {
        return video_url_list(o);
    }
    match json::text(video, key) {
        Some(t) if t.contains("://") || t.starts_with("//") => vec![json::to_https(&t)],
        _ => Vec::new(),
    }
}

/// `UrlList` / `urlList` 里"像地址"的那些（带 scheme 或协议相对），去重后保留顺序。
fn video_url_list(o: &JsonMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let arr = json::arr(o, "UrlList").or_else(|| json::arr(o, "urlList"));
    for v in arr.into_iter().flatten() {
        let Some(raw) = primitive_content(v) else { continue };
        let raw = raw.trim();
        if !(raw.contains("://") || raw.starts_with("//")) {
            continue;
        }
        let url = json::to_https(raw);
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

// ---------- 业务码 ----------

/// `statusCode` / `status_code` 不是 0 就抛（映射见 [`business_error`]）；是 0 / 没有就通过。
pub fn check_status(json: &JsonMap) -> Result<(), TiktokError> {
    let Some(code) = json::long(json, "statusCode").or_else(|| json::long(json, "status_code")) else {
        return Ok(());
    };
    if code == 0 {
        return Ok(());
    }
    let message = json::text(json, "statusMsg")
        .or_else(|| json::text(json, "status_msg"))
        .or_else(|| json::text(json, "message"));
    Err(business_error(code, message))
}

/// 业务码 → 异常。`statusCode` 与 `status_code` 用同一套。
pub fn business_error(code: i64, message: Option<String>) -> TiktokError {
    let detail = |fallback: &str| message.clone().unwrap_or_else(|| fallback.to_string());
    match code {
        8 | 2483 => TiktokError::NotLoggedIn {
            detail: Some(detail("登录已失效")),
        },
        10000 | 10001 | 2154 => TiktokError::RiskControl {
            code: Some(code.to_string()),
            detail: detail("服务端要求验证码 / 风控"),
        },
        _ => match message {
            Some(m) if m.to_lowercase().contains("url doesn't match") => {
                TiktokError::Signature { detail: m }
            }
            Some(m) => TiktokError::Api {
                code: code.to_string(),
                detail: m,
            },
            None => TiktokError::Api {
                code: code.to_string(),
                detail: "接口返回非 0 的业务码".to_string(),
            },
        },
    }
}

