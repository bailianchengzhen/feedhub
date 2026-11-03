//! 抖音的 UniFFI 导出：推荐流解析 + a_bogus / `x-secsdk-web-signature` 签名。
//!
//! 全是**纯函数**（网络 / Cookie / 代理 / 限速仍然在 Kotlin 的 `DouyinSource`）：
//! - 解析：`douyin_parse_feed(body, http_code, bdturing, passport_decision, refresh_index, emotes)`，
//!   HTTP 状态码和两条风控头的"有没有"一起传进来（错误映射在 Rust 里做，和 Kotlin `getJson` 的尾巴一一对应）；
//! - 签名：`douyin_abogus_sign(profile, env, query, now_ms, enter_page_ts, rand)` /
//!   `douyin_secsdk_sign(query, uifid_fallback, timestamp_sec)`——**时间和随机数都由 Kotlin 给**
//!   （`douyin_abogus_random_count` 是"这次要从随机流里取几个"的纯计算），`abogus_profile.json`
//!   的常量由 Kotlin 读好当参数传。
//!
//! 错误经 [`FfiError`] 带类型回 Kotlin，适配层再映射回 `NotLoggedInException` / `RiskControlException`
//! （风控码带过去）/ `SignatureException` / `ApiException` / `ParseException`。

use feedhub_core::douyin::{self, sign};

use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_item_node, to_ffi_page, FfiCommentPage, FfiDetail,
    FfiEmote, FfiError, FfiItemNode, FfiPage,
};

const PLATFORM: &str = "douyin";

pub(crate) fn map_error(e: douyin::DouyinError) -> FfiError {
    let platform = PLATFORM.to_string();
    match e {
        douyin::DouyinError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        douyin::DouyinError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        douyin::DouyinError::Signature { detail } => FfiError::Signature { platform, detail },
        douyin::DouyinError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        douyin::DouyinError::Parse { detail } => FfiError::Parse { platform, detail },
        // 下面这三个只会从平台对象（`douyin_source.rs`）的宿主回调回来；纯函数入口碰不到
        douyin::DouyinError::Network { detail } => FfiError::Network { platform, detail },
        douyin::DouyinError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        douyin::DouyinError::Host { detail } => FfiError::Internal { platform, detail },
        douyin::DouyinError::Internal { detail } => FfiError::Internal { platform, detail },
    }
}

fn to_pairs(emotes: Vec<FfiEmote>) -> Vec<(String, String)> {
    emotes.into_iter().map(|e| (e.code, e.url)).collect()
}

// ===========================================================================
// record
// ===========================================================================

/// a_bogus 的常量（Kotlin `AbogusProfile`，来自 `abogus_profile.json` / `ctx.store` 的覆盖）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinAbogusProfile {
    pub aid: i32,
    pub page_id: i32,
    pub sdk_version: String,
    pub sdk_minor: i32,
    pub u17: i32,
    /// Kotlin 侧固定 5 项（`u37[4]` 与 `u37[0..3]` 各有用处）。
    pub u37: Vec<i32>,
    pub counter: i32,
}

/// 签名时用的浏览器环境（Kotlin `AbogusEnv`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinAbogusEnv {
    pub user_agent: String,
    pub platform: String,
    pub inner_width: i32,
    pub inner_height: i32,
    pub outer_width: i32,
    pub outer_height: i32,
    pub avail_width: i32,
    pub avail_height: i32,
    pub screen_size_width: i32,
    pub screen_size_height: i32,
    /// 窗口指纹原串；`None` 时按上面那些字段拼（Kotlin `AbogusEnv.fingerprint`）。
    pub fingerprint_override: Option<String>,
}

/// a_bogus 算到 RC4 之前的所有中间字节。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinAbogusParts {
    pub u86: Vec<i32>,
    pub u88: Vec<i32>,
    pub u89: Vec<i32>,
    pub u90: Vec<i32>,
}

/// 一页推荐流 + 这一页所有卡片的 `desc` 拼串（调用方拿它判断要不要取表情表）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinFeed {
    pub page: FfiPage,
    pub texts: String,
}

