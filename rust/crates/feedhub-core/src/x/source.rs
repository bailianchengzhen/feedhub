//! X 平台层：**推荐流（`HomeTimeline`）/ 关注流（`HomeLatestTimeline`）/ 详情（`TweetDetail`）/
//! 评论 / 播放 / raw / whoami 的完整请求编排与解析**，收发一律通过宿主端口（[`HttpHost`]`）。
//!
//! 逐条对照 `source-x/XSource.kt`：
//! - `graphqlUrl` / `timelineVariables` / `detailVariables` / `flags_json`：方法 + URL + 参数顺序 + 请求头顺序
//!   必须和 Kotlin 版**逐字节一样**，`XRequestEquivalenceTest` / `XDetailRequestEquivalenceTest` 守着；
//! - 登录态先判：`feed`、`detail`、`comments`、`play` 必须在任何网络请求之前先判 `logged_in()`；
//! - txid 按需初始化一次（`ensure_txid()`），失败记录原因不阻断；
//! - 限速接线：每次 GraphQL 请求前调 `rate_limiter.check()`，收到响应后调 `rate_limiter.record()`；
//! - 404 / 422 自动扫网页脚本换 queryId 并重试一次；
//! - `raw(method, url, headers, body)` 先 `ensure_txid()`，相对路径拼 base，记录响应头。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::detail::{CommentPage, CommentSort, FeedDetail};
use crate::host::{CookieHost, HostError, HttpHost, HttpRequest, HttpResponse};
use crate::json::{self, JsonMap};
use crate::model::{Page, UserRef};
use crate::play::PlayInfo;
use crate::x::profile::{XClientProfile, XQueryIdResolver};
use crate::x::rate::{XPageBudget, XRateLimiter};
use crate::x::sign_chain::{XSignChain, XSignChainOutcome};
use crate::x::txid::{XTransaction, ADDITIONAL_RANDOM_NUMBER, DEFAULT_KEYWORD};
use crate::x::{self, FeedKind, XError, XOperation};

/// 接口根（和 `XSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://x.com";

/// 取 Cookie 用的"身份 URL"（和 Kotlin `XSource.COOKIE_URL` 一样）。
pub const COOKIE_URL: &str = "https://x.com/";

/// 网页版内置的**公开** Bearer token。
pub const BEARER_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

/// 一页条数（Kotlin `XSource.PAGE_SIZE`）。
pub const PAGE_SIZE: i64 = 20;

/// 浏览器 UA（和 `feedhub.core.UserAgents.DESKTOP_CHROME` 逐字一样）。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 一次刷新最多翻几页（Kotlin `XRatePolicy.MAX_PAGES_PER_REFRESH`）。
pub const MAX_PAGES_PER_REFRESH: u32 = 2;

/// 一页 + "queryId 可能过期了"的信号（保留给 FFI 兼容）。
#[derive(Debug, Clone, PartialEq)]
pub enum XFeedOutcome {
    Page(Page),
    StaleQueryId,
}

/// X 的平台对象。
pub struct XHostedSource {
    base: String,
    page_origin: String,
    http: Arc<dyn HttpHost>,
    page_http: Arc<dyn HttpHost>,
    cookies: Arc<dyn CookieHost>,
    query_ids: XQueryIdResolver,
    sign_chain: XSignChain,
    features: Vec<(String, bool)>,
    field_toggles: Vec<(String, bool)>,
    detail_field_toggles: Vec<(String, bool)>,
    page_budget: XPageBudget,
    rate_limiter: Arc<XRateLimiter>,
    txid_enabled: bool,
    auto_scan_query_id: bool,
    txid_generator: Mutex<Option<Arc<XTransaction>>>,
    last_txid_error: Mutex<Option<String>>,
    txid_tried: AtomicBool,
}

