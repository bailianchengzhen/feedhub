//! 小红书：推荐流（`POST homefeed`）、关注流的三个接口（`followings/self`、`followings/extra`、`user_posted`）
//! 和搜索（`POST search/notes`，见 [`search`]）的解析。
//!
//! 逐条对照：
//! - 推荐流：`source-xiaohongshu/.../XhsParser.homefeed` / `homefeedTitles` / `homefeedCard` / `user` / `likes` / `idTime`；
//! - 关注流：`XhsFollowParser.followings` / `unread` / `posted` / `postedTitles`；
//! - 搜索：[`search`]（这一家没有 Kotlin 版可以对拍，口径写在那个模块的文件头）；
//! - 错误：`XhsApi.checkResponse` / `checkBiz`（HTTP 状态码和业务码一起判断，和 Kotlin 一一对应）。
//!
//! 签名见 [`sign`]，请求怎么拼见 [`source`]。详情 / 评论 / 楼中楼 / 视频已经搬了（[`detail`] / [`play`]）；
//!
//! **时间也是注入的**：推荐流卡片没有发布时间，`publishedAt` 用笔记 id 前 8 位的创建时间，
//! 那一步要一个"现在"来判断 id 里的时间像不像真的（`idTime`），所以 `now_sec` 由 Kotlin 传进来。

pub mod account;
pub mod aes;
pub mod codec;
pub mod detail;
pub mod deflate;
pub mod emotes;
pub mod feeditem_json;
pub mod follow;
pub mod follow_feed;
pub mod jsonord;
pub mod play;
pub mod qr;
pub mod search;
pub mod sign;
pub mod source;

use crate::json::{self, JsonMap};
use crate::model::{BiliError, DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};
use serde_json::Value;

/// 小红书的平台 id。**定了就别改**（存储键、cli 参数、条目 key 都用它）。
pub const XHS: &str = "xhs";

pub const WEB: &str = "https://www.xiaohongshu.com";

/// 「当前笔记暂时无法浏览」：xsec_token 无效 / 过期、笔记被删或仅自己可见。
pub const NOTE_UNAVAILABLE: i64 = 300031;

/// 从关注列表进主页时网页用的 `xsec_source`。
pub const SOURCE_USER: &str = "pc_user";

/// 从信息流点进笔记时网页用的 `xsec_source`（推荐流卡片的 `detail` 用它）。
pub const SOURCE_FEED: &str = "pc_feed";

/// 从关注列表拉某个人的主页时网页用的 `xsec_source`（`user_posted` 的查询参数）。
pub const SOURCE_FOLLOWED: &str = "pc_followed";

pub fn user_url(id: &str) -> String {
    format!("{WEB}/user/profile/{id}")
}

/// 电脑版笔记页。带 `xsec_token` 才打得开（不带会跳到 /404/sec_…），所以分享链接也带上。
pub fn note_url(id: &str, xsec_token: Option<&str>, xsec_source: &str) -> String {
    let base = format!("{WEB}/explore/{id}");
    match xsec_token {
        Some(t) if !t.is_empty() => format!("{base}?xsec_token={}&xsec_source={xsec_source}", encode_component(t)),
        _ => base,
    }
}

/// [`FeedItem.detail`] 里的 id：`<笔记 id>@<xsec_source>@<xsec_token>`（后两段详情接口要用，卡片里自带）。
pub fn native_id(note_id: &str, xsec_source: &str, xsec_token: Option<&str>) -> String {
    match xsec_token {
        Some(t) if !t.is_empty() => format!("{note_id}@{xsec_source}@{t}"),
        _ => note_id.to_string(),
    }
}

/// [`native_id`] 的逆运算。只有笔记 id 时 source / token 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRef {
    pub note_id: String,
    pub xsec_source: Option<String>,
    pub xsec_token: Option<String>,
}

