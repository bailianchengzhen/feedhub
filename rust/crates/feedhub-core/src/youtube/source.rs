//! YouTube 的**平台层**：TV `browse`（`default` = 推荐、`FEsubscriptions` = 订阅）的请求怎么拼
//! （InnerTube body 的字段顺序、请求头顺序、`Authorization` 的位置、翻页游标）、状态码与风控怎么判、
//! 响应交给谁解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-youtube/YoutubeSource.browse` + `YoutubeApi.post` / `context` / `visitorData` /
//! `checkTransport`：方法（POST）+ URL（`/youtubei/v1/browse?prettyPrint=false`）+ body 逐字节 +
//! 请求头顺序必须和 Kotlin 版**逐字节一样**，`YoutubeRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! ## 当前搬了什么 / 没搬什么
//!
//! 搬：推荐 / 订阅两条流的整条请求 + 解析（[`crate::youtube::browse`]）+ 匿名身份（`visitor_data`）的
//! 读取与回写（`ctx.store` 的 `youtube.visitor_data`，和 Kotlin 那条路共用同一份）。
//!
//! 没搬：`player` / `next` / `search` / 设备码登录 / 播放（`YoutubeCipher` + QuickJS）。那些继续在 Kotlin
//! （`RustYoutubeSource` 逐方法委托），**token 刷新也留在 Kotlin**：这个对象只收"这次要带的 token"，
//! 不读、不存、不刷新、不打印它——Rust 这边一个字节都不会写进 store 或日志。
//!
//! ## 和 B站 样板不一样的四处
//!
//! 1. **没有 `CookieHost`**：`browse` 这条路上一个 Cookie 都不读（登录态是 `Authorization`，匿名身份是
//!    `visitor_data`），所以宿主只需要 `HttpHost` + `StoreHost` 两件。Cookie 罐照样由共用的那个 OkHttpClient
//!    自动带上（和 Kotlin 版同一份罐，字节不变）。
//! 2. **`Content-Type` 排在最后**：Kotlin 那边的 `Content-Type` 是 `RequestBody` 的 media type 带出来的，
//!    OkHttp 的 `BridgeInterceptor` 排在应用拦截器**之后**，会用 `header()` 把它从原位置挪到末尾并补上
//!    `charset=utf-8`；Rust 发出的请求 body 没有 media type，Bridge 不会动它，所以这里直接按**最终位置**
//!    写（值也写成最终那份 `application/json; charset=utf-8`）。`Accept` 同理：Kotlin 的
//!    `DefaultHeadersInterceptor` 在线上会把 `Accept` 追加到末尾，所以两边最终都是 …`Accept`, `Content-Type`。
//! 3. **`now_sec` 只给解析用**：browse 请求里没有时间戳（不需要签名），但 tile 的发布时间是相对文字
//!    （"3 days ago"），换算要用"现在"。从外面注入，两边才能用同一个钟。
//! 4. **`token` 是参数**（`Option<String>`）：只有 `Some` 才写 `Authorization: Bearer …`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::bili::source::CommentSort;
use crate::detail::{CommentPage, FeedDetail};
use crate::host::{HostError, HttpHost, HttpRequest, HttpResponse, ScriptHost, StoreHost};
use crate::json::{self, JsonMap};
use crate::model::Page;
use crate::play::PlayInfo;
use crate::youtube::account::{
    Credentials, LoginUrls, PollOutcome, RefreshOutcome, StartedDeviceCode,
};
use crate::youtube::detail;
use crate::youtube::{self, is_ascii_regex_ws, is_blank, play, trim_java, YoutubeError, YOUTUBE};

pub use crate::youtube::account::Endpoint;

/// 接口根（和 `YoutubeSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://www.youtube.com";

const PATH_BROWSE: &str = "/youtubei/v1/browse";
/// RFC 8628 §3.5：每收到一次 `slow_down`，轮询间隔加 5 秒。
const SLOW_DOWN_STEP_MS: i64 = 5_000;

/// 搜索的路径（`YoutubeSource.PATH_SEARCH`）。`youtube/search.rs` 也用它。
pub const PATH_SEARCH: &str = "/youtubei/v1/search";

/// 推荐流。
pub const BROWSE_DEFAULT: &str = "default";

/// 订阅流（接成"关注"流）。
pub const BROWSE_SUBSCRIPTIONS: &str = "FEsubscriptions";

/// 匿名身份（`visitorData`）在 `ctx.store` 里的键。**和 Kotlin `YoutubeApi.KEY_VISITOR_DATA` 一字不差**，
/// 所以两条路读写的都是同一份。
pub const KEY_VISITOR_DATA: &str = "youtube.visitor_data";
pub const KEY_ACCESS_TOKEN: &str = "youtube.access_token";
pub const KEY_REFRESH_TOKEN: &str = "youtube.refresh_token";
pub const KEY_TOKEN_EXPIRES: &str = "youtube.token_expires_at";
pub const KEY_DEVICE_ID: &str = "youtube.device_id";
pub const KEY_ENDPOINT: &str = "youtube.device_code_endpoint";
pub const KEY_TV_CLIENT_ID: &str = "youtube.tv_client_id";
pub const KEY_TV_CLIENT_SECRET: &str = "youtube.tv_client_secret";
pub const KEY_GOOGLE_CLIENT_ID: &str = "youtube.google_client_id";
pub const KEY_GOOGLE_CLIENT_SECRET: &str = "youtube.google_client_secret";

/// `context.client` 的身份：TVHTML5 7.x（`YoutubeClient.TVHTML5`，逐字节一样）。
pub const CLIENT_NAME: &str = "TVHTML5";
pub const CLIENT_VERSION: &str = "7.20260901.15.00";
pub const CLIENT_HEADER_NAME: &str = "7";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), \
                              Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)";
pub const ORIGIN: &str = "https://www.youtube.com";
pub const REFERER: &str = "https://www.youtube.com/tv";
pub const ACCEPT: &str = "application/json,text/plain,*/*";
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// 请求体的最终 Content-Type（Kotlin `YoutubeApi.JSON` 的 media type，BridgeInterceptor 会把它写成这个值）。
pub const CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// `YoutubeClient.WEB` 的版本（只有搜索用它；`context.client.clientVersion` 和
/// `X-YouTube-Client-Version` 都是这个）。
pub const WEB_CLIENT_VERSION: &str = "2.20250925.01.00";

/// `core.UserAgents.DESKTOP_CHROME`，**逐字节一样**（网页客户端和平台默认头都用它）。
pub const DESKTOP_CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 哪一条流（对应 Kotlin `FeedKind` 里 YouTube 支持的两个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

impl FeedKind {
    /// `browseId`。
    pub fn browse_id(self) -> &'static str {
        match self {
            FeedKind::Recommend => BROWSE_DEFAULT,
            FeedKind::Following => BROWSE_SUBSCRIPTIONS,
        }
    }

    /// `FeedKind.label` 的中文名（报错里要用，和 Kotlin 一字不差）。
    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Recommend => "推荐",
            FeedKind::Following => "关注",
        }
    }
}

/// YouTube 的平台对象：两个宿主端口（发请求、读写 store）+ 登录端点的四个地址。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustYoutubeHosted`）建一次、跟着 Source 活。
/// **没有后台线程 / 定时器 / 连接池**（闲置时零唤醒）；唯一的可变状态是三个进程内的缓存
/// （详情 / 评论共用的 `next`、楼中楼 token、播放器脚本全文），都用 `Mutex` 保护，可以跨线程用。
pub struct YoutubeHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    /// 登录端点的四个地址（`LoginUrls`，测试换成 MockWebServer）。
    login_urls: LoginUrls,
    /// `next {videoId}` 的结果（元数据 + 每种排序的评论游标）和"撞过风控的视频"：
    /// 详情和评论共用同一次 `next`（Kotlin 的 `watchCache` / `watchRisk` / `watchLock` 三件并成一把锁）。
    watch: Mutex<WatchCache>,
    /// 评论 id → 楼中楼的续页 token（解析一级评论时记下来的，Kotlin 的 `repliesTokens`）。
    replies_tokens: Mutex<HashMap<String, String>>,
    /// 播放器脚本按 hash 的缓存（Kotlin `YoutubePlayerScript.memory`）：一次播放会话里只下载一次全文。
    scripts: Mutex<HashMap<String, PlayerScript>>,
    /// 并发换 token 的单飞锁：详情、评论、whoami 同时发现 token 过期时只允许一跳 `/o/oauth2/token` 在飞。
    refresh_lock: Mutex<()>,
    /// 设备码轮询的退避：收到 `slow_down` 后间隔加 5 秒。宿主按首包的间隔定时来问，
    /// 没到点的那几次这里直接回"还在等"、不发请求。
    device_poll: Mutex<DevicePoll>,
}

/// 当前这次设备码登录的轮询节奏。
#[derive(Default)]
struct DevicePoll {
    device_code: String,
    interval_ms: i64,
    extra_ms: i64,
    last_ms: i64,
}

impl YoutubeHostedSource {
    pub fn new(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
    ) -> Self {
        Self::with_login_urls(base, http, store, LoginUrls::default())
    }

