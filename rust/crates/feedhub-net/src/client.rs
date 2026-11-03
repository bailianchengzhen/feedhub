//! **`NetClient`**：真正的收发。
//!
//! 连接池 / 重定向 / 代理隧道 / 解压 / 超时 / 连接复用**全部用 `wreq` 自带的**。这一层只负责：
//!
//! 1. **按指纹档案建客户端**（[`crate::profile`]）——桌面和手机同一份形状；
//! 2. **执行代理判定**（[`crate::proxy::ProxyPlan`]）——不可用时在 DNS / TCP **之前**失败（fail-closed）；
//! 这一层只在正确的位置调；
//! 4. **把响应原样带回去**（状态码 / 有序头 / body / `final_url` / 协议）——判定在平台层；
//! 5. **取消**（[`CancelToken`]）——OkHttp `Call.cancel()` 的语义；
//! 6. **并发上限**（64 总 / 每 host 5，OkHttp `Dispatcher` 的口径）。
//!
//! # 和 OkHttp 的对照
//!
//! | 项 | OkHttp 4.12 | 这里 |
//! |---|---|---|
//! | 连接池 | 5 条空闲 / 5 分钟 | [`POOL_MAX_IDLE_PER_HOST`] + [`POOL_IDLE`]；h2 一条多路复用 |
//! | 超时 | connect 15s / read 30s / call 0 | [`CONNECT_TIMEOUT`] / [`READ_TIMEOUT`]；**不设总超时** |
//! | 重定向 | 20 次上限；303→GET，301/302 非 GET/HEAD→GET，307/308 保 body | `redirect::Policy::limited(20)`（wreq 自己按方法改；测试逐个验） |
//! | gzip | 自己加 `Accept-Encoding: gzip`，**只**解 gzip；有 `Range` 时不加 | `gzip` feature（同样只 gzip；tower-http 同样让位给 `Range`） |
//! | 重试 | 只重试"连接阶段"和"刚复用的连接立刻 EOF" | [`connect_only_retry`] 收窄到这两类 |
//! | 取消 | h2 发 `RST_STREAM(CANCEL)`，h1 关 socket | drop future（关流）+ token 检查 |
//! | 并发 | 64 总 / 每 host 5 | [`CONCURRENCY`] |
//! | 判断状态码 | 不判 | **不判**：原样返回，风控在平台层 |
//!
//! # 风控红线
//!
//! 这一层**不重试** 403 / 412 / 429 / 验证码，**不换参数**、**不伪造身份**。重试只发生在
//! "连接还没建立"和"刚复用的连接立刻 EOF"这两种和请求内容无关的场景（OkHttp 同）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wreq::header::{HeaderMap, HeaderName, HeaderValue, OrigHeaderMap};
use wreq::redirect;
use wreq::retry;

use crate::error::{NetError, NetworkKind};
use crate::hosts::{CookieHost, LimitRejected, LimiterHost, NoCookies, NoLimiter, Priority};
use crate::profile::TlsProfile;
use crate::proxy::{Proxy, ProxyPlan};
use crate::rt;

// ===========================================================================
// 常量（照 OkHttp 4.12 的值）
// ===========================================================================

/// 连接超时（OkHttp `connectTimeout(15, SECONDS)`；**TLS 握手算在里面**）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 读超时（OkHttp `readTimeout(30, SECONDS)`）。
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 空闲连接保留（OkHttp `ConnectionPool` 默认 5 分钟）。
pub const POOL_IDLE: Duration = Duration::from_secs(5 * 60);
/// 每个 host 的空闲连接上限（OkHttp `ConnectionPool` 默认 5 条）。
pub const POOL_MAX_IDLE_PER_HOST: usize = 5;
/// OkHttp 没设 `userAgent` 时自己补的那一个（`BridgeInterceptor` 的兜底）。
pub const OKHTTP_USER_AGENT: &str = "okhttp/4.12.0";

/// 空值头原样传（**线路上的那个空格由 vendored hyper 的一行补丁保证**，这一层不要自己补）。
///
/// OkHttp 的 `Http1ExchangeCodec` 写的是 `name + ": " + value + "\r\n"`，所以值为空串时线上是
/// `name: \r\n`（冒号 + 一个空格）。hyper 的 `write_full_header_line`（`OrigHeaderMap` 那条
/// "原样大小写"的路）原本写的是 `name:\r\n`（少一个空格），`vendor/hyper/src/proto/h1/role.rs`
/// 的空值分支把那一个空格补上了。
///
/// 踩过两次，所以这里留个记号：① 第一次以为 hyper 会补、什么都不改（少一个空格，贴吧第 497 字节）；
/// ② 第二次以为 hyper 不补、直接把值改成 `" "`，但当时**同时**还在用另一条会 trim 的路径
/// （变成两个空格）。现在这条有原始字节门钉着（`behavior_server` 的 `raw_head`）。
fn wire_header_value(value: &str) -> &str {
    value
}

/// 重定向上限（OkHttp `MAX_FOLLOW_UPS = 20`）。
pub const MAX_REDIRECTS: usize = 20;
/// TLS 会话缓存每个 host 留几份（OkHttp 的 `Address` 各有一份；8 够覆盖一家平台的多个域名）。
pub const SESSION_CACHE_PER_HOST: usize = 8;

/// 并发上限（OkHttp `Dispatcher` 默认 `maxRequests=64` / `maxRequestsPerHost=5`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Concurrency {
    pub total: usize,
    pub per_host: usize,
}

/// [`Concurrency`] 的默认值。
pub const CONCURRENCY: Concurrency = Concurrency { total: 64, per_host: 5 };

// ===========================================================================
// 请求 / 响应
// ===========================================================================

