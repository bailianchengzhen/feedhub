//! X（twitter.com / x.com）：**信息流的解析** + [`source`] 里的**平台层**（整个请求怎么发）+ [`txid`] 里的
//! `x-client-transaction-id` 算法。
//!
//! 对应 `source-x`：
//! - `XParser`（`parseHomeTimeline` / `parseHomeLatestTimeline` / `parseTimeline` / `parseTweetResult` /
//!   `parseMedia` / `buildBody` / `parseCard` / `addEntries` / `bottomCursor` / `parseTwitterTime`）→
//!   **本模块**逐行移植，只搬**信息流卡片用得到的那部分**（`FeedItem` 及其 `repost`）；
//!   详情 / 评论（`parseTweetDetail`）继续在 Kotlin，没有搬。
//! - `XSource` 的 `feed()` / `graphql()` / `graphqlUrl()` / `timelineVariables()` / `checkTransport` /
//!   `checkErrors` / `XPageBudget` → [`source`]（平台对象 `XHostedSource`）。
//! - `XClientTransaction` / `XtxMath` 里**算法那一段** → [`txid`]。
//!
//! 边界（和 B站 / 酷安一样）：**不发请求、不碰 Cookie、不落盘**。收发全部通过 `crate::host` 的三个 trait
//! 回调宿主 OkHttp；时间戳 / 随机数由调用方注入；`.so` 里没有网络栈。
//!
//! **详情 / 评论**也搬了：`parse_tweet_detail`（`TweetDetail` 的
//! `threaded_conversation_with_injections_v2`）——焦点推文之后才是评论、`conversationthread-` 模块的第一条
//! 是评论、剩下的进 `preview_replies`、焦点之前的祖先链不展开、`TimelineTerminateTimeline(Bottom)` 之后
//! 没有下一页。**楼中楼当前仍然返回空页**。
//!
//! 没搬的：视频播放地址（`XVideoPlay.playInfo`）/ 搜索。`RustXSource` 逐方法委托 Kotlin 的 `XSource`
//! （连 `VideoSource.play` 也是）。

pub mod play;
pub mod profile;
pub mod rate;
pub mod sign_chain;
pub mod source;
pub mod txid;

use crate::detail::{Block, Comment, FeedDetail, RichText, Span, VideoInfo, VideoPart};
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision, UserRef};

/// 平台 id（和 Kotlin `Platform.define("x", "X")` 一样，定了就别改）。
pub const X: &str = "x";

/// 推文的网页根（`XParser.parseTweetResult` 拼 `url` 用的是它，和 `XSource.API` 无关）。
pub const WEB: &str = "https://x.com";

/// GraphQL 的 operation（对应 Kotlin `XOperation` / `XClientProfile.queryIds` 的键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XOperation {
    HomeTimeline,
    HomeLatestTimeline,
    TweetDetail,
}

impl XOperation {
    /// 拼在 `/i/api/graphql/<queryId>/<op>` 里的名字，也是 profile 里的键。
    pub fn op(self) -> &'static str {
        match self {
            XOperation::HomeTimeline => "HomeTimeline",
            XOperation::HomeLatestTimeline => "HomeLatestTimeline",
            XOperation::TweetDetail => "TweetDetail",
        }
    }
}

/// 哪一条流（对应 Kotlin `FeedKind` 里 X 支持的两个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

impl FeedKind {
    /// 页数账本的键（Kotlin `FeedKind.key`）。
    pub fn key(self) -> &'static str {
        match self {
            FeedKind::Recommend => "recommend",
            FeedKind::Following => "following",
        }
    }

    /// 这一条流走哪个 GraphQL operation。
    pub fn operation(self) -> XOperation {
        match self {
            FeedKind::Recommend => XOperation::HomeTimeline,
            FeedKind::Following => XOperation::HomeLatestTimeline,
        }
    }
}

/// Rust 侧的业务错误。FFI 会把它拆成带类型的 `FfiError`（见 `feedhub-ffi/src/x_source.rs` 的 `map_error`）。
///
/// 和 Kotlin `XSource.checkTransport` / `checkErrors` 抛的那几种一一对应：
/// `RiskControlException` / `NotLoggedInException` / `ApiException` / `ParseException`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：限速、锁定、`looks automated`。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl { code: Option<String>, detail: String },

    #[error("签名被拒：{detail}")]
    Signature { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 网络错误（连不上 / 超时 / 连接被重置）——只会从**平台对象**的宿主回调回来。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常），别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },

    /// 内部错误（profile 里没有 queryId 之类）。**不是**平台行为。
    #[error("内部错误：{detail}")]
    Internal { detail: String },
}

/// 一页时间线。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XTimelinePage {
    pub items: Vec<FeedItem>,
    /// 下一页游标；`None` 表示到底了（含 `TimelineTerminateTimeline(Bottom)` 的情况）。
    pub next: Option<String>,
}

impl XTimelinePage {
    pub fn into_model_page(self) -> crate::model::Page {
        crate::model::Page {
            items: self.items,
            next: self.next,
        }
    }
}

// ===========================================================================
// 时间线
// ===========================================================================

/// `HomeTimeline`（为你推荐）的响应 → 一条流的条目。
pub fn parse_home_timeline(root: &JsonMap) -> XTimelinePage {
    parse_timeline(urt_of(root))
}

/// `HomeLatestTimeline`（正在关注）的响应。**形状和上面一样**（Kotlin 两处都走 `urtOf`）。
pub fn parse_home_latest_timeline(root: &JsonMap) -> XTimelinePage {
    parse_timeline(urt_of(root))
}