    /// 带上登录端点的地址（线上就是 [`LoginUrls::default`]；测试换成 MockWebServer）。
    pub fn with_login_urls(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        login_urls: LoginUrls,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            store,
            login_urls,
            watch: Mutex::new(WatchCache::default()),
            replies_tokens: Mutex::new(HashMap::new()),
            scripts: Mutex::new(HashMap::new()),
            refresh_lock: Mutex::new(()),
            device_poll: Mutex::new(DevicePoll::default()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 宿主 HTTP 端口（同模块的 `youtube/search.rs` 也要用它发请求）。
    pub(crate) fn http_port(&self) -> Arc<dyn HttpHost> {
        self.http.clone()
    }

    /// 匿名身份（`visitor_data`）。搜索也要带它（`x-goog-visitor-id` 和 body 里的 `context.client.visitorData`）。
    pub(crate) fn visitor_for_search(&self) -> Option<String> {
        self.visitor()
    }

    /// 搜索响应里的 `responseContext.visitorData` 回写（和 `browse` 共用同一份 store 键）。
    pub(crate) fn remember_visitor_public(&self, map: &json::JsonMap) {
        self.remember_visitor(map);
    }

    /// 匿名身份（追踪标识，**不打印**）。空串当没有（Kotlin `takeIf { it.isNotBlank() }`）。
    fn visitor(&self) -> Option<String> {
        self.store
            .get(KEY_VISITOR_DATA)
            .filter(|v| !youtube::is_blank(v))
    }

    /// 一页。`now_sec` 只给解析相对时间用；`token` 是要不要带 `Authorization`（由 Kotlin 决定，见模块说明）。
    pub fn feed(
        &self,
        kind: FeedKind,
        cursor: Option<&str>,
        now_sec: i64,
        token: Option<&str>,
    ) -> Result<Page, YoutubeError> {
        let visitor = self.visitor();
        let body = browse_body(kind.browse_id(), cursor, visitor.as_deref());
        let map = self.post(PATH_BROWSE, Client::TvHtml5, &body, token)?;
        Ok(youtube::browse(&map, cursor, now_sec))
    }

    /// 记下响应里的 `responseContext.visitorData`（Kotlin `rememberVisitor` / `saveVisitorData`）。
    fn remember_visitor(&self, map: &json::JsonMap) {
        if let Some(visitor) =
            json::path(map, &["responseContext"]).and_then(|ctx| json::text(ctx, "visitorData"))
        {
            self.store.put(KEY_VISITOR_DATA, Some(visitor.as_str()));
        }
    }

    // ---------- 会话与 Token 存储 ----------

    /// 当前可用的 access token（过期的算 None，与 Kotlin `YoutubeApi.token()` 逐行等价）。
    pub fn token_at(&self, now_ms: i64) -> Option<String> {
        let expires = self
            .store
            .get(KEY_TOKEN_EXPIRES)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if (1..(now_ms + 60_000)).contains(&expires) {
            return None;
        }
        self.store
            .get(KEY_ACCESS_TOKEN)
            .filter(|t| !youtube::is_blank(t))
    }

    /// 本机存的 refresh_token（没有或空白是 None）。
    pub fn refresh_token_at(&self) -> Option<String> {
        self.store
            .get(KEY_REFRESH_TOKEN)
            .filter(|t| !youtube::is_blank(t))
    }

    /// 本机有没有登录态：`refresh_token` 非空白或 `token_at(now_ms)` 存在。
    pub fn has_session(&self, now_ms: i64) -> bool {
        self.refresh_token_at().is_some() || self.token_at(now_ms).is_some()
    }

    /// 退出登录：清空 access_token、refresh_token、token_expires_at。
    pub fn logout_session(&self) {
        self.store.put(KEY_ACCESS_TOKEN, None);
        self.store.put(KEY_REFRESH_TOKEN, None);
        self.store.put(KEY_TOKEN_EXPIRES, None);
    }

    /// 稳定的设备 id（`youtube.device_id`）：存在非空白直接复用；否则用 getrandom 生成 32 位小写 hex 写回 store。
    pub fn device_id(&self) -> String {
        if let Some(id) = self.store.get(KEY_DEVICE_ID).filter(|s| !youtube::is_blank(s)) {
            return id;
        }
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).expect("getrandom failed");
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let id: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
        self.store.put(KEY_DEVICE_ID, Some(&id));
        id
    }

    /// 上次用过的设备码端点（`youtube.device_code_endpoint`），默认 TV。
    pub fn current_endpoint(&self) -> Endpoint {
        self.store
            .get(KEY_ENDPOINT)
            .as_deref()
            .and_then(Endpoint::from_key)
            .unwrap_or(Endpoint::Tv)
    }

    /// 记住端点。
    pub fn remember_endpoint(&self, endpoint: Endpoint) {
        self.store.put(KEY_ENDPOINT, Some(endpoint.key()));
    }

    /// store 里缓存的 client 凭据。
    pub fn cached_credentials(&self, endpoint: Endpoint) -> Option<Credentials> {
        let (id_key, secret_key) = match endpoint {
            Endpoint::Tv => (KEY_TV_CLIENT_ID, KEY_TV_CLIENT_SECRET),
            Endpoint::Google => (KEY_GOOGLE_CLIENT_ID, KEY_GOOGLE_CLIENT_SECRET),
        };
        let id = self.store.get(id_key).filter(|s| !youtube::is_blank(s))?;
        let secret = self.store.get(secret_key).unwrap_or_default();
        Some(Credentials { id, secret })
    }

    /// 把抓到的 client 凭据写回 store（只有 TV 端点需要）。
    pub fn save_scraped_credentials(&self, endpoint: Endpoint, creds: &Credentials) {
        match endpoint {
            Endpoint::Tv => {
                self.store.put(KEY_TV_CLIENT_ID, Some(&creds.id));
                self.store.put(KEY_TV_CLIENT_SECRET, Some(&creds.secret));
            }
            Endpoint::Google => {}
        }
    }

    /// 解析当前端点凭据（与 Kotlin `YoutubeLogin.credentials` 逐行等价）：
    /// TV 端点缺缓存（或 secret 为空白）时先抓 `/tv` + `base.js` 并**立即写回 store**，
    /// 即使后续设备码 / token 端点返回 500 或坏 JSON 也不会丢失已抓到的凭据。
    fn resolve_session_credentials(&self, endpoint: Endpoint) -> Result<Credentials, YoutubeError> {
        match endpoint {
            Endpoint::Google => {
                let creds = self
                    .cached_credentials(Endpoint::Google)
                    .filter(|c| !youtube::is_blank(&c.id))
                    .ok_or_else(|| YoutubeError::Api {
                        code: "no-client-id".to_string(),
                        detail: "GOOGLE 端点的 client_id / client_secret 要在 ctx.store 里给（youtube.google_client_id / \
                                 youtube.google_client_secret）；舞台上的默认是 TV 端点"
                            .to_string(),
                    })?;
                Ok(creds)
            }
            Endpoint::Tv => match self.cached_credentials(Endpoint::Tv) {
                Some(creds) if !youtube::is_blank(&creds.id) && !youtube::is_blank(&creds.secret) => {
                    Ok(creds)
                }
                _ => {
                    let scraped = self.scrape_tv_credentials()?;
                    self.save_scraped_credentials(Endpoint::Tv, &scraped);
                    Ok(scraped)
                }
            },
        }
    }

    fn refresh_in_store_unlocked(&self, now_ms: i64) -> Result<bool, YoutubeError> {
        let Some(refresh) = self.refresh_token_at() else {
            return Ok(false);
        };
        let endpoint = self.current_endpoint();
        let creds = self.resolve_session_credentials(endpoint)?;
        let (outcome, _) = self.refresh_session(endpoint, &refresh, Some(creds))?;
        match outcome {
            RefreshOutcome::Refreshed(tokens) => {
                self.store.put(KEY_ACCESS_TOKEN, Some(&tokens.access_token));
                let expires_at =
                    now_ms.saturating_add(tokens.expires_in_sec.unwrap_or(3600).saturating_mul(1000));
                self.store.put(KEY_TOKEN_EXPIRES, Some(&expires_at.to_string()));
                Ok(true)
            }
            RefreshOutcome::Revoked => {
                self.logout_session();
                Ok(false)
            }
            RefreshOutcome::Failed { status } => Err(YoutubeError::Api {
                code: "token-refresh-failed".to_string(),
                detail: format!("换 YouTube 登录 token 没成功（HTTP {status}），登录还在，稍后再试"),
            }),
        }
    }

    /// 用 refresh_token 换新 token 并写回 store（`YoutubeLogin.refresh` 的语义）。
    /// 成功返回 Ok(true)；没有 refresh_token 返回 Ok(false)；凭据作废登出返回 Ok(false)；一时失败保留登录态并返回 Err。
    pub fn refresh_in_store(&self, now_ms: i64) -> Result<bool, YoutubeError> {
        let _guard = self.refresh_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh_in_store_unlocked(now_ms)
    }

    /// 并发单飞检查并按需换 token：已有未过期 access_token 时返回 `Ok(true)`；否则在 `refresh_lock` 内只换一次。
    fn ensure_valid_token_locked(&self, now_ms: i64) -> Result<bool, YoutubeError> {
        if self.token_at(now_ms).is_some() {
            return Ok(true);
        }
        if self.refresh_token_at().is_none() {
            return Ok(false);
        }
        let _guard = self.refresh_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.token_at(now_ms).is_some() {
            return Ok(true);
        }
        self.refresh_in_store_unlocked(now_ms)
    }

    /// token 过期就换一次，换不到不向外抛异常，返回 Option<YoutubeError>。
    pub fn ensure_login_token(&self, now_ms: i64) -> Option<YoutubeError> {
        if !self.has_session(now_ms) || self.token_at(now_ms).is_some() {
            return None;
        }
        match self.ensure_valid_token_locked(now_ms) {
            Ok(_) => None,
            Err(e) => Some(e),
        }
    }

    /// App 回到前台 / 刚启动时调：后台换好 token。
    pub fn keep_session_fresh(&self, now_ms: i64) {
        let _ = self.ensure_login_token(now_ms);
    }

    /// 高层推荐 / 关注流会话编排：过期刷新、401 自动重试一次。
    pub fn feed_session(
        &self,
        kind: FeedKind,
        cursor: Option<&str>,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<Page, YoutubeError> {
        if self.has_session(now_ms) && self.token_at(now_ms).is_none() {
            self.ensure_valid_token_locked(now_ms)?;
        }
        let token = self.token_at(now_ms);
        let result = self.feed(kind, cursor, now_sec, token.as_deref());
        match result {
            Err(e) => {
                if let YoutubeError::Api { ref code, .. } = e {
                    if code == "HTTP 401" && self.has_session(now_ms) {
                        let refreshed = self.refresh_in_store(now_ms).unwrap_or(false);
                        if refreshed {
                            let new_token = self.token_at(now_ms);
                            return self.feed(kind, cursor, now_sec, new_token.as_deref());
                        }
                    }
                }
                Err(e)
            }
            Ok(page) => Ok(page),
        }
    }

    /// 高层 whoami 会话编排：没登录返回 Ok(None)；过期换 token；一时失败抛错；无账号返回 Ok(None)。
    pub fn whoami_session(&self, now_ms: i64) -> Result<Option<crate::model::UserRef>, YoutubeError> {
        if !self.has_session(now_ms) {
            return Ok(None);
        }
        if !self.ensure_valid_token_locked(now_ms)? {
            return Ok(None);
        }
        self.whoami(self.token_at(now_ms).as_deref())
    }

    /// 高层详情会话编排：刷新过期 token（换不到不阻碍播放元数据降级请求）。
    pub fn detail_session(
        &self,
        id: &str,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<FeedDetail, YoutubeError> {
        let _ = self.ensure_login_token(now_ms);
        self.detail_with_clock(
            id,
            now_sec,
            now_ms,
            self.has_session(now_ms),
            self.token_at(now_ms).as_deref(),
        )
    }

    /// 高层一级评论会话编排：无有效 token 返回空页。
    pub fn comments_session(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<CommentPage, YoutubeError> {
        if !self.ensure_valid_token_locked(now_ms)? {
            return Ok(CommentPage {
                items: Vec::new(),
                next: None,
            });
        }
        self.comments_with_clock(
            id,
            cursor,
            sort,
            now_sec,
            self.token_at(now_ms).as_deref(),
        )
    }

    /// 高层楼中楼评论会话编排：无有效 token 返回空页。
    pub fn replies_session(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<CommentPage, YoutubeError> {
        if !self.ensure_valid_token_locked(now_ms)? {
            return Ok(CommentPage {
                items: Vec::new(),
                next: None,
            });
        }
        self.replies_with_clock(
            id,
            comment_id,
            cursor,
            now_sec,
            self.token_at(now_ms).as_deref(),
        )
    }

    /// 高层播放会话编排：ensure_login_token，撞 bot-check 时重抛换 token 异常。
    pub fn play_session(
        &self,
        id: &str,
        part_id: &str,
        quality: Option<i32>,
        now_ms: i64,
        script: Option<&dyn ScriptHost>,
    ) -> Result<PlayInfo, YoutubeError> {
        let failure = self.ensure_login_token(now_ms);
        let session = self.has_session(now_ms);
        let token = self.token_at(now_ms);
        self.play_internal(
            id,
            part_id,
            quality,
            now_ms,
            session,
            token.as_deref(),
            script,
            failure.as_ref(),
        )
    }

    /// 高层取设备码会话编排：记录端点、生成/复用 device_id、调用 start_device_code、落库 scraped 凭据。
    pub fn start_device_code_session(&self, now_ms: i64) -> Result<StartedDeviceCode, YoutubeError> {
        let endpoint = self.current_endpoint();
        self.remember_endpoint(endpoint);
        let device_id = self.device_id();
        let creds = self.resolve_session_credentials(endpoint)?;
        let started = self.start_device_code(endpoint, &device_id, Some(creds), now_ms)?;
        *self.device_poll.lock().unwrap_or_else(|e| e.into_inner()) = DevicePoll {
            device_code: started.device_code.clone(),
            interval_ms: started.poll_interval_ms,
            extra_ms: 0,
            last_ms: now_ms,
        };
        Ok(started)
    }

    /// 高层轮询设备码会话编排：调用 poll_device_code、落库 scraped 凭据，成功时存储 token 集合。
    pub fn poll_device_code_session(
        &self,
        device_code: &str,
        now_ms: i64,
    ) -> Result<PollOutcome, YoutubeError> {
        {
            let mut poll = self.device_poll.lock().unwrap_or_else(|e| e.into_inner());
            if poll.device_code == device_code {
                // 留 500 ms 余量：宿主的定时器不会正好卡在间隔上
                if poll.extra_ms > 0 && now_ms - poll.last_ms < poll.interval_ms + poll.extra_ms - 500 {
                    return Ok(PollOutcome::Waiting);
                }
                poll.last_ms = now_ms;
            }
        }
        let endpoint = self.current_endpoint();
        let creds = self.resolve_session_credentials(endpoint)?;
        let poll_res = self.poll_device_code(endpoint, device_code, Some(creds));
        let (outcome, _) = match poll_res {
            Ok(pair) => pair,
            Err(YoutubeError::NotLoggedIn { detail }) => {
                (PollOutcome::Failed(detail.unwrap_or_else(|| "设备码轮询失败".to_string())), None)
            }
            Err(e) => return Err(e),
        };
        let outcome = if outcome == PollOutcome::SlowDown {
            let mut poll = self.device_poll.lock().unwrap_or_else(|e| e.into_inner());
            if poll.device_code == device_code {
                poll.extra_ms += SLOW_DOWN_STEP_MS;
            }
            PollOutcome::Waiting
        } else {
            outcome
        };
        if let PollOutcome::Succeeded(ref tokens) = outcome {
            self.store.put(KEY_ACCESS_TOKEN, Some(&tokens.access_token));
            if let Some(ref refresh) = tokens.refresh_token {
                if !youtube::is_blank(refresh) {
                    self.store.put(KEY_REFRESH_TOKEN, Some(refresh));
                }
            }
            let expires_at =
                now_ms.saturating_add(tokens.expires_in_sec.unwrap_or(3600).saturating_mul(1000));
            self.store.put(KEY_TOKEN_EXPIRES, Some(&expires_at.to_string()));
        }
        Ok(outcome)
    }

    /// 调试用 `raw` 请求：共享平台 `HttpHost`（连接池 / 500ms 限速 / 代理 fail-closed），
    /// 相对路径拼 `base`，仅在 YouTube 域名或当前 `base` 域名上补齐默认头（不向外域泄露）。
    pub fn raw(
        &self,
        method: String,
        url_or_path: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, YoutubeError> {
        let full_url = if url_or_path.starts_with('/') {
            format!("{}{}", self.base.trim_end_matches('/'), url_or_path)
        } else {
            url_or_path
        };
        let mut final_headers = headers;
        if self.is_allowed_default_header_host(&full_url) {
            for (name, value) in [
                ("User-Agent", VISIONOS_USER_AGENT),
                ("Accept", ACCEPT),
                ("Accept-Language", ACCEPT_LANGUAGE),
            ] {
                if !final_headers
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
                {
                    final_headers.push((name.to_string(), value.to_string()));
                }
            }
        }
        self.http
            .execute(HttpRequest {
                method,
                url: full_url,
                headers: final_headers,
                body: body.unwrap_or_default(),
            })
            .map_err(YoutubeError::from)
    }

    fn is_allowed_default_header_host(&self, url: &str) -> bool {
        let Some(host) = extract_url_host(url) else {
            return false;
        };
        if is_youtube_host(&host) {
            return true;
        }
        match (extract_url_authority(url), extract_url_authority(&self.base)) {
            (Some(target), Some(base)) => target == base,
            _ => false,
        }
    }
}

fn extract_url_authority(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    if host_port.is_empty() {
        None
    } else {
        Some(host_port.to_ascii_lowercase())
    }
}

fn extract_url_host(url: &str) -> Option<String> {
    let host_port = extract_url_authority(url)?;
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().map(|h| format!("[{h}]"))?
    } else {
        host_port.split(':').next()?.to_string()
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub fn is_youtube_host(host: &str) -> bool {
    let h = host
        .split(':')
        .next()
        .unwrap_or(host)
        .to_ascii_lowercase();
    h == "youtube.com" || h.ends_with(".youtube.com")
}

impl From<HostError> for YoutubeError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => YoutubeError::Network { detail },
            HostError::ProxyUnavailable { detail } => YoutubeError::ProxyUnavailable { detail },
            HostError::Other { detail } => YoutubeError::Host { detail },
        }
    }
}

/// InnerTube 的 body，**字段顺序照 `YoutubeSource.browse` 的 `buildJsonObject`**，逐字节一样。
///
/// 没有用 `serde_json::to_string`：默认的 `Map` 是 `BTreeMap`，会把键按字典序重排，body 就变了。
fn browse_body(browse_id: &str, cursor: Option<&str>, visitor: Option<&str>) -> String {
    let mut s = String::with_capacity(512);
    push_context(&mut s, Client::TvHtml5, visitor);
    push_field(&mut s, "browseId", browse_id, true);
    if let Some(cursor) = cursor {
        // Kotlin 没有"空白游标当没有"这一步：`cursor?.value?.let { put("continuation", it) }`
        push_field(&mut s, "continuation", cursor, true);
    }
    s.push_str(
        ",\"tvAppInfo\":{\"appQuality\":\"TV_APP_QUALITY_FULL_ANIMATION\",\"zylonLeftNav\":true}",
    );
    s.push_str(",\"webpSupport\":false");
    s.push_str(",\"animatedWebpSupport\":true");
    s.push_str(",\"contentCheckOk\":true");
    s.push_str(",\"racyCheckOk\":true}");
    s
}

/// 追加 `,"key":"value"`（`first` 时省掉前导逗号）。
fn push_field(s: &mut String, key: &str, value: &str, leading_comma: bool) {
    if leading_comma {
        s.push(',');
    }
    s.push('"');
    s.push_str(key);
    s.push_str("\":");
    push_quoted(s, value);
}

/// kotlinx.serialization 的字符串字面量（`"` / `\` / 控制字符转义，其余原样）。
pub(crate) fn push_quoted(s: &mut String, value: &str) {
    s.push('"');
    for c in value.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\t' => s.push_str("\\t"),
            '\u{08}' => s.push_str("\\b"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\u{0C}' => s.push_str("\\f"),
            c if c < ' ' => s.push_str(&format!("\\u{:04X}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

/// 请求头，**顺序照 Kotlin 版最终落到线上的那一份**（`YoutubeApi.post` 的 `header()` 调用顺序 +
/// `Accept`，`Content-Type` 在最后，理由见模块说明第 2 条）。
///
/// `token` 是"这次要不要带 `Authorization: Bearer`"（由 Kotlin 决定，见模块说明）：只有
/// `Some` 且这个客户端是电视客户端时才写（Kotlin 的 `authorized && client.tv`）。
pub(crate) fn post_headers(
    client: Client,
    visitor: Option<&str>,
    token: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::with_capacity(10);
    let mut add = |name: &str, value: &str| headers.push((name.to_string(), value.to_string()));
    add("X-YouTube-Client-Name", client.header_name());
    add("X-YouTube-Client-Version", client.version());
    add("Origin", ORIGIN);
    add("Referer", client.referer());
    add("User-Agent", client.user_agent());
    add("Accept-Language", ACCEPT_LANGUAGE);
    if let Some(visitor) = visitor {
        add("X-Goog-Visitor-Id", visitor);
    }
    if let Some(token) = token {
        add("Authorization", &format!("Bearer {token}"));
    }
    add("Accept", ACCEPT);
    add("Content-Type", CONTENT_TYPE);
    headers
}

/// 传输层：429 是"一分钟墙"、403 是被拒、412 是网关拦截，全部停下抛风控。**不重试、不换客户端再试。**
///
/// 文案照 `YoutubeApi.checkTransport` 一字不差。
pub fn check_transport(status: u16, body: &str) -> Result<(), YoutubeError> {
    match status {
        429 => Err(YoutubeError::RiskControl {
            code: "HTTP 429".to_string(),
            detail: "被限速（可能是一分钟墙）".to_string(),
        }),
        403 => Err(YoutubeError::RiskControl {
            code: "HTTP 403".to_string(),
            detail: "请求被拒（风控 / 分片鉴权）".to_string(),
        }),
        412 => Err(YoutubeError::RiskControl {
            code: "HTTP 412".to_string(),
            detail: "被网关拦截".to_string(),
        }),
        s if !(200..=299).contains(&s) => Err(YoutubeError::Api {
            code: format!("HTTP {s}"),
            detail: json::take_chars(body, 200),
        }),
        _ if youtube::is_blank(body) => Err(YoutubeError::RiskControl {
            code: "empty-body".to_string(),
            detail: "返回空 body".to_string(),
        }),
        _ => Ok(()),
    }
}

// ===========================================================================
// 客户端身份
// ===========================================================================

/// InnerTube 的客户端身份（照 `YoutubeApi.kt` 的 `YoutubeClient`）。三个：
/// [`Client::Visionos`]（匿名拿流 / 详情降级）、[`Client::TvHtml5`]（`browse` / `next`）、
/// [`Client::TvTizen`]（登录态 `player`，要真实 5 位 `signatureTimestamp`）。
///
/// 这些值会随 YouTube 改版失效，失效时先改这几个字段（Kotlin 那边同一份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Visionos,
    TvHtml5,
    TvTizen,
    /// **网页客户端**（`YoutubeClient.WEB`）：只有搜索用它（TVHTML5 的 `search` 回的是新的
    /// `lockupViewModel` 形状，见 `youtube/search.rs` 的模块说明）。匿名身份，不带 bearer。
    Web,
}

impl Client {
    /// `context.client.clientName`（两个电视客户端的名字都是 `TVHTML5`）。
    pub fn name(self) -> &'static str {
        match self {
            Client::Visionos => "VISIONOS",
            Client::TvHtml5 | Client::TvTizen => CLIENT_NAME,
            Client::Web => "WEB",
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Client::Visionos => "1.02",
            Client::TvHtml5 => CLIENT_VERSION,
            Client::TvTizen => "5.20260707",
            Client::Web => WEB_CLIENT_VERSION,
        }
    }

    /// `X-YouTube-Client-Name`（数字）。
    pub fn header_name(self) -> &'static str {
        match self {
            Client::Visionos => "101",
            Client::TvHtml5 => CLIENT_HEADER_NAME,
            Client::TvTizen => "7",
            Client::Web => "1",
        }
    }

    pub fn user_agent(self) -> &'static str {
        match self {
            Client::Visionos => VISIONOS_USER_AGENT,
            Client::TvHtml5 => USER_AGENT,
            Client::TvTizen => TIZEN_USER_AGENT,
            // `YoutubeClient.WEB` 用的是 `UserAgents.DESKTOP_CHROME`（和 `core` 逐字节一样）
            Client::Web => super::source::DESKTOP_CHROME,
        }
    }

    pub fn referer(self) -> &'static str {
        match self {
            // `YoutubeClient` 的默认 referer 带结尾的 `/`（和 `ORIGIN` 不是同一个字符串）
            Client::Visionos => VISIONOS_REFERER,
            Client::TvHtml5 | Client::TvTizen => REFERER,
            Client::Web => VISIONOS_REFERER,
        }
    }

    /// 电视客户端（`context` 里要 `clientScreen` / `user`，且只有它支持 bearer）。
    pub fn tv(self) -> bool {
        !matches!(self, Client::Visionos | Client::Web)
    }

    fn device_make(self) -> Option<&'static str> {
        match self {
            Client::Visionos => Some("Apple"),
            Client::TvTizen => Some("Samsung"),
            Client::TvHtml5 | Client::Web => None,
        }
    }

    fn device_model(self) -> Option<&'static str> {
        match self {
            Client::Visionos => Some("RealityDevice17,1"),
            Client::TvTizen => Some("UKS9800"),
            Client::TvHtml5 | Client::Web => None,
        }
    }

    fn os_name(self) -> Option<&'static str> {
        match self {
            Client::Visionos => Some("visionOS"),
            Client::TvTizen => Some("Tizen"),
            Client::TvHtml5 | Client::Web => None,
        }
    }

