//! 酷安：推荐流（`/v6/main/indexV8`）/ 关注流（`/v6/page/dataList`）的解析，
//! `X-App-Token` 的 v2 / v3 计算，以及**两条流的整个请求逻辑**（[`source`]）。
//!
//! 对应 `source-coolapk` 的 [CoolapkParser] / [CoolapkHtml] / [CoolapkEmoji] / [CoolapkVideo] 里
//! **信息流卡片用得到的那部分**，逐条照 Kotlin 的语义移植（`CoolapkDualRunTest` 用同一份 body 两边跑、逐字段比）。
//!
//! 两半：
//! - 本模块 + `html` / `emoji` / `video` / `token` / `bcrypt`：**纯函数**（解析 / 签名），不发请求、不碰 Cookie、不落盘；
//! - [`source`]：**平台对象** `CoolapkHostedSource`——拼 URL / 参数顺序 / 请求头顺序 / 翻页游标、`X-App-Token`
//!   的计算与按秒缓存、状态码与错误码判定，收发通过 `crate::host` 的三个 trait 反向回调宿主。
//!
//! 三个模块：
//! - [`detail`]：`/v6/feed/detail`（图文 / 普通动态 / 托管视频 / 站外视频卡片）；
//! - [`comment`]：`replyList` 的一级评论 / 楼中楼（`feedType=feed_reply`）；
//! - [`source`]：还包括详情 / 评论 / 楼中楼三个方法的请求（含**评论那套 16.2.0 / API 16 / v3 身份**、
//!   人机验证的识别、`b23.tv` 短链跟一次重定向）。
//!
//! **没搬**：只剩扫码和 `raw`（开发者工具）。
//!
//! 边界（和 B站 / 知乎一样）：**不发请求、不碰 Cookie、不落盘**。设备 / 安装时间只从宿主 store 读，
//! 时间戳由调用方注入，表情表由插件给——所以这里没有随机数、没有系统时间。
//!
//! [CoolapkParser]: ../../../source-coolapk/src/main/kotlin/feedhub/source/coolapk/CoolapkParser.kt

pub mod bcrypt;
pub mod comment;
pub mod detail;
pub mod emoji;
pub mod html;
pub mod search;
pub mod source;
pub mod token;
pub mod video;

use crate::hosted::is_kotlin_blank;
use crate::json;
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

use emoji::EmoteTable;

/// 平台 id（和 Kotlin `Platform.define("coolapk", …)` 一样，定了就别改）。
pub const COOLAPK: &str = "coolapk";

/// 酷安网页根地址（`CoolapkHtml.WEB`）。
pub const WEB: &str = "https://www.coolapk.com";

/// bcrypt 自己的字母表（`AppToken` 里 v3 的 salt 末位按这张表回退 5 位，bcrypt 的编码也用它）。
pub(crate) const COOLAPK_BCRYPT_ALPHABET: &str =
    "./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Rust 侧的业务错误。FFI 会把它拆成带类型的 [`FfiError`]（见 `feedhub-ffi` 的 `coolapk.rs`）。
///
/// 和 Kotlin `CoolapkSource.checkStatus` 抛的那几种一一对应：
/// `SignatureException` / `NotLoggedInException` / `RiskControlException` / `ApiException` / `ParseException`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoolapkError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：验证码、请求被拦截、账号异常。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl { code: Option<String>, detail: String },

    /// **要人机验证**（`err_request_captcha_v2`，酷安评论 / 楼中楼那条路）。
    ///
    /// 宿主拿到之后照 Kotlin `CoolapkSource.getJson` 的流程办：让**用户本人**完成一次验证
    /// （`ctx.captcha`），把答案（`validate` cookie 或 `field` 查询参数）带上**重试同一个请求一次**；
    /// 再要验证就报风控错。**不循环、不自动绕、不改版本号**。
    #[error("需要人机验证：{detail}")]
    Captcha {
        /// 服务端下发的验证码 id（没下发时是酷安网页那版默认 id）。
        captcha_id: String,
        /// 空 = 答案写进 `validate` cookie；非空 = 作为这个查询参数回传。
        field: String,
        /// 服务端原话（`message`），界面可以直接显示。
        detail: String,
    },

    #[error("签名被拒：{detail}")]
    Signature { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 网络错误（连不上 / 超时 / 连接被重置）——只会从**平台对象**的宿主回调回来。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题），别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },

    /// 内部错误（比如 jBCrypt 也会抛的 salt 长度问题；或 store 里缺设备 / 安装时间）。
    #[error("内部错误：{detail}")]
    Internal { detail: String },
}

