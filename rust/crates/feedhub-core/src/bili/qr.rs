//! B站 的**网页扫码登录**：请求 / 签名 / 轮询状态码 / Cookie 落地全在这里。
//!
//! 逐条对照 `source-bilibili/BilibiliSource.kt` 的 `startQrLogin` / `pollQrLogin` / `qrGet` 和
//! `BiliQrParser.kt`、`core/QrLoginSupport.kt` 的 `commitQrLogin`：
//!
//! 1. `GET passport.bilibili.com/x/passport-login/web/qrcode/generate`（无参数、不要 Cookie）→ `{url, qrcode_key}`；
//! 2. 轮询 `GET …/qrcode/poll?qrcode_key=…`（普通短请求）：`data.code` 86101 / 86090 / 86038 / 0；
//! 3. 成功时登录 Cookie 由 Set-Cookie 种进**临时罐**（宿主的 [`QrHost`]），罐里没有 `SESSDATA` 时用
//!    `data.url` 里的参数补上（[`super::account::cookies_from_url`]）；
//! 4. 用**临时罐**问一次 `nav` 确认真的登录了，再清掉旧的登录 Cookie（`SESSDATA` / `bili_jct` /
//!    `DedeUserID` / `DedeUserID__ckMd5` / `sid`，`buvid3` 这些设备标识不动）、把临时罐里的全部 Cookie 写进
//!    `ctx.cookies`（`QrHost::commit`）；
//! 5. 全程**不碰** `ctx.cookies` 的读之外的东西：成功之前一格都不动。
//!
//! 身份 URL / 过期时间 / 提示语照 Kotlin：`WWW` = `https://www.bilibili.com/`，二维码 180 秒，
//! 轮询间隔用 `QrLogin` 的默认 1500 ms。

use std::sync::Arc;

use crate::account::{QrLogin, QrSessions, QrState};
use crate::bili::account;
use crate::bili::source::{USER_AGENT, ORIGIN, REFERER};
use crate::host::{HttpRequest, HttpResponse, QrHost};
use crate::json;
use crate::model::{BiliError, UserRef};

/// 线上 passport 接口的根（和 Kotlin `PASSPORT` 一样）。测试里换成 MockWebServer 的地址。
pub const PASSPORT: &str = "https://passport.bilibili.com";

/// Cookie 罐里 B站 的"身份 URL"（和 Kotlin 的 `WWW` 一样：按 www.bilibili.com 取 Cookie）。
const WWW_URL: &str = "https://www.bilibili.com/";

/// 登录态 Cookie 的域（`LoginSpec.cookieDomain`，**带前导点**，`remove` 会自己裁掉）。
pub const LOGIN_DOMAIN: &str = ".bilibili.com";

/// 二维码有效期 180 秒（Kotlin `QR_TTL_MS`）。
const QR_TTL_MS: i64 = 180_000;

/// 轮询间隔（`QrLogin` 的默认值，B站 没有覆盖）。
const QR_POLL_INTERVAL_MS: i64 = 1_500;

/// 给用户的提示（和 Kotlin `startQrLogin` 的 `hint` 逐字一样）。
pub const HINT: &str = "用哔哩哔哩 App 扫码，并在手机上确认登录";

/// B站 扫码登录的平台对象。
///
/// 状态只有两样：临时罐的 `token → 句柄` 表（[`QrSessions`]，LRU 8，语义照 Kotlin `QrSessions`）。
/// 没有后台线程、没有定时器。
pub struct BiliQrSource {
    base: String,
    passport: String,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
}

