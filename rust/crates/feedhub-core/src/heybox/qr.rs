//! 小黑盒的**网页扫码登录**：请求 / 签名 / 状态 / Cookie 落地全在这里。
//!
//! 逐条对照 `source-heybox/HeyboxSource.kt` 的 `startQrLogin` / `pollQrLogin` / `finishQrLogin` 和
//! `HeyboxQrParser.kt`（接口和状态值来自 xiaoheihe-cli `auth.py` 的 `login_qr`）：
//!
//! 1. `GET /account/get_qrcode_url/?app=web`（带网页公共参数 + `hkey` 签名，**空临时罐**）→ `result.qr_url`；
//! 2. 轮询 `GET /account/qr_state/?app=web&qr=<token>`：`result.error` 是 `wait` / `ready` / `ok` / 其他；
//! 3. 成功时账号 id 取 `result.profile.heybox_id` / `account_detail.userid` / `heybox_id`，`pkey` 取 `result.pkey`，
//!    没有就取临时罐里的 `user_pkey` / `pkey`；然后补齐 `heybox_id` / `user_heybox_id` / `pkey` / `user_pkey`
//!    四条 Cookie（域 `xiaoheihe.cn`，有效期取罐里同名账号 Cookie 的，否则一年）；
//! 4. 用临时罐调一次 `restore_login` 确认，通过后清掉旧的账号 Cookie（[`OLD_LOGIN_NAMES`]，
//!    `x_xhh_tokenid` / `smidV2` 这类设备标识保留）再写入；
//! 5. 全程不碰 `ctx.cookies`（除了第一步读设备 Cookie 之外一格都不动——和 Kotlin 一样，小黑盒**不**复制旧 Cookie
//!    进临时罐，起点是空罐）。
//!
//! 时间 / 随机量都由调用方注入（`now_ms` / `now_sec` / `nonce`），Rust 不读时钟、不取随机数。

use std::sync::Arc;

use crate::account::{QrLogin, QrSessions, QrSession, QrState};
use crate::host::{HttpRequest, JarCookie, QrHost};
use crate::heybox::source::{default_headers, signed_url_for, COOKIE_URL};
use crate::json::{self, JsonMap};
use crate::heybox::HeyboxError;
use crate::model::UserRef;

/// 账号 Cookie 的域（`LoginSpec.cookieDomain`，带前导点，`remove` 自己裁）。
pub const LOGIN_DOMAIN: &str = ".xiaoheihe.cn";

/// 二维码有效期（平台没给 `expire` 时）：实测 120 秒（Kotlin `QR_TTL_S`）。
const QR_TTL_S: i64 = 120;

/// 轮询间隔（Kotlin `QR_POLL_INTERVAL_MS`）。
const QR_POLL_INTERVAL_MS: i64 = 2_000;

/// 一年（账号 Cookie 没给过期时间时用，Kotlin `YEAR_MS`）。
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;

/// 给用户的提示（和 Kotlin `startQrLogin` 的 `hint` 逐字一样）。
pub const HINT: &str = "用小黑盒 App 扫码，并在手机上确认登录";

/// 账号 Cookie 的四个名字（`heybox_id` / `user_heybox_id` / `pkey` / `user_pkey`）。
const ALIAS_NAMES: [&str; 4] = ["heybox_id", "user_heybox_id", "pkey", "user_pkey"];

/// 换账号时要先清掉的旧账号 Cookie：登录凭据 + 网页登录后写的账号资料（Kotlin `OLD_LOGIN_NAMES`）。
pub const OLD_LOGIN_NAMES: [&str; 9] = [
    "heybox_id",
    "user_heybox_id",
    "pkey",
    "user_pkey",
    "nickname",
    "avatar",
    "level",
    "permission",
    "wiki_power",
];

/// 小黑盒扫码登录的平台对象。
pub struct HeyboxQrSource {
    base: String,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
    /// 一次调用里每个请求各用一个 nonce。
    nonces: super::sign::NonceLedger,
}

impl HeyboxQrSource {
    pub fn new(base: impl Into<String>, qr: Arc<dyn QrHost>) -> Self {
        Self {
            base: base.into(),
            qr: qr.clone(),
            sessions: QrSessions::new(qr),
            nonces: super::sign::NonceLedger::default(),
        }
    }