/// `data.home.home_timeline_urt`。
fn urt_of(root: &JsonMap) -> Option<&JsonMap> {
    let data = json::obj(root, "data")?;
    let home = json::obj(data, "home")?;
    json::obj(home, "home_timeline_urt")
}

/// `home_timeline_urt.instructions` → 条目 + 下游标。
pub fn parse_timeline(urt: Option<&JsonMap>) -> XTimelinePage {
    let Some(instructions) = urt.and_then(|u| json::arr(u, "instructions")) else {
        return XTimelinePage::default();
    };
    let entries = add_entries(instructions);

    // Kotlin 是 `LinkedHashMap<String, XTweet>`：**同一个 key 后出现的覆盖先出现的，位置不变**。
    let mut items: Vec<FeedItem> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut push = |item: FeedItem| {
        let key = item.key();
        match index.get(&key) {
            Some(&i) => items[i] = item,
            None => {
                index.insert(key, items.len());
                items.push(item);
            }
        }
    };

    for entry in &entries {
        if is_promoted(entry) {
            continue;
        }
        if entry.id.starts_with("tweet-") {
            if let Some(item) = parse_tweet_result(entry.tweet_result()) {
                push(item);
            }
        } else if entry.id.starts_with("home-conversation-")
            || entry.id.starts_with("conversationthread-")
        {
            // 会话模块：模块里的每条 TimelineTweet 都展开成独立条目
            for result in module_tweets(entry) {
                if let Some(item) = parse_tweet_result(Some(result)) {
                    push(item);
                }
            }
        }
    }

    XTimelinePage {
        items,
        next: bottom_cursor(instructions, &entries),
    }
}

// ===========================================================================
// 推文详情
// ===========================================================================

/// 推文详情的解析结果（Kotlin `XDetailPage`）：焦点推文 + 焦点之后的回复 + 下一页游标。
#[derive(Default)]
pub struct XDetailPage {
    pub tweet: Option<XParsedTweet>,
    pub replies: Vec<Comment>,
    pub next: Option<String>,
}

/// 详情的响应（`TweetDetail`：`data.threaded_conversation_with_injections_v2`）→ 详情 / 评论。
///
/// 逐条照 Kotlin `XParser.parseTweetDetail`：
/// - **每条 `tweet-` 只解析一次**（按 `entryId` 收好，后面判断焦点 / 拼回复都复用）；
/// - 焦点之后的 `tweet-` 才是评论，**焦点之前的祖先链不展开**；
/// - `conversationthread-` 模块的第一条是评论、剩下的进 `preview_replies`；
/// - `TimelineTerminateTimeline(Bottom)` 之后没有下一页（即使还挂着 bottom 游标）。
pub fn parse_tweet_detail(root: &JsonMap, focal_id: Option<&str>) -> XDetailPage {
    let Some(data) = json::obj(root, "data") else {
        return XDetailPage::default();
    };
    let urt = json::obj(data, "threaded_conversation_with_injections_v2")
        .or_else(|| json::obj(data, "threaded_conversation_with_injections"));
    let Some(urt) = urt else {
        return XDetailPage::default();
    };
    let Some(instructions) = json::arr(urt, "instructions") else {
        return XDetailPage::default();
    };
    let entries = add_entries(instructions);

    let mut parsed: Vec<(String, XParsedTweet)> = Vec::new();
    for entry in &entries {
        if is_promoted(entry) {
            continue;
        }
        if !entry.id.starts_with("tweet-") {
            continue;
        }
        if let Some(tweet) = parse_tweet(entry.tweet_result()) {
            parsed.push((entry.id.clone(), tweet));
        }
    }
    let focal_index = focal_id
        .and_then(|id| parsed.iter().position(|(_, t)| t.item.id == id))
        .unwrap_or(0);
    let focal_id_resolved = parsed.get(focal_index).map(|(_, t)| t.item.id.clone());

    let mut replies: Vec<Comment> = Vec::new();
    let mut seen_focal = parsed.get(focal_index).is_none();
    for entry in &entries {
        if is_promoted(entry) {
            continue;
        }
        if entry.id.starts_with("tweet-") {
            let tweet = parsed.iter().find(|(id, _)| id == &entry.id).map(|(_, t)| t);
            if !seen_focal {
                if let (Some(tweet), Some(focal)) = (tweet, focal_id_resolved.as_deref()) {
                    if tweet.item.id == focal {
                        seen_focal = true;
                    }
                }
                continue;
            }
            if let Some(tweet) = tweet {
                replies.push(comment_of(tweet));
            }
        } else if entry.id.starts_with("conversationthread-") {
            if !seen_focal {
                continue;
            }
            let items: Vec<XParsedTweet> = module_tweets(entry)
                .into_iter()
                .filter_map(|r| parse_tweet(Some(r)))
                .collect();
            if items.is_empty() {
                continue;
            }
            let mut first = comment_of(&items[0]);
            first.preview_replies = items[1..].iter().map(comment_of).collect();
            replies.push(first);
        }
    }

    let tweet = parsed.into_iter().nth(focal_index).map(|(_, t)| t);
    XDetailPage {
        tweet,
        replies,
        next: bottom_cursor(instructions, &entries),
    }
}

