//! **Cookie 罐 / 限速 / Cookie 导入**过 UniFFI 边界。
//!
//! Rust 侧的逻辑在 [`feedhub_core::cookies`] / [`feedhub_core::ratelimit`] /
//! [`feedhub_core::chrome_cookies`]；这里只有 record / enum、`#[derive(uniffi::Object)]` 的罐子和队列，
//! 以及**时钟**（宿主每次调都带上当前毫秒）。
//!
//! Kotlin 门面：`rust-bridge/.../feedhub/cookies/RustCookieJar.kt`（OkHttp `CookieJar` 的薄适配）、
//! `rust-bridge/.../feedhub/ratelimit/RustRateLimiter.kt`（`MinIntervalInterceptor` 的薄适配）、
//! `rust-bridge/.../feedhub/chrome/RustChromeCookies.kt`（导入器）。
//!
//! ## 为什么罐子是一个 `Object` 而不是一组纯函数
//!
//! 罐子有状态（内存里那一份 + `dirty` / `removed` / `cleared_domains` 三样记账），而且**每个请求都要过
//! 一次边界**（`loadForRequest`）。做成对象之后 Kotlin 那边持有的是一个句柄，一次调用只过 record 数组，
//! 不用把整罐搬来搬去。
//!
//! 所以过边界只剩**一个路径字符串**：
//! [`cookie_jar_open_file`]。**"读文件 / 文件锁"不是留在宿主的理由**（通用说明）。
//!
//! ## 时钟
//!
//! `now_ms` 由宿主传（和这一层其余模块一样：Rust 里不读时钟）。调用方每次调都带上当前毫秒，
//! 测试里就能固定。

use std::sync::{Arc, Mutex};

use feedhub_core::chrome_cookies as chrome;
use feedhub_core::cookies as core;
use feedhub_core::ratelimit as rate;


// ===========================================================================
// Cookie 罐
// ===========================================================================

/// 一条 Cookie（Kotlin `okhttp3.Cookie` 的那几个字段，**交接时无损**）。
///
/// `domain` **不带前导点**（OkHttp 的 `Cookie.domain` 就是这样）；`host_only` 单独一个 bool。
/// `expires_at_ms` 是 Unix 毫秒。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCookieEntry {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires_at_ms: i64,
    pub secure: bool,
    pub http_only: bool,
    pub host_only: bool,
}

impl FfiCookieEntry {
    pub(crate) fn to_core(&self) -> core::StoredCookie {
        core::StoredCookie {
            name: self.name.clone(),
            value: self.value.clone(),
            domain: self.domain.clone(),
            path: self.path.clone(),
            expires_at_ms: self.expires_at_ms,
            secure: self.secure,
            http_only: self.http_only,
            host_only: self.host_only,
        }
    }

    pub(crate) fn from_core(c: &core::StoredCookie) -> Self {
        Self {
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
}

/// 一个请求地址里罐子要用的三样（宿主从 OkHttp 的 `HttpUrl` 取）。
///
/// `host` 必须是 OkHttp **规范化之后**的（小写 + IDN → punycode + IPv6 去方括号），`encoded_path`
/// 是 `HttpUrl.encodedPath`（**已编码**）。重算这两样就等于改线上行为，所以让宿主给。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiHttpUrl {
    pub host: String,
    pub encoded_path: String,
    pub is_https: bool,
}

/// 罐子的打开入口：**给一个文件路径**（Rust 自己读 / 写 / 留底 / 加锁）。
///
/// 桌面是 `~/.config/feedhub/cookies.json`，安卓是 `filesDir/cookies.json`——宿主只决定"文件在哪"，
/// 具体怎么原子替换、600 权限、坏文件怎么留底、跨进程怎么排队全在 [`feedhub_core::cookies::FileStorage`]。
///
/// 失败（读不懂又留不了底）返回 [`FfiCookieError::CannotQuarantine`]，调用方把它当成"App 起不来"
/// 级别的错误（和 Kotlin 的 `IllegalStateException` 一样，**不许**悄悄从空罐开始）。
#[uniffi::export]
pub fn cookie_jar_open_file(path: String, now_ms: i64) -> Result<Arc<CookieJar>, FfiCookieError> {
    open_with(
        Box::new(core::FileStorage::new(std::path::PathBuf::from(path), now_ms)),
        now_ms,
    )
}

/// 内存罐子（不落盘；测试 / 演示 / 截图 / 扫码的临时罐子用）。
///
/// 它和真罐子走**同一套**匹配 / 合并逻辑（`CookieJar::in_memory`），只是落盘端口什么都不存。
#[uniffi::export]
pub fn cookie_jar_open_memory(now_ms: i64) -> Arc<CookieJar> {
    Arc::new(CookieJar {
        inner: Arc::new(core::CookieJar::in_memory(now_ms)),
    })
}

/// [`cookie_jar_open_file`] / 测试共用的那一步：把落盘端口交给罐子。
fn open_with(storage: Box<dyn core::CookieStorage>, now_ms: i64) -> Result<Arc<CookieJar>, FfiCookieError> {
    let jar = core::CookieJar::open(storage, now_ms).map_err(|e| match e {
        core::OpenError::CannotQuarantine { detail } => FfiCookieError::CannotQuarantine { detail },
    })?;
    Ok(CookieJar::from_shared(Arc::new(jar)))
}

/// **落盘 / 存档**相关的错误（Rust 侧 [`feedhub_core::cookies`] 的那几个 `Result`）。
///
/// 和 [`crate::FfiError`] 分开：那是"平台调用失败"的语义（网络 / 风控 / 未登录），
/// 这一层只有"文件读不懂 / 写不进去"，调用方不该把它当成网络错误重试。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiCookieError {
    /// 存档读不懂、而且**留不了底**（Kotlin 那句 `IllegalStateException`：宁可起不来也不覆盖）。
    #[error("cookies.json 解析失败，也没法留底；为了不覆盖已有登录，不加载也不写回：{detail}")]
    CannotQuarantine { detail: String },