/// 哪一条流（对应 Kotlin 的 `FeedKind` 里酷安支持的两个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

/// 分页游标（Kotlin `FeedCursor`）：`page` 从 1 开始，`firstItem` 是第一页第一条内容的 id，
/// `lastItem` 是上一页最后一条的 id。**编码 / 解析在 Kotlin**，这里只负责"下一页是什么"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    pub page: i64,
    pub first_item: Option<String>,
    pub last_item: Option<String>,
}

/// 内容的网页地址（`CoolapkParser.feedUrl`）。
pub fn feed_url(id: &str) -> String {
    format!("{WEB}/feed/{id}")
}

/// 用户主页地址（`CoolapkHtml.userUrl`）。
pub fn user_url(uid: &str) -> String {
    format!("{WEB}/u/{uid}")
}

// ===========================================================================
// 列表
// ===========================================================================

/// 一页信息流。`body` 是响应原文，`http_code` 是 HTTP 状态码——**错误映射也在这里做**
/// （Kotlin `CoolapkSource.getJson` 的尾巴：先按 JSON 里的 message 判断，解析不了才按状态码），
/// 这样对拍连错误语义都能一起比。
///
/// `emotes` 是"正文里的原文 → 图片地址"（Kotlin `CoolapkPlugin.emotes.codes`），表归插件所有，这里只按表挑。
pub fn parse_feed(
    kind: FeedKind,
    body: &str,
    http_code: i32,
    cursor: &Cursor,
    emotes: &[(String, String)],
) -> Result<Page, CoolapkError> {
    let table = EmoteTable::new(emotes);
    let json = decode_json(body, http_code)?;

    // 登录失效时关注流不报错，而是回一张 unLoginCard + 一堆"值得看"的文章（见 `CoolapkSource.feed`）。
    if kind == FeedKind::Following && is_un_login_page(&json) {
        return Err(CoolapkError::NotLoggedIn { detail: None });
    }
    feed_list(&json, cursor, &table)
}

/// `{"data": [实体...]}` → 只要 `entityType == "feed"` 的实体，卡片（轮播图 / 金刚区 / 广告）全丢掉。
pub fn feed_list(
    json: &json::JsonMap,
    cursor: &Cursor,
    emotes: &EmoteTable,
) -> Result<Page, CoolapkError> {
    let data = json::arr(json, "data").ok_or_else(|| CoolapkError::Parse {
        detail: "列表缺少 data 数组".to_string(),
    })?;
    let entities: Vec<&json::JsonMap> = data.iter().filter_map(|v| v.as_object()).collect();
    let items: Vec<FeedItem> = entities
        .iter()
        .filter(|o| json::string(o, "entityType").as_deref() == Some("feed"))
        .filter_map(|o| feed(o, emotes, 0))
        .collect();
    let next = next_cursor(&entities, "feed", cursor);
    Ok(Page { items, next })
}

/// 关注流在没登录 / 登录失效时返回的页面：第一张卡是 `unLoginCard`。
pub fn is_un_login_page(json: &json::JsonMap) -> bool {
    json::arr(json, "data")
        .map(|data| {
            data.iter()
                .any(|v| match v.as_object() {
                    Some(o) => json::string(o, "entityTemplate").as_deref() == Some("unLoginCard"),
                    None => false,
                })
        })
        .unwrap_or(false)
}

/// 下一页：`page + 1` + `firstItem` / `lastItem`（卡片没有 id，所以只看内容实体）。
/// 一条内容都没有就是到头了。和 Kotlin `CoolapkParser.nextCursor` 一样。
pub(crate) fn next_cursor(entities: &[&json::JsonMap], entity_type: &str, cursor: &Cursor) -> Option<String> {
    let ids: Vec<String> = entities
        .iter()
        .filter(|o| json::string(o, "entityType").as_deref() == Some(entity_type))
        .filter_map(|o| id(o))
        .collect();
    if ids.is_empty() {
        return None;
    }
    let first = cursor
        .first_item
        .clone()
        .unwrap_or_else(|| ids.first().expect("ids 非空").clone());
    let last = ids.last().expect("ids 非空").clone();
    Some(format!(
        "page={}&firstItem={}&lastItem={}",
        cursor.page + 1,
        first,
        last
    ))
}

