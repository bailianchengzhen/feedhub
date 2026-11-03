//! 知乎的**网页扫码登录**：请求 / 签名 / 状态 / Cookie 落地全在这里。
//!
//! 照 Zhihu++ `account/QrLogin.kt`（`prefetchQrLoginContext` + `requestQrCode`）和
//! `source-zhihu/ZhihuSource.kt` 的 `startQrLogin` / `pollQrLogin` / `qrRequest` / `qrChecked`：
//!
//! 1. `GET /signin?next=%2F`（登录页，服务端种 `_xsrf` / `_zap`）；
//! 2. 临时罐里没有 `d_c0` 时 `POST /udid`（body `{}`，头 `x-xsrftoken`）领一个（失败不影响后面）；
//! 3. `POST /api/v3/account/api/login/qrcode`（body `{}`）→ `{expires_at, link, token}`；
//! 4. 轮询 `GET /api/v3/account/api/login/qrcode/<token>/scan_info`（`x-zse-96` 用**临时罐**里的 `d_c0` 签）；
//! 5. 成功标志是 **`z_c0` 出现**（Set-Cookie 或响应体里的 `cookie(s)` / `z_c0` 字段）；标志有了但 `z_c0` 还没到时
//!    请求一次 `/api/v4/me` 让服务端把 Cookie 种下来；
//! 6. 落地前用临时罐调 `/api/v4/me` 验证；通过后清掉旧的 `z_c0`、写入临时罐里的**全部** Cookie
//!    （`z_c0` 和它配套的 `d_c0` / `_xsrf` 等：这次登录的 `z_c0` 是在这套 `d_c0` 下发的，成对写入）。
//!
//! 所以
//! `start` 先把本机已有的知乎**设备** Cookie（`d_c0` / `__zse_ck` / `_zap` 等，**不含** `z_c0`）复制进临时罐。
//! 这一步要 `ctx.cookies.all()`，所以本模块多一个 [`QrHost::all`] 的调用。
//!
//! 常见错误照 Kotlin：风控（403 / 429 / 4035x / 跳 unhuman）原样抛、**不重试**；这条流程本来就是匿名的，
//! "需要登录"不算登录态问题，当普通接口错误（`ApiException`）。

use std::sync::Arc;

use crate::account::{QrLogin, QrSession, QrSessions, QrState};
use crate::host::{HttpRequest, HttpResponse, JarCookie, QrHost};
use crate::json::{self, JsonMap};
use crate::model::{BiliError as ZhihuError, UserRef};
use crate::zhihu::source::{api_headers, platform_headers, USER_AGENT, WWW};
use crate::zhihu::{self, sign};

/// 登录态 Cookie 的域（`LoginSpec.cookieDomain`，带前导点，`remove` 自己裁）。
pub const LOGIN_DOMAIN: &str = ".zhihu.com";

/// 换账号时要清掉的旧登录 Cookie（只有 `z_c0`；设备 Cookie 不动）。
pub const OLD_LOGIN_NAMES: [&str; 1] = ["z_c0"];

/// 轮询间隔（Kotlin `QR_POLL_INTERVAL_MS`）。
const QR_POLL_INTERVAL_MS: i64 = 2_000;

/// 二维码默认有效期（Kotlin `ZhihuQrParser.DEFAULT_TTL_MS`，没有 `expires_at` 时）。
const DEFAULT_TTL_MS: i64 = 120_000;

/// 响应体里直接带的 Cookie 用多久（Kotlin `YEAR_MS`）。
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;

/// 给用户的提示（和 Kotlin `startQrLogin` 的 `hint` 逐字一样）。
pub const HINT: &str = "用知乎 App 扫码，并在手机上确认登录";

/// 知乎扫码登录的平台对象。
pub struct ZhihuQrSource {
    base: String,
    /// 接口根 + 结尾斜杠（`https://www.zhihu.com/`），Cookie 罐按它取。
    base_url: String,
    /// `base` 的 top private domain（线上 `zhihu.com`，测试里是 MockWebServer 的 host）。
    /// 由 Kotlin 侧按 `HttpUrl.topPrivateDomain()` 算好传进来——Rust 不背公共后缀表。
    cookie_domain: String,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
}