impl XHostedSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base: impl Into<String>,
        page_origin: impl Into<String>,
        http: Arc<dyn HttpHost>,
        page_http: Arc<dyn HttpHost>,
        cookies: Arc<dyn CookieHost>,
        query_ids: Vec<(String, String)>,
        features: Vec<(String, bool)>,
        field_toggles: Vec<(String, bool)>,
        detail_field_toggles: Vec<(String, bool)>,
    ) -> Self {
        let base_str = base.into();
        let page_origin_str = page_origin.into();

        let mut profile = XClientProfile::default();
        if !query_ids.is_empty() {
            profile.query_ids = query_ids.into_iter().collect();
        }
        if !features.is_empty() {
            profile.features = features.clone();
        }
        if !field_toggles.is_empty() {
            profile.field_toggles = field_toggles.clone();
        }
        if !detail_field_toggles.is_empty() {
            profile.tweet_detail_field_toggles = detail_field_toggles.clone();
        }

        let actual_features = if features.is_empty() {
            profile.features.clone()
        } else {
            features
        };
        let actual_field_toggles = if field_toggles.is_empty() {
            profile.field_toggles.clone()
        } else {
            field_toggles
        };
        let actual_detail_field_toggles = if detail_field_toggles.is_empty() {
            profile.tweet_detail_field_toggles.clone()
        } else {
            detail_field_toggles
        };

        let resolver = XQueryIdResolver::new(profile, page_origin_str.clone());

        Self {
            base: base_str,
            page_origin: page_origin_str,
            http,
            page_http,
            cookies,
            query_ids: resolver,
            sign_chain: XSignChain::default(),
            features: actual_features,
            field_toggles: actual_field_toggles,
            detail_field_toggles: actual_detail_field_toggles,
            page_budget: XPageBudget::new(MAX_PAGES_PER_REFRESH),
            rate_limiter: Arc::new(XRateLimiter::new()),
            txid_enabled: false,
            auto_scan_query_id: false,
            txid_generator: Mutex::new(None),
            last_txid_error: Mutex::new(None),
            txid_tried: AtomicBool::new(false),
        }
    }

    pub fn with_txid_enabled(mut self, enabled: bool) -> Self {
        self.txid_enabled = enabled;
        self
    }

    pub fn with_auto_scan_query_id(mut self, enabled: bool) -> Self {
        self.auto_scan_query_id = enabled;
        self
    }

    pub fn with_profile_json(mut self, profile_json: Option<&str>) -> Self {
        if let Some(json) = profile_json.filter(|s| !s.trim().is_empty()) {
            if let Ok(profile) = XClientProfile::parse(json) {
                self.features = profile.features.clone();
                self.field_toggles = profile.field_toggles.clone();
                self.detail_field_toggles = profile.tweet_detail_field_toggles.clone();
                self.query_ids = XQueryIdResolver::new(profile, self.page_origin.clone());
            }
        }
        self
    }

    pub fn with_sign_chain_json(mut self, sign_chain_json: Option<&str>) -> Self {
        if let Some(json) = sign_chain_json.filter(|s| !s.trim().is_empty()) {
            if let Ok(chain) = XSignChain::parse(json) {
                self.sign_chain = chain;
            }
        }
        self
    }

    pub fn with_preset_txid(self, tx: Arc<XTransaction>) -> Self {
        *self.txid_generator.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        self
    }

    pub fn with_sign_chain(mut self, chain: XSignChain) -> Self {
        self.sign_chain = chain;
        self
    }

    pub fn with_rate_limiter(mut self, limiter: Arc<XRateLimiter>) -> Self {
        self.rate_limiter = limiter;
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn rate_limiter(&self) -> &Arc<XRateLimiter> {
        &self.rate_limiter
    }

    pub fn last_txid_error(&self) -> Option<String> {
        self.last_txid_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// txid 按需初始化一次（进程内单实例只试一次，和 QuaX / Kotlin 一致）。失败只记原因，不阻断主请求。
    pub fn ensure_txid(&self) {
        if !self.txid_enabled {
            return;
        }
        if self
            .txid_generator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        if self.txid_tried.swap(true, Ordering::SeqCst) {
            return;
        }

        match self
            .sign_chain
            .resolve(&self.page_origin, &*self.page_http, None)
        {
            Ok(XSignChainOutcome::Success(sources)) => {
                match XTransaction::from_sources(
                    &sources.page_html,
                    &sources.sign_file_text,
                    DEFAULT_KEYWORD,
                    ADDITIONAL_RANDOM_NUMBER,
                ) {
                    Ok(tx) => {
                        *self
                            .txid_generator
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(tx));
                        *self
                            .last_txid_error
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = None;
                    }
                    Err(detail) => {
                        *self
                            .last_txid_error
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(detail);
                    }
                }
            }
            Ok(XSignChainOutcome::DisabledByPage) => {
                *self
                    .last_txid_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(
                    "页面的功能开关 rweb_client_transaction_id_enabled = false：官方网页这时也不带 txid，照做"
                        .to_string(),
                );
            }
            Err(e) => {
                let detail = match e {
                    XError::Signature { detail } => detail,
                    other => other.to_string(),
                };
                *self
                    .last_txid_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(detail);
            }
        }
    }

    /// 生成 txid：仅对 `/i/api/graphql/` 请求生效。
    pub fn generate_txid(
        &self,
        method: &str,
        path: &str,
        now_ms: i64,
        random_byte: u8,
    ) -> Option<String> {
        if !path.starts_with("/i/api/graphql/") {
            return None;
        }
        let gen = self
            .txid_generator
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        gen.as_ref()
            .map(|tx| tx.generate(method, path, now_ms, random_byte))
    }

    /// 查 operation 对应的 queryId：先查内存缓存，其次查 profile；开启自动扫脚本时兜底联网扫一次。
    fn query_id(&self, operation: XOperation) -> Result<String, XError> {
        if self.auto_scan_query_id {
            return self.query_ids.resolve(operation, &*self.page_http);
        }
        let op_name = operation.op();
        match self.query_ids.get(op_name) {
            Some(id) if !id.trim().is_empty() => Ok(id),
            _ => Err(XError::Internal {
                detail: format!(
                    "拿不到 {} 的 queryId（profile 里没有，扫网页脚本也没扫到）",
                    op_name
                ),
            }),
        }
    }

    /// 换一份 queryId。
    pub fn set_query_ids(&self, query_ids: Vec<(String, String)>) {
        self.query_ids.set_all(&query_ids);
    }

    /// 扫脚本刷新 queryId。
    pub fn refresh_query_id(&self, operation: XOperation) -> Option<String> {
        self.query_ids.refresh(operation, &*self.page_http).ok().flatten()
    }

    /// 一页。`cursor == None` 表示下拉刷新（第一页）。
    pub fn feed(
        &self,
        kind: FeedKind,
        cursor: Option<&str>,
        stale_retry: bool,
    ) -> Result<XFeedOutcome, XError> {
        // 顺序红线：必须先判 logged_in()（未登录直接回 NotLoggedIn，零请求，绝不先抓 txid 页面！）
        if !self.logged_in() {
            return Err(XError::NotLoggedIn {
                detail: Some(
                    "X 的时间线要登录：先在“我的”里导入 X 的 Cookie（auth_token + ct0）".to_string(),
                ),
            });
        }
        self.ensure_txid();

        let operation = kind.operation();
        let query_id = self.query_id(operation)?;
        let first = cursor.is_none();
        let variables = timeline_variables(kind, cursor, first);
        let url = self.graphql_url_with(&query_id, operation, &variables, &self.field_toggles);

        self.rate_limiter.check(crate::x::X)?;
        let headers = self.request_headers();
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers,
                body: Vec::new(),
            })
            .map_err(XError::from)?;

        self.rate_limiter.record(&response.headers);

        if stale_retry && (response.status == 404 || response.status == 422) {
            return Ok(XFeedOutcome::StaleQueryId);
        }

        let body = String::from_utf8_lossy(&response.body).into_owned();
        check_transport(&response, &body)?;
        let json = check_errors(&body)?;

        let page = match kind {
            FeedKind::Recommend => x::parse_home_timeline(&json),
            FeedKind::Following => x::parse_home_latest_timeline(&json),
        };
        let allow_more = self.allow_next_page(kind.key(), first);
        Ok(XFeedOutcome::Page(Page {
            items: page.items,
            next: if allow_more { page.next } else { None },
        }))
    }

    /// 详情：`TweetDetail` 的 `focalTweetId`。
    pub fn detail(&self, id: &str) -> Result<FeedDetail, XError> {
        let page = self.tweet_detail(id, None, "X 的推文详情要登录")?;
        x::detail_of(&page).ok_or_else(|| XError::Parse {
            detail: format!("详情里没有推文 {id}（可能已删 / 不可见 / 被广告过滤）"),
        })
    }

    /// 一级评论：同一条 `TweetDetail`，游标是 `bottom` 那条。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        _sort: CommentSort,
    ) -> Result<CommentPage, XError> {
        let page = self.tweet_detail(id, cursor, "X 的评论要登录")?;
        Ok(CommentPage {
            items: page.replies,
            next: page.next,
        })
    }

    /// 播放地址：`partId = media:<下标>`。
    pub fn play(&self, id: &str, part_id: &str) -> Result<PlayInfo, XError> {
        let page = self.tweet_detail(id, None, "X 的推文详情要登录")?;
        let tweet = page.tweet.ok_or_else(|| XError::Parse {
            detail: format!("详情里没有推文 {id}（可能已删 / 不可见 / 被广告过滤）"),
        })?;
        let video = tweet
            .videos
            .iter()
            .find(|v| v.part_id() == part_id)
            .ok_or_else(|| XError::Parse {
                detail: if tweet.videos.is_empty() {
                    format!("推文 {id} 里没有视频")
                } else {
                    let listed = tweet
                        .videos
                        .iter()
                        .map(|v| v.part_id())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("推文 {id} 里没有 {part_id}（有 {listed}）")
                },
            })?;
        super::play::play_info(video).ok_or_else(|| XError::Parse {
            detail: format!("推文 {id} 的 {part_id} 没有可用的播放地址（variant 全空）"),
        })
    }

    /// 楼中楼：当前返回空页。
    pub fn replies(&self, _id: &str, _comment_id: &str) -> Result<CommentPage, XError> {
        Ok(CommentPage::default())
    }

    fn tweet_detail(
        &self,
        id: &str,
        cursor: Option<&str>,
        login_detail: &str,
    ) -> Result<x::XDetailPage, XError> {
        if !self.logged_in() {
            return Err(XError::NotLoggedIn {
                detail: Some(login_detail.to_string()),
            });
        }
        self.ensure_txid();

        let operation = XOperation::TweetDetail;
        let variables = detail_variables(id, cursor);
        let mut refreshed = false;

        loop {
            let query_id = self.query_id(operation)?;
            let url = self.graphql_url_with(
                &query_id,
                operation,
                &variables,
                &self.detail_field_toggles,
            );

            self.rate_limiter.check(crate::x::X)?;
            let headers = self.request_headers();
            let response = self
                .http
                .execute(HttpRequest {
                    method: "GET".to_string(),
                    url,
                    headers,
                    body: Vec::new(),
                })
                .map_err(XError::from)?;

            self.rate_limiter.record(&response.headers);

            if (response.status == 404 || response.status == 422) && !refreshed {
                refreshed = true;
                let _ = self.query_ids.refresh(operation, &*self.page_http);
                continue;
            }

            let body = String::from_utf8_lossy(&response.body).into_owned();
            check_transport(&response, &body)?;
            let json = check_errors(&body)?;
            return Ok(x::parse_tweet_detail(&json, Some(id)));
        }
    }

    /// raw 请求：先 ensure_txid()，相对路径拼 base，对 X/base 域名补齐默认头与登录头，并记录响应头。
    pub fn raw(
        &self,
        method: String,
        url_or_path: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, XError> {
        self.ensure_txid();
        let full_url = if url_or_path.starts_with('/') {
            format!("{}{}", self.base.trim_end_matches('/'), url_or_path)
        } else {
            url_or_path
        };
        let mut final_headers = headers;
        if self.is_allowed_api_host(&full_url) {
            for (name, value) in browser_headers() {
                if !final_headers
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                {
                    final_headers.push((name, value));
                }
            }
            for (name, value) in self.auth_headers() {
                final_headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
                final_headers.push((name, value));
            }
        }
        let response = self
            .http
            .execute(HttpRequest {
                method,
                url: full_url,
                headers: final_headers,
                body: body.unwrap_or_default(),
            })
            .map_err(XError::from)?;
        self.rate_limiter.record(&response.headers);
        Ok(response)
    }

    fn logged_in(&self) -> bool {
        let has_ct0 = self
            .cookies
            .find(COOKIE_URL, "ct0")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !has_ct0 {
            return false;
        }
        self.cookies
            .find(COOKIE_URL, "auth_token")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }

    fn ct0(&self) -> Option<String> {
        self.cookies
            .find(COOKIE_URL, "ct0")
            .filter(|v| !v.trim().is_empty())
    }

    pub fn whoami(&self) -> Option<UserRef> {
        if !self.logged_in() {
            return None;
        }
        let response = self
            .page_http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: format!("{}/home", self.page_origin.trim_end_matches('/')),
                headers: page_headers(),
                body: Vec::new(),
            })
            .ok()?;
        if !(200..=299).contains(&response.status) {
            return None;
        }
        let html = String::from_utf8_lossy(&response.body).into_owned();
        crate::x::whoami_from_html(&html)
    }

    fn graphql_url_with(
        &self,
        query_id: &str,
        operation: XOperation,
        variables: &str,
        toggles: &[(String, bool)],
    ) -> String {
        let mut url = format!(
            "{}/i/api/graphql/{}/{}?variables={}&features={}",
            self.base,
            query_id,
            operation.op(),
            query_encode(variables),
            query_encode(&flags_json(&self.features))
        );
        if !toggles.is_empty() {
            url.push_str("&fieldToggles=");
            url.push_str(&query_encode(&flags_json(toggles)));
        }
        url
    }

    fn request_headers(&self) -> Vec<(String, String)> {
        let mut headers = browser_headers();
        headers.extend(self.auth_headers());
        headers
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![(
            "authorization".to_string(),
            format!("Bearer {BEARER_TOKEN}"),
        )];
        if let Some(ct0) = self.ct0() {
            headers.push(("x-csrf-token".to_string(), ct0));
        }
        if self.logged_in() {
            headers.push(("x-twitter-auth-type".to_string(), "OAuth2Session".to_string()));
        }
        headers
    }

    fn is_allowed_api_host(&self, url: &str) -> bool {
        let Some(host) = url_host(url) else {
            return false;
        };
        is_x_host(&host) || url_host(&self.base).as_deref() == Some(host.as_str())
    }

    fn allow_next_page(&self, key: &str, first_page: bool) -> bool {
        self.page_budget.allow_next_page(key, first_page)
    }
}

