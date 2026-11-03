//! **YouTube 的账号层**：设备码登录（两个端点候选）、换 token、`whoami`。
//!
//! 逐条对照 `source-youtube/YoutubeLogin.kt` + `YoutubeApi.accounts` / `checkPlayability` 那一套：
//!
//! - **TV 端点**（SmartTube 的路子）：`youtube.com/o/oauth2/…`，JSON body；`client_id` / `client_secret`
//!   **从 `https://www.youtube.com/tv` 页面引用的 base 脚本里抓**（`AppApi.getClientData` 的做法），
//!   scope 带 `youtube-paid-content`；
//! - **GOOGLE 端点**（Kodi 插件的路子）：`accounts.google.com/o/oauth2/device/code` +
//!   `googleapis.com/oauth2/v4/token`，form body；它的 client_id / secret 是插件自带的 key set，
//!   只能由用户在设置里给（`youtube.google_client_id` / `youtube.google_client_secret`），没有就抛说得清的错。
//!
//! ## token 不归这一层管
//!
//! 任务口径：**token 的存取留在 Kotlin**（`state.json`）。所以这里
//! - 收到的 token / refresh_token / 抓到的 client 凭据都是**参数**，不是这个对象的状态；
//! - 换到 token / 抓到凭据都**交回调用方**去存，这里不写 store、不落盘、不打日志、**绝不打印**；
//! - [`DeviceCodeLoginState`] 里也只有一个 `token` 字段，交给 Kotlin 存。
//!
//! 唯一从 store 读的是**设备 id**（`youtube.device_id`，服务端认"同一台设备"，不是凭据）——
//! 但连它也走参数（[`StartedDeviceCode::device_id`]），Kotlin 那边生成 / 存 / 复用，这一层只把它拼进 body。
//!
//! ## 轮询状态
//!
//! TV 端点等用户确认时回的是 **HTTP 200** + `{"error":"authorization_pending"}`，
//! Google 端点是 428 / 400 + 同样的 error —— 所以**先看 `error` 字段，再看状态码**（照 Kotlin 注释）。
//! `slow_down` 也当等待（Kotlin 就是这么映射的，间隔由宿主决定要不要调大）。

use crate::host::{HttpHost, HttpRequest};
use crate::hosted::{json_string, okhttp_form_encode};
use crate::json::{self, JsonMap};
use crate::model::UserRef;

use super::source::Client;
use super::{is_blank, trim_java, YoutubeError, YOUTUBE};

/// 设备码的两个端点（`YoutubeLogin.kt` 的 `DeviceCodeEndpoint`）。用 `ctx.store` 的
/// `youtube.device_code_endpoint` 记住上次用的那个——**那个键归 Kotlin**（它读出来传进来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Tv,
    Google,
}

impl Endpoint {
    pub fn key(self) -> &'static str {
        match self {
            Endpoint::Tv => "tv",
            Endpoint::Google => "google",
        }
    }

    pub fn from_key(key: &str) -> Option<Endpoint> {
        match key {
            "tv" => Some(Endpoint::Tv),
            "google" => Some(Endpoint::Google),
            _ => None,
        }
    }

    /// 默认端点（Kotlin `DeviceCodeEndpoint.DEFAULT` = TV）。
    pub fn default_endpoint() -> Endpoint {
        Endpoint::Tv
    }
}

/// 设备码要用到的四个地址（`LoginUrls`）。真实值照抄；测试换成 MockWebServer 的 http 地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginUrls {
    pub tv_device_code: String,
    pub tv_token: String,
    pub google_device_code: String,
    pub google_token: String,
}

impl Default for LoginUrls {
    fn default() -> Self {
        Self {
            tv_device_code: "https://www.youtube.com/o/oauth2/device/code".to_string(),
            tv_token: "https://www.youtube.com/o/oauth2/token".to_string(),
            google_device_code: "https://accounts.google.com/o/oauth2/device/code".to_string(),
            google_token: "https://www.googleapis.com/oauth2/v4/token".to_string(),
        }
    }
}

/// OAuth 的 client 凭据（TV 端点是抓来的，GOOGLE 端点是用户给的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub id: String,
    pub secret: String,
}

