//! **YouTube 的详情 / 评论 / 楼中楼解析**（照 `source-youtube/YoutubeParser.kt` 逐行移植）。
//!
//! 三个入口：
//! - [`video`] + [`video_meta`] + [`merge_meta`] + [`item_of`]：详情（`player` 的 `videoDetails` 为准，
//!   TV `next {videoId}` 里的视频页元数据只补空的，头像 / 发布时间只有 `next` 有）；
//! - [`comments_token`]：`next {videoId}` 的响应里评论面板的续页 token（Top / Newest 各一个）；
//! - [`comments`]：一页评论 / 楼中楼（`continuationContents.itemSectionContinuation`），顺带把每条评论的
//!   楼中楼 token（`detailViewEndpoint`）交回去——`Comment` 模型里没地方放它，游标又得由调用方给。
//!
//! 表情短码在 [`super::emotes`]（两张随插件带的表），日期解析（"Published on Oct 25, 2009"）也在这一层。
//!
//! 和 Kotlin 逐条对齐、容易写错的地方：
//! - `player` 的 `videoDetails` 现在是**新形状**（登录态 TV 只剩 `videoId` / `lengthSeconds` / `thumbnail`…），
//!   所以 [`merge_meta`] 的规则是"`player` 为准、`next` 只补空"，[`fill_missing`] 反过来"已有的绝不被覆盖"；
//! - 相对时间（"16y ago"）优先读 `accessibility.accessibilityData.label`（"16 years ago"），绝对日期优先；
//! - 评论的 `contentText.runs[]` 里 `emoji` run 和纯文字 run 混着来，纯文字里的短码还要再过一遍表。

use serde_json::Value;

use crate::detail::{Block, Comment, CommentPage, FeedDetail, RichText, Span, VideoInfo, VideoPart};
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision, UserRef};

use super::emotes;
use super::{
    is_ascii_regex_ws, is_blank, parse_relative_time_precise, text_of, text_of_element, trim_java,
    view_count_of, WATCH, YOUTUBE,
};
use crate::bili::source::CommentSort;

/// YouTube 只有一条流：播放 / 弹幕 / `VideoPart.id` 一律用它（Kotlin `YoutubeSource.MAIN_PART`）。
pub const MAIN_PART: &str = "main";

/// 年龄限制的关键词（照 `YoutubeParser.AGE_GATE_WORDS`，顺序也一样）。
const AGE_GATE_WORDS: [&str; 5] = [
    "confirm your age",
    "age-restricted",
    "inappropriate",
    "age_verification_required",
    "age_check_required",
];

/// Java `DateTimeFormatter` 的英文月份短名（`MMM`，`Locale.ENGLISH`）。
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// ===========================================================================
// player
// ===========================================================================

/// 年龄限制的判断（照 yt-dlp `_is_agegated`）：`desktopLegacyAgeGateReason` 或 reason / status 里的关键词。
pub fn is_age_restricted(json: &JsonMap) -> bool {
    let Some(ps) = json::obj(json, "playabilityStatus") else {
        return false;
    };
    if json::obj(ps, "desktopLegacyAgeGateReason").is_some() {
        return true;
    }
    let mut reasons: Vec<String> = Vec::new();
    if let Some(status) = text_of(ps, "status") {
        reasons.push(status);
    }
    if let Some(reason) = text_of(ps, "reason") {
        reasons.push(reason);
    }
    let joined = reasons.join(" ").to_lowercase();
    AGE_GATE_WORDS.iter().any(|w| joined.contains(w))
}

/// `player` 响应里详情要用的那部分（照 `YoutubeParser.video`；**`adaptiveFormats` 不解析**——
/// 取流那条线还在 Kotlin，这一层只要 `videoDetails` / 可播性 / 年龄标记）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct YoutubeVideo {
    pub video_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub channel_id: Option<String>,
    pub description: Option<String>,
    pub duration_sec: Option<i32>,
    pub view_count: Option<i64>,
    pub cover: Option<String>,
    pub cover_width: Option<i32>,
    pub cover_height: Option<i32>,
    pub live: bool,
    pub status: String,
    pub reason: Option<String>,
    /// 平台标了 18+ / 年龄限制（详情页据此转成 `item.adult = true` 的隐藏详情）。
    pub age_restricted: bool,
}

