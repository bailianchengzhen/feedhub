//! **YouTube 的解析**（`source-youtube/YoutubeParser.kt` 里信息流那一半）+ 平台层（[`source`]）。
//!
//! 当前搬的是 TV `browse` 的**整条路**：
//! - 请求：InnerTube body（`context` / `browseId` / `continuation` / `tvAppInfo` … 的字段顺序）、
//!   请求头（含 `Authorization` 的位置）、`?prettyPrint=false`、状态码与风控判定 → [`source`]；
//! - 响应：`tileRenderer` / `videoRenderer` → `FeedItem`、续页游标（三级优先）→ 这里。
//!
//! **没有搬**：`search`、设备码登录、**取流**（`YoutubeCipher` + QuickJS 解 n/sig）。那些继续在 Kotlin
//! （`RustYoutubeSource` 逐方法委托）。
//!
//! **播放地址**那条线（解 n/sig）仍在 Kotlin。
//!
//! 逐条对照 `YoutubeParser.browse` / `collectItems` / `item` / `lineParts` / `textOf` / `viewCountOf` /
//! `nextContinuation` / `collectContinuations` / `continuationTokens`，以及 `core/Text.kt` 的
//! `parseRelativeTimePrecise`（`parseDuration` 用共享的 `json::parse_duration`）。**语义逐行对齐**，
//! 对拍（`RustYoutubeSourceTest`，仓库里所有 browse fixture + 手写边界样本）用 `FeedItemDiff` 守着。
//!
//! 两个和 Kotlin 不一样、但（在现有 fixture 上）不影响结果的地方：
//! 1. `serde_json::Map` 默认是 `BTreeMap`（键按字典序），Kotlin 的 `JsonObject` 是插入序。仓库里 6 份
//!    browse fixture 上"两种遍历顺序"的条目顺序和续页游标**完全相同**，
//!    但这是**已知的结构性差异**：如果哪天一个对象里同时挂了两个带 tile 的兄弟键，顺序可能不同。
//! 2. `is_blank` 按 `Character.isWhitespace` 的口径（**不含**不换行空格 U+00A0 / U+2007 / U+202F），
//!    不是 Rust 的 `char::is_whitespace`（它把这三个也算空白）。

pub mod account;
pub mod detail;
mod emotes;
// 播放取流：`player` 的 `adaptiveFormats` → `PlayInfo`，以及 `signatureCipher` / 节流 `n` 的拼接
// （JS 求解在宿主，见 `crate::host::ScriptHost`）。
pub mod play;
// 搜索：WEB 客户端的 InnerTube body + 结果解析。
pub mod search;
pub mod source;

use serde_json::Value;

use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

/// 平台 id。Kotlin 是 `Platform.define("yt", "YouTube")`——**是 `yt` 不是 `youtube`**（定了别改）。
pub const YOUTUBE: &str = "yt";

/// 网页地址前缀（`FeedItem.url` 用它；信息流和详情同一条内容的 key 必须一致：就是 videoId）。
pub const WATCH: &str = "https://www.youtube.com/watch?v=";

/// TV tile 里唯一当视频收的类型；`TILE_CONTENT_TYPE_PLAYLIST` 的榜单卡片丢掉。
const TILE_VIDEO: &str = "TILE_CONTENT_TYPE_VIDEO";

/// `engagementPanels` / `playerOverlays` 里的东西不是信息流条目；广告槽整块跳过（照 Kotlin `SKIP_KEYS`）。
const SKIP_KEYS: [&str; 7] = [
    "engagementPanels",
    "playerOverlays",
    "trackingParams",
    "clickTrackingParams",
    // 推荐流里夹的广告卡片（`adSlotRenderer` → inFeedAdLayoutRenderer → tileRenderer）
    "adSlotRenderer",
    "adLayoutMetadata",
    "inFeedAdLayoutRenderer",
];

/// Rust 侧的业务错误（带类型过 FFI，Kotlin 再映射回现有异常）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum YoutubeError {
    /// 要登录才有这一步。
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：429 / 403 / 412 / 空 body。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl { code: String, detail: String },

    /// 视频被平台标了年龄限制（18+）。**详情里不会往外抛**：`detail()` 自己把它转成
    /// `item.adult = true` 的隐藏详情（和 Kotlin `YoutubeSource.detail` 的 `catch (AgeRestrictedException)` 一样）；
    /// 留着这个变体是为了别在 `player` 那一层把它错当成"这个客户端失败了、换下一个"。
    #[error("年龄限制：{detail}")]
    AgeRestricted { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    /// 响应结构和预期不符 / 不是 JSON（文案照 Kotlin `parseJsonObject`）。
    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 宿主执行请求时失败：连不上 / 超时 / 连接被重置（Kotlin 侧是 `java.io.IOException`）。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：YouTube 要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常）。别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },
}