/// 取一次设备码的结果（`DeviceCodeLogin` + 这次顺手抓到的凭据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub poll_interval_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub hint: String,
    /// 这次从 `/tv` 页面抓到的 client 凭据（`None` = 用的是传进来的缓存，Kotlin 不用再存）。
    pub scraped: Option<Credentials>,
}

/// 换到的一组 token（**机密**：只交给 Kotlin 存，这一层不打印）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    /// 第一次登录才有 refresh_token；换 token 的响应里没有。
    pub refresh_token: Option<String>,
    pub expires_in_sec: Option<i64>,
}

/// 一次轮询的结果（Kotlin `DeviceCodeLoginState` 的前三种 + 失败原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Waiting,
    /// 平台说"慢一点"（RFC 8628 §3.5 `slow_down`）：还在等，但轮询间隔要加 5 秒。
    /// 会话层（`poll_device_code_session`）记下退避后交出去的是 [`PollOutcome::Waiting`]。
    SlowDown,
    Expired,
    Denied,
    Succeeded(TokenSet),
    Failed(String),
}

/// 一次换 token 的结果（Kotlin `YoutubeLogin.refresh` 的三种结局）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// 换到了：交给 Kotlin 存 access token + 过期时间。
    Refreshed(TokenSet),
    /// 服务端明确说凭据作废了（用户取消授权 / 改密码 / 太久没用 / 换了 client）：
    /// **Kotlin 那边要清掉本地 token**，当未登录处理。
    Revoked,
    /// 一时失败（5xx / 限流 / 代理回的错误页 / 200 却没有 access_token）：
    /// 登录态**保留**，抛出去让调用方决定（Kotlin 拼的是 `token-refresh-failed`）。
    Failed { status: u16 },
}

/// 只有服务端明确说凭据作废时才登出（`REVOKED_ERRORS`）。
const REVOKED_ERRORS: [&str; 3] = ["invalid_grant", "invalid_client", "unauthorized_client"];

/// 设备码 / token 端点用的 UA（`YoutubeClient.TV_TIZEN.user_agent`）。
const TV_UA: &str = super::source::TIZEN_USER_AGENT;

/// `/tv` 页面（抓 client 凭据的那一步）的地址：**永远打 `youtube.com`**，和 `base` 无关
/// （Kotlin 里这个地址是写死的，只有测试把它们换成 MockWebServer；`/tv` 这一跳没有注入点，
/// 所以离线测试必须先把凭据放进 `ctx.store`——和 Kotlin 的测试一样）。
const TV_PAGE: &str = "https://www.youtube.com/tv";

/// `application/json; charset=utf-8`（`YoutubeLogin.JSON`）。
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// 一次表单 POST / JSON POST / GET（**头顺序照 Kotlin 的 `Request.Builder` 调用顺序**）。
fn post(
    http: &dyn HttpHost,
    url: &str,
    headers: Vec<(String, String)>,
    body: Option<(String, Vec<u8>)>,
) -> Result<(u16, String), YoutubeError> {
    let mut all = headers;
    if let Some((content_type, _)) = &body {
        all.push(("Content-Type".to_string(), content_type.clone()));
    }
    let response = http
        .execute(HttpRequest {
            method: "POST".to_string(),
            url: url.to_string(),
            headers: all,
            body: body.map(|(_, bytes)| bytes).unwrap_or_default(),
        })
        .map_err(YoutubeError::from)?;
    Ok((
        response.status,
        String::from_utf8_lossy(&response.body).into_owned(),
    ))
}

fn get(http: &dyn HttpHost, url: &str) -> Result<(u16, String), YoutubeError> {
    let response = http
        .execute(HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: vec![("User-Agent".to_string(), TV_UA.to_string())],
            body: Vec::new(),
        })
        .map_err(YoutubeError::from)?;
    Ok((
        response.status,
        String::from_utf8_lossy(&response.body).into_owned(),
    ))
}

/// `checkTransport`（设备码端点）：只有 429 / 403 / 412 是风控信号，其余原样往下走（**不重试**）。
pub fn check_login_transport(status: u16) -> Result<(), YoutubeError> {
    match status {
        429 => Err(YoutubeError::RiskControl {
            code: "HTTP 429".to_string(),
            detail: "设备码端点被限速".to_string(),
        }),
        403 | 412 => Err(YoutubeError::RiskControl {
            code: format!("HTTP {status}"),
            detail: "设备码端点被拒".to_string(),
        }),
        _ => Ok(()),
    }
}