/// `player` 响应 → [`YoutubeVideo`]（字段位置逐条照 `YoutubeParser.video`）。
pub fn video(json: &JsonMap) -> YoutubeVideo {
    let details = json::obj(json, "videoDetails");
    let thumbs: &[Value] = details
        .and_then(|d| json::obj(d, "thumbnail"))
        .and_then(|t| json::arr(t, "thumbnails"))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let last = thumbs.last();
    let cover = last
        .and_then(Value::as_object)
        .and_then(|t| json::text(t, "url"))
        .map(|u| json::to_https(&u));
    let playability = json::obj(json, "playabilityStatus");
    YoutubeVideo {
        // `details?.str("videoId") ?: json.text("videoId").orEmpty()`
        video_id: details
            .and_then(|d| text_of(d, "videoId"))
            .or_else(|| text_of(json, "videoId"))
            .unwrap_or_default(),
        title: details.and_then(|d| text_of(d, "title")),
        author: details.and_then(|d| text_of(d, "author")),
        channel_id: details.and_then(|d| text_of(d, "channelId")),
        description: details.and_then(|d| text_of(d, "shortDescription")),
        duration_sec: details
            .and_then(|d| json::long(d, "lengthSeconds"))
            .map(|n| n as i32),
        view_count: details.and_then(|d| json::long(d, "viewCount")),
        cover,
        cover_width: last
            .and_then(Value::as_object)
            .and_then(|t| json::int(t, "width")),
        cover_height: last
            .and_then(Value::as_object)
            .and_then(|t| json::int(t, "height")),
        live: details.and_then(|d| text_of(d, "isLiveContent")).as_deref() == Some("true"),
        status: playability
            .and_then(|p| text_of(p, "status"))
            .unwrap_or_default(),
        reason: playability.and_then(|p| text_of(p, "reason")),
        age_restricted: is_age_restricted(json),
    }
}

// ===========================================================================
// TV `next {videoId}` 里的视频页元数据
// ===========================================================================

/// TV `next {videoId}` 里的视频页元数据（`YoutubeParser.videoMeta`）。
///
/// 它是详情页**唯一**的头像 / 发布时间来源，也是登录态 TV `player` 缺
/// `title` / `author` / `shortDescription` / `viewCount` 时的补位。
#[derive(Debug, Clone, PartialEq)]
pub struct YoutubeVideoMeta {
    pub video_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub channel_id: Option<String>,
    /// `/@handle` 形式的主页后缀（去掉开头的 `/`），没有 channelId 时拿它当作者 id / 主页地址。
    pub channel_handle: Option<String>,
    pub avatar: Option<String>,
    /// 简介**全文**（`structuredDescriptionContentRenderer` 里那段，不是摘要）。
    pub description: Option<String>,
    pub view_count: Option<i64>,
    pub published_at: Option<i64>,
    pub published_at_precision: TimePrecision,
}

/// `next {videoId}` → [`YoutubeVideoMeta`]；三个来源全都没有就是 `None`（调用方降级）。
pub fn video_meta(json: &JsonMap, now: i64) -> Option<YoutubeVideoMeta> {
    let metadata = metadata_renderer(json);
    let header = description_header(json);
    let channel = description_channel(json);
    if metadata.is_none() && header.is_none() && channel.is_none() {
        return None;
    }
    let owner = metadata
        .and_then(|m| json::obj(m, "owner"))
        .and_then(|o| json::obj(o, "videoOwnerRenderer"));
    let published_text = metadata
        .and_then(|m| json::obj(m, "publishedTimeText"))
        .and_then(|m| text_of_element(Some(m)))
        .or_else(|| {
            header
                .and_then(|h| json::obj(h, "publishDate"))
                .and_then(|d| text_of_element(Some(d)))
        });
    let date_text = metadata.and_then(|m| json::obj(m, "dateText"));
    // 绝对日期优先（"Published on Oct 25, 2009" → 只到天），没有就用 `dateText` 的相对说法
    let absolute = published_date(published_text.as_deref());
    let relative = if absolute.is_none() {
        let label = date_text
            .and_then(|d| json::obj(d, "accessibility"))
            .and_then(|a| json::obj(a, "accessibilityData"))
            .and_then(|a| text_of(a, "label"));
        let text = label.or_else(|| date_text.and_then(|d| text_of_element(Some(d))));
        parse_relative_time_precise(text.as_deref(), now)
    } else {
        None
    };
    let view_count_text = metadata
        .and_then(|m| json::obj(m, "viewCount"))
        .and_then(|v| json::obj(v, "videoViewCountRenderer"))
        .and_then(|v| json::obj(v, "viewCount"))
        .and_then(|v| text_of_element(Some(v)))
        .or_else(|| {
            header
                .and_then(|h| json::obj(h, "views"))
                .and_then(|v| text_of_element(Some(v)))
        });
    let avatar = last_thumbnail(owner)
        .or_else(|| last_thumbnail(channel))
        .map(|u| json::to_https(&u));
    Some(YoutubeVideoMeta {
        video_id: metadata
            .and_then(|m| text_of(m, "videoId"))
            .or_else(|| {
                json::obj(json, "currentVideoEndpoint")
                    .and_then(|e| json::obj(e, "watchEndpoint"))
                    .and_then(|w| text_of(w, "videoId"))
            }),
        title: metadata
            .and_then(|m| json::obj(m, "title"))
            .and_then(|t| text_of_element(Some(t)))
            .or_else(|| {
                header
                    .and_then(|h| json::obj(h, "title"))
                    .and_then(|t| text_of_element(Some(t)))
            }),
        author: owner
            .and_then(|o| json::obj(o, "title"))
            .and_then(|t| text_of_element(Some(t)))
            .or_else(|| {
                header
                    .and_then(|h| json::obj(h, "channel"))
                    .and_then(|c| text_of_element(Some(c)))
            })
            .or_else(|| {
                channel
                    .and_then(|c| json::obj(c, "title"))
                    .and_then(|t| text_of_element(Some(t)))
            }),
        channel_id: channel_id_of(owner).or_else(|| channel_id_of(channel)),
        channel_handle: handle_of(owner).or_else(|| handle_of(channel)),
        avatar,
        description: description_body(json),
        view_count: view_count_text.as_deref().and_then(view_count_of),
        published_at: absolute.or_else(|| relative.map(|r| r.epoch_sec)),
        published_at_precision: if absolute.is_some() {
            TimePrecision::Day
        } else {
            relative.map(|r| r.precision).unwrap_or(TimePrecision::Second)
        },
    })
}

