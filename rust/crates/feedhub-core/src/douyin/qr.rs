//! 抖音的**网页扫码登录**：请求 / 签名 / 状态 / Cookie 落地全在这里。
//!
//! 逐条对照 `source-douyin/DouyinSource.kt` 的 `startQrLogin` / `pollQrLogin` / `passport` / `finishQrLogin`
//! 和 `DouyinQr.kt`。
//!
//! 1. `GET {passport}/passport/web/get_qrcode/`（passport SDK 那一套参数 + `p_no` + `account_sdk_source_info`，
//!    `a_bogus` 由 Rust 的签名器加）→ `data.token` / `data.qrcode_index_url` / `data.expire_time` / `data.is_frontier`；
//! 2. 轮询 `POST {passport}/passport/web/check_qrconnect/`（`token` 在**表单 body** 里，`fp` / `verifyFp` 在 query 里）：
//!    状态认字符串 `new/scanned/confirmed/expired/refreshed` 和数字 `1/2/3/5/4`；
//! 3. 确认后先 `GET redirect_url`（**只跟抖音自己的域名**）落地会话 Cookie，再用临时罐问一次 `profile/self`；
//! 4. 通过后清掉旧的登录 Cookie（[`QR_LOGIN_COOKIES`]），写入临时罐里的全部 Cookie
//!    （`ttwid` / `UIFID` / `s_v_web_id` 这些设备标识保留）。
//!
//! **不做**的（风控红线，Kotlin 那边也没做）：`__ac_signature`（要执行服务端下发的 acrawler 脚本）、
//! 登录域的 `ttwid/check`、`x-tt-session-dtrait`（跑 `dtrait-core` 的 JS）、短信二次验证（`error_code=2046`）。
//! 设备身份不新造：`account_sdk_source_info` 只写 `Device` 里真有出处的值。
//!
//! 时间 / 随机量（`now_ms`、`a_bogus` 的随机数、`biz_trace_id`）都由调用方注入，Rust 不读时钟、不取随机数。

use std::sync::Arc;

use crate::account::{QrLogin, QrSession, QrSessions, QrState};
use crate::douyin::sha256;
use crate::douyin::source::{
    form_encode, is_blank, pair, query_of, runtime_params, signals, Device,
};
use crate::douyin::sign;
use crate::host::{CookieHost, HttpRequest, HttpResponse, JarCookie, QrHost, StoreHost};
use crate::json::{self, JsonMap};
use crate::douyin::DouyinError;
use crate::model::UserRef;

/// 线上 passport 的根（Kotlin `DouyinSource.LOGIN`）。测试里换成 MockWebServer 的地址。
pub const LOGIN: &str = "https://login.douyin.com";

/// Cookie 罐里抖音的"身份 URL"（和 Kotlin 的 `WWW` 一样）。
const WWW: &str = "https://www.douyin.com/";

/// 登录 Cookie 的域（`LoginSpec.cookieDomain`，带前导点）。
pub const LOGIN_DOMAIN: &str = ".douyin.com";

/// 二维码生成（GET）。
pub const GET_QR_PATH: &str = "/passport/web/get_qrcode/";

/// 轮询（POST，`token` 在表单 body 里）。
pub const CHECK_QR_PATH: &str = "/passport/web/check_qrconnect/";

/// 扫码成功后落会话 Cookie 的落地页（`next` 参数）。
pub const NEXT: &str = "https://www.douyin.com";

/// `ctx.store` 里可以放一份新的 passport 版本号。
pub const PASSPORT_PROFILE_KEY: &str = "douyin.passport_profile";

/// `ctx.store` 里 `msToken` / `webid` 的键（和 Kotlin `storeOrCookie` 一样）。
const MS_TOKEN_KEY: &str = "douyin.ms_token";
const WEBID_KEY: &str = "douyin.webid";

/// 平台没给 `expire_time` 时的兜底：参考实现说二维码约 60 秒失效。
const QR_TTL_MS: i64 = 60_000;

/// 轮询间隔：浏览器实测 5.14 秒一发，取 5 秒（Kotlin `QR_POLL_INTERVAL_MS`）。
const QR_POLL_INTERVAL_MS: i64 = 5_000;

/// 给用户的提示（和 Kotlin 的 `hint` 逐字一样）。
pub const HINT: &str = "用抖音 App 扫码，并在手机上确认登录";

/// 判定登录态用的 Cookie。
const LOGIN_COOKIES: [&str; 3] = ["sessionid", "sessionid_ss", "sid_tt"];