/// 详情里的回复包成一级评论（Kotlin `XParser.commentOf`）：正文 / 作者 / 时间 / 点赞 / 回复数 / 语言。
fn comment_of(tweet: &XParsedTweet) -> Comment {
    Comment {
        platform: X.to_string(),
        id: tweet.item.id.clone(),
        author: tweet.item.author.clone(),
        content: tweet.body.clone(),
        published_at: tweet.item.published_at,
        published_at_precision: TimePrecision::Second,
        likes: tweet.item.stats.likes,
        reply_count: tweet.item.stats.comments,
        reply_to: None,
        ip_location: None,
        floor: None,
        pinned: false,
        preview_replies: Vec::new(),
        language: tweet.item.language.clone(),
    }
}

/// 详情页的 `FeedDetail`：焦点推文 + 正文；"整条就是视频"时把分 P 放进 `video`。
pub fn detail_of(page: &XDetailPage) -> Option<FeedDetail> {
    let tweet = page.tweet.as_ref()?;
    Some(FeedDetail {
        item: tweet.item.clone(),
        body: tweet.body.clone(),
        video: if tweet.video_only {
            Some(VideoInfo {
                parts: tweet
                    .videos
                    .iter()
                    .map(|v| VideoPart {
                        id: v.part_id(),
                        // Kotlin `XVideoPlay.parts`：GIF / "视频 N"
                        title: Some(if v.is_gif {
                            "GIF".to_string()
                        } else {
                            format!("视频 {}", v.media_index + 1)
                        }),
                        duration_sec: v.duration_sec,
                        width: v.width,
                        height: v.height,
                    })
                    .collect(),
                danmaku_segment_ms: None,
            })
        } else {
            None
        },
        follow: None,
        related: Vec::new(),
    })
}

// ===========================================================================
// 单条推文
// ===========================================================================

/// 一条推文解析出来的全部东西：信息流条目 + 详情页的正文 / 视频 / "整条就是视频"。
pub struct XParsedTweet {
    pub item: FeedItem,
    /// 正文（转推时是**被转那条**的正文，和 Kotlin `XTweet.body` 一样）。
    pub body: RichText,
    pub videos: Vec<XVideoMeta>,
    pub video_only: bool,
}

/// `tweet_results.result`（可能是 `TweetWithVisibilityResults` 包装）→ [`FeedItem`]；
/// 不该显示的（已删 / 不可见 / 订阅预览）返回 `None`。
///
/// 信息流只要条目，用这个（[`parse_tweet`] 的薄包装）。
pub fn parse_tweet_result(result: Option<&JsonMap>) -> Option<FeedItem> {
    parse_tweet(result).map(|t| t.item)
}

