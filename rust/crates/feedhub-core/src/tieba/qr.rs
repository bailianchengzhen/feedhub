//! 贴吧（百度通行证）的**网页扫码登录**。
//!
//! 逐条对照 `source-tieba/TiebaSource.kt` 的 `startQrLogin` / `pollQrLogin` / `finishQrLogin` /
//! `qrClient` / `qrChecked` / `absorbLoginCookies` / `loginCookies` 和 `TiebaQrParser.kt`：
//!
//! 1. `GET {passport}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid=…&apiver=v3&tt=…&tpl=tb&_=…` → `{errno:0, sign, imgurl}`；
//! 2. `GET imgurl` 取二维码 PNG；**内容是把这张图解出来的文字**（`t` / `local` 由服务端生成，拼不出来）——
//!    zxing 只有 Java 版，所以图解码走宿主（[`QrHost::decode_qr_image`]）；
//! 3. 轮询 `GET {passport}/channel/unicast?channel_id=<sign>&tpl=tb&gid=…&callback=…`，**长轮询**
//!    （服务端挂约 30 秒），读超时是 [`TiebaQrSource::unicast_timeout_ms`]（Kotlin 10 秒），超时 / `errno 1` 都算 Waiting；
//!    通行证**不会通知二维码过期**，所以 Waiting 时按时间判（`expiresAt + 10 秒`）；
//! 4. 已确认：`GET /v3/login/main/qrbdusslogin?bduss=<临时 bduss>&…` 换正式 BDUSS（看 Set-Cookie）；
//!    没有 STOKEN 时带 BDUSS 走 `GET /v3/login/api/auth/`，**逐跳跟重定向**（最多 6 跳，只在百度域名内）找 STOKEN；
//! 5. 用临时罐调 `whoami` 确认，通过后才清掉旧登录 Cookie（`BDUSS` / `BDUSS_BFESS` / `STOKEN` / `PTOKEN` /
//!    `TIEBAUID` / `UBI`，`BAIDUID` 等设备标识保留）再写入（登录 Cookie 一律以域 `baidu.com` 写）。
//!
//! **临时 bduss / BDUSS / STOKEN / PTOKEN 的值绝不进日志和异常信息**（异常里只有 `errno` / 平台错误号）。
//!
//! Set-Cookie 是**自己解析原始响应头**（不经宿主的罐）：STOKEN 可能由 `passport.baidu.com` 带着
//! `Domain=tieba.baidu.com` 下发，OkHttp 会因为域名对不上把它丢掉（Kotlin 那边也是这么处理的）。

use std::sync::Arc;

use crate::account::{QrLogin, QrSession, QrSessions, QrState};
use crate::host::{HttpRequest, JarCookie, QrHost};
use crate::json::{self, JsonMap};
use crate::model::{BiliError, UserRef};
use crate::tieba::source::{classify_error, is_not_logged_in, self_user, web_get_headers};

/// 登录 Cookie 的域（`LoginSpec.cookieDomain`，带前导点）。
pub const LOGIN_DOMAIN: &str = ".baidu.com";

/// 取 Cookie 用的"贴吧首页"地址（`TiebaSource.COOKIE_URL`）。
pub const COOKIE_URL: &str = "https://tieba.baidu.com/";

/// 登录凭据：BDUSS（+ 它的 `_BFESS` 副本）、STOKEN、PTOKEN。
pub const LOGIN_COOKIES: [&str; 4] = ["BDUSS", "BDUSS_BFESS", "STOKEN", "PTOKEN"];

/// 换账号时先清掉的旧登录 Cookie：上面几个加贴吧的 `TIEBAUID` / `UBI`；`BAIDUID` 等设备标识保留。
pub const OLD_LOGIN_NAMES: [&str; 6] = ["BDUSS", "BDUSS_BFESS", "STOKEN", "PTOKEN", "TIEBAUID", "UBI"];

/// 二维码有效期（通行证不告诉我们，按 180 秒估）。
const QR_TTL_MS: i64 = 180_000;

/// 过期之后再等 10 秒才判 Expired（长轮询可能刚好卡在边界上）。
const EXPIRY_GRACE_MS: i64 = 10_000;

