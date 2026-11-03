//! **`HttpHost` 的 Rust 实现**：平台对象（`feedhub-core` 里那 11 家）**不过 FFI**，直接在 Rust 内部
//! 用 [`feedhub_net::NetClient`] 发请求。
//!
//! # 为什么在 Rust 内部实现，而不是让 Kotlin 实现
//!
//! 现在的形状是"Rust 拼请求 → 回调宿主（`HostHttp` → OkHttp）→ 把响应带回来"。切过来之后是
//! "Rust 拼请求 → `NetClient` 发 → 把响应带回来"：**平台层一行不用改**（它们只认识
//! `feedhub_core::host::HttpHost`），只是把这个 trait 的实现从 `host.rs` 的 [`crate::host::HttpAdapter`]
//! 换成这个模块的 [`NetHttpHost`]。省掉的是一整趟 UniFFI 往返 + 一次 OkHttp 内部的
//! `Dispatcher` 线程切换。
//!
//! # 顺序：和 OkHttp 的拦截器链逐条对齐
//!
//! ```text
//! onRequest 计数 → 代理守卫（fail-closed）→ 默认头 → 限速等待 → [X 的 txid] → 连接
//! ```
//!
//! 具体落在哪儿：
//!
//! | 拦截器（Kotlin） | 这一层 |
//! |---|---|
//! | `onRequest` 计数（`SourceContext.client` 第一个） | 宿主门面自己数（`NetRequest` 上有 `gate` 口子，这一层不装） |
//! | `ProxyConfigGuardInterceptor` | [`feedhub_net::ProxyPlan`] 的 `Unavailable`（**在 DNS / TCP 之前**） |
//! | `DefaultHeadersInterceptor` | [`feedhub_net::NetRequest::extra_default_headers`] |
//! | `MinIntervalInterceptor` | [`PlatformLimiter`]（[`feedhub_net::LimiterHost`]） |
//! | 各平台的签名拦截器（X 的 txid） | [`feedhub_net::NetRequest::after_limit`]（**睡完了再算**） |
//! | Cookie 罐（OkHttp 的 `CookieJar`） | [`JarCookies`]（`feedhub-core/src/cookies.rs`） |
//! | `followRedirects(false)`（酷安 `biliLink` / 贴吧扫码） | [`feedhub_net::NetRequest::follow_redirects`] |
//! | `readTimeout(...)`（贴吧 `unicast` 长轮询） | [`feedhub_net::NetRequest::read_timeout`] |
//! | `X-Feedhub-Priority` 伪请求头 | [`PRIORITY_HEADER`]（**读出来就删掉，绝不写到线上**） |
//!
//! # 红线
//!
//! - **拿到响应就不重试**（`feedhub-net` 的 `connect_only_retry` 守着），403 / 412 / 429 原样交给平台层；
//! - 代理不可用 / 冷却中 → **一个字节都不发**（`NetClient::execute` 的前两步）；
//! - Cookie 值、token、签名、完整 URL 的 query **不进日志、不进错误文案**。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use feedhub_core::cookies::{self, CookieJar, HttpUrl as JarUrl, StoredCookie};
use feedhub_core::host::{HostError, HttpHost, HttpRequest, HttpResponse};
use feedhub_core::ratelimit::{RateLimitQueue, RequestPriority};
use feedhub_net::{
    CancelToken, LimitRejected, NetClient, NetError, NetRequest, Priority, ProxyPlan,
};
use crate::net::FfiNetError;

/// **内部伪请求头**：平台对象用它表达"这次是用户正等着的请求"（`RequestPriority.Interactive`）。
///
/// 和 `feedhub_core::hosted::PRIORITY_HEADER` / Kotlin 的 `PRIORITY_HEADER` **逐字相同**。
/// 为什么还留着它：11 家平台代码一行不改，它们已经在往请求头里挂这个了。
/// 这一层读出来就删掉——**绝不写到线上**（Kotlin 那边 `RustHttpHost` 也是这么做的）。
pub const PRIORITY_HEADER: &str = feedhub_core::hosted::PRIORITY_HEADER;

/// [`PRIORITY_HEADER`] 的取值（`interactive`）。
pub const PRIORITY_INTERACTIVE: &str = feedhub_core::hosted::PRIORITY_INTERACTIVE;

// ===========================================================================
// Cookie 罐 → feedhub-net 的 CookieHost
// ===========================================================================

/// 把 Rust 的 [`CookieJar`] 接到 [`feedhub_net::CookieHost`] 上。
///
/// 对应 OkHttp 的 `CookieJar` 那个位置，但**没有一趟 FFI**：`load_for_request` / `save_from_response`
/// 都是罐子本体的方法。`Set-Cookie` 的解析用 [`cookies::parse_set_cookies`]（`Cookie.parse` 的移植）。
///
/// `now_ms` 是**注入的时钟**（生产是 `System.currentTimeMillis()`，测试固定）——和这一层其余模块
/// 一个规矩：Rust 不读系统时钟，对拍才逐条对得上。
pub struct JarCookies {
    jar: Arc<CookieJar>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 收下过多少条 `Set-Cookie`（排障 / 性能记录用；**不含 Cookie 值**）。
    saved: AtomicI64,
}

impl JarCookies {
    pub fn new(jar: Arc<CookieJar>, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self { jar, now_ms, saved: AtomicI64::new(0) }
    }

}

/// URL 字符串 → 罐子要的那三样（host / 编码后的 path / 是不是 https）。
///
/// **不重算规范化**（punycode / 小写 / IPv6 去括号）：平台层拼出来的 URL 已经是我们自己要发的
/// 那个字节串，`http::Uri` 的 host 就是最终要连的那个主机名。这里只做"拆出来"。
fn jar_url(url: &str) -> Option<JarUrl> {
    let uri: http::Uri = url.parse().ok()?;
    let host = uri.host()?.to_string();
    let is_https = uri.scheme_str() == Some("https");
    // 没有 path 时 OkHttp 的 `HttpUrl.encodedPath` 是 `/`（不是空串）
    let path = match uri.path() {
        "" => "/".to_string(),
        p => p.to_string(),
    };
    Some(JarUrl::new(host, path, is_https))
}

impl feedhub_net::CookieHost for JarCookies {
    fn cookies(&self, url: &str) -> Vec<(String, String)> {
        let Some(u) = jar_url(url) else { return Vec::new() };
        self.jar
            .load_for_request(&u, (self.now_ms)())
            .into_iter()
            .map(|c| (c.name, c.value))
            .collect()
    }

    fn set_cookies(&self, url: &str, set_cookies: Vec<String>) {
        let Some(u) = jar_url(url) else { return };
        let now = (self.now_ms)();
        let parsed = cookies::parse_set_cookies(&u, &set_cookies, now);
        if parsed.is_empty() {
            return;
        }
        // 写不进去（磁盘满 / 权限）不该让这次请求失败：**响应已经在手上了**。
        // 和 OkHttp 那边一样——`CookieJar.saveFromResponse` 抛异常会毁掉这次调用，
        // 而 `RustCookieJar` 把存储错误包成 `IOException`；这里选择"记一笔、继续"，
        // 理由同 `feedhub-net` 对 `Set-Cookie` 的态度：丢一次落盘不该毁掉用户正在看的这一页。
        // **内存已经改了**，所以这次进程里的后续请求仍然带着新 Cookie。
        if self.jar.save_from_response(&parsed, now).is_ok() {
            self.saved.fetch_add(parsed.len() as i64, Ordering::Relaxed);
        }
    }

    fn put(&self, url: &str, cookies: Vec<feedhub_net::CookieSpec>) {
        let Some(u) = jar_url(url) else { return };
        let now = (self.now_ms)();
        let stored: Vec<StoredCookie> = cookies
            .into_iter()
            .map(|c| StoredCookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                expires_at_ms: c.expires_at_ms,
                secure: c.secure,
                http_only: c.http_only,
                host_only: c.host_only,
            })
            .collect();
        let _ = self.jar.save_from_response(&stored, now);
        let _ = u;
    }
}

// ===========================================================================
// 限速器：Rust 的 RateLimitQueue + 真正的"睡"
// ===========================================================================