// ===========================================================================
// 内容
// ===========================================================================

/// 一条 `feed` 实体。类型看 `feedType`，标题只用 `message_title`（`title` 是服务端拼的"xxx的动态"）。
///
/// `depth` 只用于转发的原内容（`forwardSourceFeed` / `sourceFeed`）：嵌套超过 32 层就放弃那一层，
/// 免得恶意 / 坏数据把栈打穿（Kotlin 那边会 StackOverflowError）。
pub fn feed(o: &json::JsonMap, emotes: &EmoteTable, depth: u32) -> Option<FeedItem> {
    let id = id(o)?;
    let feed_type = json::text(o, "feedType");
    let title_plain = json::text(o, "message_title")
        .map(|html| html::to_plain(&html, emotes))
        .filter(|p| !p.text.trim().is_empty());
    let summary_plain = json::text(o, "message")
        .map(|html| html::to_plain(&html, emotes))
        .filter(|p| !p.text.trim().is_empty());
    let title = title_plain.as_ref().map(|p| p.text.clone());
    let summary = summary_plain.as_ref().map(|p| p.text.clone());

    let video = video::video(o);
    let mut media: Vec<Media> = Vec::new();
    if let Some(v) = video.clone() {
        media.push(v);
    }
    media.extend(
        pictures(o)
            .into_iter()
            .map(|(url, w, h)| Media::Image {
                url,
                width: w,
                height: h,
            }),
    );
    if title.is_none() && summary.is_none() && media.is_empty() {
        return None;
    }

    let kind = if feed_type.as_deref() == Some("feedArticle") {
        ItemKind::Article
    } else if feed_type.as_deref() == Some("question") {
        ItemKind::Question
    } else if feed_type.as_deref() == Some("answer") {
        ItemKind::Answer
    } else if feed_type.as_deref() == Some("video") || video.is_some() {
        ItemKind::Video
    } else {
        ItemKind::Post
    };

    // `titleText.emotes + summaryText.emotes`：先标题（按出现顺序），再摘要里**多出来的**那些。
    let mut emote_list: Vec<(String, String)> = Vec::new();
    for plain in [&title_plain, &summary_plain] {
        if let Some(p) = plain {
            for (code, url) in &p.emotes {
                if !emote_list.iter().any(|(c, _)| c == code) {
                    emote_list.push((code.clone(), url.clone()));
                }
            }
        }
    }

    let url = feed_url(&id);
    let reason = json::text(o, "infoHtml")
        .map(|html| html::to_plain_text(&html, emotes))
        .filter(|t| !t.is_empty());
    let repost = if depth < 32 {
        json::obj(o, "forwardSourceFeed")
            .or_else(|| json::obj(o, "sourceFeed"))
            .and_then(|inner| feed(inner, emotes, depth + 1))
            .map(Box::new)
    } else {
        None
    };

    Some(FeedItem {
        platform: COOLAPK.to_string(),
        id: id.clone(),
        kind,
        title,
        summary,
        language: None,
        author: author(o),
        published_at: json::long(o, "dateline").filter(|n| *n > 0),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: json::long(o, "likenum"),
            comments: json::long(o, "replynum"),
            shares: json::long(o, "forwardnum"),
            ..Stats::default()
        },
        reason,
        community: community(o),
        community_icon: None,
        repost,
        emotes: emote_list,
        adult: false,
        url: Some(url),
        detail: DetailTarget::Native {
            id,
            platform: None,
        },
    })
}

/// 一份**不带外壳**的 `feed` 实体（测试的边界样本 / 以后搬详情时用）。
pub fn parse_item_json(body: &str, emotes: &[(String, String)]) -> Result<Option<FeedItem>, CoolapkError> {
    let json = json::parse_object(body).map_err(|e| CoolapkError::Parse { detail: e })?;
    let table = EmoteTable::new(emotes);
    Ok(feed(&json, &table, 0))
}

// ===========================================================================
// 账号
// ===========================================================================