/// 轮询间隔（unicast 是长轮询，请求之间只留很短的间隔）。
const QR_POLL_INTERVAL_MS: i64 = 300;

/// 授权跳转最多跟几跳。
const MAX_AUTH_HOPS: usize = 6;

/// 给用户的提示（和 Kotlin 的 `hint` 逐字一样）。
pub const HINT: &str = "用贴吧或百度 App 扫码，并在手机上确认登录";

/// 贴吧扫码登录的平台对象。
pub struct TiebaQrSource {
    passport: String,
    web_base: String,
    /// unicast 长轮询的读超时（毫秒；Kotlin `UNICAST_READ_TIMEOUT_S` = 10 秒，测试里改小）。
    unicast_timeout_ms: i64,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
}

impl TiebaQrSource {
    /// [`app_base`] 这一层用不上（扫码只走 passport 和网页接口），留着是为了让调用方少改一行，
    /// 参数名和 `TiebaSource` 的一致。
    pub fn new(
        passport: impl Into<String>,
        app_base: impl Into<String>,
        web_base: impl Into<String>,
        unicast_timeout_ms: i64,
        qr: Arc<dyn QrHost>,
    ) -> Self {
        let _ = app_base;
        Self {
            passport: passport.into(),
            web_base: web_base.into(),
            unicast_timeout_ms,
            qr: qr.clone(),
            sessions: QrSessions::new(qr),
        }
    }

    pub fn passport(&self) -> &str {
        &self.passport
    }