/// 一次 `x-secsdk-web-signature` 的结果（Kotlin `SecSdkSigner.Signed`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinSecSdkSigned {
    pub signature: String,
    pub query: String,
    pub timestamp: i64,
}

pub(crate) fn profile_of(p: FfiDouyinAbogusProfile) -> sign::Profile {
    sign::Profile {
        aid: p.aid,
        page_id: p.page_id,
        sdk_version: p.sdk_version,
        sdk_minor: p.sdk_minor,
        u17: p.u17,
        u37: p.u37,
        counter: p.counter,
    }
}

pub(crate) fn env_of(e: FfiDouyinAbogusEnv) -> sign::Env {
    sign::Env {
        user_agent: e.user_agent,
        platform: e.platform,
        inner_width: e.inner_width,
        inner_height: e.inner_height,
        outer_width: e.outer_width,
        outer_height: e.outer_height,
        avail_width: e.avail_width,
        avail_height: e.avail_height,
        screen_size_width: e.screen_size_width,
        screen_size_height: e.screen_size_height,
        fingerprint_override: e.fingerprint_override,
    }
}

// ===========================================================================
// 解析
// ===========================================================================

/// 解析一页推荐流（`/aweme/v1/web/module/feed/` 的响应）。
///
/// `http_code` 是 HTTP 状态码；`bdturing` / `passport_decision` 是响应里
/// 有没有 `X-Vc-Bdturing-Parameters` / `X-Tt-Verify-Passport-Decision` 这两条风控头（值不传）。
/// `refresh_index` 是这一页的序号；`next` 永远是 `refresh_index + 1`（`has_more` 明确为 false 时没有下一页）。
#[uniffi::export]
pub fn douyin_parse_feed(
    body: String,
    http_code: i32,
    bdturing: bool,
    passport_decision: bool,
    refresh_index: i64,
    emotes: Vec<FfiEmote>,
) -> Result<FfiDouyinFeed, FfiError> {
    let signals = douyin::TransportSignals {
        bdturing,
        passport_decision,
    };
    let feed = douyin::parse_feed(&body, http_code, signals, refresh_index, &to_pairs(emotes))
        .map_err(map_error)?;
    Ok(FfiDouyinFeed {
        page: to_ffi_page(&feed.page),
        texts: feed.texts,
    })
}

/// 解析一份**不带外壳**的 aweme 对象（详情 fixture 的 `aweme_detail`；测试 / 边界样本用）。
#[uniffi::export]
pub fn douyin_parse_item(
    body: String,
    emotes: Vec<FfiEmote>,
) -> Result<FfiItemNode, FfiError> {
    let item = douyin::parse_item_json(&body, &to_pairs(emotes)).map_err(map_error)?;
    Ok(to_ffi_item_node(item.as_ref()))
}

/// 解析**详情**响应（`/aweme/v1/web/aweme/detail/`）。`emotes` 是表情表（文字代码 → 图片地址），
/// 由调用方从 Kotlin 的表情表里传进来——Rust 不复制表、也不发取表的请求。
#[uniffi::export]
pub fn douyin_parse_detail(
    body: String,
    id: String,
    emotes: Vec<FfiEmote>,
) -> Result<FfiDetail, FfiError> {
    douyin::detail::parse_body(&body, &id, &to_pairs(emotes))
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_error)
}

/// 解析**一页评论**（`comment/list` 与 `comment/list/reply` 同一个结构）。
#[uniffi::export]
pub fn douyin_parse_comments(
    body: String,
    emotes: Vec<FfiEmote>,
) -> Result<FfiCommentPage, FfiError> {
    douyin::comment::page_from_body(&body, &to_pairs(emotes))
        .map(|page| to_ffi_comment_page(&page))
        .map_err(map_error)
}

/// 一页评论里所有评论正文的拼串。
#[uniffi::export]
pub fn douyin_comment_texts(body: String) -> Result<String, FfiError> {
    let json = feedhub_core::json::parse_object(&body)
        .map_err(|detail| map_error(douyin::DouyinError::Parse { detail }))?;
    Ok(douyin::comment::texts(&json))
}