/// 一个要发出去的请求。**字段和 `feedhub-core/src/host.rs` 的 `HttpRequest` 逐个对齐**
/// ，多了 `platform` / `priority` / `cancel` / `read_timeout`。
#[derive(Clone, Default)]
pub struct NetRequest {
    /// 平台 id（`bilibili` / `zhihu` …）。用来查代理和限速分桶。
    pub platform: String,
    /// 方法名（任意大小写；内部按 RFC 7231 的 token 解析，惯例是 `GET` / `POST`）。
    pub method: String,
    /// **完整 URL**（带 query、已经签好名）。
    pub url: String,
    /// **有序**请求头：按这个顺序写进去，且**不再补同名头**。重名允许（按顺序保留）。
    pub headers: Vec<(String, String)>,
    /// body 字节；空 = 没有 body。
    pub body: Vec<u8>,
    /// 优先级（只影响限速队列的排队）。
    pub priority: Priority,
    /// 取消令牌。
    pub cancel: Arc<CancelToken>,
    /// 这一跳的读超时覆盖（贴吧 `unicast` 长轮询用；`None` = 客户端默认 30 s）。
    pub read_timeout: Option<Duration>,
    /// 额外观察者（响应回来时调一次，**只读**；性能记录 / 排障用）。
    pub observer: Option<Arc<dyn Fn(&NetResponse) + Send + Sync>>,
    /// 平台默认头里**请求本身还没有**的那几个（对应 Kotlin 的 `DefaultHeadersInterceptor`）。
    ///
    /// 语义逐条照它：**同名的不覆盖**（请求自己的优先），补在请求头**之后**、`Cookie` / `Host` /
    /// `Connection` / `Accept-Encoding` **之前**——就是 OkHttp 上那层应用拦截器的位置。
    /// 平台层大部分请求都自己带全了头（Rust 平台代码是"最终落到线上的顺序"一次写全），
    /// 这里给的是**页面 / 清单**那几条只写了 `Accept` 的请求用的兜底。
    pub extra_default_headers: Vec<(String, String)>,
    /// 上面那张表**只对这些主机生效**（`DefaultHeadersInterceptor` 的 `hostFilter`）。
    ///
    /// 空 = 全部生效。贴吧那条是"`baidu.com` 和它的子域"，酷安是"`API_HOSTS` 那几个"——
    /// 漏掉它会往**不该补的地方**补头（贴吧的 `/c/f/forum/forumGuide` 走 App 客户端、
    /// 一个默认头都不该有；不带过滤就会多出 UA / Referer，`TiebaWireEquivalenceTest` 抓出来的）。
    pub default_headers_hosts: Vec<String>,
    /// 这一跳跟不跟重定向（`None` = 客户端默认的 20 次）。
    ///
    /// `Some(false)` 对应 OkHttp 的 `followRedirects(false)` + `followSslRedirects(false)`：
    /// 酷安的 `biliLink`（要自己读 `Location`）和贴吧扫码的临时罐客户端都是这个口径。
    pub follow_redirects: Option<bool>,
    /// 发出**之前**的最后一道闸（cli 的冷却 / 预算 / 计数、App 的 `onRequest` 计数）。
    ///
    /// 位置照现在的拦截器链：**在代理判定之前**（被这一道拦下的请求，`onRequest` 也数过了）。
    /// 返回 `Err` 时一个字节都不发。
    pub gate: Option<Arc<dyn PreSend>>,
    /// 拿到响应头、**已经收下 `Set-Cookie`** 之后调一次（在读 body 之前，和 OkHttp 的
    /// `BridgeInterceptor` 顺序一致）。返回 `Err` 时这次调用的结果就是那个错——
    /// cli 的"403 / 412 / 429 写冷却 + 抛风控"就走这里。
    pub after_headers: Option<Arc<dyn PostHeaders>>,
    /// 这个请求用哪一个 Cookie 罐（`None` = 用客户端建时给的那一份）。
    ///
    /// 为什么是逐请求的：一个平台一个罐子句柄、但**连接池要共用一份**（OkHttp 那边
    /// `SourceContext.client(platform)` 是从基础客户端 `newBuilder()` 派生的，连接池是同一个）。
    /// 一份 `NetClient` 服务全部平台 + 逐请求换罐子，正好对上那个语义。
    pub cookies: Option<Arc<dyn CookieHost>>,
    /// 这个请求用哪一个限速器（`None` = 用客户端建时给的那一份）。理由同上：每家一个排队器、
    /// 连接池共用。
    pub limiter: Option<Arc<dyn LimiterHost>>,
    /// 这一跳**显式指定**的代理档（`None` = 按 [`NetRequest::platform`] 去 `ProxyPlan` 里查）。
    ///
    /// 为什么需要逐请求的：取订阅那三条路线（直连 / 系统 / 经核心）不是"按平台"分的，而且
    /// "经核心"的端口会随核心起停变；一份 `NetClient` 服务三条路线，逐请求给档位比建三份客户端省事
    /// （也保住了连接池共用）。**判定仍然只有一处**：档位是宿主按 `feedhub-core/src/proxy.rs` 的规则算好传进来的。
    pub proxy: Option<Proxy>,
    /// 限速放行之后、**拼请求之前**的最后一次改写机会（X 的 `x-client-transaction-id`）。
    ///
    /// 位置必须在这里：txid 里含一个"当前时刻"，
    /// 又必须在拼 `HeaderMap` / `OrigHeaderMap` 之前，这样追加的头才会落到"平台头之后、
    /// `Cookie` 之前"——和 OkHttp 上那头拦截器在链尾的位置逐字节一样。
    pub after_limit: Option<Arc<dyn AfterLimit>>,
}

/// 限速放行之后的改写（现在只有 X 的 txid）。
///
/// 可以**追加**头（`headers.push`），不要重排已有的头：顺序是线上字节的一部分。
pub trait AfterLimit: Send + Sync {
    fn after(
        &self,
        platform: &str,
        method: &str,
        url: &str,
        headers: &mut Vec<(String, String)>,
    ) -> Result<(), NetError>;

    /// 限速器放行时"在队列里等了多久"（毫秒），在 [`after`](Self::after) **之前**调一次。
    ///
    /// 给 `queue_ms` 记账用（性能记录）；默认什么都不做。
    fn waited(&self, _wait_ms: i64) {}
}

/// 发请求之前的一道闸（cli 的 `CliGateInterceptor` 前半段 / App 的 `onRequest` 计数）。
///
/// **同步、在调用线程上**（和 `LimiterHost` 一样）：它要做的事是"查冷却文件 / 数个数"，
/// 不该为它多一次线程切换。
pub trait PreSend: Send + Sync {
    /// [`url`] 是**完整 URL**（带 query，可能有签名）：实现里**不要**把它写进日志。
    fn before(&self, platform: &str, method: &str, url: &str) -> Result<(), NetError>;
}

/// 拿到响应头之后的钩子（cli 的 `CliGateInterceptor` 后半段：风控信号 → 写冷却 → 抛）。
pub trait PostHeaders: Send + Sync {
    /// 只看状态码 / 头名，**不看 body**（这时候 body 还没读）。
    fn after(&self, platform: &str, url: &str, status: u16, headers: &[(String, String)])
        -> Result<(), NetError>;
}

impl std::fmt::Debug for NetRequest {
    /// **不打印 URL / 头 / body**（可能带签名 / token）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetRequest")
            .field("platform", &self.platform)
            .field("method", &self.method)
            .field("headers", &self.headers.len())
            .field("body_len", &self.body.len())
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

impl NetRequest {
    /// 一个 GET（`platform` 用来查代理 / 限速）。
    pub fn get(platform: &str, url: &str) -> Self {
        Self {
            platform: platform.to_string(),
            method: "GET".to_string(),
            url: url.to_string(),
            cancel: Arc::new(CancelToken::new()),
            ..Default::default()
        }
    }

    /// 一个 POST。
    pub fn post(platform: &str, url: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            platform: platform.to_string(),
            method: "POST".to_string(),
            url: url.to_string(),
            body: body.into(),
            cancel: Arc::new(CancelToken::new()),
            ..Default::default()
        }
    }

    /// 挂一个有序头（**按调用顺序**写进请求）。
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// 换一个取消令牌。
    pub fn with_cancel(mut self, cancel: Arc<CancelToken>) -> Self {
        self.cancel = cancel;
        self
    }
}

/// HTTP 协议版本（性能记录 / 排障用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Http1_1,
    Http2,
}

impl Protocol {
    fn from_version(v: http::Version) -> Option<Self> {
        match v {
            http::Version::HTTP_11 => Some(Self::Http1_1),
            http::Version::HTTP_2 => Some(Self::Http2),
            _ => None,
        }
    }
}

/// 一个响应。**字段和 `feedhub-core/src/host.rs` 的 `HttpResponse` 对齐**，多了 `protocol`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetResponse {
    pub status: u16,
    /// **有序**响应头（同名多次按原顺序保留；`Set-Cookie` 不合并）。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 跟随后得到的最终 URL（重定向之后）。**排障用，不进日志**。
    pub final_url: String,
    /// 协商到的协议。
    pub protocol: Option<Protocol>,
}