    /// 第一步：要一张二维码。[`gid`] 由调用方生成（Kotlin `UUID.randomUUID().toString().uppercase()`）。
    pub fn start(&self, now_ms: i64, gid: &str) -> Result<QrLogin, BiliError> {
        let session = self.qr.begin();
        let url = format!(
            "{}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid={}&apiver=v3&tt={now_ms}&tpl=tb&_={now_ms}",
            self.passport,
            crate::hosted::query_encode(gid)
        );
        let response = self.execute(&session, url, None)?;
        let checked = self.check(&response, "getqrcode")?;
        let root = self.json(&checked.body, "百度通行证的响应不是 JSON / JSONP")?;
        let generated = generated(&root)?;
        // 图片地址只取路径和 query，主机用 passportBase（和接口同一个主机，测试里也能指向 MockWebServer）
        let img_url = rebase(&declared_image_url(&generated.image_url), &self.passport);
        let image = self.execute_with_headers(
            &session,
            img_url,
            vec![(
                "Accept".to_string(),
                "image/png,image/*;q=0.8,*/*;q=0.5".to_string(),
            )],
            None,
        )?;
        if !(200..=299).contains(&image.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", image.status),
                detail: "二维码图片".to_string(),
            });
        }
        let content = self
            .qr
            .decode_qr_image(image.body.clone())
            .ok_or_else(|| BiliError::Parse {
                detail: "二维码图片里没有解出内容".to_string(),
            })?;
        if !content.starts_with("http") {
            return Err(BiliError::Api {
                code: "qr".to_string(),
                detail: format!(
                    "百度给的二维码图片不是登录地址（扫出来是 {}），可能已经过期",
                    json::take_chars(&content, 20)
                ),
            });
        }
        self.sessions.begin(&generated.sign, &QrSession(session));
        Ok(QrLogin {
            platform: "tieba".to_string(),
            content,
            token: generated.sign,
            extra: vec![("gid".to_string(), gid.to_string())],
            expires_at_ms: Some(now_ms.saturating_add(QR_TTL_MS)),
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    /// 第二步：问一次状态。`gid` 由调用方给（`login.extra["gid"]`，没有就新生成一个）。
    pub fn poll(&self, login: &QrLogin, now_ms: i64, gid: &str) -> Result<QrState, BiliError> {
        let session = self.sessions.jar(&login.token);
        let callback = format!("bd__cbs__{}", base36(now_ms));
        let url = format!(
            "{}/channel/unicast?channel_id={}&tpl=tb&gid={}&callback={}&apiver=v3&tt={now_ms}&_={now_ms}",
            self.passport,
            crate::hosted::query_encode(&login.token),
            crate::hosted::query_encode(gid),
            crate::hosted::query_encode(&callback)
        );
        let response = match self.execute(&session.0, url, Some(self.unicast_timeout_ms)) {
            Ok(response) => response,
            // 读超时 = 长轮询里没等到事件；连接超时是真的网络问题，照常抛
            // （宿主只给 message，这里照 Kotlin 的字符串判断：带 "connect" 的当网络错误）
            Err(e) if is_connect_error(&e) => return Err(e),
            Err(BiliError::Network { .. }) => {
                return Ok(self.pending(login, now_ms));
            }
            Err(e) => return Err(e),
        };
        let checked = match self.check(&response, "unicast") {
            Ok(checked) => checked,
            Err(e) => return Err(e),
        };
        let root = self.json(&checked.body, "百度通行证的响应不是 JSON / JSONP")?;
        match unicast(&root)? {
            Event::Nothing => Ok(self.pending(login, now_ms)),
            Event::Scanned => Ok(QrState::Scanned),
            Event::Cancelled => {
                self.sessions.end(&login.token);
                Ok(QrState::Expired)
            }
            Event::Error(errno) => {
                self.sessions.end(&login.token);
                Ok(QrState::Failed(format!("百度扫码轮询出错（errno {errno}）")))
            }
            Event::Confirmed(temp_bduss) => {
                let result = self.finish(login, &session.0, &temp_bduss, gid, now_ms);
                self.sessions.end(&login.token);
                result
            }
        }
    }

    /// 等待 / 读超时：过了 `expiresAt + 10 秒` 就判过期（通行证不会告诉我们）。
    fn pending(&self, login: &QrLogin, now_ms: i64) -> QrState {
        if login.expired_at(now_ms, EXPIRY_GRACE_MS) {
            self.sessions.end(&login.token);
            QrState::Expired
        } else {
            QrState::Waiting
        }
    }

    /// 已确认：换正式 BDUSS → 补 STOKEN → 用临时罐验证 → 落地。
    fn finish(
        &self,
        login: &QrLogin,
        session: &str,
        temp_bduss: &str,
        gid: &str,
        now_ms: i64,
    ) -> Result<QrState, BiliError> {
        let url = format!(
            "{}/v3/login/main/qrbdusslogin?v={now_ms}&bduss={}&u=https%3A%2F%2Ftieba.baidu.com%2Findex.html&loginVersion=v4&qrcode=1&tpl=tb&apiver=v3&tt={now_ms}&alg=v3&time={}&gid={}&callback=bd__cbs__{}",
            self.passport,
            crate::hosted::query_encode(temp_bduss),
            now_ms / 1000,
            crate::hosted::query_encode(gid),
            base36(now_ms)
        );
        let response = self.execute(session, url, None)?;
        let checked = self.check(&response, "qrbdusslogin")?;
        let mut raw: Vec<(String, JarCookie)> = Vec::new();
        absorb_login_cookies(&checked.headers, now_ms, &mut raw);
        let result = login_result(&self.json(&checked.body, "百度通行证的响应不是 JSON / JSONP").ok().unwrap_or_default());
        if !result.ok() {
            let msg = json::take_chars(result.msg.as_deref().unwrap_or(""), 60);
            let no = result.no.clone().unwrap_or_default();
            if RISK_WORDS.iter().any(|w| msg.contains(w)) {
                return Err(BiliError::RiskControl {
                    code: Some(no),
                    detail: msg,
                });
            }
            return Ok(QrState::Failed(format!(
                "百度确认登录失败（{no} {msg}），现有登录态没有改动"
            )));
        }
        // 没有 STOKEN：带着 BDUSS 走一遍通行证给贴吧授权的跳转（逐跳看 Location 和 Set-Cookie）
        if has_login(&login_cookies(session, &raw, &self.qr), "BDUSS")
            && !has_login(&login_cookies(session, &raw, &self.qr), "STOKEN")
        {
            let mut next = Some(format!(
                "{}/v3/login/api/auth/?return_type=3&tpl=tb&u=https%3A%2F%2Ftieba.baidu.com%2Findex.html",
                self.passport
            ));
            let mut hops = 0;
            while let Some(url) = next.clone() {
                if hops >= MAX_AUTH_HOPS || has_login(&login_cookies(session, &raw, &self.qr), "STOKEN") {
                    break;
                }
                hops += 1;
                let hop = self.execute(session, url.clone(), None)?;
                absorb_login_cookies(&hop.headers, now_ms, &mut raw);
                next = crate::hosted::header_value(&hop.headers, "location")
                    .and_then(|loc| resolve_url(&url, loc))
                    .filter(|u| is_baidu_host(&url_host(u)));
            }
        }
        let logins = login_cookies(session, &raw, &self.qr);
        if !has_login(&logins, "BDUSS") {
            return Ok(QrState::Failed(
                "百度说登录成功了但没拿到 BDUSS，现有登录态没有改动".to_string(),
            ));
        }
        // 登录 Cookie 统一以 baidu.com 域写进一个新罐（和临时罐里其他 Cookie 一起），验证、写入都用它
        let final_session = self.qr.begin();
        let others: Vec<JarCookie> = self
            .qr
            .cookies(session)
            .into_iter()
            .filter(|c| !LOGIN_COOKIES.contains(&c.name.as_str()))
            .collect();
        if !others.is_empty() {
            self.qr.put(&final_session, COOKIE_URL, others);
        }
        self.qr.put(
            &final_session,
            COOKIE_URL,
            logins
                .iter()
                .map(|(_, c)| JarCookie {
                    name: c.name.clone(),
                    value: c.value.clone(),
                    domain: "baidu.com".to_string(),
                    path: "/".to_string(),
                    expires_at_ms: c.expires_at_ms,
                    secure: c.secure,
                    http_only: c.http_only,
                    host_only: false,
                })
                .collect(),
        );
        let state = match self.verify(&final_session) {
            Ok(Some(user)) => {
                let fresh = self.qr.cookies(&final_session);
                self.qr.commit(
                    COOKIE_URL,
                    LOGIN_DOMAIN,
                    OLD_LOGIN_NAMES.iter().map(|n| n.to_string()).collect(),
                    fresh,
                );
                QrState::Success(Some(user))
            }
            Ok(None) | Err(BiliError::NotLoggedIn { .. }) => QrState::Failed(
                "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
            ),
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功 → 照样写入，`user` 留空
            Err(_) => {
                let fresh = self.qr.cookies(&final_session);
                self.qr.commit(
                    COOKIE_URL,
                    LOGIN_DOMAIN,
                    OLD_LOGIN_NAMES.iter().map(|n| n.to_string()).collect(),
                    fresh,
                );
                QrState::Success(None)
            }
        };
        self.qr.end(&final_session);
        let _ = login;
        Ok(state)
    }

    /// 用某个临时罐问一次 `whoami`（`web` 客户端的头 + 那个罐的 Cookie）。
    fn verify(&self, session: &str) -> Result<Option<UserRef>, BiliError> {
        let url = format!("{}/mo/q/newmoindex?need_user=1", self.web_base);
        let response = self.execute(session, url.clone(), None)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: "newmoindex".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|_| BiliError::Parse {
            detail: "newmoindex 响应不是 JSON".to_string(),
        })?;
        let code = json::long(&root, "no").ok_or_else(|| BiliError::Parse {
            detail: "newmoindex 缺少 no".to_string(),
        })?;
        if code != 0 {
            let msg = json::text(&root, "error").unwrap_or_default();
            if is_not_logged_in(&msg) {
                return Ok(None);
            }
            return Err(classify_error(code as i32, &msg));
        }
        Ok(json::obj(&root, "data").and_then(self_user))
    }

    /// 网页 GET（`web` 客户端的默认头：百度域名才有的 UA / Referer）。
    fn execute(
        &self,
        session: &str,
        url: String,
        timeout_ms: Option<i64>,
    ) -> Result<crate::host::HttpResponse, BiliError> {
        self.execute_with_headers(session, url, Vec::new(), timeout_ms)
    }

    fn execute_with_headers(
        &self,
        session: &str,
        url: String,
        headers: Vec<(String, String)>,
        timeout_ms: Option<i64>,
    ) -> Result<crate::host::HttpResponse, BiliError> {
        let mut all = web_get_headers(&url);
        // 显式头放前面（Kotlin 的 `Request.Builder(...).header(...)` 也是先请求级、再拦截器补）
        let mut merged = headers;
        merged.extend(all.drain(..));
        self.qr
            .execute(
                session,
                HttpRequest {
                    method: "GET".to_string(),
                    url,
                    headers: merged,
                    body: Vec::new(),
                },
                timeout_ms,
            )
            .map_err(BiliError::from)
    }

    /// Kotlin `qrChecked`：403 / 429 → 风控；非 2xx → `Api`（信息里不带响应内容）。
    fn check(
        &self,
        response: &crate::host::HttpResponse,
        what: &str,
    ) -> Result<crate::host::HttpResponse, BiliError> {
        if response.status == 403 || response.status == 429 {
            return Err(BiliError::RiskControl {
                code: Some(format!("HTTP {}", response.status)),
                detail: format!("百度通行证拒绝了请求（{what}）"),
            });
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: what.to_string(),
            });
        }
        Ok(response.clone())
    }

    /// 百度通行证的 JSON / JSONP（出错信息里不带响应内容）。
    fn json(&self, body: &[u8], parse_error: &str) -> Result<JsonMap, BiliError> {
        let text = String::from_utf8_lossy(body);
        jsonp_object(&text).map_err(|detail| BiliError::Parse {
            detail: if detail.is_empty() {
                parse_error.to_string()
            } else {
                detail
            },
        })
    }
}

