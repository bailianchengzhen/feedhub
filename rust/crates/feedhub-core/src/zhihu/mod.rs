//! 知乎：推荐流 + 关注动态的解析，以及带类型接口错误的映射。
//!
//! 逐条对照 `source-zhihu/ZhihuParser.kt`（卡片字段的含义参考过 Zhihu++ 的 `data/Feed.kt`，只用了接口字段名）：
//! - 推荐流 `/api/v3/feed/topstory/recommend`、关注动态 `/api/v3/moments`：`data[]` 里是卡片，
//!   `type=feed_group` 展开 `list`、`feed_advert` 和带 `promotion_extra` / `ad` 的推广丢掉；
//! - 卡片内容在 `target`，按 `target.type` 分成 answer / article / pin / zvideo / question；
//! - 条目 id 带类型前缀（`answer:123`），回答 / 文章 / 想法的数字 id 各自编号，前缀保证跨流去重；
//! - 摘要 / 标题里的表情占位符（`[感谢]`）对到**调用方传进来的**表情表（Kotlin 的 `ZhihuSticker.urls`）：
//!   整张表随插件带资源，Rust 不复制"表情包名 → 资源路径"这套命名。
//!
//! 详情 / 评论 / 搜索 / 扫码 / 播放当前**不搬**，继续用 Kotlin。
//! 错误映射（`ZhihuParser.error`）要看 HTTP 状态码和 `Location`，所以这两个由 Kotlin 当参数传进来。

pub mod comment;
pub mod detail;
pub mod html;
pub mod links;
pub mod play;
pub mod qa;
pub mod qr;
pub mod search;
pub mod sign;
pub mod source;

use crate::json::{self, JsonMap};
use crate::model::{
    BiliError as ZhihuError, DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision,
    UserRef,
};
use serde_json::Value;

/// 知乎的平台 id。**定了就别改**（存储键、cli 参数、条目 key 都用它）。
pub const ZHIHU: &str = "zhihu";

pub fn answer_url(question_id: &str, answer_id: &str) -> String {
    format!("https://www.zhihu.com/question/{question_id}/answer/{answer_id}")
}

pub fn question_url(id: &str) -> String {
    format!("https://www.zhihu.com/question/{id}")
}

pub fn article_url(id: &str) -> String {
    format!("https://zhuanlan.zhihu.com/p/{id}")
}

pub fn pin_url(id: &str) -> String {
    format!("https://www.zhihu.com/pin/{id}")
}

pub fn zvideo_url(id: &str) -> String {
    format!("https://www.zhihu.com/zvideo/{id}")
}

