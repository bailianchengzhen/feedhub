//! 知乎搜索（`GET /api/v4/search_v3`）的**平台层**：请求怎么拼（`x-zse-96` 签名、参数顺序、
//! 没 `d_c0` 时先 `POST /udid`）、翻页游标（`paging.next` 的 query，原样带上）、状态码 / `Location` /
//! 错误体的判定、结果怎么解析。**收发仍然通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-zhihu/ZhihuSource.search` + `ZhihuSearch.kt` + `ZhihuParser.search`：
//! 方法 + 完整 URL + 请求头顺序必须和 Kotlin 版**逐字节一样**，
//! `ZhihuSearchRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 几个口径（都是 Kotlin 那边定的，别"优化"）：
//!
//! - **翻页不自己递增 offset**：第一页用固定参数，之后把上一页 `paging.next` 里 `?` 之后的 query
//!   **原样**接上（自己递增 offset 没有出处）。路径和域名固定，免得游标被改成别的地址把 Cookie 带出去；
//! - **只有"相关"**：在线实测 `sort=upvoted_count` 被服务端忽略（返回顺序和相关一样），
//!   所以插件能力里只声明了 `RELEVANCE`；这里 `Latest` / `Hot` 仍然照 Kotlin 的映射发
//!   （`created_time` / `upvoted_count`），只是聚合层不会传进来——**能力声明和请求映射是两件事**，
//!   映射留在这一层，行为和 Kotlin 一模一样；
//! - **`kind == VIDEO`** 才加 `vertical=zvideo` + `vertical_info=0,0,…`；
//! - **查询串按 OkHttp 解析 URL 的口径规范化**（`hosted::okhttp_canonical_query`），和第一页那份
//!   手写 query 走的不是同一个编码器——第一页是 Kotlin 拼好的字符串，游标是 OkHttp 解析出来的；
//! - `data[]` 里只收 `type == "search_result"` 的卡片（`people` / `topic` / `anniversary_activity` /
//!   `hot_timing` 丢掉），`object` 复用推荐流的 `target_item`；
//! - 标题 / 摘要里的 `<em>` 高亮去掉（**连 `object.question.title` 也带**），按 `id` 去重；
//! - `is_end` 为 true 或没有 `next` 就是最后一页（最后一页 `next` 仍然有值，指回起点，所以先看 `is_end`）。

use super::source::ZhihuHostedSource;
use crate::zhihu::{self, ZhihuError, ZHIHU};
use crate::json::{self, JsonMap};
use crate::model::{FeedItem, Page};

/// 搜索接口路径（`ZhihuSource.SEARCH_PATH`）。
pub const SEARCH_PATH: &str = "/api/v4/search_v3";

/// 搜索结果里唯一收的卡片 `type`。
const SEARCH_RESULT_TYPE: &str = "search_result";

/// 搜索的类型（Kotlin `SearchKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

/// 排序（Kotlin `SearchSort`）。**能力里只声明了 `Relevance`**（`sort=upvoted_count` 在线被忽略），
/// 但映射照 Kotlin 抄：`Latest` → `created_time`，`Hot` → `upvoted_count`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Relevance,
    Latest,
    Hot,
}

impl SearchSort {
    /// 排序档 → `sort` 参数；相关是默认，**不带这个参数**。
    pub fn param(self) -> Option<&'static str> {
        match self {
            SearchSort::Relevance => None,
            SearchSort::Latest => Some("created_time"),
            SearchSort::Hot => Some("upvoted_count"),
        }
    }
}