/// [`parse_tweet_result`] 的完整版：正文的 `RichText`、视频元数据、`videoOnly` 都在这里（详情页要用）。
pub fn parse_tweet(result: Option<&JsonMap>) -> Option<XParsedTweet> {
    let result = result?;
    match json::string(result, "__typename").as_deref() {
        Some("TweetUnavailable") | Some("TweetTombstone") | Some("TweetPreviewDisplay") => {
            return None
        }
        _ => {}
    }
    // TweetWithVisibilityResults / 订阅预览等：内容在 .tweet 里
    if let Some(inner) = json::obj(result, "tweet") {
        return parse_tweet(Some(inner));
    }
    if json::obj(result, "tombstone").is_some() {
        return None;
    }
    let legacy = json::obj(result, "legacy")?;

    let id = json::text(legacy, "id_str").or_else(|| json::text(result, "rest_id"))?;
    let author = parse_author(result);
    let screen_name = author
        .as_ref()
        .and_then(|a| a.url.as_deref())
        .map(|u| u.rsplit('/').next().unwrap_or("").to_string());
    let url = match screen_name {
        Some(ref sn) if !sn.is_empty() => format!("{WEB}/{sn}/status/{id}"),
        _ => format!("{WEB}/i/status/{id}"),
    };

    let note_result = json::obj(result, "note_tweet")
        .and_then(|n| json::obj(n, "note_tweet_results"))
        .and_then(|n| json::obj(n, "result"));
    let note_text = note_result.and_then(|n| json::text(n, "text"));
    let entities = if note_text.is_some() {
        note_result.and_then(|n| json::obj(n, "entity_set"))
    } else {
        json::obj(legacy, "entities")
    };
    let text = note_text
        .clone()
        .or_else(|| json::text(legacy, "full_text"))
        .or_else(|| json::text(legacy, "text"))
        .unwrap_or_default();

    let media = parse_media(legacy, &id, &url);
    // note tweet 的 legacy.display_text_range 是给 legacy.full_text 的，套到长文正文上会截断
    let built = build_body(
        &text,
        entities,
        legacy,
        json::obj(result, "card"),
        &media,
        note_text.is_none(),
    );
    let summary = nonzero_text(&built.text_only);
    // 整条就是视频：正文没有文字、只有视频 → 视频块已经在 build_body 里不放正文了
    let own_video_only = built.visible_blank && media.iter().any(|m| m.is_video);

    let own = FeedItem {
        platform: X.to_string(),
        id: id.clone(),
        kind: if own_video_only {
            ItemKind::Video
        } else {
            ItemKind::Post
        },
        title: None,
        summary,
        // X 自己检测过这条推文的语言（BCP 47）；翻译门控直接用，不再让本地模型猜。
        language: json::text(legacy, "lang"),
        author,
        published_at: parse_twitter_time(json::text(legacy, "created_at").as_deref()),
        published_at_precision: TimePrecision::Second,
        media: media.iter().map(|m| m.media.clone()).collect(),
        stats: Stats {
            views: json::obj(result, "views").and_then(|v| json::long(v, "count")),
            likes: json::long(legacy, "favorite_count"),
            comments: json::long(legacy, "reply_count"),
            shares: json::long(legacy, "retweet_count"),
            ..Stats::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        // 平台标的 18+ / 敏感内容 → 统一的成人标记（整条隐藏）；转推 / 引用各自带自己的
        adult: json::bool(legacy, "possibly_sensitive") == Some(true),
        url: Some(url),
        detail: DetailTarget::Native {
            id: id.clone(),
            platform: None,
        },
    };

    // 转推：外层是"谁转了"，内容用被转的那条；引用同理（都放 repost）
    let retweeted = json::obj(legacy, "retweeted_status_result")
        .and_then(|r| json::obj(r, "result"))
        .and_then(|r| parse_tweet(Some(r)));
    let quoted = json::obj(result, "quoted_status_result")
        .and_then(|r| json::obj(r, "result"))
        .and_then(|r| parse_tweet(Some(r)));

    let (item, body, videos, video_only) = match retweeted {
        Some(orig) => {
            let videos = if orig.videos.is_empty() {
                media.iter().filter_map(|m| m.video.clone()).collect()
            } else {
                orig.videos.clone()
            };
            (
                FeedItem {
                    title: orig.item.title.clone(),
                    summary: orig.item.summary.clone(),
                    // 卡片 / 详情显示的是被转推的正文，语言也要跟着被转的那条走
                    language: orig.item.language.clone(),
                    media: orig.item.media.clone(),
                    published_at: orig.item.published_at,
                    stats: orig.item.stats.clone(),
                    reason: Some("转推".to_string()),
                    repost: Some(Box::new(orig.item.clone())),
                    ..own
                },
                orig.body,
                videos,
                orig.video_only,
            )
        }
        None => (
            FeedItem {
                repost: quoted.as_ref().map(|q| Box::new(q.item.clone())),
                ..own
            },
            built.rich,
            media.iter().filter_map(|m| m.video.clone()).collect(),
            own_video_only,
        ),
    };

    Some(XParsedTweet {
        item,
        body,
        videos,
        video_only,
    })
}

/// 作者。X 有两套形状：老的在 `legacy.screen_name` / `legacy.profile_image_url_https`，
/// 2026 录到的响应里是新的——`core.screen_name` / `core.name` + `avatar.image_url`。两套都认。
fn parse_author(result: &JsonMap) -> Option<UserRef> {
    let user = json::obj(result, "core")
        .and_then(|c| json::obj(c, "user_results"))
        .and_then(|r| json::obj(r, "result"))?;
    let legacy = json::obj(user, "legacy");
    let new_core = json::obj(user, "core");
    let id =
        json::text(user, "rest_id").or_else(|| legacy.and_then(|l| json::text(l, "id_str")))?;
    let screen_name = new_core
        .and_then(|c| json::text(c, "screen_name"))
        .or_else(|| legacy.and_then(|l| json::text(l, "screen_name")));
    let name = new_core
        .and_then(|c| json::text(c, "name"))
        .or_else(|| legacy.and_then(|l| json::text(l, "name")))
        .or_else(|| screen_name.clone())
        .unwrap_or_else(|| id.clone());
    let avatar = json::obj(user, "avatar")
        .and_then(|a| json::text(a, "image_url"))
        .or_else(|| legacy.and_then(|l| json::text(l, "profile_image_url_https")))
        .map(|u| json::to_https(&u));
    Some(UserRef {
        platform: X.to_string(),
        id,
        name,
        avatar,
        url: screen_name.map(|sn| format!("{WEB}/{sn}")),
    })
}

// ---------- 媒体 ----------

/// 一条媒体：信息流卡片要的 [`Media`] + 正文要的 [`Block`] + 详情页要的视频元数据。
///
/// 播放地址**不在这里**（`XVideoPlay` 现取，会过期）；这里只有 `part_id` / 时长 / 尺寸。
pub struct ParsedMedia {
    pub media: Media,
    pub is_video: bool,
    /// 正文里的块（图片 / 视频；Kotlin `ParsedMedia.block`）。
    pub block: Option<Block>,
    /// 视频的那份元数据（`XTweet.videos`）；图片是 `None`。
    pub video: Option<XVideoMeta>,
}

/// 视频地址的一个 variant（Kotlin `XVideoVariant`）：`content_type` 是 `video/mp4` 或
/// `application/x-mpegURL`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XVideoVariant {
    pub url: String,
    pub content_type: String,
    pub bitrate: Option<i64>,
}

/// 一个视频在推文里的身份 + 地址候选（Kotlin `XVideo`）：`part_id` 是交给 `VideoSource.play` 的。
#[derive(Clone, Debug)]
pub struct XVideoMeta {
    /// 在 `extended_entities.media` 里的**原始下标**（Kotlin `mediaIndex`）。
    pub media_index: usize,
    pub is_gif: bool,
    pub duration_sec: Option<i32>,
    /// `video_info.duration_millis` 原值（播放信息用毫秒；`duration_sec` 是它除以 1000 取整）。
    pub duration_ms: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// `video_info.variants`：地址会过期（带时效签名），所以**每次播放都重新拉一次推文详情**
    /// （见 [`super::play`]），不复用信息流 / 详情里的旧地址。
    pub variants: Vec<XVideoVariant>,
}

impl XVideoMeta {
    pub fn part_id(&self) -> String {
        format!("media:{}", self.media_index)
    }
}