/// 一个平台的排队器（对应 Kotlin 的 `MinIntervalInterceptor`）。
///
/// **决策在 [`RateLimitQueue`]**，
/// 这里只做三件 OkHttp 那边由拦截器做的事：**睡**（每 100 ms 醒一次问一次）、**看取消**、
/// 放行时把"等了多久"记下来（`queue_ms`）。
///
/// 睡眠在**调用线程**上（`HostedCall.blocking` 已经在 `Dispatchers.IO` 上了）——和
/// `MinIntervalInterceptor` 在 OkHttp 工作线程上睡是同一个位置。
pub struct PlatformLimiter {
    queue: Mutex<RateLimitQueue>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 等待切片：被取消的请求最多拖这么久才让出时间槽（和 `MinIntervalInterceptor.DEFAULT_POLL_MS` 一样）。
    poll_ms: i64,
}

impl PlatformLimiter {
    /// `seed` 由调用方注入（生产 `System.nanoTime()`，测试固定值——抖动逐票可复现）。
    pub fn new(
        min_interval_ms: i64,
        jitter_ms: i64,
        background_grace_ms: i64,
        seed: i64,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            queue: Mutex::new(RateLimitQueue::new(
                min_interval_ms,
                jitter_ms,
                background_grace_ms,
                seed,
            )),
            now_ms,
            poll_ms: 100,
        }
    }

    fn acquire_blocking(
        &self,
        priority: Priority,
        cancel: &Arc<CancelToken>,
    ) -> Result<i64, LimitRejected> {
        let pri = match priority {
            Priority::Interactive => RequestPriority::Interactive,
            Priority::Background => RequestPriority::Background,
        };
        let handle = {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.enqueue(pri, (self.now_ms)())
        };
        // 票没发出去就一定要让出来（取消 / 出错都要）；放行了的票是空操作。
        let mut granted = false;
        let result = loop {
            if cancel.is_cancelled() {
                break Err(LimitRejected::Cancelled);
            }
            let now = (self.now_ms)();
            let waited = {
                let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                q.try_grant(handle, now)
            };
            match waited {
                Some(ms) => {
                    granted = true;
                    break Ok(ms);
                }
                None => {
                    let remaining = {
                        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                        q.wait_ms(handle, now)
                    };
                    if remaining <= 0 {
                        continue;
                    }
                    // 用 CancelToken 的 Condvar 等待：取消时立刻唤醒，平时仍保留 poll_ms 切片感知前序让票
                    let chunk = remaining.min(self.poll_ms).max(1);
                    if cancel.wait_timeout(Duration::from_millis(chunk as u64)) {
                        break Err(LimitRejected::Cancelled);
                    }
                }
            }
        };
        if !granted {
            let now = (self.now_ms)();
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.release(handle, now);
        }
        result
    }
}

impl feedhub_net::LimiterHost for PlatformLimiter {
    fn acquire(
        &self,
        _platform: &str,
        priority: Priority,
        cancel: &Arc<CancelToken>,
    ) -> Result<(), LimitRejected> {
        self.acquire_blocking(priority, cancel).map(|_| ())
    }

    fn acquire_with_wait(
        &self,
        _platform: &str,
        priority: Priority,
        cancel: &Arc<CancelToken>,
    ) -> Result<i64, LimitRejected> {
        self.acquire_blocking(priority, cancel)
    }
}

// ===========================================================================
// NetHttpHost：feedhub-core 的 HttpHost
// ===========================================================================

/// **平台对象用的 `HttpHost`**：把 [`HttpRequest`] 翻成 [`NetRequest`]、调 [`NetClient::execute`]、
/// 再把结果翻回 [`HttpResponse`]。
///
/// 一家平台一个实例（和 Kotlin 那边 `ctx.client(platform)` 派生一个客户端对齐）：代理档、限速器、
/// Cookie 罐都由构造时给。
pub struct NetHttpHost {
    client: Arc<NetClient>,
    platform: String,
    /// 这家平台的默认头（`DefaultHeadersInterceptor` 那张表）。**同名不覆盖**。
    default_headers: Vec<(String, String)>,
    /// 上面那张表只对这些主机生效（`hostFilter`）。空 = 全部生效。
    default_headers_hosts: Vec<String>,
    /// API 登录头（每次请求现算，所以是闭包；`after_limit` 里追加）。
    auth_headers: Option<Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>>,
    /// txid 生成器（`(方法, 路径) -> 头值`；`None` = 不算）。
    txid: Option<Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>>,
    /// 响应头观察者（X 的 `x-rate-limit-*` 记账）。
    on_headers: Option<Arc<dyn Fn(&[(String, String)]) + Send + Sync>>,
    /// **当前在飞那个请求的**取消令牌（`None` = 现在没有请求在飞）。
    ///
    /// 为什么是"每个请求一个新令牌"而不是一个常驻的：`CancelToken` 是一次性的
    /// （`is_cancelled` 一旦置上就回不去），而 OkHttp 的 `Call.cancel()` 只掐那一次调用，
    /// 之后同一个客户端照常能用。所以每次 `execute` 换一个新的，取消只影响当时在飞的那条。
    in_flight: Mutex<Option<Arc<CancelToken>>>,
    /// 这一家的 Cookie 罐（逐请求带给 `NetClient`：连接池共用，罐子各是各的）。
    jar_cookies: Option<Arc<JarCookies>>,
    /// 这一家的排队器（逐请求带给 `NetClient`，理由同上）。
    limiter: Option<Arc<PlatformLimiter>>,
    /// 这一跳的读超时覆盖（贴吧 `unicast` 长轮询）。
    read_timeout: Option<Duration>,
    /// 跟不跟重定向（`None` = 客户端默认 20 次）。
    follow_redirects: Option<bool>,
    /// 上一次调用在限速队列里等了多久（`queue_ms` 记账）。
    limiter_wait: Arc<AtomicI64>,
}

impl NetHttpHost {
    pub fn new(client: Arc<NetClient>, platform: &str) -> Self {
        Self {
            client,
            platform: platform.to_string(),
            default_headers: Vec::new(),
            default_headers_hosts: Vec::new(),
            auth_headers: None,
            txid: None,
            on_headers: None,
            in_flight: Mutex::new(None),
            jar_cookies: None,
            limiter: None,
            read_timeout: None,
            follow_redirects: None,
            limiter_wait: Arc::new(AtomicI64::new(0)),
        }
    }