    /// 接上宿主的 nonce 来源（见 [`super::sign::NonceLedger`]）。
    pub fn set_nonce_source(&self, source: Arc<dyn super::sign::NonceSource>) {
        self.nonces.set_host(source);
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn start(&self, now_ms: i64, now_sec: i64, nonce: &str) -> Result<QrLogin, HeyboxError> {
        let session = self.qr.begin();
        let root = self.get_json(
            &session,
            "/account/get_qrcode_url/",
            &[("app".to_string(), "web".to_string())],
            now_sec,
            nonce,
        )?;
        let generated = qr_generated(&root)?;
        self.sessions.begin(&generated.token, &QrSession(session));
        Ok(QrLogin {
            platform: "heybox".to_string(),
            content: generated.url,
            token: generated.token,
            extra: Vec::new(),
            expires_at_ms: Some(
                now_ms.saturating_add(generated.expire_sec.unwrap_or(QR_TTL_S).saturating_mul(1000)),
            ),
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    pub fn poll(
        &self,
        login: &QrLogin,
        now_ms: i64,
        now_sec: i64,
        nonce: &str,
    ) -> Result<QrState, HeyboxError> {
        let session = self.sessions.jar(&login.token);
        let root = self.get_json(
            &session.0,
            "/account/qr_state/",
            &[
                ("app".to_string(), "web".to_string()),
                ("qr".to_string(), login.token.clone()),
            ],
            now_sec,
            nonce,
        )?;
        if let Some(state) = qr_state(&root)? {
            if state == QrState::Expired {
                self.sessions.end(&login.token);
            }
            return Ok(state);
        }
        // `result.error == "ok"`：用 result 里的账号 / pkey 补齐 Cookie，再验证 + 落地
        let result = self
            .finish(&root, &session.0, now_ms, now_sec, nonce)
            .map_err(|e| {
            self.sessions.end(&login.token);
            e
        })?;
        self.sessions.end(&login.token);
        Ok(result)
    }

    fn finish(
        &self,
        root: &JsonMap,
        session: &str,
        now_ms: i64,
        now_sec: i64,
        nonce: &str,
    ) -> Result<QrState, HeyboxError> {
        let seen = self.qr.cookies(session);
        let seen_value = |names: &[&str]| -> Option<String> {
            seen.iter()
                .find(|c| names.contains(&c.name.as_str()) && !c.value.trim().is_empty())
                .map(|c| c.value.clone())
        };
        let user_id = super::account(root)
            .map(|u| u.id)
            .or_else(|| seen_value(&["user_heybox_id", "heybox_id"]));
        let pkey = json::obj(root, "result")
            .and_then(|r| json::text(r, "pkey"))
            .or_else(|| seen_value(&["user_pkey", "pkey"]));
        let missing = if user_id.is_none() { "账号 id" } else { "pkey" };
        let (Some(user_id), Some(pkey)) = (user_id, pkey) else {
            return Ok(QrState::Failed(format!(
                "登录成功了但没拿到{missing}，现有登录态没有改动"
            )));
        };
        // 过期时间：罐里同名的账号 Cookie 还没过期就用它的，否则一年
        let expires_at = seen
            .iter()
            .find(|c| ALIAS_NAMES.contains(&c.name.as_str()))
            .map(|c| c.expires_at_ms)
            .filter(|ms| *ms > now_ms)
            .unwrap_or_else(|| now_ms.saturating_add(YEAR_MS));
        self.qr.put(
            session,
            COOKIE_URL,
            [("heybox_id", &user_id), ("user_heybox_id", &user_id), ("pkey", &pkey), ("user_pkey", &pkey)]
                .iter()
                .map(|(name, value)| JarCookie {
                    name: (*name).to_string(),
                    value: (*value).to_string(),
                    domain: "xiaoheihe.cn".to_string(),
                    path: "/".to_string(),
                    expires_at_ms: expires_at,
                    secure: false,
                    http_only: false,
                    host_only: false,
                })
                .collect(),
        );
        Ok(self.commit(session, now_sec, nonce))
    }

    /// 验证（临时罐 `restore_login`）+ 落地（先删旧账号 Cookie，再写临时罐里的全部）。
    fn commit(&self, session: &str, now_sec: i64, nonce: &str) -> QrState {
        let user = match self.verify(session, now_sec, nonce) {
            Ok(Some(user)) => Some(user),
            Ok(None) | Err(HeyboxError::NotLoggedIn { .. }) => {
                return QrState::Failed(
                    "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
                )
            }
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功 → 照样写入，`user` 留空
            Err(_) => None,
        };
        let fresh = self.qr.cookies(session);
        self.qr.commit(
            COOKIE_URL,
            LOGIN_DOMAIN,
            OLD_LOGIN_NAMES.iter().map(|n| n.to_string()).collect(),
            fresh,
        );
        QrState::Success(user)
    }

    /// 用临时罐问一次 `restore_login`（和 [`super::source::HeyboxHostedSource::whoami`] 同一个请求 / 解析）。
    fn verify(&self, session: &str, now_sec: i64, nonce: &str) -> Result<Option<UserRef>, HeyboxError> {
        let request = self.request("/account/restore_login", &[], now_sec, nonce);
        let response = self
            .qr
            .execute(session, request, None)
            .map_err(HeyboxError::from)?;
        let body = match response.status {
            200..=299 => String::from_utf8_lossy(&response.body).into_owned(),
            401 => return Ok(None),
            code => {
                return Err(HeyboxError::Api {
                    code: format!("HTTP {code}"),
                    detail: json::take_chars(&String::from_utf8_lossy(&response.body), 200),
                })
            }
        };
        let root = json::parse_object(&body).map_err(HeyboxError::parse)?;
        if let Some(status) = json::string(&root, "status") {
            if super::source::NOT_LOGGED_IN.contains(&status.as_str()) {
                return Ok(None);
            }
        }
        super::source::check_status(&root)?;
        Ok(super::account(&root))
    }

    /// 扫码接口的一次 GET：带网页公共参数 + `hkey` 签名（[`signed_url_for`]，和推荐流同一份口径），
    /// 用临时罐发。HTTP 判定照 Kotlin `getBoth`（401 → 未登录、403 / 429 → 风控、其他 → 接口错误）。
    fn get_json(
        &self,
        session: &str,
        path: &str,
        params: &[(String, String)],
        now_sec: i64,
        nonce: &str,
    ) -> Result<JsonMap, HeyboxError> {
        let response = self
            .qr
            .execute(session, self.request(path, params, now_sec, nonce), None)
            .map_err(HeyboxError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        match response.status {
            200..=299 => {}
            401 => return Err(HeyboxError::NotLoggedIn { detail: None }),
            403 => {
                return Err(HeyboxError::RiskControl {
                    code: Some("HTTP 403".to_string()),
                    detail: "请求被拒绝".to_string(),
                })
            }
            429 => {
                return Err(HeyboxError::RiskControl {
                    code: Some("HTTP 429".to_string()),
                    detail: "请求过于频繁".to_string(),
                })
            }
            code => {
                return Err(HeyboxError::Api {
                    code: format!("HTTP {code}"),
                    detail: json::take_chars(&body, 200),
                })
            }
        }
        let root = json::parse_object(&body).map_err(HeyboxError::parse)?;
        // 扫码时临时罐就是"没登录"的正常状态，`show_captcha` 是真的验证码 → 由 `check_status` 翻成风控
        super::source::check_status(&root)?;
        Ok(root)
    }

    fn request(
        &self,
        path: &str,
        params: &[(String, String)],
        now_sec: i64,
        nonce: &str,
    ) -> HttpRequest {
        HttpRequest {
            method: "GET".to_string(),
            url: signed_url_for(&self.base, path, params, now_sec, &self.nonces.next(nonce, now_sec)),
            headers: default_headers(),
            body: Vec::new(),
        }
    }
}

// ===========================================================================
// 纯解析（Kotlin `HeyboxQrParser`）
// ===========================================================================

/// `get_qrcode_url` 的 `result`：二维码内容、轮询 token（url 里的 `qr` 参数）、有效秒数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrGenerated {
    pub url: String,
    pub token: String,
    pub expire_sec: Option<i64>,
}

pub fn qr_generated(root: &JsonMap) -> Result<QrGenerated, HeyboxError> {
    let result = json::obj(root, "result").ok_or_else(|| HeyboxError::Parse {
        detail: "二维码响应缺少 result".to_string(),
    })?;
    let url = json::text(result, "qr_url").ok_or_else(|| HeyboxError::Parse {
        detail: "二维码响应缺少 qr_url".to_string(),
    })?;
    let token = query_parameter(&url, "qr")
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| HeyboxError::Parse {
            detail: "二维码地址里没有 qr 参数".to_string(),
        })?;
    Ok(QrGenerated {
        url,
        token,
        expire_sec: json::long(result, "expire"),
    })
}

/// `qr_state` 的 `result.error` → 状态；成功（`ok`）返回 `None`，由调用方继续处理。
pub fn qr_state(root: &JsonMap) -> Result<Option<QrState>, HeyboxError> {
    let error = json::obj(root, "result")
        .and_then(|r| json::text(r, "error"))
        .ok_or_else(|| HeyboxError::Parse {
            detail: "状态响应缺少 result.error".to_string(),
        })?;
    Ok(match error.as_str() {
        "wait" => Some(QrState::Waiting),
        // xiaoheihe-cli 里 `ready` 和 wait 一样继续等；从名字推断是"已扫码、等确认"，按 Scanned 处理
        "ready" => Some(QrState::Scanned),
        "ok" => None,
        _ => Some(QrState::Expired),
    })
}

/// URL 里某个 query 参数的原值（Kotlin `toHttpUrlOrNull()?.queryParameter(name)`）。
fn query_parameter(url: &str, name: &str) -> Option<String> {
    let scheme = url.find("://")?;
    let rest = &url[scheme + 3..];
    let host_len = rest.find(['/', '?'])?;
    if host_len == 0 {
        return None;
    }
    let query = &url[url.find('?')? + 1..];
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

