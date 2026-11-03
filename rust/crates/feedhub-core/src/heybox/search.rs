//! 小黑盒搜索（`GET /bbs/app/api/general/search/v1`）的**平台层**：请求怎么拼（URL / 参数顺序 /
//! `sort_filter` / 翻页游标）、结果怎么解析、错误怎么判。**收发仍然通过宿主**（[`HttpHost`] →
//! OkHttp：TLS / Cookie / 代理 / fail-closed / 600ms 限速一个字节都不变），这里不碰 socket。
//!
//! 接口和参数照 xiaoheihe-cli 的 `xhh_operator/service.py:289-303`：
//! `q`、`search_type=link`、`offset`、`limit`，再补网页公共参数和 `hkey`（[`super::sign`]）。
//!
//! - 响应是 `{status, msg, result:{items:[{type, report_id, info}], filter_list, …}}`。
//!   `type` 有 `link`（帖子，`info` 就是 `link/tree` 的 `link_data` 那一套字段）、`space`（用户卡）、
//!   `relevant_search`（相关搜索词）。**只收 `link`**，
//! - `offset` 是**帖子结果的绝对下标**（不是页号、也不是原始 item 数）：拿 30 条就 +30。
//!   在线验过：`create_date` 排序下 `offset=30` 的首页正好接着 `offset=0` 的末条（中间新发了 1 条帖子，
//!   所以有一条边界重复；聚合层按 key 去重）。
//! - 服务端**不按 `limit` 截**：`limit=10` / `limit=20` 都回 30 条 link。
//!   所以一页的条数按**响应里实际的 link 条数**算（[`SEARCH_LIMIT`] 只是请求口径，和别的平台一样）。
//! - 默认排序（相关）每次请求的尾部会变（同一组参数连发两次，前 4 条一样、后 2 条换了），
//!   所以"相关"翻页本来就可能重 / 漏；`sort_filter=create_date`（最新）是稳定的（严格按 `create_at` 递减）。
//! - 搜索结果标题 / 摘要里**有** `[cube_惊讶]` 这样的表情代码，所以和推荐流一样按需取表情表
//!   （[`HeyboxHostedSource::emote_table`]，和推荐流共用内存 / `ctx.store` 缓存）。

use super::source::HeyboxHostedSource;
use super::{contains_emote_token, link, HeyboxError};
use crate::json::{self, JsonMap};
use crate::model::Page;

/// 搜索接口路径（`HeyboxSource.API` 之后的部分）。签名按这个路径算。
pub const SEARCH_PATH: &str = "/bbs/app/api/general/search/v1";

/// 搜索结果里 `type` 是帖子的那一种。
const LINK_TYPE: &str = "link";

/// 请求的 `limit`：照 xiaoheihe-cli 的默认值 20（服务端实际会回 30 条 link，见模块注释）。
///
/// `SearchCapability.pageSize`（Kotlin 侧）也是这个数：它只是**请求口径**，一页实际多少条按响应算。
pub const SEARCH_LIMIT: i32 = 20;

/// 搜索排序。接口没有独立的"相关度"参数：不传 `sort_filter` 就是默认（相关）。
///
/// `filter_list` 里还列了 `award_num`（最多点赞）/ `comment_num`（最多评论）/ `time_range`，
/// 当前**不声明**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    /// 相关（默认）：不传 `sort_filter`。
    Relevance,
    /// 最新：`sort_filter=create_date`。
    Latest,
}

impl SearchSort {
    /// 对应 `filter_list[].filters[].key`；相关＝不传。
    pub fn sort_filter(self) -> Option<&'static str> {
        match self {
            SearchSort::Relevance => None,
            SearchSort::Latest => Some("create_date"),
        }
    }
}

/// 一页搜索的 query 参数。
pub fn search_params(text: &str, sort: SearchSort, offset: i32) -> Vec<(String, String)> {
    let mut params = vec![
        ("q".to_string(), text.to_string()),
        ("search_type".to_string(), LINK_TYPE.to_string()),
    ];
    if let Some(filter) = sort.sort_filter() {
        params.push(("sort_filter".to_string(), filter.to_string()));
    }
    params.push(("offset".to_string(), offset.to_string()));
    params.push(("limit".to_string(), SEARCH_LIMIT.to_string()));
    params
}