    /// 挂这家平台的默认头（`DefaultHeadersInterceptor` 的语义：同名不覆盖）。
    pub fn with_default_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.default_headers = headers;
        self
    }

    /// 默认头只对这些主机生效（`DefaultHeadersInterceptor` 的 `hostFilter`）。空 = 全部生效。
    pub fn with_default_header_hosts(mut self, hosts: Vec<String>) -> Self {
        self.default_headers_hosts = hosts;
        self
    }

    /// API 登录头（每次请求现算：`ct0` 可能刚变）。
    pub fn with_auth_headers(
        mut self,
        f: Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>,
    ) -> Self {
        self.auth_headers = Some(f);
        self
    }

    /// txid 生成器（`(方法, 路径) -> 头值`）。
    pub fn with_txid(
        mut self,
        f: Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>,
    ) -> Self {
        self.txid = Some(f);
        self
    }

    /// 响应头观察者（X 的 `x-rate-limit-*` 记账）。
    pub fn with_header_observer(
        mut self,
        f: Arc<dyn Fn(&[(String, String)]) + Send + Sync>,
    ) -> Self {
        self.on_headers = Some(f);
        self
    }

    /// 挂这一家的 Cookie 罐（`CookieHost::cookies` / `set_cookies` 走它）。
    pub fn with_cookies(mut self, jar: Arc<JarCookies>) -> Self {
        self.jar_cookies = Some(jar);
        self
    }

    /// 挂这一家的排队器。
    pub fn with_limiter(mut self, limiter: Arc<PlatformLimiter>) -> Self {
        self.limiter = Some(limiter);
        self
    }

    /// 取消这个 host 上**正在飞的那一个**请求（`Call.cancel()` 的语义）。
    ///
    /// 幂等 / 可重复用：取消之后下一个请求会拿到一个新令牌，照常能发。
    pub fn cancel(&self) {
        let slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = slot.as_ref() {
            t.cancel();
        }
    }

    /// 上一次调用在限速队列里等了多久（毫秒；`queue_ms` 记账用）。
    pub fn last_wait_ms(&self) -> i64 {
        self.limiter_wait.load(Ordering::Relaxed)
    }

    /// 这次请求的 `after_limit`：**登录头 → txid**，顺序照 Kotlin 的拦截器链。
    ///
    /// 只有 X 会填（[`Self::with_auth_headers`] / [`Self::with_txid`]）；其余平台两个都是 `None`，
    /// 返回 `None` —— `NetClient` 因此**一个钩子都不调**。
    ///
    /// 这里**一条钩子只装一遍**：`XDetailRequestEquivalenceTest` 抓到过登录头加两遍
    /// （同一个头既当默认头又当登录头），所以顺序和"谁来装"都只留这一个入口。
    fn after_limit_hook(&self) -> Option<Arc<dyn feedhub_net::AfterLimit>> {
        if self.auth_headers.is_none() && self.txid.is_none() {
            return None;
        }
        Some(Arc::new(HostAfterLimit {
            auth: self.auth_headers.clone(),
            txid: self.txid.clone(),
            default_headers_hosts: self.default_headers_hosts.clone(),
        }))
    }

    /// 这次的 `after_headers`：X 的 `x-rate-limit-*` 记账（只看响应头，**不拦**）。
    fn after_headers_hook(&self) -> Option<Arc<dyn feedhub_net::PostHeaders>> {
        self.on_headers.clone().map(|f| {
            Arc::new(HostAfterHeaders { on_headers: f }) as Arc<dyn feedhub_net::PostHeaders>
        })
    }

    /// 把 `HttpRequest` 翻成 `NetRequest`（**不执行**；测试直接看这个）。
    pub fn to_net_request(&self, request: &HttpRequest) -> (NetRequest, Priority) {
        // 伪请求头：读出来就删（**绝不写到线上**），转成 `Priority`。
        let mut priority = Priority::Background;
        let mut headers: Vec<(String, String)> = Vec::with_capacity(request.headers.len());
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case(PRIORITY_HEADER) {
                if value.eq_ignore_ascii_case(PRIORITY_INTERACTIVE) {
                    priority = Priority::Interactive;
                }
            } else {
                headers.push((name.clone(), value.clone()));
            }
        }
        let req = NetRequest {
            platform: self.platform.clone(),
            method: request.method.clone(),
            url: request.url.clone(),
            headers,
            body: request.body.clone(),
            priority,
            cancel: Arc::new(CancelToken::new()),
            read_timeout: self.read_timeout,
            proxy: None,
            observer: None,
            extra_default_headers: self.default_headers.clone(),
            default_headers_hosts: self.default_headers_hosts.clone(),
            follow_redirects: self.follow_redirects,
            cookies: self
                .jar_cookies
                .clone()
                .map(|j| j as Arc<dyn feedhub_net::CookieHost>),
            limiter: self
                .limiter
                .clone()
                .map(|l| l as Arc<dyn feedhub_net::LimiterHost>),
            // 发之前没有闸：`onRequest` 计数 / cli 的冷却都在宿主门面上（这一层一个都不装）。
            gate: None,
            // 登录头 / txid（**限速之后、发之前**）；两者都没有时是 `None`
            after_limit: self.after_limit_hook(),
            // 响应头记账（X 的 `x-rate-limit-*`）
            after_headers: self.after_headers_hook(),
        };
        (req, priority)
    }
}

/// [`NetError`] → [`FfiHostError`]（扫码那条路**不过 core 的 `HostError`**）。
///
/// 分类和 [`to_host_error`] 那张表逐条相同，只是少一次转换：扫码宿主直接对 FFI 返回，
/// 中间没有"core 平台对象"那一层。
fn to_ffi_host_error(e: NetError) -> crate::host::FfiHostError {
    match to_host_error(e) {
        HostError::Network { detail } => crate::host::FfiHostError::Network { detail },
        HostError::ProxyUnavailable { detail } => {
            crate::host::FfiHostError::ProxyUnavailable { detail }
        }
        HostError::Other { detail } => crate::host::FfiHostError::Other { detail },
    }
}

/// [`NetError`] → [`HostError`]。
///
/// | `NetError` | `HostError` | 为什么 |
/// |---|---|---|
/// | `ProxyUnavailable` | `ProxyUnavailable` | fail-closed 的语义要一路保到 cli 的退出码 6 |
/// | `Timeout` | `Network` | OkHttp 那边 `SocketTimeoutException` 是 `IOException` 的子类 |
/// | `Cancelled` | `Network`（detail = `Canceled`） | `Call.cancel()` 抛 `IOException("Canceled")` |
/// | `Network` | `Network` | 同名 |
/// | `BadRequest` | `Network` | OkHttp 那边是 `IllegalArgumentException`，但宿主只在 IO 线程上 catch `IOException`；**照旧映射成 Network**（见下文） |
/// | `Other` | `Other` | 同名 |
pub fn to_host_error(e: NetError) -> HostError {
    match e {
        NetError::ProxyUnavailable { detail } => HostError::ProxyUnavailable { detail },
        NetError::Network { detail, .. } => HostError::Network { detail },
        NetError::Timeout { detail } => HostError::Network { detail },
        NetError::Cancelled => HostError::Network { detail: "Canceled".to_string() },
        // 但调用方（`HostedCall.blocking` / `guard`）只把 `IOException` 当"网络错误"原样抛，
        // 别的会包成 `SourceException`。平台层拼出来的 URL 一直是合法的（有单测守着），
        // 这一档真出现就是 BUG；归到 `Network` 让它至少是一条"请求失败"而不是"socket 层崩溃"。
        NetError::BadRequest { detail } => HostError::Network {
            detail: format!("请求不合法：{detail}"),
        },
        NetError::Other { detail } => HostError::Other { detail },
    }
}

impl HttpHost for NetHttpHost {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HostError> {
        let (req, _priority) = self.to_net_request(&request);
        // 把这次请求的令牌挂到"在飞槽"上：`cancel()` 掐的就是它。
        // 请求结束（成功 / 失败 / 取消）都要清掉，否则 `cancel()` 会掐一个已经结束的令牌
        // （那是无害的，但会让下一个请求的令牌**没被取消**这件事看起来像 bug）。
        {
            let mut slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            *slot = Some(req.cancel.clone());
        }
        let result = self.client.execute(req);
        {
            let mut slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            *slot = None;
        }
        let resp = result.map_err(to_host_error)?;
        Ok(HttpResponse {
            status: resp.status,
            headers: resp.headers,
            body: resp.body,
            final_url: resp.final_url,
        })
    }
}

// ===========================================================================
// 罐子的另一半：feedhub_core::host::CookieHost（登录态判定 / 种 Cookie）
// ===========================================================================

/// 把同一个 [`CookieJar`] 接到 `feedhub_core::host::CookieHost` 上（平台对象用它判登录态 / 种 Cookie）。
///
/// 和 [`JarCookies`] 是**同一个罐子的两个门**：那边是"发请求带哪些 / 收下 `Set-Cookie`"，
/// 这边是"登录态在不在 / 自己种一条"。以前后者要过 FFI 回调 Kotlin 的 `ctx.cookies`
/// （`CookiesAdapter` → `RustCookieHost`），现在直接落到罐子上。
pub struct JarFinder {
    jar: Arc<CookieJar>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl JarFinder {
    pub fn new(jar: Arc<CookieJar>, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self { jar, now_ms }
    }
}

impl feedhub_core::host::CookieHost for JarFinder {
    fn find(&self, url: &str, name: &str) -> Option<String> {
        let u = jar_url(url)?;
        self.jar.find(&u, name, (self.now_ms)())
    }

    fn put(&self, url: &str, cookies: Vec<feedhub_core::host::CookieSpec>) {
        let Some(_u) = jar_url(url) else { return };
        let now = (self.now_ms)();
        let stored: Vec<StoredCookie> = cookies
            .into_iter()
            .map(|c| StoredCookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                // **域 Cookie**（`host_only = false`）：Kotlin 的 `RustCookieHost.put` 用的是
                // `Cookie.Builder().domain(...)`，不是 `hostOnlyDomain(...)`。写成 host-only 的话
                // `domain=bilibili.com` 的 buvid3 在 `www.bilibili.com` 上匹配不到，每次请求都会重领。
                host_only: false,
                path: c.path,
                expires_at_ms: c.expires_at_ms,
                secure: false,
                http_only: false,
            })
            .collect();
        let _ = self.jar.save_from_response(&stored, now);
    }
}

// ===========================================================================
// 一个平台要的那一份"发请求的东西"
// ===========================================================================

