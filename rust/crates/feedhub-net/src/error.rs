//! 错误分类。
//!
//! **和 `feedhub-core/src/host.rs` 的 [`HostError`] 一一对应**：
//! `Network` / `ProxyUnavailable` / `Other` 三个分支的含义、文案、上层映射都一样。
//! 这里多了两档这一层自己产生的：`Cancelled`（取消）和 `Timeout`（超时）——
//! OkHttp 那边它们是 `IOException` 的两个子类（`InterruptedIOException`），
//! 宿主的 `RustHttpHost` 也是按 `IOException` 处理的，所以 `From<NetError> for HostError` 会把它们
//! 归到 `Network` 里，**不改变上层的分类**。


/// 联网层能出的错。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// 连不上 / 被重置 / 证书 / DNS / HTTP 协议错……（宿主是 `java.io.IOException`）。
    ///
    /// [`kind`](Self::Network::kind) 是**粗分类**（DNS / 连不上 / 其余）：取订阅那边要把失败分成
    /// `unknown_host` / `connect` / `timeout` 三句界面文案，以前是 catch `java.net.UnknownHostException`
    /// 得出的，切到这一层之后只能在产生错误的地方记下来（见 [`NetworkKind`]）。
    #[error("网络错误：{detail}")]
    Network {
        detail: String,
        #[allow(missing_docs)]
        kind: NetworkKind,
    },

    /// 请求超时（连接 / 读 / 写 / 整体）。OkHttp 是 `SocketTimeoutException`（`IOException` 的子类）。
    #[error("超时：{detail}")]
    Timeout { detail: String },

    /// 被取消（`Call.cancel()`）。OkHttp 抛 `IOException("Canceled")`。
    #[error("已取消")]
    Cancelled,

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，**一个字节都没发**。
    /// 上层映射成 `ProxyUnavailableException`（cli 退出码 6），和普通网络错误分开。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 请求本身不合法（URL 解析不出来 / 头名不合法 / body 和方法不匹配）。
    /// 这类**在发请求之前**就失败，和网络错误分开（OkHttp 是 `IllegalArgumentException`）。
    #[error("请求不合法：{detail}")]
    BadRequest { detail: String },

    /// 这一层自己的意外（BUG / 宿主回调里抛了别的）。
    #[error("内部错误：{detail}")]
    Other { detail: String },
}

/// **网络错误的粗分类**。
///
/// 为什么需要它：`FfiProxyCoreFetchHost` 的契约是"失败只报一个分类串"（`unknown_host` /
/// `connect` / `timeout`），Rust 那边把它换成界面文案（`域名解析失败` / `连不上` / `超时`）。
/// 切到这一层之后没有人再 catch `java.net.UnknownHostException` 了，所以分类要在**产生错误的地方**做。
///
/// 口径照 Kotlin 那份 `classify(e)`：异常类型 → 三个串；其余按"这一层自己认识的那几档"落到
/// `NetError::kind()`（`proxy_unavailable` / `cancelled` / `bad_request` / `other`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkKind {
    /// 域名解析不出来（`java.net.UnknownHostException`）。
    UnknownHost,
    /// 连不上（`java.net.ConnectException`）。
    Connect,
    /// 超时（`java.net.SocketTimeoutException`）。
    Timeout,
    /// 其余（用 `NetError::kind()` 那个串）。
    Other,
}

impl NetworkKind {
    /// 给 `FfiProxyCoreFetchHost` 的那个分类串（**和 Kotlin 的 `classify` 逐字一致**）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnknownHost => "unknown_host",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Other => "other",
        }
    }
}

impl NetError {
    /// 一句话分类。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Timeout { .. } => "timeout",
            Self::Cancelled => "cancelled",
            Self::ProxyUnavailable { .. } => "proxy_unavailable",
            Self::BadRequest { .. } => "bad_request",
            Self::Other { .. } => "other",
        }
    }

    /// 粗分类（取订阅 / 界面文案用；**不含 URL**）。
    ///
    /// `Timeout` 归 [`NetworkKind::Timeout`]、`Network` 用它自己记下的那一档，其余按变体给。
    pub fn network_kind(&self) -> NetworkKind {
        match self {
            Self::Network { kind, .. } => *kind,
            Self::Timeout { .. } => NetworkKind::Timeout,
            _ => NetworkKind::Other,
        }
    }

    /// 是不是"可以换一条路线重试"的那类错（OkHttp `retryOnConnectionFailure` 的口径）。
    ///
    /// **只对连接建立阶段有意义**：调用方（[`crate::client`]）只在这个字段为 true **且**
    /// 请求还没发出去（或刚复用的连接立刻 EOF）时才重试。**已发出的请求绝不重试**。
    pub fn retryable_connect(&self) -> bool {
        matches!(self, Self::Network { .. } | Self::Timeout { .. })
    }
}

impl From<wreq::Error> for NetError {
    fn from(e: wreq::Error) -> Self {
        // **不带 URI**：URL 的 query 里可能有签名 / token，错误文案里一律不出现
        let e = e.without_uri();
        if e.is_timeout() {
            return Self::Timeout { detail: e.to_string() };
        }
        // 重定向次数超了：OkHttp 抛 `ProtocolException("Too many follow-up requests: 21")`。
        // 这一档归 Network（`IOException`），文案里说清楚。
        if e.is_redirect() {
            return Self::Network { detail: format!("重定向：{e}"), kind: NetworkKind::Other };
        }
        if e.is_builder() {
            return Self::BadRequest { detail: e.to_string() };
        }
        // DNS 和"连不上"要分开：取订阅的界面文案靠它（`域名解析失败` / `连不上`），
        // 而 Kotlin 那边是 catch `UnknownHostException` / `ConnectException` 分出来的。
        let kind = if e.is_dns() {
            NetworkKind::UnknownHost
        } else if e.is_connect() {
            NetworkKind::Connect
        } else {
            NetworkKind::Other
        };
        Self::Network { detail: e.to_string(), kind }
    }
}

/// 和 `feedhub-core/src/host.rs` 的 `HostError` 的对应关系：
///
/// | `NetError` | `HostError` | 为什么 |
/// | `ProxyUnavailable` | `ProxyUnavailable` | fail-closed 的语义要一路保到 cli 的退出码 6 |
/// | `Timeout` | `Network` | OkHttp 那边 `SocketTimeoutException` 是 `IOException` 的子类 |
/// | `Cancelled` | `Network`（detail = `Canceled`） | OkHttp 的 `Call.cancel()` 抛 `IOException("Canceled")` |
/// | `Network` | `Network` | 同名 |
/// | `BadRequest` | `Network` | OkHttp 那边 URL / 头不合法是 `IllegalArgumentException`，但宿主只在 IO 线程上 catch `IOException`；
///
/// **这个 `From` 故意不写在这一个 crate 里**：`feedhub-net` 不依赖 `feedhub-core`
/// （反过来才对——`feedhub-core` 一行网络依赖都不许有）。转换放在 `feedhub-ffi`（它同时依赖两边）。
///
/// 方便测试 / 日志：`NetError` 的 Debug 里**不含**任何 URL。
impl NetError {
    pub fn detail(&self) -> &str {
        match self {
            Self::Network { detail, .. }
            | Self::Timeout { detail }
            | Self::ProxyUnavailable { detail }
            | Self::BadRequest { detail }
            | Self::Other { detail } => detail,
            Self::Cancelled => "Canceled",
        }
    }
}

