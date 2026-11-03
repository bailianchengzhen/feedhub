//! X 的限速策略、`x-rate-limit-*` 记账、一次刷新两页的账本与风控信号。
//!
//! 对齐 Kotlin `feedhub.source.x.XRateLimit` / `XRatePolicy`：
//! - 两次请求之间至少 2.5 秒 + 0~800ms 抖动；
//! - 页面 / 脚本 1 秒 + 0~300ms 抖动；
//! - 一次刷新最多 2 页；
//! - 429 / 错误码 88 / `x-rate-limit-remaining: 0` / looks automated 立刻停。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::x::XError;

/// 限速策略常量（对齐 Kotlin `XRatePolicy`）。
pub struct XRatePolicy;

impl XRatePolicy {
    pub const MIN_INTERVAL_MS: i64 = 2500;
    pub const JITTER_MS: i64 = 800;
    pub const MAX_PAGES_PER_REFRESH: u32 = 2;

    /// 页面 / 脚本请求（`whoami` 的 `x.com/home`、txid 三跳的页面和 JS）的间隔：1 秒 + 0~300ms 抖动。
    pub const PAGE_MIN_INTERVAL_MS: i64 = 1000;
    pub const PAGE_JITTER_MS: i64 = 300;
}

/// `x-rate-limit-*` 响应头（对齐 Kotlin `XRateLimit`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XRateLimit {
    pub limit: Option<i64>,
    pub remaining: Option<i64>,
    pub reset_at_sec: Option<i64>,
}

impl XRateLimit {
    /// 余额用完了（`remaining <= 0`）。
    pub fn exhausted(&self) -> bool {
        self.remaining.map(|r| r <= 0).unwrap_or(false)
    }

    /// 从响应头解析（大小写不敏感）。三个头全没有时返回 `None`。
    pub fn from_headers(headers: &[(String, String)]) -> Option<Self> {
        let find_header = |name: &str| -> Option<&str> {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim())
        };
        let limit = find_header("x-rate-limit-limit").and_then(|v| v.parse::<i64>().ok());
        let remaining = find_header("x-rate-limit-remaining").and_then(|v| v.parse::<i64>().ok());
        let reset = find_header("x-rate-limit-reset").and_then(|v| v.parse::<i64>().ok());

        if limit.is_none() && remaining.is_none() && reset.is_none() {
            None
        } else {
            Some(Self {
                limit,
                remaining,
                reset_at_sec: reset,
            })
        }
    }
}

/// 记最近一次响应的 `x-rate-limit-*`，并在下一次请求前检查（对齐 Kotlin `XRateLimiter`）。
/// 没有余额就直接返回 `RiskControlException` 对应的错误，一个请求都不发。
pub struct XRateLimiter {
    last: Mutex<Option<XRateLimit>>,
}

impl Default for XRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl XRateLimiter {
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    pub fn last(&self) -> Option<XRateLimit> {
        *self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn record(&self, headers: &[(String, String)]) {
        if let Some(parsed) = XRateLimit::from_headers(headers) {
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(parsed);
        }
    }

    pub fn check(&self, _platform: &str) -> Result<(), XError> {
        let current = *self.last.lock().unwrap_or_else(|e| e.into_inner());
        let Some(limit) = current else {
            return Ok(());
        };
        if !limit.exhausted() {
            return Ok(());
        }
        let reset = limit.reset_at_sec;
        let message = if let Some(r) = reset {
            if r > 0 {
                format!(
                    "这个接口的额度用完了（x-rate-limit-remaining=0），服务端说 {} 之后才重置",
                    r
                )
            } else {
                "这个接口的额度用完了（x-rate-limit-remaining=0）".to_string()
            }
        } else {
            "这个接口的额度用完了（x-rate-limit-remaining=0）".to_string()
        };
        Err(XError::RiskControl {
            code: Some("rate-limit".to_string()),
            detail: message,
        })
    }

    pub fn clear(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// "一次刷新最多翻两页"的账本（对齐 Kotlin `XPageBudget`）。
pub struct XPageBudget {
    max_pages: u32,
    pages: Mutex<HashMap<String, u32>>,
}

impl Default for XPageBudget {
    fn default() -> Self {
        Self::new(XRatePolicy::MAX_PAGES_PER_REFRESH)
    }
}

impl XPageBudget {
    pub fn new(max_pages: u32) -> Self {
        Self {
            max_pages,
            pages: Mutex::new(HashMap::new()),
        }
    }

    pub fn allow_next_page(&self, key: &str, first_page: bool) -> bool {
        let mut map = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        let count = if first_page {
            1
        } else {
            map.get(key).copied().unwrap_or(1) + 1
        };
        map.insert(key.to_string(), count);
        count < self.max_pages
    }

    pub fn pages_this_refresh(&self, key: &str) -> u32 {
        self.pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    pub fn reset(&self) {
        self.pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// 响应正文里像"被风控 / 被当成机器人"的信号（对齐 Kotlin `xRiskSignalIn`）。
pub fn x_risk_signal_in(body: &str) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("looks like it might be automated") {
        Some("服务端提示「looks automated」")
    } else if lower.contains("to protect our users from spam") {
        Some("服务端提示疑似自动化流量")
    } else if lower.contains("rate limit exceeded") {
        Some("服务端提示 rate limit exceeded")
    } else {
        None
    }
}