// ===========================================================================
// a_bogus
// ===========================================================================

/// 这一次签名要从随机数流里取几个 `nextDouble()`（Kotlin 那边照这个数先取好再传进来）。
///
/// 只是纯计算（指纹串 / 时间串的长度决定 `u88` 有几段），不取随机数。
#[uniffi::export]
pub fn douyin_abogus_random_count(env: FfiDouyinAbogusEnv, now_ms: i64) -> u32 {
    sign::random_count(&env_of(env), now_ms) as u32
}

/// 生成 a_bogus。`query` 是已经百分号编码、不含 `a_bogus` 自己的 query 串；
/// `rand` 必须是 Kotlin 的 `AbogusRandom` 按同样顺序取的前 `douyin_abogus_random_count` 个值。
#[uniffi::export]
pub fn douyin_abogus_sign(
    profile: FfiDouyinAbogusProfile,
    env: FfiDouyinAbogusEnv,
    query: String,
    now_ms: i64,
    enter_page_ts: i64,
    rand: Vec<f64>,
) -> Result<String, FfiError> {
    sign::sign(
        &profile_of(profile),
        &env_of(env),
        &query,
        now_ms,
        enter_page_ts,
        &rand,
    )
    .map_err(map_error)
}

/// a_bogus 的中间字节。
#[uniffi::export]
pub fn douyin_abogus_parts(
    profile: FfiDouyinAbogusProfile,
    env: FfiDouyinAbogusEnv,
    query: String,
    now_ms: i64,
    enter_page_ts: i64,
    rand: Vec<f64>,
) -> Result<FfiDouyinAbogusParts, FfiError> {
    let parts = sign::build_parts(
        &profile_of(profile),
        &env_of(env),
        &query,
        now_ms,
        enter_page_ts,
        &rand,
    )
    .map_err(map_error)?;
    Ok(FfiDouyinAbogusParts {
        u86: parts.u86,
        u88: parts.u88,
        u89: parts.u89,
        u90: parts.u90,
    })
}

// ===========================================================================
// x-secsdk-web-signature
// ===========================================================================

/// 算 `x-secsdk-web-signature`，返回签名值 + **实际应该发出去的** query。
#[uniffi::export]
pub fn douyin_secsdk_sign(
    query: String,
    uifid_fallback: Option<String>,
    timestamp_sec: i64,
) -> FfiDouyinSecSdkSigned {
    let signed = sign::secsdk_sign(&query, uifid_fallback.as_deref(), timestamp_sec);
    FfiDouyinSecSdkSigned {
        signature: signed.signature,
        query: signed.query,
        timestamp: signed.timestamp,
    }
}

/// 规范化 query（`SecSdkSigner.canonicalQuery`；测试 / 排障用）。
#[uniffi::export]
pub fn douyin_secsdk_canonical_query(query: String) -> String {
    sign::canonical_query(&query)
}

/// 这个路径要不要加 `x-secsdk-web-signature`。
#[uniffi::export]
pub fn douyin_secsdk_is_protected(path: String, method: String) -> bool {
    sign::secsdk_is_protected(&path, &method)
}

/// 等价于 JS 的 `encodeURIComponent`（`DouyinSource.formEncode` 用它拼 query，测试用来对拍）。
#[uniffi::export]
pub fn douyin_secsdk_encode(text: String) -> String {
    sign::encode_component(&text)
}

/// 等价于 JS 的 `decodeURIComponent`（外加 `+` → 空格）。
#[uniffi::export]
pub fn douyin_secsdk_decode(text: String) -> String {
    sign::decode_component(&text)
}

/// SM3 的小写十六进制（和 Kotlin `Sm3.hex` 对拍用）。
#[uniffi::export]
pub fn douyin_sm3_hex(text: String) -> String {
    douyin::sm3::hex(&douyin::sm3::digest(text.as_bytes()))
}