/// **合并规则：`player` 为准，`next` 只补空**（`YoutubeParser.mergeMeta`）。
///
/// `player.videoDetails` 是视频本身的权威字段（`author` 是完整频道名、`shortDescription` 是作者原文）；
/// TV 的 `videoMetadataRenderer` 里 `mayTruncateChannelName: true`，长频道名可能被截断。
pub fn merge_meta(video: YoutubeVideo, meta: Option<&YoutubeVideoMeta>) -> YoutubeVideo {
    let Some(meta) = meta else { return video };
    YoutubeVideo {
        title: non_blank(video.title).or_else(|| meta.title.clone()),
        author: non_blank(video.author).or_else(|| meta.author.clone()),
        channel_id: video.channel_id.or_else(|| meta.channel_id.clone()),
        description: non_blank(video.description).or_else(|| meta.description.clone()),
        view_count: video.view_count.or(meta.view_count),
        ..video
    }
}

/// 用一个**兜底响应**（另一条路的 `player`）补齐 `video` 缺的字段（`YoutubeParser.fillMissing`）：
/// 已有的字段绝不被覆盖（同一份视频，先拿到的那份算数）。
pub fn fill_missing(video: YoutubeVideo, fallback: &YoutubeVideo) -> YoutubeVideo {
    YoutubeVideo {
        title: non_blank(video.title).or_else(|| fallback.title.clone()),
        author: non_blank(video.author).or_else(|| fallback.author.clone()),
        channel_id: video.channel_id.clone().or_else(|| fallback.channel_id.clone()),
        description: non_blank(video.description)
            .or_else(|| non_blank(fallback.description.clone())),
        view_count: video.view_count.or(fallback.view_count),
        cover: video.cover.clone().or_else(|| fallback.cover.clone()),
        ..video
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !is_blank(v))
}

/// `videoMetadataRenderer`（TV watch next 的头部元数据）。
fn metadata_renderer(json: &JsonMap) -> Option<&JsonMap> {
    let contents: &[Value] = json::obj(json, "contents")
        .and_then(|c| json::obj(c, "singleColumnWatchNextResults"))
        .and_then(|c| json::obj(c, "results"))
        .and_then(|c| json::obj(c, "results"))
        .and_then(|c| json::arr(c, "contents"))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    for c in contents {
        let Some(section) = c
            .get("itemSectionRenderer")
            .and_then(Value::as_object)
            .and_then(|s| json::arr(s, "contents"))
        else {
            continue;
        };
        for item in section {
            if let Some(found) = item
                .get("videoMetadataRenderer")
                .and_then(Value::as_object)
            {
                return Some(found);
            }
        }
    }
    // 续页 / 别的包装层也认一下（同一份响应里名字固定，但别依赖它的位置）
    find_renderer_in(json, "videoMetadataRenderer")
}