    /// 写不进去（磁盘满、权限、原子替换失败）。
    #[error("写 cookies.json 失败：{detail}")]
    Storage { detail: String },

    /// 宿主自己的意外。
    #[error("宿主的 Cookie 落盘出错：{detail}")]
    Host { detail: String },

    /// 找不到 Chrome 的 Cookie 库（profile 目录里没有 `Cookies`）。
    #[error("没找到 Chrome 的 Cookie 文件（{path}）")]
    ChromeMissingDb { path: String },

    /// Chrome 的 Cookie 库读不了（不是 SQLite、被锁着、表结构不认识）。
    #[error("读不了 Chrome 的 Cookie 库：{detail}")]
    ChromeSqlite { detail: String },

    /// 取不到 Chrome Safe Storage 的密码（KWallet / 钥匙环都没有）。
    #[error("KWallet 和钥匙环里都没取到 Chrome 的密钥（Chrome Safe Storage）")]
    ChromeNoKey,
}

/// 存在一份 JSON 文本里的 Cookie 罐（Rust 里是 [`feedhub_core::cookies::CookieJar`]）。
///
/// **线程安全**：内部一把 `Mutex`，OkHttp 的连接池线程可以同时调（`load_for_request` /
/// `save_from_response`）。
#[derive(uniffi::Object)]
pub struct CookieJar {
    /// **`Arc` 而不是本体**：Rust 的联网层（`crate::net_host`）要**不过 FFI**
    /// 直接读写同一个罐子（`CookieHost::cookies` / `set_cookies`）。罐子本体只有 `&self` 方法，
    /// 外面包一层 `Arc` 不改变任何并发语义（内部 `Mutex` + 文件锁都在本体里）。
    inner: Arc<core::CookieJar>,
}

impl CookieJar {
    /// 从已经存在的 `Arc` 造一个句柄（`FfiNetStack` 和它共用同一个罐子）。
    pub fn from_shared(inner: Arc<core::CookieJar>) -> Arc<Self> {
        Arc::new(Self { inner })
    }

    /// 这份句柄背后的罐子（给 `net_host` 用）。
    pub fn shared(&self) -> Arc<core::CookieJar> {
        self.inner.clone()
    }
}

#[uniffi::export]
impl CookieJar {
    /// 发往 `url` 的请求会带上的 Cookie（顺序 = 罐子里的顺序）。
    pub fn load_for_request(&self, url: FfiHttpUrl, now_ms: i64) -> Vec<FfiCookieEntry> {
        let url = to_url(&url);
        self.inner
            .load_for_request(&url, now_ms)
            .iter()
            .map(FfiCookieEntry::from_core)
            .collect()
    }

    /// 发往 `url` 的请求会带上的、名为 `name` 的 Cookie 值。
    pub fn find(&self, url: FfiHttpUrl, name: String, now_ms: i64) -> Option<String> {
        self.inner.find(&to_url(&url), &name, now_ms)
    }

