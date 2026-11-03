//! B站搜索（`GET /x/web-interface/wbi/search/type`）的**平台层**：请求怎么拼（WBI 签名、参数顺序、
//! `Origin` / `Referer`）、翻页游标（页码）、状态码与 `{code}` 外壳的判定、结果怎么解析。
//! **收发仍然通过宿主**（[`HttpHost`] → OkHttp：TLS / Cookie / 代理 / fail-closed / 500ms 限速一个字节都不变）。
//!
//! 逐条对照 `source-bilibili/BilibiliSource.search` + `BiliSearchParser.kt`：
//! 方法 + 完整 URL（含参数顺序）+ 请求头顺序必须和 Kotlin 版**逐字节一样**，
//! `BiliSearchRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 几个口径（都是 Kotlin 那边定的，别"优化"）：
//!
//! - **只有视频**：`search_type` 永远是 `video`；
//! - **排序三档**：相关 `totalrank` / 最新 `pubdate` / 最热 `click`；
//! - **游标是页码**（从 1 起）；`numPages > 当前页` 才有下一页，**一条都没有时也不给游标**；
//! - 标题 / 简介里的 `<em class="keyword">` 高亮标签要去掉、`&amp;` 这类实体要还原、
//!   连续空白（空格 / 制表符 / 全角空格）压成一个——照 Kotlin 的 `BiliSearchParser.clean`；
//! - `pic` / `upic` 补 `https:`；`duration` 是 `"12:34"` 这种文字，转成秒；
//! - `url` 用 BV 号的**规范地址**（不是响应里的 `arcurl`，那是 av 号形态）；
//! - 412 是风控（`RiskControl`，"请求被拦截"），别的非 2xx 是 `Api`（body 前 200 字符），
//!   业务码按 `bili::check_envelope_object`（-352 / -412 / -509 / -799 / 22015 风控、-101 / -111 未登录、
//!   -403 签名、其它 Api）。**不重试、不换参数**。

use super::source::BiliHostedSource;
use crate::bili::{self, BILI};
use crate::json::{self, JsonMap};
use crate::model::{
    BiliError as BiliErr, DetailTarget, FeedItem, ItemKind, Media, Page, Stats, UserRef,
};
use crate::html::clean_search_text;

/// 搜索接口路径（WBI 签名按这个路径算）。
pub const SEARCH_PATH: &str = "/x/web-interface/wbi/search/type";

/// 搜索特有的两个请求头（`Origin` / `Referer` 指 search.bilibili.com）。
const SEARCH_ORIGIN: &str = "https://search.bilibili.com";
const SEARCH_REFERER: &str = "https://search.bilibili.com/";

/// 搜索结果的类型（Kotlin `SearchKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

/// 排序（Kotlin `SearchSort`）。B站 三档都有。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Relevance,
    Latest,
    Hot,
}

impl SearchSort {
    /// 排序档 → `order` 参数（Kotlin `BiliSearchParser.order`）。
    pub fn order(self) -> &'static str {
        match self {
            SearchSort::Relevance => "totalrank",
            SearchSort::Latest => "pubdate",
            SearchSort::Hot => "click",
        }
    }
}

impl SearchKind {
    /// 搜索类型 → `search_type`。
    pub fn search_type(self) -> Result<&'static str, BiliErr> {
        match self {
            SearchKind::All | SearchKind::Video | SearchKind::Post => Ok("video"),
            SearchKind::User | SearchKind::Topic => Err(BiliErr::Parse {
                detail: format!(
                    "暂不支持搜 {}（FeedItem 没有对应模型）",
                    match self {
                        SearchKind::User => "USER",
                        _ => "TOPIC",
                    }
                ),
            }),
        }
    }
}

/// 请求参数（照 Kotlin `BiliSearchParser.params`，顺序就是 URL 里的顺序；`wts` / `w_rid` 由签名追加）。
pub fn search_params(
    text: &str,
    kind: SearchKind,
    sort: SearchSort,
    page: i32,
    page_size: i32,
) -> Result<Vec<(String, String)>, BiliErr> {
    Ok(vec![
        ("keyword".to_string(), text.to_string()),
        ("search_type".to_string(), kind.search_type()?.to_string()),
        ("order".to_string(), sort.order().to_string()),
        ("page".to_string(), page.to_string()),
        ("page_size".to_string(), page_size.to_string()),
        ("platform".to_string(), "pc".to_string()),
        ("web_location".to_string(), "1430654".to_string()),
    ])
}