/// 简介面板的 `videoDescriptionHeaderRenderer`（标题 / 频道 / 观看数 / 绝对日期）。
fn description_header(json: &JsonMap) -> Option<&JsonMap> {
    description_items(json)
        .into_iter()
        .find_map(|item| json::obj(item, "videoDescriptionHeaderRenderer"))
}

/// 简介面板的 `videoDescriptionChannelSectionRenderer.channel.videoOwnerRenderer`。
fn description_channel(json: &JsonMap) -> Option<&JsonMap> {
    description_items(json).into_iter().find_map(|item| {
        json::obj(item, "videoDescriptionChannelSectionRenderer")
            .and_then(|s| json::obj(s, "channel"))
            .and_then(|c| json::obj(c, "videoOwnerRenderer"))
    })
}

/// `engagementPanels[].content.structuredDescriptionContentRenderer.items[]`（第一个有这一段的 panel）。
fn description_items(json: &JsonMap) -> Vec<&JsonMap> {
    let panels = json::arr(json, "engagementPanels").map(|v| v.as_slice()).unwrap_or(&[]);
    let items = panels.iter().find_map(|panel| {
        panel
            .get("engagementPanelSectionListRenderer")
            .and_then(Value::as_object)
            .and_then(|p| json::obj(p, "content"))
            .and_then(|c| json::obj(c, "structuredDescriptionContentRenderer"))
            .and_then(|c| json::arr(c, "items"))
    });
    items
        .map(|list| list.iter().filter_map(|i| i.as_object()).collect())
        .unwrap_or_default()
}

/// 简介**全文**：`descriptionBodyText.runs[].text`。表情 run 没有 `text`，退回它的文字代码；
/// `None` = 这个视频没有简介。
fn description_body(json: &JsonMap) -> Option<String> {
    let body = description_items(json).into_iter().find_map(|item| {
        json::obj(item, "expandableVideoDescriptionBodyRenderer")
    });
    let body = body.or_else(|| find_renderer_in(json, "expandableVideoDescriptionBodyRenderer"));
    let runs = body
        .and_then(|b| json::obj(b, "descriptionBodyText"))
        .and_then(|t| json::arr(t, "runs"))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let mut text = String::new();
    for run in runs {
        if let Some(t) = run.as_object().and_then(|m| json::text(m, "text")) {
            text.push_str(&t);
            continue;
        }
        // `run.field("emoji")?.array("shortCodes")?.firstOrNull()` 的 `(it as? JsonPrimitive)?.content`
        if let Some(short) = run
            .get("emoji")
            .and_then(Value::as_object)
            .and_then(|e| json::arr(e, "shortCodes"))
            .and_then(|list| list.first())
            .and_then(json::primitive_content)
        {
            text.push_str(&short);
        }
    }
    if is_blank(&text) {
        None
    } else {
        Some(text)
    }
}

/// 递归找某个 renderer（Kotlin `findRenderer`：先看直接子键，再按顺序递归子树）。
fn find_renderer<'a>(el: &'a Value, name: &str) -> Option<&'a JsonMap> {
    match el {
        Value::Object(map) => find_renderer_in(map, name),
        Value::Array(list) => list.iter().find_map(|v| find_renderer(v, name)),
        _ => None,
    }
}

/// 同上，接收者已经是对象（省掉一次把整份响应当成 [`Value`] 的包装）。
fn find_renderer_in<'a>(map: &'a JsonMap, name: &str) -> Option<&'a JsonMap> {
    if let Some(found) = map.get(name).and_then(Value::as_object) {
        return Some(found);
    }
    map.values().find_map(|v| find_renderer(v, name))
}

fn channel_id_of(owner: Option<&JsonMap>) -> Option<String> {
    owner
        .and_then(|o| json::obj(o, "navigationEndpoint"))
        .and_then(|e| json::obj(e, "browseEndpoint"))
        .and_then(|b| text_of(b, "browseId"))
}

/// `/@RickAstleyYT` → `@RickAstleyYT`（Kotlin `removePrefix("/")`：**只去一个** `/`）。
fn handle_of(owner: Option<&JsonMap>) -> Option<String> {
    let raw = owner
        .and_then(|o| json::obj(o, "navigationEndpoint"))
        .and_then(|e| json::obj(e, "browseEndpoint"))
        .and_then(|b| text_of(b, "canonicalBaseUrl"))?;
    let tail = raw.strip_prefix('/').unwrap_or(&raw);
    tail.starts_with('@').then(|| tail.to_string())
}