    /// 服务端下发的 Cookie（或者自己种的）：同名同域同路径先删，没过期才留下，然后写回文件。
    ///
    /// `url` 只为和 Kotlin 的 `saveFromResponse(url, cookies)` 签名一致：**过期判定用 `expires_at_ms`**，
    /// 不用 URL（和 Kotlin 一样）。
    pub fn save_from_response(
        &self,
        url: FfiHttpUrl,
        cookies: Vec<FfiCookieEntry>,
        now_ms: i64,
    ) -> Result<(), FfiCookieError> {
        let _ = to_url(&url);
        let core_cookies: Vec<core::StoredCookie> = cookies.iter().map(FfiCookieEntry::to_core).collect();
        self.inner
            .save_from_response(&core_cookies, now_ms)
            .map_err(|e| FfiCookieError::Storage {
                detail: e.to_string(),
            })
    }

    /// 删掉某个域（含子域）下的全部 Cookie。
    pub fn clear_domain(&self, domain: String, now_ms: i64) -> Result<(), FfiCookieError> {
        self.inner
            .clear_domain(&domain, now_ms)
            .map_err(|e| FfiCookieError::Storage {
                detail: e.to_string(),
            })
    }

    /// 删掉某个域（含子域）下名字在 `names` 里的 Cookie（不管路径），其他 Cookie 不动。
    pub fn remove(&self, domain: String, names: Vec<String>, now_ms: i64) -> Result<(), FfiCookieError> {
        self.inner
            .remove(&domain, &names, now_ms)
            .map_err(|e| FfiCookieError::Storage {
                detail: e.to_string(),
            })
    }

    /// 全部 Cookie（`cookie list` 用；**不过滤过期**，和 Kotlin `all()` 一致）。
    pub fn all(&self) -> Vec<FfiCookieEntry> {
        self.inner.all().iter().map(FfiCookieEntry::from_core).collect()
    }

    /// 现在有几条。
    pub fn len(&self) -> u64 {
        self.inner.len() as u64
    }

    /// 开启 / 调整 `save_from_response` 写盘防抖窗口（毫秒；`<= 0` = 每次同步落盘）。
    pub fn set_debounce_ms(&self, debounce_ms: i64) {
        self.inner.set_debounce_ms(debounce_ms.max(0) as u64);
    }

    /// 立刻把防抖队列里尚未落盘的改动写回 `cookies.json`（`onStop` / 退出时调）。
    pub fn flush(&self, now_ms: i64) -> Result<(), FfiCookieError> {
        self.inner.flush(now_ms).map_err(|e| FfiCookieError::Storage {
            detail: e.to_string(),
        })
    }
}

fn to_url(u: &FfiHttpUrl) -> core::HttpUrl {
    core::HttpUrl {
        host: u.host.clone(),
        encoded_path: u.encoded_path.clone(),
        is_https: u.is_https,
    }
}

// ===========================================================================
// 限速：排队决策（`MinIntervalInterceptor` 的脑子）
// ===========================================================================

/// 请求优先级（Kotlin `RequestPriority`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRequestPriority {
    Interactive,
    Background,
}

impl FfiRequestPriority {
    fn to_core(self) -> rate::RequestPriority {
        match self {
            Self::Interactive => rate::RequestPriority::Interactive,
            Self::Background => rate::RequestPriority::Background,
        }
    }
}

/// 队头状态（宿主每 100ms 问一次这个）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiQueueSnapshot {
    /// 队头还要等多久（毫秒，0 = 现在就能发）。
    pub head_wait_ms: i64,
    /// 队列里有几张票。
    pub size: u32,
}

/// **一个平台的排队状态**（Kotlin `RateLimitQueue` 的 Rust 版）。
///
/// 宿主（`RustRateLimiter`）为每个 `MinIntervalInterceptor` 建一个，`with_foreign` 那种"一次调用算一个
/// 时间点"的用法：`enqueue` → 循环 `snapshot` / `try_grant`（中间自己 sleep）→ 放行或 `release`。
///
/// **票是数字句柄**：`enqueue` 返回一个 id，之后用 id 问"还等多久 / 放行了吗 / 取消"。
#[derive(uniffi::Object)]
pub struct RateLimitQueue {
    inner: Mutex<rate::RateLimitQueue>,
}

impl RateLimitQueue {
    /// `random_seed` 注入（Rust 里不取随机数）：生产里宿主传一个真的随机值，测试里固定。
    fn new(min_interval_ms: i64, jitter_ms: i64, background_grace_ms: i64, random_seed: i64) -> Self {
        Self {
            inner: Mutex::new(rate::RateLimitQueue::new(
                min_interval_ms,
                jitter_ms,
                background_grace_ms,
                random_seed,
            )),
        }
    }
}