/// Kotlin `parseMedia(legacy, tweetId, tweetUrl)`：下标是**原始数组下标**（跳过的元素也占位）。
fn parse_media(legacy: &JsonMap, tweet_id: &str, tweet_url: &str) -> Vec<ParsedMedia> {
    let list = json::obj(legacy, "extended_entities")
        .and_then(|e| json::arr(e, "media"))
        .or_else(|| json::obj(legacy, "entities").and_then(|e| json::arr(e, "media")));
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, element) in list.iter().enumerate() {
        let Some(m) = element.as_object() else {
            continue;
        };
        let media_type = json::string(m, "type").unwrap_or_default();
        let url = json::text(m, "media_url_https").map(|u| json::to_https(&u));
        let original = json::obj(m, "original_info");
        let width = original.and_then(|o| json::int(o, "width"));
        let height = original.and_then(|o| json::int(o, "height"));
        let alt = json::text(m, "ext_alt_text");
        match media_type.as_str() {
            "photo" => {
                let Some(url) = url else { continue };
                out.push(ParsedMedia {
                    media: Media::Image {
                        url: url.clone(),
                        width,
                        height,
                    },
                    is_video: false,
                    block: Some(Block::Image {
                        url,
                        width,
                        height,
                        original: None,
                        caption: alt,
                    }),
                    video: None,
                });
            }
            "video" | "animated_gif" => {
                let info = json::obj(m, "video_info");
                // 少数视频的 original_info 是空的，退回 URL 里的 /<w>x<h>/
                let (w, h) = match (width, height) {
                    (Some(w), Some(h)) => (Some(w), Some(h)),
                    _ => size_from_url(url.as_deref()),
                };
                let duration_ms = info.and_then(|i| json::long(i, "duration_millis"));
                let duration_sec = duration_ms.map(|ms| (ms / 1000) as i32);
                let is_gif = media_type == "animated_gif";
                let meta = XVideoMeta {
                    media_index: index,
                    is_gif,
                    duration_sec,
                    duration_ms,
                    width: w,
                    height: h,
                    variants: parse_variants(info),
                };
                out.push(ParsedMedia {
                    media: Media::Video {
                        cover: url.clone(),
                        duration_sec,
                        width: w,
                        height: h,
                    },
                    is_video: true,
                    block: Some(Block::Video {
                        id: tweet_id.to_string(),
                        part_id: meta.part_id(),
                        cover: url,
                        duration_sec,
                        width: w,
                        height: h,
                        // 正文里那一小块：GIF / 视频（分 P 的标题在 `VideoPart` 里，见 `video_info`）
                        title: Some(if is_gif { "GIF" } else { "视频" }.to_string()),
                        url: Some(tweet_url.to_string()),
                    }),
                    video: Some(meta),
                });
            }
            _ => {}
        }
    }
    out
}

/// `video_info.variants` → [`XVideoVariant`]（Kotlin `XParser` 里那段 `mapNotNull`）：
/// 缺 `url` 的丢掉，`content_type` 缺了就是空串（Kotlin `.orEmpty()`），`bitrate` 缺了是 `None`。
fn parse_variants(info: Option<&JsonMap>) -> Vec<XVideoVariant> {
    let Some(list) = info.and_then(|i| json::arr(i, "variants")) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|v| v.as_object())
        .filter_map(|vo| {
            let url = json::text(vo, "url")?;
            Some(XVideoVariant {
                url,
                content_type: json::string(vo, "content_type").unwrap_or_default(),
                bitrate: json::long(vo, "bitrate"),
            })
        })
        .collect()
}

/// 从 `.../abc/1280x720/xyz.mp4` 这种地址里抠宽高。
fn size_from_url(url: Option<&str>) -> (Option<i32>, Option<i32>) {
    let Some(url) = url else {
        return (None, None);
    };
    let bytes = url.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'/' {
            i += 1;
            continue;
        }
        // `/数字x数字/`
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > start && j < bytes.len() && bytes[j] == b'x' {
            let w = &url[start..j];
            let mut k = j + 1;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > j + 1 && k < bytes.len() && bytes[k] == b'/' {
                return (w.parse().ok(), url[j + 1..k].parse().ok());
            }
        }
        i += 1;
    }
    (None, None)
}

// ---------- 正文 / 实体 ----------

/// 正文装配的结果：`rich` 是详情页的正文，`text_only` 是信息流卡片的 `summary` 来源，
/// `visible_blank` 给"整条就是视频"用。
pub struct BuiltBody {
    pub rich: RichText,
    pub text_only: String,
    pub visible_blank: bool,
}

