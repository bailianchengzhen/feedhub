//! 酷安搜索（`GET /v6/search`）—— `CoolapkSearch.kt` 的 Rust 版。
//!
//! 三件事，和 Kotlin 一一对应：
//! - [`params`]：一次搜索的查询参数（**顺序照 Kotlin 的 `buildList`**：`type, feedType, sort,
//!   searchValue, page, lastItem?, showAnonymous`）；
//! - [`decode_cursor`] / [`encode`]：游标是 `page|lastItem`（`page` 从 1 开始，`lastItem` 是**上一页
//!   最后一条内容**的 id）——和列表那套 `page=N&firstItem=…&lastItem=…` **不是**同一个格式，别混；
//! - [`parse`]：响应 `{data:[实体]}` 和信息流同构，只留 `entityType == "feed"` 的，卡片 / 用户 / 话题丢掉；
//!   解析复用信息流的 [`super::feed`]。
//!
//! 撞上 `err_request_captcha*` 时**不弹验证、不重试**：那一步在
//! [`super::check_status`] 里，和别的接口共用同一份判定。
//!
//! 参数照 c001apk（`ApiService.kt:61-73`）和 Coolapk-UWP（`UriHelper.cs:198-204`）。

use super::emoji::EmoteTable;
use super::{id, CoolapkError};
use crate::hosted::is_kotlin_blank;
use crate::json;
use crate::model::{FeedItem, Page};

/// 搜什么排序（Kotlin `SearchSort`）。酷安三档都支持：综合 / 最新 / 最热。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    /// 综合（`sort=none`，缺省）。
    Relevance,
    /// 最新。
    Latest,
    /// 最热。
    Hot,
}

impl SearchSort {
    /// 排序档 → `sort` 参数值（Kotlin `CoolapkSearch.sortOf`）。
    pub fn param(self) -> &'static str {
        match self {
            SearchSort::Relevance => "none",
            SearchSort::Latest => "dateline",
            SearchSort::Hot => "hot",
        }
    }
}

/// 一页的查询参数。**顺序和 Kotlin 逐字一样**（请求等价门盯着）。
///
/// `last_item` 只在"翻页且非空白"时出现（Kotlin `lastItem?.takeIf { it.isNotBlank() }`），
/// 而且排在 `page` **之后**、`showAnonymous` 之前。
pub fn params(
    text: &str,
    sort: SearchSort,
    page: i64,
    last_item: Option<&str>,
) -> Vec<(String, String)> {
    let mut out = vec![
        ("type".to_string(), "feed".to_string()),
        ("feedType".to_string(), "all".to_string()),
        ("sort".to_string(), sort.param().to_string()),
        ("searchValue".to_string(), text.to_string()),
        ("page".to_string(), page.to_string()),
    ];
    if let Some(last) = last_item.filter(|v| !is_kotlin_blank(v)) {
        out.push(("lastItem".to_string(), last.to_string()));
    }
    out.push(("showAnonymous".to_string(), "-1".to_string()));
    out
}

/// 游标 → `(page, lastItem)`（Kotlin `CoolapkSearch.decode`）。
///
/// **解不出来就当第一页（不抛错）**：`'|'` 之前的部分不是数字 → 第 1 页；没有 `'|'` → 没有 `lastItem`。
pub fn decode_cursor(cursor: Option<&str>) -> (i64, Option<String>) {
    let Some(text) = cursor else {
        return (1, None);
    };
    let (page_text, last) = match text.split_once('|') {
        Some((p, rest)) => (p, rest),
        None => (text, ""),
    };
    let page = page_text
        .parse::<i64>()
        .ok()
        .map(|n| n.max(1))
        .unwrap_or(1);
    (page, if last.is_empty() { None } else { Some(last.to_string()) })
}

/// `(页码, 上一页最后一条的 id)` → 游标字符串（Kotlin `CoolapkSearch.encode`）。
pub fn encode(page: i64, last_item: &str) -> String {
    format!("{page}|{last_item}")
}

/// 一页搜索结果。`page` 是**当前**页（下一页游标要用它 +1）。
///
/// 到头了就是 `next = None`：这一页一条内容都没有，或者一条都取不出 id（和 Kotlin 一样）。
pub fn parse(
    json: &json::JsonMap,
    page: i64,
    emotes: &EmoteTable,
) -> Result<Page, CoolapkError> {
    let data = json::arr(json, "data").ok_or_else(|| CoolapkError::Parse {
        detail: "搜索结果缺少 data 数组".to_string(),
    })?;
    let feeds: Vec<&json::JsonMap> = data
        .iter()
        .filter_map(|v| v.as_object())
        .filter(|o| json::string(o, "entityType").as_deref() == Some("feed"))
        .collect();
    let items: Vec<FeedItem> = feeds.iter().filter_map(|o| super::feed(o, emotes, 0)).collect();
    let last = feeds.iter().filter_map(|o| id(o)).next_back();
    let next = match last {
        Some(last) if !items.is_empty() => Some(encode(page + 1, &last)),
        _ => None,
    };
    Ok(Page { items, next })
}

/// 一个搜索响应（body + HTTP 状态码）→ 一页。
///
/// 错误映射（业务码 / 状态码 / `err_request_captcha*` → 风控）在 [`crate::coolapk::decode_json`] 里，
/// 和 Kotlin `CoolapkSource.getJson` 的尾巴一字不差；平台对象和纯函数入口都走这一条。
///
/// `emotes` 收的是"原文 → 图片地址"的原始表（和 [`super::parse_feed`] 一个形状），表在这里现包一次。
pub fn parse_response(
    body: &str,
    http_code: i32,
    page: i64,
    emotes: &[(String, String)],
) -> Result<Page, CoolapkError> {
    let json = crate::coolapk::decode_json(body, http_code)?;
    parse(&json, page, &EmoteTable::new(emotes))
}

