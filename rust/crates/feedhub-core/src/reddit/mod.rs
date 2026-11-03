//! Reddit：`www.reddit.com/<path>.json`的解析，以及
//! **推荐流 / 关注流的整个请求逻辑**（[`source`]）。
//!
//! 对应 `source-reddit/RedditParser.kt` / `RedditMarkdown.kt` 里**信息流卡片用得到的那部分**，
//! 逐条照 Kotlin 的语义移植（`RustRedditHostedTest` 用同一份 body 两边跑、`FeedItemDiff` 逐字段比）。
//!
//! 两半：
//! - [`markdown`]：`selftext` 的 markdown → 摘要纯文本（`RedditMarkdown.toPlain` 的 Rust 版）；
//! - 本模块：Listing / Thing → [`crate::model::FeedItem`]（列表的 `data.children[]` + `data.after`），
//!   以及 [`source`]：**平台对象** `RedditHostedSource`——拼 URL / 参数顺序 / 请求头顺序、翻页游标、
//!   登录态判定、状态码与风控信号判定，收发通过 `crate::host` 的三个 trait 反向回调宿主。
//!
//! 搜索也在 [`source`]：它和推荐流同构，参数在
//! [`source::RedditHostedSource::search`]，解析直接复用本模块的 [`parse_listing`]。
//!
//! **没搬**：详情（`/comments/<id>.json`）、评论 / 楼中楼（`/api/morechildren`）、作者头像批量取
//! （`/api/user_data_by_account_ids.json`）、视频（`RedditVideo.play` / DASH 清单）。它们继续在
//! Kotlin（`RustRedditSource` 逐方法委托）。信息流卡片要判断"这条是不是视频"、要视频的时长 / 尺寸，
//! 所以 [`video_of`] 那一小段（`RedditVideo.videoOf` 的**元数据**部分）跟着解析一起搬了——
//! 播放地址和清单解析一行没动。
//!
//! 边界（和 B站 / 知乎 / 酷安一样）：**不发请求、不碰 Cookie、不落盘**、不读系统时钟（Reddit 的信息流
//! 请求本来就没有时间戳 / 签名 / 随机数）。
//!
//! [`RedditParser`]: ../../../source-reddit/src/main/kotlin/feedhub/source/reddit/RedditParser.kt
//! [`RedditMarkdown`]: ../../../source-reddit/src/main/kotlin/feedhub/source/reddit/RedditMarkdown.kt

pub mod detail;
pub mod markdown;
pub mod play;
pub mod source;

use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

/// 平台 id（和 Kotlin `Platform.define("reddit", …)` 一样，定了就别改）。
pub const REDDIT: &str = "reddit";

/// 网页根地址（`RedditSource.WWW`）。
pub const WWW: &str = "https://www.reddit.com";

/// 摘要最长多少（信息流卡片只显示一行；和 Kotlin `RedditParser.SUMMARY_LEN` 一样）。
pub const SUMMARY_LEN: usize = 200;

/// 取摘要前先截这么多字符原文（`SUMMARY_SOURCE_LEN`）。
pub const SUMMARY_SOURCE_LEN: usize = 4096;

/// Rust 侧的业务错误。FFI 会把它拆成带类型的 [`FfiError`]（见 `feedhub-ffi` 的 `reddit_source.rs`）。
///
/// 和 Kotlin `RedditSource.checkTransport` 抛的那几种一一对应：
/// `NotLoggedInException` / `RiskControlException` / `ApiException` / `ParseException`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedditError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：403 / 429 / 限流额度用尽 / HTML 挑战页。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl { code: Option<String>, detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 网络错误（连不上 / 超时 / 连接被重置）——只会从平台对象的宿主回调回来。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题），别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },
}

/// 一页列表：`data.children[]`（只要 `kind == "t3"`）+ `data.after`。
///
/// 信息流卡片不画作者头像（画版块图标，见 [`community_icon`]），所以这里没有 `avatars` 参数、
/// 一个请求都不多发（Kotlin `listing(json, avatars = emptyMap())`）。
pub fn parse_listing(json: &JsonMap) -> Result<Page, RedditError> {
    let data = json::obj(json, "data").ok_or_else(|| RedditError::Parse {
        detail: "Listing 缺少 data".to_string(),
    })?;
    let mut items = Vec::new();
    for child in json::arr(data, "children").map(|v| v.as_slice()).unwrap_or(&[]) {
        let Some(o) = child.as_object() else { continue };
        if json::text(o, "kind").as_deref() != Some("t3") {
            continue;
        }
        if let Some(d) = json::obj(o, "data") {
            if let Some(item) = post(d) {
                items.push(item);
            }
        }
    }
    // Kotlin: `data.text("after")?.takeIf { it.isNotBlank() }` —— `text` 已经滤掉空白串
    let next = json::text(data, "after");
    Ok(Page { items, next })
}