impl NetResponse {
    /// 第一个叫 `name` 的响应头的值（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 所有叫 `name` 的响应头的值（`Set-Cookie` 用）。
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

// ===========================================================================
// 取消
// ===========================================================================

/// 取消令牌（OkHttp `Call.cancel()` 的语义）。
///
/// `cancel()` 可以从**任意线程**调；取消之后：
/// - 还没开始的请求立刻返回 [`NetError::Cancelled`]；
/// - 正在飞的请求：future 被 drop（= 关流 / 关 socket，h2 由 `http2` 发 `RST_STREAM`）；
/// - 已经在等的限速槽**让出来**（[`LimiterHost::acquire`] 自己看 token）。
pub struct CancelToken {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
    sync_wake: (Mutex<()>, std::sync::Condvar),
}

impl Default for CancelToken {
    fn default() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            sync_wake: (Mutex::new(()), std::sync::Condvar::new()),
        }
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取消（幂等，任意线程可调；立刻唤醒所有 async / 同步等待方，无需轮询）。
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            let _g = self.sync_wake.0.lock().unwrap_or_else(|e| e.into_inner());
            self.sync_wake.1.notify_all();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 在调用线程上最多等 `timeout`；如果期间（或之前）被取消则立刻返回 `true`。
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let guard = self.sync_wake.0.lock().unwrap_or_else(|e| e.into_inner());
        let (_guard, _) = self
            .sync_wake
            .1
            .wait_timeout_while(guard, timeout, |_| !self.is_cancelled())
            .unwrap_or_else(|e| e.into_inner());
        self.is_cancelled()
    }
}

// ===========================================================================
// 客户端
// ===========================================================================

/// 联网客户端。**按"代理快照"整份建**（换代理 = 换一份新池，和
/// `SourceContext.withFreshConnectionPool()` 对齐）。
pub struct NetClient {
    /// 直连的那个 `wreq::Client`（`ProxyPlan` 里 `Direct` 的平台用它）。
    direct: wreq::Client,
    /// 有代理的那几档：**每档一个 client**（= 每档一个连接池）。
    ///
    /// 为什么不是"一次请求建一个 client"：那样每个请求都是冷连接池（没有复用、每次重新握手），
    /// 连接数、时序、TLS 会话恢复全都不对。App 里每个平台的代理是恒定的，所以按**代理档**分池
    /// 正好对上 `PlatformProxySelector`（同一个平台的客户端共用一份池）的语义。
    proxy_clients: Mutex<HashMap<String, wreq::Client>>,
    profile: TlsProfile,
    proxies: ProxyPlan,
    cookies: Arc<dyn CookieHost>,
    limiter: Arc<dyn LimiterHost>,
    gzip: bool,
    cert_store: Option<wreq::tls::trust::CertStore>,
    session_resumption: bool,
    /// 建这份客户端时的超时（每档代理的 `wreq::Client` 也要用同一份）。
    connect_timeout: Duration,
    read_timeout: Duration,
    resolves: Vec<(String, std::net::SocketAddr)>,
    /// 每 host 的并发信号量。
    in_flight: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    /// 全局并发信号量。
    total_in_flight: Arc<tokio::sync::Semaphore>,
    concurrency: Concurrency,
}

impl std::fmt::Debug for NetClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetClient")
            .field("profile", &self.profile.name)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

/// 建客户端的参数（链式）。
pub struct NetClientBuilder {
    profile: TlsProfile,
    proxies: ProxyPlan,
    cookies: Arc<dyn CookieHost>,
    limiter: Arc<dyn LimiterHost>,
    concurrency: Concurrency,
    /// 连接超时（默认 [`CONNECT_TIMEOUT`]）。
    ///
    /// 为什么可调：**本机翻译服务**（`llama-server`）的客户端是 `connectTimeout(2s)`——
    /// 连不上本机端口时不该让用户等 15 秒。
    connect_timeout: Duration,
    /// 读超时（默认 [`READ_TIMEOUT`]）。同一个客户端是 `readTimeout(120s)`：模型生成一段译文要几十秒。
    read_timeout: Duration,
    cert_store: Option<wreq::tls::trust::CertStore>,
    session_resumption: bool,
    gzip: bool,
    /// **只给测试**：域名 → 地址的钉死（生产为空 = 走系统 DNS）。
    resolves: Vec<(String, std::net::SocketAddr)>,
}

impl Default for NetClientBuilder {
    fn default() -> Self {
        Self {
            profile: TlsProfile::okhttp4_12(),
            proxies: ProxyPlan::direct(),
            cookies: Arc::new(NoCookies),
            limiter: Arc::new(NoLimiter),
            concurrency: CONCURRENCY,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            cert_store: None,
            session_resumption: true,
            gzip: true,
            resolves: Vec::new(),
        }
    }
}