/// 造一个队列（`background_grace_ms` 一般给 5000：后台等超过 5 秒就不再被前台插队）。
#[uniffi::export]
pub fn rate_limit_queue_new(
    min_interval_ms: i64,
    jitter_ms: i64,
    background_grace_ms: i64,
    random_seed: i64,
) -> Arc<RateLimitQueue> {
    Arc::new(RateLimitQueue::new(
        min_interval_ms,
        jitter_ms,
        background_grace_ms,
        random_seed,
    ))
}

#[uniffi::export]
impl RateLimitQueue {
    /// 排进队列，返回票的句柄。`now_ms` 每次都由宿主传（Rust 不读时钟）。
    pub fn enqueue(&self, priority: FfiRequestPriority, now_ms: i64) -> u64 {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.enqueue(priority.to_core(), now_ms)
    }

    /// 队头到点就放行；返回它在队列里等了多久（毫秒）。还没轮到返回 null（票留在队列里）。
    pub fn try_grant(&self, ticket: u64, now_ms: i64) -> Option<i64> {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.try_grant(ticket, now_ms)
    }

    /// 还要等多久（毫秒）。非队头给的是"按现在的排法"的预计时刻；队列变了会变。
    pub fn wait_ms(&self, ticket: u64, now_ms: i64) -> i64 {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.wait_ms(ticket, now_ms)
    }

    /// 没发出去的请求退出（取消 / 出错）：让出时间槽，后面的往前挪。
    pub fn release(&self, ticket: u64, now_ms: i64) {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.release(ticket, now_ms);
    }

    /// 队头状态（宿主每 100ms 问一次）。
    pub fn snapshot(&self, now_ms: i64) -> FfiQueueSnapshot {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let s = q.snapshot(now_ms);
        FfiQueueSnapshot {
            head_wait_ms: s.head_wait_ms,
            size: s.size as u32,
        }
    }

    /// 这张票按现在的排队"该在什么时刻发出去"。
    pub fn scheduled_at(&self, ticket: u64) -> Option<i64> {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.ticket(ticket).map(|t| t.scheduled_at)
    }

    /// 这张票自己的抖动。
    pub fn jitter_ms(&self, ticket: u64) -> Option<i64> {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.ticket(ticket).map(|t| t.jitter_ms)
    }

    /// 这张票还在队里吗（放行 / 取消之后就没有了）。
    pub fn is_pending(&self, ticket: u64) -> bool {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.is_pending(ticket)
    }

    /// 队列里有几个。
    pub fn size(&self) -> u32 {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.size() as u32
    }
}

// ===========================================================================
// 限速：跨进程（cli）
// ===========================================================================

/// 跨进程限速的判定（`~/.cache/feedhub/cli-rate/<平台>.rate`）。
///
/// 格式一个字都没变：文件里就是一个十进制毫秒时间戳。**拿锁 / 睡 / 写文件**的顺序在宿主：
/// 拿锁 → `wait_ms` → 睡 → `commit` → 放锁（和 Kotlin `FileLocks.withLock { … }` 一样）。
#[derive(uniffi::Object)]
pub struct CrossProcessRateLimit {
    inner: rate::CrossProcess,
}

/// 造一个（`dir` = `~/.cache/feedhub/cli-rate`）。
#[uniffi::export]
pub fn cross_process_rate_limit_new(dir: String) -> Arc<CrossProcessRateLimit> {
    Arc::new(CrossProcessRateLimit {
        inner: rate::CrossProcess::new(dir),
    })
}

/// 造一个**换过间隔表**的（测试 / 探针用：真实网络间隔 1.5～3 秒，测试等不起）。
///
/// `intervals` 是"覆盖"不是"替换"：表里没写的平台照旧走默认那张；`default_interval_ms` ≤ 0 时
/// 也用默认值（不会出现"零间隔"这种能关掉限速的档）。
#[uniffi::export]
pub fn cross_process_rate_limit_new_with_intervals(
    dir: String,
    intervals: Vec<FfiPlatformInterval>,
    default_interval_ms: i64,
) -> Arc<CrossProcessRateLimit> {
    let base = rate::CrossProcess::new(dir);
    let mut merged = rate::CrossProcess::default_intervals();
    for item in intervals {
        match merged.iter_mut().find(|(k, _)| *k == item.platform) {
            Some((_, v)) => *v = item.interval_ms,
            None => merged.push((item.platform, item.interval_ms)),
        }
    }
    let default = if default_interval_ms > 0 {
        default_interval_ms
    } else {
        rate::CrossProcess::DEFAULT_INTERVAL_MS
    };
    Arc::new(CrossProcessRateLimit {
        inner: base.with_intervals(merged, default),
    })
}

#[uniffi::export]
impl CrossProcessRateLimit {
    /// 这个平台的最小间隔（毫秒）。
    pub fn interval_for(&self, platform: String) -> i64 {
        self.inner.interval_for(&platform)
    }