// ===========================================================================
// JSON 上的可空链（对齐 `YoutubeJson.kt` 的 `JsonElement?.field/array/str/intOf/longOf`）
// ===========================================================================

/// `(el as? JsonObject)?.get(key) as? JsonObject`（Kotlin 的 `field`）。
fn field<'a>(el: &'a Value, key: &str) -> Option<&'a JsonMap> {
    el.as_object()?.get(key)?.as_object()
}

/// 同上，只是接收者是 `Option<&Value>`（链上已经可能断掉的地方）。
fn field_opt<'a>(el: Option<&'a Value>, key: &str) -> Option<&'a JsonMap> {
    field(el?, key)
}

/// `(el as? JsonObject)?.text(key)`：非空白字符串。
fn text_key(el: &Value, key: &str) -> Option<String> {
    json::text(el.as_object()?, key)
}

fn int_key(el: &Value, key: &str) -> Option<i32> {
    json::int(el.as_object()?, key)
}

/// `JsonObject.text(key)`：非空白字符串。
fn text_of(map: &JsonMap, key: &str) -> Option<String> {
    json::text(map, key)
}

// ===========================================================================
// 文本工具（对齐 `core/Text.kt`；Java 的 `\s` / `isWhitespace` 口径）
// ===========================================================================

/// Java 正则 `\s`：只有这 6 个 ASCII 空白（**不是** Unicode 空白）。
fn is_ascii_regex_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// `Character.isWhitespace` 的口径：Unicode 空白**去掉** NBSP U+00A0 / U+2007 / U+202F。
pub fn is_java_whitespace(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{A0}' | '\u{2007}' | '\u{202F}')
}

/// Kotlin 的 `isBlank()`。
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_java_whitespace)
}

/// Kotlin 的 `trim()`（去掉两端的 `isWhitespace`）。
fn trim_java(s: &str) -> &str {
    s.trim_matches(is_java_whitespace)
}

/// [`text_of_element`] 的 `pub(crate)` 别名：同模块的 `youtube/search.rs` 也要按同一个口径取文字。
pub(crate) fn text_of_element_public(o: Option<&JsonMap>) -> Option<String> {
    text_of_element(o)
}

/// [`view_count_of`] 的 `pub(crate)` 别名（搜索的 `viewCountText` 也是 `"1.2B views"` 这种）。
pub(crate) fn view_count_of_public(text: &str) -> Option<i64> {
    view_count_of(text)
}

/// [`collect_continuations`] 的 `pub(crate)` 入口：返回**页面级 → tvSurface → 其他**三级合并后的游标
/// （顺序就是 `browse` 挑游标时的优先级）。搜索的续页也在 `continuationCommand.token` 里，
/// 所以和 `browse` 共用这一份扫描器，别各写一遍。
pub(crate) fn collect_continuations_public(root: &serde_json::Value) -> Vec<String> {
    let mut page: Vec<String> = Vec::new();
    let mut surface: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    collect_continuations(root, &mut page, &mut surface, &mut other);
    page.into_iter().chain(surface).chain(other).collect()
}

/// `{simpleText}` / `{runs:[{text}]}` / `{text}` / `{accessibility.accessibilityData.label}` / `{content}`
/// 五种形状都取（照 `YoutubeParser.textOf`，顺序也一样）。
fn text_of_element(o: Option<&JsonMap>) -> Option<String> {
    let o = o?;
    if let Some(t) = text_of(o, "simpleText") {
        return Some(t);
    }
    if let Some(runs) = json::arr(o, "runs") {
        let mut joined = String::new();
        for run in runs {
            // Kotlin: `it.field("text")?.str("text") ?: it.str("text").orEmpty()`
            let part = field_opt(run.as_object().and_then(|m| m.get("text")), "text")
                .and_then(|nested| text_of(nested, "text"))
                .or_else(|| text_key(run, "text"))
                .unwrap_or_default();
            joined.push_str(&part);
        }
        // 只有非空白才算数（`takeIf { it.isNotBlank() }`），否则继续往下试
        if !is_blank(&joined) {
            return Some(joined);
        }
    }
    if let Some(t) = text_of(o, "text") {
        return Some(t);
    }
    if let Some(label) =
        field_opt(o.get("accessibility"), "accessibilityData").and_then(|d| text_of(d, "label"))
    {
        return Some(label);
    }
    text_of(o, "content")
}