impl NetClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn profile(mut self, p: TlsProfile) -> Self {
        self.profile = p;
        self
    }
    pub fn proxies(mut self, p: ProxyPlan) -> Self {
        self.proxies = p;
        self
    }
    pub fn cookies(mut self, c: Arc<dyn CookieHost>) -> Self {
        self.cookies = c;
        self
    }
    pub fn limiter(mut self, l: Arc<dyn LimiterHost>) -> Self {
        self.limiter = l;
        self
    }
    pub fn concurrency(mut self, c: Concurrency) -> Self {
        self.concurrency = c;
        self
    }
    /// 连接超时（默认 [`CONNECT_TIMEOUT`]；本机翻译服务用 2 秒）。
    pub fn connect_timeout(mut self, t: Duration) -> Self {
        self.connect_timeout = t;
        self
    }
    /// 读超时（默认 [`READ_TIMEOUT`]；本机翻译服务用 120 秒，模型生成要几十秒）。
    pub fn read_timeout(mut self, t: Duration) -> Self {
        self.read_timeout = t;
        self
    }
    /// 信任库（测试用本机 CA；生产不调 = 系统 CA：安卓读系统根证书目录，其它系统是 BoringSSL 默认路径，见 [`crate::roots`]）。
    pub fn cert_store(mut self, s: wreq::tls::trust::CertStore) -> Self {
        self.cert_store = Some(s);
        self
    }
    /// 会话恢复（生产 true；测试里关掉抓"首连"形状）。
    pub fn session_resumption(mut self, on: bool) -> Self {
        self.session_resumption = on;
        self
    }
    /// 透明解压 gzip（生产 true）。
    pub fn gzip(mut self, on: bool) -> Self {
        self.gzip = on;
        self
    }

    /// **只给测试**：把 `host` 钉到 `addr`（不写 `/etc/hosts`、不发 DNS 请求）。
    ///
    /// 生产路径**不用它**：DNS 是系统解析（`wreq` 默认的 `GaiResolver`），
    /// 这一档只是让指纹测试能只连回环。
    pub fn resolve(mut self, host: &str, addr: std::net::SocketAddr) -> Self {
        self.resolves.push((host.to_string(), addr));
        self
    }

    pub fn build(mut self) -> Result<NetClient, NetError> {
        // 没显式给信任库：安卓上换成系统根证书（BoringSSL 的默认路径 `/etc/ssl` 在安卓上不存在，
        // 不换的话每个 HTTPS 握手都校验失败），其它系统还是 BoringSSL 默认路径。见 [`crate::roots`]。
        if self.cert_store.is_none() {
            self.cert_store = crate::roots::system_cert_store();
        }
        let direct = self.build_one(&Proxy::Direct)?;
        Ok(NetClient {
            direct,
            proxy_clients: Mutex::new(HashMap::new()),
            profile: self.profile,
            proxies: self.proxies,
            cookies: self.cookies,
            limiter: self.limiter,
            gzip: self.gzip,
            cert_store: self.cert_store,
            session_resumption: self.session_resumption,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            resolves: self.resolves,
            in_flight: Mutex::new(HashMap::new()),
            total_in_flight: Arc::new(tokio::sync::Semaphore::new(self.concurrency.total)),
            concurrency: self.concurrency,
        })
    }

    /// 建一份 `wreq::Client`（一个连接池）。
    fn build_one(&self, proxy: &Proxy) -> Result<wreq::Client, NetError> {
        let tls = self.profile.tls_options(self.session_resumption, self.cert_store.clone());
        let h2 = crate::profile::http2_options();

        let mut b = wreq::Client::builder()
            // ---- TLS / h2 形状（golden）
            .tls_options(tls)
            .http2_options(h2)
            // ---- 会话恢复（OkHttp 默认做；wreq 的 per-host LRU）
            .tls_session_cache(wreq::tls::session::LruTlsSessionCache::new(SESSION_CACHE_PER_HOST))
            // ---- 超时（OkHttp 的默认值；**不设总超时**，OkHttp 的 callTimeout 也是 0）
            .connect_timeout(self.connect_timeout)
            .read_timeout(self.read_timeout)
            // ---- 连接池（OkHttp ConnectionPool 默认：每 host 5 条空闲 / 5 分钟）
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .pool_idle_timeout(POOL_IDLE)
            // ---- 重定向：20 次（OkHttp MAX_FOLLOW_UPS）
            .redirect(redirect::Policy::limited(MAX_REDIRECTS))
            // ---- 重试：只重试"连接建立阶段"（见 connect_only_retry）
            .retry(connect_only_retry())
            // ---- 解压：只 gzip（OkHttp 只做 gzip，**不加 br**）
            .gzip(self.gzip)
            .no_brotli()
            .no_deflate()
            .no_zstd()
            // ---- 代理：这一份客户端只按这一档走（判定在 feedhub-core，这一层照办）
            .no_proxy();
        if let Some(p) = proxy.to_wreq() {
            b = b.proxy(p);
        }
        if let Some(store) = self.cert_store.clone() {
            b = b.tls_cert_store(store);
        }
        // **只给测试**：钉死域名解析（生产是空的）
        for (host, addr) in &self.resolves {
            b = b.resolve(host.clone(), *addr);
        }
        b.build().map_err(|e| NetError::Other { detail: e.to_string() })
    }
}

/// 只重试"连接建立阶段失败"（OkHttp `retryOnConnectionFailure` 的口径）。
///
/// **wreq 的层次顺序**：`RetryLayer` 在 `FollowRedirectLayer` **外面**（`client.rs` 的 build 里
/// 先 `.layer(RetryLayer)` 再 `.layer(FollowRedirectLayer)`），所以一个"整条重定向链"在
/// 重试层看来只是**一次**请求。这意味着**不能用 `status().is_none()` 当判据**：
/// 重定向次数超限时 `FollowRedirectLayer` 返回的是 `Error`（没有状态码），会被当成可重试，
/// 于是整条链重跑 3 遍——实测 `limited(20)` 变成了 63 个请求。
///
/// 正确判据（`ReqRep::error()` 拿得到错误本体）：
/// - **只重试连接类错误**：`is_connect()` / `is_connection_reset()` / `is_dns()`
///   （OkHttp 的 `retryOnConnectionFailure` 也是这几类）；
/// - **不重试**：重定向（`is_redirect`）、超时（OkHttp 的超时是 `SocketTimeoutException`，
///   它确实会重试"连接超时"，但**不重试读超时**——这里保守起见一律不重试，宁可让它失败）、
///   状态码（`error_for_status` 我们不用，拿到了响应就 `success()`）。
///
/// 风控红线：**拿到响应之后绝不重试**（403 / 412 / 429 一个字节都不许重发）。
fn connect_only_retry() -> retry::Policy {
    retry::Policy::default()
        .no_budget()
        .max_retries_per_request(2)
        .classify_fn(|req_rep| {
            if req_rep.status().is_some() {
                // 拿到响应了：绝不重试
                return req_rep.success();
            }
            match req_rep.error().and_then(|e| e.downcast_ref::<wreq::Error>()) {
                // 连接建立阶段 / 刚复用的连接立刻 EOF：OkHttp 会换路线重试
                Some(e) if e.is_connect() || e.is_connection_reset() || e.is_dns() => {
                    req_rep.retryable()
                }
                // 其它（重定向超限 / 解码 / 请求构造 / 读超时……）：不重试
                _ => req_rep.success(),
            }
        })
}

impl NetClient {
    pub fn builder() -> NetClientBuilder {
        NetClientBuilder::new()
    }