/// 换账号时要清掉的登录 Cookie；设备标识（`ttwid` / `UIFID` / `s_v_web_id` / `msToken`）保留。
pub const QR_LOGIN_COOKIES: [&str; 8] = [
    "sessionid",
    "sessionid_ss",
    "sid_tt",
    "sid_guard",
    "uid_tt",
    "uid_tt_ss",
    "passport_auth_status",
    "passport_auth_status_ss",
];

const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded; charset=utf-8";

/// 域名是不是抖音的（Kotlin `isDouyinHost`）。
pub fn is_douyin_host(host: &str) -> bool {
    let h = host.to_lowercase();
    let h = h.split(':').next().unwrap_or("");
    h == "douyin.com" || h.ends_with(".douyin.com")
}

// ===========================================================================
// passport 的版本常量（Kotlin `DouyinQr.Profile`）
// ===========================================================================

/// passport SDK 那几个**随页面版本变**的常量。随包一份，`ctx.store` 的 [`PASSPORT_PROFILE_KEY`] 能覆盖。
///
/// MIT），不是我们自己抓的，
/// 也没在线验过；版本对不上时服务端回 `error_code=4031`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportProfile {
    pub aid: String,
    pub passport_jssdk_version: String,
    pub p_ui: String,
    pub p_ca: String,
    pub p_ca_real: String,
    pub p_zt: String,
    pub p_ver: String,
    pub p_bd: String,
}

impl Default for PassportProfile {
    fn default() -> Self {
        Self {
            aid: "6383".to_string(),
            passport_jssdk_version: "3.2.0".to_string(),
            p_ui: "2.2.0".to_string(),
            p_ca: "4.0.26".to_string(),
            p_ca_real: "1.0.0.843".to_string(),
            p_zt: "3.3.11".to_string(),
            p_ver: "1.1.3".to_string(),
            p_bd: "1.0.1.20".to_string(),
        }
    }
}

impl PassportProfile {
    /// `ctx.store` 的覆盖（Kotlin `Profile.parse`，`ignoreUnknownKeys`）：认得几个就覆盖几个。
    pub fn from_store(text: &str) -> Option<Self> {
        let root = json::parse_object(text).ok()?;
        // 键名照 Kotlin `@Serializable` 的默认 camelCase 名（`passportJssdkVersion` / `pUi` …）
        let mut profile = Self::default();
        if let Some(v) = json::text(&root, "aid") {
            profile.aid = v;
        }
        if let Some(v) = json::text(&root, "passportJssdkVersion") {
            profile.passport_jssdk_version = v;
        }
        if let Some(v) = json::text(&root, "pUi") {
            profile.p_ui = v;
        }
        if let Some(v) = json::text(&root, "pCa") {
            profile.p_ca = v;
        }
        if let Some(v) = json::text(&root, "pCaReal") {
            profile.p_ca_real = v;
        }
        if let Some(v) = json::text(&root, "pZt") {
            profile.p_zt = v;
        }
        if let Some(v) = json::text(&root, "pVer") {
            profile.p_ver = v;
        }
        if let Some(v) = json::text(&root, "pBd") {
            profile.p_bd = v;
        }
        Some(profile)
    }

    /// 生成二维码的 query（顺序照 `createQR`）。`p_ca` 第一次传 `0`（不带 device_fp）。
    pub fn get_qr_params(
        &self,
        now_ms: i64,
        trace_id: &str,
        source_info: &str,
        ms_token: Option<&str>,
    ) -> Vec<(String, String)> {
        let ts = now_ms.to_string();
        let mut params = vec![
            pair("passport_jssdk_version", self.passport_jssdk_version.clone()),
            pair("passport_jssdk_type", "normal"),
            pair("is_from_ttaccountsdk", "1"),
            pair("aid", self.aid.clone()),
            pair("language", "zh"),
            pair("account_app_language", "zh-CN"),
            pair("next", NEXT),
            pair("need_short_url", "true"),
            pair("need_logo", "false"),
            pair("is_new_login", "1"),
            pair("is_from_iesaccountsaas", "1"),
            pair("p_ui", self.p_ui.clone()),
            pair("account_sdk_source", "web"),
            pair("account_sdk_source_info", source_info.to_string()),
            pair("p_js_v", self.passport_jssdk_version.clone()),
            pair("p_js_t", "pro"),
            pair("p_zt", self.p_zt.clone()),
            pair("p_ver", self.p_ver.clone()),
            pair("p_ver_real", "0"),
            pair("request_host", "https%3A%2F%2Fwww.douyin.com"),
            pair("p_bd", self.p_bd.clone()),
            pair("p_ts", ts.clone()),
            pair("p_no", self.p_no(&ts, "0")),
            pair("biz_trace_id", trace_id.to_string()),
        ];
        if let Some(ms_token) = ms_token {
            params.push(pair("msToken", ms_token.to_string()));
        }
        params
    }