/// "1.2B views" / "1.1M views" / "12万次观看" → 数字。
///
/// 照 `YoutubeParser.viewCountOf`：`([\d.,]+)\s*([KMB])?\s*(views?|次观看|观看)`，忽略大小写，**取第一个匹配**
/// （第一个匹配的数字解析不出来就返回 `None`，不继续往后找）。没有 regex crate，这里手写扫描器，
/// 连"贪婪 + 回溯"的顺序都按 Java 正则的行为来：位数从多到少、每一步先试有 K/M/B 再试没有。
fn view_count_of(text: &str) -> Option<i64> {
    let chars: Vec<char> = text.chars().collect();
    let is_number_char = |c: char| c.is_ascii_digit() || c == '.' || c == ',';
    for start in 0..chars.len() {
        if !is_number_char(chars[start]) {
            continue;
        }
        let mut end = start;
        while end < chars.len() && is_number_char(chars[end]) {
            end += 1;
        }
        for len in (1..=end - start).rev() {
            let number: String = chars[start..start + len].iter().collect();
            // `([KMB])?` 贪婪：先把候选当成"有单位"，再当成"没有单位"
            let candidates: [(Option<char>, f64); 4] = [
                (Some('K'), 1_000.0),
                (Some('M'), 1_000_000.0),
                (Some('B'), 1_000_000_000.0),
                (None, 1.0),
            ];
            for (kind, multiplier) in candidates {
                let mut after = start + len;
                if let Some(kind) = kind {
                    match chars.get(after) {
                        Some(c) if c.eq_ignore_ascii_case(&kind) => after += 1,
                        _ => continue,
                    }
                }
                if view_suffix_at(&chars, after).is_none() {
                    continue;
                }
                // 匹配成功：数字解析不出来就是 None（Kotlin 直接 `return null`，不再往后找）
                let cleaned = number.replace(',', "");
                let n = cleaned.parse::<f64>().ok()?;
                return Some((n * multiplier).trunc() as i64);
            }
        }
    }
    None
}

/// 从 `at` 开始跳过 `\s*` 再看是不是 `views?` / `次观看` / `观看`（英文忽略大小写）。
fn view_suffix_at(chars: &[char], at: usize) -> Option<()> {
    let mut i = at;
    while i < chars.len() && is_ascii_regex_ws(chars[i]) {
        i += 1;
    }
    let words = ["views", "view"];
    for word in words {
        if starts_with_ignore_case(chars, i, &word.chars().collect::<Vec<_>>()) {
            return Some(());
        }
    }
    for word in ["次观看", "观看"] {
        if starts_with(chars, i, &word.chars().collect::<Vec<_>>()) {
            return Some(());
        }
    }
    None
}

fn starts_with(chars: &[char], at: usize, prefix: &[char]) -> bool {
    at + prefix.len() <= chars.len() && chars[at..at + prefix.len()] == *prefix
}

