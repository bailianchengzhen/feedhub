//! 小红书的**网页扫码登录**：请求 / 签名 / 轮询状态 / Cookie 落地全在这里。
//!
//! 逐条对照 `source-xiaohongshu/.../XhsSource.kt` 的 `startQrLogin` / `pollQrLogin` / `finishQrLogin` /
//! `seedQrJar` / `putWebSession` 和 `XhsQr.kt`：
//!
//! 1. 开一个**空的临时罐**（宿主的 [`QrHost::begin`]），把本机已有的、域是 `xiaohongshu.com`（含子域）的
//!    Cookie 复制进去——**没有 `a1` 就一个请求都不发**（每个请求的签名都用它）；
//! 2. `POST /api/sns/web/v1/login/qrcode/create`，body `{"qr_type":1}` → `data {qr_id, code, url}`；
//!    顺手记下生成时的**游客 `web_session`**（成功后要拿它比对，防"没换成正式会话"）；
//! 3. 轮询 `POST /api/qrcode/userinfo`，body `{"qrId":…,"code":…}` → `data.codeStatus`：
//!    `0` 未扫 / `1` 已扫待确认 / `3` 过期 / `2` 已确认；没见过的值 → `Failed`（不瞎猜）；
//! 4. 确认之后 `GET /api/sns/web/v1/login/qrcode/status?qr_id=…&code=…`（**空的 `x-login-mode` 头照着网页发**）
//!    → `data.code_status` 是 `2` 才算成功，正式会话在 `data.login_info.session`；
//! 5. 三条判定（照 cv-cat，另外补了"和游客会话比对"）：拿到 `login_info.session` 就直接用；
//!    只从 Set-Cookie 拿到 `web_session` 时它必须和生成二维码时那个**不一样**；都没有 / 没变就是
//!    `Failed("…拿到的还是游客会话…")`，**现有登录态一个都不动**；
//! 6. 用**临时罐**问一次 `v2/user/me` 确认平台真的认这个会话，再清掉旧的登录 Cookie
//!    （[`LOGIN_COOKIES`]：`web_session` / `id_token`，`a1` / `loadts` / `webBuild` 这些设备 Cookie 保留）、
//!    把临时罐里的全部 Cookie 写进 `ctx.cookies`（[`QrHost::commit`]）；
//! 7. 全程**不碰** `ctx.cookies`：成功之前一格都不动（取消 / 过期 / 失败都不动）。
//!
//! 风控（HTTP 461 / 471、300011 / 300012、`300015`）照 Kotlin 原样抛，**不重试、不换参数**。
//! **时间 / 随机数都由调用方注入**（`now_ms` / [`SignRandom`]）；签名计数**和平台对象共用一份**
//! （[`SharedSign`]）——一次会话里的 `seq` / `signCount` / `xraySeq` 是一条序列（Kotlin 那边
//! `XhsSource` 的所有签名都走同一个 `XhsSigner`）。
//!
//! 三条"请求为什么这么拼"的细节：
//! - 签名 / 默认头 / Content-Type 的**顺序**：显式给的 `extra` 头（`qrcode/status` 的空 `x-login-mode`）
//!   → 默认头 → 签名头 → `Content-Type`（来自 `RequestBody`，BridgeInterceptor 排在应用拦截器之后）；
//! - **不签 `x-rap-param` / `xy-direction`**：这三个路径都不在 `XhsApi.RAP_PATHS` / `XY_PATHS` 里；
//! - 错误信息里**不带响应内容**（`check_http_secret`；成功的响应里有 `login_info.session`）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::account::{QrLogin, QrSession, QrSessions, QrState};
use crate::host::{HttpRequest, JarCookie, QrHost, StoreHost};
use crate::json::{self, JsonMap};
use crate::model::{BiliError, UserRef};
use crate::xhs::sign::{self, SignRandom, SignRequest, SignState};
use crate::xhs::{self, account, codec};

/// 生成二维码：`{"qr_type":1}`（网页就是这个，只有一个字段）。
const CREATE: &str = "/api/sns/web/v1/login/qrcode/create";
/// 轮询：`{"qrId":…,"code":…}`。
const USERINFO: &str = "/api/qrcode/userinfo";
/// 确认之后换正式会话。
const STATUS: &str = "/api/sns/web/v1/login/qrcode/status";
/// "我是谁"（落地前的验证）。
const ME: &str = "/api/sns/web/v2/user/me";