    /// 轮询的 query（顺序照 `buildCheckRequest`）：比生成多 `p_ca` / `p_ca_real` / `fp` / `verifyFp`，
    /// `p_no` 用真正的 `p_ca`。
    pub fn check_query(
        &self,
        now_ms: i64,
        trace_id: &str,
        source_info: &str,
        verify_fp: &str,
        ms_token: Option<&str>,
    ) -> Vec<(String, String)> {
        let ts = now_ms.to_string();
        let mut params = vec![
            pair("passport_jssdk_version", self.passport_jssdk_version.clone()),
            pair("passport_jssdk_type", "normal"),
            pair("is_from_ttaccountsdk", "1"),
            pair("aid", self.aid.clone()),
            pair("language", "zh"),
            pair("account_app_language", "zh-CN"),
            pair("is_from_iesaccountsaas", "1"),
            pair("p_ui", self.p_ui.clone()),
            pair("p_ca", self.p_ca.clone()),
            pair("p_ca_real", self.p_ca_real.clone()),
            pair("fp", verify_fp),
            pair("verifyFp", verify_fp),
            pair("account_sdk_source", "web"),
            pair("account_sdk_source_info", source_info.to_string()),
            pair("p_js_v", self.passport_jssdk_version.clone()),
            pair("p_js_t", "pro"),
            pair("p_zt", self.p_zt.clone()),
            pair("p_ver", self.p_ver.clone()),
            pair("p_ver_real", "0"),
            pair("request_host", "https%3A%2F%2Fwww.douyin.com"),
            pair("p_bd", self.p_bd.clone()),
            pair("p_ts", ts.clone()),
            pair("p_no", self.p_no(&ts, &self.p_ca)),
            pair("biz_trace_id", trace_id.to_string()),
            pair("is_new_login", "1"),
        ];
        if let Some(ms_token) = ms_token {
            params.push(pair("msToken", ms_token.to_string()));
        }
        params
    }

    /// 轮询的表单 body（`token` 在这里，不在 query 里）。
    pub fn check_form(&self, token: &str, frontier: bool) -> Vec<(String, String)> {
        let mut form = vec![
            pair("need_logo", "false"),
            pair("is_frontier", "true"),
            pair("token", token),
            pair("is_new_login", "1"),
            pair("next", NEXT),
            pair("need_short_url", "true"),
        ];
        if !frontier {
            // Kotlin：`if (!state.frontier) put("is_frontier", "false")` —— 覆盖掉前面的 true（顺序不变）
            for entry in form.iter_mut() {
                if entry.0 == "is_frontier" {
                    entry.1 = "false".to_string();
                }
            }
        }
        form
    }

    /// `p_no`：`p_*` 那几项按**键名排序**拼成 `k=v&…` 再 SHA-256（小写十六进制）。
    /// 排序后的顺序是 `p_bd, p_ca, p_ts, p_ver, p_zt, passport_jssdk_version`（`'_'` 比 `'a'` 小）。
    pub fn p_no(&self, ts: &str, p_ca: &str) -> String {
        let mut values = [
            ("p_bd", self.p_bd.as_str()),
            ("p_ca", p_ca),
            ("passport_jssdk_version", self.passport_jssdk_version.as_str()),
            ("p_ts", ts),
            ("p_ver", self.p_ver.as_str()),
            ("p_zt", self.p_zt.as_str()),
        ];
        values.sort_by(|a, b| a.0.cmp(b.0));
        let text = values
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        sha256::hex(&text)
    }
}

/// `account_sdk_source_info`：把一段描述浏览器的 JSON **逐字节 XOR 5 再转 hex**（`createQR` 里就是这么来的）。
///
/// 只放 [`Device`] 里**真有出处**的值（窗口 / 屏幕 / 核数 / 时间原点）；参考实现里的
/// `webgl.vendor` / `plugins: 5` / `navigationTiming` / `browser.t` 是它自己编的，这里不写。
pub fn source_info(device: &Device, window_width: i32, window_height: i32, now_ms: i64) -> String {
    let text = format!(
        "{{\"hardwareConcurrency\":{},\"webdriver\":false,\"chromedriver\":false,\"shelldriver\":false,\
         \"plugins\":0,\"innerHeight\":{},\"innerWidth\":{},\"outerHeight\":{},\"outerWidth\":{},\
         \"performance\":{{\"timeOrigin\":{}}}}}",
        device.cpu_core_num,
        window_height,
        window_width,
        window_height,
        window_width,
        now_ms - 500
    );
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.as_bytes() {
        out.push_str(&format!("{:02x}", byte ^ 5));
    }
    out
}