/// 一家平台的联网配置（限速 / 默认头 / 读超时 / 跟随重定向）。
///
/// 这些值原来散在 Kotlin 的 `ctx.client(platform)` 后面那几个 `addInterceptor` 调用上，
/// 现在由调用方（`rust-bridge` 的门面）一次说明白，Rust 照着建。
#[derive(Debug, Clone, Default)]
pub struct HostSpec {
    /// 平台 id（限速分桶 / 代理档 / 错误文案）。
    pub platform: String,
    /// 平台默认头（`DefaultHeadersInterceptor` 那张表的顺序）。**同名不覆盖**。
    pub default_headers: Vec<(String, String)>,
    /// 上面那张表**只对这些主机生效**（`hostFilter`）。空 = 全部生效。
    pub default_headers_hosts: Vec<String>,
    /// 限速：两次请求之间至少隔这么久（毫秒；`0` = 不限速）。
    pub min_interval_ms: i64,
    /// 限速的抖动（毫秒）。
    pub jitter_ms: i64,
    /// 后台请求的保护期（毫秒；`0` = 用默认 5 秒）。
    pub background_grace_ms: i64,
    /// 抖动用的随机种子（生产 `System.nanoTime()`，测试固定）。
    pub random_seed: i64,
    /// 这一跳的读超时覆盖（毫秒；`0` = 客户端默认 30 s）。
    pub read_timeout_ms: i64,
    /// 跟不跟重定向（`true` = 20 次，和 OkHttp 默认一样）。
    pub follow_redirects: bool,
}

/// 默认的后台保护期（和 `MinIntervalInterceptor.DEFAULT_BACKGROUND_GRACE_MS` 一样）。
pub const DEFAULT_BACKGROUND_GRACE_MS: i64 = 5_000;

// ===========================================================================
// UniFFI 入口：让 Kotlin 能用"Rust 发请求"的那一套建平台对象
// ===========================================================================

/// 这一层唯一的时钟（**Rust 全家只有这里读系统时间**）。
///
/// 为什么破例：`feedhub-core` / `feedhub-net` 的规矩是"时钟注入"，
/// 而这一层是**生产粘合层**——UniFFI 传不了闭包，`FfiNetStack::new` 收的 `now_ms` 只是"建栈那一刻"。
/// Cookie 的过期是小时 / 天级的，秒级误差不影响任何语义；系统时间倒退（用户改表）时用建栈时那个
/// 兜底，不让罐子把"未来的 Cookie"判成过期。
fn wall_clock(built_at_ms: i64) -> Arc<dyn Fn() -> i64 + Send + Sync> {
    Arc::new(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(built_at_ms);
        // 时间倒退（或者压根没对上）时用建栈那一刻：宁可用旧时间，也不要凭空判过期
        if now < built_at_ms {
            built_at_ms
        } else {
            now
        }
    })
}

/// 一家平台的联网描述（过边界的形状，字段见 [`HostSpec`]）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHostSpec {
    /// 平台 id（`bilibili` / `reddit` / `x` …）。
    pub platform: String,
    /// 平台默认头：`"Name: Value"` 的字符串（**有序**）。
    pub default_headers: Vec<String>,
    /// 上面那张表**只对这些主机生效**（`DefaultHeadersInterceptor` 的 `hostFilter`）。空 = 全部生效。
    pub default_headers_hosts: Vec<String>,
    /// 两次请求之间至少隔多久（毫秒；`0` = 不限速）。
    pub min_interval_ms: i64,
    /// 限速抖动（毫秒）。
    pub jitter_ms: i64,
    /// 后台请求的保护期（毫秒；`0` = 用默认 5 秒）。
    pub background_grace_ms: i64,
    /// 抖动用的随机种子（生产传 `System.nanoTime()`，测试给固定值）。
    pub random_seed: i64,
    /// 这一跳的读超时覆盖（毫秒；`0` = 默认 30 秒）。
    pub read_timeout_ms: i64,
    /// 跟不跟重定向。
    pub follow_redirects: bool,
    /// **API 登录头**（`authorization` / `x-csrf-token` / `x-twitter-auth-type`）：`"Name: Value"`，**有序**。
    ///
    /// 这些都是"每次请求现算"的（`ct0` 可能刚变），所以**不能**放进 `default_headers`（那张表是构造时定死的）。
    /// 追加位置照 Kotlin 的 `AuthHeadersInterceptor`：默认头**之后**、限速**之前**
    /// ——不过它们和限速的相对顺序不影响线上字节（限速不产生头），所以挂在这里的 `after_limit` 里、
    /// 在 txid **之前**追加即可。
    pub auth_headers: Vec<String>,
    /// 要不要算 **txid**（`x-client-transaction-id`）：只有 X 的 API 请求要（页面不要）。
    pub txid: bool,
}

fn spec_of(ffi: &FfiHostSpec) -> Result<HostSpec, FfiNetError> {
    let mut headers = Vec::with_capacity(ffi.default_headers.len());
    for line in &ffi.default_headers {
        match line.split_once(':') {
            Some((k, v)) => headers.push((k.trim().to_string(), v.trim().to_string())),
            None => {
                return Err(FfiNetError::BadRequest {
                    detail: format!("默认请求头要写成 \"Name: Value\"：{line}"),
                })
            }
        }
    }
    // spec 里那两份没人读。FFI 记录上的字段留着（Kotlin 还在传），
    // 但这里仍然**校验**格式——拼错的头不该被静默丢掉。txid 是布尔，读一下表示"这个字段还有用"。
    let _ = auth_of(&ffi.auth_headers)?;
    let _ = ffi.txid;
    Ok(HostSpec {
        platform: ffi.platform.clone(),
        default_headers: headers,
        default_headers_hosts: ffi.default_headers_hosts.clone(),
        min_interval_ms: ffi.min_interval_ms,
        jitter_ms: ffi.jitter_ms,
        background_grace_ms: ffi.background_grace_ms,
        random_seed: ffi.random_seed,
        read_timeout_ms: ffi.read_timeout_ms,
        follow_redirects: ffi.follow_redirects,
    })
}

/// **Rust 侧的联网栈**（一份客户端 = 一个代理快照 + 一个连接池）。
///
/// 用法（Kotlin）：
/// ```kotlin
/// val stack = FfiNetStack(proxies, null, jar, nowMs = { System.currentTimeMillis() })
/// val spec  = FfiHostSpec(platform = "reddit", defaultHeaders = listOf("User-Agent: …"), …)
/// val host  = stack.host(spec)      // -> HostHttp 的 Rust 实现
/// val src   = FfiRedditSource(base, host, stack.cookies())
/// ```
///
/// **代理档变了就整份换新**（和 `AppGraph.watchProxySettings` / `SourceContext.withFreshConnectionPool()`
/// 对齐）：旧的那份让连接自然回收。
#[derive(uniffi::Object)]
pub struct FfiNetStack {
    client: Arc<NetClient>,
    jar: Arc<CookieJar>,
    /// 这份栈建出来的那些 host（`cancel` 要把它们的在飞请求一起掐掉）。
    ///
    /// 用 `Weak`：host 的生命周期归调用方（平台对象持有它），栈只是"取消的时候能找到它们"。
    hosts: Mutex<Vec<std::sync::Weak<NetHttpHost>>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

#[uniffi::export]
impl FfiNetStack {
    /// 建一份。`proxies` 是"平台 → 代理档"，没列出的用 `default_proxy`（`None` = 直连）。
    #[uniffi::constructor]
    pub fn new(
        proxies: Vec<crate::net::FfiNetPlatformProxy>,
        default_proxy: Option<crate::net::FfiNetProxy>,
        jar: Arc<crate::cookies::CookieJar>,
        now_ms: i64,
    ) -> Result<Arc<Self>, FfiNetError> {
        let mut plan = ProxyPlan::direct();
        if let Some(d) = default_proxy {
            plan = plan.with_default(d.into());
        }
        for entry in proxies {
            plan = plan.set(&entry.platform, entry.proxy.into());
        }
        let client = NetClient::builder()
            .proxies(plan)
            .build()
            .map_err(FfiNetError::from)?;
        // **和过 FFI 的那个句柄共用同一个罐子**（`CookieJar::shared()` 返回 `Arc` 的克隆）。
        Ok(Arc::new(Self {
            client: Arc::new(client),
            jar: jar.shared(),
            hosts: Mutex::new(Vec::new()),
            // 时钟：UniFFI 不支持把闭包传过来，所以**这一层自己读系统时间**
            // （唯一读时钟的地方，理由写在 `wall_clock` 的注释上；`now_ms` 是"建这份栈时"的毫秒，
            // 只用来兜底——系统时间倒退时才用它）。
            now_ms: wall_clock(now_ms),
        }))
    }