/// 读超时（不是连接超时）：宿主只给 message，这里照 Kotlin 的 `contains("connect")` 判断。
fn is_connect_error(e: &BiliError) -> bool {
    match e {
        BiliError::Network { detail } => detail.to_lowercase().contains("connect"),
        _ => false,
    }
}

/// 百度通行证的 JSON / JSONP：`callback({...})` 去掉外壳；不是 JSON 对象就报错。
pub fn jsonp_object(body: &str) -> Result<JsonMap, String> {
    let text = body.trim();
    let inner = if text.starts_with('{') {
        text
    } else {
        let open = text.find('(');
        let close = text.rfind(')');
        match (open, close) {
            (Some(open), Some(close)) if close > open => &text[open + 1..close],
            _ => return Err("百度通行证的响应不是 JSON / JSONP".to_string()),
        }
    };
    match json::parse_object(inner) {
        Ok(root) => Ok(root),
        Err(_) => Err("百度通行证的响应不是合法 JSON".to_string()),
    }
}

/// `getqrcode` 的响应：`sign`（也是 unicast 的 channel_id）和二维码图片地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrGenerated {
    pub sign: String,
    pub image_url: String,
}

pub fn generated(root: &JsonMap) -> Result<QrGenerated, BiliError> {
    let errno = json::long(root, "errno").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 errno".to_string(),
    })?;
    if errno != 0 {
        return Err(BiliError::Api {
            code: errno.to_string(),
            detail: "百度没给二维码".to_string(),
        });
    }
    let sign = json::text(root, "sign").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 sign".to_string(),
    })?;
    let img = json::text(root, "imgurl").ok_or_else(|| BiliError::Parse {
        detail: "二维码响应缺少 imgurl".to_string(),
    })?;
    Ok(QrGenerated {
        sign,
        image_url: if img.starts_with("http") {
            img
        } else {
            format!("https://{img}")
        },
    })
}