pub fn people_url(url_token: &str) -> String {
    format!("https://www.zhihu.com/people/{url_token}")
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 对应 Kotlin 的 `ZhihuParser.error(code, body, location)`：正常返回 `Ok(root)`，否则给出**带类型**的错误。
///
/// 知乎没有统一的外壳（不像 B站的 `{code}`），错误体是 `{"error":{"code","message","need_login","redirect"}}`，
/// 再配合 HTTP 状态码 / `Location` 判断。顺序照 Kotlin 抄：
/// 1. 跳 `/account/unhuman` 或 `/account/risk_control` → 风控（优先于 need_login）；
/// 2. 2xx 且没有 `error` → 正常；
/// 3. 401 或 `need_login` → 未登录；
/// 4. `10003`（"请求参数异常，请升级客户端后重试"）→ 签名被拒；
/// 5. 403 / 429 / 错误码 4035x / 4036x → 风控；
/// 6. 3xx → 意外的重定向（接口错误）；
/// 7. 其他 → 接口错误。
pub fn check_http(
    http_code: i32,
    body: &str,
    location: Option<&str>,
) -> Result<JsonMap, ZhihuError> {
    if let Some(e) = check_error(http_code, body, location) {
        return Err(e);
    }
    json::parse_object(body).map_err(|detail| ZhihuError::Parse { detail })
}

/// 只做 Kotlin `ZhihuParser.error(code, body, location)` 那一步（**不要求 body 是可解析的 JSON**）。
///
/// 单独拆出来是因为 `ensureDc0` 的 `POST /udid` 也用它：那个接口 200 时经常回空 body / 非 JSON，
/// Kotlin 那边只查错误、不解析；`check_http` 是"查错误 + 解析成功体"，只给会回 JSON 的接口用。
pub fn check_error(http_code: i32, body: &str, location: Option<&str>) -> Option<ZhihuError> {
    // Kotlin 是 runCatching { FeedJson.parseToJsonElement(body).obj()?.obj("error") }：解析失败就当没有错误体。
    let parsed = serde_json::from_str::<Value>(body).ok();
    let err = parsed
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| json::obj(m, "error"));
    let err_code = err.and_then(|e| json::string(e, "code"));
    let msg = err
        .and_then(|e| json::text(e, "message"))
        .unwrap_or_else(|| json::take_chars(body, 200));
    let redirect = location
        .map(str::to_string)
        .or_else(|| err.and_then(|e| json::text(e, "redirect")));

    if let Some(r) = redirect.as_deref() {
        if r.contains("/account/unhuman") || r.contains("/account/risk_control") {
            return Some(ZhihuError::RiskControl {
                code: Some(
                    err_code
                        .clone()
                        .unwrap_or_else(|| format!("HTTP {http_code}")),
                ),
                detail: "要求人机验证，请在浏览器里打开知乎完成验证".to_string(),
            });
        }
    }
    if (200..=299).contains(&http_code) && err.is_none() {
        return None;
    }
    if http_code == 401 || err.and_then(|e| bool_of(e, "need_login")).unwrap_or(false) {
        return Some(ZhihuError::NotLoggedIn { detail: None });
    }
    if err_code.as_deref() == Some("10003") {
        return Some(ZhihuError::Signature {
            detail: format!("10003 {msg}"),
        });
    }
    let risk_code = err_code
        .as_deref()
        .map(|c| c.starts_with("4035") || c.starts_with("4036"))
        .unwrap_or(false);
    if http_code == 403 || http_code == 429 || risk_code {
        return Some(ZhihuError::RiskControl {
            code: Some(
                err_code
                    .clone()
                    .unwrap_or_else(|| format!("HTTP {http_code}")),
            ),
            detail: msg,
        });
    }
    if (300..=399).contains(&http_code) {
        return Some(ZhihuError::Api {
            code: format!("HTTP {http_code}"),
            detail: format!("意外的重定向：{}", redirect.unwrap_or_default()),
        });
    }
    Some(ZhihuError::Api {
        code: err_code.unwrap_or_else(|| format!("HTTP {http_code}")),
        detail: msg,
    })
}

// ---------------------------------------------------------------------------
// 流
// ---------------------------------------------------------------------------

/// 推荐流：`/api/v3/feed/topstory/recommend`。
pub fn parse_recommend(
    body: &str,
    http_code: i32,
    location: Option<&str>,
    emotes: &[(String, String)],
) -> Result<Page, ZhihuError> {
    let root = check_http(http_code, body, location)?;
    page(&root, "推荐流", emotes)
}

/// 信息流卡片的**问题版面**：一条回答在卡片上只显示它的问题——
/// 没有答主那一行、摘要是问题描述、不带回答里的配图、计数只有回答数（放在 `comments`，和详情
/// [`qa::question_view`] 一个口径；平台给 0 就不显示）。**条目身份不变**（id / 链接 / 类型还是那条回答），
/// 收藏、历史、分享、点进去的问题版面都照旧。`root` 是整页响应，回答对应的 `question` 从里面找（`feed_group`
/// 里嵌着的也算）。
///
/// **摘要**：推荐流里回答附带的 `question` 实测 `detail` / `excerpt` 都是空串、`answer_count` 是 0（平台不下发），
/// 所以问题描述多半没有——这时**用这条回答自己的摘要当正文**。
pub fn as_question_cards(root_body: &str, items: &mut [FeedItem], emotes: &[(String, String)]) {
    let Ok(root) = json::parse_object(root_body) else { return };
    let mut questions = std::collections::HashMap::new();
    collect_questions(&Value::Object(root), &mut questions, 0);
    for item in items.iter_mut().filter(|it| it.kind == ItemKind::Answer) {
        let Some(q) = questions.get(&item.id) else { continue };
        item.author = None;
        let question_text = json::text(q, "excerpt")
            .or_else(|| json::text(q, "detail"))
            .map(|s| strip_html(&s))
            .filter(|s| !s.trim().is_empty());
        if question_text.is_some() {
            item.summary = question_text;
        }
        item.media = Vec::new();
        item.stats = Stats {
            comments: json::long(q, "answer_count").filter(|v| *v > 0),
            ..Stats::default()
        };
        let texts = [item.title.as_deref(), item.summary.as_deref()];
        item.emotes = emotes_used(emotes, &texts);
    }
}