/// Kotlin `id.split('@', limit = 3)`：正好 3 段才拆出 source / token，别的都只看第一段。
pub fn parse_native_id(id: &str) -> NoteRef {
    let parts: Vec<&str> = id.splitn(3, '@').collect();
    if parts.len() == 3 {
        NoteRef {
            note_id: parts[0].to_string(),
            xsec_source: if parts[1].is_empty() { None } else { Some(parts[1].to_string()) },
            xsec_token: if parts[2].is_empty() { None } else { Some(parts[2].to_string()) },
        }
    } else {
        NoteRef {
            note_id: parts[0].to_string(),
            xsec_source: None,
            xsec_token: None,
        }
    }
}

/// 笔记 id 是 24 位十六进制（和 MongoDB ObjectId 同构），前 8 位是创建时间（Unix 秒）。
///
/// 它 ≤ 笔记的发布时间 `time`，实测相差 0 到约一天（定时发布和先存草稿再发的会更大）；
/// 推荐流的卡片没有发布时间，逐条调详情又会触发风控，所以信息流先用它，打开详情后再用真实的 `time`。
pub fn id_time(note_id: &str, now_sec: i64) -> Option<i64> {
    let chars: Vec<char> = note_id.chars().collect();
    if chars.len() != 24 {
        return None;
    }
    let head: String = chars[..8].iter().collect();
    let t = i64::from_str_radix(&head, 16).ok()?;
    if (1_356_998_400..=(now_sec + 86_400)).contains(&t) {
        Some(t)
    } else {
        None
    }
}

/// 点赞数文本："4673"、"1.3万"、"10万+"（超过 10 万的封顶写法，去掉 `+` 按 10 万算）。
pub fn likes(text: Option<&str>) -> Option<i64> {
    let t = text?.trim().trim_end_matches('+');
    json::parse_count(Some(t))
}

/// 查询串编码：值按 `encodeURIComponent` 编码，但逗号保持原样（网页的 `image_formats=jpg,webp,avif`）。
///
/// 平台层（[`source`]）拼 URL 也用它，所以是 `pub`：**必须**和 Kotlin `XhsApi.encodeComponent` 逐字节一致
///（不是 OkHttp 的 `addQueryParameter` 那套）。
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let c = b as char;
        if b < 0x80 && (c.is_ascii_alphanumeric() || "-_.!~*'(),".contains(c)) {
            out.push(c);
        } else {
            out.push('%');
            out.push("0123456789ABCDEF".as_bytes()[(b >> 4) as usize] as char);
            out.push("0123456789ABCDEF".as_bytes()[(b & 15) as usize] as char);
        }
    }
    out
}

// ===========================================================================
// 错误（`XhsApi.checkResponse` / `checkBiz` 的移植）
// ===========================================================================

fn note_unavailable() -> BiliError {
    BiliError::Api {
        code: NOTE_UNAVAILABLE.to_string(),
        detail: "当前笔记暂时无法浏览：入口里的 xsec_token 失效了（很久以前收藏的会这样），或者笔记被删除 / 仅自己可见。请从信息流重新打开"
            .to_string(),
    }
}

/// 业务码：`{code, success, msg, data}`。没有 `code` 的接口（redmoji）按成功算。
fn check_biz(root: JsonMap) -> Result<JsonMap, BiliError> {
    let code = match json::long(&root, "code") {
        Some(c) => c,
        None => return Ok(root),
    };
    let msg = json::text(&root, "msg").map(|m| json::take_chars(&m, 100)).unwrap_or_default();
    match code {
        0 => Ok(root),
        -100 | -101 => Err(BiliError::NotLoggedIn {
            detail: Some(format!("登录已失效（{code}）")),
        }),
        300011 => Err(BiliError::RiskControl {
            code: Some("300011".to_string()),
            detail: "账号被限制，停止一切请求，先到网页版看看有没有验证或提示".to_string(),
        }),
        300012 => Err(BiliError::RiskControl {
            code: Some("300012".to_string()),
            detail: "当前 IP 被限制，换网络或等一阵".to_string(),
        }),
        300015 => Err(BiliError::Signature {
            detail: format!("300015 {msg}"),
        }),
        NOTE_UNAVAILABLE => Err(note_unavailable()),
        _ => Err(BiliError::Api {
            code: code.to_string(),
            detail: msg,
        }),
    }
}