/// 一条帖子（`t3` 的 `data`）→ [`FeedItem`]。广告 / 置顶 / 没有 id 的返回 `None`。
///
/// 跨帖（crosspost）：外层的标题、作者、分数是转的人给的，原帖内容走 [`FeedItem::repost`]；
/// 外层自己没有媒体时借原帖的媒体（和 Kotlin `RedditParser.post` 一样）。
pub fn post(o: &JsonMap) -> Option<FeedItem> {
    post_with_avatars(o, None)
}

/// [`post`] 加一张作者头像表（`author_fullname` → 地址；详情 / 评论页才有）。
pub fn post_with_avatars(
    o: &JsonMap,
    avatars: Option<&std::collections::HashMap<String, String>>,
) -> Option<FeedItem> {
    if json::bool(o, "promoted") == Some(true) {
        return None;
    }
    if json::bool(o, "stickied") == Some(true) || json::bool(o, "pinned") == Some(true) {
        return None;
    }
    let id = json::text(o, "id")?;
    let parent = json::arr(o, "crosspost_parent_list")
        .and_then(|a| a.first())
        .and_then(|v| v.as_object())
        .filter(|p| json::text(p, "id").is_some());
    let video = video_of(o);
    let mut media = media_of(o, video.as_ref());
    if media.is_empty() {
        if let Some(p) = parent {
            media = media_of(p, video_of(p).as_ref());
        }
    }
    let selftext = json::string(o, "selftext").unwrap_or_default();
    // 摘要只取前 SUMMARY_LEN 字，先截原文再解析 markdown（长文不用整段走一遍）
    let source = markdown::take_utf16(&selftext, SUMMARY_SOURCE_LEN);
    let summary = {
        let text = markdown::take_utf16(&markdown::to_plain(source), SUMMARY_LEN).to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    };
    let permalink = json::text(o, "permalink");
    Some(FeedItem {
        platform: REDDIT.to_string(),
        id: id.clone(),
        kind: if video.is_some() {
            ItemKind::Video
        } else {
            ItemKind::Post
        },
        title: json::text(o, "title"),
        summary,
        language: None,
        author: author_with_avatars(o, avatars),
        published_at: json::long(o, "created_utc"),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            // Reddit 不给浏览量，就不编；likes 先看 score 再看 ups
            likes: json::long(o, "score").or_else(|| json::long(o, "ups")),
            comments: json::long(o, "num_comments"),
            ..Stats::default()
        },
        reason: None,
        community: community(o),
        community_icon: community_icon(o),
        repost: parent
            .and_then(|p| post_with_avatars(p, avatars))
            .map(Box::new),
        emotes: Vec::new(),
        adult: json::bool(o, "over_18") == Some(true),
        url: permalink.map(|p| format!("{WWW}{p}")),
        detail: DetailTarget::Native {
            id,
            platform: None,
        },
    })
}

/// 版块图标：`sr_detail.community_icon`（列表请求带 `sr_detail=1` 时才有），没有就退回 `sr_detail.icon_img`。
pub fn community_icon(o: &JsonMap) -> Option<String> {
    let sr = json::obj(o, "sr_detail")?;
    let url = json::text(sr, "community_icon").or_else(|| json::text(sr, "icon_img"))?;
    let url = json::to_https(&url.replace("&amp;", "&"));
    if url.trim().is_empty() {
        None
    } else {
        Some(url)
    }
}

/// 所属版块：`r/xxx`。
pub fn community(o: &JsonMap) -> Option<String> {
    json::text(o, "subreddit_name_prefixed")
        .or_else(|| json::text(o, "subreddit").map(|s| format!("r/{s}")))
}