/// 逐行照 `XParser.buildBody`：先按 `display_text_range` 裁出正文，再把实体（链接 / @ / 话题）
/// 排进段落，最后拼上图片 / 视频块和卡片块（`[title]`）。
///
/// **下标按 UTF-16 算**（Kotlin 的 `String.substring` 是 UTF-16 code unit 下标），所以内部先把正文转成
/// `Vec<u16>` 再切片——否则带 emoji 的推文会和 Kotlin 差一个字符。
fn build_body(
    text: &str,
    entities: Option<&JsonMap>,
    legacy: &JsonMap,
    card: Option<&JsonMap>,
    media: &[ParsedMedia],
    display_range: bool,
) -> BuiltBody {
    let text_u16: Vec<u16> = text.encode_utf16().collect();
    let len = text_u16.len();

    let range = if display_range {
        json::arr(legacy, "display_text_range")
    } else {
        None
    };
    let start = range.and_then(|r| r.first()).and_then(json_int).unwrap_or(0);
    let end = range
        .and_then(|r| r.get(1))
        .and_then(json_int)
        .unwrap_or(len as i32);
    let from = start.clamp(0, len as i32) as usize;
    let to = end.clamp(from as i32, len as i32) as usize;
    let visible: Vec<u16> = text_u16[from..to].to_vec();

    let mut found: Vec<(i32, i32, Span)> = Vec::new();
    if let Some(entities) = entities {
        if let Some(urls) = json::arr(entities, "urls") {
            for e in urls {
                let Some(o) = e.as_object() else { continue };
                let Some((a, b)) = indices(o) else { continue };
                let expanded = json::text(o, "expanded_url").or_else(|| json::text(o, "url"));
                let Some(expanded) = expanded else { continue };
                let display = json::text(o, "display_url").unwrap_or(expanded.clone());
                found.push((
                    a,
                    b,
                    Span::Link {
                        text: display,
                        url: expanded,
                        target: None,
                    },
                ));
            }
        }
        if let Some(mentions) = json::arr(entities, "user_mentions") {
            for e in mentions {
                let Some(o) = e.as_object() else { continue };
                let Some((a, b)) = indices(o) else { continue };
                let Some(screen) = json::text(o, "screen_name") else {
                    continue;
                };
                let id = json::text(o, "id_str").unwrap_or_else(|| screen.clone());
                let name = json::text(o, "name").unwrap_or_else(|| screen.clone());
                found.push((
                    a,
                    b,
                    Span::Mention {
                        text: format!("@{screen}"),
                        user: UserRef {
                            platform: X.to_string(),
                            id,
                            name,
                            avatar: None,
                            url: Some(format!("{WEB}/{screen}")),
                        },
                    },
                ));
            }
        }
        if let Some(tags) = json::arr(entities, "hashtags") {
            for e in tags {
                let Some(o) = e.as_object() else { continue };
                let Some((a, b)) = indices(o) else { continue };
                let Some(tag) = json::text(o, "text") else { continue };
                found.push((
                    a,
                    b,
                    Span::Link {
                        text: format!("#{tag}"),
                        url: format!("{WEB}/hashtag/{tag}"),
                        target: None,
                    },
                ));
            }
        }
    }

    // 只留完全落在正文范围内的实体，按起点稳定排序
    found.retain(|(a, b, _)| *a >= from as i32 && *b <= to as i32);
    found.sort_by_key(|(a, _, _)| *a);

    let visible_len = visible.len() as i32;
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = from as i32;
    for (a, b, span) in found {
        let aa = (a - from as i32).clamp(0, visible_len);
        let cc = (cursor - from as i32).clamp(0, visible_len);
        if aa < cc {
            continue; // 重叠的实体跳过，别让下标回退
        }
        if aa > cc {
            spans.push(Span::Text {
                text: utf16_slice(&visible, cc as usize, aa as usize),
                bold: false,
                italic: false,
            });
        }
        spans.push(span);
        cursor = cursor.max(b);
    }
    if cursor < to as i32 {
        let start_at = (cursor - from as i32).clamp(0, visible_len) as usize;
        spans.push(Span::Text {
            text: utf16_slice(&visible, start_at, visible_len as usize),
            bold: false,
            italic: false,
        });
    }
    if spans.is_empty() {
        spans.push(Span::Text {
            text: utf16_slice(&visible, 0, visible_len as usize),
            bold: false,
            italic: false,
        });
    }

    let mut blocks: Vec<Block> = vec![Block::Paragraph { spans }];
    // 整条就是视频的推文（没有文字、只有视频）：视频块**不放正文**，详情页按约定放 FeedDetail.video。
    // 图片 / 卡片照旧留在正文里（"没有文字"只看 display_text_range 之内的文字）。
    let video_only = visible_blank(&visible) && media.iter().any(|m| m.is_video);
    for parsed in media {
        match parsed.block {
            Some(Block::Image { .. }) => blocks.push(parsed.block.clone().unwrap()),
            Some(Block::Video { .. }) => {
                if !video_only {
                    blocks.push(parsed.block.clone().unwrap());
                }
            }
            _ => {}
        }
    }
    if let Some(card) = parse_card(card) {
        blocks.push(card);
    }
    // `RichText.textOnly()`：段落 + 卡片块（图片 / 视频块被过滤掉），块之间用 `\n` 连
    let text_only = crate::detail::text_only(&blocks);
    BuiltBody {
        rich: RichText { blocks },
        text_only,
        visible_blank: visible_blank(&visible),
    }
}

/// `visible.isBlank()`（Kotlin 的空白口径）。
fn visible_blank(visible: &[u16]) -> bool {
    String::from_utf16_lossy(visible).trim().is_empty()
}

fn utf16_slice(u16s: &[u16], from: usize, to: usize) -> String {
    let from = from.min(u16s.len());
    let to = to.min(u16s.len()).max(from);
    String::from_utf16_lossy(&u16s[from..to])
}

fn indices(obj: &JsonMap) -> Option<(i32, i32)> {
    let arr = json::arr(obj, "indices")?;
    let a = arr.first().and_then(json_int)?;
    let b = arr.get(1).and_then(json_int)?;
    Some((a, b))
}

/// 对齐 Kotlin `JsonPrimitive.content.trim('"').toIntOrNull()`（`display_text_range` / `indices` 里
/// 数字偶尔写成字符串）。
fn json_int(v: &serde_json::Value) -> Option<i32> {
    match v {
        serde_json::Value::Number(n) => n.as_i64().map(|n| n as i32),
        serde_json::Value::String(s) => s.trim_matches('"').trim().parse::<i32>().ok(),
        _ => None,
    }
}