/// `thumbnail.thumbnails[].url` 的最后一个（非空白）。
fn last_thumbnail(owner: Option<&JsonMap>) -> Option<String> {
    owner.and_then(|o| last_thumbnail_in(o, "thumbnail"))
}

/// `<key>.thumbnails[].url` 的最后一个（非空白）。评论是 `authorThumbnail`，视频 owner 是 `thumbnail`。
fn last_thumbnail_in(map: &JsonMap, key: &str) -> Option<String> {
    json::obj(map, key)
        .and_then(|t| json::arr(t, "thumbnails"))
        .and_then(|list| {
            list.iter()
                .filter_map(|t| t.as_object())
                .filter_map(|t| json::text(t, "url"))
                .last()
        })
}

/// "Published on Oct 25, 2009" / "Streamed live on Jun 5, 2021" → 那一天对应的 Unix 秒
/// （只到"天"，展示按 [`TimePrecision::Day`] 截断）。
///
/// **取当天 12:00 UTC**，不是 00:00：平台给的是"日期"没有钟点，而展示按**本机时区**取日期；
/// 存 00:00 UTC 在西半球会显示成前一天。
///
/// 正则 `([A-Z][a-z]{2,8})\.?\s+(\d{1,2}),?\s+(\d{4})` 照 Java 的**贪婪 + 回溯**来（见 [`scan_published`]）：
/// 找到的第一个匹配如果月份 / 日期不合法（`LocalDate.parse` 会抛），**整条返回 `None`**，不再往后找。
pub fn published_date(text: Option<&str>) -> Option<i64> {
    let text = text?;
    let (month, day, year) = scan_published(text)?;
    let month = MONTHS.iter().position(|m| *m == month)? as u32 + 1;
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + 12 * 3_600)
}

/// 按 Java 正则的匹配顺序找第一个 `[A-Z][a-z]{2,8}\.?\s+\d{1,2},?\s+\d{4}`。
fn scan_published(text: &str) -> Option<(String, u32, i64)> {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if !chars[start].is_ascii_uppercase() {
            continue;
        }
        // `[a-z]{2,8}` 贪婪：先量出这一段有多少个小写字母，再从长到短试（Java 的回溯顺序）
        let mut run = 0usize;
        while run < 8 && chars.get(start + 1 + run).is_some_and(|c| c.is_ascii_lowercase()) {
            run += 1;
        }
        let mut len = run;
        while len >= 2 {
            // 第 1 个字符（大写）也要算进名字里：`[A-Z][a-z]{2,8}` 是一个整体
            let name: String = chars[start..start + 1 + len].iter().collect();
            let after_name = start + 1 + len;
            // `\.?` 贪婪：先试"有点"，再试"没点"
            for dot in [true, false] {
                if dot && chars.get(after_name) != Some(&'.') {
                    continue;
                }
                let mut i = after_name + usize::from(dot);
                let ws = i;
                while chars.get(i).is_some_and(|c| is_ascii_regex_ws(*c)) {
                    i += 1;
                }
                if i == ws {
                    continue;
                }
                // `(\d{1,2})` 贪婪：先试 2 位、再试 1 位
                for day_len in [2usize, 1] {
                    let Some(day) = digits_at(&chars, i, day_len) else {
                        continue;
                    };
                    let j = i + day_len;
                    for comma in [true, false] {
                        if comma && chars.get(j) != Some(&',') {
                            continue;
                        }
                        let mut k = j + usize::from(comma);
                        let ws2 = k;
                        while chars.get(k).is_some_and(|c| is_ascii_regex_ws(*c)) {
                            k += 1;
                        }
                        if k == ws2 {
                            continue;
                        }
                        let Some(year) = digits_at(&chars, k, 4) else {
                            continue;
                        };
                        return Some((name, day, year as i64));
                    }
                }
            }
            len -= 1;
        }
    }
    None
}

/// `at` 开始正好 `n` 个 ASCII 数字。
fn digits_at(chars: &[char], at: usize, n: usize) -> Option<u32> {
    let mut value = 0u32;
    for i in 0..n {
        let c = *chars.get(at + i)?;
        if !c.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (c as u32 - '0' as u32);
    }
    Some(value)
}

/// 公历 → "从 1970-01-01 起的天数"（Howard Hinnant 的 `days_from_civil`）。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

// ===========================================================================
// 详情
// ===========================================================================