fn parse_json(body: &str) -> Result<JsonMap, YoutubeError> {
    json::parse_object(body).map_err(|_| YoutubeError::Api {
        code: "bad-json".to_string(),
        detail: "设备码端点返回的不是 JSON".to_string(),
    })
}

// ===========================================================================
// 请求体（字段顺序照 Kotlin 的字符串拼接）
// ===========================================================================

// TV 端点的 scope：设备码那一跳要 `youtube-paid-content`（照 SmartTube）。
const TV_SCOPE: &str =
    "http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content";
// GOOGLE 端点的 scope（标准 YouTube 读权限）
const GOOGLE_SCOPE: &str = "https://www.googleapis.com/auth/youtube";
// 设备码授权的 grant type（两个端点都一样）
const DEVICE_GRANT: &str = "http://oauth.net/grant_type/device/1.0";

/// `POST /o/oauth2/device/code`（TV）：`{client_id, device_id, model_name, scope}`。
fn tv_device_code_body(client_id: &str, device_id: &str) -> String {
    format!(
        "{{\"client_id\":{},\"device_id\":{},\"model_name\":\"ytlr::\",\"scope\":{}}}",
        json_string(client_id),
        json_string(device_id),
        json_string(TV_SCOPE)
    )
}

/// `POST /o/oauth2/token`（TV）轮询：`{code, client_id, client_secret, grant_type}`。
fn tv_poll_body(code: &str, creds: &Credentials) -> String {
    format!(
        "{{\"code\":{},\"client_id\":{},\"client_secret\":{},\"grant_type\":{}}}",
        json_string(code),
        json_string(&creds.id),
        json_string(&creds.secret),
        json_string(DEVICE_GRANT)
    )
}

/// `POST /o/oauth2/token`（TV）换 token：`{refresh_token, client_id, client_secret, grant_type}`。
fn tv_refresh_body(refresh: &str, creds: &Credentials) -> String {
    format!(
        "{{\"refresh_token\":{},\"client_id\":{},\"client_secret\":{},\"grant_type\":\"refresh_token\"}}",
        json_string(refresh),
        json_string(&creds.id),
        json_string(&creds.secret)
    )
}

/// `OkHttp FormBody.Builder` 的口径（空格 `%20`，和 `URLEncoder` 的 `+` 不同）。
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", okhttp_form_encode(k), okhttp_form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

// ===========================================================================
// 平台对象上的三个方法
// ===========================================================================

impl super::source::YoutubeHostedSource {
    /// 取一组设备码（`YoutubeLogin.start`）。
    ///
    /// [`cached`] 是 Kotlin 从 store 里读出来的 client 凭据（TV 端点才用；`None` = 去 `/tv` 页抓一次，
    /// 抓到的会放在返回值里让 Kotlin 存）。[`device_id`] 由 Kotlin 生成 / 存 / 复用（稳定设备 id，不是凭据）。
    pub fn start_device_code(
        &self,
        endpoint: Endpoint,
        device_id: &str,
        cached: Option<Credentials>,
        now_ms: i64,
    ) -> Result<StartedDeviceCode, YoutubeError> {
        let (creds, scraped) = self.credentials(endpoint, cached)?;
        let (status, body) = match endpoint {
            Endpoint::Tv => post(
                self.http_port().as_ref(),
                &self.login_urls().tv_device_code,
                vec![("User-Agent".to_string(), TV_UA.to_string())],
                Some((
                    JSON_CONTENT_TYPE.to_string(),
                    tv_device_code_body(&creds.id, device_id).into_bytes(),
                )),
            )?,
            Endpoint::Google => post(
                self.http_port().as_ref(),
                &self.login_urls().google_device_code,
                Vec::new(),
                Some((
                    "application/x-www-form-urlencoded".to_string(),
                    form_body(&[
                        ("client_id", creds.id.as_str()),
                        ("scope", GOOGLE_SCOPE),
                    ])
                    .into_bytes(),
                )),
            )?,
        };
        check_login_transport(status)?;
        let json = parse_json(&body)?;
        let device_code = json::text(&json, "device_code").ok_or_else(|| YoutubeError::Api {
            code: "device_code".to_string(),
            detail: "设备码端点没有返回 device_code".to_string(),
        })?;
        let user_code = json::text(&json, "user_code").ok_or_else(|| YoutubeError::Api {
            code: "user_code".to_string(),
            detail: "设备码端点没有返回 user_code".to_string(),
        })?;
        let url = json::text(&json, "verification_url")
            .or_else(|| json::text(&json, "verification_uri"))
            .ok_or_else(|| YoutubeError::Api {
                code: "verification_url".to_string(),
                detail: "设备码端点没有返回 verification_url".to_string(),
            })?;
        let interval = json::long(&json, "interval").unwrap_or(5) * 1000;
        let expires_in = json::long(&json, "expires_in");
        Ok(StartedDeviceCode {
            device_code,
            user_code,
            verification_url: url.clone(),
            poll_interval_ms: interval,
            expires_at_ms: expires_in.map(|sec| now_ms + sec * 1000),
            hint: format!("在 {url} 输入短码并确认（不要把这组码发出去）"),
            scraped,
        })
    }