pub fn browser_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        (
            "Accept-Language".to_string(),
            "en-US,en;q=0.9".to_string(),
        ),
        ("Referer".to_string(), "https://x.com/".to_string()),
        ("Origin".to_string(), "https://x.com".to_string()),
        ("x-twitter-active-user".to_string(), "yes".to_string()),
        ("x-twitter-client-language".to_string(), "en".to_string()),
        ("Sec-Fetch-Dest".to_string(), "empty".to_string()),
        ("Sec-Fetch-Mode".to_string(), "cors".to_string()),
        ("Sec-Fetch-Site".to_string(), "same-origin".to_string()),
    ]
}

pub fn is_x_host(host: &str) -> bool {
    let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    ["x.com", "twitter.com", "twimg.com"]
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

fn url_host(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split_once(']')?.0
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let trimmed = host.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

impl From<HostError> for XError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => XError::Network { detail },
            HostError::ProxyUnavailable { detail } => XError::ProxyUnavailable { detail },
            HostError::Other { detail } => XError::Host { detail },
        }
    }
}

pub fn timeline_variables(kind: FeedKind, cursor: Option<&str>, first: bool) -> String {
    let mut out = String::from("{\"count\":");
    out.push_str(&PAGE_SIZE.to_string());
    out.push_str(",\"includePromotedContent\":false");
    if kind == FeedKind::Following {
        out.push_str(",\"latestControlAvailable\":true");
    }
    if let Some(cursor) = cursor {
        out.push_str(",\"cursor\":");
        out.push_str(&json_string(cursor));
    }
    if first {
        out.push_str(",\"requestContext\":\"launch\"");
    }
    if kind == FeedKind::Recommend {
        out.push_str(",\"withCommunity\":true");
    }
    out.push('}');
    out
}

