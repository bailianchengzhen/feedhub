//! **代理**：这一层只**执行**宿主的判定，不自己再判一遍。
//!
//! 判定只有一处：`feedhub-core/src/proxy.rs` 的 `resolve` / `fallback_reason`。这一层收一份 [`ProxyPlan`]（每个平台一档），**照它办**：
//!
//! | [`Proxy`] | 怎么连 | 失败时 |
//! |---|---|---|
//! | [`Proxy::Direct`] | 不走代理 | 普通网络错误 |
//! | [`Proxy::Http`] | `CONNECT host:port` 隧道 | fail-closed（不回退直连） |
//! | [`Proxy::Socks5`] / [`Proxy::Socks5h`] | SOCKS5；`h` 表示**域名交给代理解析** | 同上 |
//! | [`Proxy::Unavailable`] | **一个字节都不发**，直接 [`crate::NetError::ProxyUnavailable`] | — |
//!
//! # fail-closed 是怎么保证的
//!
//! [`Proxy::Unavailable`] 的检查在 [`crate::client::NetClient::execute`] **最前面**，在 DNS / TCP **之前**；
//! 所以"代理不可用时一个字节都没发"是结构上成立的，不是靠"连失败了再回退"。
//! 测试用本机监听套接字的 accept 计数断言（`tests/fail_closed.rs`）。
//!
//! # SOCKS5 的两种解析口径
//!
//! OkHttp 的 SOCKS 是**本机解析域名**、把 IP 交给代理（`InetSocketAddress.createUnresolved` 只用于
//! `socks5h` 那种写法，而 Android/OkHttp 的 `Proxy.Type.SOCKS` 走的是 `createUnresolved` →
//! java.net 的 SOCKS 实现**在本地解析**）。wreq 的 `socks5://` 也是本地解析（`DnsResolve::Local`），
//! `socks5h://` 才是远端解析（`DnsResolve::Remote`）。**默认用 `socks5`**（= OkHttp 的行为），
//! 要远端解析时显式给 `socks5h`。

use std::fmt;

/// 这个平台这次怎么连（在 `feedhub-core::proxy::ProxyMode` 的基础上**丢掉**这一层不需要的信息）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Proxy {
    /// 直连。
    #[default]
    Direct,
    /// HTTP `CONNECT` 隧道（`http://host:port`）。
    Http { host: String, port: u16 },
    /// SOCKS5，**本机解析域名**（= OkHttp / java.net 的行为）。
    Socks5 { host: String, port: u16 },
    /// SOCKS5，**代理解析域名**（`socks5h://`，只有显式要这个才用）。
    Socks5h { host: String, port: u16 },
    /// 这个平台要用代理，但这次没有可用的：**fail-closed，一个字节都不发**。
    Unavailable {
        /// 一句话原因（**不含地址 / 端口**，和 `ProxyMode::unavailableReason` 的文案一致）。
        reason: String,
    },
}

/// 代理类型（和 `feedhub-core::proxy::ProxyType` 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

impl Proxy {
    /// 这个平台这次**不该发请求**（fail-closed）。
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// 转成 wreq 的 [`wreq::Proxy`]；不可用时返回 `None`（调用方在**解析代理之前**就失败）。
    pub(crate) fn to_wreq(&self) -> Option<wreq::Proxy> {
        let build = |scheme: &str, host: &str, port: u16| {
            let uri = format!("{scheme}://{host}:{port}");
            match wreq::Proxy::all(uri) {
                Ok(p) => Some(p),
                // 地址里带了非法字符（设置页理论上不让存）：当成不可用处理，**绝不悄悄直连**
                Err(_) => None,
            }
        };
        match self {
            Self::Direct => None,
            Self::Http { host, port } => build("http", host, *port),
            Self::Socks5 { host, port } => build("socks5", host, *port),
            Self::Socks5h { host, port } => build("socks5h", host, *port),
            Self::Unavailable { .. } => None,
        }
    }
}

impl fmt::Display for Proxy {
    /// **不含地址 / 端口**。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => write!(f, "直连"),
            Self::Http { .. } => write!(f, "HTTP 代理"),
            Self::Socks5 { .. } => write!(f, "SOCKS5 代理"),
            Self::Socks5h { .. } => write!(f, "SOCKS5 代理（远端解析）"),
            Self::Unavailable { reason } => write!(f, "代理不可用：{reason}"),
        }
    }
}

/// **一份代理快照**：每个平台一档（`NetClient` 构造时收）。
///
/// 为什么是"快照"而不是"每次问宿主"：和现在 `AppGraph.watchProxySettings` 的对齐方式一样——
/// 设置变了就**整份换新** `NetClient`（新连接池），旧连接留在旧池里自然回收
/// （`SourceContext.withFreshConnectionPool()` 的注释就是这么说的）。
#[derive(Debug, Clone, Default)]
pub struct ProxyPlan {
    /// 平台 id → 这一档。
    entries: Vec<(String, Proxy)>,
    /// 没有显式列出的平台用这一档（cli 的 `--proxy` 覆盖、单平台测试）。
    default: Proxy,
}

impl ProxyPlan {
    /// 全部直连。
    pub fn direct() -> Self {
        Self::default()
    }

    /// 所有没显式列出的平台都走这一档。
    pub fn with_default(mut self, proxy: Proxy) -> Self {
        self.default = proxy;
        self
    }

    /// 给某个平台指定一档（后设的覆盖先设的）。
    pub fn set(mut self, platform: &str, proxy: Proxy) -> Self {
        if let Some(slot) = self.entries.iter_mut().find(|(p, _)| p == platform) {
            slot.1 = proxy;
        } else {
            self.entries.push((platform.to_string(), proxy));
        }
        self
    }

    /// 这个平台这次怎么连。
    pub fn for_platform(&self, platform: &str) -> &Proxy {
        self.entries
            .iter()
            .find(|(p, _)| p == platform)
            .map(|(_, proxy)| proxy)
            .unwrap_or(&self.default)
    }

    /// 有没有任何一个平台是 fail-closed 的（测试 / 排障用）。
    pub fn any_unavailable(&self) -> bool {
        self.default.is_unavailable() || self.entries.iter().any(|(_, p)| p.is_unavailable())
    }
}