/// `/v6/account/checkLoginInfo` 的响应 → 当前用户（`CoolapkParser.account`）。
///
/// 没有 `data`、`data` 里没有 `uid`、`uid` 是空白或 `"0"`——都算"没拿到人"，返回 `None`（Kotlin 一样）。
/// 头像统一成 https；网页地址是 `https://www.coolapk.com/u/<uid>`。
pub fn account(json: &json::JsonMap) -> Option<UserRef> {
    let data = json::obj(json, "data")?;
    let uid = json::string(data, "uid").filter(|s| !is_kotlin_blank(s) && s != "0")?;
    let name = json::text(data, "username").unwrap_or_else(|| uid.clone());
    let avatar = json::text(data, "userAvatar").map(|s| json::to_https(&s));
    Some(UserRef {
        platform: COOLAPK.to_string(),
        url: Some(user_url(&uid)),
        id: uid,
        name,
        avatar,
    })
}

/// `checkLoginInfo` 的**整个**判定（错误映射 + 取人），Kotlin `CoolapkSource.whoami` 的 body：
///
/// `NotLoggedIn` 不往上抛、折成 `Ok(None)`——Kotlin 那里是 `catch (_: NotLoggedInException) { return null }`
/// （服务端对未登录用户回的是 `{status:-1, message:"登录信息有误"}`，**这是正常结果，不是错误**）。
/// 别的错误（风控 / 签名 / 接口码 / 解析）照常抛。
pub fn whoami_from(body: &str, http_code: i32) -> Result<Option<UserRef>, CoolapkError> {
    match decode_json(body, http_code) {
        Ok(json) => Ok(account(&json)),
        Err(CoolapkError::NotLoggedIn { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

// ===========================================================================
// 小工具（逐条对 Kotlin `CoolapkParser` 的私有函数）
// ===========================================================================

/// `(string("id") ?: string("entityId"))?.trim()?.takeIf { 非空 && != "0" }`
pub(crate) fn id(o: &json::JsonMap) -> Option<String> {
    json::string(o, "id")
        .or_else(|| json::string(o, "entityId"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "0")
}

pub(crate) fn author(o: &json::JsonMap) -> Option<UserRef> {
    let info = json::obj(o, "userInfo");
    let uid = info
        .and_then(|i| json::string(i, "uid"))
        .or_else(|| json::string(o, "uid"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "0")?;
    let name = info
        .and_then(|i| json::text(i, "username"))
        .or_else(|| json::text(o, "username"))
        .unwrap_or_else(|| uid.clone());
    let avatar = info
        .and_then(|i| json::text(i, "userAvatar"))
        .or_else(|| json::text(o, "userAvatar"))
        .map(|s| json::to_https(&s));
    Some(UserRef {
        platform: COOLAPK.to_string(),
        url: Some(user_url(&uid)),
        id: uid,
        name,
        avatar,
    })
}

/// 话题 / 数码产品 / 看看号：先 `ttitle`，再 `targetRow`，再 `relationRows` 第一个，最后 `dyh_name`。
fn community(o: &json::JsonMap) -> Option<String> {
    if let Some(t) = json::text(o, "ttitle") {
        return Some(t.trim().to_string());
    }
    if let Some(row) = json::obj(o, "targetRow") {
        if let Some(t) = json::text(row, "title") {
            return Some(t.trim().to_string());
        }
    }
    if let Some(rows) = json::arr(o, "relationRows") {
        for row in rows {
            if let Some(t) = row.as_object().and_then(|r| json::text(r, "title")) {
                return Some(t.trim().to_string());
            }
        }
    }
    json::text(o, "dyh_name").map(|t| t.trim().to_string())
}

/// `picArr`（没有就退回 `pic`），去重保序，地址统一 https，顺便从地址里读出宽高。
pub(crate) fn pictures(o: &json::JsonMap) -> Vec<(String, Option<i32>, Option<i32>)> {
    let mut urls: Vec<String> = Vec::new();
    for v in json::arr(o, "picArr").map(|a| a.as_slice()).unwrap_or(&[]) {
        match v {
            serde_json::Value::String(s) => {
                if !s.trim().is_empty() {
                    urls.push(s.clone());
                }
            }
            serde_json::Value::Number(n) => urls.push(n.to_string()),
            serde_json::Value::Bool(b) => urls.push(b.to_string()),
            _ => {}
        }
    }
    if urls.is_empty() {
        if let Some(pic) = json::text(o, "pic") {
            urls.push(pic);
        }
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for u in urls {
        if seen.contains(&u) {
            continue;
        }
        let (w, h) = html::image_size(&u);
        out.push((json::to_https(&u), w, h));
        seen.push(u);
    }
    out
}

// ===========================================================================
// 错误映射（Kotlin `CoolapkSource.getJson` 的尾巴 + 同文件的 `checkStatus`）
// ===========================================================================

/// 把"HTTP 响应"变成"JSON 或者一条带类型的错误"（自己再解析一遍）。
pub(crate) fn decode_json(body: &str, http_code: i32) -> Result<json::JsonMap, CoolapkError> {
    decode_parsed(body, http_code, json::parse_object(body).ok().as_ref())
}

/// 同上，但 JSON 已经解析过了（评论那边要先看是不是"要人机验证"，只解析一次）。
pub(crate) fn decode_parsed(
    body: &str,
    http_code: i32,
    parsed: Option<&json::JsonMap>,
) -> Result<json::JsonMap, CoolapkError> {
    if let Some(json) = parsed {
        // 出错时酷安也可能回非 200 + JSON 消息，先按消息判断，解析不了再按状态码
        if (200..=299).contains(&http_code) || json.contains_key("message") {
            check_status(json)?;
            return Ok(json.clone());
        }
    }
    Err(status_error(http_code, body))
}

pub(crate) fn status_error(http_code: i32, body: &str) -> CoolapkError {
    let short = json::take_chars(body, 200);
    match http_code {
        200..=299 => CoolapkError::Parse {
            detail: format!("响应不是 JSON：{short}"),
        },
        401 => CoolapkError::NotLoggedIn { detail: None },
        403 => CoolapkError::RiskControl {
            code: Some("HTTP 403".to_string()),
            detail: "请求被拒绝".to_string(),
        },
        429 => CoolapkError::RiskControl {
            code: Some("HTTP 429".to_string()),
            detail: "请求过于频繁".to_string(),
        },
        other => CoolapkError::Api {
            code: format!("HTTP {other}"),
            detail: short,
        },
    }
}

/// 酷安出错时不给 data，只给 `{status, error, message, messageStatus}`。判断顺序和 Kotlin 一字不差。
pub fn check_status(json: &json::JsonMap) -> Result<(), CoolapkError> {
    let Some(message) = json::string(json, "message") else {
        match json.get("data") {
            None | Some(serde_json::Value::Null) => {
                return Err(CoolapkError::Parse {
                    detail: "响应既没有 data 也没有 message".to_string(),
                })
            }
            _ => return Ok(()),
        }
    };
    let status = json::long(json, "status");
    let message_status = json::string(json, "messageStatus");
    let code = status
        .map(|s| s.to_string())
        .or_else(|| message_status.clone())
        .unwrap_or_else(|| "?".to_string());
    let msg = json::take_chars(&message, 100);

    const SIGNATURE_STATUS: [i64; 2] = [1001, 1004];
    const APP_VERSION_WORDS: [&str; 3] = ["升级到新版酷安", "升级到最新版", "请升级酷安"];
    const RISK_WORDS: [&str; 5] = ["验证码", "频繁", "风险", "异常操作", "安全验证"];
    const CAPTCHA_STATUS: &str = "err_request_captcha";

    if status.map(|s| SIGNATURE_STATUS.contains(&s)).unwrap_or(false)
        || msg.contains("请求验证错误")
        || msg.contains("请求方式错误")
    {
        return Err(CoolapkError::Signature {
            detail: format!("{code} {msg}"),
        });
    }
    if APP_VERSION_WORDS.iter().any(|w| msg.contains(w)) {
        return Err(CoolapkError::Api {
            code,
            detail: format!(
                "酷安要求更新 App 版本号（{msg}）：改 AppVersion 的 NAME / CODE / API，步骤见 source-coolapk/README.md"
            ),
        });
    }
    if msg.contains("登录") {
        return Err(CoolapkError::NotLoggedIn {
            detail: Some(format!("未登录或登录已失效（{msg}）")),
        });
    }
    if message_status
        .as_deref()
        .map(|m| m.starts_with(CAPTCHA_STATUS))
        .unwrap_or(false)
        || RISK_WORDS.iter().any(|w| msg.contains(w))
    {
        return Err(CoolapkError::RiskControl {
            code: message_status.or(Some(code)),
            detail: msg,
        });
    }
    Err(CoolapkError::Api { code, detail: msg })
}