pub fn detail_variables(focal_id: &str, cursor: Option<&str>) -> String {
    let mut out = String::from("{\"focalTweetId\":");
    out.push_str(&crate::hosted::json_string(focal_id));
    out.push_str(",\"with_rux_injections\":false");
    out.push_str(",\"rankingMode\":\"Relevance\"");
    out.push_str(",\"includePromotedContent\":false");
    out.push_str(",\"withCommunity\":true");
    out.push_str(",\"withQuickPromoteEligibilityTweetFields\":true");
    out.push_str(",\"withBirdwatchNotes\":true");
    out.push_str(",\"withVoice\":true");
    if let Some(cursor) = cursor.filter(|c| !c.trim().is_empty()) {
        out.push_str(",\"cursor\":");
        out.push_str(&crate::hosted::json_string(cursor));
        out.push_str(",\"referrer\":\"tweet\"");
    }
    out.push('}');
    out
}

pub fn flags_json(flags: &[(String, bool)]) -> String {
    let mut out = String::from("{");
    for (i, (name, value)) in flags.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(name));
        out.push(':');
        out.push_str(if *value { "true" } else { "false" });
    }
    out.push('}');
    out
}

pub use crate::hosted::json_string;
pub use crate::hosted::query_encode;

pub fn page_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        (
            "Accept".to_string(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
        ),
        ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string()),
        ("Cache-Control".to_string(), "no-cache".to_string()),
        ("Referer".to_string(), "https://x.com".to_string()),
        ("x-twitter-active-user".to_string(), "yes".to_string()),
        ("x-twitter-client-language".to_string(), "en".to_string()),
    ]
}