impl BiliQrSource {
    pub fn new(
        base: impl Into<String>,
        passport: impl Into<String>,
        qr: Arc<dyn QrHost>,
    ) -> Self {
        Self {
            base: base.into(),
            passport: passport.into(),
            qr: qr.clone(),
            sessions: QrSessions::new(qr),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 第一步：要一张二维码。返回的 `token` 就是轮询用的 `qrcode_key`。
    pub fn start(&self, now_ms: i64) -> Result<QrLogin, BiliError> {
        let session = self.qr.begin();
        let root = self.get_json(&session, "/x/passport-login/web/qrcode/generate", &[])?;
        let generated = account::qr_generated(&root)?;
        self.sessions.begin(&generated.key, &crate::account::QrSession(session));
        Ok(QrLogin {
            platform: "bili".to_string(),
            content: generated.url,
            token: generated.key,
            extra: Vec::new(),
            expires_at_ms: Some(now_ms.saturating_add(QR_TTL_MS)),
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    /// 第二步：问一次状态。终态（成功 / 过期 / 失败）都会把临时罐丢掉。
    pub fn poll(&self, login: &QrLogin, now_ms: i64) -> Result<QrState, BiliError> {
        let session = self.sessions.jar(&login.token);
        let root = self.get_json(
            &session.0,
            "/x/passport-login/web/qrcode/poll",
            &[("qrcode_key".to_string(), login.token.clone())],
        )?;
        let poll = account::qr_poll(&root)?;
        if let Some(state) = account::qr_waiting(poll.code) {
            if state == QrState::Expired {
                self.sessions.end(&login.token);
            }
            return Ok(state);
        }
        if poll.code != 0 {
            self.sessions.end(&login.token);
            return Ok(QrState::Failed(format!(
                "扫码登录失败（{} {}）",
                poll.code, poll.message
            )));
        }
        // 登录 Cookie 正常由 Set-Cookie 种进临时罐；罐里没有 SESSDATA 时用 data.url 里的参数补上。
        if !self.has_sessdata(&session.0) {
            let cookies = account::cookies_from_url(poll.url.as_deref(), now_ms);
            if !cookies.is_empty() {
                self.qr.put(&session.0, WWW_URL, cookies);
            }
        }
        if !self.has_sessdata(&session.0) {
            self.sessions.end(&login.token);
            return Ok(QrState::Failed(
                "登录成功了但响应里没有 SESSDATA，现有登录态没有改动".to_string(),
            ));
        }
        // 用临时罐问一次 nav 确认真的登录了。验证用的是**临时罐**，不是 ctx.cookies。
        let state = match self.verify(&session.0) {
            Ok(Some(user)) => self.commit(&session.0, Some(user)),
            Ok(None) | Err(BiliError::NotLoggedIn { .. }) => QrState::Failed(
                "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
            ),
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功（这个二维码也用掉了），照样写入，
            // `user` 留空让前端再问一次（Kotlin `commitQrLogin` 的同一个判断）。
            Err(_) => self.commit(&session.0, None),
        };
        self.sessions.end(&login.token);
        Ok(state)
    }

    /// 落地：清掉旧的登录 Cookie，再把临时罐里的全部 Cookie 原样写进 `ctx.cookies`。
    fn commit(&self, session: &str, user: Option<UserRef>) -> QrState {
        let fresh = self.qr.cookies(session);
        self.qr.commit(
            WWW_URL,
            LOGIN_DOMAIN,
            account::OLD_LOGIN_NAMES.iter().map(|n| n.to_string()).collect(),
            fresh,
        );
        QrState::Success(user)
    }

    fn has_sessdata(&self, session: &str) -> bool {
        self.qr
            .cookies(session)
            .iter()
            .any(|c| c.name == "SESSDATA" && !c.value.is_empty())
    }

    /// 用临时罐问一次 `nav`（和 [`super::source::BiliHostedSource::whoami`] 同一个请求 / 同一个解析）。
    fn verify(&self, session: &str) -> Result<Option<UserRef>, BiliError> {
        let response = self
            .qr
            .execute(session, self.request("/x/web-interface/nav", &[]), None)
            .map_err(BiliError::from)?;
        let body = self.check(&response, "/x/web-interface/nav")?;
        super::source::BiliHostedSource::parse_whoami(&body)
    }

    /// `qrGet`：用临时罐发 GET，检查 HTTP 状态和 `{code}` 外壳。出错信息里**不带**响应内容
    /// （成功的响应里有凭据）。
    fn get_json(
        &self,
        session: &str,
        path: &str,
        params: &[(String, String)],
    ) -> Result<crate::json::JsonMap, BiliError> {
        let response = self
            .qr
            .execute(session, self.request(path, params), None)
            .map_err(BiliError::from)?;
        let body = self.check(&response, path)?;
        // 和 Kotlin `parseQrJson` 一样：不是 JSON 对象就说 Parse，**不回显内容**（里面有凭据）。
        let root = json::parse_object(&body).map_err(|_| BiliError::Parse {
            detail: "二维码接口的响应不是合法 JSON".to_string(),
        })?;
        crate::bili::check_envelope_object(&root)?;
        Ok(root)
    }

    /// Kotlin `qrGet` 的状态判定：412 → 风控；非 2xx → `Api`（detail 是 `"扫码登录接口 <path>"`）。
    fn check(&self, response: &HttpResponse, path: &str) -> Result<String, BiliError> {
        if matches!(response.status, 403 | 412 | 429) {
            return Err(BiliError::RiskControl {
                code: Some(format!("HTTP {}", response.status)),
                detail: "请求被拦截".to_string(),
            });
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: format!("扫码登录接口 {path}"),
            });
        }
        Ok(String::from_utf8_lossy(&response.body).into_owned())
    }

    /// passport 的 GET：默认头（UA / Referer / Origin，顺序照 `DefaultHeadersInterceptor`），不带 body。
    fn request(&self, path: &str, params: &[(String, String)]) -> HttpRequest {
        let mut url = format!("{}{}", self.passport, path);
        if !params.is_empty() {
            url.push('?');
            let query = params
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        crate::hosted::query_encode(k),
                        crate::hosted::query_encode(v)
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            url.push_str(&query);
        }
        HttpRequest {
            method: "GET".to_string(),
            url,
            headers: vec![
                ("User-Agent".to_string(), USER_AGENT.to_string()),
                ("Referer".to_string(), REFERER.to_string()),
                ("Origin".to_string(), ORIGIN.to_string()),
            ],
            body: Vec::new(),
        }
    }
}