/// 整页里每条回答（`type == "answer"`）→ 它的 `question` 对象，键是条目 id（`answer:<id>`）。
fn collect_questions(v: &Value, out: &mut std::collections::HashMap<String, JsonMap>, depth: usize) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Object(o) => {
            if json::string(o, "type").as_deref() == Some("answer") {
                if let (Some(id), Some(q)) = (json::string(o, "id"), json::obj(o, "question")) {
                    out.entry(format!("answer:{id}")).or_insert_with(|| q.clone());
                }
            }
            for child in o.values() {
                collect_questions(child, out, depth + 1);
            }
        }
        Value::Array(a) => a.iter().for_each(|child| collect_questions(child, out, depth + 1)),
        _ => {}
    }
}

/// 关注动态：`/api/v3/moments`。
pub fn parse_moments(
    body: &str,
    http_code: i32,
    location: Option<&str>,
    emotes: &[(String, String)],
) -> Result<Page, ZhihuError> {
    let root = check_http(http_code, body, location)?;
    page(&root, "关注动态", emotes)
}

fn page(root: &JsonMap, what: &str, emotes: &[(String, String)]) -> Result<Page, ZhihuError> {
    let data = json::arr(root, "data").ok_or_else(|| ZhihuError::Parse {
        detail: format!("{what} 缺少 data"),
    })?;
    let mut items: Vec<FeedItem> = Vec::new();
    for el in data {
        let Some(o) = el.as_object() else { continue };
        items.extend(card(o, emotes));
    }
    // Kotlin `distinctBy { it.id }`：保留第一次出现的那条
    let mut seen = std::collections::HashSet::new();
    items.retain(|it| seen.insert(it.id.clone()));
    Ok(Page {
        items,
        next: next_cursor(root, what)?,
    })
}

/// 游标是 `paging.next` 里 `?` 之后的部分（路径和域名由 Kotlin 侧固定）。
/// `is_end` 或没有 next 就是最后一页。
fn next_cursor(root: &JsonMap, what: &str) -> Result<Option<String>, ZhihuError> {
    let paging = json::obj(root, "paging").ok_or_else(|| ZhihuError::Parse {
        detail: format!("{what} 缺少 paging"),
    })?;
    if bool_of(paging, "is_end") == Some(true) {
        return Ok(None);
    }
    Ok(json::text(paging, "next")
        .and_then(|next| next.split_once('?').map(|(_, q)| q.to_string()))
        .filter(|q| !q.is_empty()))
}

/// 一张外层卡片，可能展开成多条（`feed_group`）。广告和不认识的卡片返回空。
fn card(o: &JsonMap, emotes: &[(String, String)]) -> Vec<FeedItem> {
    match json::string(o, "type").as_deref() {
        Some("feed_group") => json::arr(o, "list")
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_object())
                    .flat_map(|item| card(item, emotes))
                    .collect()
            })
            .unwrap_or_default(),
        Some("feed_advert") => Vec::new(),
        _ => feed_card(o, emotes).into_iter().collect(),
    }
}

fn feed_card(o: &JsonMap, emotes: &[(String, String)]) -> Option<FeedItem> {
    if is_ad(o) {
        return None;
    }
    let target = json::obj(o, "target")?;
    let mut item = target_item(target, emotes)?;
    item.reason = reason(o);
    Some(item)
}

