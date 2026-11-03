//! **宿主端口**：这一层需要宿主告诉它 / 帮它做的事——一共两件，都用 trait 注入。
//!
//! 这一层只负责**在建立连接之前**调它，且取消时要把时间槽让出来 |
//!
//! 两个都必须是 `Send + Sync`（会被多个调用线程同时用）。**都是同步阻塞**的：和 `HostHttp` 一样的
//! 口径——这一层在调用线程上调它们，
//!
//! 现在只有测试实现和"不启用"的空实现：**产品路径还没接线**。

use std::sync::Arc;

/// 要种进罐子的一条 Cookie。
///
/// 和 `feedhub-core/src/host.rs` 的 `JarCookie` 字段**逐个对齐**（含 `secure` / `http_only` /
/// `host_only`）——`okhttp3.Cookie` 的字段影响 `Cookie.matches(url)`，丢一个就是
/// "用户以为登录了其实没有"（扫码登录的语义要求原样搬运）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CookieSpec {
    pub name: String,
    pub value: String,
    /// `bilibili.com`（不带前导点；`host_only` 决定它是不是只匹配这个主机）。
    pub domain: String,
    pub path: String,
    /// 毫秒时间戳；`<= 0` = 会话 Cookie。
    pub expires_at_ms: i64,
    pub secure: bool,
    pub http_only: bool,
    /// `true` = host-only（`Cookie.Builder.hostOnlyDomain`），`false` = 域 Cookie（`Cookie.Builder.domain`）。
    pub host_only: bool,
}

/// Cookie 罐（对应 OkHttp 的 `CookieJar`）。
///
/// 语义照 `okhttp3.CookieJar`：
/// - [`cookies`](Self::cookies)：**发请求前**问这个 URL 该带哪些（按路径长的先、创建早的先，
///   最后拼成一条 `a=b; c=d`）；
/// - [`set_cookies`](Self::set_cookies)：**收到响应后**把这条响应里**全部** `Set-Cookie`
///   **按顺序**交回去（包括被删掉的 `Max-Age=0`）。
///
/// **这一层不解释 Cookie 属性**（domain / path / expires / secure / httpOnly 全由宿主按 RFC 6265
/// 判定）：两份实现一定会漂，而 `cookies.json` 已经是登录态的唯一真相。
pub trait CookieHost: Send + Sync {
    /// 这个 URL 该带哪些 Cookie（**有序**；值原样，不含 `Cookie:` 前缀）。
    fn cookies(&self, url: &str) -> Vec<(String, String)>;

    /// 把一条响应里的 `Set-Cookie` 交回去（**按响应里的顺序**，值原样）。
    fn set_cookies(&self, url: &str, set_cookies: Vec<String>);

    /// 把这几条 Cookie 种进罐里（扫码登录 / 接口下发的设备标识用；不会顶掉别的域名）。
    fn put(&self, url: &str, cookies: Vec<CookieSpec>);
}

/// "什么 Cookie 都不带、也不收"的罐子（图片客户端 / 匿名请求用；对应 `CookieJar.NO_COOKIES`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCookies;

impl CookieHost for NoCookies {
    fn cookies(&self, _url: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    fn set_cookies(&self, _url: &str, _set_cookies: Vec<String>) {}
    fn put(&self, _url: &str, _cookies: Vec<CookieSpec>) {}
}

/// 请求优先级（和 App 的 `RequestPriority` 同一套）。
///
/// 这一层**只把它交给 [`LimiterHost`]**：连接的调度不分优先级（OkHttp 也不分——
/// `Dispatcher` 是先到先服务，优先级只影响限速队列的排队顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Priority {
    /// 用户正等着的（详情 / 评论 / 楼中楼）。
    Interactive,
    /// 刷流 / 预取。
    #[default]
    Background,
}

/// 限速 / 冷却。
///
/// 调用时机（**照现在的拦截器链**）：`onRequest` 计数 → 代理守卫 → 默认头 → **限速等待** → 连接。
/// 也就是说 [`acquire`](Self::acquire) 在**建立连接之前**被调，可以在这里等、也可以直接拒绝。
///
/// 返回 `Err(Wait)` 表示"这个请求这次不该发"（冷却中 / 超出配额），`detail` 是给用户的一句话。
pub trait LimiterHost: Send + Sync {
    /// 排到号了再返回；被取消就返回 `Err(Cancelled)`。
    ///
    /// `platform` 是平台 id（`bilibili` / `zhihu` …），限速器按它分桶。
    fn acquire(
        &self,
        platform: &str,
        priority: Priority,
        cancel: &Arc<crate::CancelToken>,
    ) -> Result<(), LimitRejected>;

    /// 和 [`acquire`](Self::acquire) 一样，但**把"在队列里等了多久"带回来**（毫秒）。
    ///
    /// 为什么要它：App 的性能记录里有一栏 `queue_ms`（`core/RateLimit.kt` 的 `RateLimitWait`，
    /// 由 `MinIntervalInterceptor` 在放行时累加）。切到这一层之后"放行"发生在 Rust 里，
    /// 只能在这儿把数字带出来，否则性能记录里那一栏会静悄悄消失。
    ///
    /// 默认实现 = 调 [`acquire`](Self::acquire) 然后报 0（不记账的限速器不用改）。
    fn acquire_with_wait(
        &self,
        platform: &str,
        priority: Priority,
        cancel: &Arc<crate::CancelToken>,
    ) -> Result<i64, LimitRejected> {
        self.acquire(platform, priority, cancel).map(|()| 0)
    }
}

/// 限速器拒绝的两种理由（分开是为了上层的错误文案 / 退出码）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitRejected {
    /// 冷却中 / 超配额 / 风控还没过：**这次不发**。
    #[error("{detail}")]
    Blocked { detail: String },
    /// 等的时候被取消了。
    #[error("已取消")]
    Cancelled,
}

/// 不做任何限速的限速器（测试 / cli 的 `--no-limit`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLimiter;

impl LimiterHost for NoLimiter {
    fn acquire(
        &self,
        _platform: &str,
        _priority: Priority,
        cancel: &Arc<crate::CancelToken>,
    ) -> Result<(), LimitRejected> {
        if cancel.is_cancelled() {
            return Err(LimitRejected::Cancelled);
        }
        Ok(())
    }
}