/// 图片地址重挂到 `base` 上（Kotlin：`"$passportBase${img.encodedPath}" + (query ?: "")`）。
fn declared_image_url(url: &str) -> String {
    url.to_string()
}

fn rebase(url: &str, base: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let path_start = rest.find(['/', '?']).unwrap_or(rest.len());
    let path_and_query = &rest[path_start..];
    format!("{base}{path_and_query}")
}

/// `unicast` 的一次响应（Kotlin `TiebaQrParser.Event`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 服务端等到超时都没事件（`errno 1`）：继续等。
    Nothing,
    Scanned,
    /// 手机上确认了；临时凭据**不能打印**。
    Confirmed(String),
    /// 手机上取消 / 别的状态。
    Cancelled,
    Error(i64),
}

pub fn unicast(root: &JsonMap) -> Result<Event, BiliError> {
    let errno = json::long(root, "errno").ok_or_else(|| BiliError::Parse {
        detail: "轮询响应缺少 errno".to_string(),
    })?;
    if errno == 1 {
        return Ok(Event::Nothing);
    }
    if errno != 0 {
        return Ok(Event::Error(errno));
    }
    // channel_v 在顶层还是 data 里、是 JSON 字符串还是对象，都兼容
    let Some(raw) = root.get("channel_v").or_else(|| json::obj(root, "data").and_then(|d| d.get("channel_v"))) else {
        return Ok(Event::Nothing);
    };
    let value = match raw {
        serde_json::Value::Object(_) => raw.clone(),
        serde_json::Value::String(text) => {
            if text.trim().is_empty() {
                return Ok(Event::Nothing);
            }
            match jsonp_object(text) {
                Ok(root) => serde_json::Value::Object(root),
                Err(_) => return Ok(Event::Nothing),
            }
        }
        _ => return Ok(Event::Nothing),
    };
    let Some(v) = value.as_object() else {
        return Ok(Event::Nothing);
    };
    Ok(match json::int(v, "status") {
        Some(1) => Event::Scanned,
        Some(0) => match json::text(v, "v") {
            Some(temp) => Event::Confirmed(temp),
            None => Event::Cancelled,
        },
        _ => Event::Cancelled,
    })
}