/// 第一页的 query（照 Kotlin `ZhihuSearch.firstQuery`，顺序也是它）。
///
/// 值是**原样拼**的（不做 URL 编码）：`q` 是用户输入，Kotlin 那边也是 `"q=${request.text}"` 直接接上，
/// 之后由 `toHttpUrl()` 解析时统一规范化。两边都这样，请求字节才一样。
pub fn first_query(text: &str, kind: SearchKind, sort: SearchSort, page_size: i32) -> String {
    let mut parts: Vec<(&str, String)> = vec![
        ("gk_version", "gz-gaokao".to_string()),
        ("t", "general".to_string()),
        ("q", text.to_string()),
        ("correction", "1".to_string()),
        ("offset", "0".to_string()),
        ("limit", page_size.to_string()),
        ("search_source", "Normal".to_string()),
        ("show_all_topics", "0".to_string()),
        ("include", "data[*].highlight,object,type".to_string()),
    ];
    if kind == SearchKind::Video {
        parts.push(("vertical", "zvideo".to_string()));
        parts.push(("vertical_info", "0,0,0,0,0,0,0,0,0,0".to_string()));
    }
    if let Some(value) = sort.param() {
        parts.push(("sort", value.to_string()));
    }
    parts
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

impl ZhihuHostedSource {
    /// 搜一页。`seed` 是 `x-zse-96` 的明文首字节（0..=255），由调用方注入；
    /// `cursor` 是上一页 `paging.next` 的 query（`None` = 第一页）。
    ///
    /// 请求序列和 Kotlin 一样：`ensure_dc0`（没有 `d_c0` 时先 `POST /udid`）→ `search_v3`。
    /// 风控（403 / 429 / 4035x / 跳到 unhuman）由 [`zhihu::check_error`] 映射，**不重试**。
    pub fn search(
        &self,
        text: &str,
        kind: SearchKind,
        sort: SearchSort,
        cursor: Option<&str>,
        page_size: i32,
        seed: u8,
    ) -> Result<Page, ZhihuError> {
        // 两页的 query 都按 OkHttp 解析 URL 的口径规范化：Kotlin 那边是
        // `"$base$SEARCH_PATH?$query".toHttpUrl()`，规范化是 OkHttp 自己做的
        // （游标是上一页 `paging.next` 的 query，第一页是 `first_query` 手拼的串，两条路都要过一遍）。
        let raw_query = match cursor {
            Some(cursor) if !cursor.is_empty() => cursor.to_string(),
            _ => first_query(text, kind, sort, page_size),
        };
        let query = crate::hosted::okhttp_canonical_query(&raw_query);
        let path_and_query = format!("{SEARCH_PATH}?{query}");
        let url = format!("{}{path_and_query}", self.base());
        self.ensure_dc0()?;
        let response = self.get(&url, &path_and_query, seed)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let location = crate::hosted::header_value(&response.headers, "location").map(str::to_string);
        let root = zhihu::check_http(response.status as i32, &body, location.as_deref())?;
        parse_search(&root, &self.platform_emotes())
    }
}

/// 搜索响应 → 一页。**纯函数**。
pub fn parse_search(root: &JsonMap, emotes: &[(String, String)]) -> Result<Page, ZhihuError> {
    let data = json::arr(root, "data").ok_or_else(|| ZhihuError::Parse {
        detail: "搜索结果缺少 data".to_string(),
    })?;
    let mut items: Vec<FeedItem> = Vec::new();
    for el in data {
        let Some(o) = el.as_object() else { continue };
        if json::string(o, "type").as_deref() != Some(SEARCH_RESULT_TYPE) {
            continue;
        }
        let Some(object) = json::obj(o, "object") else {
            continue;
        };
        let Some(item) = zhihu::target_item(object, emotes) else {
            continue;
        };
        // 搜索结果的标题里带 `<em>` 高亮（包括 `object.question.title`），统一去掉
        let title = item.title.map(|t| crate::html::strip_html(&t)).filter(|t| !t.is_empty());
        let summary = item
            .summary
            .map(|s| crate::html::strip_html(&s))
            .filter(|s| !s.is_empty());
        items.push(FeedItem {
            title,
            summary,
            ..item
        });
    }
    // Kotlin `distinctBy { it.id }`：保留第一次出现的那条
    let mut seen = std::collections::HashSet::new();
    items.retain(|it| seen.insert(it.id.clone()));
    Ok(Page {
        items,
        next: next_cursor(root, "搜索结果")?,
    })
}

/// 游标是 `paging.next` 里 `?` 之后的部分（路径和域名固定）。`is_end` 或没有 next 就是最后一页。
fn next_cursor(root: &JsonMap, what: &str) -> Result<Option<String>, ZhihuError> {
    let paging = json::obj(root, "paging").ok_or_else(|| ZhihuError::Parse {
        detail: format!("{what} 缺少 paging"),
    })?;
    if json::bool(paging, "is_end") == Some(true) {
        return Ok(None);
    }
    Ok(json::text(paging, "next")
        .and_then(|next| next.split_once('?').map(|(_, q)| q.to_string()))
        .filter(|q| !q.is_empty()))
}

/// 平台 id（测试里断言"Rust 侧记住的就是这个平台"）。
pub const PLATFORM: &str = ZHIHU;