impl ZhihuQrSource {
    pub fn new(base: impl Into<String>, cookie_domain: impl Into<String>, qr: Arc<dyn QrHost>) -> Self {
        let base = base.into();
        let base_url = format!("{base}/");
        Self {
            cookie_domain: cookie_domain.into(),
            base_url,
            base,
            qr: qr.clone(),
            sessions: QrSessions::new(qr),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn start(&self, now_ms: i64, seed: u8) -> Result<QrLogin, ZhihuError> {
        let session = self.qr.begin();
        // 用本机已有的知乎设备 Cookie（d_c0、__zse_ck、_zap 等，不含登录凭据 z_c0）起步：
        // 全新的匿名会话轮询 scan_info 会被风控。
        let domain = self.cookie_domain.trim_start_matches('.');
        let device: Vec<JarCookie> = self
            .qr
            .all()
            .into_iter()
            .filter(|c| {
                (c.domain == domain || c.domain.ends_with(&format!(".{domain}"))) && c.name != "z_c0"
            })
            .collect();
        if !device.is_empty() {
            self.qr.put(&session, &self.base_url, device);
        }
        // 1. 登录页（拿 _xsrf / _zap）
        let signin = self.signin_request(&session);
        let signin_response = self.execute(&session, signin)?;
        self.check(&signin_response)?;
        // 2. 没有 d_c0 就领一个（失败不影响后面）。注意 Kotlin 这一跳用的是 `qrRequest("/udid").post(EMPTY_JSON)`，
        //    不是 `ensureDc0` 那套头（两边差别在 `Accept` / `Referer` / 三个 `Sec-Fetch-*`），所以照 QR 那套写。
        if self.find(&session, "d_c0").is_none() {
            let udid = self.udid_qr_request(&session);
            let response = self.execute(&session, udid)?;
            let body = String::from_utf8_lossy(&response.body).into_owned();
            if let Some(e) = zhihu::check_error(response.status as i32, &body, None) {
                if !matches!(e, ZhihuError::NotLoggedIn { .. }) {
                    return Err(e);
                }
            }
        }
        // 3. 要二维码
        let request = self.qr_request(
            &session,
            "/api/v3/account/api/login/qrcode",
            false,
            Some(b"{}".as_slice()),
            seed,
            false,
        );
        let response = self.execute(&session, request)?;
        self.check(&response)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&body).map_err(|_| ZhihuError::Parse {
            detail: "二维码接口的响应不是合法 JSON".to_string(),
        })?;
        let generated = qr_generated(&root, now_ms)?;
        self.sessions.begin(&generated.token, &QrSession(session));
        Ok(QrLogin {
            platform: "zhihu".to_string(),
            content: generated.link,
            token: generated.token,
            extra: Vec::new(),
            expires_at_ms: Some(generated.expires_at_ms),
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    pub fn poll(&self, login: &QrLogin, now_ms: i64, seed: u8) -> Result<QrState, ZhihuError> {
        let session = self.sessions.jar(&login.token);
        let path = format!("/api/v3/account/api/login/qrcode/{}/scan_info", login.token);
        let request = self.qr_request(&session.0, &path, true, None, seed, true);
        let response = match self.execute(&session.0, request) {
            Ok(response) => response,
            Err(e) => {
                self.sessions.end(&login.token);
                return Err(e);
            }
        };
        let checked = match self.check(&response) {
            Ok(()) => response,
            Err(e) => {
                self.sessions.end(&login.token);
                return Err(e);
            }
        };
        let body = String::from_utf8_lossy(&checked.body).into_owned();
        let root = if body.trim().is_empty() {
            JsonMap::new()
        } else {
            match json::parse_object(&body) {
                Ok(root) => root,
                Err(_) => {
                    self.sessions.end(&login.token);
                    return Err(ZhihuError::Parse {
                        detail: "扫码状态接口的响应不是合法 JSON".to_string(),
                    });
                }
            }
        };
        let info = scan_info(&root);
        // 响应体里直接带的 Cookie 补进临时罐（一般情况在 Set-Cookie 里）
        if !info.inline_cookies.is_empty() {
            let expires = now_ms.saturating_add(YEAR_MS);
            self.qr.put(
                &session.0,
                &self.base_url,
                info.inline_cookies
                    .iter()
                    .map(|(name, value)| JarCookie {
                        name: name.clone(),
                        value: value.clone(),
                        domain: self.cookie_domain.trim_start_matches('.').to_string(),
                        path: "/".to_string(),
                        expires_at_ms: expires,
                        secure: false,
                        http_only: false,
                        host_only: false,
                    })
                    .collect(),
            );
        }
        if info.expired() {
            self.sessions.end(&login.token);
            return Ok(QrState::Expired);
        }
        if self.find(&session.0, "z_c0").is_none() && info.logged_in {
            // 成功标志有了但 z_c0 还没到：请求一次 /api/v4/me，让服务端把 Cookie 种下来
            let me = self.api_get(&session.0, "/api/v4/me", seed);
            match me {
                Ok(response) => {
                    let body = String::from_utf8_lossy(&response.body).into_owned();
                    if let Some(ZhihuError::RiskControl { .. }) =
                        zhihu::check_error(response.status as i32, &body, None)
                    {
                        self.sessions.end(&login.token);
                        return Err(zhihu::check_error(response.status as i32, &body, None).unwrap());
                    }
                }
                // 网络抖动：下面发现没有 z_c0 就报失败，用户重新扫
                Err(ZhihuError::Network { .. }) => {}
                Err(e) => {
                    self.sessions.end(&login.token);
                    return Err(e);
                }
            }
        }
        if self.find(&session.0, "z_c0").is_some() {
            let state = self.commit(&session.0);
            self.sessions.end(&login.token);
            return Ok(state);
        }
        if info.logged_in {
            self.sessions.end(&login.token);
            return Ok(QrState::Failed(
                "登录成功了但没拿到 z_c0，现有登录态没有改动".to_string(),
            ));
        }
        Ok(if info.scanned() {
            QrState::Scanned
        } else {
            QrState::Waiting
        })
    }

    /// 落地：验证（临时罐 `/api/v4/me`）→ 删旧 `z_c0` → 写临时罐里的全部 Cookie。
    fn commit(&self, session: &str) -> QrState {
        let user = match self.verify(session) {
            Ok(Some(user)) => Some(user),
            Ok(None) | Err(ZhihuError::NotLoggedIn { .. }) => {
                return QrState::Failed(
                    "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
                )
            }
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功 → 照样写入，`user` 留空
            Err(_) => None,
        };
        let fresh = self.qr.cookies(session);
        self.qr.commit(
            &self.base_url,
            LOGIN_DOMAIN,
            OLD_LOGIN_NAMES.iter().map(|n| n.to_string()).collect(),
            fresh,
        );
        QrState::Success(user)
    }

    /// 用临时罐问一次 `/api/v4/me`（和 `whoami` 同一个请求 / 同一个错误映射，只是换 d_c0 的来源）。
    fn verify(&self, session: &str) -> Result<Option<UserRef>, ZhihuError> {
        let path = "/api/v4/me";
        let response = self.api_get(session, path, 0)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        if response.status == 401 {
            return Ok(None);
        }
        let location = crate::hosted::header_value(&response.headers, "location").map(str::to_string);
        match zhihu::check_http(response.status as i32, &body, location.as_deref()) {
            Ok(json) => Ok(zhihu::user(&json)),
            Err(ZhihuError::NotLoggedIn { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 带签名的 GET（`/api/v4/me`）：头见 [`api_headers`]，`d_c0` 取自**临时罐**。
    fn api_get(&self, session: &str, path: &str, seed: u8) -> Result<HttpResponse, ZhihuError> {
        let url = format!("{}{path}", self.base);
        let dc0 = self.find(session, "d_c0");
        let headers = api_headers(&url, path, dc0.as_deref(), seed);
        self.execute(
            session,
            HttpRequest {
                method: "GET".to_string(),
                url,
                headers,
                body: Vec::new(),
            },
        )
    }

    /// `/signin?next=%2F`：Kotlin `qrRequest` 之后再 `removeHeader(Origin)` / `removeHeader("x-requested-with")`、
    /// 改 `Accept` / `Referer`、补三个 `Sec-Fetch-*`。头**顺序**照 OkHttp `Builder` 的"删了再追加"口径。
    fn signin_request(&self, session: &str) -> HttpRequest {
        let mut headers = self.qr_base_headers(session, false);
        // qrRequest 里有 Origin / x-requested-with，这两条在这里去掉
        headers.retain(|(name, _)| name != "Origin" && name != "x-requested-with");
        // Accept / Referer 改成登录页的（`header()` 会先删同名再追加到末尾）
        let accept = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string();
        let referer = format!("{}/", self.base);
        headers.retain(|(name, _)| name != "Accept" && name != "Referer");
        headers.push(("Accept".to_string(), accept));
        headers.push(("Referer".to_string(), referer));
        for (name, value) in [
            ("Sec-Fetch-Dest", "document"),
            ("Sec-Fetch-Mode", "navigate"),
            ("Sec-Fetch-Site", "none"),
        ] {
            headers.retain(|(n, _)| n != name);
            headers.push((name.to_string(), value.to_string()));
        }
        // 拦截器只补**缺失**的同名头（Referer 有了、没有 User-Agent / Accept-Language / x-api-version / x-app-za）
        headers.extend(platform_headers_missing(&headers, false));
        HttpRequest {
            method: "GET".to_string(),
            url: format!("{}/signin?next=%2F", self.base),
            headers,
            body: Vec::new(),
        }
    }

    /// `/udid`：Kotlin `startQrLogin` 里那一跳 `qrRequest("/udid", jar).post(EMPTY_JSON)`——
    /// 用扫码那套头（**不签名**，因为这时候罐里还没有 `d_c0`），body 是 `{}`。
    fn udid_qr_request(&self, session: &str) -> HttpRequest {
        let mut headers = self.qr_base_headers(session, false);
        headers.extend(platform_headers_missing(&headers, false));
        headers.push((
            "Content-Type".to_string(),
            "application/json;charset=UTF-8".to_string(),
        ));
        HttpRequest {
            method: "POST".to_string(),
            url: format!("{}/udid", self.base),
            headers,
            body: b"{}".to_vec(),
        }
    }

    /// 扫码登录的请求（Kotlin `qrRequest`）：`Aceept` / `Origin` / `Referer` / `x-requested-with` /
    /// 三个 client hints / 三个 `Sec-Fetch-*`，最后是临时罐里的 `_xsrf` → `x-xsrftoken`。
    fn qr_request(
        &self,
        session: &str,
        path: &str,
        poll: bool,
        body: Option<&[u8]>,
        seed: u8,
        sign: bool,
    ) -> HttpRequest {
        let mut headers = self.qr_base_headers(session, poll);
        // 签名（`x-zse-96`）用临时罐里的 d_c0；没有就不签（服务端决定收不收）。
        // **生成二维码那一跳不签**（Kotlin `startQrLogin` 直接 fetch，没走 `sign()`）——只有轮询和验证签。
        let dc0 = self.find(session, "d_c0").filter(|_| sign);
        if let Some(dc0) = dc0.as_deref() {
            // Kotlin 的 `sign()` 用 `header()`（先删同名再追加），所以 x-requested-with 会挪到签名三件套后面
            headers.retain(|(name, _)| name != "x-requested-with");
            headers.push(("x-zse-93".to_string(), sign::ZSE93.to_string()));
            headers.push((
                "x-zse-96".to_string(),
                sign::zse96(path, dc0, None, seed),
            ));
            headers.push(("x-requested-with".to_string(), "fetch".to_string()));
        }
        headers.extend(platform_headers_missing(&headers, false));
        let url = format!("{}{path}", self.base);
        if let Some(body) = body {
            // 宿主接口只有 body 字节：Content-Type 自己写，位置和 OkHttp `BridgeInterceptor` 补的一样
            headers.push((
                "Content-Type".to_string(),
                "application/json;charset=UTF-8".to_string(),
            ));
            HttpRequest {
                method: "POST".to_string(),
                url,
                headers,
                body: body.to_vec(),
            }
        } else {
            HttpRequest {
                method: "GET".to_string(),
                url,
                headers,
                body: Vec::new(),
            }
        }
    }

    /// `qrRequest` 那批固定头（不含签名三件套）。
    fn qr_base_headers(&self, session: &str, poll: bool) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "Accept".to_string(),
                if poll {
                    "*/*".to_string()
                } else {
                    "application/json, text/plain, */*".to_string()
                },
            ),
            ("Origin".to_string(), self.base.clone()),
            (
                "Referer".to_string(),
                if poll {
                    format!("{}/signin?next=%2F", self.base)
                } else {
                    format!("{}/signin", self.base)
                },
            ),
            ("x-requested-with".to_string(), "fetch".to_string()),
            ("sec-ch-ua".to_string(), SEC_CH_UA.to_string()),
            ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
            ("sec-ch-ua-platform".to_string(), "\"Windows\"".to_string()),
            ("Sec-Fetch-Dest".to_string(), "empty".to_string()),
            ("Sec-Fetch-Mode".to_string(), "cors".to_string()),
            ("Sec-Fetch-Site".to_string(), "same-origin".to_string()),
        ];
        if let Some(xsrf) = self.find(session, "_xsrf") {
            headers.push(("x-xsrftoken".to_string(), xsrf));
        }
        headers
    }

    fn find(&self, session: &str, name: &str) -> Option<String> {
        self.qr.find(session, &self.base_url, name)
    }

    fn execute(&self, session: &str, request: HttpRequest) -> Result<HttpResponse, ZhihuError> {
        self.qr
            .execute(session, request, None)
            .map_err(ZhihuError::from)
    }

    /// Kotlin `qrChecked`：风控照抛；"需要登录"当普通接口错误（这条流程本来就是匿名的）。
    fn check(&self, response: &HttpResponse) -> Result<(), ZhihuError> {
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let location = crate::hosted::header_value(&response.headers, "location").map(str::to_string);
        match zhihu::check_error(response.status as i32, &body, location.as_deref()) {
            None => Ok(()),
            Some(ZhihuError::NotLoggedIn { .. }) => Err(ZhihuError::Api {
                code: format!("HTTP {}", response.status),
                detail: "扫码登录接口回了需要登录".to_string(),
            }),
            Some(e) => Err(e),
        }
    }
}

/// 平台默认头里**还没有**的那几个（OkHttp 的 `DefaultHeadersInterceptor` 只补缺失的同名头）。
fn platform_headers_missing(existing: &[(String, String)], include_accept: bool) -> Vec<(String, String)> {
    platform_headers(include_accept)
        .into_iter()
        .filter(|(name, _)| !existing.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
        .collect()
}

/// 网页的 `sec-ch-ua`（Kotlin `SEC_CH_UA`，按我们用的 Chrome 版本）。
const SEC_CH_UA: &str = "\"Google Chrome\";v=\"141\", \"Not?A_Brand\";v=\"8\", \"Chromium\";v=\"141\"";

// 让 `WWW` / `USER_AGENT` 在文档里可点（这一层不直接用，但两边必须指同一个地址 / UA）。
const _: () = {
    let _ = WWW;
    let _ = USER_AGENT;
};

// ===========================================================================
// 纯解析（Kotlin `ZhihuQrParser`）
// ===========================================================================

/// `login/qrcode` 的响应：二维码内容（`link`）、轮询 token、过期时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrGenerated {
    pub link: String,
    pub token: String,
    pub expires_at_ms: i64,
}

pub fn qr_generated(root: &JsonMap, now_ms: i64) -> Result<QrGenerated, ZhihuError> {
    let token = json::text(root, "token")
        .or_else(|| json::text(root, "qrcode_token"))
        .ok_or_else(|| ZhihuError::Parse {
            detail: "二维码响应缺少 token".to_string(),
        })?;
    let link = json::text(root, "link").ok_or_else(|| ZhihuError::Parse {
        detail: "二维码响应缺少 link".to_string(),
    })?;
    Ok(QrGenerated {
        link,
        token,
        expires_at_ms: expiry_ms(json::long(root, "expires_at"), now_ms),
    })
}

/// `expires_at` 实测是 Unix 秒，但 Zhihu++ 注释说有时是 TTL，所以一并兼容（Kotlin `expiryMs`）：
/// 没有 / 非正数 → 默认 2 分钟；小于当前 Unix 秒数 → 当 TTL（≤ 86400 按秒，≤ 86400000 按毫秒）；
/// < 1e10 → Unix 秒；否则 Unix 毫秒。
pub fn expiry_ms(raw: Option<i64>, now_ms: i64) -> i64 {
    let Some(raw) = raw.filter(|v| *v > 0) else {
        return now_ms.saturating_add(DEFAULT_TTL_MS);
    };
    let now_sec = now_ms / 1000;
    if raw < now_sec {
        if raw <= 86_400 {
            now_ms.saturating_add(raw.saturating_mul(1000))
        } else if raw <= 86_400_000 {
            now_ms.saturating_add(raw)
        } else {
            now_ms.saturating_add(DEFAULT_TTL_MS)
        }
    } else if raw < 10_000_000_000 {
        raw.saturating_mul(1000)
    } else {
        raw
    }
}

/// `scan_info` 里和状态有关的字段（Kotlin `ZhihuQrParser.ScanInfo`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanInfo {
    pub status: Option<i32>,
    pub login_status: String,
    pub logged_in: bool,
    pub inline_cookies: Vec<(String, String)>,
}

impl ScanInfo {
    pub fn scanned(&self) -> bool {
        self.status == Some(1)
    }

    pub fn expired(&self) -> bool {
        self.status == Some(2) || EXPIRED.contains(&self.login_status.as_str())
    }
}

const EXPIRED: [&str; 3] = ["EXPIRED", "QR_CODE_EXPIRED", "LOGIN_EXPIRED"];
const SUCCESS: [&str; 5] = ["CONFIRMED", "LOGIN_SUCCESS", "SUCCESS", "OK", "LOGGED_IN"];

pub fn scan_info(root: &JsonMap) -> ScanInfo {
    let login_status = json::text(root, "login_status")
        .unwrap_or_default()
        .to_uppercase();
    let logged_in = json::text(root, "user_id").is_some()
        || json::text(root, "access_token").is_some()
        || json::bool(root, "success") == Some(true)
        || json::bool(root, "logged_in") == Some(true)
        || SUCCESS.contains(&login_status.as_str());
    let mut cookies: Vec<(String, String)> = Vec::new();
    let raw = [json::text(root, "cookie"), json::text(root, "cookies")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(";");
    for (name, value) in parse_cookie_assignments(&raw) {
        push_cookie(&mut cookies, name, value);
    }
    if let Some(zc0) = json::text(root, "z_c0") {
        push_cookie(&mut cookies, "z_c0".to_string(), zc0);
    }
    ScanInfo {
        status: json::int(root, "status"),
        login_status,
        logged_in,
        inline_cookies: cookies,
    }
}

/// 同名后者覆盖前者（Kotlin 用 `mutableMap` + `put`，`z_c0` 字段最后写）。
fn push_cookie(into: &mut Vec<(String, String)>, name: String, value: String) {
    if let Some(entry) = into.iter_mut().find(|(n, _)| *n == name) {
        entry.1 = value;
    } else {
        into.push((name, value));
    }
}

/// `a=1; b=2` → 名字 → 值，跳过 Cookie 属性（Domain / Path ……）和空值（Kotlin `parseCookieAssignments`）。
pub fn parse_cookie_assignments(raw: &str) -> Vec<(String, String)> {
    let attributes = [
        "domain", "path", "expires", "max-age", "httponly", "secure", "samesite",
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for item in raw.split(';') {
        let Some(i) = item.find('=') else { continue };
        if i == 0 {
            continue;
        }
        let name = item[..i].trim();
        let value = item[i + 1..].trim();
        if name.is_empty() || value.is_empty() || attributes.contains(&name.to_lowercase().as_str()) {
            continue;
        }
        push_cookie(&mut out, name.to_string(), value.to_string());
    }
    out
}