impl BiliHostedSource {
    /// 搜一页。`now_sec` 是 WBI 的 `wts` 和"今天"（WBI key 的跨天刷新）用的秒级时间戳，
    /// 由调用方注入；`cursor` 是页码（`None` / 不是数字 / 小于 1 都是第 1 页，和 Kotlin 的
    /// `toIntOrNull()?.coerceAtLeast(1) ?: 1` 一致）。
    ///
    /// 请求序列：`ensure_buvid`（没有 `buvid3` 时先领一次，和 Kotlin 一样）→ `nav`（WBI key 没缓存 /
    /// 跨天了才发）→ `search/type`。
    pub fn search(
        &self,
        text: &str,
        kind: SearchKind,
        sort: SearchSort,
        cursor: Option<&str>,
        page_size: i32,
        now_sec: i64,
    ) -> Result<Page, BiliErr> {
        // 类型不支持要在**发请求之前**报（Kotlin 那边也是 `params()` 里先抛）
        let params = search_params(text, kind, sort, page_of(cursor), page_size)?;
        self.ensure_buvid(now_sec)?;
        let url = self.signed_url(SEARCH_PATH, &params, now_sec)?;
        // 头顺序照 Kotlin：请求上先 `.header("Origin")` / `.header("Referer")`，然后 `DefaultHeadersInterceptor`
        // **只补缺失的同名头**（它按 User-Agent / Referer / Origin 的顺序遍历，Referer / Origin 已经有了就跳过），
        // 所以最终是 `Origin` → `Referer` → `User-Agent`。**不能直接 append `default_headers()`**：
        // 那样会多出两条重复的 Referer / Origin，请求字节就不一样了。
        let mut headers = vec![
            ("Origin".to_string(), SEARCH_ORIGIN.to_string()),
            ("Referer".to_string(), SEARCH_REFERER.to_string()),
        ];
        for (name, value) in super::source::default_headers() {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(&name)) {
                headers.push((name, value));
            }
        }
        let response = self
            .http
            .execute(crate::host::HttpRequest {
                method: "GET".to_string(),
                url,
                headers,
                body: Vec::new(),
            })
            .map_err(BiliErr::from)?;
        if matches!(response.status, 403 | 412 | 429) {
            return Err(BiliErr::RiskControl {
                code: Some(format!("HTTP {}", response.status)),
                detail: "请求被拦截".to_string(),
            });
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliErr::Api {
                code: format!("HTTP {}", response.status),
                detail: json::take_chars(&String::from_utf8_lossy(&response.body), 200),
            });
        }
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&body).map_err(|detail| BiliErr::Parse { detail })?;
        bili::check_envelope_object(&root)?;
        parse_search(&root, page_of(cursor))
    }
}

/// 游标 → 页码（Kotlin `cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1`）。
fn page_of(cursor: Option<&str>) -> i32 {
    cursor
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| v.max(1))
        .unwrap_or(1)
}

/// 搜索响应 → 一页。**纯函数**。
///
/// `{code, data:{page, numPages, result:[…]}}`：没有 `data` 就是 `ParseException`；
/// `result` 里没有 `bvid` 的条目丢掉；`numPages > page` 才给下一页游标。
pub fn parse_search(root: &JsonMap, page: i32) -> Result<Page, BiliErr> {
    let data = json::obj(root, "data").ok_or_else(|| BiliErr::Parse {
        detail: "搜索结果缺少 data".to_string(),
    })?;
    let items: Vec<FeedItem> = json::arr(data, "result")
        .map(|list| list.iter().filter_map(|el| el.as_object().and_then(search_video)).collect())
        .unwrap_or_default();
    let num_pages = json::int(data, "numPages").unwrap_or(0);
    let has_more = !items.is_empty() && num_pages > page;
    Ok(Page {
        items,
        next: if has_more {
            Some((page + 1).to_string())
        } else {
            None
        },
    })
}

/// 一条视频搜索结果。`title` / `description` 里的高亮标签去掉；`pic` / `upic` 补上 `https:`。
pub fn search_video(o: &JsonMap) -> Option<FeedItem> {
    let bvid = json::text(o, "bvid")?;
    let title = json::text(o, "title")
        .map(|t| clean(&t))
        .filter(|t| !t.trim().is_empty());
    let description = json::text(o, "description")
        .map(|t| clean(&t))
        .filter(|t| !t.trim().is_empty());
    let author = json::text(o, "author").filter(|t| !t.trim().is_empty());
    let mid = json::string(o, "mid");
    let duration = json::parse_duration(json::text(o, "duration").as_deref());
    Some(FeedItem {
        platform: BILI.to_string(),
        id: bvid.clone(),
        kind: ItemKind::Video,
        title,
        summary: description,
        language: None,
        author: author.map(|name| UserRef {
            platform: BILI.to_string(),
            id: mid.unwrap_or_default(),
            name,
            avatar: json::text(o, "upic").map(|u| json::to_https(&u)),
            url: None,
        }),
        published_at: json::long(o, "pubdate").filter(|t| *t > 0),
        published_at_precision: crate::model::TimePrecision::Second,
        media: vec![Media::Video {
            cover: json::text(o, "pic").map(|u| json::to_https(&u)),
            duration_sec: duration,
            width: None,
            height: None,
        }],
        stats: Stats {
            views: json::long(o, "play"),
            // `video_review` 是弹幕数（BiliPai 的字段名）；`review` 是评论数
            danmaku: json::long(o, "video_review"),
            comments: json::long(o, "review"),
            favorites: json::long(o, "favorites"),
            likes: json::long(o, "like"),
            ..Stats::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        // 统一用 bvid 的规范地址（响应里的 `arcurl` 是 av 号形态）
        url: Some(bili::video_url(&bvid)),
        detail: DetailTarget::Native {
            id: bvid,
            platform: None,
        },
    })
}

/// 去掉 `<em class="keyword">` 之类的高亮标签、还原实体，再把连续空白压成一个。
///
/// Kotlin `BiliSearchParser.clean` = `stripHtml(html).replace(Regex("[ \\t\\u3000]{2,}"), " ").trim()`
/// ——和贴吧 `TiebaSearch.cleanTitle` / `cleanText` **是同一行代码**，所以共用
/// [`crate::html::clean_search_text`]（搜索两期之后这已经是第三处调用方）。
pub fn clean(html: &str) -> String {
    clean_search_text(html)
}