    /// 上次请求记在什么时候（没有记录 / 读不出来返回 null）。
    pub fn last_request_at(&self, platform: String) -> Option<i64> {
        self.inner.last_request_at(&platform)
    }

    /// 还要等多久（毫秒，0 = 现在就能发）。**不写文件**。
    pub fn wait_ms(&self, platform: String, now_ms: i64) -> i64 {
        self.inner.wait_ms(&platform, now_ms)
    }

    /// 记下这次请求的时刻（原子替换）。调用方要先拿锁、而且在 `wait_ms` 为 0 之后调。
    pub fn commit(&self, platform: String, now_ms: i64) -> Result<(), FfiCookieError> {
        self.inner
            .commit(&platform, now_ms)
            .map_err(|detail| FfiCookieError::Storage { detail })
    }
}

/// 默认的按平台间隔表（`capabilities` 命令 / 文档用）：`[(平台, 毫秒)]`。
#[uniffi::export]
pub fn cross_process_default_intervals() -> Vec<FfiPlatformInterval> {
    rate::CrossProcess::default_intervals()
        .into_iter()
        .map(|(platform, ms)| FfiPlatformInterval { platform, interval_ms: ms })
        .collect()
}

/// 一条"平台 → 最小间隔"。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlatformInterval {
    pub platform: String,
    pub interval_ms: i64,
}

/// 没有配在表里的平台用的默认间隔（毫秒）。
#[uniffi::export]
pub fn cross_process_default_interval() -> i64 {
    rate::CrossProcess::DEFAULT_INTERVAL_MS
}

// ===========================================================================
// 风控冷却（cli）
// ===========================================================================

/// 一条冷却记录（Kotlin `CooldownStore.Entry`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCooldownEntry {
    pub platform: String,
    /// 冷却到什么时候（Unix 毫秒）。
    pub until: i64,
    /// 什么时候写下的（Unix 毫秒）。
    pub at: i64,
    /// 为什么：`risk_control` / `captcha_required` / `http_403` 这类。
    pub reason: String,
}

/// `check` 的结论。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCooldownCheck {
    /// 没在冷却。
    Clear,
    /// 正在冷却。
    Cooling {
        until: i64,
        retry_after_ms: i64,
        reason: String,
    },
}

/// 风控冷却（`~/.cache/feedhub/cooldown/<平台>.json`）。**App 不受影响**：只是 cli 自己的闸。
#[derive(uniffi::Object)]
pub struct CooldownStore {
    inner: rate::CooldownStore,
}

/// 造一个（`dir` = `~/.cache/feedhub/cooldown`）。
#[uniffi::export]
pub fn cooldown_store_new(dir: String) -> Arc<CooldownStore> {
    Arc::new(CooldownStore {
        inner: rate::CooldownStore::new(dir),
    })
}

#[uniffi::export]
impl CooldownStore {
    /// 冷却多久（毫秒）。
    pub fn ttl_ms(&self) -> i64 {
        self.inner.ttl_ms()
    }

    /// 正在冷却就返回还差多久；过期了就顺手删掉。
    pub fn check(&self, platform: String, now_ms: i64) -> FfiCooldownCheck {
        match self.inner.check(&platform, now_ms) {
            rate::CooldownCheck::Clear => FfiCooldownCheck::Clear,
            rate::CooldownCheck::Cooling {
                until,
                retry_after_ms,
                reason,
            } => FfiCooldownCheck::Cooling {
                until,
                retry_after_ms,
                reason,
            },
        }
    }

    /// 写一笔冷却（覆盖这个平台原来的标记）。失败只该打一行 stderr，不能盖住原来那个错误。
    pub fn mark(&self, platform: String, reason: String, now_ms: i64) -> Result<(), FfiCookieError> {
        self.inner
            .mark(&platform, &reason, now_ms)
            .map_err(|detail| FfiCookieError::Storage { detail })
    }

    /// 现在在冷却的平台，按 `until` 从早到晚。
    pub fn list(&self, now_ms: i64) -> Vec<FfiCooldownEntry> {
        self.inner
            .list(now_ms)
            .into_iter()
            .map(|e| FfiCooldownEntry {
                platform: e.platform,
                until: e.until,
                at: e.at,
                reason: e.reason,
            })
            .collect()
    }

    /// 清掉一个平台的冷却。清掉了（文件本来在）返回 true。
    pub fn clear(&self, platform: String, now_ms: i64) -> bool {
        self.inner.clear(&platform, now_ms)
    }
}

// ===========================================================================
// Chrome Cookie 导入
// ===========================================================================

