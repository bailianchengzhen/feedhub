//! YouTube 搜索（`POST /youtubei/v1/search`）的**平台层**：请求怎么拼（**WEB 客户端**的 InnerTube body、
//! 字段顺序、请求头顺序）、翻页游标（`continuation`）、状态码与风控判定、结果怎么解析。
//! **收发仍然通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-youtube/YoutubeSource.search` + `YoutubeParser.search` / `searchVideo`：
//! 方法 + 完整 URL + body 逐字节 + 请求头顺序必须和 Kotlin 版**逐字节一样**，
//! `YoutubeSearchRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! ## 为什么是 WEB 客户端（不是 `browse` 的 TVHTML5）
//!
//! 但回的是新的 `lockupViewModel` 形状
//! （顶部先是频道卡片、然后是 cluster shelf），网页客户端才是经典的 `videoRenderer`
//! （标题 / 频道 / 相对时间 / 时长 / 观看数）。**`browse` 仍然是 TVHTML5**，只有搜索用 WEB。
//!
//! ## 搜索一律匿名
//!
//! InnerTube 匿名可搜。登录态是电视客户端的设备码 token，拿它配网页客户端发请求是自相矛盾的身份
//! （和"手机 WebView 换桌面 UA"同一个道理），所以**不带 `Authorization`**——即使有登录态。
//! 客户端健康冷却 / 回退链也不适用于搜索：只有一次请求，`429` / `403` / `412` / 空 body / "确认你不是机器人"
//! 一律由 [`super::source::check_transport`] 抛风控，**不重试、不换客户端**。
//!
//! ## 几个口径（照 Kotlin，别"优化"）
//!
//! - 第一页 body 是 `{context, query, params?, webpSupport:false, contentCheckOk:true, racyCheckOk:true}`，
//!   `params=EgIQAQ==` **只在 `kind == VIDEO` 时**带（综合不带）；
//! - 续页 body 只有 `{context, continuation}`（**别的字段一个都不带**，和 Kotlin 的 if/else 一致）；
//! - 只收 `videoRenderer`：`videoId` 必须 11 个字符；标题必须有；
//! - 频道 / 封面 / 观看数 / 相对时间从各自的位置读（见 [`search_video`]）；
//! - 续页游标用**和 `browse` 同一个** [`super::collect_continuations`] 三级优先级，并跳过当前游标自己
//!   （网页搜索的续页在 `sectionListRenderer.contents[].continuationItemRenderer.continuationCommand.token`
//!   里，**不在** `continuations[]`）。

use super::source::{check_transport, post_headers, Client, PATH_SEARCH};
use super::YOUTUBE;
use crate::host::HttpRequest;
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};
use crate::youtube::{
    collect_continuations_public, is_blank, parse_relative_time_precise, text_of_element_public,
    view_count_of_public, YoutubeError,
};
use crate::youtube::source::YoutubeHostedSource;

/// 搜索的类型（Kotlin `SearchKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

impl SearchKind {
    /// 搜索筛选：只有视频时带 `params=EgIQAQ==`；综合不带。
    ///
    /// 别的 `kind` 也一律不带（Kotlin 的 `when` 就是 `VIDEO -> … else -> null`），
    /// 能力里也没声明它们。
    pub fn params(self) -> Option<&'static str> {
        match self {
            SearchKind::Video => Some("EgIQAQ=="),
            _ => None,
        }
    }
}

/// 第一页的 body（字段顺序照 Kotlin 的 `buildJsonObject`，逐字节一样）。
fn first_page_body(context: &str, query: &str, kind: SearchKind) -> String {
    let mut s = String::with_capacity(512);
    s.push_str("{\"context\":");
    s.push_str(context);
    s.push_str(",\"query\":");
    super::source::push_quoted(&mut s, query);
    if let Some(params) = kind.params() {
        super::source::push_quoted_field(&mut s, "params", params);
    }
    s.push_str(",\"webpSupport\":false,\"contentCheckOk\":true,\"racyCheckOk\":true}");
    s
}

/// 续页的 body：**只有** `{context, continuation}`。
fn continuation_body(context: &str, cursor: &str) -> String {
    let mut s = String::with_capacity(256);
    s.push_str("{\"context\":");
    s.push_str(context);
    super::source::push_quoted_field(&mut s, "continuation", cursor);
    s.push('}');
    s
}

