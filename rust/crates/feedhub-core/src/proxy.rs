//! **按平台指定代理的判定**：`core/Proxy.kt` 的 `resolveProxyMode` /
//! `proxyFallbackReason` / `ProxyMode.unavailable` / `unavailableReason` 的逐行移植。
//!
//! 这一层只回答一个问题：**这个平台这次该走哪个代理，还是干脆不发请求**。真正建连 / 选 OkHttp
//! 客户端 / 探测连通性、把 [`ProxyMode`] 换成 `java.net.Proxy` 都还在宿主（`core/Proxy.kt` 的
//! `PlatformProxySelector` / `ProxyConfigGuardInterceptor` / `toOkHttpProxy`）——和"收发留在宿主"一条规矩。
//!
//! 规则：
//!
//! 1. [`ProxyChoice::Custom`]（这个平台单独指定）**优先**，**永远 fail-closed**：地址 / 端口没填全就返回
//!    一个 `valid == false` 的 [`ProxyMode::Custom`]（发请求前被宿主拦下），出口锁对它**不起作用**，
//!    **绝不回退直连**；
//! 2. [`ProxyChoice::Direct`] → 直连；但**境外平台没有这一档**（红线），拿到它也按 [`ProxyChoice::Follow`]
//!    办（正常情况下设置页根本不会让用户选到这一档）；
//! 3. [`ProxyChoice::Follow`] → 全局代理；出口锁**关着**且全局代理**用不了**时改为直连。
//!
//! 两种"用不了"（[`ProxyMode::unavailable`]）：`Custom` 的地址 / 端口没填全、`BuiltinCore`（内置代理核心）
//! 没在运行。`Direct` / `System` 永远是用户自己选的两档，不算"用不了"。
//!
//! **口径**：`host.isNotBlank()` 是 Kotlin 的 `Char.isWhitespace()`（= Java 的 `Character.isWhitespace ||
//! Character.isSpaceChar`），所以 NBSP（U+00A0）/ U+2007 / U+202F 也算空白，而 U+0085 **不算**——和
//! `crate::hosted::is_blank`（只照 Java 的 `Character.isWhitespace`）不一样，别混用。
//! 判定表由 `ProxyRulesParityTest`逐个组合守着。

/// 代理类型（对应 Kotlin `ProxyType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyType {
    /// HTTP CONNECT 隧道。
    Http,
    /// SOCKS5。
    Socks5,
}

/// 这个平台**这次实际**怎么连（对应 Kotlin `ProxyMode`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyMode {
    /// 直连（不走任何代理）。
    Direct,
    /// 跟随系统（JVM 的 `ProxySelector` / 环境变量那一套）。
    System,
    /// 指定代理。地址 / 端口没填全时 [`ProxyMode::valid`] 是 false——那是一个"必然连不上"的替身，
    /// 真正让请求失败的是宿主的 `ProxyConfigGuardInterceptor`（在发请求之前抛错，**绝不悄悄直连**）。
    Custom {
        host: String,
        port: i32,
        kind: ProxyType,
    },
    /// 内置代理核心（本机 mixed 入站的 SOCKS5）。`port` 不在 1..=65535 时 = 没在运行。
    BuiltinCore { port: i32, reason: Option<String> },
}

/// 这个平台在设置里怎么选（对应 Kotlin `ProxyChoice`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyChoice {
    /// 跟随全局（默认）。
    Follow,
    /// 直连（境外平台没有这一档，见 [`resolve`]）。
    Direct,
    /// 这个平台单独走一个代理。
    Custom {
        host: String,
        port: i32,
        kind: ProxyType,
    },
}

impl ProxyMode {
    /// 这一档现在能不能用（对应 Kotlin `ProxyMode.valid`）：`Custom` 要地址 + 端口填全，
    /// `BuiltinCore` 要在运行。`Direct` / `System` 永远可以用。
    pub fn valid(&self) -> bool {
        match self {
            Self::Direct | Self::System => true,
            Self::Custom { host, port, .. } => custom_valid(host, *port),
            Self::BuiltinCore { port, .. } => builtin_valid(*port),
        }
    }

    /// 这一档现在**用不了**（对应 Kotlin `ProxyMode.unavailable`）。
    pub fn unavailable(&self) -> bool {
        match self {
            Self::Direct | Self::System => false,
            Self::Custom { .. } | Self::BuiltinCore { .. } => !self.valid(),
        }
    }

    /// 用不了时的一句话原因（**不含地址 / 端口 / 主机名**）；用得了就是 `None`。
    /// 对应 Kotlin `ProxyMode.unavailableReason`，文案逐字一样。
    pub fn unavailable_reason(&self) -> Option<String> {
        match self {
            Self::Direct | Self::System => None,
            Self::Custom { .. } => {
                if self.valid() {
                    None
                } else {
                    Some("「指定代理」的地址或端口没填全".to_string())
                }
            }
            Self::BuiltinCore { reason, .. } => {
                if self.valid() {
                    return None;
                }
                Some(match reason.as_deref() {
                    Some(text) if !is_blank(text) => text.to_string(),
                    _ => "内置代理核心没有运行".to_string(),
                })
            }
        }
    }
}