/// `qrbdusslogin` 的结果：`errInfo.no` 为 "0" 才算成功（没有 `errInfo` 也当成功）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginResult {
    pub no: Option<String>,
    pub msg: Option<String>,
}

impl LoginResult {
    pub fn ok(&self) -> bool {
        self.no.as_deref().map(|no| no == "0").unwrap_or(true)
    }
}

pub fn login_result(root: &JsonMap) -> LoginResult {
    let info = json::obj(root, "errInfo");
    LoginResult {
        no: info.and_then(|i| json::text(i, "no")),
        msg: info.and_then(|i| json::text(i, "msg")),
    }
}

/// 风控关键词（和 Kotlin `RISK_WORDS` 一样）。
const RISK_WORDS: [&str; 6] = ["频繁", "验证", "安全", "风险", "异常", "封禁"];

/// 把响应里 `Set-Cookie` 的登录 Cookie 收进 `into`（**同名覆盖**，保持首次出现的顺序）。
///
/// Kotlin 用的是 `Cookie.parse(COOKIE_URL, header)`：按**发给 `tieba.baidu.com`** 解析，
/// 所以 passport 下发的 `Domain=tieba.baidu.com` 的 STOKEN 也认；过期 / 空值 / `deleted` 的不要。
fn absorb_login_cookies(
    headers: &[(String, String)],
    now_ms: i64,
    into: &mut Vec<(String, JarCookie)>,
) {
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let Some(cookie) = parse_set_cookie(value, now_ms) else {
            continue;
        };
        if !LOGIN_COOKIES.contains(&cookie.name.as_str())
            || cookie.value.is_empty()
            || cookie.value == "deleted"
            || cookie.expires_at_ms <= now_ms
        {
            continue;
        }
        if let Some(existing) = into.iter_mut().find(|(n, _)| *n == cookie.name) {
            existing.1 = cookie;
        } else {
            into.push((cookie.name.clone(), cookie));
        }
    }
}

/// 临时罐里的登录 Cookie + 原始 Set-Cookie 里的（同名的以原始 Set-Cookie 为准，顺序是首次出现的顺序）。
fn login_cookies(
    session: &str,
    raw: &[(String, JarCookie)],
    qr: &Arc<dyn QrHost>,
) -> Vec<(String, JarCookie)> {
    let mut out: Vec<(String, JarCookie)> = Vec::new();
    let mut put = |name: String, cookie: JarCookie| {
        if let Some(existing) = out.iter_mut().find(|(n, _)| *n == name) {
            existing.1 = cookie;
        } else {
            out.push((name, cookie));
        }
    };
    for cookie in qr.cookies(session) {
        if LOGIN_COOKIES.contains(&cookie.name.as_str()) && !cookie.value.is_empty() {
            put(cookie.name.clone(), cookie);
        }
    }
    for (name, cookie) in raw {
        put(name.clone(), cookie.clone());
    }
    out
}

/// `login_cookies` 的结果里有没有这个名字。
fn has_login(cookies: &[(String, JarCookie)], name: &str) -> bool {
    cookies.iter().any(|(n, _)| n == name)
}