/// `/api/me.json` 的响应 → 当前登录的账号。
///
/// 响应有两种形态：`{"kind":"t2","data":{…}}` 和直接就是 `data`。没有 `name` 就是 `None`
/// （宁可显示"登录已失效"，也不编一个账号）；头像取 `icon_img` / `snoovatar_img`（前者空着就当没有，
/// 和 Kotlin 的 `text(key) ?: text(key)` 一样）。`url` 是个人主页（和 Kotlin 一样总是给）。
pub fn parse_me(json: &JsonMap) -> Option<UserRef> {
    let data = json::obj(json, "data").unwrap_or(json);
    let name = json::text(data, "name")?;
    let id = json::text(data, "id").unwrap_or_else(|| name.clone());
    Some(UserRef {
        platform: REDDIT.to_string(),
        id,
        name: name.clone(),
        avatar: json::text(data, "icon_img")
            .or_else(|| json::text(data, "snoovatar_img"))
            .map(|u| json::to_https(&u)),
        url: Some(format!("{WWW}/user/{name}")),
    })
}

/// 作者。`author_fullname`（`t2_…`）是账号 id，`UserRef.id` 用去掉 `t2_` 的那段。
///
/// 信息流不取头像（列表 / 评论 JSON 本来也不带），所以这里 `avatar` 永远是 `None`——
/// 和 Kotlin `listing` 传空头像表是同一个结果。
pub fn author(o: &JsonMap) -> Option<UserRef> {
    author_with_avatars(o, None)
}

/// [`author`] 加头像表：`author_fullname` 查到就填 `avatar`，查不到留空（不影响名字 / 正文）。
pub fn author_with_avatars(
    o: &JsonMap,
    avatars: Option<&std::collections::HashMap<String, String>>,
) -> Option<UserRef> {
    let name = json::text(o, "author")?;
    if name == "[deleted]" {
        return None;
    }
    let full = json::text(o, "author_fullname").filter(|s| !s.trim().is_empty());
    let id = full
        .as_deref()
        .map(|f| f.strip_prefix("t2_").unwrap_or(f))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&name)
        .to_string();
    let avatar = full
        .as_deref()
        .and_then(|f| avatars.and_then(|m| m.get(f)).cloned());
    Some(UserRef {
        platform: REDDIT.to_string(),
        id,
        name: name.clone(),
        avatar,
        url: Some(format!("{WWW}/user/{name}")),
    })
}

// ===========================================================================
// 媒体（信息流卡片画什么）
// ===========================================================================

/// 视频帖的元数据。播放地址 / DASH 清单解析**不在当前**（见模块注释）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMeta {
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 从一条帖子（或它的 crosspost 原帖）里挑出 `reddit_video`。
/// 优先 `secure_media`，其次 `media`，最后 `preview.reddit_video_preview`（和 `RedditVideo.videoOf` 一样）。
pub fn video_of(post: &JsonMap) -> Option<VideoMeta> {
    let mut candidates: Vec<&JsonMap> = vec![post];
    if let Some(p) = json::arr(post, "crosspost_parent_list")
        .and_then(|a| a.first())
        .and_then(|v| v.as_object())
    {
        candidates.push(p);
    }
    for o in candidates {
        let rv = json::obj(o, "secure_media")
            .and_then(|m| json::obj(m, "reddit_video"))
            .or_else(|| json::obj(o, "media").and_then(|m| json::obj(m, "reddit_video")))
            .or_else(|| {
                json::obj(o, "preview").and_then(|m| json::obj(m, "reddit_video_preview"))
            });
        let Some(rv) = rv else { continue };
        let fallback = json::text(rv, "fallback_url");
        let hls = json::text(rv, "hls_url");
        let dash = json::text(rv, "dash_url");
        // Kotlin 里 `id = dash→videoIdOf ?: fallback→videoIdOf ?: o.text("id") ?: continue`：
        // 三个地址都没有、帖子又没有 id 的时候这个候选是**跳过**，不是"有视频但没地址"
        let video_id = dash
            .as_deref()
            .and_then(video_id_of)
            .or_else(|| fallback.as_deref().and_then(video_id_of))
            .or_else(|| json::text(o, "id"));
        if video_id.is_none() {
            continue;
        }
        if fallback.is_none() && hls.is_none() && dash.is_none() {
            continue;
        }
        return Some(VideoMeta {
            duration_sec: json::int(rv, "duration"),
            width: json::int(rv, "width"),
            height: json::int(rv, "height"),
        });
    }
    None
}