    /// 轮询一次（`YoutubeLogin.poll`）。`authorization_pending` / `slow_down` → 等待；
    /// 换到 token 就交回（**存是 Kotlin 的事**）。
    pub fn poll_device_code(
        &self,
        endpoint: Endpoint,
        device_code: &str,
        cached: Option<Credentials>,
    ) -> Result<(PollOutcome, Option<Credentials>), YoutubeError> {
        let (creds, scraped) = self.credentials(endpoint, cached)?;
        let (status, body) = match endpoint {
            Endpoint::Tv => post(
                self.http_port().as_ref(),
                &self.login_urls().tv_token,
                vec![("User-Agent".to_string(), TV_UA.to_string())],
                Some((
                    JSON_CONTENT_TYPE.to_string(),
                    tv_poll_body(device_code, &creds).into_bytes(),
                )),
            )?,
            Endpoint::Google => post(
                self.http_port().as_ref(),
                &self.login_urls().google_token,
                Vec::new(),
                Some((
                    "application/x-www-form-urlencoded".to_string(),
                    form_body(&[
                        ("client_id", creds.id.as_str()),
                        ("client_secret", creds.secret.as_str()),
                        ("code", device_code),
                        ("grant_type", DEVICE_GRANT),
                    ])
                    .into_bytes(),
                )),
            )?,
        };
        // TV 端点等确认时是 **HTTP 200 + error**，所以先看 error 再看状态码
        let json = match parse_json(&body) {
            Ok(json) => json,
            // Kotlin 在 poll 里直接 `parse(res.body)`（不是 runCatching），坏 JSON 会抛 —— 这里照办
            Err(e) => return Err(e),
        };
        let error = json::text(&json, "error");
        if error.is_some() || !(200..=299).contains(&status) {
            let outcome = match error.as_deref() {
                Some("authorization_pending") => PollOutcome::Waiting,
                Some("slow_down") => PollOutcome::SlowDown,
                Some("expired_token") => PollOutcome::Expired,
                Some("access_denied") => PollOutcome::Denied,
                other => PollOutcome::Failed(
                    json::text(&json, "error_description")
                        .or_else(|| other.map(str::to_string))
                        .unwrap_or_else(|| format!("HTTP {status}")),
                ),
            };
            return Ok((outcome, scraped));
        }
        let access = json::text(&json, "access_token").ok_or_else(|| YoutubeError::Api {
            code: "access_token".to_string(),
            detail: "token 端点没有返回 access_token".to_string(),
        });
        match access {
            Ok(access) => Ok((
                PollOutcome::Succeeded(TokenSet {
                    access_token: access,
                    refresh_token: json::text(&json, "refresh_token"),
                    expires_in_sec: json::long(&json, "expires_in"),
                }),
                scraped,
            )),
            // Kotlin 是 `?: return Failed("token 端点没有返回 access_token")`——是**失败状态**，不是异常
            Err(e) => {
                let message = match e {
                    YoutubeError::Api { detail, .. } => detail,
                    other => other.to_string(),
                };
                Ok((PollOutcome::Failed(message), scraped))
            }
        }
    }