/// 广告：`feed_advert`（在 [`card`] 里丢掉）、带 `promotion_extra`（"知+"推广的竞价参数 / 追踪地址）、
/// 或卡片上直接挂 `ad` 对象。注意 `promotion_extra` 是空字符串 / null / 数字时**不算**广告（和 Kotlin `text` / `obj` 一致）。
fn is_ad(o: &JsonMap) -> bool {
    json::text(o, "promotion_extra").is_some()
        || json::obj(o, "promotion_extra").is_some()
        || json::obj(o, "ad").is_some()
}

/// 动作说明："张三 赞同了回答"。关注动态里 `actors` 是做这件事的人，`action_text` 是动作；
/// 动作里已经有人名时不重复拼。
fn reason(o: &JsonMap) -> Option<String> {
    let action = json::text(o, "action_text")
        .map(|s| strip_html(&s))
        .filter(|s| !s.trim().is_empty());
    let actors: Vec<String> = json::arr(o, "actors")
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|a| json::text(a, "name"))
                .collect()
        })
        .unwrap_or_default();
    let action = action?;
    if actors.is_empty() || actors.iter().any(|name| action.contains(name.as_str())) {
        Some(action)
    } else {
        Some(format!("{} {}", actors.join("、"), action))
    }
}

/// 一个 `target` 对象 → [`FeedItem`]（不含 reason）。不认识的类型返回 `None`。
pub fn target_item(t: &JsonMap, emotes: &[(String, String)]) -> Option<FeedItem> {
    let mut item = match json::string(t, "type").as_deref() {
        Some("answer") => answer(t),
        Some("article") => article(t),
        Some("pin") => pin(t),
        Some("zvideo") => zvideo(t),
        Some("question") => question(t),
        _ => None,
    }?;
    let texts = [item.title.as_deref(), item.summary.as_deref()];
    item.emotes = emotes_used(emotes, &texts);
    Some(item)
}

/// 解析一份**不带外壳**的 `target` 对象（详情接口的响应就是这个形状）。
///
/// 生产上的信息流不走这里（走 [`parse_recommend`] / [`parse_moments`]）；它是给对拍覆盖
/// 详情 fixture（回答 / 文章 / 想法 / 视频 / 问题）用的，以后搬详情时也能直接接上。
pub fn parse_target_object(
    body: &str,
    emotes: &[(String, String)],
) -> Result<Option<FeedItem>, ZhihuError> {
    let map = json::parse_object(body).map_err(|detail| ZhihuError::Parse { detail })?;
    Ok(target_item(&map, emotes))
}

fn answer(t: &JsonMap) -> Option<FeedItem> {
    let id = json::string(t, "id")?;
    let q = json::obj(t, "question")?;
    let qid = json::string(q, "id")?;
    let item_id = format!("answer:{id}");
    let mut item = base(
        item_id.clone(),
        ItemKind::Answer,
        answer_url(&qid, &id),
        DetailTarget::Native {
            id: item_id,
            platform: None,
        },
    );
    item.title = json::text(q, "title").or_else(|| json::text(q, "name"));
    item.summary = excerpt(t);
    item.author = json::obj(t, "author").and_then(user);
    item.published_at = json::long(t, "created_time").filter(|v| *v > 0);
    item.media = thumbnails(t);
    item.stats = Stats {
        likes: json::long(t, "voteup_count"),
        comments: json::long(t, "comment_count"),
        ..Stats::default()
    };
    Some(item)
}

fn article(t: &JsonMap) -> Option<FeedItem> {
    let id = json::string(t, "id")?;
    let item_id = format!("article:{id}");
    let mut item = base(
        item_id.clone(),
        ItemKind::Article,
        article_url(&id),
        DetailTarget::Native {
            id: item_id,
            platform: None,
        },
    );
    item.title = json::text(t, "title");
    item.summary = excerpt(t);
    item.author = json::obj(t, "author").and_then(user);
    item.published_at = json::long(t, "created")
        .or_else(|| json::long(t, "created_time"))
        .filter(|v| *v > 0);
    let thumbs = thumbnails(t);
    item.media = if thumbs.is_empty() {
        json::text(t, "image_url")
            .map(|u| {
                vec![Media::Image {
                    url: json::to_https(&u),
                    width: None,
                    height: None,
                }]
            })
            .unwrap_or_default()
    } else {
        thumbs
    };
    item.stats = Stats {
        likes: json::long(t, "voteup_count"),
        comments: json::long(t, "comment_count"),
        ..Stats::default()
    };
    Some(item)
}