fn starts_with_ignore_case(chars: &[char], at: usize, prefix: &[char]) -> bool {
    at + prefix.len() <= chars.len()
        && chars[at..at + prefix.len()]
            .iter()
            .zip(prefix.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// 相对时间的换算结果：Unix 秒 + 它精确到哪一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativeTime {
    pub epoch_sec: i64,
    pub precision: TimePrecision,
}

/// 相对时间单位（照 Kotlin 正则里的 17 个，**顺序也一样**：`个月` 在 `月` 前、`分钟` 在 …）。
const UNITS: [(&str, i64, TimePrecision); 17] = [
    ("second", 1, TimePrecision::Second),
    ("minute", 60, TimePrecision::Minute),
    ("hour", 3_600, TimePrecision::Hour),
    ("day", 86_400, TimePrecision::Day),
    ("week", 604_800, TimePrecision::Day),
    ("month", 2_592_000, TimePrecision::Month),
    ("year", 31_536_000, TimePrecision::Year),
    ("秒", 1, TimePrecision::Second),
    ("分钟", 60, TimePrecision::Minute),
    ("小时", 3_600, TimePrecision::Hour),
    ("天", 86_400, TimePrecision::Day),
    ("日", 86_400, TimePrecision::Day),
    ("周", 604_800, TimePrecision::Day),
    ("星期", 604_800, TimePrecision::Day),
    ("个月", 2_592_000, TimePrecision::Month),
    ("月", 2_592_000, TimePrecision::Month),
    ("年", 31_536_000, TimePrecision::Year),
];

/// `core.Text.parseRelativeTimePrecise` 的移植：`"3 days ago"` → `now - 3×86400` + `DAY`。
///
/// 认不出来返回 `None`（`Preparing`、"Scheduled" 之类；**宁可这条没有发布时间，也不要编一个**）。
/// `now` 从外面注入（Kotlin 侧同样是参数），测试才能两边用同一个钟。
pub fn parse_relative_time_precise(text: Option<&str>, now: i64) -> Option<RelativeTime> {
    let raw = trim_java(text?);
    // Kotlin: `.lowercase().removePrefix("streamed").removePrefix("premiered").trim()`
    let lowered = raw.to_lowercase();
    let without_streamed = lowered.strip_prefix("streamed").unwrap_or(&lowered);
    let t = trim_java(
        without_streamed
            .strip_prefix("premiered")
            .unwrap_or(without_streamed),
    );
    if t.is_empty() {
        return None;
    }
    if t == "just now" || t == "刚刚" || t == "moments ago" {
        return Some(RelativeTime {
            epoch_sec: now,
            precision: TimePrecision::Second,
        });
    }
    match match_relative(t) {
        Some((n, secs, precision)) => Some(RelativeTime {
            epoch_sec: now.wrapping_sub(n.wrapping_mul(secs)),
            precision,
        }),
        None => {
            if t == "yesterday" || t == "昨天" {
                Some(RelativeTime {
                    epoch_sec: now.wrapping_sub(86_400),
                    precision: TimePrecision::Day,
                })
            } else {
                None
            }
        }
    }
}

/// 在 `t` 上找第一个 `\d+\s*(unit)s?\s*(ago|前)?`。数字溢出 i64 时 Kotlin 会 `return null`。
fn match_relative(t: &str) -> Option<(i64, i64, TimePrecision)> {
    let chars: Vec<char> = t.chars().collect();
    for start in 0..chars.len() {
        if !chars[start].is_ascii_digit() {
            continue;
        }
        let mut end = start;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        let digits: String = chars[start..end].iter().collect();
        let mut i = end;
        while i < chars.len() && is_ascii_regex_ws(chars[i]) {
            i += 1;
        }
        for (name, secs, precision) in UNITS {
            let unit: Vec<char> = name.chars().collect();
            if !starts_with(&chars, i, &unit) {
                continue;
            }
            // 数字溢出 → Kotlin `toLongOrNull() ?: return null`（整条返回 null，不再往后找）
            let n = digits.parse::<i64>().ok()?;
            // `s?`（英文复数）和 `\s*(ago|前)?` 都是可有可无的，匹配到单位就算数。
            return Some((n, secs, precision));
        }
    }
    None
}

// ===========================================================================
// 信息流（TV browse）
// ===========================================================================

/// 一页 `tileRenderer` / `videoRenderer` → `Page`。语义照 `YoutubeParser.browse`：
/// 先按顺序收条目（同一个 videoId 只留第一个），再挑第一个可用的续页游标（跳过当前游标自己）。
pub fn browse(json: &JsonMap, cursor: Option<&str>, now: i64) -> Page {
    let mut items: Vec<FeedItem> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    collect_items_in(json, &mut items, &mut seen, now);
    let mut page: Vec<String> = Vec::new();
    let mut surface: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    collect_continuations_in(json, &mut page, &mut surface, &mut other);
    let next = (page.into_iter())
        .chain(surface)
        .chain(other)
        .find(|token| !is_blank(token) && Some(token.as_str()) != cursor);
    Page { items, next }
}

fn push_unique(item: FeedItem, out: &mut Vec<FeedItem>, seen: &mut Vec<String>) {
    if seen.iter().any(|id| id == &item.id) {
        return;
    }
    seen.push(item.id.clone());
    out.push(item);
}

/// 递归找条目。见到已知的条目包装就收（**并且不再往下钻**），其余键继续下钻；`SKIP_KEYS` 跳过。
fn collect_items(el: &Value, out: &mut Vec<FeedItem>, seen: &mut Vec<String>, now: i64) {
    match el {
        Value::Array(list) => {
            for it in list {
                collect_items(it, out, seen, now);
            }
        }
        Value::Object(map) => collect_items_in(map, out, seen, now),
        _ => {}
    }
}

fn collect_items_in(map: &JsonMap, out: &mut Vec<FeedItem>, seen: &mut Vec<String>, now: i64) {
    let tile = map.get("tileRenderer").and_then(Value::as_object);
    let video = map
        .get("videoRenderer")
        .and_then(Value::as_object)
        .or_else(|| map.get("gridVideoRenderer").and_then(Value::as_object))
        .or_else(|| map.get("compactVideoRenderer").and_then(Value::as_object));
    if let Some(tile) = tile {
        if let Some(item) = item(tile, now) {
            push_unique(item, out, seen);
        }
    } else if let Some(video) = video {
        if let Some(item) = item(video, now) {
            push_unique(item, out, seen);
        }
    } else {
        for (k, v) in map {
            if SKIP_KEYS.contains(&k.as_str()) {
                continue;
            }
            collect_items(v, out, seen, now);
        }
    }
}

/// 一段文字行（`lines[].lineRenderer.items[].lineItemRenderer`）：文字或徽章（"4K"）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinePart {
    text: String,
    badge: Option<String>,
}