/// [`YoutubeVideo`] → 详情页的 [`FeedItem`]（照 `YoutubeParser.itemOf`；信息流里同一条内容的 key
/// 必须一致：就是 videoId）。年龄限制映射到统一的 `FeedItem.adult`，展示端整条隐藏。
pub fn item_of(video: &YoutubeVideo, meta: Option<&YoutubeVideoMeta>) -> FeedItem {
    let channel_id = video.channel_id.clone().or_else(|| meta.and_then(|m| m.channel_id.clone()));
    let author = video.author.as_ref().map(|name| {
        let id = channel_id
            .clone()
            .or_else(|| meta.and_then(|m| m.channel_handle.clone()))
            .unwrap_or_else(|| name.clone());
        let url = channel_id
            .as_ref()
            .map(|id| format!("https://www.youtube.com/channel/{id}"))
            .or_else(|| {
                meta.and_then(|m| m.channel_handle.as_ref())
                    .map(|handle| format!("https://www.youtube.com/{handle}"))
            });
        UserRef {
            platform: YOUTUBE.to_string(),
            id,
            name: name.clone(),
            avatar: meta.and_then(|m| m.avatar.clone()),
            url,
        }
    });
    let summary = video.description.as_ref().and_then(|d| {
        if is_blank(d) {
            return None;
        }
        Some(take_chars(first_line(d), 200))
    });
    FeedItem {
        platform: YOUTUBE.to_string(),
        id: video.video_id.clone(),
        kind: ItemKind::Video,
        title: video.title.clone(),
        summary,
        language: None,
        author,
        published_at: meta.and_then(|m| m.published_at),
        published_at_precision: meta
            .map(|m| m.published_at_precision)
            .unwrap_or(TimePrecision::Second),
        media: vec![Media::Video {
            cover: video.cover.clone(),
            duration_sec: video.duration_sec,
            width: video.cover_width,
            height: video.cover_height,
        }],
        stats: Stats {
            views: video.view_count,
            ..Stats::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: Vec::new(),
        adult: video.age_restricted,
        url: Some(format!("{WATCH}{}", video.video_id)),
        detail: DetailTarget::Native {
            id: video.video_id.clone(),
            platform: None,
        },
    }
}

/// 详情的 [`VideoInfo`]：YouTube 没有分 P，只有 `main` 那条。
pub fn video_info(video: &YoutubeVideo) -> VideoInfo {
    VideoInfo {
        parts: vec![VideoPart {
            id: MAIN_PART.to_string(),
            title: video.title.clone(),
            duration_sec: video.duration_sec,
            width: video.cover_width,
            height: video.cover_height,
        }],
        danmaku_segment_ms: None,
    }
}

/// 年龄限制的详情（`YoutubeSource.hiddenDetail`）：只有一条被标成 18+ 的条目，没有正文、没有播放信息。
pub fn hidden_detail(id: &str) -> FeedDetail {
    FeedDetail {
        item: FeedItem {
            platform: YOUTUBE.to_string(),
            id: id.to_string(),
            kind: ItemKind::Video,
            title: None,
            summary: None,
            language: None,
            author: None,
            published_at: None,
            published_at_precision: TimePrecision::Second,
            media: Vec::new(),
            stats: Stats::default(),
            reason: None,
            community: None,
            community_icon: None,
            repost: None,
            emotes: Vec::new(),
            adult: true,
            url: Some(format!("{WATCH}{id}")),
            detail: DetailTarget::Native {
                id: id.to_string(),
                platform: None,
            },
        },
        body: RichText::EMPTY,
        video: None,
        follow: None,
        related: Vec::new(),
    }
}

/// 详情正文：简介（`RichText.ofText`，非空白才算）。
pub fn body_of(video: &YoutubeVideo) -> RichText {
    match video.description.as_ref().filter(|d| !is_blank(d)) {
        Some(text) => RichText {
            blocks: vec![Block::Paragraph {
                spans: vec![Span::Text {
                    text: text.clone(),
                    bold: false,
                    italic: false,
                }],
            }],
        },
        None => RichText::EMPTY,
    }
}

/// Kotlin `lineSequence().firstOrNull()`。
fn first_line(text: &str) -> &str {
    match text.find(|c| c == '\r' || c == '\n') {
        Some(at) => &text[..at],
        None => text,
    }
}

/// Kotlin `String.take(n)`。
fn take_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// ===========================================================================
// 评论 / 楼中楼
// ===========================================================================

/// `engagementPanels` 里的评论面板（`panelIdentifier == "comment-item-section"`）→ 第一页评论的游标。
///
/// 面板头的排序菜单 `sortFilterSubMenuRenderer.subMenuItems[]` 里每个排序项各带一个续页 token
/// （`Top` / `Newest`），面板内容里还有一个默认的 `itemSectionRenderer.continuations[0]`。
/// **这一步要发的 `next` 请求是省不掉的**（先 `next videoId`，再带上这个 token `next continuation`）。
pub fn comments_token(json: &JsonMap, sort: CommentSort) -> Option<String> {
    if let Some(panel) = engagement_panel(json, "comment-item-section") {
        let sub_menu: &[Value] = json::obj(panel, "header")
            .and_then(|h| json::obj(h, "engagementPanelTitleHeaderRenderer"))
            .and_then(|h| json::obj(h, "menu"))
            .and_then(|m| json::obj(m, "sortFilterSubMenuRenderer"))
            .and_then(|m| json::arr(m, "subMenuItems"))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if !sub_menu.is_empty() {
            let wanted = match sort {
                CommentSort::Newest => Some("Newest"),
                CommentSort::Hot => Some("Top"),
                CommentSort::Default => None,
            };
            let item = sub_menu
                .iter()
                .find(|item| {
                    item.as_object()
                        .and_then(|m| text_of(m, "title"))
                        .as_deref()
                        == wanted
                })
                .or_else(|| {
                    sub_menu.iter().find(|item| {
                        item.as_object().and_then(|m| text_of(m, "selected")).as_deref()
                            == Some("true")
                    })
                })
                .or_else(|| sub_menu.first());
            if let Some(token) = item
                .and_then(|i| i.get("continuation"))
                .and_then(Value::as_object)
                .and_then(|c| json::obj(c, "reloadContinuationData"))
                .and_then(|r| text_of(r, "continuation"))
            {
                return Some(token);
            }
        }
    }
    fallback_comments_token(json)
}

/// 评论面板自己的 `itemSectionRenderer.continuations[0].reloadContinuationData.continuation`。
fn fallback_comments_token(json: &JsonMap) -> Option<String> {
    let panel = engagement_panel(json, "comment-item-section")?;
    let section = json::obj(panel, "content")?.get("sectionListRenderer")?.as_object()?;
    json::arr(section, "contents")?
        .iter()
        .filter_map(|c| c.get("itemSectionRenderer").and_then(Value::as_object))
        .flat_map(|s| json::arr(s, "continuations").map(|v| v.as_slice()).unwrap_or(&[]))
        .find_map(|c| {
            c.get("reloadContinuationData")
                .and_then(Value::as_object)
                .and_then(|d| text_of(d, "continuation"))
        })
}

/// `engagementPanels[]` 里 `panelIdentifier` 匹配的那个面板。
fn engagement_panel<'a>(json: &'a JsonMap, identifier: &str) -> Option<&'a JsonMap> {
    json::arr(json, "engagementPanels")?
        .iter()
        .filter_map(|p| p.get("engagementPanelSectionListRenderer").and_then(Value::as_object))
        .find(|p| text_of(p, "panelIdentifier").as_deref() == Some(identifier))
}

