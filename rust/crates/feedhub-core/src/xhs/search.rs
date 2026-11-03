//! 小红书搜索：`POST /api/sns/web/v1/search/notes`（v1，`edith.xiaohongshu.com`）。
//!
//! 这一家的搜索**从零开始**（没有 Kotlin 版可以逐字节对拍），所以口径写在下面，测试逐条断言：
//!
//! - **域名 / 路径**：`edith.xiaohongshu.com` + `/api/sns/web/v1/search/notes`。
//! 但那是**换域名**，
//! v1/edith 的 rap 哈希前缀（`//edith.xiaohongshu.com`）
//!   和我们现成的签名栈一致，而且 `redbook`/ `xhs-mcp`还在用。
//! - **body**：字段与顺序照浏览器实抓（`keyword` / `page` / `page_size` / `search_id` / `sort` / `note_type` /
//!   `ext_flags` / `geo` / `image_formats`），**不带 `filters`**（`Spider_XHS`：当前 PC 默认搜索体与浏览器一致不带）。
//! - **`search_id`**：客户端自己造，`base36((now_ms << 64) + random)`，同一轮搜索的所有页**复用同一个**
//!   （四处参考实现一致，见 `search_id`）。
//! - **翻页**：`page` 递增 + 同一个 `search_id`，看 `data.has_more`。游标是 `<search_id>|<下一页>`。
//! - **`x-rap-param`**：要带（`XhsApi.RAP_PATHS` 里两个搜索路径都写着）；`xy-direction` 不带（只有 homefeed / feed）。
//! - **详情**：搜索来的笔记 `xsec_source` 是 `pc_search`，id 是 `<笔记 id>@pc_search@<xsec_token>`。
//!
//! 解析只收内容卡片（`model_type == "note"`、有 `note_card`、`ignore` 不为真），用户 / 话题卡片丢掉
//! （`FeedItem` 没有对应模型，见 `core/Search.kt`）。

use crate::json::{self, JsonMap};
use crate::model::{BiliError, DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision};
use serde_json::Value;

use super::{check_http, emotes_used, id_time, note_url, native_id, user_of, XHS};

/// 搜索笔记的路径（v1，edith 域）。
pub const SEARCH_NOTES: &str = "/api/sns/web/v1/search/notes";

/// 一页多少条（网页实抓的 `page_size`）。
pub const PAGE_SIZE: i32 = 20;

/// 只做「综合」排序；能力里也只声明这一档。
pub const SORT_GENERAL: &str = "general";

/// 不限类型（视频 / 图文都收）。
pub const NOTE_TYPE_ALL: i32 = 0;

/// 从搜索结果点进笔记时网页用的 `xsec_source`。
pub const SOURCE_SEARCH: &str = "pc_search";

/// `base36` 的字母表：**大写**（`redbook` / `xhs-mcp` / `xiaohongshu-cli` 三家一致；
/// `Spider_XHS` 用小写，见文件头的说明）。
const ALPHABET36: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// 一轮搜索的 `search_id`：`base36((now_ms << 64) + random_part)`。
///
/// 时间 `now_ms` 和随机数 `random_part`（`1..=2147483646`）都是调用方给的（Rust 不读时钟、不取随机数），
/// 所以同一个 `(now_ms, random_part)` 一定得到同一个 id——`search_pad_len` 和真正的请求**必须**用同一对值，
/// 否则 `x-rap-param` 的补位长度会和实际 body 对不上（这和 homefeed 的补位长度是同一个道理）。
pub fn search_id(now_ms: i64, random_part: i64) -> String {
    let ts = now_ms.max(0) as u128;
    let random = random_part.clamp(0, 0x7fff_fffe) as u128;
    let mut num = (ts << 64) + random;
    if num == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while num > 0 {
        out.push(ALPHABET36[(num % 36) as usize]);
        num /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// 游标：`<search_id>|<下一页页码>`。
///
/// 解析不出来（空串 / 没有 `|` / 页码不是正整数）就当第一页：和 Kotlin 那边
/// `SearchCursor.decode` 一样"宽容"，不抛错（`Cursor` 是不透明字符串，别的入口也可能塞进来）。
pub fn parse_cursor(cursor: &str) -> (String, i32) {
    let (id, page) = match cursor.split_once('|') {
        Some((id, page)) => (id, page),
        None => (cursor, ""),
    };
    let page = page.parse::<i32>().ok().filter(|p| *p >= 1).unwrap_or(1);
    (id.to_string(), page)
}

/// 下一页的游标；`has_more` 是假就没有下一页。
pub fn next_cursor(search_id: &str, page: i32, has_more: bool) -> Option<String> {
    if has_more {
        Some(format!("{search_id}|{}", page.wrapping_add(1)))
    } else {
        None
    }
}

/// 搜索的请求体：字段与顺序照浏览器实抓（紧凑 JSON，签名和实际发出的是同一串）。
pub fn search_body(keyword: &str, page: i32, search_id: &str) -> String {
    format!(
        "{{\"keyword\":{},\"page\":{},\"page_size\":{PAGE_SIZE},\"search_id\":{},\
\"sort\":\"{SORT_GENERAL}\",\"note_type\":{NOTE_TYPE_ALL},\"ext_flags\":[],\"geo\":\"\",\
\"image_formats\":[\"jpg\",\"webp\",\"avif\"]}}",
        crate::hosted::json_string(keyword),
        page.max(1),
        crate::hosted::json_string(search_id)
    )
}

/// 搜索的一页：条目 + 游标 + 这一页所有卡片的标题拼串（Kotlin 拿它判断要不要去取表情表，和推荐流一样）。
#[derive(Debug)]
pub struct SearchPage {
    pub page: Page,
    pub titles: String,
}

/// `POST search/notes` 的响应：`data.{items[], has_more}`。
///
/// `page` 是这一页的页码（请求时的那个），`search_id` 是当前搜索的 id（下一页游标要用）。
pub fn parse_search(
    body: &str,
    http_code: i32,
    page: i32,
    search_id: &str,
    now_sec: i64,
    emotes: &[(String, String)],
) -> Result<SearchPage, BiliError> {
    let root = check_http(body, http_code)?;
    let data = json::obj(&root, "data");
    let empty: &[Value] = &[];
    let raw = data.and_then(|d| json::arr(d, "items")).map(|v| v.as_slice()).unwrap_or(empty);
    let items = raw
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| search_card(o, emotes, now_sec))
        .collect::<Vec<_>>();
    let titles = raw
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| json::obj(o, "note_card"))
        .filter_map(|c| json::text(c, "display_title").or_else(|| json::text(c, "title")))
        .collect::<Vec<_>>()
        .join("\n");
    let has_more = data.and_then(|d| json::bool(d, "has_more")) == Some(true);
    let next = next_cursor(search_id, page, has_more && !raw.is_empty());
    Ok(SearchPage {
        page: Page { items, next },
        titles,
    })
}

