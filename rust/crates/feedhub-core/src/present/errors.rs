//! 出错时界面上那一句**人话**（`app/detail/FailureText.kt` 的 `friendlyError`）。
//!
//! 只有这几种：没网、没登录、被风控拦了、这个平台看不了、加载失败。技术细节走日志，不进界面。
//!
//! **异常归类复用 [`crate::appapi::classify`] 那一套 [`FailureKind`]**：宿主（Kotlin）只做
//! "`Throwable` → 种类"那一次 `when`（`is NotLoggedInException -> …` 之类），
//! "种类 → 界面上的字"只有这里一份。这样对外接口的错误码和界面文案不会各说各的。

use crate::appapi::FailureKind;

/// `UnsupportedOperationException` 的 `message` 多长就不再摆到界面上（Kotlin `UNSUPPORTED_MAX`）。
const UNSUPPORTED_MAX: usize = 24;

/// 异常种类 + 可选的原话 → 界面上那一句。
///
/// * [`FailureKind::NotLoggedIn`] → `"没登录"`
/// * [`FailureKind::RiskControl`] → `"被风控拦了，稍后再试"`
/// * [`FailureKind::Network`] → `"网络错误"`
/// * [`FailureKind::NotSupported`] → 原话（够短、不带路径），否则 `"这个平台看不了"`
/// * 其余 → `"加载失败"`
pub fn friendly_error(kind: FailureKind, message: Option<&str>) -> String {
    match kind {
        FailureKind::NotLoggedIn => "没登录".to_string(),
        FailureKind::RiskControl => "被风控拦了，稍后再试".to_string(),
        FailureKind::Network => "网络错误".to_string(),
        FailureKind::NotSupported => message_or_friendly(message),
        _ => "加载失败".to_string(),
    }
}

/// "这个平台看不了"这类**故意抛出来**的说明本身就是给用户看的，但历史上有几句写成了给开发看的
/// ：太长、带路径的换成一句人话。
fn message_or_friendly(message: Option<&str>) -> String {
    match message {
        // Kotlin：`m.length <= 24`（**字符数**，不是字节数）
        Some(m) if m.chars().count() <= UNSUPPORTED_MAX && !m.contains('/') => m.to_string(),
        _ => "这个平台看不了".to_string(),
    }
}