    /// 这份客户端的指纹档案名（写结果 / 日志用）。
    pub fn profile_name(&self) -> &'static str {
        self.profile.name
    }

    /// 这份客户端的代理快照（测试用）。
    pub fn proxies(&self) -> &ProxyPlan {
        &self.proxies
    }

    /// **发一个请求、等响应**（同步；在调用线程上 block）。
    ///
    /// 步骤（顺序照现在的 OkHttp 拦截器链）：
    /// 1. 取消检查；
    /// 2. [`NetRequest::gate`]（`onRequest` 计数 / cli 的冷却与预算）——**在代理判定之前**；
    /// 3. 代理判定（**fail-closed：一个字节都不发**）；
    /// 4. 限速（[`LimiterHost::acquire`]，在建立连接之前）；
    /// 5. 并发上限（64 总 / 每 host 5）；
    /// 6. 拼请求（平台头 → 默认头 → Cookie → OkHttp 那三个 h1 头）、发、取消；
    /// 7. 拿到响应头就 [`NetRequest::after_headers`]（cli 的风控），再读 body，原样带回来。
    pub fn execute(&self, req: NetRequest) -> Result<NetResponse, NetError> {
        let prepared = self.prepare_send(&req)?;
        let out = self.send_and_read(prepared)?;
        if let Some(obs) = &req.observer {
            obs(&out);
        }
        Ok(out)
    }

    /// **发一个请求、把 body 当流读出来**（同步入口：拿到响应头之后返回一个 [`NetStream`]）。
    ///
    /// 为什么需要它：桌面播放器的分片中转是"上游边收边转发给播放器"的——整段缓冲会把一个大分片
    /// 全塞进内存，而且播放器一断开就没法立刻停掉上游（那正是"拖动进度条之后后台还在下载"）。
    ///
    /// 除 body 之外**和 [`Self::execute`] 逐条一样**（同一个拦截器链的顺序：闸 → 代理 → 限速 → 并发 →
    /// 拼头 → 发；`Set-Cookie` 在读 body 之前落；`after_headers` 在拿到头时调）。
    pub fn execute_stream(&self, req: NetRequest) -> Result<NetStream, NetError> {
        let prepared = self.prepare_send(&req)?;
        let cancel = prepared.cancel.clone();
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, NetError>>(STREAM_CHUNKS);
        rt::shared().spawn(async move {
            pump(prepared, head_tx, chunk_tx).await;
        });
        // 等"响应头到手"（这一步是阻塞的：调用线程就是播放器中转的工作线程）
        let head = match rt::block_on(head_rx) {
            Ok(h) => h,
            // 任务还没发 head 就没了（panic / runtime 关了）：当成一次传输错误
            Err(_) => return Err(NetError::Other { detail: "流式响应提前结束".into() }),
        }?;
        Ok(NetStream {
            status: head.status,
            headers: head.headers,
            final_url: head.final_url,
            protocol: head.protocol,
            chunk_rx,
            cancel,
        })
    }

    /// 拼请求并做前面那几道闸（[`Self::execute`] / [`Self::execute_stream`] 共用）。
    fn prepare_send(&self, req: &NetRequest) -> Result<Prepared, NetError> {
        if req.cancel.is_cancelled() {
            return Err(NetError::Cancelled);
        }

        // ---- 1. 最后一道闸（cli 的冷却 / 预算 / 计数；App 的 onRequest）。位置在代理判定之前：
        // 被这一道拦下的请求，宿主那边的"试过发请求"计数也已经涨过了（和 OkHttp 的链序一致）。
        if let Some(gate) = &req.gate {
            gate.before(&req.platform, &req.method, &req.url)?;
        }

        // ---- 2. 代理（fail-closed 在 DNS / TCP 之前，一个字节都不发）
        let proxy = match &req.proxy {
            Some(p) => p.clone(),
            None => self.proxies.for_platform(&req.platform).clone(),
        };
        if let Proxy::Unavailable { reason } = proxy {
            return Err(NetError::ProxyUnavailable { detail: reason });
        }

        // ---- 3. **离线闸**（`FEEDHUB_OFFLINE=1`）：只允许回环，其余一律在发请求之前失败。
        if offline_blocked_for(&req.url, &proxy) {
            return Err(NetError::Network {
                detail: "离线模式（FEEDHUB_OFFLINE=1）：只允许回环地址，这个请求没有发出去".into(),
                kind: NetworkKind::Other,
            });
        }

        // ---- 4. 限速
        let limiter: &dyn LimiterHost = match &req.limiter {
            Some(l) => l.as_ref(),
            None => self.limiter.as_ref(),
        };
        let limiter_wait_ms = limiter
            .acquire_with_wait(&req.platform, req.priority, &req.cancel)
            .map_err(|e| match e {
                LimitRejected::Cancelled => NetError::Cancelled,
                LimitRejected::Blocked { detail } => NetError::ProxyUnavailable { detail },
            })?;
        if let Some(hook) = &req.after_limit {
            hook.waited(limiter_wait_ms);
        }

        // ---- 5. 并发上限（守卫放进 `Prepared`，直到响应体读完 / 流关闭才释放）
        let host = host_key(&req.url)?;
        let slot =
            Slot::acquire(&self.total_in_flight, &self.in_flight, &host, self.concurrency, &req.cancel)?;

        // ---- 6. 限速之后、拼请求之前的改写（X 的 txid：这里取时间才是"睡完了再算"）
        let mut platform_headers = req.headers.clone();
        if let Some(hook) = &req.after_limit {
            hook.after(&req.platform, &req.method, &req.url, &mut platform_headers)?;
        }

        // ---- 7. 拼
        let (method, uri) = parse_method_url(req)?;
        let cookie_host: &dyn CookieHost = match &req.cookies {
            Some(c) => c.as_ref(),
            None => self.cookies.as_ref(),
        };
        let cookie_pairs = cookie_host.cookies(&req.url);
        let h1_extra = h1_extra_headers(req, &uri, self.gzip);
        let apply_defaults = default_headers_apply(req, uri.host());
        let headers = header_map(req, &uri, &platform_headers, &cookie_pairs, &h1_extra)?;
        let order = header_order(req, &platform_headers, &cookie_pairs, &h1_extra, apply_defaults)?;

        let client = self.client_for(&proxy)?;
        let mut builder = client.request(method, uri).headers(headers).orig_headers(order);
        if let Some(follow) = req.follow_redirects {
            builder = builder.redirect(if follow {
                redirect::Policy::limited(MAX_REDIRECTS)
            } else {
                redirect::Policy::none()
            });
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body.clone());
        }
        if let Some(t) = req.read_timeout {
            builder = builder.read_timeout(t);
        }
        Ok(Prepared {
            builder,
            cancel: req.cancel.clone(),
            cookie_owner: cookie_owner(req, self),
            after_headers: req.after_headers.clone(),
            platform: req.platform.clone(),
            _slot: slot,
        })
    }

    /// 发出去 + 读完 body（`execute` 用）。
    fn send_and_read(&self, prepared: Prepared) -> Result<NetResponse, NetError> {
        let Prepared { builder, cancel, cookie_owner, after_headers, platform, _slot } = prepared;
        let fut = async move {
            let _keep_slot = _slot;
            let resp = tokio::select! {
                biased;
                () = wait_cancel(&cancel) => return Err(NetError::Cancelled),
                r = builder.send() => r,
            }?;
            let head = head_of(&resp, &cookie_owner, after_headers.as_deref(), &platform)?;
            let body = tokio::select! {
                biased;
                () = wait_cancel(&cancel) => return Err(NetError::Cancelled),
                r = resp.bytes() => r,
            }?;
            Ok::<_, NetError>(head.into_response(Vec::from(body)))
        };
        rt::block_on(fut)
    }

    fn client_for(&self, proxy: &Proxy) -> Result<wreq::Client, NetError> {
        if matches!(proxy, Proxy::Direct) {
            return Ok(self.direct.clone());
        }
        let key = proxy_cache_key(proxy);
        {
            let map = self.proxy_clients.lock().unwrap();
            if let Some(c) = map.get(&key) {
                return Ok(c.clone());
            }
        }
        let built = NetClientBuilder {
            profile: self.profile,
            proxies: ProxyPlan::direct(),
            cookies: Arc::new(NoCookies),
            limiter: Arc::new(NoLimiter),
            concurrency: self.concurrency,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            cert_store: self.cert_store.clone(),
            session_resumption: self.session_resumption,
            gzip: self.gzip,
            resolves: self.resolves.clone(),
        }
        .build_one(proxy)?;
        self.proxy_clients.lock().unwrap().insert(key, built.clone());
        Ok(built)
    }
}

// ===========================================================================
// 流式响应（`execute_stream`）：桌面播放器的分片中转用它
// ===========================================================================

/// 流式响应的分块队列有多深（64 KB 一块，8 块 = 512 KB 的缓冲；够抹平调度抖动，又不会把整个分片攒下来）。
const STREAM_CHUNKS: usize = 8;

/// 已经拼好、过了前六道闸的一次发送（[`NetClient::execute`] / [`NetClient::execute_stream`] 共用）。
struct Prepared {
    builder: wreq::RequestBuilder,
    cancel: Arc<CancelToken>,
    cookie_owner: Arc<dyn CookieHost>,
    after_headers: Option<Arc<dyn PostHeaders>>,
    platform: String,
    _slot: Slot,
}

/// 罐子要活到 future 里（`Arc` 一份，`&dyn` 借不到 `'static` 的 future 里）。
fn cookie_owner(req: &NetRequest, client: &NetClient) -> Arc<dyn CookieHost> {
    match &req.cookies {
        Some(c) => c.clone(),
        None => client.cookies.clone(),
    }
}