    fn os_version(self) -> Option<&'static str> {
        match self {
            Client::Visionos => Some("26.5.23O471"),
            Client::TvTizen => Some("2.4.0"),
            Client::TvHtml5 | Client::Web => None,
        }
    }

    /// 健康记录在 `ctx.store` 里的键（照 `YoutubeClientHealth.key`：
    /// `youtube.health.<名字小写>_<版本前 3 个字符>`）。
    pub fn health_key(self) -> String {
        format!(
            "youtube.health.{}_{}",
            self.name().to_lowercase(),
            take_chars(self.version(), 3)
        )
    }
}

/// 匿名 VISIONOS 的 `Referer`（`YoutubeClient` 的默认值：**带结尾的 `/`**）。
pub const VISIONOS_REFERER: &str = "https://www.youtube.com/";

/// 匿名 VISIONOS 的 UA（`YoutubeClient.VISIONOS`）。
pub const VISIONOS_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

/// 登录态 TV_TIZEN 的 UA（`YoutubeClient.TV_TIZEN`，yt-dlp 的 `tv_samsung`）。
pub const TIZEN_USER_AGENT: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 \
     (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1";

const PATH_PLAYER: &str = "/youtubei/v1/player";
const PATH_NEXT: &str = "/youtubei/v1/next";
const PATH_IFRAME_API: &str = "/iframe_api";

/// `whoami` 的账号列表。
pub(crate) const PATH_ACCOUNTS: &str = "/youtubei/v1/account/accounts_list";

/// 播放器脚本里 hash / sts 的位置（Kotlin `YoutubePlayerScript` 的两个正则）。
const PLAYER_SCRIPT_PATH: &str = "/s/player";

/// 播放器脚本的缓存键（和 Kotlin `YoutubePlayerScript` 一字不差，两边共用同一份缓存）。
const KEY_PLAYER_HASH: &str = "youtube.player_hash";
const KEY_PLAYER_STS: &str = "youtube.player_sts";
const KEY_PLAYER_AT: &str = "youtube.player_at";

/// sts 缓存多久（6 小时：播放器 hash 一般几天才换）。
const STS_TTL_MS: i64 = 6 * 3600 * 1000;

/// 一个客户端连续失败几次进冷却（Kotlin `YoutubeClientHealth.MAX_FAILS`）。
const HEALTH_MAX_FAILS: i64 = 2;

/// 冷却多久（30 分钟）。
const HEALTH_COOLDOWN_MS: i64 = 30 * 60 * 1000;

/// `next {videoId}` 的结果：元数据 + 每种排序的评论游标（Kotlin `YoutubeSource.WatchInfo`）。
#[derive(Debug, Clone, Default)]
struct WatchInfo {
    meta: Option<detail::YoutubeVideoMeta>,
    tokens: Vec<(CommentSort, String)>,
}

impl WatchInfo {
    fn token_for(&self, sort: CommentSort) -> Option<String> {
        self.tokens
            .iter()
            .find(|(s, _)| *s == sort)
            .map(|(_, t)| t.clone())
    }
}

/// 详情 / 评论共用的缓存（Kotlin 的 `watchCache` / `watchRisk`）。
#[derive(Debug, Default)]
struct WatchCache {
    infos: HashMap<String, WatchInfo>,
    /// 这个视频的 `next` 撞过风控：当前**不再问**（风控红线：遇到就停，不重试）。原错误留着，照实抛。
    risks: HashMap<String, YoutubeError>,
}

/// `context` 的 JSON **字符串**（[`push_context`] 的成品）：搜索要把 `{context, query, …}` 拼成一个字符串
/// 才能和 Kotlin 的 `buildJsonObject` 逐字节一样（`serde_json` 的 `Map` 是 `BTreeMap`，会按字典序重排键）。
pub(crate) fn context_json(client: Client, visitor: Option<&str>) -> String {
    // `push_context` 产出的是 `{"context":{"client":{…},"user":{…}}}`；搜索的 body 是
    // `{"context":{…}, "query": …}`，所以要的是第一层大括号**里面**那一份（结构相同、只切外壳，
    // 不重新序列化——重新序列化就会按字典序重排键，body 就不一样了）。
    let wrapped = {
        let mut s = String::with_capacity(384);
        push_context(&mut s, client, visitor);
        s
    };
    // 注意：`push_context` 只产出到"`context` 这个值闭合"为止，**最外层的 `{` 还没闭合**
    // （`browse_body` 会接着往里加 `browseId`，最后自己补 `}`）。所以这里只切前缀，不能顺手切后缀。
    wrapped
        .strip_prefix("{\"context\":")
        .map(str::to_string)
        .unwrap_or(wrapped)
}

/// 追加 `,"key":"value"`（搜索的 body 用；和 [`push_field`] 一样，只是名字更好读）。
pub(crate) fn push_quoted_field(s: &mut String, key: &str, value: &str) {
    push_field(s, key, value, true);
}

/// `context`（Kotlin `YoutubeApi.context`）：字段顺序逐条照抄，`browse` / `player` / `next` 共用。
///
/// 电视客户端照 MediaServiceCore 的 AppClient：要 `clientScreen` / 语言 / 地区，否则 `browse` 400；
/// 它们还要一个 `user` 块（`context` 的兄弟，不是 `client` 里的）。**`client` 只用一个 `}` 收尾**，
/// 写错会让 `user` 跑到根上（请求等价门抓到过一次）。
fn push_context(s: &mut String, client: Client, visitor: Option<&str>) {
    s.push_str("{\"context\":{\"client\":{");
    push_field(s, "clientName", client.name(), false);
    push_field(s, "clientVersion", client.version(), true);
    push_field(s, "hl", "en", true);
    push_field(s, "timeZone", "UTC", true);
    s.push_str(",\"utcOffsetMinutes\":0");
    if let Some(make) = client.device_make() {
        push_field(s, "deviceMake", make, true);
    }
    if let Some(model) = client.device_model() {
        push_field(s, "deviceModel", model, true);
    }
    if let Some(os) = client.os_name() {
        push_field(s, "osName", os, true);
    }
    if let Some(version) = client.os_version() {
        push_field(s, "osVersion", version, true);
    }
    push_field(s, "userAgent", client.user_agent(), true);
    if client.tv() {
        push_field(s, "clientScreen", "WATCH", true);
        push_field(s, "acceptLanguage", ACCEPT_LANGUAGE, true);
        push_field(s, "acceptRegion", "US", true);
    }
    if let Some(visitor) = visitor {
        push_field(s, "visitorData", visitor, true);
    }
    s.push('}');
    if client.tv() {
        s.push_str(",\"user\":{\"enableSafetyMode\":false,\"lockedSafetyMode\":false}");
    }
    s.push('}');
}

/// `player` 的 body（照 `YoutubeSource.playerWith` 的 `buildJsonObject`）。
fn player_body(client: Client, video_id: &str, sts: Option<i32>, visitor: Option<&str>) -> String {
    let mut s = String::with_capacity(512);
    push_context(&mut s, client, visitor);
    push_field(&mut s, "videoId", video_id, true);
    s.push_str(",\"contentCheckOk\":true");
    s.push_str(",\"racyCheckOk\":true");
    s.push_str(",\"playbackContext\":{\"contentPlaybackContext\":{\"html5Preference\":\"HTML5_PREF_WANTS\"");
    if let Some(sts) = sts {
        // 电视客户端：真实 5 位 sts（**不要补成 8 位**，补了第一个 Range 就 403）
        s.push_str(&format!(",\"signatureTimestamp\":{sts}"));
    }
    s.push_str("}}}");
    s
}

/// `context` 的 JSON 串，给同模块的 `account.rs` 拼 `accounts_list` 的 body（`whoami`）。
pub(crate) fn push_context_for(s: &mut String, client: Client, visitor: Option<&str>) {
    push_context(s, client, visitor);
}

/// `next` 的 body（照 `YoutubeSource.next`）：`{videoId}` 或 `{continuation}`，二选一。
fn next_body(target: &str, visitor: Option<&str>) -> String {
    let mut s = String::with_capacity(384);
    push_context(&mut s, Client::TvHtml5, visitor);
    if is_video_id(target) {
        push_field(&mut s, "videoId", target, true);
    } else {
        push_field(&mut s, "continuation", target, true);
    }
    s.push_str(",\"contentCheckOk\":true,\"racyCheckOk\":true}");
    s
}

/// Kotlin `videoIdOf` 里那个判断：11 个"字母 / 数字 / `-` / `_`"就是 videoId，否则当续页 token。
fn is_video_id(value: &str) -> bool {
    value.chars().count() == 11
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// `DetailTarget.Native.id` → videoId（照 `YoutubeSource.videoIdOf`：也接受完整网址）。
pub fn video_id_of(id: &str) -> Result<String, YoutubeError> {
    let trimmed = trim_java(id);
    if is_video_id(trimmed) {
        return Ok(trimmed.to_string());
    }
    if let Some(found) = capture_after(trimmed, "v=", true) {
        return Ok(found);
    }
    if let Some(found) = capture_after(trimmed, "youtu.be/", false) {
        return Ok(found);
    }
    Err(YoutubeError::Parse {
        detail: format!("不是 YouTube 视频 id：{id}"),
    })
}

/// `[?&]v=` / `youtu.be/` 后面那 11 个 `[\w-]`（Kotlin 的两个正则，`find` = 最左匹配）。
fn capture_after(text: &str, needle: &str, require_prefix: bool) -> Option<String> {
    for (at, _) in text.match_indices(needle) {
        if require_prefix {
            // 正则里 `needle` 前面还有一个 `[?&]`
            let ok = text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c == '?' || c == '&');
            if !ok {
                continue;
            }
        }
        let rest = &text[at + needle.len()..];
        let candidate: String = rest.chars().take(11).collect();
        if candidate.chars().count() == 11
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Some(candidate);
        }
    }
    None
}