    /// 取消在这一份栈上**所有在飞的**请求。
    ///
    /// 这是 `HostedCall.blocking` 在协程被取消时调的那一下：Rust 的调用是同步阻塞的
    /// （JNA 那一层打断不了），只能从外面把每个 host 的在飞请求掐掉，让 Rust 那边立刻
    /// 带着 `Cancelled` 返回、协程马上结束。
    pub fn cancel(&self) {
        let hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        for weak in hosts.iter() {
            if let Some(h) = weak.upgrade() {
                h.cancel();
            }
        }
    }

    /// 建一家平台的 `HttpHost`（Rust 实现，**不过 FFI**）。
    pub fn host(&self, spec: FfiHostSpec) -> Result<Arc<FfiNetHostHandle>, FfiNetError> {
        let spec = spec_of(&spec)?;
        let host = self.build_host(spec);
        self.track(&host);
        Ok(Arc::new(FfiNetHostHandle { inner: host }))
    }

    /// 建一家平台的 `HostCookies`（登录态判定 / 种 Cookie）。
    pub fn cookies(&self) -> Arc<FfiNetCookies> {
        Arc::new(FfiNetCookies {
            inner: Arc::new(JarFinder::new(self.jar.clone(), self.now_ms.clone())),
        })
    }

    ///
    /// [spec] 和接口那个 host **用同一份**（默认头 / 限速 / 读超时 / 重定向），[decoder] 是宿主
    /// 那半边（zxing 只有 Java 版；`None` = 这个平台用不到解码）。
    ///
    /// 和 [host] 的区别只有两处：**罐子换成临时的**（逐请求，和正式罐完全隔离）、
    /// 以及多出来 `all` / `begin` / `end` / `commit` / `cookies` / `find` / `put` 这几个罐子操作。
    /// 代理 / fail-closed / 连接池 / 限速器和接口请求**一模一样**（同一个 `NetClient` + 同一个排队器）。
    pub fn qr_host(
        &self,
        spec: FfiHostSpec,
        decoder: Option<Arc<dyn crate::host::HostQrDecoder>>,
    ) -> Result<Arc<FfiNetQrHost>, FfiNetError> {
        let spec = spec_of(&spec)?;
        let limiter = Arc::new(PlatformLimiter::new(
            spec.min_interval_ms,
            spec.jitter_ms,
            if spec.background_grace_ms > 0 {
                spec.background_grace_ms
            } else {
                DEFAULT_BACKGROUND_GRACE_MS
            },
            spec.random_seed,
            self.now_ms.clone(),
        ));
        let decode: Option<QrDecodeFn> = decoder
            .map(|d| Arc::new(move |png: Vec<u8>| d.decode_qr_image(png)) as QrDecodeFn);
        Ok(Arc::new(FfiNetQrHost {
            inner: Arc::new(NetQrHost::new(
                spec,
                self.client.clone(),
                self.jar.clone(),
                limiter,
                self.now_ms.clone(),
                decode,
            )),
        }))
    }
}

impl FfiNetStack {
    /// 给各平台对象的 `<with_net>` 构造函数用（那边要的是 `Arc<dyn HttpHost>`，不是 FFI 句柄）。
    pub(crate) fn build_host_public(
        &self,
        spec: FfiHostSpec,
    ) -> Result<Arc<NetHttpHost>, FfiNetError> {
        let host = self.build_host(spec_of(&spec)?);
        self.track(&host);
        Ok(host)
    }

    /// 记住这个 host（取消的时候能找到它的在飞请求）。顺手清掉已经析构的。
    fn track(&self, host: &Arc<NetHttpHost>) {
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        hosts.retain(|w| w.strong_count() > 0);
        hosts.push(Arc::downgrade(host));
    }

    /// 建一个**带登录头 + txid** 的 host（X 的 API 端口）。
    pub(crate) fn build_host_with_hooks(
        &self,
        spec: FfiHostSpec,
        auth: Option<Arc<dyn HostAuthHeaders>>,
        txid: Option<Arc<dyn HostTxid>>,
        on_headers: Option<Arc<dyn HostHeaders>>,
    ) -> Result<Arc<NetHttpHost>, FfiNetError> {
        let spec = spec_of(&spec)?;
        let mut host = NetHttpHost::new(self.client.clone(), &spec.platform)
            .with_default_headers(spec.default_headers)
            .with_default_header_hosts(spec.default_headers_hosts)
            .with_cookies(Arc::new(JarCookies::new(self.jar.clone(), self.now_ms.clone())));
        let limiter = Arc::new(PlatformLimiter::new(
            spec.min_interval_ms,
            spec.jitter_ms,
            if spec.background_grace_ms > 0 {
                spec.background_grace_ms
            } else {
                DEFAULT_BACKGROUND_GRACE_MS
            },
            spec.random_seed,
            self.now_ms.clone(),
        ));
        host = host.with_limiter(limiter);
        if let Some(a) = auth {
            let a = AuthHeadersAdapter(a);
            host = host.with_auth_headers(Arc::new(move || {
                a.0.headers()
                    .into_iter()
                    .filter_map(|line| {
                        line.split_once(':')
                            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                    })
                    .collect()
            }));
        }
        if let Some(t) = txid {
            let t = TxidAdapter(t);
            host = host.with_txid(Arc::new(move |method: &str, path: &str| {
                t.0.txid(method.to_string(), path.to_string())
            }));
        }
        if let Some(h) = on_headers {
            host = host.with_header_observer(Arc::new(move |headers: &[(String, String)]| {
                h.on_headers(
                    headers
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}"))
                        .collect(),
                );
            }));
        }
        host.read_timeout = if spec.read_timeout_ms > 0 {
            Some(Duration::from_millis(spec.read_timeout_ms as u64))
        } else {
            None
        };
        host.follow_redirects = Some(spec.follow_redirects);
        let host = Arc::new(host);
        self.track(&host);
        Ok(host)
    }

    /// 建一个**不带 Cookie 罐、不占限速**的 host（酷安的 `media` / `links` 两个端口）。
    ///
    /// 它们和主端口共用同一个连接池（同一份 `NetClient`），但**罐子和排队器都不给**：
    /// `media` 拉媒体文件不该带着 `.coolapk.com` 的登录 Cookie 去 CDN，也不该占平台那 600ms 的槽。
    pub(crate) fn build_media_spec_public(
        &self,
        spec: FfiHostSpec,
    ) -> Result<Arc<NetHttpHost>, FfiNetError> {
        let spec = spec_of(&spec)?;
        let mut host = NetHttpHost::new(self.client.clone(), &spec.platform)
            .with_default_headers(spec.default_headers)
            .with_default_header_hosts(spec.default_headers_hosts);
        host.read_timeout = if spec.read_timeout_ms > 0 {
            Some(Duration::from_millis(spec.read_timeout_ms as u64))
        } else {
            None
        };
        host.follow_redirects = Some(spec.follow_redirects);
        let host = Arc::new(host);
        self.track(&host);
        Ok(host)
    }

    /// 给各平台对象的 `<with_net>` 构造函数用：`Arc<dyn CookieHost>`（Rust 实现，不过 FFI）。
    pub(crate) fn cookies_host(&self) -> Arc<dyn feedhub_core::host::CookieHost> {
        Arc::new(JarFinder::new(self.jar.clone(), self.now_ms.clone()))
    }
    fn build_host(&self, spec: HostSpec) -> Arc<NetHttpHost> {
        let limiter = Arc::new(PlatformLimiter::new(
            spec.min_interval_ms,
            spec.jitter_ms,
            if spec.background_grace_ms > 0 {
                spec.background_grace_ms
            } else {
                DEFAULT_BACKGROUND_GRACE_MS
            },
            spec.random_seed,
            self.now_ms.clone(),
        ));
        let mut host = NetHttpHost::new(self.client.clone(), &spec.platform)
            .with_default_headers(spec.default_headers)
            .with_default_header_hosts(spec.default_headers_hosts)
            .with_cookies(Arc::new(JarCookies::new(self.jar.clone(), self.now_ms.clone())))
            .with_limiter(limiter);
        host.read_timeout = if spec.read_timeout_ms > 0 {
            Some(Duration::from_millis(spec.read_timeout_ms as u64))
        } else {
            None
        };
        host.follow_redirects = Some(spec.follow_redirects);
        Arc::new(host)
    }
}