/// 一条从 Chrome 的 `cookies` 表读出来的行（宿主读、Rust 解）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiChromeCookieRow {
    pub host_key: String,
    pub name: String,
    pub path: String,
    pub expires_utc: i64,
    pub is_secure: bool,
    pub is_httponly: bool,
    pub encrypted_value: Vec<u8>,
}

/// 导入范围（Kotlin `CookieImportSpec`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCookieImportSpec {
    /// 导入前先清掉的域（一般是主域名）。
    pub domain: String,
    /// 这些域本身和它们的子域都收。
    pub suffixes: Vec<String>,
    /// 只收这几个 host（百度要 `.baidu.com` 和 `tieba.baidu.com`，网盘等子域不碰）。
    pub exact_hosts: Vec<String>,
}

/// 导入的结论（**只有数量，从来没有值**）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCookieImportOutcome {
    /// 收进来的条数。
    pub imported: u32,
    /// 解得开但坏了（解密失败 / 域哈希对不上）的条数。
    pub failed: u32,
    /// 先清哪个域；null = 一条都没收（**不清**，和 Kotlin 的 `if (imported.isNotEmpty())` 一致）。
    pub clear_domain: Option<String>,
    /// 要写进罐子的条目，**已经按域分好组**（组的先后 = 域第一次出现的先后）。
    pub groups: Vec<FfiCookieImportGroup>,
}

/// 一个域下的一批 Cookie。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCookieImportGroup {
    pub domain: String,
    pub cookies: Vec<FfiCookieEntry>,
}

/// 把 Chrome Cookie 库解成一条条 Cookie（**纯函数**：不打库、不读文件、不跑命令）。
///
/// `db_version` 是 `SELECT value FROM meta WHERE key = 'version'`；`password_v10` / `password_v11` 是
/// KWallet / 钥匙环取到的 Chrome Safe Storage 密码（`v10` 那批在没有钥匙环时是固定的 `"peanuts"`）。
///
/// 返回 null = 这个平台一条都没收到（调用方**不清**它的域）。
#[uniffi::export]
pub fn chrome_cookie_import(
    db_version: i64,
    password_v10: String,
    password_v11: String,
    rows: Vec<FfiChromeCookieRow>,
    spec: FfiCookieImportSpec,
    now_ms: i64,
) -> Option<FfiCookieImportOutcome> {
    let core_rows: Vec<chrome::ChromeCookieRow> = rows
        .into_iter()
        .map(|r| chrome::ChromeCookieRow {
            host_key: r.host_key,
            name: r.name,
            path: r.path,
            expires_utc: r.expires_utc,
            is_secure: r.is_secure,
            is_httponly: r.is_httponly,
            encrypted_value: r.encrypted_value,
        })
        .collect();
    let core_spec = chrome::CookieImportSpec {
        domain: spec.domain,
        suffixes: spec.suffixes,
        exact_hosts: spec.exact_hosts,
    };
    chrome::import_rows(
        db_version,
        &password_v10,
        &password_v11,
        &core_rows,
        &core_spec,
        now_ms,
    )
    .map(to_ffi_outcome)
}

/// 一个平台要导入什么（按平台 key 对回去）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlatformImportSpec {
    /// 平台 key（`bili` / `zhihu` …），只用来把结果对回去。
    pub platform: String,
    pub spec: FfiCookieImportSpec,
}

/// 一个平台的导入结论。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPlatformImportOutcome {
    pub platform: String,
    /// null = 这个平台一条都没收到（调用方**不清**它的域）。
    pub outcome: Option<FfiCookieImportOutcome>,
}

/// **整条 Chrome 导入**：复制库到私有临时目录 → `rusqlite` 只读打开 →
/// 真有 `v11` 行时才跑 `kwallet-query` / `secret-tool` 取密码 → 解密 / 收哪些 / 过期 / 分组 →
/// 删副本。宿主只剩"用户点了导入"。
///
/// `profile` 是 Chrome profile 目录（`<profile>/Cookies` 是那个库）。密码 / key / 明文都不过在边界上
/// 来回：过 FFI 的只有**要写进罐子的那些条目**（们本来就是罐子的内容）和结果的数量。
#[uniffi::export]
pub fn chrome_cookie_import_file(
    profile: String,
    platforms: Vec<FfiPlatformImportSpec>,
    now_ms: i64,
) -> Result<Vec<FfiPlatformImportOutcome>, FfiCookieError> {
    chrome_import_with(chrome::ChromeImporter::new(profile), platforms, now_ms)
}