// ===========================================================================
// 响应解析（Kotlin `DouyinQr.generated` / `status` / `data`）
// ===========================================================================

/// `get_qrcode` 的 `data`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrGenerated {
    pub token: String,
    pub url: String,
    pub expire_at_sec: Option<i64>,
    pub frontier: bool,
}

pub fn generated(root: &JsonMap) -> Result<QrGenerated, DouyinError> {
    let data = data(root)?;
    let token = json::text(&data, "token")
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| DouyinError::Parse {
            detail: "二维码响应缺少 token".to_string(),
        })?;
    let url = json::text(&data, "qrcode_index_url")
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| DouyinError::Parse {
            detail: "二维码响应里只有图片、没有可编码的链接（qrcode_index_url），扫不了".to_string(),
        })?;
    Ok(QrGenerated {
        token,
        url,
        expire_at_sec: json::long(&data, "expire_time").filter(|v| *v > 0),
        frontier: json::bool(&data, "is_frontier").unwrap_or(true),
    })
}

/// 第 2 步的状态（两种写法都认）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Scanned,
    Confirmed(Option<String>),
    Expired,
    /// `4`：平台把二维码换了（带新 token），这张已经作废。
    Refreshed,
}

pub fn status(root: &JsonMap) -> Result<Status, DouyinError> {
    let data = data(root)?;
    // Kotlin 取的是 `JsonPrimitive.content`：字符串 / 数字都按原文比较
    let raw = data
        .get("status")
        .and_then(json::primitive_content)
        .ok_or_else(|| DouyinError::Parse {
            detail: "二维码状态响应里没有 status".to_string(),
        })?;
    Ok(match raw.as_str() {
        "new" | "1" => Status::Waiting,
        "scanned" | "2" => Status::Scanned,
        "confirmed" | "3" => Status::Confirmed(json::text(&data, "redirect_url")),
        "expired" | "5" => Status::Expired,
        "4" | "refreshed" => Status::Refreshed,
        other => {
            return Err(DouyinError::Parse {
                detail: format!("二维码状态是没见过的值（{other}）"),
            })
        }
    })
}

/// passport 的外壳：`{message:"success", data:{error_code:0, …}}` 与业务码映射。
pub fn data(root: &JsonMap) -> Result<JsonMap, DouyinError> {
    let data = json::obj(root, "data").ok_or_else(|| DouyinError::Parse {
        detail: "passport 响应里没有 data".to_string(),
    })?;
    let code = json::long(data, "error_code").unwrap_or(0);
    if code == 0 {
        return Ok(data.clone());
    }
    let description = json::text(data, "description")
        .map(|d| json::take_chars(&d, 120))
        .unwrap_or_default();
    Err(match code {
        7 => DouyinError::RiskControl {
            code: Some("7".to_string()),
            detail: "抖音说访问太频繁（轮询限频），按规矩停下，过几分钟再试".to_string(),
        },
        2046 => DouyinError::RiskControl {
            code: Some("2046".to_string()),
            detail: "抖音要求在手机上做短信二次验证（这里只做扫码），按规矩停下".to_string(),
        },
        8 => DouyinError::NotLoggedIn { detail: None },
        4031 => DouyinError::Api {
            code: "4031".to_string(),
            detail: format!(
                "passport 拒绝了这次扫码请求（参数 / SDK 版本对不上，或者要页面脚本的人机校验）{}",
                if description.is_empty() {
                    String::new()
                } else {
                    format!("：{description}")
                }
            ),
        },
        2156 => DouyinError::Api {
            code: "2156".to_string(),
            detail: format!("抖音说系统繁忙（会话缺少 verifycenter 的信任 Cookie）：{description}"),
        },
        other => DouyinError::Api {
            code: other.to_string(),
            detail: if description.is_empty() {
                format!("passport 业务码 {other}")
            } else {
                description
            },
        },
    })
}