/// 想法：正文在 `content` 数组里（text / image / link / video……）。
fn pin(t: &JsonMap) -> Option<FeedItem> {
    let id = json::string(t, "id")?;
    let item_id = format!("pin:{id}");
    let mut item = base(
        item_id.clone(),
        ItemKind::Moment,
        pin_url(&id),
        DetailTarget::Native {
            id: item_id,
            platform: None,
        },
    );
    let content: Vec<&JsonMap> = json::arr(t, "content")
        .map(|arr| arr.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    let text = content
        .iter()
        .filter(|c| json::string(c, "type").as_deref() == Some("text"))
        .filter_map(|c| json::text(c, "content").map(|s| strip_html(&s)))
        .collect::<Vec<_>>()
        .join("\n");
    let text = if text.trim().is_empty() {
        None
    } else {
        Some(text)
    };
    item.summary = text.or_else(|| {
        json::text(t, "excerpt_title").map(|s| strip_html(&s))
    });
    item.author = json::obj(t, "author").and_then(user);
    item.published_at = json::long(t, "created").filter(|v| *v > 0);
    item.media = content
        .iter()
        .filter(|c| json::string(c, "type").as_deref() == Some("image"))
        .filter_map(|c| {
            let url = json::text(c, "url").or_else(|| json::text(c, "original_url"))?;
            Some(Media::Image {
                url: json::to_https(&url),
                width: json::int(c, "width"),
                height: json::int(c, "height"),
            })
        })
        .collect();
    item.stats = Stats {
        likes: json::long(t, "like_count").or_else(|| json::long(t, "reaction_count")),
        comments: json::long(t, "comment_count"),
        ..Stats::default()
    };
    Some(item)
}

fn zvideo(t: &JsonMap) -> Option<FeedItem> {
    let id = json::string(t, "id")?;
    let item_id = format!("zvideo:{id}");
    let mut item = base(
        item_id.clone(),
        ItemKind::Video,
        zvideo_url(&id),
        DetailTarget::Native {
            id: item_id,
            platform: None,
        },
    );
    let video = json::obj(t, "video");
    item.title = json::text(t, "title");
    item.summary = json::text(t, "description")
        .or_else(|| json::text(t, "excerpt"))
        .map(|s| strip_html(&s))
        .filter(|s| !s.trim().is_empty());
    item.author = json::obj(t, "author").and_then(user);
    item.published_at = json::long(t, "published_at")
        .or_else(|| json::long(t, "created_at"))
        .filter(|v| *v > 0);
    let cover = video
        .and_then(|v| json::text(v, "thumbnail"))
        .or_else(|| json::text(t, "image_url"))
        .map(|u| json::to_https(&u));
    item.media = vec![Media::Video {
        cover,
        duration_sec: video
            .and_then(|v| json::long(v, "duration"))
            .map(|n| n as i32),
        // 宽高带上，卡片和沉浸界面一开始就按竖屏 / 横屏摆
        width: video.and_then(|v| json::int(v, "width")).filter(|n| *n > 0),
        height: video.and_then(|v| json::int(v, "height")).filter(|n| *n > 0),
    }];
    item.stats = Stats {
        views: video
            .and_then(|v| json::long(v, "play_count"))
            .or_else(|| json::long(t, "play_count")),
        likes: json::long(t, "voteup_count").or_else(|| json::long(t, "vote_count")),
        comments: json::long(t, "comment_count"),
        ..Stats::default()
    };
    Some(item)
}

fn question(t: &JsonMap) -> Option<FeedItem> {
    let id = json::string(t, "id")?;
    let item_id = format!("question:{id}");
    let mut item = base(
        item_id.clone(),
        ItemKind::Question,
        question_url(&id),
        DetailTarget::Native {
            id: item_id,
            platform: None,
        },
    );
    item.title = json::text(t, "title").or_else(|| json::text(t, "name"));
    item.summary = json::text(t, "excerpt")
        .or_else(|| json::text(t, "detail"))
        .map(|s| strip_html(&s))
        .filter(|s| !s.trim().is_empty());
    item.author = json::obj(t, "author").and_then(user);
    item.published_at = json::long(t, "created").filter(|v| *v > 0);
    item.stats = Stats {
        comments: json::long(t, "comment_count"),
        ..Stats::default()
    };
    Some(item)
}

fn excerpt(t: &JsonMap) -> Option<String> {
    json::text(t, "excerpt")
        .or_else(|| json::text(t, "excerpt_new"))
        .map(|s| strip_html(&s))
        .filter(|s| !s.trim().is_empty())
}

/// 回答 / 文章的配图：`thumbnails`（字符串数组），没有就退回单张 `thumbnail`。
fn thumbnails(t: &JsonMap) -> Vec<Media> {
    let mut urls: Vec<String> = json::arr(t, "thumbnails")
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        if let Some(single) = json::text(t, "thumbnail") {
            urls.push(single);
        }
    }
    let mut seen = std::collections::HashSet::new();
    urls.into_iter()
        .filter(|u| !u.trim().is_empty())
        .filter(|u| seen.insert(u.clone()))
        .map(|url| Media::Image {
            url: json::to_https(&url),
            width: None,
            height: None,
        })
        .collect()
}