/// 响应头那一半（两条路径共用：`Set-Cookie` 先落、再调风控闸）。
struct Head {
    status: u16,
    headers: Vec<(String, String)>,
    final_url: String,
    protocol: Option<Protocol>,
}

impl Head {
    fn into_response(self, body: Vec<u8>) -> NetResponse {
        NetResponse {
            status: self.status,
            headers: self.headers,
            body,
            final_url: self.final_url,
            protocol: self.protocol,
        }
    }
}

fn head_of(
    resp: &wreq::Response,
    cookie_owner: &Arc<dyn CookieHost>,
    after_headers: Option<&dyn PostHeaders>,
    platform: &str,
) -> Result<Head, NetError> {
    let status = resp.status().as_u16();
    let version = resp.version();
    let final_url = resp.uri().to_string();
    let headers = collect_headers(resp.headers());

    // `Set-Cookie` 按响应里的顺序交给罐子。**在读完 body 之前**落（和 OkHttp 一样：
    // body 读失败不该丢掉已经收到的 Cookie）。
    let set_cookies: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect();
    if !set_cookies.is_empty() {
        cookie_owner.set_cookies(&final_url, set_cookies);
    }

    // 风控闸（cli）：只看状态码 / 头名，**不看 body**（body 还没读）。
    if let Some(guard) = after_headers {
        guard.after(platform, &final_url, status, &headers)?;
    }

    Ok(Head {
        status,
        headers,
        final_url,
        protocol: Protocol::from_version(version),
    })
}

/// 发出去、把响应头交给调用方、再把 body 一块一块推进队列（`execute_stream` 的后台那半）。
async fn pump(
    prepared: Prepared,
    head_tx: tokio::sync::oneshot::Sender<Result<Head, NetError>>,
    chunk_tx: tokio::sync::mpsc::Sender<Result<Vec<u8>, NetError>>,
) {
    let Prepared { builder, cancel, cookie_owner, after_headers, platform, _slot } = prepared;
    // 并发名额只占到响应头交出去为止：后面推 body 的快慢取决于读的那一侧（播放器暂停 / 缓冲满 / flush 时
    // 会停着不读），占着不还会让同一 host 的后续分片请求全排在信号量上，DASH 的 seek / 即时变速跟着卡死
    let mut slot = Some(_slot);
    let mut head_tx = Some(head_tx);
    let fail = |head_tx: &mut Option<tokio::sync::oneshot::Sender<Result<Head, NetError>>>, e: NetError| {
        if let Some(tx) = head_tx.take() {
            let _ = tx.send(Err(e));
        }
    };
    let resp = tokio::select! {
        biased;
        () = wait_cancel(&cancel) => {
            fail(&mut head_tx, NetError::Cancelled);
            return;
        }
        r = builder.send() => r,
    };
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            fail(&mut head_tx, NetError::from(e));
            return;
        }
    };
    let head = match head_of(&resp, &cookie_owner, after_headers.as_deref(), &platform) {
        Ok(h) => h,
        Err(e) => {
            fail(&mut head_tx, e);
            return;
        }
    };
    if let Some(tx) = head_tx.take() {
        let _ = tx.send(Ok(head));
    }
    drop(slot.take());
    // 头已经交出去了：后面出错只能靠"队列里塞一个 Err"告诉读的那一侧。
    use futures_util::StreamExt as _;
    let mut body = std::pin::pin!(resp.bytes_stream());
    loop {
        let next = tokio::select! {
            biased;
            () = wait_cancel(&cancel) => {
                let _ = chunk_tx.send(Err(NetError::Cancelled)).await;
                return;
            }
            r = body.next() => r,
        };
        match next {
            Some(Ok(bytes)) => {
                if chunk_tx.send(Ok(Vec::from(bytes))).await.is_err() {
                    // 读的那一侧走了（播放器断开 / 中转关掉）：**立刻停上游**
                    cancel.cancel();
                    return;
                }
            }
            None => return,
            Some(Err(e)) => {
                let _ = chunk_tx.send(Err(NetError::from(e))).await;
                return;
            }
        }
    }
}

/// 一个**边收边读**的响应体（[`NetClient::execute_stream`] 的返回值）。
///
/// 语义就是 OkHttp 的 `Response.body.byteStream()` 那一条：`read` 阻塞到有下一块 / 读完（`Ok(0)`）/
/// 出错。**读的那一侧 `drop` 掉它就等于取消上游**（后台那个 `pump` 发现队列没人收就掐掉请求）。
pub struct NetStream {
    pub status: u16,
    /// **有序**响应头（同名多次按原顺序保留；`Set-Cookie` 不合并）。
    pub headers: Vec<(String, String)>,
    pub final_url: String,
    pub protocol: Option<Protocol>,
    chunk_rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, NetError>>,
    cancel: Arc<CancelToken>,
}

impl NetStream {
    /// 第一个叫 `name` 的响应头的值（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 读下一块（阻塞）。`Ok(0)` = 读完；`Err` = 传输错误 / 被取消。
    pub fn read_chunk(&mut self) -> Result<Vec<u8>, NetError> {
        match rt::block_on(self.chunk_rx.recv()) {
            Some(Ok(bytes)) => Ok(bytes),
            Some(Err(e)) => Err(e),
            None => Ok(Vec::new()),
        }
    }

    /// 取消这一趟（关中转时调；语义同 `Call.cancel()`）。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for NetStream {
    fn drop(&mut self) {
        // 读的一侧不再读了：让 `pump` 发现队列没人收（它会自己掐上游），并直接置上取消
        self.cancel.cancel();
        self.chunk_rx.close();
    }
}

/// 代理档的缓存键（**含地址**——这是内部键，不进日志）。
fn proxy_cache_key(proxy: &Proxy) -> String {
    match proxy {
        Proxy::Direct => "direct".into(),
        Proxy::Http { host, port } => format!("http://{host}:{port}"),
        Proxy::Socks5 { host, port } => format!("socks5://{host}:{port}"),
        Proxy::Socks5h { host, port } => format!("socks5h://{host}:{port}"),
        Proxy::Unavailable { .. } => "unavailable".into(),
    }
}

// ===========================================================================
// h1 的三个补充头
// ===========================================================================

/// **h1 上 OkHttp 自己写、wreq 不会按同样大小写写的三个头**（golden 的 `http1.jsonl`）：
/// `Host` / `Connection: Keep-Alive` / `Accept-Encoding: gzip`。
///
/// 为什么显式补：OkHttp 的 `Http1ExchangeCodec` 把这几个用**固定大小写**写进请求行之后；
/// wreq 那边 `Host` 只在缺的时候补（小写 `host`）、`Connection` 由连接层按需补（`keep-alive` 小写）、
/// `Accept-Encoding` 由 tower-http 补（小写头名）。HTTP/1.1 的头名大小写是**指纹的一部分**
/// （golden 逐字节记下来了），所以这里按 OkHttp 的写法显式给。
///
/// **h2 上**：`Connection` 类头由 `strip_connection_headers` 丢掉；`Host` **没人丢**，所以它只进
/// 顺序表、不进头表（见 [`header_map`]），h1 的 `Host` 由 wreq 发送前补上。
fn h1_extra_headers(req: &NetRequest, uri: &http::Uri, gzip: bool) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    // **`Content-Length` 排在最前面**：OkHttp 的 `BridgeInterceptor` 先写 Content-Type /
    // Content-Length（从 body 推），**之后**才写 Host / Connection / Accept-Encoding。
    // 线上顺序就是 `平台头 → Content-Type → Content-Length → Host → Connection →
    // Accept-Encoding → Cookie`（`BiliAccountEquivalenceTest` 的关注 / 取关门抓出来的：
    // wreq 自己补的 `content-length` 是小写、而且排在最后）。
    if !req.body.is_empty()
        && !req.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-length"))
    {
        out.push(("Content-Length", req.body.len().to_string()));
    }
    if let Some(host) = uri.host() {
        let authority = match uri.port_u16() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        out.push(("Host", authority));
        out.push(("Connection", "Keep-Alive".to_string()));
        if gzip {
            out.push(("Accept-Encoding", "gzip".to_string()));
        }
    }
    out
}