/// Kotlin `String.take(n)`（按 UTF-16 码元；这里按字符，和 `detail.rs` 同一口径）。
fn take_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// 播放器脚本的 hash：`player\\?/([0-9a-fA-F]{8})\\?/`（Java 正则的"贪婪 + 回溯"照抄）。
fn find_player_hash(text: &str) -> Option<String> {
    for (start, _) in text.match_indices("player") {
        let rest = &text[start + "player".len()..];
        if let Some(hash) = hex8_after(rest) {
            return Some(hash);
        }
    }
    None
}

/// `\\?/` + 8 位 hex + `\\?/`（两个反斜杠都是可有可无的）。
fn hex8_after(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    for with_backslash in [true, false] {
        let mut i = 0usize;
        if with_backslash {
            if bytes.first() != Some(&b'\\') {
                continue;
            }
            i += 1;
        }
        if bytes.get(i) != Some(&b'/') {
            continue;
        }
        i += 1;
        let Some(window) = bytes.get(i..i + 8) else {
            continue;
        };
        if !window.iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let hash = String::from_utf8_lossy(window).into_owned();
        i += 8;
        for tail_backslash in [true, false] {
            let mut k = i;
            if tail_backslash {
                if bytes.get(k) != Some(&b'\\') {
                    continue;
                }
                k += 1;
            }
            if bytes.get(k) == Some(&b'/') {
                return Some(hash);
            }
        }
    }
    None
}