/// 一家平台的 `HostHttp`（`feedhub_core::host::HttpHost` 的 Rust 实现）的句柄。
///
/// Kotlin 拿到的就是它，直接当 `FfiXxxSource::new(base, host, …)` 的第一个参数传进去——
/// **形状和原来的 `RustHttpHost` 一样**（都是 `HostHttp`），所以平台门面一行不用改。
#[derive(uniffi::Object)]
pub struct FfiNetHostHandle {
    inner: Arc<NetHttpHost>,
}

#[uniffi::export]
impl FfiNetHostHandle {
    /// 发一个请求（**同步**；Kotlin 在 `Dispatchers.IO` 上调）。
    ///
    /// 形状和 `crate::host::FfiHttpRequest` / `FfiHttpResponse` **逐个字段一样**，所以 Kotlin 那边
    /// 的适配器只有一层搬运（`RustNetHttpHost`），平台门面一行不用改。
    ///
    /// 错误走 [`FfiHostError`]（和原来 `HostHttp` 那条路是同一个 enum）：`ProxyUnavailable` 必须
    /// 单独一档（cli 的退出码 6 靠它），其余归 `Network` / `Other`。
    pub fn execute(
        &self,
        request: crate::host::FfiHttpRequest,
    ) -> Result<crate::host::FfiHttpResponse, crate::host::FfiHostError> {
        let core_request = HttpRequest {
            method: request.method,
            url: request.url,
            headers: request
                .headers
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
            body: request.body,
        };
        match self.inner.execute(core_request) {
            Ok(r) => Ok(crate::host::FfiHttpResponse {
                status: r.status,
                headers: r
                    .headers
                    .into_iter()
                    .map(|(name, value)| crate::host::FfiHeader { name, value })
                    .collect(),
                body: r.body,
                final_url: r.final_url,
            }),
            Err(HostError::ProxyUnavailable { detail }) => {
                Err(crate::host::FfiHostError::ProxyUnavailable { detail })
            }
            Err(HostError::Network { detail }) => {
                Err(crate::host::FfiHostError::Network { detail })
            }
            Err(HostError::Other { detail }) => Err(crate::host::FfiHostError::Other { detail }),
        }
    }

    /// 取消这个 host 上正在飞的那个请求（`Call.cancel()` 的语义）。
    ///
    /// 调用方：`HostedCall.blocking` 在协程被取消时调它（Rust 的调用是同步阻塞的，
    /// JNA 那一层打断不了，只能从外面把在飞的请求掐掉，让 Rust 那边立刻带着
    /// `Cancelled` 返回）。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// 上一次调用在限速队列里等了多久（毫秒；`queue_ms` 记账）。
    pub fn last_wait_ms(&self) -> i64 {
        self.inner.last_wait_ms()
    }
}

/// 一家平台的 `HostCookies`（`feedhub_core::host::CookieHost` 的 Rust 实现）的句柄。
///
/// 形状和 `crate::host::HostCookies` **一样**（`find(url, name)` / `put(url, cookies)`），
/// Kotlin 那边的适配器只有一层搬运。
#[derive(uniffi::Object)]
pub struct FfiNetCookies {
    inner: Arc<JarFinder>,
}

#[uniffi::export]
impl FfiNetCookies {
    /// 发往 `url` 的请求会带上的、名为 `name` 的 Cookie 值（登录态判定用它）。
    pub fn find(&self, url: String, name: String) -> Option<String> {
        feedhub_core::host::CookieHost::find(self.inner.as_ref(), &url, &name)
    }

    /// 把这几条 Cookie 种进罐里（不会顶掉别的域名）。
    pub fn put(&self, url: String, cookies: Vec<crate::host::FfiCookie>) {
        let specs = cookies
            .into_iter()
            .map(|c| feedhub_core::host::CookieSpec {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                expires_at_ms: c.expires_at_ms,
            })
            .collect();
        feedhub_core::host::CookieHost::put(self.inner.as_ref(), &url, specs)
    }
}

/// `after_limit` 的实现：追加**登录头**，再算 **txid**（顺序和 Kotlin 的拦截器链一样）。
///
/// 为什么必须在 `after_limit`（= 限速之后）：txid 里含"当前时刻"，
struct HostAfterLimit {
    auth: Option<Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>>,
    txid: Option<Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>>,
    default_headers_hosts: Vec<String>,
}

impl feedhub_net::AfterLimit for HostAfterLimit {
    fn after(
        &self,
        _platform: &str,
        method: &str,
        url: &str,
        headers: &mut Vec<(String, String)>,
    ) -> Result<(), NetError> {
        let host_allowed = if self.default_headers_hosts.is_empty() {
            true
        } else {
            let host = url
                .parse::<http::Uri>()
                .ok()
                .and_then(|u| u.host().map(|h| h.to_ascii_lowercase()))
                .unwrap_or_default();
            self.default_headers_hosts.iter().any(|suffix| {
                let s = suffix.trim_start_matches('.').to_ascii_lowercase();
                host == s || host.ends_with(&format!(".{s}"))
            })
        };

        if host_allowed {
            if let Some(auth) = &self.auth {
                for (name, value) in auth() {
                    headers.push((name, value));
                }
            }
            if let Some(txid) = &self.txid {
                // 只有 `/i/api/graphql/` 的请求带这个头（判定在宿主那边，和 `TxidInterceptor` 一样）
                let path = url
                    .parse::<http::Uri>()
                    .ok()
                    .map(|u| u.path().to_string())
                    .unwrap_or_default();
                if let Some(value) = txid(method, &path) {
                    headers.push(("x-client-transaction-id".to_string(), value));
                }
            }
        }
        Ok(())
    }
}

/// `after_headers` 的实现：把响应头交给宿主记账（X 的 `x-rate-limit-*`），**永远不拦**。
struct HostAfterHeaders {
    on_headers: Arc<dyn Fn(&[(String, String)]) + Send + Sync>,
}

impl feedhub_net::PostHeaders for HostAfterHeaders {
    fn after(
        &self,
        _platform: &str,
        _url: &str,
        _status: u16,
        headers: &[(String, String)],
    ) -> Result<(), NetError> {
        (self.on_headers)(headers);
        Ok(())
    }
}

/// `"Name: Value"` 列表 → 有序的 `(name, value)`。
fn auth_of(lines: &[String]) -> Result<Vec<(String, String)>, FfiNetError> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        match line.split_once(':') {
            Some((k, v)) => out.push((k.trim().to_string(), v.trim().to_string())),
            None => {
                return Err(FfiNetError::BadRequest {
                    detail: format!("登录头要写成 \"Name: Value\"：{line}"),
                })
            }
        }
    }
    Ok(out)
}

/// **X 的登录头**（过边界的 trait）：每次请求现算（`ct0` 可能刚变）。
///
/// 为什么是 `with_foreign` 回调而不是一个固定表：`authorization` 是常量，但
/// `x-csrf-token`（`ct0`）和 `x-twitter-auth-type`（登没登录）是**现读 Cookie 罐**的。
#[uniffi::export(with_foreign)]
pub trait HostAuthHeaders: Send + Sync {
    /// `"Name: Value"` 列表（**有序**；空 = 这条请求不带登录头）。
    fn headers(&self) -> Vec<String>;
}

/// **X 的 txid**（过边界的 trait）：算法在宿主（`RustX.transaction` 走的是 Rust 的纯函数，
/// 但**取什么时间 / 随机字节**在宿主），判定"要不要带"由 Rust 侧做（只对 `/i/api/graphql/`）。
#[uniffi::export(with_foreign)]
pub trait HostTxid: Send + Sync {
    /// `(方法, 路径)` → 头值；`None` = 这条请求不带这个头。
    fn txid(&self, method: String, path: String) -> Option<String>;
}

/// **响应头观察者**（过边界的 trait）：X 的 `x-rate-limit-*` 记账。
#[uniffi::export(with_foreign)]
pub trait HostHeaders: Send + Sync {
    /// 这一条响应的头（`"Name: Value"`，有序）。**不拦**，只是记账。
    fn on_headers(&self, headers: Vec<String>);
}

/// 把 `HostAuthHeaders` / `HostTxid` 包成 [`NetHttpHost`] 的闭包。
pub(crate) struct AuthHeadersAdapter(pub Arc<dyn HostAuthHeaders>);
pub(crate) struct TxidAdapter(pub Arc<dyn HostTxid>);

// ===========================================================================
// 测试
// ===========================================================================

// ===========================================================================
// 扫码登录的临时 Cookie 罐
// ===========================================================================