/// `card.legacy.binding_values` → 卡片标题（没有标题的卡片不是链接卡片，丢掉）。
fn parse_card_values(card: Option<&JsonMap>) -> Option<Vec<(String, String)>> {
    let legacy = json::obj(card?, "legacy")?;
    let values = json::arr(legacy, "binding_values")?;
    let mut out: Vec<(String, String)> = Vec::new();
    for e in values {
        let Some(o) = e.as_object() else { continue };
        let Some(key) = json::text(o, "key") else { continue };
        let Some(value) = json::obj(o, "value") else {
            continue;
        };
        let string_value = json::text(value, "string_value").or_else(|| {
            json::obj(value, "image_value")
                .and_then(|i| json::text(i, "url"))
                .map(|u| json::to_https(&u))
        });
        if let Some(text) = string_value {
            out.push((key, text));
        }
    }
    Some(out)
}

/// 卡片里某个键的值（同样的键取**后出现的**——Kotlin 用 `LinkedHashMap.put`）。
fn card_value<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .last()
}

/// `card.legacy.binding_values`（数组）→ [`Block::Card`]。没有 `title` 的卡片不是链接卡片，丢掉。
fn parse_card(card: Option<&JsonMap>) -> Option<Block> {
    let values = parse_card_values(card)?;
    let title = card_value(&values, "title")?;
    Some(Block::Card {
        title: title.to_string(),
        subtitle: card_value(&values, "description").map(|s| s.to_string()),
        cover: card_value(&values, "summary_photo_image_original")
            .or_else(|| card_value(&values, "thumbnail_image_original"))
            .or_else(|| card_value(&values, "photo_image_full_size_original"))
            .map(|s| s.to_string()),
        url: card_value(&values, "card_url")
            .or_else(|| card_value(&values, "vanity_url"))
            .map(|s| s.to_string()),
        target: None,
    })
}

// ---------- instructions / entries ----------

/// 一条时间线条目。`TimelineAddToModule` 里的推文被"包"成 `tweet-<id>` 的形状，所以这里
/// 用两个可选指针表示（不克隆整棵 JSON）。
struct Entry<'a> {
    id: String,
    /// `content`（会话模块 / 游标条目用）
    content: Option<&'a JsonMap>,
    /// `content.itemContent`（`TimelineAddToModule` 里那条推文）
    item_content: Option<&'a JsonMap>,
}

impl<'a> Entry<'a> {
    fn item_content(&self) -> Option<&'a JsonMap> {
        self.item_content
            .or_else(|| self.content.and_then(|c| json::obj(c, "itemContent")))
    }

    /// `entry.content.itemContent.tweet_results.result`
    fn tweet_result(&self) -> Option<&'a JsonMap> {
        self.item_content()
            .and_then(|c| json::obj(c, "tweet_results"))
            .and_then(|r| json::obj(r, "result"))
    }
}