impl LinePart {
    /// "•" / "-" 这种分隔符不算内容。
    fn useful(&self) -> bool {
        !is_blank(&self.text) && self.text != "•" && self.text != "-"
    }
}

fn line_parts(meta: Option<&JsonMap>) -> Vec<LinePart> {
    let mut out = Vec::new();
    let Some(meta) = meta else { return out };
    let Some(lines) = json::arr(meta, "lines") else {
        return out;
    };
    for line in lines {
        let Some(line) = field(line, "lineRenderer") else {
            continue;
        };
        // 有的形状是 lineRenderer.title，有的是 items[]（`items.isNullOrEmpty()` 都走 title 那一支）
        match json::arr(line, "items") {
            Some(items) if !items.is_empty() => {
                for item in items {
                    let Some(li) = field(item, "lineItemRenderer") else {
                        continue;
                    };
                    let text = text_of_element(li.get("text").and_then(Value::as_object));
                    let badge = field_opt(li.get("badge"), "metadataBadgeRenderer")
                        .and_then(|r| text_of(r, "label"));
                    if text.is_none() && badge.is_none() {
                        continue;
                    }
                    out.push(LinePart {
                        text: text.unwrap_or_default(),
                        badge,
                    });
                }
            }
            _ => {
                if let Some(t) = text_of_element(line.get("title").and_then(Value::as_object)) {
                    out.push(LinePart {
                        text: t,
                        badge: None,
                    });
                }
            }
        }
    }
    out
}

