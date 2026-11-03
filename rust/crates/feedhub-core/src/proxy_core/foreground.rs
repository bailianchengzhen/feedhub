//! 「内置代理核心**只在前台跑**」的判定表（`core/proxy/ForegroundCoreLifecycle.kt` 的 Rust 版）。
//!
//! 所以安卓上核心不必常驻，退到后台就停，省内存 / 省电。
//!
//! 规则：
//!
//! - 设置选了「内置核心」**且 App 在前台** → 起核心；
//! - 退到后台**等 `stop_delay_ms`（默认 30 秒）再停**——这期间切回来就不停
//!   （切出去回个消息再回来不用重连）；
//! - 设置改成别的档位 → **立刻停**（不占内存）；
//! - 在后台把设置改成「内置核心」不会起（前台没有请求）；
//! - 被系统杀掉后回到前台：Activity 重新走一遍 `onStart` → `set_foreground(true)` → 重新起
//!   （端口会变，客户端按 `AppCore.watchProxyCore` 观察的核心状态换）。
//!
//! **这一层是纯状态机**：`std::time::Instant` 都不用，`now_ms` 由宿主注入（测试用假时钟，
//! 秒级精度就够）。**延迟本身由宿主做**：[`ForegroundGate::poll`] 返回"现在该不该跑"，宿主每几百毫秒问一次就行。
//!
//! 为什么不给一个"什么时候该醒"的时间戳：停核心的宽限期只有一处、30 秒，宿主已经在跑一个
//! `poll` 循环了（核心状态本身也要它推进）。多一条"该在几毫秒之后再看一眼"的约定，
//! 就多一个两边对不上的机会。

/// 「只在前台跑」的判定表。**不是线程安全的**（宿主在一条协程 / 一条线程上用），要跨线程就加锁。
#[derive(Debug, Clone)]
pub struct ForegroundGate {
    /// 退到后台之后等多久停核心（毫秒）。
    stop_delay_ms: i64,
    /// App 现在在前台吗（宿主从 `onStart` / `onStop` 喂进来）。
    foreground: bool,
    /// 设置里现在选的是「内置核心」吗。
    enabled: bool,
    /// 上一次**报给宿主**的值（同一个值不重复通知）。
    notified: bool,
    /// 退到后台的时刻（`foreground` 从 true 变 false 的那一下）；在前台时是 `None`。
    backgrounded_at_ms: Option<i64>,
}

impl Default for ForegroundGate {
    fn default() -> Self {
        ForegroundGate::new(DEFAULT_STOP_DELAY_MS)
    }
}

/// 退到后台之后等多久停核心（毫秒）。
pub const DEFAULT_STOP_DELAY_MS: i64 = 30_000;

impl ForegroundGate {
    pub fn new(stop_delay_ms: i64) -> ForegroundGate {
        ForegroundGate {
            stop_delay_ms,
            foreground: false,
            enabled: false,
            notified: false,
            backgrounded_at_ms: None,
        }
    }

    pub fn foreground(&self) -> bool {
        self.foreground
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 现在是不是"核心该跑"（宽限期里退到后台仍算 true）。
    pub fn should_run(&self) -> bool {
        self.notified
    }

    /// 设置里选了「内置核心」/ 换成别的档位。返回**要不要通知宿主**（`Some(true/false)` = 该跑 / 该停）。
    pub fn set_enabled(&mut self, value: bool, _now_ms: i64) -> Option<bool> {
        if value == self.enabled {
            return None;
        }
        self.enabled = value;
        if !value {
            // 换档位立刻停
            self.backgrounded_at_ms = None;
            return self.notify(false);
        }
        if self.foreground {
            self.backgrounded_at_ms = None;
            return self.notify(true);
        }
        // 后台选「内置核心」不起，等回前台
        None
    }

    /// App 进前台 / 退到后台（安卓侧从 Activity 生命周期喂）。
    pub fn set_foreground(&mut self, value: bool, now_ms: i64) -> Option<bool> {
        if value == self.foreground {
            return None;
        }
        self.foreground = value;
        if value {
            self.backgrounded_at_ms = None;
            if self.enabled {
                return self.notify(true);
            }
            return None;
        }
        // 退到后台：本来就没在跑就不用排停
        if !self.notified {
            return None;
        }
        self.backgrounded_at_ms = Some(now_ms);
        None
    }

    /// 宿主定时器调：宽限期到了没有？该停就返回 `Some(false)`（并记住已经通知过）。
    ///
    /// **不用 `now_ms` 之外的任何东西**：所以在测试里可以把 30 秒一秒一秒地推过去。
    pub fn poll(&mut self, now_ms: i64) -> Option<bool> {
        let Some(backgrounded_at) = self.backgrounded_at_ms else {
            return None;
        };
        if !self.notified {
            self.backgrounded_at_ms = None;
            return None;
        }
        if now_ms - backgrounded_at >= self.stop_delay_ms {
            self.backgrounded_at_ms = None;
            return self.notify(false);
        }
        None
    }

    /// 宽限期还剩多少毫秒（宿主可以据此少醒几次；`None` = 没在等）。
    pub fn pending_stop_in_ms(&self, now_ms: i64) -> Option<i64> {
        let at = self.backgrounded_at_ms?;
        if !self.notified {
            return None;
        }
        Some((self.stop_delay_ms - (now_ms - at)).max(0))
    }

    fn notify(&mut self, value: bool) -> Option<bool> {
        if self.notified == value {
            return None;
        }
        self.notified = value;
        Some(value)
    }
}