impl YoutubeHostedSource {
    /// 搜一页。`now_sec` 只给解析相对时间用（`"4 months ago"` → Unix 秒 + 精度档）；
    /// `cursor` 是上一页给的 `continuation`（`None` = 第一页）。
    ///
    /// **不带 `Authorization`**：搜索一律匿名（见模块说明）。
    pub fn search(
        &self,
        text: &str,
        kind: SearchKind,
        cursor: Option<&str>,
        now_sec: i64,
    ) -> Result<Page, YoutubeError> {
        let visitor = self.visitor_for_search();
        let context = super::source::context_json(Client::Web, visitor.as_deref());
        let body = match cursor {
            Some(cursor) => continuation_body(&context, cursor),
            None => first_page_body(&context, text, kind),
        };
        let map = self.post_search(&body, visitor.as_deref())?;
        Ok(parse_search(&map, cursor, now_sec))
    }

    /// 一次 `search` 的 POST（`?prettyPrint=false`、WEB 客户端的头、状态码判定、匿名身份回写）。
    fn post_search(
        &self,
        body: &str,
        visitor: Option<&str>,
    ) -> Result<JsonMap, YoutubeError> {
        let url = format!("{}{PATH_SEARCH}?prettyPrint=false", self.base().trim_end_matches('/'));
        let response = self
            .http_port()
            .execute(HttpRequest {
                method: "POST".to_string(),
                url,
                headers: post_headers(Client::Web, visitor, None),
                body: body.as_bytes().to_vec(),
            })
            .map_err(YoutubeError::from)?;
        let text = String::from_utf8_lossy(&response.body).into_owned();
        check_transport(response.status, &text)?;
        let map = json::parse_object(&text).map_err(|detail| YoutubeError::Parse { detail })?;
        self.remember_visitor_public(&map);
        Ok(map)
    }
}

/// 搜索响应 → 一页。**纯函数**。
///
/// 语义照 Kotlin `YoutubeParser.search`：先按出现顺序收 `videoRenderer`（同一个 videoId 只留第一个），
/// 再挑第一个可用的续页游标（跳过当前游标自己，避免原地打转）。
pub fn parse_search(root: &JsonMap, cursor: Option<&str>, now: i64) -> Page {
    let mut items: Vec<FeedItem> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    collect_search_videos(&serde_json::Value::Object(root.clone()), &mut items, &mut seen, now);
    let next = collect_continuations_public(&serde_json::Value::Object(root.clone()))
        .into_iter()
        .find(|token| !is_blank(token) && Some(token.as_str()) != cursor);
    Page { items, next }
}