fn biz_code(body: &str) -> Option<i64> {
    let root = json::parse_object(body).ok()?;
    json::long(&root, "code")
}

/// Kotlin `bizError`：`try { checkBiz(parseJsonObject(...)); null } catch (e: Exception) { e }`。
/// 注意"body 不是 JSON"也是它返回的错误（不是 `None`）——状态码非 2xx 时那就是最终异常。
fn biz_error(body: &str) -> Option<BiliError> {
    match json::parse_object(body) {
        Ok(root) => check_biz(root).err(),
        Err(detail) => Some(BiliError::Parse { detail }),
    }
}

/// 响应检查：HTTP 状态码 → 业务码，返回 `data` 外面的整个 JSON 对象（成功时）。
///
/// 和 Kotlin `XhsApi.checkResponse` 一一对应：461 / 471 里 `300031`（笔记打不开）单独区分，
/// 406 / 416 当签名问题，401 当未登录；其余状态码先看业务码、再退回"HTTP n"。
pub fn check_http(body: &str, http_code: i32) -> Result<JsonMap, BiliError> {
    check_http_impl(body, http_code, false)
}

/// 扫码登录那几条请求的响应检查（Kotlin `checkResponse(res, secret = true)`）。
///
/// 差别只有两处，都在**出错信息**上（成功的响应里带着 `login_info.session` 这种凭据）：
/// 1. "不是 JSON 对象"的解析错误**不回显响应内容**（Kotlin `parseQrJson`）；
/// 2. 兜底的 `HTTP n` 只写状态码，不带 body 前 120 字（Kotlin 的 `secret` 分支）。
pub fn check_http_secret(body: &str, http_code: i32) -> Result<JsonMap, BiliError> {
    check_http_impl(body, http_code, true)
}

fn check_http_impl(body: &str, http_code: i32, secret: bool) -> Result<JsonMap, BiliError> {
    match http_code {
        200..=299 => {}
        461 | 471 => {
            if biz_code(body) == Some(NOTE_UNAVAILABLE) {
                return Err(note_unavailable());
            }
            return Err(BiliError::RiskControl {
                code: Some(format!("HTTP {http_code}")),
                detail: "需要验证码，先别请求了，到网页版里过一下验证".to_string(),
            });
        }
        // 403 / 412 / 429：项目红线里的风控信号，立刻停（原来落进"其余状态码"成了 `Api`，
        // 关注流调度会把它当"这个作者读不了"接着请求下一个作者）
        403 | 412 | 429 => {
            return Err(BiliError::RiskControl {
                code: Some(format!("HTTP {http_code}")),
                detail: "请求被限制，先别请求了".to_string(),
            });
        }
        406 => {
            return Err(BiliError::Signature {
                detail: "HTTP 406（多半是签名格式过期，也可能是 TLS 指纹被拒）".to_string(),
            })
        }
        416 => {
            return Err(BiliError::Signature {
                detail: "HTTP 416（签名错误；网页上登录的账号可能被踢下线）".to_string(),
            })
        }
        401 => return Err(BiliError::NotLoggedIn { detail: None }),
        _ => {
            if let Some(e) = biz_error(body) {
                return Err(e);
            }
            return Err(BiliError::Api {
                code: format!("HTTP {http_code}"),
                // Kotlin 的 `secret` 分支只写状态码（body 里可能有凭据）
                detail: if secret {
                    format!("扫码登录接口回 HTTP {http_code}")
                } else {
                    json::take_chars(body, 120)
                },
            });
        }
    }
    let root = if secret {
        parse_secret_object(body)?
    } else {
        json::parse_object(body).map_err(|detail| BiliError::Parse { detail })?
    };
    check_biz(root)
}