fn add_entries(instructions: &[serde_json::Value]) -> Vec<Entry<'_>> {
    let mut out = Vec::new();
    for instruction in instructions {
        let Some(obj) = instruction.as_object() else {
            continue;
        };
        match json::string(obj, "type").as_deref() {
            Some("TimelineAddEntries") => {
                if let Some(entries) = json::arr(obj, "entries") {
                    for e in entries {
                        if let Some(entry) = e.as_object() {
                            out.push(Entry {
                                id: json::string(entry, "entryId").unwrap_or_default(),
                                content: json::obj(entry, "content"),
                                item_content: None,
                            });
                        }
                    }
                }
            }
            Some("TimelinePinEntry") => {
                if let Some(entry) = json::obj(obj, "entry") {
                    out.push(Entry {
                        id: json::string(entry, "entryId").unwrap_or_default(),
                        content: json::obj(entry, "content"),
                        item_content: None,
                    });
                }
            }
            Some("TimelineAddToModule") => {
                if let Some(items) = json::arr(obj, "moduleItems") {
                    for element in items {
                        // moduleItems[].item = { itemType, itemContent }：包成 tweet entry 的形状走同一条解析路
                        let item_content = element
                            .as_object()
                            .and_then(|e| json::obj(e, "item"))
                            .and_then(|i| json::obj(i, "itemContent"));
                        let Some(item_content) = item_content else {
                            continue;
                        };
                        if json::string(item_content, "itemType").as_deref() != Some("TimelineTweet") {
                            continue;
                        }
                        let result = json::obj(item_content, "tweet_results")
                            .and_then(|r| json::obj(r, "result"));
                        let Some(result) = result else { continue };
                        let id = json::obj(result, "tweet")
                            .and_then(|t| json::text(t, "rest_id"))
                            .or_else(|| json::text(result, "rest_id"));
                        let Some(id) = id else { continue };
                        out.push(Entry {
                            id: format!("tweet-{id}"),
                            content: None,
                            item_content: Some(item_content),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// 会话模块里每条 `TimelineTweet` 的 result（广告条目跳过）。
fn module_tweets<'a>(entry: &Entry<'a>) -> Vec<&'a JsonMap> {
    let Some(items) = entry.content.and_then(|c| json::arr(c, "items")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for element in items {
        let content = element
            .as_object()
            .and_then(|e| json::obj(e, "item"))
            .and_then(|i| json::obj(i, "itemContent"));
        let Some(content) = content else { continue };
        if json::string(content, "itemType").as_deref() != Some("TimelineTweet") {
            continue;
        }
        if json::obj(content, "promotedMetadata").is_some() {
            continue;
        }
        if let Some(result) = json::obj(content, "tweet_results")
            .and_then(|r| json::obj(r, "result"))
        {
            out.push(result);
        }
    }
    out
}

/// 广告：`entryId` 带 `promoted`，或 `itemContent.promotedMetadata` 存在。
fn is_promoted(entry: &Entry<'_>) -> bool {
    if entry.id.contains("promoted") {
        return true;
    }
    entry
        .item_content()
        .map(|c| json::obj(c, "promotedMetadata").is_some())
        .unwrap_or(false)
}

/// 下一页游标：`TimelineTerminateTimeline.direction == Bottom` 时**即使有 bottom 游标也返回 `None`**。
/// 游标三种形态都兼容（`content.value` / `content.operation.cursor.value` / `content.itemContent.value`）。
fn bottom_cursor(instructions: &[serde_json::Value], entries: &[Entry<'_>]) -> Option<String> {
    let ended = instructions.iter().any(|instruction| {
        let Some(obj) = instruction.as_object() else {
            return false;
        };
        json::string(obj, "type").as_deref() == Some("TimelineTerminateTimeline")
            && json::string(obj, "direction")
                .map(|d| d.eq_ignore_ascii_case("Bottom"))
                .unwrap_or(false)
    });
    let value = entries.iter().find_map(|entry| {
        if !(entry.id.starts_with("cursor-bottom") || entry.id.starts_with("cursor-showmore")) {
            return None;
        }
        let content = entry.content?;
        json::text(content, "value")
            .or_else(|| {
                json::obj(content, "operation")
                    .and_then(|o| json::obj(o, "cursor"))
                    .and_then(|c| json::text(c, "value"))
            })
            .or_else(|| json::obj(content, "itemContent").and_then(|c| json::text(c, "value")))
    });
    if ended {
        return None;
    }
    value.filter(|v| !v.trim().is_empty())
}

// ---------- 时间 ----------

/// X 的 `created_at`："Wed Oct 10 20:19:24 +0000 2012" → Unix 秒。解析不了就是 `None`（不编值）。
///
/// Kotlin 用的是 `SimpleDateFormat("EEE MMM dd HH:mm:ss Z yyyy", Locale.US)`，`Z` 会读进时区偏移，
/// 所以结果和本机时区无关——这里按同样的字段自己算。
pub fn parse_twitter_time(raw: Option<&str>) -> Option<i64> {
    let raw = raw?.trim();
    let parts: Vec<&str> = raw.split_whitespace().collect();
    // 星期、月、日、时分秒、时区、年
    if parts.len() < 6 {
        return None;
    }
    let month = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let day: i64 = parts[2].parse().ok()?;
    let mut hms = parts[3].split(':');
    let hh: i64 = hms.next()?.parse().ok()?;
    let mm: i64 = hms.next()?.parse().ok()?;
    let ss: i64 = hms.next()?.parse().ok()?;
    let tz = parts[4];
    let sign = match tz.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &tz[1..];
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let tz_hours: i64 = digits[0..2].parse().ok()?;
    let tz_minutes: i64 = digits[2..4].parse().ok()?;
    let offset = sign * (tz_hours * 3600 + tz_minutes * 60);
    let year: i64 = parts[5].parse().ok()?;
    if !(1..=9999).contains(&year) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// `YYYY-MM-DD` → 距 1970-01-01 的天数（Howard Hinnant 的 `days_from_civil`）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `String.ifBlank { null }`：空白（含全空白）当没有。
fn nonzero_text(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// 登录态：`whoami` 的 HTML 抠取

/// `x.com/home` 的 HTML → 当前登录的账号（对应 `XSource.whoami` 里那两条正则）。
///
/// Kotlin 是两个 `Regex.find`：`"user_id":"(\d+)"` 和 `"screen_name":"([^"]+)"`。这里不引 `regex`
/// crate（`.so` 里不多一个正则引擎），按 `find` 的语义手扫：**从左到右找字面量前缀**，找到了就吃
/// 后面**至少一个**字符（`\d+` 是贪婪、`[^"]+` 一直吃到下一个引号或字符串结尾），吃不到就接着往后找。
///
/// 抠不到 `user_id` 就是 `None`（Kotlin 的"页面结构变了"），`screen_name` 抠不到就退回 id 当名字
/// （`name = screen ?: id`），`url` 也只在有 screen_name 时给。
pub fn whoami_from_html(html: &str) -> Option<UserRef> {
    let id = first_capture(html, "\"user_id\":\"", |c| c.is_ascii_digit())?;
    let screen = first_capture(html, "\"screen_name\":\"", |c| c != '"');
    Some(UserRef {
        platform: X.to_string(),
        id: id.clone(),
        name: screen.clone().unwrap_or(id),
        avatar: None,
        url: screen.map(|name| format!("{WEB}/{name}")),
    })
}

/// `prefix` 后面第一段由 `take` 认的字符（至少一个）；整串扫完都没有就 `None`。
fn first_capture(html: &str, prefix: &str, take: impl Fn(char) -> bool) -> Option<String> {
    let mut from = 0usize;
    while let Some(offset) = html[from..].find(prefix) {
        let start = from + offset + prefix.len();
        let captured: String = html[start..].chars().take_while(|c| take(*c)).collect();
        if !captured.is_empty() {
            return Some(captured);
        }
        // 这个前缀后面没有一个字符符合条件：从下一个字符接着找（regex 的 find 也是这样往后挪的）
        from = start;
        if from >= html.len() {
            return None;
        }
        // 别在非字符边界上切（理论上 prefix 都是 ASCII，start 一定在边界上）
        while from < html.len() && !html.is_char_boundary(from) {
            from += 1;
        }
    }
    None
}