/// 最小的 `Set-Cookie` 解析（OkHttp `Cookie.parse` 的常用子集）：
/// `name=value; Expires=…; Max-Age=…; Domain=…; Path=…; Secure; HttpOnly`。
///
/// 过期时间：`Max-Age` 优先（秒），没有就 `Expires`（RFC 1123 / 常见变体）；都没有就是会话 Cookie
/// （这里给 `i64::MAX`，和 OkHttp 的"会话 Cookie"等价——我们用 `> now` 判有效）。
pub fn parse_set_cookie(header: &str, now_ms: i64) -> Option<JarCookie> {
    let mut parts = header.split(';');
    let first = parts.next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut expires_at_ms = i64::MAX;
    let mut secure = false;
    let mut http_only = false;
    let mut path = "/".to_string();
    for part in parts {
        let part = part.trim();
        let (key, val) = match part.split_once('=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim().to_string()),
            None => (part.to_lowercase(), String::new()),
        };
        match key.as_str() {
            "max-age" => {
                if let Ok(seconds) = val.parse::<i64>() {
                    expires_at_ms = now_ms.saturating_add(seconds.saturating_mul(1000));
                }
            }
            "expires" => {
                // Max-Age 更优先：只有还没被 Max-Age 设过时才用它
                if expires_at_ms == i64::MAX {
                    if let Some(ms) = parse_http_date(&val) {
                        expires_at_ms = ms;
                    }
                }
            }
            "domain" => {
                // 百度域下的都认（Kotlin 按 tieba.baidu.com 解析，域名对不上也不丢）
            }
            "path" => {
                if !val.is_empty() {
                    path = val;
                }
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            _ => {}
        }
    }
    Some(JarCookie {
        name: name.to_string(),
        value: value.trim().to_string(),
        domain: "baidu.com".to_string(),
        path,
        expires_at_ms,
        secure,
        http_only,
        host_only: false,
    })
}

/// `Expires=Wed, 21 Oct 2015 07:28:00 GMT` → Unix 毫秒。只认常见写法，认不出给 `None`。
fn parse_http_date(text: &str) -> Option<i64> {
    // 去掉星期几
    let rest = match text.split_once(',') {
        Some((_, rest)) => rest.trim(),
        None => text.trim(),
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }
    let day: i64 = fields[0].parse().ok()?;
    let month = match fields[1].to_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i64 = fields[2].parse().ok()?;
    let time: Vec<&str> = fields[3].split(':').collect();
    if time.len() != 3 {
        return None;
    }
    let hour: i64 = time[0].parse().ok()?;
    let minute: i64 = time[1].parse().ok()?;
    let second: i64 = time[2].parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1000)
}

/// 公历 → 纪元日（和 `hosted::civil_from_days` 互逆）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `host` 是不是百度的（Kotlin `isBaiduHost` 用的是 `is_baidu_host` + 两个根）。
fn is_baidu_host(host: &str) -> bool {
    crate::tieba::source::is_baidu_host(host)
}

fn url_host(url: &str) -> String {
    crate::tieba::source::url_host(url)
}

/// 相对 / 绝对 `Location` → 绝对地址（Kotlin `hop.url.resolve(location)` 的常用子集）。
fn resolve_url(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_string());
    }
    let scheme_end = base.find("://")?;
    let scheme = &base[..scheme_end];
    let rest = &base[scheme_end + 3..];
    let host_end = rest.find(['/', '?' ]).unwrap_or(rest.len());
    let host = &rest[..host_end];
    if location.starts_with("//") {
        return Some(format!("{scheme}:{location}"));
    }
    if location.starts_with('/') {
        return Some(format!("{scheme}://{host}{location}"));
    }
    // 相对路径：拼到 base 的目录上（够用就行：授权跳转给的是绝对路径 / 绝对地址）
    let path_end = base.find('?').unwrap_or(base.len());
    let path = &base[..path_end];
    let dir_end = path.rfind('/').map(|i| i + 1).unwrap_or(path.len());
    Some(format!("{}{location}", &path[..dir_end]))
}

/// `now_ms` → base36（Kotlin `Long.toString(36)`，小写）。
fn base36(mut value: i64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let negative = value < 0;
    if negative {
        value = -value;
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    if negative {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