/// 一页评论（一级 / 楼中楼同一个结构）。
///
/// 形状：`continuationContents.itemSectionContinuation.contents[]`，每一项是
/// `commentThreadRenderer.comment.commentRenderer`（一级评论）或裸的 `commentRenderer`（楼中楼的第一项是父评论
/// 自己，`parent_id` 传进来时跳过）。`handoffInstructionRenderer`（"在手机上继续看"）不是评论，丢掉。
/// 下一页在 `itemSectionContinuation.continuations[0].nextContinuationData.continuation`。
///
/// 每条评论的楼中楼 token 从 `detailViewEndpoint` 里抠出来、放进返回的那个数组
/// （`Comment` 模型里没地方放它，游标又得由调用方给）。
pub fn comments(
    json: &JsonMap,
    now: i64,
    parent_id: Option<&str>,
) -> (CommentPage, Vec<(String, String)>) {
    let section = json::obj(json, "continuationContents")
        .and_then(|c| json::obj(c, "itemSectionContinuation"));
    let mut items: Vec<Comment> = Vec::new();
    let mut tokens: Vec<(String, String)> = Vec::new();
    if let Some(section) = section {
        for element in json::arr(section, "contents").map(|v| v.as_slice()).unwrap_or(&[]) {
            let renderer = element
                .get("commentThreadRenderer")
                .and_then(Value::as_object)
                .and_then(|c| c.get("comment"))
                .and_then(Value::as_object)
                .and_then(|c| c.get("commentRenderer"))
                .and_then(Value::as_object)
                .or_else(|| element.get("commentRenderer").and_then(Value::as_object));
            let Some(renderer) = renderer else { continue };
            let Some(id) = text_of(renderer, "commentId") else {
                continue;
            };
            if Some(id.as_str()) == parent_id {
                continue;
            }
            if let Some(token) = replies_token_of(renderer) {
                tokens.push((id.clone(), token));
            }
            items.push(comment(renderer, now));
        }
    }
    let next = section
        .and_then(|s| json::arr(s, "continuations"))
        .and_then(|list| {
            list.iter().find_map(|c| {
                c.get("nextContinuationData")
                    .and_then(Value::as_object)
                    .and_then(|d| text_of(d, "continuation"))
                    .filter(|t| !is_blank(t))
            })
        });
    (
        CommentPage { items, next },
        tokens,
    )
}