/// Kotlin `parseQrJson`：和 `parseJsonObject` 一样，但错误信息里**不带响应内容**
/// （成功的响应里有 `login_info.session` 这种凭据）。
fn parse_secret_object(body: &str) -> Result<JsonMap, BiliError> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err(BiliError::Parse {
            detail: "二维码接口的响应不是 JSON 对象".to_string(),
        }),
        Err(_) => Err(BiliError::Parse {
            detail: "二维码接口的响应不是合法 JSON".to_string(),
        }),
    }
}

// ===========================================================================
// 表情
// ===========================================================================

/// 从整张表里挑出 `texts` 里真的出现过的代码（对应 `Emotes.used`：**保持表的顺序**，空的跳过）。
pub(crate) fn emotes_used(table: &[(String, String)], texts: &[Option<&str>]) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let present: Vec<&str> = texts.iter().flatten().filter(|t| !t.is_empty()).copied().collect();
    if present.is_empty() {
        return Vec::new();
    }
    table
        .iter()
        .filter(|(code, url)| !code.is_empty() && !url.is_empty() && present.iter().any(|t| t.contains(code.as_str())))
        .cloned()
        .collect()
}

// ===========================================================================
// 推荐流
// ===========================================================================

/// `POST homefeed` 的一页。
#[derive(Debug)]
pub struct HomefeedPage {
    pub page: Page,
    /// 这一页所有卡片的标题拼在一起（Kotlin 用它检查有没有表情记号，有才去取表情表）。
    pub titles: String,
}

/// `POST /api/sns/web/v1/homefeed` 的响应：`data.{cursor_score, items[]}`。只收 `model_type == "note"` 的卡片。
///
/// [`note_index`] 是这一页开始时的 `note_index`（`cursor_score|cursor` 里的那一段），下一页的游标是
/// `<cursor_score>|<note_index + 本页条目数>`（和 Kotlin `XhsSource.recommend` 算的一样，条目数算**被丢掉的**卡片）。
pub fn parse_homefeed(
    body: &str,
    http_code: i32,
    note_index: i32,
    xsec_source: &str,
    now_sec: i64,
    emotes: &[(String, String)],
) -> Result<HomefeedPage, BiliError> {
    let root = check_http(body, http_code)?;
    let data = json::obj(&root, "data");
    let empty: &[Value] = &[];
    let raw = data.and_then(|d| json::arr(d, "items")).map(|v| v.as_slice()).unwrap_or(empty);
    let items = raw
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| homefeed_card(o, emotes, xsec_source, now_sec))
        .collect::<Vec<_>>();
    let titles = titles_of(raw);
    let next = data
        .and_then(|d| json::text(d, "cursor_score"))
        .filter(|_| !raw.is_empty())
        .map(|score| format!("{score}|{}", note_index.wrapping_add(raw.len() as i32)));
    Ok(HomefeedPage {
        page: Page { items, next },
        titles,
    })
}