// ===========================================================================
// 工具
// ===========================================================================

fn parse_method_url(req: &NetRequest) -> Result<(http::Method, http::Uri), NetError> {
    let method = http::Method::try_from(req.method.as_str())
        .map_err(|e| NetError::BadRequest { detail: format!("方法名不合法：{e}") })?;
    let uri: http::Uri = req
        .url
        .parse()
        .map_err(|e| NetError::BadRequest { detail: format!("URL 解析不了：{e}") })?;
    if uri.scheme().is_none() || uri.authority().is_none() {
        return Err(NetError::BadRequest { detail: "URL 缺 scheme 或 host".into() });
    }
    Ok((method, uri))
}

/// 这一趟**真正会连到哪儿**是不是回环（离线闸用）。
fn offline_blocked_for(url: &str, proxy: &Proxy) -> bool {
    offline_blocked_via(url, proxy, offline_env())
}

/// [`offline_blocked_for`] 的纯函数版（`offline` 由调用方给，测试才能两条分支都断言）。
///
/// 有代理时字节只发到代理那儿（`Proxy::Http` / `Socks5` 都是先连代理），所以判定看的是**代理的地址**：
/// 代理在本机（测试里的假代理就是本机 MockWebServer）→ 这一次不会出网，放行；代理在别处 → 那还是出网，
/// 拦下。没有代理（直连）时看 URL 的主机名。
///
/// 为什么这么写：离线用例里"假代理 + 假 CDN"是常态（`ProxyWiringTest` / `PlatformProxyRoutingTest`），
/// 目标是永不解析的 `.test` 域名，而请求真的只到 127.0.0.1 的假代理。按 URL 主机名一刀切会把这类
/// **完全离线**的用例全拦掉，反而逼着测试绕开真接线。
///
/// 例外：本机的**真代理**端口（`10808` SOCKS5 / `10809` HTTP，也是设置里的默认值，常有本地代理在听）
/// 不算"本机假代理"——经它的请求会真的出网，所以除非 URL 本身是回环，一律拦下。
pub fn offline_blocked_via(url: &str, proxy: &Proxy, offline: bool) -> bool {
    if !offline {
        return false;
    }
    match proxy {
        Proxy::Unavailable { .. } => true,
        Proxy::Direct => offline_blocked_with(url, true),
        Proxy::Http { host, port } | Proxy::Socks5 { host, port } | Proxy::Socks5h { host, port } => {
            !is_loopback_host(host)
                || (REAL_PROXY_PORTS.contains(port) && offline_blocked_with(url, true))
        }
    }
}

/// 真代理（v2ray）听的端口：离线闸不把它们当"本机假代理"。
const REAL_PROXY_PORTS: [u16; 2] = [10808, 10809];

/// 主机名 / 地址是不是回环（`127.x` / `::1` / `localhost`）。
fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    h == "localhost" || h == "::1" || h.starts_with("127.")
}

/// [`offline_blocked`] 的纯函数版（`offline` 由调用方给，测试才能两条分支都断言）。
///
/// 口径和根 `build.gradle.kts` 的 `offlineGuard` 一致：回环（`127.x` / `::1` / `localhost`）放行
/// （MockWebServer、本机中转、测量台都在那里），其余一律拒。
pub fn offline_blocked_with(url: &str, offline: bool) -> bool {
    if !offline {
        return false;
    }
    match url
        .parse::<http::Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_string))
    {
        Some(host) => !is_loopback_host(&host),
        // 解析不出来：交给后面的 `parse_method_url` 报 `BadRequest`（这里不抢它的活）
        None => false,
    }
}

/// `FEEDHUB_OFFLINE=1` 读一次就缓存（环境变量在一个进程里不会变）。
fn offline_env() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("FEEDHUB_OFFLINE")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

/// `host:port`（每 host 并发上限的分桶键）。
fn host_key(url: &str) -> Result<String, NetError> {
    let uri: http::Uri = url
        .parse()
        .map_err(|e| NetError::BadRequest { detail: format!("URL 解析不了：{e}") })?;
    let host = uri
        .host()
        .ok_or(NetError::BadRequest { detail: "URL 里没有 host".into() })?;
    Ok(match uri.port_u16() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    })
}

/// 这条请求吃不吃那批默认头（`DefaultHeadersInterceptor` 的 `hostFilter`）。
///
/// 口径照 Kotlin 那边各家的写法：**空表 = 全部生效**；否则 `host == 条目` 或 `host.ends_with("." + 条目)`
/// （`hostFilter = { it == "baidu.com" || it.endsWith(".baidu.com") }` 这种的等价形式）。
fn default_headers_apply(req: &NetRequest, host: Option<&str>) -> bool {
    if req.default_headers_hosts.is_empty() {
        return true;
    }
    let Some(host) = host else { return false };
    let host = host.to_ascii_lowercase();
    req.default_headers_hosts.iter().any(|suffix| {
        let s = suffix.trim_start_matches('.').to_ascii_lowercase();
        host == s || host.ends_with(&format!(".{s}"))
    })
}