/// 楼中楼的续页 token（每条评论的 `detailViewEndpoint` 里的那个面板）。
fn replies_token_of(renderer: &JsonMap) -> Option<String> {
    let section = json::obj(renderer, "detailViewEndpoint")
        .and_then(|e| json::obj(e, "showEngagementPanelEndpoint"))
        .and_then(|e| json::obj(e, "engagementPanel"))
        .and_then(|e| json::obj(e, "engagementPanelSectionListRenderer"))
        .and_then(|e| json::obj(e, "content"))
        .and_then(|e| json::obj(e, "sectionListRenderer"))?;
    json::arr(section, "contents")?
        .iter()
        .find_map(|c| c.get("itemSectionRenderer").and_then(Value::as_object))
        .and_then(|s| json::arr(s, "continuations"))
        .and_then(|list| {
            list.iter().find_map(|c| {
                c.get("reloadContinuationData")
                    .and_then(Value::as_object)
                    .and_then(|d| text_of(d, "continuation"))
            })
        })
}

/// 一条 `commentRenderer` → [`Comment`]。作者只有 `@handle`（TV 不给显示名），点赞是 "104K" 这种计数文本。
fn comment(r: &JsonMap, now: i64) -> Comment {
    // `authorText` 是 TextItem（`{simpleText}` / `{runs}`），不是字符串
    let author = text_of_element(json::obj(r, "authorText")).map(|raw| {
        // Kotlin 是 `textOf(...)?.trim()`，id / name 都用 trim 过的那份
        let name = trim_java(&raw).to_string();
        UserRef {
            platform: YOUTUBE.to_string(),
            id: name.clone(),
            name,
            // 评论的头像在 `authorThumbnail`，不是 `thumbnail`（Kotlin `r.field("authorThumbnail")`）
            avatar: last_thumbnail_in(r, "authorThumbnail").map(|u| json::to_https(&u)),
            url: None,
        }
    });
    let runs: &[Value] = json::obj(r, "contentText")
        .and_then(|c| json::arr(c, "runs"))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    // 表情：run 自带 `emoji` 对象（有图就用图的地址）先处理；纯文字 run 里的短码再查表。
    let mut spans: Vec<Span> = Vec::new();
    for run in runs {
        if run.get("emoji").is_some() {
            if let Some(span) = emotes::span(run) {
                spans.push(span);
            }
            continue;
        }
        if let Some(text) = run
            .as_object()
            .and_then(|m| json::text(m, "text"))
            .filter(|t| !t.is_empty())
        {
            spans.extend(emotes::spans(&text));
        }
    }
    let published = parse_relative_time_precise(
        json::obj(r, "publishedTimeText")
            .and_then(|p| text_of_element(Some(p)))
            .as_deref(),
        now,
    );
    let reply_count = json::long(r, "replyCount").or_else(|| {
        json::obj(r, "repliesCount")
            .and_then(|c| text_of_element(Some(c)))
            .as_deref()
            .and_then(|t| json::parse_count(Some(t)))
    });
    let mut comment = Comment::new(
        YOUTUBE,
        text_of(r, "commentId").unwrap_or_default(),
        RichText {
            blocks: vec![Block::Paragraph { spans }],
        },
    );
    comment.author = author;
    comment.published_at = published.map(|p| p.epoch_sec);
    comment.published_at_precision = published.map(|p| p.precision).unwrap_or(TimePrecision::Second);
    comment.likes = json::obj(r, "voteCount")
        .and_then(|v| text_of_element(Some(v)))
        .as_deref()
        .and_then(|t| json::parse_count(Some(t)));
    comment.reply_count = reply_count;
    comment.pinned = json::obj(r, "pinnedCommentBadge").is_some();
    comment
}