    /// 用 refresh_token 换新 token（`YoutubeLogin.refresh`）。
    ///
    /// 只有服务端**明确说凭据作废**（[`REVOKED_ERRORS`]）才回 [`RefreshOutcome::Revoked`]；
    /// 其余失败回 [`RefreshOutcome::Failed`]（登录态保留）。坏 JSON 当"没有 error 字段"处理，
    /// 所以 200 + 没有 access_token 也是 `Failed`。
    pub fn refresh_session(
        &self,
        endpoint: Endpoint,
        refresh_token: &str,
        cached: Option<Credentials>,
    ) -> Result<(RefreshOutcome, Option<Credentials>), YoutubeError> {
        let (creds, scraped) = self.credentials(endpoint, cached)?;
        let (status, body) = match endpoint {
            Endpoint::Tv => post(
                self.http_port().as_ref(),
                &self.login_urls().tv_token,
                vec![("User-Agent".to_string(), TV_UA.to_string())],
                Some((
                    JSON_CONTENT_TYPE.to_string(),
                    tv_refresh_body(refresh_token, &creds).into_bytes(),
                )),
            )?,
            Endpoint::Google => post(
                self.http_port().as_ref(),
                &self.login_urls().google_token,
                Vec::new(),
                Some((
                    "application/x-www-form-urlencoded".to_string(),
                    form_body(&[
                        ("client_id", creds.id.as_str()),
                        ("client_secret", creds.secret.as_str()),
                        ("refresh_token", refresh_token),
                        ("grant_type", "refresh_token"),
                    ])
                    .into_bytes(),
                )),
            )?,
        };
        let json = json::parse_object(&body).ok();
        let access = json.as_ref().and_then(|m| json::text(m, "access_token"));
        if (200..=299).contains(&status) {
            if let Some(access) = access {
                return Ok((
                    RefreshOutcome::Refreshed(TokenSet {
                        access_token: access,
                        refresh_token: None,
                        expires_in_sec: json.as_ref().and_then(|m| json::long(m, "expires_in")),
                    }),
                    scraped,
                ));
            }
        }
        let error = json.as_ref().and_then(|m| json::text(m, "error"));
        if let Some(error) = error {
            if REVOKED_ERRORS.contains(&error.as_str()) {
                return Ok((RefreshOutcome::Revoked, scraped));
            }
        }
        Ok((RefreshOutcome::Failed { status }, scraped))
    }