/// 生成二维码的请求体（逐字节）。
const CREATE_BODY: &str = r#"{"qr_type":1}"#;

/// `x-rap-param` **不**带的路径（这三个都不在 `XhsApi.RAP_PATHS` 里；签名器里 `rap: false`）。
/// `xy-direction` 同样不带（只有 homefeed / feed 带）。

/// `data.codeStatus` 的四个值。
const WAITING: i32 = 0;
const SCANNED: i32 = 1;
const CONFIRMED: i32 = 2;
const EXPIRED: i32 = 3;

/// `qrcode/status` 响应里的 `data.code_status` 是 2 才算成功（字段名和上面那个**不一样**，照实抓）。
const LOGIN_OK: i64 = 2;

/// `QrLogin.extra` 里存二维码 `code` 用的键（Kotlin `XhsQr.CODE`）。
pub const CODE_KEY: &str = "code";

/// 轮询间隔（Kotlin `QR_POLL_INTERVAL_MS`；限速仍然由 `XhsApi` 那 1.5 秒 + 抖动管）。
const QR_POLL_INTERVAL_MS: i64 = 2_000;

/// 给用户的提示（和 Kotlin `startQrLogin` 的 `hint` 逐字一样）。
pub const HINT: &str = "用小红书 App 扫码，并在手机上确认登录";

/// 换账号时要清掉的登录 Cookie（`a1` / `loadts` / `webBuild` 这些设备 Cookie 保留）。
pub const LOGIN_COOKIES: [&str; 2] = ["web_session", "id_token"];

/// 登录 Cookie 的域（`LoginSpec.cookieDomain`，**带**前导点；`remove` / 比较时自己裁）。
pub const LOGIN_DOMAIN: &str = ".xiaohongshu.com";

/// 身份 URL（`LoginSpec.cookieUrl` / `XhsApi.WEB_URL`）：按 `www.xiaohongshu.com` 取 Cookie。
pub const COOKIE_URL: &str = "https://www.xiaohongshu.com/";

/// 正式会话的有效期：一年（Kotlin `YEAR_MS`）。
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;

/// POST 的 Content-Type（`XhsApi.JSON_TYPE`，逐字节一样）。
const JSON_TYPE: &str = "application/json;charset=UTF-8";

/// 一次扫码里**每一次签名**的随机量（小红书每个请求都要一组：`version` / 两个 traceid / `x-t` 用同一个钟）。
///
/// **为什么是一次性给一串、而不是一个 `&SignRandom`**：一个 `poll` 可能签 2～3 个请求
/// （`userinfo` → `qrcode/status` → `user/me`），而 Kotlin 那边每签一个就从自己的 `Random` 里取一组
/// （`RustXhsParse.sign` 每次都被拦截器调一次）。所以边界上按"最多要几组"一次给足，
/// Rust 按发请求的顺序**依次消费**（多给的不用、少了就是宿主算错了）。
///
/// 取完还往下要就是编程错误：`None` → `BiliError::Host`（不会悄悄复用上一组随机量）。
pub struct RandCursor<'a> {
    items: &'a [SignRandom],
    next: std::cell::Cell<usize>,
}

impl<'a> RandCursor<'a> {
    pub fn new(items: &'a [SignRandom]) -> Self {
        Self {
            items,
            next: std::cell::Cell::new(0),
        }
    }

    /// 这一次签名要用的那一组；用完了返回 `None`。
    pub fn take(&self) -> Option<&'a SignRandom> {
        let index = self.next.get();
        let item = self.items.get(index)?;
        self.next.set(index + 1);
        Some(item)
    }

    pub fn used(&self) -> usize {
        self.next.get()
    }
}

/// 和平台对象**共用的签名计数**（一次会话一条序列）。
///
/// 小红书这一家必须共用：`XhsSource` 里所有签名（推荐流、详情、表情表、扫码、`whoami`）用的都是
/// 同一个 `XhsSigner`，`seq` / `signCount` / `xraySeq` 是一条流水。Rust 侧照抄——扫码对象和平台对象
/// 拿到的是**同一个 `Arc`**（构造时由 Kotlin 把平台对象的那一份交进来）。
pub type SharedSign = Arc<Mutex<SignState>>;