/// 取视频 id：`https://v.redd.it/<id>/...` 的第一段路径（`RedditVideo.videoIdOf`）。
fn video_id_of(url: &str) -> Option<String> {
    // Kotlin: `substringAfter("//").substringAfter('/', "").substringBefore('/')`
    let after = url.split_once("//").map(|(_, r)| r).unwrap_or(url);
    let path = after.split_once('/').map(|(_, r)| r).unwrap_or("");
    let first = path.split_once('/').map(|(a, _)| a).unwrap_or(path);
    if first.trim().is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

fn media_of(o: &JsonMap, video: Option<&VideoMeta>) -> Vec<Media> {
    if let Some(v) = video {
        return vec![Media::Video {
            cover: preview_image(o).map(|i| i.url),
            duration_sec: v.duration_sec,
            width: v.width,
            height: v.height,
        }];
    }
    if let Some(gallery) = gallery_images(o) {
        if !gallery.is_empty() {
            return gallery
                .into_iter()
                .map(|i| Media::Image {
                    url: i.url,
                    width: i.width,
                    height: i.height,
                })
                .collect();
        }
    }
    if let Some(img) = preview_image(o) {
        return vec![Media::Image {
            url: img.url,
            width: img.width,
            height: img.height,
        }];
    }
    Vec::new()
}

/// 一张图（信息流卡片只画 url / 尺寸；详情正文还要原图和 alt 文字）。
pub(crate) struct Img {
    pub(crate) url: String,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) original: Option<String>,
    pub(crate) caption: Option<String>,
}

/// 相册：`gallery_data.items[]` 给顺序，`media_metadata[media_id].s` 给地址和尺寸（`status` 不是 valid 的丢掉）。
pub(crate) fn gallery_images(o: &JsonMap) -> Option<Vec<Img>> {
    if json::bool(o, "is_gallery") != Some(true) {
        return None;
    }
    let items = json::obj(o, "gallery_data").and_then(|g| json::arr(g, "items"))?;
    let meta = json::obj(o, "media_metadata")?;
    let mut out = Vec::new();
    for item in items {
        let Some(it) = item.as_object() else { continue };
        let Some(media_id) = json::text(it, "media_id") else {
            continue;
        };
        let Some(m) = json::obj(meta, &media_id) else {
            continue;
        };
        if json::text(m, "status").map(|s| s.to_lowercase()).as_deref() != Some("valid") {
            continue;
        }
        let source = json::obj(m, "s");
        // `s.u` 是最大的那档；动图的真身看 `s.gif`
        let url = source
            .and_then(|s| json::text(s, "gif").or_else(|| json::text(s, "u")))
            .map(|u| json::to_https(&u));
        let Some(url) = url else { continue };
        out.push(Img {
            original: original_of(&url),
            url,
            width: source.and_then(|s| json::int(s, "x")),
            height: source.and_then(|s| json::int(s, "y")),
            caption: json::text(it, "caption"),
        });
    }
    Some(out)
}

/// 单图：`preview.images[0].source`（`preview.redd.it`）。
pub(crate) fn preview_image(o: &JsonMap) -> Option<Img> {
    let source = json::obj(o, "preview")
        .and_then(|p| json::arr(p, "images"))
        .and_then(|a| a.first())
        .and_then(|v| v.as_object())
        .and_then(|img| json::obj(img, "source"))?;
    let url = json::to_https(&json::text(source, "url")?);
    Some(Img {
        original: original_of(&url),
        url,
        width: json::int(source, "width"),
        height: json::int(source, "height"),
        caption: None,
    })
}

/// `preview.redd.it/<hash>.jpg?…` 的原图是 `i.redd.it/<hash>.jpg`（Reddit 自己的图床同路径，去掉缩放参数）。
///
/// **`external-preview.redd.it` 不算**：那是站外图片的再托管副本，`i.redd.it/<hash>` 上并没有这张图
/// 。别的域名、路径不像单段文件名时也给 `None`。
fn original_of(url: &str) -> Option<String> {
    let after = url.split_once("//").map(|(_, r)| r).unwrap_or(url);
    let host = after.split('/').next().unwrap_or("");
    if host != "preview.redd.it" {
        return None;
    }
    let path = after
        .split_once('/')
        .map(|(_, r)| r)
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    if path.is_empty() || path.contains('/') {
        return None;
    }
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
        return None;
    }
    Some(format!("https://i.redd.it/{path}"))
}