/// 同 [`chrome_cookie_import_file`]，但换一组"取密码"的命令（**测试 / 排障用**：不碰真的钥匙环）。
///
/// `password_commands` 为空 = 不要密码（遇到 `v11` 行就报 `ChromeNoKey`）。生产路径不经过它。
#[uniffi::export]
pub fn chrome_cookie_import_file_with_commands(
    profile: String,
    platforms: Vec<FfiPlatformImportSpec>,
    password_commands: Vec<Vec<String>>,
    password_timeout_ms: i64,
    now_ms: i64,
) -> Result<Vec<FfiPlatformImportOutcome>, FfiCookieError> {
    let timeout = std::time::Duration::from_millis(password_timeout_ms.max(1) as u64);
    chrome_import_with(
        chrome::ChromeImporter::new(profile)
            .with_password_commands(password_commands)
            .with_password_timeout(timeout),
        platforms,
        now_ms,
    )
}

fn chrome_import_with(
    importer: chrome::ChromeImporter,
    platforms: Vec<FfiPlatformImportSpec>,
    now_ms: i64,
) -> Result<Vec<FfiPlatformImportOutcome>, FfiCookieError> {
    let specs: Vec<chrome::PlatformImportSpec> = platforms
        .into_iter()
        .map(|p| chrome::PlatformImportSpec {
            platform: p.platform,
            spec: chrome::CookieImportSpec {
                domain: p.spec.domain,
                suffixes: p.spec.suffixes,
                exact_hosts: p.spec.exact_hosts,
            },
        })
        .collect();
    importer
        .import(&specs, now_ms)
        .map(|list| {
            list.into_iter()
                .map(|o| FfiPlatformImportOutcome {
                    platform: o.platform,
                    outcome: o.outcome.map(to_ffi_outcome),
                })
                .collect()
        })
        .map_err(|e| match e {
            chrome::ChromeImportError::MissingDb { path } => FfiCookieError::ChromeMissingDb { path },
            chrome::ChromeImportError::Sqlite { detail } => FfiCookieError::ChromeSqlite { detail },
            chrome::ChromeImportError::NoKey => FfiCookieError::ChromeNoKey,
            chrome::ChromeImportError::Unsupported { platform } => {
                FfiCookieError::ChromeSqlite {
                    detail: format!("{platform}不支持从浏览器导入 Cookie"),
                }
            }
        })
}

fn to_ffi_outcome(o: chrome::ImportOutcome) -> FfiCookieImportOutcome {
    FfiCookieImportOutcome {
        imported: o.imported as u32,
        failed: o.failed as u32,
        clear_domain: o.clear_domain,
        groups: o
            .groups
            .into_iter()
            .map(|(domain, cookies)| FfiCookieImportGroup {
                domain,
                cookies: cookies.iter().map(FfiCookieEntry::from_core).collect(),
            })
            .collect(),
    }
}

/// **整条导入 + 直接写进罐子**：读库 → 解密 → 先 `clear_domain` → 再按域分组 `save_from_response`。
///
/// "导入动作的顺序"（先清域、再按域分组写，一条都没收时**不清**）也从 Kotlin 搬过来了：
/// 宿主那边只剩"用户点了导入、要让哪些平台导"，它连解出来的 Cookie 都不用碰
/// （Kotlin 的 `ChromeCookieImporter` 只把结果的数量翻成 `ImportResult`）。
#[uniffi::export]
pub fn cookie_jar_import_chrome(
    jar: Arc<CookieJar>,
    profile: String,
    platforms: Vec<FfiPlatformImportSpec>,
    now_ms: i64,
) -> Result<Vec<FfiPlatformImportOutcome>, FfiCookieError> {
    let imported = chrome_import_with(chrome::ChromeImporter::new(profile), platforms, now_ms)?;
    apply_import(&jar, &imported, now_ms)?;
    Ok(imported)
}

/// 同 [`cookie_jar_import_chrome`]，但换一组"取密码"的命令（**测试 / 排障用**：不碰真的钥匙环）。
#[uniffi::export]
pub fn cookie_jar_import_chrome_with_commands(
    jar: Arc<CookieJar>,
    profile: String,
    platforms: Vec<FfiPlatformImportSpec>,
    password_commands: Vec<Vec<String>>,
    password_timeout_ms: i64,
    now_ms: i64,
) -> Result<Vec<FfiPlatformImportOutcome>, FfiCookieError> {
    let timeout = std::time::Duration::from_millis(password_timeout_ms.max(1) as u64);
    let imported = chrome_import_with(
        chrome::ChromeImporter::new(profile)
            .with_password_commands(password_commands)
            .with_password_timeout(timeout),
        platforms,
        now_ms,
    )?;
    apply_import(&jar, &imported, now_ms)?;
    Ok(imported)
}