/// 扫码登录的传输层风控识别（和 Kotlin `checkQrTransport` 一字不差，**错误信息里不贴 body**）。
pub fn check_qr_transport(response: &HttpResponse, expect_json: bool) -> Result<(), DouyinError> {
    if crate::hosted::has_header(&response.headers, "X-Vc-Bdturing-Parameters") {
        return Err(DouyinError::RiskControl {
            code: Some("bdturing".to_string()),
            detail: "服务端要求过人机验证".to_string(),
        });
    }
    if crate::hosted::has_header(&response.headers, "X-Tt-Verify-Passport-Decision") {
        return Err(DouyinError::RiskControl {
            code: Some("passport-decision".to_string()),
            detail: "服务端要求二次身份验证".to_string(),
        });
    }
    match response.status {
        412 | 429 => {
            return Err(DouyinError::RiskControl {
                code: Some(format!("HTTP {}", response.status)),
                detail: "被限速 / 风控拦了，停了".to_string(),
            })
        }
        403 => {
            return Err(DouyinError::RiskControl {
                code: Some("HTTP 403".to_string()),
                detail: "网关拒绝了扫码请求（多半要 Argus 签名 / 人机校验），停了".to_string(),
            })
        }
        200..=299 => {}
        code => {
            return Err(DouyinError::Api {
                code: format!("HTTP {code}"),
                detail: format!("扫码登录的 passport 接口回 HTTP {code}"),
            })
        }
    }
    // HTTP 200 + 空 body：签名 / 指纹 / WAF 拒绝
    if response.body.is_empty() {
        return Err(DouyinError::RiskControl {
            code: Some("empty-body".to_string()),
            detail: "服务端返回空 body（签名 / 指纹 / WAF 拒绝）".to_string(),
        });
    }
    let body = String::from_utf8_lossy(&response.body);
    if body.contains("__ac_nonce") || body.contains("$jsvmprt") {
        return Err(DouyinError::RiskControl {
            code: Some("acrawler".to_string()),
            detail: "拿到 acrawler 的 JS 挑战页（要执行页面脚本才过），按规矩停了".to_string(),
        });
    }
    // 只有 passport 的接口要求 JSON；落地页（`finish` 的 redirect_url）本来就是 HTML
    if expect_json && !body.trim_start().starts_with('{') {
        return Err(DouyinError::RiskControl {
            code: Some("challenge".to_string()),
            detail: "响应不是 JSON（多半是登录风控页），停了".to_string(),
        });
    }
    Ok(())
}

// ===========================================================================
// 平台对象
// ===========================================================================

/// 生成二维码时定下来、轮询要沿用的一小组值：`is_frontier`、`account_sdk_source_info`、`biz_trace_id`。
/// `account_sdk_source_info` 声称的是"同一台浏览器"，**不能每跳重算**。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PassportState {
    frontier: bool,
    source_info: String,
    trace_id: String,
}

/// 抖音扫码登录的平台对象。
pub struct DouyinQrSource {
    base: String,
    passport: String,
    device: Device,
    profile: sign::Profile,
    env: sign::Env,
    enter_page_ts: i64,
    /// 窗口尺寸（`DouyinDevice.windowWidth` / `windowHeight`）：`account_sdk_source_info` 声称的是**窗口**，
    /// 不是屏幕（Kotlin `DouyinQr.sourceInfo` 用的就是这两个）。
    window_width: i32,
    window_height: i32,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    qr: Arc<dyn QrHost>,
    sessions: QrSessions,
    states: std::sync::Mutex<Vec<(String, PassportState)>>,
}

