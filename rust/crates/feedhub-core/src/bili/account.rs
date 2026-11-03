//! B站 的**账号层**：`whoami` / 关注列表 / 关注·取关 / 网页扫码登录的解析。
//!
//! 逐条对照 `source-bilibili/BilibiliSource.kt` 的同名方法、`BiliFollowParser.kt`、`BiliQrParser.kt`
//! （以及 `BiliParser.nav` / `spaceUrl`）。请求怎么拼（URL / 参数顺序 / 头顺序 / 表单 body）在
//! [`super::source`] 的平台对象里，这里只放**纯解析**，方便对拍（同一份 body 两边逐字段比）和排障。
//!
//! 三条容易踩的：
//! 1. 扫码**成功之前不动 `ctx.cookies`**：临时罐在宿主（[`crate::host::QrHost`]），只有
//!    [`super::source::BiliHostedSource::commit_qr_login`] 那一步才写进真正的罐子；
//! 2. 登录 Cookie 的**属性**要带齐：`SESSDATA` 是 `HttpOnly` + `Secure`，其余三个是 `Secure`，
//!    域是 `bilibili.com`（**域 Cookie**，不是 host-only——不然 `www.bilibili.com` 上带不上）；
//! 3. 轮询的状态码只有四个：86101 未扫 / 86090 已扫待确认 / 86038 失效 / 0 成功，别的都是 `Failed`。

use crate::account::{FollowPage, QrState};
use crate::detail::{FollowKind, FollowTarget};
use crate::host::JarCookie;
use crate::json::{self, JsonMap};
use crate::model::{BiliError, UserRef};

/// 登录 Cookie 的名字（照浏览器登录后的 Cookie 和 `crossDomain` 的参数）。
pub const LOGIN_COOKIES: [&str; 4] = ["SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMd5"];

/// 换账号时要先清掉的旧登录 Cookie：上面四个加 `sid`；`buvid3` / `buvid4` / `b_nut` 这些设备标识不动。
pub const OLD_LOGIN_NAMES: [&str; 5] = [
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "sid",
];

/// `passport` 给的 `data.url` 里没有 `Expires`（或已经过期）时的有效期：180 天。
const DEFAULT_TTL_MS: i64 = 180 * 24 * 3600 * 1000;

// ===========================================================================
// 我是谁（`/x/web-interface/nav` 的 `data`）
// ===========================================================================

/// `nav` 的 `data` → 账号。`isLogin != true` 或 mid 是空 / "0" 就是 `None`（Kotlin `BiliParser.nav`）。
pub fn nav_user(root: &JsonMap) -> Option<UserRef> {
    let data = json::obj(root, "data")?;
    if json::bool(data, "isLogin") != Some(true) {
        return None;
    }
    user(
        json::string(data, "mid").as_deref(),
        json::text(data, "uname").as_deref(),
        json::text(data, "face").as_deref(),
    )
}

/// Kotlin `BiliParser.user`：mid 空 / "0" 就是 `None`；名字缺了用 mid；头像转 https；主页固定。
pub fn user(mid: Option<&str>, name: Option<&str>, face: Option<&str>) -> Option<UserRef> {
    let mid = mid.filter(|m| !m.is_empty() && *m != "0")?;
    Some(UserRef {
        platform: "bili".to_string(),
        id: mid.to_string(),
        name: name.map(|n| n.to_string()).unwrap_or_else(|| mid.to_string()),
        avatar: face.map(json::to_https),
        url: Some(format!("https://space.bilibili.com/{mid}")),
    })
}

// ===========================================================================
// 关注列表（`/x/relation/followings`）
// ===========================================================================

/// 关注列表一页（Kotlin `BiliFollowParser.followings`）：页码翻页，`data.total` 是关注总数；
/// 已取条数 ≥ total 或这页是空的就到头了。
pub fn followings_page(root: &JsonMap, pn: i32, ps: i32) -> Result<FollowPage, BiliError> {
    let data = json::obj(root, "data").ok_or_else(|| BiliError::Parse {
        detail: "关注列表缺少 data".to_string(),
    })?;
    let items: Vec<FollowTarget> = json::arr(data, "list")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(follow_target)
                .collect()
        })
        .unwrap_or_default();
    let total = json::long(data, "total").unwrap_or(0);
    let next = if !items.is_empty() && (pn as i64) * (ps as i64) < total {
        Some((pn + 1).to_string())
    } else {
        None
    };
    Ok(FollowPage { items, next })
}

/// 一条关注对象：`mid` 是空 / "0" 就跳过（Kotlin `mapNotNull`）。
fn follow_target(o: &JsonMap) -> Option<FollowTarget> {
    let mid = json::string(o, "mid").filter(|m| !m.is_empty() && m != "0")?;
    Some(FollowTarget {
        platform: "bili".to_string(),
        kind: FollowKind::User,
        id: mid.clone(),
        name: json::text(o, "uname").unwrap_or_else(|| mid.clone()),
        avatar: json::text(o, "face").map(|f| json::to_https(&f)),
        description: json::text(o, "sign"),
        url: Some(format!("https://space.bilibili.com/{mid}")),
    })
}