impl HeyboxHostedSource {
    /// 搜一页。
    ///
    /// `cursor` 是帖子的绝对下标（`offset` 的十进制写法；`null` / 不是数字都是第 1 页——
    /// 和推荐流的宽容口径一样）；`now_sec` / `nonce` 从外面注入（Rust 不读时钟、不取随机数）。
    ///
    /// 请求序列：`general/search/v1` 一次；标题 / 摘要里有表情代码时再取一次 `emojis/list`
    /// （内存 / `ctx.store` 命中就不发）。
    pub fn search(
        &self,
        text: &str,
        sort: SearchSort,
        cursor: Option<&str>,
        now_sec: i64,
        nonce: &str,
    ) -> Result<Page, HeyboxError> {
        let offset = cursor.and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let params = search_params(text, sort, offset);
        // 树已经查过 status 了（`get_both` 的 `require_ok`），直接拿它解析
        let root = self.get_both(SEARCH_PATH, &params, true, now_sec, nonce)?;
        let texts = search_texts(&root);
        let emotes = if contains_emote_token(&texts) {
            self.emote_table(now_sec, nonce)?
        } else {
            Vec::new()
        };
        parse_search_object(&root, offset, &emotes)
    }
}

/// 这一页里所有帖子（`type=link`）的标题和摘要拼在一起：有 `[cube_惊讶]` 这样的表情代码才去取表情表
/// （和推荐流的 `feed_texts` 同一个口径，只是数据在 `result.items[].info` 里）。
pub fn search_texts(root: &JsonMap) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(result) = json::obj(root, "result") {
        for v in json::arr(result, "items")
            .map(|v| v.as_slice())
            .unwrap_or_default()
        {
            let Some(o) = v.as_object() else { continue };
            if json::string(o, "type").as_deref() != Some(LINK_TYPE) {
                continue;
            }
            let Some(info) = json::obj(o, "info") else {
                continue;
            };
            let mut lines: Vec<String> = Vec::new();
            if let Some(t) = json::string(info, "title") {
                lines.push(t);
            }
            if let Some(d) = json::string(info, "description") {
                lines.push(d);
            }
            parts.push(lines.join("\n"));
        }
    }
    parts.join("\n")
}

/// 搜索响应 → 一页条目。
///
/// 只收 `type=link` 的 `info`，解析**直接复用推荐流的 [`link`]**（真实响应里 `info` 和 `feeds` 的
/// `links[]` 是同一套字段：`linkid` / `title` / `description` / `create_at` / `user` / `topics` /
/// `imgs` / `thumbs` / `has_video` / 计数……）。`space`（用户卡）和 `relevant_search`（相关搜索词）跳过。
///
/// 游标 = `offset + 这一页的 link 条数`（**原始条数**，包括被 [`link`] 跳过的广告 / 空卡片）；
/// 一条 link 都没有时 `next = None`（到底了）。
pub fn parse_search_object(
    root: &JsonMap,
    offset: i32,
    emotes: &[(String, String)],
) -> Result<Page, HeyboxError> {
    let items = json::path(root, &["result"])
        .and_then(|r| json::arr(r, "items"))
        .ok_or_else(|| HeyboxError::parse("搜索结果缺少 result.items"))?;
    let mut link_count: i32 = 0;
    let mut out = Vec::new();
    for item in items {
        let Some(o) = item.as_object() else { continue };
        if json::string(o, "type").as_deref() != Some(LINK_TYPE) {
            continue;
        }
        // 原始条数按 type=link 算（哪怕 info 缺了 / 被当成广告跳掉，它也占了服务端的一个位置）
        link_count += 1;
        let Some(info) = json::obj(o, "info") else {
            continue;
        };
        if let Some(feed) = link(info, emotes) {
            out.push(feed);
        }
    }
    let next = if link_count == 0 {
        None
    } else {
        // Kotlin 的 `offset + links.size` 是 Int 加法（溢出会绕），这里照做
        Some(offset.wrapping_add(link_count).to_string())
    };
    Ok(Page { items: out, next })
}