    /// **`whoami`**（`YoutubeSource.whoami`）：`POST /youtubei/v1/account/accounts_list`（TV_TIZEN，
    /// 带 bearer）→ 第一个 `accountItem` 的频道名 / handle / 头像。
    ///
    /// **没 token 就不发请求**、直接 `None`（"没登录"和"登录了但服务端没给账号"都是 `None`，
    /// 不编一个账号）；风控 / 状态码照 [`check_transport`]，不重试。
    pub fn whoami(&self, token: Option<&str>) -> Result<Option<UserRef>, YoutubeError> {
        let token = token.filter(|t| !is_blank(t));
        let Some(token) = token else {
            return Ok(None);
        };
        let visitor = self.visitor_public();
        let mut body = String::with_capacity(512);
        super::source::push_context_for(&mut body, Client::TvTizen, visitor.as_deref());
        body.push('}');
        let json = self.post_public(super::source::PATH_ACCOUNTS, Client::TvTizen, &body, Some(token))?;
        let Some(contents) = json.get("contents").map(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(account) = find_first_account(&contents) else {
            return Ok(None);
        };
        let name = text_item(account.get("accountName"))
            .or_else(|| text_item(account.get("channelHandle")));
        let Some(name) = name else {
            return Ok(None);
        };
        let handle = text_item(account.get("channelHandle"));
        let avatar = account
            .get("accountPhoto")
            .and_then(|v| v.as_object())
            .and_then(|photo| json::arr(photo, "thumbnails"))
            .and_then(|list| list.last())
            .and_then(|t| t.as_object())
            .and_then(|t| json::text(t, "url"));
        Ok(Some(UserRef {
            platform: YOUTUBE.to_string(),
            // id 要稳定：优先频道 handle（@xxx），没有就用频道名
            id: handle.clone().unwrap_or_else(|| name.clone()),
            name,
            avatar,
            url: None,
        }))
    }

    // ---------- 内部 ----------

    /// 端点凭据：TV 端点先看传进来的缓存，没有再抓 `/tv` 页面；GOOGLE 端点只认传进来的
    /// （缺了就是 `no-client-id`，和 Kotlin 一字不差）。
    fn credentials(
        &self,
        endpoint: Endpoint,
        cached: Option<Credentials>,
    ) -> Result<(Credentials, Option<Credentials>), YoutubeError> {
        match endpoint {
            Endpoint::Google => {
                let creds = cached.filter(|c| !is_blank(&c.id)).ok_or_else(|| YoutubeError::Api {
                    code: "no-client-id".to_string(),
                    detail: "GOOGLE 端点的 client_id / client_secret 要在 ctx.store 里给（youtube.google_client_id / \
                             youtube.google_client_secret）；舞台上的默认是 TV 端点"
                        .to_string(),
                })?;
                Ok((creds, None))
            }
            Endpoint::Tv => match cached {
                Some(creds) if !is_blank(&creds.id) && !is_blank(&creds.secret) => Ok((creds, None)),
                _ => {
                    let scraped = self.scrape_tv_credentials()?;
                    Ok((scraped.clone(), Some(scraped)))
                }
            },
        }
    }

    /// 照 MediaServiceCore 的做法：GET `/tv`（带 TV UA）→ 正则取 base 脚本地址和 visitorData →
    /// GET 脚本 → 正则取 `clientId:"<...>.apps.googleusercontent.com"` 和它后面的 secret。
    ///
    /// 抓到的 visitorData 写回 `ctx.store`（和 Kotlin 同一个键，共用的那份匿名身份）。
    pub(crate) fn scrape_tv_credentials(&self) -> Result<Credentials, YoutubeError> {
        let (status, page) = get(self.http_port().as_ref(), TV_PAGE)?;
        if !(200..=299).contains(&status) {
            return Err(YoutubeError::Api {
                code: format!("HTTP {status}"),
                detail: "/tv 页面取不到".to_string(),
            });
        }
        if let Some(visitor) = extract_visitor_data(&page) {
            if !is_blank(&visitor) {
                self.store_put_visitor(&visitor);
            }
        }
        let scripts = script_sources(&page);
        let base_url = scripts
            .iter()
            .find(|src| src.contains("base"))
            .or_else(|| scripts.first())
            .ok_or_else(|| YoutubeError::Api {
                code: "no-base-js".to_string(),
                detail: "/tv 页面里没有脚本地址，TV 端点可能变了".to_string(),
            })?;
        let url = if base_url.starts_with("http") {
            base_url.clone()
        } else {
            format!("https://www.youtube.com{base_url}")
        };
        let (status, js) = get(self.http_port().as_ref(), &url)?;
        if !(200..=299).contains(&status) {
            return Err(YoutubeError::Api {
                code: format!("HTTP {status}"),
                detail: "TV base 脚本取不到".to_string(),
            });
        }
        let id = find_client_id(&js).ok_or_else(|| YoutubeError::Api {
            code: "no-client-id".to_string(),
            detail: "TV base 脚本里找不到 client_id（端点可能变了）".to_string(),
        })?;
        let secret = find_client_secret(&js).unwrap_or_default();
        Ok(Credentials { id, secret })
    }
}

// ===========================================================================
// 解析（都在 `contains` / 手写扫描器上做，不引 regex crate）
// ===========================================================================

/// `JsonElement?.obj("accountItem") ?: obj("accountItemRenderer") ?: values.firstNotNullOfOrNull(…)`
/// （Kotlin `findFirstAccount`：对象先看自己这两个键，再看子值；数组逐个下钻）。
fn find_first_account(value: &serde_json::Value) -> Option<&JsonMap> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = json::obj(map, "accountItem") {
                return Some(found);
            }
            if let Some(found) = json::obj(map, "accountItemRenderer") {
                return Some(found);
            }
            map.values().find_map(find_first_account)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_first_account),
        _ => None,
    }
}

/// InnerTube 的 TextItem：`{simpleText}` / `{runs:[{text}]}` / `{text}`（Kotlin `textItem`）。
fn text_item(value: Option<&serde_json::Value>) -> Option<String> {
    let map = value?.as_object()?;
    if let Some(text) = json::text(map, "simpleText") {
        return Some(text);
    }
    if let Some(runs) = json::arr(map, "runs") {
        let joined: String = runs
            .iter()
            .map(|run| {
                run.as_object()
                    .and_then(|r| json::string(r, "text"))
                    .unwrap_or_default()
            })
            .collect();
        if !is_blank(&joined) {
            return Some(joined);
        }
    }
    if let Some(text) = json::text(map, "text") {
        return Some(text);
    }
    None
}