/// 播放器脚本里的 `(?:signatureTimestamp|sts)\s*:\s*([0-9]{5})`（照 Java 的从左到右 + 回溯）。
fn find_sts(text: &str) -> Option<i32> {
    for (at, _) in text.char_indices() {
        if !text[at..].starts_with('s') {
            continue;
        }
        for literal in ["signatureTimestamp", "sts"] {
            let Some(rest) = text[at..].strip_prefix(literal) else {
                continue;
            };
            // `\s*`
            let mut i = 0usize;
            while rest[i..].chars().next().is_some_and(is_ascii_regex_ws) {
                i += rest[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            }
            if !rest[i..].starts_with(':') {
                continue;
            }
            i += 1;
            while rest[i..].chars().next().is_some_and(is_ascii_regex_ws) {
                i += rest[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            }
            let digits: String = rest[i..].chars().take(5).collect();
            if digits.chars().count() == 5 && digits.chars().all(|c| c.is_ascii_digit()) {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// `playabilityStatus` 的映射（照 `YoutubeApi.checkPlayability`）。
///
/// 年龄限制 → [`YoutubeError::AgeRestricted`]（详情那边转成"18+ 隐藏"）；"确认你不是机器人" → 风控；
/// 要登录 → `NotLoggedIn`；`ERROR` / `UNPLAYABLE` / 别的 → `Api`。**文案逐字照 Kotlin。**
pub fn check_playability(json: &JsonMap) -> Result<(), YoutubeError> {
    let Some(ps) = json::obj(json, "playabilityStatus") else {
        return Ok(());
    };
    let Some(status) = text_of(ps, "status") else {
        return Ok(());
    };
    if status == "OK" {
        return Ok(());
    }
    let reason = text_of(ps, "reason").unwrap_or_default();
    if detail::is_age_restricted(json) {
        return Err(YoutubeError::AgeRestricted {
            detail: format!("YouTube 标了年龄限制（18+）：{reason}"),
        });
    }
    let lower = reason.to_lowercase();
    if lower.contains("bot") || lower.contains("not a robot") || (status == "LOGIN_REQUIRED" && lower.contains("confirm")) {
        return Err(YoutubeError::RiskControl {
            code: status,
            detail: format!("要确认不是机器人：{reason}"),
        });
    }
    match status.as_str() {
        "LOGIN_REQUIRED" => Err(YoutubeError::NotLoggedIn {
            detail: Some(format!("YouTube 要求登录：{reason}")),
        }),
        "ERROR" => Err(YoutubeError::Api {
            code: status,
            detail: if is_blank(&reason) { "服务端错误".to_string() } else { reason },
        }),
        "UNPLAYABLE" => Err(YoutubeError::Api {
            code: status,
            detail: if is_blank(&reason) { "这个视频不可播放".to_string() } else { reason },
        }),
        other => Err(YoutubeError::Api {
            code: other.to_string(),
            detail: if is_blank(&reason) {
                format!("播放状态 {other}")
            } else {
                reason
            },
        }),
    }
}

/// `text(map, key)` 的本地别名（`json::text` 接收者是 `&JsonMap`，写起来短一点）。
fn text_of(map: &JsonMap, key: &str) -> Option<String> {
    json::text(map, key)
}

// ===========================================================================
// 详情 / 评论 / 楼中楼
// ===========================================================================

impl YoutubeHostedSource {
    /// 一次 InnerTube POST：`?prettyPrint=false`、头顺序、状态码判定、匿名身份回写，全走这里。
    fn post(
        &self,
        path: &str,
        client: Client,
        body: &str,
        auth: Option<&str>,
    ) -> Result<JsonMap, YoutubeError> {
        let visitor = self.visitor();
        // Kotlin：`authorized && client.tv` 才写 `Authorization`（匿名客户端永远不带 bearer）
        let auth = if client.tv() {
            auth.filter(|t| !t.is_empty())
        } else {
            None
        };
        let url = format!("{}{path}?prettyPrint=false", self.base.trim_end_matches('/'));
        let response = self
            .http
            .execute(HttpRequest {
                method: "POST".to_string(),
                url,
                headers: post_headers(client, visitor.as_deref(), auth),
                body: body.as_bytes().to_vec(),
            })
            .map_err(YoutubeError::from)?;
        let text = String::from_utf8_lossy(&response.body).into_owned();
        check_transport(response.status, &text)?;
        let map = json::parse_object(&text).map_err(|detail| YoutubeError::Parse { detail })?;
        self.remember_visitor(&map);
        Ok(map)
    }

    /// 取一段文本（播放器脚本那两跳；**只带 User-Agent**，和 Kotlin `YoutubePlayerScript.get` 一样）。
    fn get_text(&self, url: &str) -> Result<String, YoutubeError> {
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: url.to_string(),
                headers: vec![("User-Agent".to_string(), VISIONOS_USER_AGENT.to_string())],
                body: Vec::new(),
            })
            .map_err(YoutubeError::from)?;
        if !(200..=299).contains(&response.status) {
            return Err(YoutubeError::Api {
                code: format!("HTTP {}", response.status),
                detail: format!("取播放器脚本失败：{url}"),
            });
        }
        Ok(String::from_utf8_lossy(&response.body).into_owned())
    }

    /// 真实 5 位 `signatureTimestamp`（`YoutubePlayerScript.get(needJs = false)`）。
    ///
    /// 缓存（hash / sts / 取的时间）在 `ctx.store` 里，**键和 Kotlin 一字不差**：播放那边是另一份
    /// `YoutubePlayerScript` 实例，两边共用同一份缓存（6 小时内不再请求）。只需要 sts 时不下载脚本全文。
    pub fn player_script_sts(&self, now_ms: i64) -> Result<Option<i32>, YoutubeError> {
        let stored_hash = self.store.get(KEY_PLAYER_HASH);
        let at = self
            .store
            .get(KEY_PLAYER_AT)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if stored_hash.is_some() && now_ms.saturating_sub(at) < STS_TTL_MS {
            return Ok(self
                .store
                .get(KEY_PLAYER_STS)
                .and_then(|v| v.parse::<i32>().ok()));
        }
        let api = self.get_text(&format!(
            "{}{PATH_IFRAME_API}",
            self.base.trim_end_matches('/')
        ))?;
        let hash = find_player_hash(&api).ok_or_else(|| YoutubeError::Api {
            code: "no-player-hash".to_string(),
            detail: "iframe_api 里找不到播放器 hash（改版了）".to_string(),
        })?;
        self.fetch_player_js(&hash, now_ms)
    }

    /// 按已知 hash 取播放器脚本并抽出 sts（`YoutubePlayerScript.fetchJs`）。
    fn fetch_player_js(&self, hash: &str, now_ms: i64) -> Result<Option<i32>, YoutubeError> {
        let url = format!(
            "{}{PLAYER_SCRIPT_PATH}/{hash}/player_ias.vflset/en_US/base.js",
            self.base.trim_end_matches('/')
        );
        let js = self.get_text(&url)?;
        let sts = find_sts(&js);
        self.store.put(KEY_PLAYER_HASH, Some(hash));
        match sts {
            Some(value) => self.store.put(KEY_PLAYER_STS, Some(&value.to_string())),
            None => self.store.put(KEY_PLAYER_STS, None),
        }
        self.store.put(KEY_PLAYER_AT, Some(&now_ms.to_string()));
        Ok(sts)
    }

    // ---------- 客户端健康（Kotlin `YoutubeClientHealth`）----------

    fn health_read(&self, client: Client) -> (i64, i64) {
        let Some(raw) = self.store.get(&client.health_key()) else {
            return (0, 0);
        };
        let Ok(map) = json::parse_object(&raw) else {
            return (0, 0);
        };
        (
            json::long(&map, "fails").unwrap_or(0),
            json::long(&map, "until").unwrap_or(0),
        )
    }

    fn health_usable(&self, client: Client, now_ms: i64) -> bool {
        let (fails, until) = self.health_read(client);
        fails < HEALTH_MAX_FAILS || until <= now_ms
    }

    fn health_success(&self, client: Client) {
        self.store.put(&client.health_key(), None);
    }

    fn health_failure(&self, client: Client, now_ms: i64) {
        let (fails, _) = self.health_read(client);
        let n = fails + 1;
        let until = if n >= HEALTH_MAX_FAILS {
            now_ms + HEALTH_COOLDOWN_MS
        } else {
            0
        };
        self.store.put(
            &client.health_key(),
            Some(&format!("{{\"fails\":{n},\"until\":{until}}}")),
        );
    }

    // ---------- 详情 ----------

    /// **详情**（Kotlin `YoutubeSource.detail`）：`player` 为准 + TV `next {videoId}` 补空的。
    ///
    /// 顺序和 Kotlin 逐条对齐：
    /// 1. `player`（登录态先 `TV_TIZEN`，失败回退匿名 `VISIONOS`；连续失败进冷却）；
    /// 2. `next {videoId}`（和评论第一跳共用、按 videoId 缓存）拿标题 / 作者 / 头像 / 简介 / 播放数 / 发布时间；
    /// 3. 登录态 TV 的 `player` 是新形状（缺 `title` / `author` / `shortDescription` / `viewCount`），
    ///    这时用 `next` 补；`next` 自己失败（网络 / 形状变了 / 风控）就降级，**不让整页失败**；
    /// 4. `next` 也补不上（形状变了 / 被风控）且不是匿名响应 → 再用匿名 `VISIONOS` 的 `player` 补**一次**。
    ///    风控过了的视频**不再发**（红线：遇到就停）。
    ///
    /// 年龄限制**不报错**：转成 `item.adult = true` 的隐藏详情（正文 / 播放信息都不给）。
    pub fn detail(
        &self,
        id: &str,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
    ) -> Result<FeedDetail, YoutubeError> {
        self.detail_with_clock(id, now_ms / 1000, now_ms, session, token)
    }

    fn detail_with_clock(
        &self,
        id: &str,
        now_sec: i64,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
    ) -> Result<FeedDetail, YoutubeError> {
        let video_id = video_id_of(id)?;
        let (client, response) = match self.player(&video_id, now_ms, session, token) {
            Ok(pair) => pair,
            Err(YoutubeError::AgeRestricted { .. }) => return Ok(detail::hidden_detail(&video_id)),
            Err(e) => return Err(e),
        };
        let mut video = detail::video(&response);
        let mut blocked = false;
        let meta = match self.watch_info_with_clock(&video_id, now_sec, token) {
            Ok(info) => info.meta,
            Err(YoutubeError::RiskControl { .. }) => {
                // 风控过的视频连"补一次匿名 player"也不再发（红线）
                blocked = true;
                None
            }
            // 别的失败（网络 / 500 / 形状变了）都只是降级，不往上报
            Err(_) => None,
        };
        video = detail::merge_meta(video, meta.as_ref());
        if (video.title.is_none() || video.author.is_none())
            && client != Client::Visionos
            && !blocked
        {
            video = self.fill_from_anonymous(&video_id, video, now_ms);
        }
        Ok(FeedDetail {
            item: detail::item_of(&video, meta.as_ref()),
            body: detail::body_of(&video),
            video: Some(detail::video_info(&video)),
            follow: None,
            related: Vec::new(),
        })
    }

    /// 详情降级：匿名 `VISIONOS` 的 `player` 里有完整的 `videoDetails`，拿它补缺的字段。
    /// **失败就算了**（年龄限制 / 机器人验证 / 网络），不重试、不记冷却、不抛给调用方。
    fn fill_from_anonymous(
        &self,
        video_id: &str,
        video: detail::YoutubeVideo,
        now_ms: i64,
    ) -> detail::YoutubeVideo {
        match self.player_with(Client::Visionos, video_id, now_ms, None) {
            Ok(json) => detail::fill_missing(video, &detail::video(&json)),
            Err(_) => video,
        }
    }

    /// **详情用的** `player`（Kotlin `YoutubeSource.player`）：按客户端链试，风控 / 年龄限制直接抛。
    ///
    /// 返回"用哪个客户端拿到的"，详情降级要判断"是不是已经是匿名响应了"。
    /// **和 Kotlin 逐条对齐的一点**：除了风控 / 年龄限制，**别的错误都算"这个客户端失败了、换下一个"**
    /// ——包括网络错误和"代理不可用"（Kotlin 的 `catch (e: Exception)` 就是这么做，`ProxyUnavailableException`
    /// 也是 `Exception`）。所以客户端链全失败时抛的是 `Api { all-clients-failed }`，不是原来那个网络错误；
    /// 而 `feed` / `comments` / `replies` 没有这条链，网络错误 / fail-closed 原样带回去（cli 的退出码 6 靠它）。
    fn player(
        &self,
        video_id: &str,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
    ) -> Result<(Client, JsonMap), YoutubeError> {
        let mut failures: Vec<String> = Vec::new();
        for client in client_chain(session, |c| self.health_usable(c, now_ms)) {
            match self.player_with(client, video_id, now_ms, token) {
                Ok(json) => {
                    self.health_success(client);
                    return Ok((client, json));
                }
                // 风控要立刻停；年龄限制是内容本身的属性（详情那边转成 18+ 隐藏）
                Err(e @ YoutubeError::AgeRestricted { .. })
                | Err(e @ YoutubeError::RiskControl { .. }) => return Err(e),
                Err(e) => {
                    failures.push(format!("{}：{e}", client.name()));
                    self.health_failure(client, now_ms);
                }
            }
        }
        Err(YoutubeError::Api {
            code: "all-clients-failed".to_string(),
            detail: format!("YouTube 的客户端都失败了：{}", failures.join("；")),
        })
    }

    /// 用某个客户端发一次 `player`（`decode = false`：**不为了拿元数据去解 n/sig**）。
    fn player_with(
        &self,
        client: Client,
        video_id: &str,
        now_ms: i64,
        token: Option<&str>,
    ) -> Result<JsonMap, YoutubeError> {
        // 电视客户端要真实 5 位 sts（取不到 / hash 变了就换下一个客户端）
        let sts = if client == Client::TvTizen {
            self.player_script_sts(now_ms)?
        } else {
            None
        };
        let visitor = self.visitor();
        let body = player_body(client, video_id, sts, visitor.as_deref());
        let auth = if client == Client::TvTizen { token } else { None };
        let json = self.post(PATH_PLAYER, client, &body, auth)?;
        check_playability(&json)?;
        Ok(json)
    }

    // ---------- 评论 / 楼中楼 ----------

    /// 一级评论（Kotlin `YoutubeSource.comments`）。**两跳**：先 `next {videoId}` 拿到评论面板的续页
    /// token（按 videoId 缓存，详情页已经替它发过了），再 `next {continuation: token}` 拿第一页；
    /// 续页直接用上一页给回来的游标。
    ///
    /// 楼中楼的 token 在每条评论的 `detailViewEndpoint` 里，解析时记进缓存，[`Self::replies`] 用得到。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_ms: i64,
        token: Option<&str>,
    ) -> Result<CommentPage, YoutubeError> {
        self.comments_with_clock(id, cursor, sort, now_ms / 1000, token)
    }

    fn comments_with_clock(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_sec: i64,
        token: Option<&str>,
    ) -> Result<CommentPage, YoutubeError> {
        let video_id = video_id_of(id)?;
        let continuation = match cursor {
            Some(cursor) => cursor.to_string(),
            None => self
                .watch_info_with_clock(&video_id, now_sec, token)?
                .token_for(sort)
                .ok_or_else(|| YoutubeError::Api {
                    code: "no-comments-token".to_string(),
                    detail: "这个视频的 next 响应里没有评论面板".to_string(),
                })?,
        };
        self.next_page(&continuation, now_sec, token, None)
    }

    /// 楼中楼（Kotlin `YoutubeSource.replies`）。游标优先（上一页给的）；没有游标就用解析一级评论时
    /// 记下的 token；缓存没命中（比如刚启动 App 直接点开楼中楼）就先取一次一级评论把它填上
    /// ——多花一个请求，但只在第一次。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        token: Option<&str>,
    ) -> Result<CommentPage, YoutubeError> {
        self.replies_with_clock(id, comment_id, cursor, now_ms / 1000, token)
    }

    fn replies_with_clock(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_sec: i64,
        token: Option<&str>,
    ) -> Result<CommentPage, YoutubeError> {
        let mut continuation = cursor
            .map(|c| c.to_string())
            .or_else(|| self.replies_token(comment_id));
        if continuation.is_none() {
            self.comments_with_clock(id, None, CommentSort::Default, now_sec, token)?;
            continuation = self.replies_token(comment_id);
        }
        let Some(continuation) = continuation else {
            // Kotlin：`if (token == null) return Page(emptyList(), null)`
            return Ok(CommentPage::default());
        };
        self.next_page(&continuation, now_sec, token, Some(comment_id))
    }

    fn replies_token(&self, comment_id: &str) -> Option<String> {
        self.replies_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(comment_id)
            .cloned()
    }

    /// 发一跳 `next {continuation}` 并解析（一级评论和楼中楼共用）。
    fn next_page(
        &self,
        continuation: &str,
        now_sec: i64,
        token: Option<&str>,
        parent_id: Option<&str>,
    ) -> Result<CommentPage, YoutubeError> {
        let json = self.next_request(continuation, token)?;
        let (page, tokens) = detail::comments(&json, now_sec, parent_id);
        {
            let mut cache = self
                .replies_tokens
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (id, value) in tokens {
                cache.insert(id, value);
            }
        }
        Ok(page)
    }

    /// 一次 `next`（`{videoId}` 或 `{continuation}`，二选一）。
    fn next_request(
        &self,
        target: &str,
        token: Option<&str>,
    ) -> Result<JsonMap, YoutubeError> {
        let visitor = self.visitor();
        let body = next_body(target, visitor.as_deref());
        // `authorized = true`（电视客户端 + 有 token 才真的带 bearer）
        self.post(PATH_NEXT, Client::TvHtml5, &body, token)
    }

    /// `next {videoId}`（TVHTML5，评论两跳的第一跳）**发一次、两处用**：详情的元数据 + 评论面板游标。
    ///
    /// 同一个 videoId 只发一次（锁挡住详情 / 评论同时加载时的并发）；拿到了就缓存。**风控过的视频当前
    /// 不再问**：详情那边降级，评论那边照实抛同一个错误（红线：遇到就停，不重试）。
    fn watch_info_with_clock(
        &self,
        video_id: &str,
        now_sec: i64,
        token: Option<&str>,
    ) -> Result<WatchInfo, YoutubeError> {
        let mut cache = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(info) = cache.infos.get(video_id) {
            return Ok(info.clone());
        }
        if let Some(e) = cache.risks.get(video_id) {
            return Err(e.clone());
        }
        // 锁一直拿着：详情和评论可能在不同线程上同时来，同一个视频只允许一个 next 在飞
        let json = match self.next_request(video_id, token) {
            Ok(json) => json,
            Err(e) => {
                if matches!(e, YoutubeError::RiskControl { .. }) {
                    cache.risks.insert(video_id.to_string(), e.clone());
                }
                return Err(e);
            }
        };
        let meta = detail::video_meta(&json, now_sec);
        let mut tokens = Vec::new();
        for sort in [CommentSort::Default, CommentSort::Hot, CommentSort::Newest] {
            if let Some(token) = detail::comments_token(&json, sort) {
                tokens.push((sort, token));
            }
        }
        let info = WatchInfo { meta, tokens };
        cache.infos.insert(video_id.to_string(), info.clone());
        Ok(info)
    }
}

/// 详情 / 评论用的客户端顺序（Kotlin `YoutubeSource.clientChain`）：未登录只有匿名 `VISIONOS`；
/// 登录了先 `TV_TIZEN`。冷却里的客户端先跳过，**全在冷却里就照原顺序试**。
fn client_chain(session: bool, usable: impl Fn(Client) -> bool) -> Vec<Client> {
    let order: Vec<Client> = if session {
        vec![Client::TvTizen, Client::Visionos]
    } else {
        vec![Client::Visionos]
    };
    let filtered: Vec<Client> = order.iter().copied().filter(|c| usable(*c)).collect();
    if filtered.is_empty() {
        order
    } else {
        filtered
    }
}

/// 平台 id（测试里断言"Rust 侧记住的就是这个平台"）。
pub const PLATFORM: &str = YOUTUBE;

// ===========================================================================
// 播放取流
// ===========================================================================

impl YoutubeHostedSource {
    /// **取流**（Kotlin `YoutubeSource.play`）：客户端回退链（**先匿名 `VISIONOS`、再登录态 `TV_TIZEN`**）
    /// + `player` 请求 + `adaptiveFormats` 解析 + 健康冷却，外加 n/sig 的**整批**委托。
    ///
    /// 回退顺序的理由：登录态 TVHTML5 5.x/Tizen
    /// 回来的分片地址被 CDN 直接拒（`SegmentBase` 的 `indexRange` 第一个 Range 请求就 403、空 body），
    /// 同一个视频 / 同一个字节范围换成匿名 VISIONOS 的地址就是 206。所以**取流**先问不需要 PO Token、
    /// 也不需要解 n/sig 的匿名客户端；它给不出能播的流（儿童 / 年龄限制 / 账号受限）或者请求本身失败时，
    /// 才回退登录态客户端。
    ///
    /// "这个客户端没给出能播的流"也算它失败（换下一个）：响应可以是 200 但一条带地址的格式都没有。
    ///
    /// 那时是直接抛，登录态客户端根本没轮到。
    /// 现在分三种（和 Kotlin 逐条一致）：
    /// 1. 有登录态：匿名客户端撞这种风控时，**换成登录态 `TV_TIZEN` 再问一次**（YouTube 给的办法就是登录，
    ///    属于换成用户本人的身份，不是"换参数绕风控"）；
    /// 2. 登录态客户端**也被拦**：立刻抛风控，不换第三个、不重试；
    /// 3. 没登录：抛 `NotLoggedIn`，界面据此给"登录 YouTube 后再播"+ 直接打开设备码登录。
    ///
    /// [`script`] 是宿主的 JS 求解端口（**n/sig 的求解在 Kotlin 的 QuickJS 里**）：要解的挑战**整批**
    /// 交出去、一次拿回结果，不一条格式过一次边界；宿主说"解不了"就当这个客户端失败了换下一个
    /// （和 Kotlin 的 `catch (e: Exception)` 一样）。`None` = 宿主没提供这个能力（只可能出现在测试里，
    /// 那样带 cipher 的响应会被当成"这个客户端失败"）。
    ///
    /// [`refresh_failure`] 是 Kotlin 那边"token 过期了、换一次没换到"的那个错误（`Option<String>` 只带
    /// **人话文案**，不带凭据）：匿名客户端被要求确认不是机器人、正要换登录态客户端时，Kotlin 原版会把这个
    /// 错误抛出来（避免拿一个没有 bearer 的"登录态客户端"再撞一次）——这里照办。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        quality: Option<i32>,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
        script: Option<&dyn ScriptHost>,
        refresh_failure: Option<&str>,
    ) -> Result<PlayInfo, YoutubeError> {
        let err = refresh_failure.map(|message| YoutubeError::Api {
            code: "token-refresh-failed".to_string(),
            detail: message.to_string(),
        });
        self.play_internal(
            id,
            part_id,
            quality,
            now_ms,
            session,
            token,
            script,
            err.as_ref(),
        )
    }

    pub fn play_internal(
        &self,
        id: &str,
        part_id: &str,
        quality: Option<i32>,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
        script: Option<&dyn ScriptHost>,
        refresh_failure: Option<&YoutubeError>,
    ) -> Result<PlayInfo, YoutubeError> {
        if part_id != detail::MAIN_PART {
            return Err(YoutubeError::Parse {
                detail: format!(
                    "YouTube 只有一条流（partId 应为 {}，收到 {part_id}）",
                    detail::MAIN_PART
                ),
            });
        }
        let video_id = video_id_of(id)?;
        let mut failures: Vec<String> = Vec::new();
        let mut login_required = false;
        // 队尾可能临时插入登录态客户端（匿名客户端被要求确认不是机器人时），见下面的 catch
        let mut queue: Vec<Client> = playback_chain(session, |c| self.health_usable(c, now_ms));
        while !queue.is_empty() {
            let client = queue.remove(0);
            match self.play_with(client, &video_id, quality, now_ms, session, token, script) {
                Ok(info) if !info.video.is_empty() || !info.progressive.is_empty() => {
                    self.health_success(client);
                    return Ok(info);
                }
                Ok(_) => {
                    failures.push(format!("{}：响应里没有能播的流", client.name()));
                    self.health_failure(client, now_ms);
                }
                // 年龄限制是内容本身的属性：直接抛（不换客户端）
                Err(e @ YoutubeError::AgeRestricted { .. }) => return Err(e),
                Err(YoutubeError::NotLoggedIn { detail }) => {
                    login_required = true;
                    failures.push(format!(
                        "{}：{}",
                        client.name(),
                        detail.as_deref().unwrap_or("未登录或登录已失效")
                    ));
                    self.health_failure(client, now_ms);
                }
                Err(e @ YoutubeError::RiskControl { .. }) => {
                    self.health_failure(client, now_ms);
                    if is_bot_check(&e) {
                        // 匿名客户端在机房出口上被要求"确认不是机器人"时，有登录态就换成用户自己的
                        // 登录态客户端再问一次（YouTube 给的办法就是登录，不是换参数绕风控）。
                        if session && client == Client::Visionos {
                            // token 刚才没换到：登录态客户端这时也不带 bearer，再问只会再撞一次
                            if let Some(err) = refresh_failure {
                                return Err(err.clone());
                            }
                            failures.push(format!("{}：要确认不是机器人，换登录态客户端再试", client.name()));
                            if !queue.contains(&Client::TvTizen) {
                                queue.insert(0, Client::TvTizen);
                            }
                            continue;
                        }
                        // 没登录：这是"登录后再播"能解决的情形，抛可操作的那一种
                        if !session {
                            return Err(YoutubeError::NotLoggedIn {
                                detail: Some("YouTube 要确认不是机器人：登录后再播".to_string()),
                            });
                        }
                    }
                    return Err(e);
                }
                Err(e) => {
                    failures.push(format!("{}：{e}", client.name()));
                    self.health_failure(client, now_ms);
                }
            }
        }
        if login_required {
            return Err(YoutubeError::NotLoggedIn {
                detail: Some("YouTube 要求登录：登录后再播这个视频".to_string()),
            });
        }
        Err(YoutubeError::Api {
            code: "all-clients-failed".to_string(),
            detail: format!("YouTube 的客户端都没给出能播的流：{}", failures.join("；")),
        })
    }

    /// 用某个客户端取一次流：`player`（TV 客户端要真实 5 位 sts）→ 可播性判定 →
    /// **需要解 n/sig 就整批交给宿主** → [`play::play_info`]。
    ///
    /// `decode` 这一层比详情那条路多两件事（详情只要元数据，**不为了元数据解 n/sig**）：
    /// 1. 真的把播放器脚本全文取下来（`player_script(need_js = true)`）；
    /// 2. 把 `sig` / `n` 的挑战整批交给宿主，答案一次拼回所有 URL。
    ///
    /// 取不到脚本全文时抛 `no-player-js`（和 Kotlin 的 `ApiException("no-player-js")` 一样）——
    /// 这是"这个客户端失败了"，外层的循环会换下一个。
    fn play_with(
        &self,
        client: Client,
        video_id: &str,
        quality: Option<i32>,
        now_ms: i64,
        session: bool,
        token: Option<&str>,
        script: Option<&dyn ScriptHost>,
    ) -> Result<PlayInfo, YoutubeError> {
        let mut json = self.player_with(client, video_id, now_ms, token)?;
        if !play::needs_solving(&json) {
            return play::play_info(&json, quality);
        }
        // 需要解 n/sig：把播放器脚本全文取下来（每个播放器 hash 一次），再由宿主跑 JS
        let fetched = self.player_script(now_ms, true)?;
        let Some(js) = fetched.js else {
            return Err(YoutubeError::Api {
                code: "no-player-js".to_string(),
                detail: "拿不到播放器脚本，解不了 n/sig".to_string(),
            });
        };
        let Some(host) = script else {
            return Err(YoutubeError::Api {
                code: "no-player-js".to_string(),
                detail: "没有 JS 求解宿主，解不了 n/sig".to_string(),
            });
        };
        let requests: Vec<(String, Vec<String>)> = play::solve_requests(&json)
            .into_iter()
            .map(|r| (r.kind.to_string(), r.challenges))
            .collect();
        let answers = host.solve(&js, &fetched.hash, requests);
        let answers: HashMap<String, String> = answers.into_iter().collect();
        play::apply_answers(&mut json, &answers);
        // `session` 在这里不再需要（token 已经决定带没带），留着参数只是为了和 Kotlin 的调用顺序对得上
        let _ = session;
        play::play_info(&json, quality)
    }

    /// 取播放器脚本（`YoutubePlayerScript.get`）。
    ///
    /// [`need_js`] = false 时只要 sts：store 里有没到期的缓存就直接用，**不产生请求**；
    /// true 时要脚本全文，hash 还是新的就按它取（**不用再问一次 `/iframe_api`**），
    /// 取失败就整个重来一次（`fetch()`）。
    pub fn player_script(&self, now_ms: i64, need_js: bool) -> Result<PlayerScript, YoutubeError> {
        let stored_hash = self.store.get(KEY_PLAYER_HASH);
        let at = self
            .store
            .get(KEY_PLAYER_AT)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let fresh = stored_hash.is_some() && now_ms.saturating_sub(at) < STS_TTL_MS;
        if fresh {
            let hash = stored_hash.clone().unwrap_or_default();
            if let Some(hit) = self.memory_script(&hash, need_js) {
                return Ok(hit);
            }
            let sts = self
                .store
                .get(KEY_PLAYER_STS)
                .and_then(|v| v.parse::<i32>().ok());
            // 只需要 sts：store 里的缓存够用，不发请求
            if !need_js {
                return Ok(PlayerScript {
                    hash,
                    sts,
                    js: None,
                });
            }
            // 需要脚本全文：hash 还是新的，直接按它取，**不用再问一次 /iframe_api**
            match self.fetch_player_script(&hash, now_ms) {
                Ok(fetched) if fetched.sts.is_some() => return Ok(fetched),
                Ok(fetched) => {
                    return Ok(PlayerScript {
                        hash: fetched.hash,
                        sts: fetched.sts.or(sts),
                        js: fetched.js,
                    })
                }
                Err(_) => return self.fetch_player_script_by_iframe(now_ms),
            }
        }
        self.fetch_player_script_by_iframe(now_ms)
    }

    /// `/iframe_api` → hash → base 脚本（`YoutubePlayerScript.fetch`）。
    fn fetch_player_script_by_iframe(&self, now_ms: i64) -> Result<PlayerScript, YoutubeError> {
        let api = self.get_text(&format!(
            "{}{PATH_IFRAME_API}",
            self.base.trim_end_matches('/')
        ))?;
        let hash = find_player_hash(&api).ok_or_else(|| YoutubeError::Api {
            code: "no-player-hash".to_string(),
            detail: "iframe_api 里找不到播放器 hash（改版了）".to_string(),
        })?;
        self.fetch_player_script(&hash, now_ms)
    }

    /// 进程内的脚本缓存（`YoutubePlayerScript.memory`：一次播放会话里只用下载一次全文）。
    fn memory_script(&self, hash: &str, need_js: bool) -> Option<PlayerScript> {
        let cache = self.scripts.lock().expect("scripts 锁被污染");
        let hit = cache.get(hash)?;
        if need_js && hit.js.is_none() {
            return None;
        }
        Some(hit.clone())
    }

    fn remember_script(&self, script: &PlayerScript) {
        self.scripts
            .lock()
            .expect("scripts 锁被污染")
            .insert(script.hash.clone(), script.clone());
    }

    /// 按已知 hash 取播放器脚本并抽出 sts（`YoutubePlayerScript.fetchJs`）：**三个缓存键一起写**
    /// （`youtube.player_hash` / `youtube.player_sts` / `youtube.player_at`，和 Kotlin 共用同一份）。
    fn fetch_player_script(&self, hash: &str, now_ms: i64) -> Result<PlayerScript, YoutubeError> {
        let url = format!(
            "{}{PLAYER_SCRIPT_PATH}/{hash}/player_ias.vflset/en_US/base.js",
            self.base.trim_end_matches('/')
        );
        let js = self.get_text(&url)?;
        let sts = find_sts(&js);
        self.store.put(KEY_PLAYER_HASH, Some(hash));
        match sts {
            Some(value) => self.store.put(KEY_PLAYER_STS, Some(&value.to_string())),
            None => self.store.put(KEY_PLAYER_STS, None),
        }
        self.store.put(KEY_PLAYER_AT, Some(&now_ms.to_string()));
        let script = PlayerScript {
            hash: hash.to_string(),
            sts,
            js: Some(js),
        };
        self.remember_script(&script);
        Ok(script)
    }

    // ---------- 账号层 / FFI 门面要用的口子 ----------

    /// 登录端点的四个地址（`LoginUrls`）。
    pub(crate) fn login_urls(&self) -> &crate::youtube::account::LoginUrls {
        &self.login_urls
    }

    /// 匿名身份（`visitor_data`），给 `whoami` / 账号层拼 `context` 用。
    pub(crate) fn visitor_public(&self) -> Option<String> {
        self.visitor()
    }

    /// 把抓到的 `visitorData` 写回 store（账号层抓 `/tv` 页面那一跳）。
    pub(crate) fn store_put_visitor(&self, visitor: &str) {
        self.store.put(KEY_VISITOR_DATA, Some(visitor));
    }

    /// 一次 InnerTube POST，给同模块的 `account.rs` 用（`whoami` 的 `accounts_list`）。
    pub(crate) fn post_public(
        &self,
        path: &str,
        client: Client,
        body: &str,
        auth: Option<&str>,
    ) -> Result<JsonMap, YoutubeError> {
        self.post(path, client, body, auth)
    }

}

/// 播放器脚本（Kotlin `YoutubePlayerScript.Script`）：`js` 只有"要解 n/sig"时才取全文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerScript {
    pub hash: String,
    pub sts: Option<i32>,
    pub js: Option<String>,
}