/// 这一页所有卡片的标题拼在一起（`XhsParser.homefeedTitles`）。
fn titles_of(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| json::obj(o, "note_card"))
        .filter_map(|c| json::text(c, "display_title"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn homefeed_card(item: &JsonMap, emotes: &[(String, String)], xsec_source: &str, now_sec: i64) -> Option<FeedItem> {
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
    let token = json::text(item, "xsec_token");
    let is_video = json::text(card, "type").as_deref() == Some("video");
    let title = json::text(card, "display_title");
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
    let likes = json::obj(card, "interact_info")
        .and_then(|i| json::string(i, "liked_count"))
        .map(|s| s.trim().trim_end_matches('+').to_string());
    Some(FeedItem {
        platform: XHS.to_string(),
        id: id.clone(),
        kind: if is_video { ItemKind::Video } else { ItemKind::Post },
        title: title.clone(),
        summary: None,
        language: None,
        author,
        published_at: id_time(&id, now_sec),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: likes.and_then(|t| json::parse_count(Some(&t))),
            ..Default::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: emotes_used(emotes, &[title.as_deref()]),
        adult: false,
        url: Some(note_url(&id, token.as_deref(), xsec_source)),
        detail: DetailTarget::Native {
            id: native_id(&id, xsec_source, token.as_deref()),
            platform: None,
        },
    })
}

/// 卡片 / 详情里的作者：`user_id`、`nickname`（= `nick_name`）、`avatar`。
fn user_of(o: &JsonMap) -> Option<UserRef> {
    let id = json::text(o, "user_id")?;
    let name = json::text(o, "nickname").or_else(|| json::text(o, "nick_name")).unwrap_or_else(|| id.clone());
    let avatar = json::text(o, "avatar").map(|a| json::to_https(&a));
    Some(UserRef {
        platform: XHS.to_string(),
        id: id.clone(),
        name,
        avatar,
        url: Some(user_url(&id)),
    })
}

// ===========================================================================
// 关注流
// ===========================================================================

/// 关注列表里的一个人（`followings/self` 的 `user_list[]`）。
///
/// `xsec_token` 是列表项自带的：拉这个人的 `user_posted` 要用它（`xsec_source=pc_followed`），token 会过期，
/// 所以关注列表缓存最多一天（见 Kotlin `XhsFollowFeed`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedAuthor {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub xsec_token: Option<String>,
}

/// `GET user/followings/self` 的一页。`cursor` 是下一页要带的游标；`has_more` 最后一页之前就可能是 true，
/// 翻页时要同时看是不是空页（见 Kotlin `XhsFollowFeed`）。
#[derive(Debug)]
pub struct FollowingsPage {
    pub users: Vec<FollowedAuthor>,
    pub cursor: Option<String>,
    pub has_more: bool,
    pub total: Option<i32>,
}

pub fn parse_followings(body: &str, http_code: i32) -> Result<FollowingsPage, BiliError> {
    let root = check_http(body, http_code)?;
    let data = json::obj(&root, "data").ok_or_else(|| BiliError::Parse {
        detail: "followings/self 缺少 data".to_string(),
    })?;
    let users = json::arr(data, "user_list")
        .map(|v| v.iter().filter_map(|x| x.as_object()).filter_map(followed_author).collect())
        .unwrap_or_default();
    Ok(FollowingsPage {
        users,
        cursor: json::text(data, "cursor"),
        has_more: json::bool(data, "has_more") == Some(true),
        total: json::int(data, "total"),
    })
}

fn followed_author(o: &JsonMap) -> Option<FollowedAuthor> {
    let id = json::text(o, "user_id")?;
    Some(FollowedAuthor {
        name: json::text(o, "nickname").or_else(|| json::text(o, "remark")).unwrap_or_else(|| id.clone()),
        id,
        avatar: json::text(o, "image").map(|a| json::to_https(&a)),
        xsec_token: json::text(o, "xsec_token"),
    })
}

/// `followings/extra` 的响应：用户 id → 未看笔记数。只认 `resource_type == UNREAD_COUNT` 的项（没有这个字段也认）；
/// 请求里有、响应里没有的用户由调用方当作 0。
pub fn parse_unread(body: &str, http_code: i32) -> Result<Vec<(String, i32)>, BiliError> {
    let root = check_http(body, http_code)?;
    let list = json::obj(&root, "data")
        .and_then(|d| json::arr(d, "follow_extra_list"))
        .ok_or_else(|| BiliError::Parse {
            detail: "followings/extra 缺少 follow_extra_list".to_string(),
        })?;
    let mut out: Vec<(String, i32)> = Vec::with_capacity(list.len());
    for e in list {
        let Some(o) = e.as_object() else { continue };
        if let Some(t) = json::text(o, "resource_type") {
            if t != "UNREAD_COUNT" {
                continue;
            }
        }
        let Some(id) = json::text(o, "user_id") else { continue };
        let count = json::int(o, "unread_note_count").unwrap_or(0).max(0);
        // Kotlin 是 HashMap 覆盖：同一个 id 出现两次以后一个为准
        if let Some(slot) = out.iter_mut().find(|(k, _)| *k == id) {
            slot.1 = count;
        } else {
            out.push((id, count));
        }
    }
    Ok(out)
}

/// `user_posted` 的一页：笔记 + 每条是不是置顶 + 这个人还有没有更早的笔记 + 标题拼串（表情检测用）。
#[derive(Debug)]
pub struct PostedPage {
    pub notes: Vec<(FeedItem, bool)>,
    pub has_more: bool,
    pub titles: String,
}

/// `GET user_posted` 的响应。[`author`] 是关注列表里的这个人：笔记项里的 `user` 缺失时用它。
///
/// 笔记的 `xsec_source` 用 `pc_user`（从作者主页点进笔记时网页的值），token 是笔记项自己的。
/// 没有发布时间的笔记（响应里没有 `time`，笔记 id 也推不出）丢掉：关注流按发布时间排，没有时间排不了。
pub fn parse_posted(
    body: &str,
    http_code: i32,
    author: &FollowedAuthor,
    now_sec: i64,
    emotes: &[(String, String)],
) -> Result<PostedPage, BiliError> {
    let root = check_http(body, http_code)?;
    let data = json::obj(&root, "data").ok_or_else(|| BiliError::Parse {
        detail: "user_posted 缺少 data".to_string(),
    })?;
    let empty: &[Value] = &[];
    let raw = json::arr(data, "notes").map(|v| v.as_slice()).unwrap_or(empty);
    let notes = raw
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| posted_note(o, author, emotes, now_sec))
        .collect::<Vec<_>>();
    let titles = raw
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| json::text(o, "display_title"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(PostedPage {
        notes,
        has_more: json::bool(data, "has_more") == Some(true),
        titles,
    })
}

fn posted_note(
    o: &JsonMap,
    author: &FollowedAuthor,
    emotes: &[(String, String)],
    now_sec: i64,
) -> Option<(FeedItem, bool)> {
    let id = json::text(o, "note_id")?;
    let published_at = json::long(o, "time")
        .filter(|t| *t > 0)
        .map(|t| t / 1000)
        .or_else(|| id_time(&id, now_sec))?;
    let token = json::text(o, "xsec_token");
    let is_video = json::text(o, "type").as_deref() == Some("video");
    let title = json::text(o, "display_title");
    let cover = json::obj(o, "cover");
    let cover_url = cover
        .and_then(|c| {
            json::text(c, "url_default")
                .or_else(|| json::text(c, "url_pre"))
                .or_else(|| json::text(c, "url"))
        })
        .map(|u| json::to_https(&u));
    let width = cover.and_then(|c| json::int(c, "width"));
    let height = cover.and_then(|c| json::int(c, "height"));
    let interact = json::obj(o, "interact_info");
    let user = json::obj(o, "user").and_then(user_of).unwrap_or_else(|| UserRef {
        platform: XHS.to_string(),
        id: author.id.clone(),
        name: author.name.clone(),
        avatar: author.avatar.clone(),
        url: Some(user_url(&author.id)),
    });
    let mut media = Vec::new();
    if is_video {
        media.push(Media::Video {
            cover: cover_url,
            duration_sec: None,
            width,
            height,
        });
    } else if let Some(url) = cover_url {
        media.push(Media::Image { url, width, height });
    }
    let likes = interact
        .and_then(|i| json::string(i, "liked_count"))
        .map(|s| s.trim().trim_end_matches('+').to_string());
    let item = FeedItem {
        platform: XHS.to_string(),
        id: id.clone(),
        kind: if is_video { ItemKind::Video } else { ItemKind::Post },
        title: title.clone(),
        summary: None,
        language: None,
        author: Some(user),
        published_at: Some(published_at),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: likes.and_then(|t| json::parse_count(Some(&t))),
            ..Default::default()
        },
        reason: None,
        community: None,
        community_icon: None,
        repost: None,
        emotes: emotes_used(emotes, &[title.as_deref()]),
        adult: false,
        url: Some(note_url(&id, token.as_deref(), SOURCE_USER)),
        detail: DetailTarget::Native {
            id: native_id(&id, SOURCE_USER, token.as_deref()),
            platform: None,
        },
    };
    let sticky = interact.and_then(|i| json::bool(i, "sticky")) == Some(true);
    Some((item, sticky))
}