/// **`HostQrLogin` 的 Rust 实现**：临时罐挂在 Rust 联网层的请求上，不再回 Kotlin 造 OkHttp 客户端。
///
/// 语义逐条照 `feedhub_core::host::QrHost`（也就是搬之前那个 Kotlin `RustQrHost` + `QrSessions`）：
///
/// | 这件事 | 以前（Kotlin） | 现在 |
/// |---|---|---|
/// | 临时罐 | `MemoryCookieJar` + `client.newBuilder().cookieJar(临时罐)` | [`QrSessionJar`]（`feedhub_core::CookieJar::in_memory`），逐请求交给 `NetRequest::cookies` |
/// | 和 `ctx.cookies` 隔离 | 两个不同的 `CookieJar` 对象 | **两个不同的 Rust 罐**（`begin` 时新开一个内存罐，一个字节都不碰正式罐） |
/// | 会话编号 | 宿主自己记 `qr-1` / `qr-2`… | 一样（[`QrSessions::next`]） |
/// | `execute` 用的客户端 | 平台那个 OkHttp（限速 / 拦截器 / 连接池） | **同一个 [`NetClient`]** + 平台那份 [`HostSpec`]（默认头 / 限速 / 读超时 / 跟不跟重定向照旧），只是罐子换成临时的 |
/// | `commit` 的顺序 | 先 `remove` 旧登录 Cookie，再 `put` 新的 | 一样，顺序不能反 |
/// | `decode_qr_image` | zxing（只有 Java 版） | **还在宿主**（回调 Kotlin 那一半） |
///
/// 为什么"隔离"这条要用**两个罐对象**：扫码期间服务端会下发一整套新 Cookie（含设备标识），中途失败
/// 就会把用户的正式登录态弄脏；两个对象之间没有任何共享状态，这条红线不靠"记得别写"来保证。
pub struct NetQrHost {
    /// 平台那份 `HostSpec`（默认头 / 限速 / 读超时 / 重定向）——扫码请求和接口请求**同一份口径**。
    spec: HostSpec,
    /// 正式罐（`all()` 从这里读；`commit` 往这里写）。
    jar: Arc<CookieJar>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 平台那个排队器（和接口请求**共用一个**：`MinIntervalInterceptor` 只有一个实例）。
    limiter: Arc<PlatformLimiter>,
    /// 这份栈的 `NetClient`（扫码和接口**共用连接池**：`ProxyPlan` 一样，代理档也就一样）。
    client: Arc<NetClient>,
    /// 临时罐（`begin` 时建，`end` 时丢）。访问序 LRU 8 个——照 Kotlin `QrSessions`。
    sessions: Mutex<QrSessions>,
    /// 当前在飞那一跳的取消令牌（[`Self::cancel`] 掐它）。
    in_flight: Mutex<Option<Arc<CancelToken>>>,
    /// 二维码解码（宿主那一半；`None` = 这个平台用不到解码，比如 B站）。
    decode_qr: Option<QrDecodeFn>,
}

/// 临时罐的 LRU（键 = 宿主给的会话编号）。
///
/// 为什么是 8：Kotlin `QrSessions` 就是 8（最多同时开 8 个扫码窗口，正常只用 1 个）；再多不该无限涨——
/// 每一个都是一个内存罐 + 一整套 Cookie。
struct QrSessions {
    next: u64,
    /// `(编号, 罐子)`，**按访问序**排（`get` 把命中的挪到末尾）。
    entries: Vec<(String, Arc<CookieJar>)>,
}

impl QrSessions {
    fn new() -> Self {
        Self { next: 0, entries: Vec::new() }
    }

    fn begin(&mut self, now_ms: i64) -> String {
        self.next += 1;
        let id = format!("qr-{}", self.next);
        self.entries.push((id.clone(), Arc::new(CookieJar::in_memory(now_ms))));
        while self.entries.len() > 8 {
            // 超了就从**最久没用的**那头丢
            self.entries.remove(0);
        }
        id
    }

    fn get(&mut self, id: &str) -> Option<Arc<CookieJar>> {
        let idx = self.entries.iter().position(|(k, _)| k == id)?;
        let entry = self.entries.remove(idx);
        let jar = entry.1.clone();
        self.entries.push(entry);
        Some(jar)
    }

    fn end(&mut self, id: &str) {
        self.entries.retain(|(k, _)| k != id);
    }
}

/// 二维码解码：宿主的 `HostQrDecoder` 折成纯函数（zxing 只有 Java 版，这一件事留在 Kotlin）。
pub type QrDecodeFn = Arc<dyn Fn(Vec<u8>) -> Option<String> + Send + Sync>;

/// [`NetQrHost`] 的 UniFFI 句柄：平台对象（`FfiBiliQr` / `FfiZhihuQr` / …）要的就是它
/// （那些构造函数收 `Arc<dyn HostQrLogin>`，所以这里**实现那个 trait**，过边界只是套一层）。
#[derive(uniffi::Object)]
pub struct FfiNetQrHost {
    inner: Arc<NetQrHost>,
}

#[uniffi::export]
impl FfiNetQrHost {
    /// 取消在飞的那一跳（`HostedCall.blocking` 在协程取消时调）。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// `ctx.cookies.all()`（知乎 / 抖音的扫码要从本机设备 Cookie 起步）。
    pub fn all(&self) -> Vec<crate::host::FfiQrCookie> {
        crate::host::HostQrLogin::all(self.inner.as_ref())
    }

    /// 开一个空的临时罐，返回句柄（`qr-1` / `qr-2`…）。
    pub fn begin(&self) -> String {
        crate::host::HostQrLogin::begin(self.inner.as_ref())
    }

    /// 用临时罐发一个请求（**同步**；Kotlin 在 `Dispatchers.IO` 上调）。
    pub fn execute(
        &self,
        session: String,
        request: crate::host::FfiHttpRequest,
        timeout_ms: Option<i64>,
    ) -> Result<crate::host::FfiHttpResponse, crate::host::FfiHostError> {
        crate::host::HostQrLogin::execute(self.inner.as_ref(), session, request, timeout_ms)
    }

    /// 临时罐里的全部 Cookie（`jar.all()` 的顺序）。
    pub fn cookies(&self, session: String) -> Vec<crate::host::FfiQrCookie> {
        crate::host::HostQrLogin::cookies(self.inner.as_ref(), session)
    }

    /// `jar.find(url, name)`。
    pub fn find(&self, session: String, url: String, name: String) -> Option<String> {
        crate::host::HostQrLogin::find(self.inner.as_ref(), session, url, name)
    }

    /// `jar.put(url, cookies)`。
    pub fn put(&self, session: String, url: String, cookies: Vec<crate::host::FfiQrCookie>) {
        crate::host::HostQrLogin::put(self.inner.as_ref(), session, url, cookies)
    }

    /// 丢掉临时罐。
    pub fn end(&self, session: String) {
        crate::host::HostQrLogin::end(self.inner.as_ref(), session)
    }

    /// 落地：先删旧登录 Cookie（域 + 名单），再按顺序写新的。
    pub fn commit(
        &self,
        url: String,
        remove_domain: String,
        remove_names: Vec<String>,
        cookies: Vec<crate::host::FfiQrCookie>,
    ) {
        crate::host::HostQrLogin::commit(self.inner.as_ref(), url, remove_domain, remove_names, cookies)
    }

    /// 二维码图片 → 里面的文字（zxing 只有 Java 版，所以这一条回到宿主）。
    pub fn decode_qr_image(&self, png: Vec<u8>) -> Option<String> {
        crate::host::HostQrLogin::decode_qr_image(self.inner.as_ref(), png)
    }
}