/// 是不是"**要确认你不是机器人**"这种风控（`YoutubeApi.isBotCheck`：只有 status = `LOGIN_REQUIRED`
/// 才算；传输层的 429 / 403 / 412 / 空 body 的 code 是另一套）。
///
/// 只有这一种风控允许"换成用户自己的登录态再问一次"：YouTube 自己给的办法就是登录。
pub fn is_bot_check(e: &YoutubeError) -> bool {
    matches!(e, YoutubeError::RiskControl { code, .. } if code == "LOGIN_REQUIRED")
}

/// **取流**用的客户端顺序（Kotlin `YoutubeSource.playbackChain`）：先匿名 `VISIONOS`，登录了再拿
/// `TV_TIZEN` 兜底（理由见 [`YoutubeHostedSource::play`]）。冷却里的客户端先跳过，**全在冷却里就照原顺序**。
fn playback_chain(session: bool, usable: impl Fn(Client) -> bool) -> Vec<Client> {
    let order: Vec<Client> = if session {
        vec![Client::Visionos, Client::TvTizen]
    } else {
        vec![Client::Visionos]
    };
    let filtered: Vec<Client> = order.iter().copied().filter(|c| usable(*c)).collect();
    if filtered.is_empty() {
        order
    } else {
        filtered
    }
}