/// 递归找 `videoRenderer`。见到它就收（**不再往下钻**），其余键继续下钻（跳过 `SKIP_KEYS`）。
fn collect_search_videos(
    el: &serde_json::Value,
    out: &mut Vec<FeedItem>,
    seen: &mut Vec<String>,
    now: i64,
) {
    match el {
        serde_json::Value::Array(list) => {
            for it in list {
                collect_search_videos(it, out, seen, now);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(renderer) = map.get("videoRenderer").and_then(|v| v.as_object()) {
                if let Some(item) = search_video(renderer, now) {
                    if !seen.iter().any(|id| id == &item.id) {
                        seen.push(item.id.clone());
                        out.push(item);
                    }
                }
                return;
            }
            for (key, value) in map {
                if super::SKIP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                collect_search_videos(value, out, seen, now);
            }
        }
        _ => {}
    }
}

/// 一个 `videoRenderer` → [`FeedItem`]（照 Kotlin `YoutubeParser.searchVideo`，字段位置逐条对齐）。
///
/// - `videoId` 必须正好 11 个字符，否则丢掉（Kotlin 的 `takeIf { it.length == 11 }`）；
/// - 标题必须有（`title` 的三种形状都认，见 [`text_of_element_public`]）；
/// - 频道名在 `ownerText`（`simpleText` 或 `runs[0].text`），频道 id 在
///   `ownerText.runs[0].navigationEndpoint.browseEndpoint.browseId`（拿不到就用名字）；
/// - 头像在 `channelThumbnailSupportedRenderers.channelThumbnailWithLinkRenderer.thumbnail.thumbnails[last]`；
/// - 封面取 `thumbnails[]` 里 `width` **最大**的那张；
/// - 发布时间的精度档跟着"相对文字"走（`"4 months ago"` → 月），不是秒；
/// - 摘要取 `detailedMetadataSnippets[0].snippetText.runs[].text` 拼起来再 trim。
pub fn search_video(r: &JsonMap, now: i64) -> Option<FeedItem> {
    let video_id = json::string(r, "videoId").filter(|s| s.chars().count() == 11)?;
    let title = text_of_element_public(json::obj(r, "title"))?;
    let owner = json::obj(r, "ownerText");
    let channel = owner.and_then(|o| {
        json::string(o, "simpleText").or_else(|| {
            json::arr(o, "runs")
                .and_then(|list| list.first())
                .and_then(|run| json::string(run.as_object()?, "text"))
        })
    });
    let channel_id = owner
        .and_then(|o| json::arr(o, "runs"))
        .and_then(|list| list.first())
        .and_then(|run| json::obj(run.as_object()?, "navigationEndpoint"))
        .and_then(|end| json::obj(end, "browseEndpoint"))
        .and_then(|browse| json::string(browse, "browseId"));
    let avatar = json::obj(r, "channelThumbnailSupportedRenderers")
        .and_then(|o| json::obj(o, "channelThumbnailWithLinkRenderer"))
        .and_then(|o| json::obj(o, "thumbnail"))
        .and_then(|o| json::arr(o, "thumbnails"))
        .and_then(|list| list.last())
        .and_then(|el| json::string(el.as_object()?, "url"))
        .map(|u| json::to_https(&u));
    let published = parse_relative_time_precise(
        text_of_element_public(json::obj(r, "publishedTimeText")).as_deref(),
        now,
    );
    let thumbnails = json::obj(r, "thumbnail")
        .and_then(|o| json::arr(o, "thumbnails"))
        .map(|list| list.as_slice())
        .unwrap_or(&[]);
    let thumb = thumbnails
        .iter()
        .filter_map(|el| el.as_object())
        .max_by_key(|o| json::int(o, "width").unwrap_or(0));
    let snippet = json::arr(r, "detailedMetadataSnippets")
        .and_then(|list| list.first())
        .and_then(|el| json::obj(el.as_object()?, "snippetText"))
        .and_then(|o| json::arr(o, "runs"))
        .map(|runs| {
            runs.iter()
                .map(|run| {
                    run.as_object()
                        .and_then(|o| json::string(o, "text"))
                        .unwrap_or_default()
                })
                .collect::<String>()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(FeedItem {
        platform: YOUTUBE.to_string(),
        id: video_id.clone(),
        kind: ItemKind::Video,
        title: Some(title),
        summary: snippet,
        language: None,
        author: channel.map(|name| UserRef {
            platform: YOUTUBE.to_string(),
            id: channel_id.unwrap_or_else(|| name.clone()),
            name,
            avatar,
            url: None,
        }),
        published_at: published.as_ref().map(|p| p.epoch_sec),
        published_at_precision: published
            .as_ref()
            .map(|p| p.precision)
            .unwrap_or(TimePrecision::Second),
        media: vec![Media::Video {
            cover: thumb
                .and_then(|t| json::string(t, "url"))
                .map(|u| json::to_https(&u)),
            duration_sec: json::parse_duration(
                text_of_element_public(json::obj(r, "lengthText")).as_deref(),
            ),
            width: thumb.and_then(|t| json::int(t, "width")),
            height: thumb.and_then(|t| json::int(t, "height")),
        }],
        stats: Stats {
            views: text_of_element_public(json::obj(r, "viewCountText"))
                .and_then(|text| view_count_of_public(&text)),
            ..Stats::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        url: Some(format!("{}{video_id}", crate::youtube::WATCH)),
        detail: DetailTarget::Native {
            id: video_id,
            platform: None,
        },
    })
}

/// 平台 id（测试里断言"Rust 侧记住的就是这个平台"）。
pub const PLATFORM: &str = YOUTUBE;