/// 临时罐的 `CookieHost` 门（和 [`JarCookies`] 逐条一样，只是罐子是临时的）。
struct QrSessionJar {
    jar: Arc<CookieJar>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl feedhub_net::CookieHost for QrSessionJar {
    fn cookies(&self, url: &str) -> Vec<(String, String)> {
        let Some(u) = jar_url(url) else { return Vec::new() };
        self.jar
            .load_for_request(&u, (self.now_ms)())
            .into_iter()
            .map(|c| (c.name, c.value))
            .collect()
    }

    fn set_cookies(&self, url: &str, set_cookies: Vec<String>) {
        let Some(u) = jar_url(url) else { return };
        let now = (self.now_ms)();
        let parsed = cookies::parse_set_cookies(&u, &set_cookies, now);
        if parsed.is_empty() {
            return;
        }
        // 和正式罐一样：丢一次落盘不该毁掉用户正在看的这一页（内存已经改了）
        let _ = self.jar.save_from_response(&parsed, now);
    }

    fn put(&self, url: &str, cookies: Vec<feedhub_net::CookieSpec>) {
        let Some(_u) = jar_url(url) else { return };
        let now = (self.now_ms)();
        let stored: Vec<StoredCookie> = cookies.into_iter().map(spec_to_stored).collect();
        let _ = self.jar.save_from_response(&stored, now);
    }
}

fn spec_to_stored(c: feedhub_net::CookieSpec) -> StoredCookie {
    StoredCookie {
        name: c.name,
        value: c.value,
        domain: c.domain,
        path: c.path,
        expires_at_ms: c.expires_at_ms,
        secure: c.secure,
        http_only: c.http_only,
        host_only: c.host_only,
    }
}

fn stored_to_qr_ffi(c: &StoredCookie) -> crate::host::FfiQrCookie {
    crate::host::FfiQrCookie {
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.clone(),
        path: c.path.clone(),
        expires_at_ms: c.expires_at_ms,
        secure: c.secure,
        http_only: c.http_only,
        host_only: c.host_only,
    }
}

fn qr_ffi_to_stored(c: crate::host::FfiQrCookie) -> StoredCookie {
    StoredCookie {
        name: c.name,
        value: c.value,
        domain: c.domain,
        path: c.path,
        expires_at_ms: c.expires_at_ms,
        secure: c.secure,
        http_only: c.http_only,
        host_only: c.host_only,
    }
}

impl NetQrHost {
    pub fn new(
        spec: HostSpec,
        client: Arc<NetClient>,
        jar: Arc<CookieJar>,
        limiter: Arc<PlatformLimiter>,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        decode_qr: Option<QrDecodeFn>,
    ) -> Self {
        Self {
            spec,
            jar,
            now_ms,
            limiter,
            client,
            sessions: Mutex::new(QrSessions::new()),
            in_flight: Mutex::new(None),
            decode_qr,
        }
    }

    /// 取消在飞的那一跳（`HostedCall.blocking` 的取消路径；语义同 `Call.cancel()`）。
    pub fn cancel(&self) {
        let slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = slot.as_ref() {
            t.cancel();
        }
    }

    fn session_jar(&self, session: &str) -> Option<Arc<CookieJar>> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
    }

    /// 把一次扫码请求翻成 [`NetRequest`]：**平台那份 spec + 临时罐**。
    ///
    /// `proxy` 是 `None`：扫码请求和接口请求用同一份 `ProxyPlan`（按 `platform` 查），
    /// 所以"这个平台的代理 / fail-closed"一个字都没变。
    fn to_net_request(
        &self,
        session: &str,
        request: HttpRequest,
        timeout_ms: Option<i64>,
    ) -> NetRequest {
        let mut priority = Priority::Background;
        let mut headers: Vec<(String, String)> = Vec::with_capacity(request.headers.len());
        for (name, value) in request.headers {
            if name.eq_ignore_ascii_case(PRIORITY_HEADER) {
                if value.eq_ignore_ascii_case(PRIORITY_INTERACTIVE) {
                    priority = Priority::Interactive;
                }
            } else {
                headers.push((name, value));
            }
        }
        let temp = self.session_jar(session).map(|j| {
            Arc::new(QrSessionJar { jar: j, now_ms: self.now_ms.clone() })
                as Arc<dyn feedhub_net::CookieHost>
        });
        NetRequest {
            platform: self.spec.platform.clone(),
            method: request.method,
            url: request.url,
            headers,
            body: request.body,
            priority,
            cancel: Arc::new(CancelToken::new()),
            // `timeout_ms` 是**这一跳**的读超时（贴吧 `unicast` 长轮询）；没给就用平台 spec 上的
            read_timeout: timeout_ms
                .filter(|ms| *ms > 0)
                .map(|ms| Duration::from_millis(ms as u64))
                .or_else(|| {
                    if self.spec.read_timeout_ms > 0 {
                        Some(Duration::from_millis(self.spec.read_timeout_ms as u64))
                    } else {
                        None
                    }
                }),
            proxy: None,
            observer: None,
            extra_default_headers: self.spec.default_headers.clone(),
            default_headers_hosts: self.spec.default_headers_hosts.clone(),
            follow_redirects: Some(self.spec.follow_redirects),
            cookies: temp,
            // **和接口请求共用一个排队器**（`MinIntervalInterceptor` 只有一个实例）
            limiter: Some(self.limiter.clone() as Arc<dyn feedhub_net::LimiterHost>),
            gate: None,
            after_limit: None,
            after_headers: None,
        }
    }
}

impl crate::host::HostQrLogin for NetQrHost {
    fn all(&self) -> Vec<crate::host::FfiQrCookie> {
        self.jar.all().iter().map(stored_to_qr_ffi).collect()
    }

    fn begin(&self) -> String {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin((self.now_ms)())
    }

    fn execute(
        &self,
        session: String,
        request: crate::host::FfiHttpRequest,
        timeout_ms: Option<i64>,
    ) -> Result<crate::host::FfiHttpResponse, crate::host::FfiHostError> {
        let req = self.to_net_request(
            &session,
            HttpRequest {
                method: request.method,
                url: request.url,
                headers: request.headers.into_iter().map(|h| (h.name, h.value)).collect(),
                body: request.body,
            },
            timeout_ms,
        );
        // 挂到"在飞槽"上：`cancel()` 掐的就是它（和 `NetHttpHost::execute` 同一个做法）。
        // 请求结束（成功 / 失败 / 取消）都要清掉。
        {
            let mut slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            *slot = Some(req.cancel.clone());
        }
        let result = self.client.execute(req);
        {
            let mut slot = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            *slot = None;
        }
        let resp = result.map_err(to_ffi_host_error)?;
        Ok(crate::host::FfiHttpResponse {
            status: resp.status,
            headers: resp
                .headers
                .into_iter()
                .map(|(name, value)| crate::host::FfiHeader { name, value })
                .collect(),
            body: resp.body,
            final_url: resp.final_url,
        })
    }

    fn cookies(&self, session: String) -> Vec<crate::host::FfiQrCookie> {
        match self.session_jar(&session) {
            Some(j) => j.all().iter().map(stored_to_qr_ffi).collect(),
            None => Vec::new(),
        }
    }

    fn find(&self, session: String, url: String, name: String) -> Option<String> {
        let jar = self.session_jar(&session)?;
        let u = jar_url(&url)?;
        jar.find(&u, &name, (self.now_ms)())
    }

    fn put(&self, session: String, url: String, cookies: Vec<crate::host::FfiQrCookie>) {
        if cookies.is_empty() {
            return;
        }
        let Some(jar) = self.session_jar(&session) else { return };
        // URL 只为对齐 Kotlin 的签名（属性里已经带 domain / path），和 `JarCookies::put` 一样
        let _ = jar_url(&url);
        let now = (self.now_ms)();
        let stored: Vec<StoredCookie> = cookies.into_iter().map(qr_ffi_to_stored).collect();
        let _ = jar.save_from_response(&stored, now);
    }

    fn end(&self, session: String) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end(&session);
    }

    /// 落地：**先**删旧登录 Cookie（域 + 名单），**再**按顺序写新的（`commitQrLogin` 的语义）。
    fn commit(
        &self,
        _url: String,
        remove_domain: String,
        remove_names: Vec<String>,
        cookies: Vec<crate::host::FfiQrCookie>,
    ) {
        let now = (self.now_ms)();
        // ① 先删：旧登录 Cookie 不清掉的话，"名字不在这批里的"旧账号 Cookie（比如上一个账号的
        //    `DedeUserID`）会活下来——那就是两个账号混在一起。
        let _ = self.jar.remove(&remove_domain, &remove_names, now);
        // ② 再写：属性逐条照搬（`secure` / `httpOnly` / `hostOnly` 影响 `matches(url)`）
        if cookies.is_empty() {
            return;
        }
        let stored: Vec<StoredCookie> = cookies.into_iter().map(qr_ffi_to_stored).collect();
        let _ = self.jar.save_from_response(&stored, now);
    }

    fn decode_qr_image(&self, png: Vec<u8>) -> Option<String> {
        // zxing 只有 Java 版，所以这一条**仍然回宿主**（`RustQrHost` 那一半）
        self.decode_qr.as_ref().and_then(|f| f(png))
    }
}