/// `<script[^>]*\ssrc="([^"]+)"` 的全部捕获（顺序 = 文档顺序）。
fn script_sources(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<script") {
        let after = &rest[at + "<script".len()..];
        // `[^>]*` 到标签结束
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        if let Some(src) = attr_value(tag, "src") {
            out.push(src);
        }
        rest = &after[tag_end + 1..];
    }
    out
}

/// 标签里 `name="…"` 的值（`\ssrc=` 的空格在前，`[^>]*` 已由调用方切掉）。
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut search = tag;
    let mut offset = 0usize;
    while let Some(at) = search.find(&needle) {
        let absolute = offset + at;
        // 前面必须是空白（`\ssrc="…"`）
        let ok = tag[..absolute]
            .chars()
            .next_back()
            .map(|c| c.is_whitespace())
            .unwrap_or(false);
        let start = absolute + needle.len();
        if ok {
            let tail = &tag[start..];
            if let Some(end) = tail.find('"') {
                return Some(tail[..end].to_string());
            }
            return None;
        }
        offset = start;
        search = &tag[start..];
    }
    None
}

/// `clientId:"([-\w]+\.apps\.googleusercontent\.com)"`。
fn find_client_id(js: &str) -> Option<String> {
    const NEEDLE: &str = "clientId:\"";
    const SUFFIX: &str = ".apps.googleusercontent.com";
    let mut rest = js;
    while let Some(at) = rest.find(NEEDLE) {
        let start = at + NEEDLE.len();
        let tail = &rest[start..];
        if let Some(quote) = tail.find('"') {
            let candidate = &tail[..quote];
            // 正则要求 `[-\w]+` + `.apps.googleusercontent.com`，也就是**整个**值就是这个形状
            if candidate.ends_with(SUFFIX) {
                let local = &candidate[..candidate.len() - SUFFIX.len()];
                if !local.is_empty() && local.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                    return Some(candidate.to_string());
                }
            }
        }
        rest = &rest[start..];
    }
    None
}

/// `clientId:"[-‑\w]+\.apps\.googleusercontent\.com",\s*[\w$]+:"(\w+)"`。
fn find_client_secret(js: &str) -> Option<String> {
    const NEEDLE: &str = "clientId:\"";
    let mut rest = js;
    while let Some(at) = rest.find(NEEDLE) {
        let start = at + NEEDLE.len();
        let tail = &rest[start..];
        if let Some(quote) = tail.find('"') {
            let id = &tail[..quote];
            if id.ends_with(".apps.googleusercontent.com") {
                let after = &tail[quote + 1..];
                // `,\s*`（Java 的 `\s`：6 个 ASCII 空白）
                if let Some(rest2) = after.strip_prefix(',') {
                    let mut i = 0usize;
                    while rest2[i..]
                        .chars()
                        .next()
                        .is_some_and(super::is_ascii_regex_ws)
                    {
                        i += rest2[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                    }
                    let rest3 = &rest2[i..];
                    // `[\w$]+` → `:"(\w+)"`
                    let name_len = rest3
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                        .map(char::len_utf8)
                        .sum::<usize>();
                    if name_len > 0 {
                        let rest4 = &rest3[name_len..];
                        if let Some(rest5) = rest4.strip_prefix(":\"") {
                            let value: String = rest5
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            if !value.is_empty() {
                                return Some(value);
                            }
                        }
                    }
                }
            }
        }
        rest = &rest[start..];
    }
    None
}

/// `/tv` 页面里的 `"visitorData":"…"`（`\u003d` 还原成 `=`）。
fn extract_visitor_data(html: &str) -> Option<String> {
    const NEEDLE: &str = "\"visitorData\":\"";
    let at = html.find(NEEDLE)? + NEEDLE.len();
    let tail = &html[at..];
    let end = tail.find('"')?;
    Some(tail[..end].replace("\\u003d", "="))
}

/// `trim` 在设备 id / token 上的口径（Kotlin `isBlank` / `trim` 用 `Character.isWhitespace`）。
pub fn is_blank_token(value: &str) -> bool {
    trim_java(value).is_empty()
}