/// 搜索结果里的一张卡片。形状和推荐流的卡片一样（`model_type` / `note_card` / `xsec_token` / `ignore`），
/// 但标题字段在搜索里可能是 `display_title`、也可能只有 `title`（两个都认）。
fn search_card(item: &JsonMap, emotes: &[(String, String)], now_sec: i64) -> Option<FeedItem> {
    if let Some(t) = json::text(item, "model_type") {
        if t != "note" {
            return None;
        }
    }
    if json::bool(item, "ignore") == Some(true) {
        return None;
    }
    let id = json::text(item, "id")?;
    let card = json::obj(item, "note_card")?;
    let token = json::text(item, "xsec_token").filter(|t| !t.is_empty());
    let is_video = json::text(card, "type").as_deref() == Some("video");
    let title = json::text(card, "display_title").or_else(|| json::text(card, "title"));
    let cover = json::obj(card, "cover");
    let cover_url = cover
        .and_then(|c| {
            json::text(c, "url_default")
                .or_else(|| json::text(c, "url_pre"))
                .or_else(|| json::text(c, "url"))
        })
        .map(|u| json::to_https(&u));
    let width = cover.and_then(|c| json::int(c, "width"));
    let height = cover.and_then(|c| json::int(c, "height"));
    let mut media = Vec::new();
    if is_video {
        let duration = json::obj(card, "video")
            .and_then(|v| json::obj(v, "capa"))
            .and_then(|c| json::int(c, "duration"));
        media.push(Media::Video {
            cover: cover_url,
            duration_sec: duration,
            width,
            height,
        });
    } else if let Some(url) = cover_url {
        media.push(Media::Image { url, width, height });
    }
    let author = json::obj(card, "user").and_then(user_of);
    // 搜索卡片的 interact_info 比推荐流全，
    // 数字都是 "1.3万" 这种文本，统一走 trim_count + parse_count。
    let interact = json::obj(card, "interact_info");
    let count = |key: &str| {
        interact
            .and_then(|i| json::string(i, key))
            .map(|s| trim_count(&s))
            .and_then(|t| json::parse_count(Some(&t)))
    };
    // 搜索结果偶尔带发布时间（毫秒），没有就退回笔记 id 前 8 位（≤ 真实时间，差 0~1 天）
    let published_at = json::long(card, "time")
        .or_else(|| json::long(item, "time"))
        .filter(|t| *t > 0)
        .map(|t| t / 1000)
        .or_else(|| id_time(&id, now_sec));
    Some(FeedItem {
        platform: XHS.to_string(),
        id: id.clone(),
        kind: if is_video { ItemKind::Video } else { ItemKind::Post },
        title: title.clone(),
        summary: None,
        language: None,
        author,
        published_at,
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: count("liked_count"),
            comments: count("comment_count"),
            favorites: count("collected_count"),
            shares: count("shared_count"),
            ..Default::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: emotes_used(emotes, &[title.as_deref()]),
        adult: false,
        url: Some(note_url(&id, token.as_deref(), SOURCE_SEARCH)),
        detail: DetailTarget::Native {
            id: native_id(&id, SOURCE_SEARCH, token.as_deref()),
            platform: None,
        },
    })
}

/// `liked_count` 是 `"1.3万"` / `"4673"` / `"10万+"` 这种文本，去掉封顶写法里的 `+` 再交给 `parse_count`。
fn trim_count(s: &str) -> String {
    s.trim().trim_end_matches('+').to_string()
}