/// 小红书扫码登录的平台对象。
///
/// 状态：临时罐的 `token → 句柄` 表（[`QrSessions`]，LRU 8，语义照 Kotlin `QrSessions`）、
/// 每张二维码生成时的**游客会话**（Kotlin `guestSessions`）、共用的签名计数、`ctx.store`（b1 / user_id）。
/// 没有后台线程、没有定时器。
pub struct XhsQrSource {
    base: String,
    profile: sign::Profile,
    store: Arc<dyn StoreHost>,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
    sign: SharedSign,
    /// `qr_id → 生成二维码时临时罐里的 web_session`（Kotlin `guestSessions`）。
    guests: Mutex<HashMap<String, String>>,
}

impl XhsQrSource {
    /// [`profile_json`] 就是平台对象那一份（Kotlin 读好的 `sign_profile.json` 或 `ctx.store` 里的覆盖版）；
    /// [`sign`] 用平台对象的那一份——两边必须是**同一个 `Arc`**（见 [`SharedSign`]）。
    pub fn new(
        base: impl Into<String>,
        profile_json: &str,
        sign: SharedSign,
        store: Arc<dyn StoreHost>,
        qr: Arc<dyn QrHost>,
    ) -> Result<Self, String> {
        Ok(Self {
            base: base.into(),
            profile: sign::Profile::parse(profile_json)?,
            store,
            sessions: QrSessions::new(qr.clone()),
            qr,
            sign,
            guests: Mutex::new(HashMap::new()),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 第一步：要一张二维码。`token` 就是轮询用的 `qr_id`，`extra` 里带 `code`。
    ///
    /// **没有 `a1` 就一个请求都不发**（Kotlin 也是这个顺序：先复制临时罐、再检查 a1）。
    pub fn start(&self, now_ms: i64, rand: &RandCursor<'_>) -> Result<QrLogin, BiliError> {
        let session = self.qr.begin();
        // seedQrJar：本机已有的小红书 Cookie（不含别的平台的）复制进临时罐
        let existing: Vec<JarCookie> = self
            .qr
            .all()
            .into_iter()
            .filter(|c| domain_matches(&c.domain, LOGIN_DOMAIN))
            .collect();
        if !existing.is_empty() {
            self.qr.put(&session, COOKIE_URL, existing.clone());
        }
        if !existing
            .iter()
            .any(|c| c.name == "a1" && !c.value.trim().is_empty())
        {
            return Err(BiliError::Api {
                code: "a1".to_string(),
                detail: "小红书扫码要用现有的 a1 设备 Cookie（每个请求的签名用它）：先把本机的登录态弄进来——\
「网页登录」打开一次小红书官网会种下 a1，或者从电脑「同步到手机」/「从 Chrome 导入」"
                    .to_string(),
            });
        }

        let root = self.post_json(&session, CREATE, CREATE_BODY, now_ms, rand)?;
        let generated = generated(&root)?;
        let guest = self
            .qr
            .cookies(&session)
            .into_iter()
            .find(|c| c.name == "web_session")
            .map(|c| c.value)
            .unwrap_or_default();
        self.guests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(generated.qr_id.clone(), guest);
        self.sessions.begin(&generated.qr_id, &QrSession(session));

        Ok(QrLogin {
            platform: "xhs".to_string(),
            content: generated.url,
            token: generated.qr_id,
            extra: vec![(CODE_KEY.to_string(), generated.code)],
            // 二维码多久失效平台会告诉我们（`codeStatus = 3`），不猜时间
            expires_at_ms: None,
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    /// 第二步：问一次状态。终态（成功 / 过期 / 失败）都会把临时罐丢掉；
    /// **风控**（[`BiliError::RiskControl`]）也丢，别的错误（网络 / 解析）留给下一次轮询
    /// ——和 Kotlin 的 `catch (e: RiskControlException) { endQr(); throw e }` 一样。
    pub fn poll(&self, login: &QrLogin, now_ms: i64, rand: &RandCursor<'_>) -> Result<QrState, BiliError> {
        let code = match login.extra(CODE_KEY) {
            Some(code) if !code.trim().is_empty() => code.to_string(),
            _ => {
                return Err(BiliError::Parse {
                    detail: "这次扫码的 token 里没有 code，重新生成二维码".to_string(),
                })
            }
        };
        let session = self.sessions.jar(&login.token);
        let body = check_body(&login.token, &code);
        let root = match self.post_json(&session.0, USERINFO, &body, now_ms, rand) {
            Ok(root) => root,
            Err(e) => {
                if matches!(e, BiliError::RiskControl { .. }) {
                    self.end(&login.token);
                }
                return Err(e);
            }
        };
        match status_of(&root) {
            Some(WAITING) => Ok(QrState::Waiting),
            Some(SCANNED) => Ok(QrState::Scanned),
            Some(EXPIRED) => {
                self.end(&login.token);
                Ok(QrState::Expired)
            }
            Some(CONFIRMED) => self.finish(login, &code, &session.0, now_ms, rand),
            None => {
                self.end(&login.token);
                Ok(QrState::Failed(
                    "二维码状态接口没有给 codeStatus，现有登录态没有改动".to_string(),
                ))
            }
            Some(other) => {
                self.end(&login.token);
                Ok(QrState::Failed(format!(
                    "二维码状态是没见过的值（{other}），现有登录态没有改动"
                )))
            }
        }
    }

    /// 确认之后：换正式会话 → 用临时罐验证 → 落地（先删旧登录 Cookie、再写新的）。
    fn finish(
        &self,
        login: &QrLogin,
        code: &str,
        session: &str,
        now_ms: i64,
        rand: &RandCursor<'_>,
    ) -> Result<QrState, BiliError> {
        let query = vec![
            ("qr_id".to_string(), login.token.clone()),
            ("code".to_string(), code.to_string()),
        ];
        // `qrcode/status` 要带一个**空的** `x-login-mode` 头（照网页实抓）
        let root = self.get_json(
            session,
            STATUS,
            &query,
            &[("x-login-mode".to_string(), String::new())],
            now_ms,
            rand,
        )?;
        let result = login_result(&root);
        if !result.confirmed {
            self.end(&login.token);
            return Ok(QrState::Failed(format!(
                "平台说已经确认了，但登录状态接口没给成功（code_status 不是 {LOGIN_OK}），现有登录态没有改动"
            )));
        }
        let guest = self
            .guests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&login.token)
            .cloned()
            .unwrap_or_default();
        let from_jar = self
            .qr
            .cookies(session)
            .into_iter()
            .filter(|c| c.name == "web_session" && !c.value.trim().is_empty())
            .last()
            .map(|c| c.value);
        let fresh = result.session.clone().or(from_jar);
        match fresh {
            Some(s) if !s.trim().is_empty() && s != guest => {}
            _ => {
                self.end(&login.token);
                return Ok(QrState::Failed(
                    "登录成功了但拿到的还是游客会话（没有换成正式 web_session），现有登录态没有改动"
                        .to_string(),
                ));
            }
        }
        if let Some(new_session) = result.session.as_deref() {
            self.put_web_session(session, new_session, now_ms);
        }
        // commitQrLogin：先用临时罐问一次"我是谁"，再删旧 Cookie、写新的（顺序不能反）
        let state = self.commit(session, now_ms, rand);
        self.end(&login.token);
        Ok(state)
    }

    /// Kotlin `SourceContext.commitQrLogin` 的移植。
    ///
    /// 1. **一个 Cookie 都没有** → `Failed`，什么都不动；
    /// 2. 用临时罐问一次"我是谁"：`None` / 未登录 → `Failed`（一格 Cookie 都不动）；
    ///    网络 / 风控等验证不了 → 照样写入、`user` 留空（Kotlin 的判断一样）；
    /// 3. 先 `remove(LOGIN_DOMAIN, LOGIN_COOKIES)` 再 `put(COOKIE_URL, 临时罐里的全部)`。
    ///
    /// 注意 `verify` 在 `commit` **之前**跑：它可能写 `xhs.user_id`，但那和 `ctx.cookies` 无关，
    /// 而且 `whoami` 本来就会写（Kotlin 也一样）。
    fn commit(&self, session: &str, now_ms: i64, rand: &RandCursor<'_>) -> QrState {
        let cookies = self.qr.cookies(session);
        if cookies.is_empty() {
            return QrState::Failed("登录成功了但没拿到任何 Cookie，现有登录态没有改动".to_string());
        }
        let user = match self.verify(session, now_ms, rand) {
            Ok(Some(user)) => Some(user),
            Ok(None) | Err(BiliError::NotLoggedIn { .. }) => {
                return QrState::Failed(
                    "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
                )
            }
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功（这个二维码也用掉了），
            // 照样写入，`user` 留空让前端再问一次（Kotlin `commitQrLogin` 的同一个判断）。
            Err(_) => None,
        };
        self.qr.commit(
            COOKIE_URL,
            LOGIN_DOMAIN,
            LOGIN_COOKIES.iter().map(|n| n.to_string()).collect(),
            cookies,
        );
        QrState::Success(user)
    }

    /// 用临时罐问一次"我是谁"（和平台对象的 `whoami` 同一个请求 / 同一个解析，只是换一个罐）。
    ///
    /// Kotlin 那边是 `XhsSource(ctx.withCookieJar(platform, jar), …).whoami()`：先看
    /// `a1` + `web_session` 在不在（不在就是 `null`，**不发请求**），401 / `-100` / `-101` 折成 `null`。
    fn verify(
        &self,
        session: &str,
        now_ms: i64,
        rand: &RandCursor<'_>,
    ) -> Result<Option<UserRef>, BiliError> {
        let has_session = self
            .qr
            .find(session, COOKIE_URL, "a1")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_login = has_session
            && self
                .qr
                .find(session, COOKIE_URL, "web_session")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
        if !has_login {
            return Ok(None);
        }
        let root = match self.get_json(session, ME, &[], &[], now_ms, rand) {
            Ok(root) => root,
            Err(BiliError::NotLoggedIn { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let me = account::me_from(&root);
        if let Some(user) = &me {
            // `api.userId = me.id`：算 `xy-direction` 用（和 Kotlin 一样写进 store，键也同一个）
            self.store
                .put(crate::xhs::source::KEY_USER_ID, Some(&user.id));
        }
        Ok(me)
    }

    /// 正式会话（`login_info.session`）写回临时罐：同域同路径的旧值会被替换（Kotlin `putWebSession`）。
    fn put_web_session(&self, session: &str, value: &str, now_ms: i64) {
        self.qr.put(
            session,
            COOKIE_URL,
            vec![JarCookie {
                name: "web_session".to_string(),
                value: value.to_string(),
                domain: LOGIN_DOMAIN.trim_start_matches('.').to_string(),
                path: "/".to_string(),
                expires_at_ms: now_ms.saturating_add(YEAR_MS),
                secure: false,
                http_only: false,
                host_only: false,
            }],
        );
    }

    fn end(&self, token: &str) {
        self.sessions.end(token);
        self.guests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }

    // -----------------------------------------------------------------------
    // 请求
    // -----------------------------------------------------------------------

    /// POST 一段紧凑 JSON（签名包含 body）。
    fn post_json(
        &self,
        session: &str,
        path: &str,
        body: &str,
        now_ms: i64,
        rand: &RandCursor<'_>,
    ) -> Result<JsonMap, BiliError> {
        let built = Built {
            method: "POST",
            path,
            query: Vec::new(),
            body: Some(body.to_string()),
        };
        self.send(session, &built, &[], now_ms, rand)
    }

    /// GET + 查询参数（签名包含路径 + `?` + 查询串）。
    fn get_json(
        &self,
        session: &str,
        path: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        now_ms: i64,
        rand: &RandCursor<'_>,
    ) -> Result<JsonMap, BiliError> {
        let built = Built {
            method: "GET",
            path,
            query: query.to_vec(),
            body: None,
        };
        self.send(session, &built, extra, now_ms, rand)
    }

    /// 签名 → 拼头 → 回调宿主发出去 → 判状态码 / 业务码 → 返回整个 JSON 对象。
    ///
    /// 头的顺序照 Kotlin 最终发出去的那一份：**调用方显式给的 `extra` 头 → 默认头 → 签名头 → Content-Type**
    /// （`extra` 是 `Request.Builder.header()` 加的，排在默认头之前；Content-Type 来自 `RequestBody`，
    /// BridgeInterceptor 排在应用拦截器之后，所以它落在最后）。
    fn send(
        &self,
        session: &str,
        built: &Built,
        extra: &[(String, String)],
        now_ms: i64,
        rand: &RandCursor<'_>,
    ) -> Result<JsonMap, BiliError> {
        let a1 = match self.qr.find(session, COOKIE_URL, "a1") {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Err(BiliError::NotLoggedIn {
                    detail: Some("没有小红书的 a1 Cookie，先导入登录态".to_string()),
                })
            }
        };
        let loadts = self
            .qr
            .find(session, COOKIE_URL, "loadts")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(now_ms - 10 * 60_000);
        let web_build = self
            .qr
            .find(session, COOKIE_URL, "webBuild")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| self.profile.web_build.clone());
        let b1 = self.b1(loadts, now_ms);
        let user_id = self.store.get(crate::xhs::source::KEY_USER_ID);
        let request = SignRequest {
            api: built.api(),
            body: built.body.clone(),
            a1,
            loadts_ms: loadts,
            web_build,
            b1,
            user_id,
            // 这三个路径都不带 x-rap-param / xy-direction（`XhsApi.RAP_PATHS` / `XY_PATHS`）
            rap: false,
            xy: false,
        };
        let one = rand.take().ok_or_else(|| BiliError::Host {
            detail: "扫码这一次给的随机量不够（每签一个请求要一组），宿主算错了".to_string(),
        })?;
        let headers = {
            let mut state = self.sign.lock().unwrap_or_else(|e| e.into_inner());
            let (headers, next) =
                sign::sign(&self.profile, &request, now_ms, one, *state).map_err(sign_failed)?;
            *state = next;
            headers
        };
        // 头的顺序照 Kotlin **最终**发出去的那一份：显式 `extra` 头 → 默认头 → 签名头 → Content-Type。
        // 验证那一跳（`user/me`）在 Kotlin 那边属于 `NO_CACHE_PATHS`，多 `cache-control` / `pragma` 两行，
        // 位置在默认头**之内**（`priority` 之后）——Rust 这边照同一个位置补，顺序才逐字节一致。
        let mut all: Vec<(String, String)> = Vec::with_capacity(headers.len() + 17);
        all.extend(extra.iter().cloned());
        all.extend(self.default_headers());
        if is_no_cache(built.path) {
            all.push(("cache-control".to_string(), "no-cache".to_string()));
            all.push(("pragma".to_string(), "no-cache".to_string()));
        }
        all.extend(headers);
        if built.body.is_some() {
            all.push(("Content-Type".to_string(), JSON_TYPE.to_string()));
        }
        let response = self
            .qr
            .execute(
                session,
                HttpRequest {
                    method: built.method.to_string(),
                    url: built.url(&self.base),
                    headers: all,
                    body: built
                        .body
                        .as_ref()
                        .map(|b| b.clone().into_bytes())
                        .unwrap_or_default(),
                },
                None,
            )
            .map_err(BiliError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        // **secret**：出错信息里不带响应内容（成功的响应里有 login_info.session）
        xhs::check_http_secret(&body, response.status as i32)
    }

    /// 平台默认头，顺序照 Kotlin `XhsApi.defaultHeaders()`（宿主只补缺失的同名头，不会重复）。
    fn default_headers(&self) -> Vec<(String, String)> {
        let p = &self.profile;
        vec![
            ("User-Agent".to_string(), p.user_agent.clone()),
            (
                "Accept".to_string(),
                "application/json, text/plain, */*".to_string(),
            ),
            ("Accept-Language".to_string(), p.accept_language.clone()),
            ("Origin".to_string(), xhs::WEB.to_string()),
            ("Referer".to_string(), COOKIE_URL.to_string()),
            ("sec-ch-ua".to_string(), p.sec_ch_ua.clone()),
            ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
            (
                "sec-ch-ua-platform".to_string(),
                format!("\"{}\"", p.platform),
            ),
            ("sec-fetch-dest".to_string(), "empty".to_string()),
            ("sec-fetch-mode".to_string(), "cors".to_string()),
            ("sec-fetch-site".to_string(), "same-site".to_string()),
            ("priority".to_string(), "u=1, i".to_string()),
        ]
    }

    /// b1：`ctx.store` 的 `xhs.b1`（长度 > 200 就直接用），没有就按 profile + 时间生成并写回。
    /// 扫码的临时罐也用**同一份**（和 Kotlin 一样：b1 是设备级的，不分罐）。
    fn b1(&self, loadts_ms: i64, now_ms: i64) -> String {
        if let Some(cached) = self.store.get(crate::xhs::source::KEY_B1) {
            if cached.chars().count() > 200 {
                return cached;
            }
        }
        let b1 = sign::b1::encode(&self.profile, now_ms, loadts_ms);
        self.store.put(crate::xhs::source::KEY_B1, Some(&b1));
        b1
    }
}

/// 拼好的一个请求（URL + 签名用同一个 builder，避免"两边各拼一遍"）。
#[derive(Debug, Clone)]
struct Built<'a> {
    method: &'static str,
    path: &'a str,
    query: Vec<(String, String)>,
    body: Option<String>,
}

impl Built<'_> {
    /// 网址里的那串（编码和 Kotlin `XhsApi.encodeQuery` 一样：`encodeURIComponent` + 逗号保留）。
    fn encoded_query(&self) -> String {
        self.query
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    xhs::encode_component(k),
                    xhs::encode_component(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    /// 签名用的 `api`：POST 是路径，GET 是路径 + `?` + 查询串（和实际发出去的逐字节一致）。
    fn api(&self) -> String {
        if self.query.is_empty() {
            self.path.to_string()
        } else {
            format!("{}?{}", self.path, self.encoded_query())
        }
    }

    fn url(&self, base: &str) -> String {
        format!("{}{}", base, self.api())
    }
}

/// `XhsApi.NO_CACHE_PATHS`（`user/me` / `config` / `system/config`）：默认头里多两行 `no-cache`。
fn is_no_cache(path: &str) -> bool {
    matches!(
        path,
        "/api/sns/web/v2/user/me" | "/api/sns/web/v1/config" | "/api/sns/web/v1/system/config"
    )
}

/// 域匹配（Kotlin `seedQrJar`：`d == domain || d.endsWith(".$domain")`，两端都裁前导点）。
fn domain_matches(cookie_domain: &str, login_domain: &str) -> bool {
    let d = cookie_domain.trim_start_matches('.');
    let want = login_domain.trim_start_matches('.');
    d == want || d.ends_with(&format!(".{want}"))
}

/// `XhsQr.checkBody`：键序照网页（`qrId` 在前）。
fn check_body(qr_id: &str, code: &str) -> String {
    format!(
        "{{\"qrId\":{},\"code\":{}}}",
        codec::json_string(qr_id),
        codec::json_string(code)
    )
}

/// 二维码响应 `data`：`qr_id` / `code` / `url`（缺一个就没法轮询，直接报错；错误信息里不带响应内容）。
struct Generated {
    qr_id: String,
    code: String,
    url: String,
}

fn generated(root: &JsonMap) -> Result<Generated, BiliError> {
    let data = json::obj(root, "data").ok_or_else(|| BiliError::Parse {
        detail: "二维码接口的响应里没有 data".to_string(),
    })?;
    let need = |key: &str, what: &str| -> Result<String, BiliError> {
        match json::text(data, key) {
            Some(v) => Ok(v),
            None => Err(BiliError::Parse {
                detail: what.to_string(),
            }),
        }
    };
    Ok(Generated {
        qr_id: need("qr_id", "二维码响应缺少 qr_id")?,
        code: need("code", "二维码响应缺少 code")?,
        url: need("url", "二维码响应缺少 url")?,
    })
}

/// `data.codeStatus`；没有这个字段就是 `None`（调用方当失败处理）。
fn status_of(root: &JsonMap) -> Option<i32> {
    json::obj(root, "data").and_then(|d| json::int(d, "codeStatus"))
}

/// `qrcode/status` 的结果：`data.code_status` 是 2 才算成功；正式会话在 `data.login_info.session`。
struct LoginResult {
    confirmed: bool,
    session: Option<String>,
}

fn login_result(root: &JsonMap) -> LoginResult {
    let Some(data) = json::obj(root, "data") else {
        return LoginResult {
            confirmed: false,
            session: None,
        };
    };
    LoginResult {
        confirmed: json::long(data, "code_status") == Some(LOGIN_OK),
        session: json::obj(data, "login_info").and_then(|i| json::text(i, "session")),
    }
}

/// 签名 / b1 这类本地计算的失败：映射到 `BiliError::Host`（和平台对象那个出口一样）。
fn sign_failed(detail: String) -> BiliError {
    BiliError::Host { detail }
}