/// 知乎用户。匿名用户 id 是 `"0"`、没有 `url_token`，保留名字（"匿名用户"），不给主页地址。
pub(crate) fn user(a: &JsonMap) -> Option<UserRef> {
    let id = json::text(a, "id")?;
    let name = json::text(a, "name")?;
    let token = json::text(a, "url_token");
    Some(UserRef {
        platform: ZHIHU.to_string(),
        id: id.clone(),
        name,
        avatar: json::text(a, "avatar_url").map(|u| json::to_https(&u)),
        url: if id != "0" {
            token.map(|t| people_url(&t))
        } else {
            None
        },
    })
}

fn base(id: String, kind: ItemKind, url: String, detail: DetailTarget) -> FeedItem {
    FeedItem {
        platform: ZHIHU.to_string(),
        id,
        kind,
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
        adult: false,
        url: Some(url),
        detail,
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// Kotlin `JsonObject.bool` 的精确移植：`booleanOrNull` 之后是 `longOrNull != 0`。
/// 共享的 `json::as_bool` 对 `1.5` 这种数字返回 None，Kotlin 会走 `1 != 0` → true（B站那边没用
/// 到这种形态，这里也不改共享实现，只在知乎这边按 Kotlin 的语义取）。
pub(super) fn bool_of(m: &JsonMap, key: &str) -> Option<bool> {
    // kotlinx 的 `booleanOrNull` 对**字符串** "true" / "false" 也给值（`content.toBooleanStrictOrNull()`），
    // 所以这一步就是共享的 `json::as_bool`；它认不出来的再走 `longOrNull != 0`。
    match m.get(key)? {
        Value::Bool(b) => Some(*b),
        other => json::as_bool(other).or_else(|| json::as_long(other).map(|n| n != 0)),
    }
}

/// `Emotes.used`：挑出标题 / 摘要里**真的出现过**的表情代码，保持表情表里的顺序。
fn emotes_used(table: &[(String, String)], texts: &[Option<&str>]) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let present: Vec<&str> = texts
        .iter()
        .flatten()
        .copied()
        .filter(|t| !t.is_empty())
        .collect();
    if present.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (code, url) in table {
        if code.is_empty() || url.is_empty() {
            continue;
        }
        if present.iter().any(|t| t.contains(code.as_str())) {
            out.push((code.clone(), url.clone()));
        }
    }
    out
}

/// `core/Text.kt` 的 `stripHtml`。
///
/// 原来这里是一份自己写的
/// 字符扫描（和 `html/mod.rs` 那份的差别只在 trim 的空白口径：这里用 Rust 的 `trim`，
/// 会多去掉 `U+0085`、少去掉 `U+001C..U+001F`；Kotlin 的 `String.trim()` 用的是
/// `Char.isWhitespace`，`html::kotlin_trim` 才是那个口径）。
fn strip_html(html: &str) -> String {
    crate::html::strip_html(html)
}