fn header_map(
    req: &NetRequest,
    uri: &http::Uri,
    platform_headers: &[(String, String)],
    cookie_pairs: &[(String, String)],
    extra: &[(&'static str, String)],
) -> Result<HeaderMap, NetError> {
    let mut map = HeaderMap::new();
    for (name, value) in platform_headers {
        let hn = HeaderName::try_from(name.as_str())
            .map_err(|e| NetError::BadRequest { detail: format!("请求头名不合法：{e}") })?;
        let hv = HeaderValue::try_from(wire_header_value(value))
            .map_err(|e| NetError::BadRequest { detail: format!("请求头的值不合法：{e}") })?;
        map.append(hn, hv);
    }
    // 平台默认头（`DefaultHeadersInterceptor`）：**同名的不覆盖**，位置在请求头之后、Cookie 之前。
    // `default_headers_hosts` 非空时只对那几个域生效（`hostFilter`）。
    let apply_defaults = default_headers_apply(req, uri.host());
    for (name, value) in req.extra_default_headers.iter().filter(|_| apply_defaults) {
        let hn = HeaderName::try_from(name.as_str())
            .map_err(|e| NetError::BadRequest { detail: format!("请求头名不合法：{e}") })?;
        if map.contains_key(&hn) {
            continue;
        }
        let hv = HeaderValue::try_from(wire_header_value(value))
            .map_err(|e| NetError::BadRequest { detail: format!("请求头的值不合法：{e}") })?;
        map.append(hn, hv);
    }
    // **OkHttp 的兜底 UA**：`BridgeInterceptor` 在整条链的最后补 `User-Agent: okhttp/4.12.0`
    // （`userAgent` 没显式设过时）。只在**一个 UA 都没有**的时候补——平台自己写了就不动。
    // 贴吧 App 接口（`/c/f/forum/forumGuide`）就是"一个默认头都不加"的那种客户端，
    // 线上真的带 `okhttp/4.12.0`（`TiebaWireEquivalenceTest` 抓出来的）。
    if !map.contains_key("user-agent") {
        map.append(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(OKHTTP_USER_AGENT),
        );
    }
    // OkHttp 在平台头之后补的那几个。`Connection` 在 h2 上会被 `strip_connection_headers` 剥掉；
    // **`Host` 不进头表**，只留在顺序表里（[`header_order`]）：h1 由 wreq 在发送前自己补
    // （`set_host`，连接已经知道是 h1 了），写出来的大小写和位置照顺序表，和 OkHttp 一样是 `Host`；
    // h2 上就不会有它。wreq 和 `http2` 都**不会**替我们把 `host` 换成 `:authority`，放进头表的话
    // h2 帧里就是 `:authority` + `host` 两份——OkHttp 的 h2 不发 `host`，B站的 Tengine 直接回
    // 400。
    for (name, value) in extra {
        if *name == "Host" {
            continue;
        }
        let hn = HeaderName::try_from(*name).expect("我们自己给的头名一定合法");
        if !map.contains_key(&hn) {
            let hv = HeaderValue::try_from(wire_header_value(value))
                .map_err(|e| NetError::BadRequest { detail: format!("请求头的值不合法：{e}") })?;
            map.append(hn, hv);
        }
    }
    // **罐子给的 `Cookie` 排在最后**：OkHttp 的 `BridgeInterceptor` 顺序是
    // `用户头 → Host → Connection → Accept-Encoding → Cookie → User-Agent`，
    // 而 `Request.Builder.header()` 对"没有的同名头"是 append。所以 jar 里带出来的 Cookie
    // 落在 `Accept-Encoding` **之后**（`source-reddit` 的 MPD 请求等价门当场抓出来的；
    // 平台自己写的 `Cookie:` 头不受这个影响——它在 `platform_headers` 里，位置由平台决定）。
    if !cookie_pairs.is_empty() && !map.contains_key("cookie") {
        let joined = join_cookies(cookie_pairs);
        if let Ok(hv) = HeaderValue::try_from(wire_header_value(&joined)) {
            map.append(HeaderName::from_static("cookie"), hv);
        }
    }
    Ok(map)
}

/// 请求头的**顺序 + 原始大小写**。
///
/// `OrigHeaderMap` 的两种来源：`HeaderName` 会被当成"标准写法"（**小写**），`String` / `Bytes`
/// 会当成"原样保留大小写"。HTTP/1.1 的头名大小写是**指纹的一部分**（golden 里是 `User-Agent` /
/// `Accept-Encoding` 这种标准写法），所以这里一律传 `String`（平台层给的原样）。
fn header_order(
    req: &NetRequest,
    platform_headers: &[(String, String)],
    cookie_pairs: &[(String, String)],
    extra: &[(&'static str, String)],
    apply_defaults: bool,
) -> Result<OrigHeaderMap, NetError> {
    let mut order = OrigHeaderMap::new();
    for (name, _) in platform_headers {
        // 校验一次名字合法（非法名在 `header_map` 里已经报错；这里只是保证顺序表里也是合法的）
        HeaderName::try_from(name.as_str())
            .map_err(|e| NetError::BadRequest { detail: format!("请求头名不合法：{e}") })?;
        let canonical = HeaderName::try_from(name.as_str()).unwrap();
        if !order.iter().any(|(n, _)| n == &canonical) {
            // **原样大小写**
            order.insert(name.clone());
        }
    }
    // 平台默认头（`DefaultHeadersInterceptor`）：请求里已有的同名头**不补**（判断要按大小写无关）。
    if apply_defaults {
        for (name, _) in &req.extra_default_headers {
            if platform_headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
                continue;
            }
            if !order.iter().any(|(n, _)| n.as_str().eq_ignore_ascii_case(name)) {
                // **原样大小写**（默认头那张表里写的就是 OkHttp 线上看到的写法）
                order.insert(name.clone());
            }
        }
    }
    // OkHttp 补的那几个：**排在平台头之后**，名字按 OkHttp 的写法（原样大小写）
    for (name, _) in extra {
        if !order.iter().any(|(n, _)| n.as_str().eq_ignore_ascii_case(name)) {
            order.insert((*name).to_string());
        }
    }
    // 罐子给的 `Cookie`（`BridgeInterceptor` 先补 Host / Connection / Accept-Encoding，再 set Cookie）。
    if !cookie_pairs.is_empty() && !platform_headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("cookie")) {
        order.insert("Cookie".to_string());
    }
    // **OkHttp 的兜底 UA 排在整条链的最后**（`BridgeInterceptor` 是最后一个补头的拦截器，
    // 而它在 set Cookie 之后才写 `User-Agent`）。只在没有 UA 的时候补，见 `header_map`。
    if !platform_headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent"))
        && !(apply_defaults
            && req
                .extra_default_headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("user-agent")))
    {
        order.insert("User-Agent".to_string());
    }
    Ok(order)
}

/// `a=b; c=d`（RFC 6265 §5.4 的拼法；顺序由罐子决定）。
fn join_cookies(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn collect_headers(map: &HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// 等取消（`select!` 的分支，基于 `Notify` 事件唤醒，在飞期间零定时轮询）。
async fn wait_cancel(cancel: &Arc<CancelToken>) {
    loop {
        let notified = cancel.notify.notified();
        tokio::pin!(notified);
        // 先登记 waiter 再读原子位，保证和任意线程的 `cancel()` 交错都不会漏通知
        notified.as_mut().enable();
        if cancel.is_cancelled() {
            return;
        }
        notified.await;
        if cancel.is_cancelled() {
            return;
        }
    }
}

// ===========================================================================
// 并发槽（异步 Semaphore 排队，drop 时自动归还，零 1ms 自旋）
// ===========================================================================

struct Slot {
    _total: tokio::sync::OwnedSemaphorePermit,
    _host: tokio::sync::OwnedSemaphorePermit,
}

impl Slot {
    fn acquire(
        total: &Arc<tokio::sync::Semaphore>,
        in_flight: &Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
        host: &str,
        c: Concurrency,
        cancel: &Arc<CancelToken>,
    ) -> Result<Self, NetError> {
        let host_sem = {
            let mut map = in_flight.lock().unwrap();
            map.entry(host.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(c.per_host)))
                .clone()
        };
        let total_sem = total.clone();
        let cancel = cancel.clone();
        rt::block_on(async move {
            let total_permit = acquire_semaphore(total_sem, &cancel).await?;
            let host_permit = acquire_semaphore(host_sem, &cancel).await?;
            Ok(Self {
                _total: total_permit,
                _host: host_permit,
            })
        })
    }
}

async fn acquire_semaphore(
    sem: Arc<tokio::sync::Semaphore>,
    cancel: &Arc<CancelToken>,
) -> Result<tokio::sync::OwnedSemaphorePermit, NetError> {
    tokio::select! {
        biased;
        () = wait_cancel(cancel) => Err(NetError::Cancelled),
        res = tokio::time::timeout(Duration::from_secs(300), sem.acquire_owned()) => match res {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(NetError::Other { detail: "并发信号量已关闭".into() }),
            Err(_) => Err(NetError::Other { detail: "等并发槽超时".into() }),
        },
    }
}