/// 一个 `tileRenderer` / `videoRenderer` → `FeedItem`（照 `YoutubeParser.item`，字段位置逐条对齐）。
fn item(r: &JsonMap, now: i64) -> Option<FeedItem> {
    let content_type = text_of(r, "contentType");
    if let Some(t) = &content_type {
        if t != TILE_VIDEO {
            return None;
        }
    }
    let video_id = r
        .get("onSelectCommand")
        .and_then(|v| field(v, "watchEndpoint"))
        .and_then(|m| text_of(m, "videoId"))
        .or_else(|| text_of(r, "videoId").filter(|s| s.chars().count() == 11))
        .or_else(|| {
            text_of(r, "contentId")
                .filter(|s| content_type.as_deref() == Some(TILE_VIDEO) || s.chars().count() == 11)
        })?;
    let header = r.get("header").and_then(|v| field(v, "tileHeaderRenderer"));
    let overlays: &[Value] = header
        .and_then(|h| json::arr(h, "thumbnailOverlays"))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let meta = r
        .get("metadata")
        .and_then(|v| field(v, "tileMetadataRenderer"))
        .or_else(|| {
            overlays
                .iter()
                .find_map(|o| field(o, "tileMetadataRenderer"))
        });
    let thumbnail = header
        .and_then(|h| h.get("thumbnail"))
        .and_then(Value::as_object)
        .and_then(|t| json::arr(t, "thumbnails"))
        .and_then(|list| list.last());
    let duration = overlays.iter().find_map(|o| {
        let status = field(o, "thumbnailOverlayTimeStatusRenderer")?;
        let text = text_of_element(status.get("text").and_then(Value::as_object));
        json::parse_duration(text.as_deref())
    });
    let parts = line_parts(meta);
    let channel = parts
        .iter()
        .find(|p| p.badge.is_none() && p.useful())
        .map(|p| p.text.clone());
    let views = parts.iter().find_map(|p| view_count_of(&p.text));
    // 平台只给 "3 days ago" 这种相对文字：带上它的精度档，展示时按那一档截断（`core.TimeText`）
    let published = parts
        .iter()
        .find_map(|p| parse_relative_time_precise(Some(p.text.as_str()), now));
    Some(FeedItem {
        platform: YOUTUBE.to_string(),
        id: video_id.clone(),
        kind: ItemKind::Video,
        title: text_of_element(meta.and_then(|m| m.get("title")).and_then(Value::as_object)),
        summary: None,
        language: None,
        author: channel.map(|name| UserRef {
            platform: YOUTUBE.to_string(),
            id: name.clone(),
            name,
            avatar: None,
            url: None,
        }),
        published_at: published.as_ref().map(|p| p.epoch_sec),
        published_at_precision: published
            .as_ref()
            .map(|p| p.precision)
            .unwrap_or(TimePrecision::Second),
        media: vec![Media::Video {
            cover: thumbnail
                .and_then(|t| text_key(t, "url"))
                .map(|u| json::to_https(&u)),
            duration_sec: duration,
            width: thumbnail.and_then(|t| int_key(t, "width")),
            height: thumbnail.and_then(|t| int_key(t, "height")),
        }],
        stats: Stats {
            views,
            ..Stats::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: false,
        url: Some(format!("{WATCH}{video_id}")),
        detail: DetailTarget::Native {
            id: video_id,
            platform: None,
        },
    })
}

fn push_token(out: &mut Vec<String>, token: String) {
    if !out.iter().any(|t| t == &token) {
        out.push(token);
    }
}

fn collect_continuations(
    el: &Value,
    page: &mut Vec<String>,
    surface: &mut Vec<String>,
    other: &mut Vec<String>,
) {
    match el {
        Value::Array(list) => {
            for it in list {
                collect_continuations(it, page, surface, other);
            }
        }
        Value::Object(map) => collect_continuations_in(map, page, surface, other),
        _ => {}
    }
}

/// 第一个可用的续页游标（跳过当前游标自己，避免原地打转）。
///
/// **按层级挑，不是拿到哪个算哪个**：登录后的推荐流里，每个 shelf 自己也有一个
/// `horizontalListRenderer.continuations[]`，它排在页面级 `sectionListRenderer.continuations[]` 前面；
/// 挑错了会变成"只翻这一排"。优先级：页面级 → `tvSurfaceContentRenderer.continuation`（订阅流的频道标签页）
/// → 其他任何 `nextContinuationData` / `continuationCommand.token` / `continuation`。
fn collect_continuations_in(
    map: &JsonMap,
    page: &mut Vec<String>,
    surface: &mut Vec<String>,
    other: &mut Vec<String>,
) {
    for (k, v) in map {
        if SKIP_KEYS.contains(&k.as_str()) {
            continue;
        }
        if k == "sectionListRenderer" || k.ends_with("Continuation") {
            if let Some(list) = v.as_object().and_then(|m| json::arr(m, "continuations")) {
                for el in list {
                    continuation_tokens(el, page);
                }
            }
        } else if k == "tvSurfaceContentRenderer" {
            if let Some(t) = text_key(v, "continuation") {
                push_token(surface, t);
            }
        } else if k == "nextContinuationData" || k == "reloadContinuationData" {
            if let Some(t) = text_key(v, "continuation") {
                push_token(other, t);
            }
        } else if k == "continuationCommand" {
            // 注意：这里读的是 `token` 而不是 `continuation`
            if let Some(t) = text_key(v, "token") {
                push_token(other, t);
            }
        } else if k == "continuation" {
            if let Some(c) = v.as_str() {
                if !is_blank(c) {
                    push_token(other, c.to_string());
                }
            }
        }
        collect_continuations(v, page, surface, other);
    }
}

/// `continuations[].nextContinuationData.continuation`（也认元素自己带 `continuation` 的形状）。
fn continuation_tokens(el: &Value, out: &mut Vec<String>) {
    match el {
        Value::Array(list) => {
            for it in list {
                continuation_tokens(it, out);
            }
        }
        Value::Object(map) => {
            let token = text_of(map, "continuation").or_else(|| {
                map.get("nextContinuationData")
                    .and_then(Value::as_object)
                    .and_then(|nd| text_of(nd, "continuation"))
            });
            if let Some(t) = token {
                push_token(out, t);
            }
            for v in map.values() {
                continuation_tokens(v, out);
            }
        }
        _ => {}
    }
}