impl ProxyChoice {
    /// 这个选择本身填全了没有（对应 Kotlin `ProxyChoice.Custom.valid`；别的档没有这个概念，算 true）。
    pub fn valid(&self) -> bool {
        match self {
            Self::Follow | Self::Direct => true,
            Self::Custom { host, port, .. } => custom_valid(host, *port),
        }
    }
}

/// 「指定代理」的地址 / 端口填全了没有（Kotlin `ProxyMode.Custom.valid` / `ProxyChoice.Custom.valid`）。
pub fn custom_valid(host: &str, port: i32) -> bool {
    !is_blank(host) && (1..=65535).contains(&port)
}

/// 内置代理核心在跑没有（Kotlin `ProxyMode.BuiltinCore.valid`）。
pub fn builtin_valid(port: i32) -> bool {
    (1..=65535).contains(&port)
}

/// **唯一判定处**：这个平台这次怎么连（对应 Kotlin `resolveProxyMode`）。
///
/// 参数就是 Kotlin 那四个：平台的 [`ProxyChoice`]、全局 [`ProxyMode`]（调用方已经把「内置核心」的端口 /
/// 原因填进去了）、出口锁、是不是境外平台。
pub fn resolve(
    choice: &ProxyChoice,
    global: &ProxyMode,
    exit_lock: bool,
    overseas: bool,
) -> ProxyMode {
    let mode = match choice {
        // 单独指定：直接用（下面那一段出口锁的兜底**不管它**，fail-closed）
        ProxyChoice::Custom { host, port, kind } => ProxyMode::Custom {
            host: host.clone(),
            port: *port,
            kind: *kind,
        },
        // 境外平台没有「直连」这一档：按「跟随全局」办（红线）
        ProxyChoice::Direct => {
            if overseas {
                global.clone()
            } else {
                ProxyMode::Direct
            }
        }
        ProxyChoice::Follow => global.clone(),
    };
    if matches!(choice, ProxyChoice::Custom { .. }) {
        return mode;
    }
    if exit_lock || !mode.unavailable() {
        mode
    } else {
        // 出口锁关着、全局代理又用不了：改为直连（用户自己关的锁）
        ProxyMode::Direct
    }
}

/// 「因为出口锁关着、全局代理由用不了而改成直连」的原因（一句话，**不含地址**）；没有这回事就是 `None`。
/// 对应 Kotlin `proxyFallbackReason`：单独指定 / 直连的平台永远没有。
pub fn fallback_reason(
    choice: &ProxyChoice,
    global: &ProxyMode,
    exit_lock: bool,
) -> Option<String> {
    match choice {
        ProxyChoice::Custom { .. } | ProxyChoice::Direct => None,
        ProxyChoice::Follow => {
            if exit_lock {
                None
            } else {
                global.unavailable_reason()
            }
        }
    }
}

/// Kotlin `Char.isWhitespace()`：Java 的 `Character.isWhitespace(ch) || Character.isSpaceChar(ch)`。
///
/// 和 `crate::hosted::is_blank`（只照 `Character.isWhitespace`）的差别：**多了** Zs 类里 Java 不算
/// `isWhitespace` 的那几个（U+00A0 / U+2007 / U+202F），**少了** U+0085（Java 两个判定都不认它，
/// 而 Rust 的 `char::is_whitespace` 认）。代理地址里不会出现这些字符，但对拍比的是"设置页边打字边存"
/// 的中间状态，口径必须逐字一样。
pub fn is_blank(text: &str) -> bool {
    text.chars().all(is_kotlin_whitespace)
}

fn is_kotlin_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000D}'      // TAB / LF / VT / FF / CR（isWhitespace）
            | '\u{001C}'..='\u{001F}' // FILE/GROUP/RECORD/UNIT SEPARATOR（isWhitespace）
            | '\u{0020}'              // SPACE（两者都是）
            | '\u{00A0}'              // NBSP（isSpaceChar）
            | '\u{1680}'              // OGHAM SPACE MARK（两者都是）
            | '\u{2000}'..='\u{200A}' // EN QUAD … HAIR SPACE（含 U+2007 FIGURE SPACE，只在 isSpaceChar 里）
            | '\u{2028}'              // LINE SEPARATOR（两者都是）
            | '\u{2029}'              // PARAGRAPH SEPARATOR（两者都是）
            | '\u{202F}'              // NARROW NO-BREAK SPACE（isSpaceChar）
            | '\u{205F}'              // MEDIUM MATHEMATICAL SPACE（两者都是）
            | '\u{3000}'              // IDEOGRAPHIC SPACE（两者都是）
    )
}