/// 把导入结果写进罐子：**先清域、再按域分组写**（Kotlin `ChromeCookieImporter.import` 里那三行的 Rust 版）。
fn apply_import(
    jar: &CookieJar,
    imported: &[FfiPlatformImportOutcome],
    now_ms: i64,
) -> Result<(), FfiCookieError> {
    let storage_error = |e: core::StorageError| FfiCookieError::Storage {
        detail: e.to_string(),
    };
    for item in imported {
        let Some(outcome) = &item.outcome else { continue };
        // `clear_domain` 是 Some 就说明至少收到了一条（`import_rows` 只在非空时给）
        if let Some(domain) = &outcome.clear_domain {
            jar.inner.clear_domain(domain, now_ms).map_err(storage_error)?;
        }
        for group in &outcome.groups {
            let cookies: Vec<core::StoredCookie> =
                group.cookies.iter().map(FfiCookieEntry::to_core).collect();
            jar.inner
                .save_from_response(&cookies, now_ms)
                .map_err(storage_error)?;
        }
    }
    jar.inner.flush(now_ms).map_err(storage_error)?;
    Ok(())
}

/// 默认的 Chrome / Chromium profile 目录（Kotlin `ChromeCookieImporter.defaultProfile`）。
///
/// `xdg_config_home` 给 null / 空串就走 `$HOME/.config`；优先取**真有 `Cookies` 文件**的那个 profile，
/// 都没有就退回 `google-chrome/Default`（后面打开时会报"没找到"）。
#[uniffi::export]
pub fn chrome_cookie_default_profile(xdg_config_home: Option<String>, home: String) -> String {
    chrome::default_profile(xdg_config_home.as_deref(), &home)
        .to_string_lossy()
        .to_string()
}

/// `v10` / `v11` 的密钥（PBKDF2-HMAC-SHA1(密码, `"saltysalt"`, 1 次, 16 字节)）——**测试 / 排障用**。
///
/// 生产路径不经过它（导入时在 Rust 里现算）。返回值是 key 的十六进制；**绝不打印**真实密码算出来的那个。
#[uniffi::export]
pub fn chrome_cookie_derive_key_hex(password: String) -> String {
    match chrome::derive_key(&password) {
        Ok(k) => k.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => String::new(),
    }
}

// ===========================================================================
// 纯函数：cookies.json 编解码 / Cookie 请求头 / 匹配
// ===========================================================================

/// `cookies.json` 的编码（**逐字节**等于 Kotlin `CookieJson.encode`）。
///
/// `CookieJson` 那四个调用点（cli 的测试夹具、`cookie list` 的排障输出、两个旧测试）用它，
/// 不用为此建一个罐子。
#[uniffi::export]
pub fn cookie_json_encode(cookies: Vec<FfiCookieEntry>) -> String {
    let list: Vec<core::StoredCookie> = cookies.iter().map(FfiCookieEntry::to_core).collect();
    core::cookie_json::encode(&list)
}

/// `cookies.json` 的解码（**读得懂 Kotlin 写的每一份**）。
#[uniffi::export]
pub fn cookie_json_decode(text: String) -> Result<Vec<FfiCookieEntry>, FfiCookieError> {
    core::cookie_json::decode(&text)
        .map(|list| list.iter().map(FfiCookieEntry::from_core).collect())
        .map_err(|e| FfiCookieError::Storage {
            detail: e.to_string(),
        })
}

/// `Cookie: a=1; b=2` → 罐条目（Kotlin `parseCookieHeader`；过期给一年）。
#[uniffi::export]
pub fn cookie_parse_header(header: String, domain: String, now_ms: i64) -> Vec<FfiCookieEntry> {
    core::parse_cookie_header(&header, &domain, now_ms)
        .iter()
        .map(FfiCookieEntry::from_core)
        .collect()
}

/// 多段 `Cookie` 请求头 → 罐条目，**同名只留第一个**（Kotlin `WebLoginCookies.parse`）。
#[uniffi::export]
pub fn cookie_parse_headers(headers: Vec<String>, domain: String, now_ms: i64) -> Vec<FfiCookieEntry> {
    core::parse_cookie_headers(&headers, &domain, now_ms)
        .iter()
        .map(FfiCookieEntry::from_core)
        .collect()
}

/// 这条 Cookie 会不会发往 `url`（**不含过期判断**：调用方自己带 `now_ms` 语义）。
#[uniffi::export]
pub fn cookie_matches(cookie: FfiCookieEntry, url: FfiHttpUrl) -> bool {
    cookie.to_core().matches(&to_url(&url))
}