pub fn check_transport(response: &HttpResponse, body: &str) -> Result<(), XError> {
    match response.status {
        429 => {
            return Err(XError::RiskControl {
                code: Some("429".to_string()),
                detail: "被限速（Too Many Requests）".to_string(),
            })
        }
        401 => {
            return Err(XError::NotLoggedIn {
                detail: Some("登录已失效（HTTP 401）".to_string()),
            })
        }
        403 => {
            return Err(XError::NotLoggedIn {
                detail: Some("登录已失效或账号被限制（HTTP 403）".to_string()),
            })
        }
        412 => {
            return Err(XError::RiskControl {
                code: Some("412".to_string()),
                detail: "被风控（Precondition Failed）".to_string(),
            })
        }
        _ => {}
    }
    if let Some(signal) = risk_signal_in(body) {
        return Err(XError::RiskControl {
            code: Some("automated".to_string()),
            detail: signal,
        });
    }
    if !(200..=299).contains(&response.status) {
        return Err(XError::Api {
            code: format!("HTTP {}", response.status),
            detail: json::take_chars(body, 160),
        });
    }
    Ok(())
}

pub fn check_errors(body: &str) -> Result<JsonMap, XError> {
    let json = json::parse_object(body).map_err(|detail| XError::Parse { detail })?;
    let Some(errors) = json.get("errors").and_then(|e| e.as_array()) else {
        return Ok(json);
    };
    if errors.is_empty() {
        return Ok(json);
    }
    let first = errors.first().and_then(|e| e.as_object());
    let err_code = first.and_then(|e| json::long(e, "code"));
    let message = first
        .and_then(|e| json::text(e, "message"))
        .unwrap_or_else(|| "接口返回 errors".to_string());
    match err_code {
        Some(88) => Err(XError::RiskControl {
            code: Some("88".to_string()),
            detail: format!("被限速：{message}"),
        }),
        Some(215) => Err(XError::NotLoggedIn {
            detail: Some("账号校验失败（code 215）".to_string()),
        }),
        Some(32) | Some(34) => Err(XError::NotLoggedIn {
            detail: Some(format!(
                "登录已失效（code {}）",
                err_code.unwrap_or_default()
            )),
        }),
        Some(63) | Some(64) => Err(XError::Api {
            code: err_code.unwrap_or_default().to_string(),
            detail: format!("账号被冻结 / 暂停：{message}"),
        }),
        other => Err(XError::Api {
            code: other
                .map(|c| c.to_string())
                .unwrap_or_else(|| "errors".to_string()),
            detail: message,
        }),
    }
}

pub fn risk_signal_in(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("looks like it might be automated") {
        return Some("服务端提示「looks automated」".to_string());
    }
    if lower.contains("to protect our users from spam") {
        return Some("服务端提示疑似自动化流量".to_string());
    }
    if lower.contains("rate limit exceeded") {
        return Some("服务端提示 rate limit exceeded".to_string());
    }
    None
}