// ===========================================================================
// 扫码登录（`passport.bilibili.com/x/passport-login/web/qrcode/{generate,poll}`）
// ===========================================================================

/// `generate` 的 `data`：二维码内容（`url`）和轮询用的 key，缺一个就报解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrGenerated {
    pub url: String,
    pub key: String,
}

pub fn qr_generated(root: &JsonMap) -> Result<QrGenerated, BiliError> {
    let data = json::obj(root, "data").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 data".to_string(),
    })?;
    let url = json::text(data, "url").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 url".to_string(),
    })?;
    let key = json::text(data, "qrcode_key").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 qrcode_key".to_string(),
    })?;
    Ok(QrGenerated { url, key })
}

/// `poll` 的 `data`（**不是**外壳的 code）：`code` 决定状态，`message` 是给人看的，`url` 里带登录 Cookie。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPoll {
    pub code: i64,
    pub message: String,
    pub url: Option<String>,
}

pub fn qr_poll(root: &JsonMap) -> Result<QrPoll, BiliError> {
    let data = json::obj(root, "data").ok_or_else(|| BiliError::Parse {
        detail: "轮询响应缺少 data".to_string(),
    })?;
    let code = json::long(data, "code").ok_or_else(|| BiliError::Parse {
        detail: "轮询响应缺少 data.code".to_string(),
    })?;
    Ok(QrPoll {
        code,
        message: json::text(data, "message").unwrap_or_default(),
        url: json::text(data, "url"),
    })
}

/// 轮询状态码 → 状态（成功 0 由调用方单独处理，要写 Cookie）。86101 未扫 / 86090 已扫待确认 / 86038 失效。
pub fn qr_waiting(code: i64) -> Option<QrState> {
    match code {
        86101 => Some(QrState::Waiting),
        86090 => Some(QrState::Scanned),
        86038 => Some(QrState::Expired),
        _ => None,
    }
}

/// 登录成功时 `data.url` 是个跨域登录跳转地址：
/// `…/crossDomain?DedeUserID=…&DedeUserID__ckMd5=…&Expires=…&SESSDATA=…&bili_jct=…&gourl=…`。
/// 登录 Cookie 的值都在 query 里（取**未解码**的原样，和 Set-Cookie 里的写法一样，例如 SESSDATA 里的逗号是 `%2C`）。
/// 正常情况 Cookie 由 Set-Cookie 种进临时罐，这个只在罐里没有 `SESSDATA` 时当后备。
///
/// 属性照 `BiliQrParser.cookiesFromUrl`：域 `bilibili.com`（**域 Cookie**）、路径 `/`、
/// `SESSDATA` 是 `HttpOnly` + `Secure`，其余三个只 `Secure`。过期时间优先用 `Expires`（Unix 秒），否则 180 天。
pub fn cookies_from_url(url: Option<&str>, now_ms: i64) -> Vec<JarCookie> {
    let Some(url) = url else {
        return Vec::new();
    };
    // Kotlin 用 `toHttpUrlOrNull()?.encodedQuery`：解析失败就是空。
    let Some(query) = encoded_query(url) else {
        return Vec::new();
    };
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|p| {
            let i = p.find('=')?;
            if i == 0 {
                None
            } else {
                Some((p[..i].to_string(), p[i + 1..].to_string()))
            }
        })
        .collect();
    let lookup = |name: &str| -> Option<String> {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let expires = lookup("Expires")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|sec| sec.saturating_mul(1000))
        .filter(|ms| *ms > now_ms)
        .unwrap_or(now_ms.saturating_add(DEFAULT_TTL_MS));
    LOGIN_COOKIES
        .iter()
        .filter_map(|name| {
            let value = lookup(name).filter(|v| !v.is_empty())?;
            Some(JarCookie {
                name: (*name).to_string(),
                value,
                domain: "bilibili.com".to_string(),
                path: "/".to_string(),
                expires_at_ms: expires,
                secure: true,
                http_only: *name == "SESSDATA",
                host_only: false,
            })
        })
        .collect()
}

/// `HttpUrl.encodedQuery` 的近似：能按 OkHttp 认出来（有 scheme + host），否则 `None`。
///
/// 这里不重写整个 URL 解析器：`passport` 给的地址一定是 `https://…?…`，只要"有 scheme、有 host、
/// 有一个 `?`"就取问号后面那段；Kotlin 那边用的是 `toHttpUrlOrNull()`，对这几条用例结果一样
/// （`BiliQrLoginTest` 里成功响应三种形状都覆盖）。
fn encoded_query(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let path_start = rest.find(['/', '?'])?;
    let host = &rest[..path_start];
    if host.is_empty() {
        return None;
    }
    let query_start = url.find('?')?;
    Some(url[query_start + 1..].to_string())
}