impl DouyinQrSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base: impl Into<String>,
        passport: impl Into<String>,
        device: Device,
        profile: sign::Profile,
        env: sign::Env,
        enter_page_ts: i64,
        window_width: i32,
        window_height: i32,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
        qr: Arc<dyn QrHost>,
    ) -> Self {
        Self {
            base: base.into(),
            passport: passport.into(),
            device,
            profile,
            env,
            enter_page_ts,
            window_width,
            window_height,
            store,
            cookies,
            qr: qr.clone(),
            sessions: QrSessions::new(qr),
            states: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 第一步：要一张二维码。`trace_id` 由调用方生成（Kotlin `newTraceId()`：UUID 去横线取前 8 位）。
    pub fn start(
        &self,
        now_ms: i64,
        trace_id: &str,
        rand: &[f64],
    ) -> Result<QrLogin, DouyinError> {
        let session = self.qr.begin();
        // 临时罐只带**设备标识**（ttwid / UIFID / s_v_web_id / msToken 之类），不带旧的登录凭据
        let existing: Vec<JarCookie> = self
            .qr
            .all()
            .into_iter()
            .filter(|c| {
                is_douyin_host(&c.domain)
                    && !QR_LOGIN_COOKIES.contains(&c.name.as_str())
            })
            .collect();
        if !existing.is_empty() {
            self.qr.put(&session, WWW, existing);
        }
        let source_info = source_info(&self.device, self.window_width, self.window_height, now_ms);
        let profile = self.passport_profile();
        let params = profile.get_qr_params(
            now_ms,
            trace_id,
            &source_info,
            self.real_store_or(MS_TOKEN_KEY, "msToken").as_deref(),
        );
        let json = self.passport(&session, "GET", GET_QR_PATH, &params, &[], now_ms, rand)?;
        let generated = generated(&json)?;
        self.remember(
            &generated.token,
            PassportState {
                frontier: generated.frontier,
                source_info,
                trace_id: trace_id.to_string(),
            },
        );
        self.sessions.begin(&generated.token, &QrSession(session));
        Ok(QrLogin {
            platform: "douyin".to_string(),
            content: generated.url,
            token: generated.token,
            extra: Vec::new(),
            expires_at_ms: Some(
                generated
                    .expire_at_sec
                    .map(|s| s.saturating_mul(1000))
                    .unwrap_or_else(|| now_ms.saturating_add(QR_TTL_MS)),
            ),
            poll_interval_ms: QR_POLL_INTERVAL_MS,
            hint: HINT.to_string(),
        })
    }

    /// 第二步：问一次状态。
    pub fn poll(
        &self,
        login: &QrLogin,
        now_ms: i64,
        trace_id: &str,
        rand: &[f64],
    ) -> Result<QrState, DouyinError> {
        let session = self.sessions.jar(&login.token);
        let state = self
            .state(&login.token)
            .unwrap_or_else(|| PassportState {
                frontier: true,
                source_info: source_info(
                    &self.device,
                    self.window_width,
                    self.window_height,
                    now_ms,
                ),
                trace_id: trace_id.to_string(),
            });
        let profile = self.passport_profile();
        let query = profile.check_query(
            now_ms,
            &state.trace_id,
            &state.source_info,
            &self.real_verify_fp(),
            self.real_store_or(MS_TOKEN_KEY, "msToken").as_deref(),
        );
        let form = profile.check_form(&login.token, state.frontier);
        let json = match self.passport(&session.0, "POST", CHECK_QR_PATH, &query, &form, now_ms, rand)
        {
            Ok(json) => json,
            Err(e) => {
                self.end(&login.token);
                return Err(e);
            }
        };
        Ok(match status(&json)? {
            Status::Waiting => QrState::Waiting,
            Status::Scanned => QrState::Scanned,
            Status::Expired => {
                self.end(&login.token);
                QrState::Expired
            }
            Status::Refreshed => {
                self.end(&login.token);
                QrState::Failed(
                    "平台把二维码换了一张（这张已经作废），点「刷新二维码」重新来".to_string(),
                )
            }
            Status::Confirmed(redirect) => {
                let result = self.finish(&session.0, redirect.as_deref(), now_ms, rand);
                self.end(&login.token);
                result?
            }
        })
    }

    /// 手机上确认之后：先 GET 平台给的 `redirect_url` 落地页（会话 Cookie 是那一步种下来的），
    /// 再用临时罐问一次 `whoami` 确认；通过后清旧写新。
    fn finish(
        &self,
        session: &str,
        redirect_url: Option<&str>,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<QrState, DouyinError> {
        if let Some(url) = redirect_url.filter(|u| !u.trim().is_empty()) {
            if let Some(url) = parse_http_url(url) {
                if is_douyin_host(&url.0) {
                    // 落地页只是 HTML / 跳转，Cookie 在 Set-Cookie 里；不解析 body，只按传输层风控识别
                    let response = self.qr.execute(
                        session,
                        HttpRequest {
                            method: "GET".to_string(),
                            url: url.1.clone(),
                            headers: self.default_headers(None),
                            body: Vec::new(),
                        },
                        None,
                    )?;
                    check_qr_transport(&response, false)?;
                }
            }
        }
        let state = match self.verify(session, now_ms, rand) {
            Ok(Some(user)) => self.commit(session, Some(user)),
            Ok(None) | Err(DouyinError::NotLoggedIn { .. }) => QrState::Failed(
                "登录成功了但验证没通过（平台说没登录），现有登录态没有改动".to_string(),
            ),
            // 网络 / 风控等：没法验证，但平台已经明确说登录成功 → 照样写入，`user` 留空
            Err(_) => self.commit(session, None),
        };
        Ok(state)
    }

    fn commit(&self, session: &str, user: Option<UserRef>) -> QrState {
        let fresh = self.qr.cookies(session);
        self.qr.commit(
            WWW,
            LOGIN_DOMAIN,
            QR_LOGIN_COOKIES.iter().map(|n| n.to_string()).collect(),
            fresh,
        );
        QrState::Success(user)
    }

    /// 用临时罐问一次 `profile/self`（和 `DouyinHostedSource::whoami` 同一个请求 / 解析，
    /// 只是身份值都从临时罐读）。
    fn verify(
        &self,
        session: &str,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<Option<UserRef>, DouyinError> {
        let mut params = runtime_params(&self.device, crate::douyin::source::VERSION_CODE, crate::douyin::source::VERSION_NAME, "0");
        params.extend(self.identity_params(session));
        let raw_query = query_of(&params);
        let query = crate::hosted::okhttp_canonical_query(&raw_query);
        let url = format!("{}{}?{}", self.base.trim_end_matches('/'), "/aweme/v1/web/user/profile/self/", self.signed_query(&query, now_ms, rand)?);
        let response = self.qr.execute(
            session,
            HttpRequest {
                method: "GET".to_string(),
                url,
                headers: self.default_headers(self.jar_uifid(session).as_deref()),
                body: Vec::new(),
            },
            None,
        )?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        crate::douyin::source::DouyinHostedSource::parse_whoami(
            &body,
            response.status as i32,
            signals(&response),
        )
    }

    /// 身份值（和 `identity_params` 同一套顺序），但 `uifid` / `s_v_web_id` / `msToken` 都看**临时罐**
    /// （Kotlin 验证时用的是 `DouyinSource(ctx.withCookieJar(platform, jar))`，它的 `ctx.cookies` 就是临时罐）。
    fn identity_params(&self, session: &str) -> Vec<(String, String)> {
        let mut params = Vec::new();
        if let Some(v) = self.jar_store_or(session, WEBID_KEY, "webid") {
            params.push(pair("webid", v));
        }
        if let Some(v) = self.jar_uifid(session) {
            params.push(pair("uifid", v));
        }
        let fp = self.jar_verify_fp(session);
        params.push(pair("verifyFp", fp.clone()));
        params.push(pair("fp", fp));
        if let Some(v) = self.jar_store_or(session, MS_TOKEN_KEY, "msToken") {
            params.push(pair("msToken", v));
        }
        params
    }

    /// `ctx.cookies` 里的 `uifid`（passport 那几跳用；Kotlin `withDefaultHeaders()` 看的就是它）。
    fn real_uifid(&self) -> Option<String> {
        self.cookies
            .find(WWW, "UIFID")
            .filter(|v| !is_blank(v))
            .or_else(|| self.cookies.find(WWW, "UIFID_TEMP").filter(|v| !is_blank(v)))
    }

    /// `ctx.cookies` 里的 `s_v_web_id`，没有就用设备里存的那个（Kotlin `verifyFp()`）。
    fn real_verify_fp(&self) -> String {
        self.cookies
            .find(WWW, "s_v_web_id")
            .filter(|v| !is_blank(v))
            .unwrap_or_else(|| self.device.verify_fp.clone())
    }

    /// `ctx.store` 优先、再看 `ctx.cookies`（Kotlin `storeOrCookie`）。
    fn real_store_or(&self, store_key: &str, cookie_name: &str) -> Option<String> {
        self.store
            .get(store_key)
            .filter(|v| !is_blank(v))
            .or_else(|| {
                self.cookies
                    .find(WWW, cookie_name)
                    .filter(|v| !is_blank(v))
            })
    }

    fn jar_uifid(&self, session: &str) -> Option<String> {
        self.qr
            .find(session, WWW, "UIFID")
            .filter(|v| !is_blank(v))
            .or_else(|| {
                self.qr
                    .find(session, WWW, "UIFID_TEMP")
                    .filter(|v| !is_blank(v))
            })
    }

    fn jar_verify_fp(&self, session: &str) -> String {
        self.qr
            .find(session, WWW, "s_v_web_id")
            .filter(|v| !is_blank(v))
            .unwrap_or_else(|| self.device.verify_fp.clone())
    }

    fn jar_store_or(&self, session: &str, store_key: &str, cookie_name: &str) -> Option<String> {
        self.store
            .get(store_key)
            .filter(|v| !is_blank(v))
            .or_else(|| {
                self.qr
                    .find(session, WWW, cookie_name)
                    .filter(|v| !is_blank(v))
            })
    }

    /// passport 的一次请求：`GET` 参数在 query 里、`POST` 参数在表单 body 里，`a_bogus` 加在 query 最后。
    ///
    /// **已知缺口**（Kotlin 那边一样）：参考实现的 a_bogus 把 POST 的 body 一起签进去，
    /// 我们的签名器只签 query——没有独立测试向量，不猜着写。
    #[allow(clippy::too_many_arguments)]
    fn passport(
        &self,
        session: &str,
        method: &str,
        path: &str,
        params: &[(String, String)],
        form: &[(String, String)],
        now_ms: i64,
        rand: &[f64],
    ) -> Result<JsonMap, DouyinError> {
        let raw_query = query_of(params);
        let query = crate::hosted::okhttp_canonical_query(&raw_query);
        let signed = self.signed_query(&query, now_ms, rand)?;
        let url = if signed.is_empty() {
            format!("{}{path}", self.passport.trim_end_matches('/'))
        } else {
            format!("{}{path}?{signed}", self.passport.trim_end_matches('/'))
        };
        let (method, body, content_type) = if method == "POST" {
            let body = form
                .iter()
                .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            ("POST", body.into_bytes(), Some(FORM_CONTENT_TYPE))
        } else {
            ("GET", Vec::new(), None)
        };
        let _ = session;
        let mut headers = self.default_headers(self.real_uifid().as_deref());
        if let Some(content_type) = content_type {
            headers.push(pair("Content-Type", content_type));
        }
        let response = self.qr.execute(
            session,
            HttpRequest {
                method: method.to_string(),
                url,
                headers,
                body,
            },
            None,
        )?;
        check_qr_transport(&response, true)?;
        // 返回**整份**响应：`data()` 在 `generated` / `status` 里各剥一次外壳
        json::parse_object(&String::from_utf8_lossy(&response.body)).map_err(|detail| {
            DouyinError::Parse { detail }
        })
    }

    /// `a_bogus`（+ 受保护路径的 secsdk，passport / profile-self 都不在保护名单里）追加到 query 后面。
    fn signed_query(
        &self,
        query: &str,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<String, DouyinError> {
        let bogus = sign::sign(
            &self.profile,
            &self.env,
            query,
            now_ms,
            self.enter_page_ts,
            rand,
        )?;
        Ok(if query.is_empty() {
            format!("a_bogus={bogus}")
        } else {
            format!("{query}&a_bogus={bogus}")
        })
    }

    /// 请求头：**顺序照 Kotlin 最终落到线上的顺序**——`withDefaultHeaders()` 的 `uifid` 先放（如果有），
    /// 再按 `DefaultHeadersInterceptor` 的 map 顺序补剩下的（这里一次写全，宿主一个都不会再补）。
    fn default_headers(&self, uifid: Option<&str>) -> Vec<(String, String)> {
        let d = &self.device;
        let mut headers: Vec<(String, String)> = Vec::with_capacity(13);
        if let Some(uifid) = uifid {
            headers.push(pair("uifid", uifid.to_string()));
        }
        headers.push(pair("User-Agent", d.user_agent.clone()));
        headers.push(pair("Accept", "application/json,text/plain,*/*"));
        headers.push(pair(
            "Accept-Language",
            "zh-CN,zh;q=0.9,en;q=0.8,zh-TW;q=0.7,ja;q=0.6",
        ));
        headers.push(pair(
            "sec-ch-ua",
            format!(
                "\"Not=A?Brand\";v=\"99\", \"Google Chrome\";v=\"{}\", \"Chromium\";v=\"{}\"",
                d.browser_major, d.browser_major
            ),
        ));
        headers.push(pair("sec-ch-ua-mobile", "?0"));
        headers.push(pair("sec-ch-ua-platform", d.ua_platform.clone()));
        headers.push(pair("priority", "u=1, i"));
        headers.push(pair("sec-fetch-dest", "empty"));
        headers.push(pair("sec-fetch-mode", "cors"));
        headers.push(pair("sec-fetch-site", "same-origin"));
        headers.push(pair("Referer", "https://www.douyin.com/"));
        headers.push(pair("x-tt-argus", "1"));
        headers
    }

    fn passport_profile(&self) -> PassportProfile {
        self.store
            .get(PASSPORT_PROFILE_KEY)
            .and_then(|text| PassportProfile::from_store(&text))
            .unwrap_or_default()
    }

    fn state(&self, token: &str) -> Option<PassportState> {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(t, _)| t == token)
            .map(|(_, s)| s.clone())
    }

    fn remember(&self, token: &str, state: PassportState) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.retain(|(t, _)| t != token);
        states.push((token.to_string(), state));
    }

    fn end(&self, token: &str) {
        self.sessions.end(token);
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(t, _)| t != token);
    }

    /// 判定"是不是已经登录"用的 Cookie 名（给 FFI 的排障用；实际请求里由 `whoami` 判）。
    pub fn login_cookie_names(&self) -> Vec<String> {
        LOGIN_COOKIES.iter().map(|n| n.to_string()).collect()
    }
}

/// 一个 http(s) 地址 → (host, url)；认不出就给 `None`（Kotlin `toHttpUrl().getOrNull()` 的近似）。
fn parse_http_url(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.split('@').next_back().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() || host.contains(' ') {
        return None;
    }
    Some((host.to_string(), url.to_string()))
}

