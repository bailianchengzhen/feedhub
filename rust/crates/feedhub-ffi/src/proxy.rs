//! **按平台指定代理的判定**过 UniFFI 边界。
//!
//! Rust 侧是 [`feedhub_core::proxy`]（`core/Proxy.kt` 那套 `resolveProxyMode` / `proxyFallbackReason` /
//! `unavailable` / `unavailableReason` 的逐行移植）；这里只有 record / enum 和一层薄薄的转发。
//! Kotlin 门面在 `rust-bridge/.../feedhub/proxy/ProxyRules.kt`。
//!
//! 为什么不是"一个对象"：这一层是**纯函数**（没有宿主回调、没有缓存、没有锁），一个平台一次判定
//! 就是一次调用。别家不会再占。
//!
//! **地址 / 端口 / 主机名会过边界**（宿主本来就知道它们，没有新增暴露面）；原因文案里**不含**它们
//! （`unavailableReason` 的口径，日志和界面直接用）。
//!
//! **`valid` 不过边界**：判定（"这个平台的指定代理填全了没有 / 内置核心在不在跑"）是 Rust 现算的
//! （`host` 空不空、`port` 在不在 1..=65535），**不接受宿主传进来的"我以为它有效"**——不然"没填全就不发
//! 请求"这条判定就漏到 Kotlin 去了。宿主拿到 `FfiProxyMode` 之后按同一口径自己算
//! 一次（`core/Proxy.kt` 的 `Custom.valid` / `BuiltinCore.valid`，宿主建连要用它选替身地址），两份口径由
//! `proxy_custom_valid` / `proxy_builtin_valid` 和 `ProxyRulesParityTest` 的表守着。

use feedhub_core::proxy::{self, ProxyChoice, ProxyMode, ProxyType};

/// 代理类型（Kotlin `ProxyType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProxyType {
    Http,
    Socks5,
}

impl FfiProxyType {
    fn to_core(self) -> ProxyType {
        match self {
            Self::Http => ProxyType::Http,
            Self::Socks5 => ProxyType::Socks5,
        }
    }

    fn from_core(kind: ProxyType) -> Self {
        match kind {
            ProxyType::Http => Self::Http,
            ProxyType::Socks5 => Self::Socks5,
        }
    }
}

/// 这次实际怎么连（Kotlin `ProxyMode`）。
///
/// 字段叫 `kind` 而不是 `type`：`type` 在生成的 Kotlin 里虽然合法，但和 `ProxyMode.Custom.type`
/// 的序列化名放在一起看容易混；映射只有门面里的那一行。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProxyMode {
    Direct,
    System,
    Custom {
        host: String,
        port: i32,
        kind: FfiProxyType,
    },
    BuiltinCore {
        port: i32,
        reason: Option<String>,
    },
}

impl FfiProxyMode {
    fn to_core(&self) -> ProxyMode {
        match self {
            Self::Direct => ProxyMode::Direct,
            Self::System => ProxyMode::System,
            Self::Custom { host, port, kind, .. } => ProxyMode::Custom {
                host: host.clone(),
                port: *port,
                kind: kind.to_core(),
            },
            Self::BuiltinCore { port, reason, .. } => ProxyMode::BuiltinCore {
                port: *port,
                reason: reason.clone(),
            },
        }
    }

    pub(crate) fn from_core(mode: &ProxyMode) -> Self {
        match mode {
            ProxyMode::Direct => Self::Direct,
            ProxyMode::System => Self::System,
            ProxyMode::Custom { host, port, kind } => Self::Custom {
                host: host.clone(),
                port: *port,
                kind: FfiProxyType::from_core(*kind),
            },
            ProxyMode::BuiltinCore { port, reason } => Self::BuiltinCore {
                port: *port,
                reason: reason.clone(),
            },
        }
    }
}

/// 这个平台在设置里怎么选（Kotlin `ProxyChoice`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProxyChoice {
    Follow,
    Direct,
    Custom {
        host: String,
        port: i32,
        kind: FfiProxyType,
    },
}

impl FfiProxyChoice {
    fn to_core(&self) -> ProxyChoice {
        match self {
            Self::Follow => ProxyChoice::Follow,
            Self::Direct => ProxyChoice::Direct,
            Self::Custom { host, port, kind } => ProxyChoice::Custom {
                host: host.clone(),
                port: *port,
                kind: kind.to_core(),
            },
        }
    }
}

/// **唯一判定处**：这个平台这次怎么连（Kotlin `resolveProxyMode`）。
///
/// 参数和 Kotlin 一模一样：平台的 `choice`、全局 `global`（调用方已经把「内置核心」的端口 / 原因填好）、
/// 出口锁 `exit_lock`、是不是境外平台 `overseas`。
#[uniffi::export]
pub fn proxy_resolve(
    choice: FfiProxyChoice,
    global: FfiProxyMode,
    exit_lock: bool,
    overseas: bool,
) -> FfiProxyMode {
    FfiProxyMode::from_core(&proxy::resolve(
        &choice.to_core(),
        &global.to_core(),
        exit_lock,
        overseas,
    ))
}

/// 「因为出口锁关着、全局代理用不了而改成直连」的原因（一句话，**不含地址**）；没有就是 None。
/// Kotlin `proxyFallbackReason`。
#[uniffi::export]
pub fn proxy_fallback_reason(
    choice: FfiProxyChoice,
    global: FfiProxyMode,
    exit_lock: bool,
) -> Option<String> {
    proxy::fallback_reason(&choice.to_core(), &global.to_core(), exit_lock)
}

/// 这一档现在用不用得了（Kotlin `ProxyMode.unavailable`）。
#[uniffi::export]
pub fn proxy_mode_unavailable(mode: FfiProxyMode) -> bool {
    mode.to_core().unavailable()
}

/// 用不了时的一句话原因（**不含地址 / 端口 / 主机名**）；用得了就是 None。
/// Kotlin `ProxyMode.unavailableReason`。
#[uniffi::export]
pub fn proxy_mode_unavailable_reason(mode: FfiProxyMode) -> Option<String> {
    mode.to_core().unavailable_reason()
}

/// 「指定代理」的地址 / 端口填全了没有（Kotlin `Custom.valid` / `ProxyChoice.Custom.valid` /
/// `ProxyConfig.valid` 用的是同一个口径）。
#[uniffi::export]
pub fn proxy_custom_valid(host: String, port: i32) -> bool {
    proxy::custom_valid(&host, port)
}

/// 内置代理核心在跑没有（Kotlin `ProxyMode.BuiltinCore.valid`）。
#[uniffi::export]
pub fn proxy_builtin_valid(port: i32) -> bool {
    proxy::builtin_valid(port)
}

