//! 抖音的**设备环境 / 本地设备身份**过 UniFFI 的那一层。
//!
//! 纯函数 + 两个宿主端口（`HostStore` 读写 `ctx.store`、`HostRandom` 抽随机数），**不落盘、不发请求**。
//! 「已经在 `ctx.store` 里的值原样沿用」这条红线在 `feedhub_core::douyin::device` 里实现
//! （先读后写），这一层只做转发。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val env = douyinDeviceEnv()                 // 环境常量
//! val fp = douyinVerifyFp(store, nowMs, randomHost)   // 有就返回旧的，没有才生成并写回
//! val at = douyinInstallTime(store, nowMs)
//! val trace = douyinTraceId(randomHost)       // 每个扫码请求现生成

use std::sync::Arc;

use feedhub_core::douyin::device;

use crate::host::{HostRandom, HostStore, RandomAdapter, StoreAdapter};

/// 抖音设备环境（Kotlin `DouyinDevice` 的那组只读属性）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinDeviceEnv {
    pub chrome_version: String,
    pub browser_major: String,
    pub user_agent: String,
    pub platform: String,
    pub ua_platform: String,
    pub os_name: String,
    pub os_version: String,
    pub libra_divert: String,
    pub screen_width: i32,
    pub screen_height: i32,
    pub cpu_core_num: i32,
    pub device_memory: i32,
    pub window_width: i32,
    pub window_height: i32,
    /// 窗口指纹串的**实测原串**。
    pub fingerprint: String,
}

/// 抓包定下来的那套环境。
#[uniffi::export]
pub fn douyin_device_env() -> FfiDouyinDeviceEnv {
    let env = device::env();
    FfiDouyinDeviceEnv {
        chrome_version: env.chrome_version,
        browser_major: env.browser_major,
        user_agent: env.user_agent,
        platform: env.platform,
        ua_platform: env.ua_platform,
        os_name: env.os_name,
        os_version: env.os_version,
        libra_divert: env.libra_divert,
        screen_width: env.screen_width,
        screen_height: env.screen_height,
        cpu_core_num: env.cpu_core_num,
        device_memory: env.device_memory,
        window_width: env.window_width,
        window_height: env.window_height,
        fingerprint: env.fingerprint,
    }
}

/// `ctx.store` 里的 `s_v_web_id` 兜底值：**有就原样返回**（哪怕是空串），没有才生成一次并写回去。
///
/// [random] 要能连抽 [`douyinVerifyFpDraws`] 个 `[0, 1)` 的随机数（宿主那边是同一个 `Random`）。
#[uniffi::export]
pub fn douyin_verify_fp(
    store: Arc<dyn HostStore>,
    now_ms: i64,
    random: Arc<dyn HostRandom>,
) -> String {
    device::verify_fp(&StoreAdapter(store), now_ms, &RandomAdapter(random))
}

/// 这个 `verify_fp` 生成器要几个随机数。
#[uniffi::export]
pub fn douyin_verify_fp_draws() -> u32 {
    device::VERIFY_FP_DRAWS as u32
}

/// `ctx.store` 里的安装时间（秒）：有就返回，没有才取 [now_ms] / 1000 并写回去。
#[uniffi::export]
pub fn douyin_install_time(store: Arc<dyn HostStore>, now_ms: i64) -> i64 {
    device::install_time(&StoreAdapter(store), now_ms)
}

/// `biz_trace_id`：8 位十六进制，**不落盘、每次现生成**。要 [`douyinTraceIdDraws`] 个随机数。
#[uniffi::export]
pub fn douyin_trace_id(random: Arc<dyn HostRandom>) -> String {
    device::trace_id(&RandomAdapter(random))
}

/// 生成一个 `trace_id` 要几个随机数。
#[uniffi::export]
pub fn douyin_trace_id_draws() -> u32 {
    device::TRACE_ID_DRAWS as u32
}

/// `ctx.store` 里那两个键（排障 / 测试断言用；和 Kotlin `DouyinDevice` 的常量一致）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinDeviceKeys {
    pub verify_fp: String,
    pub install_time: String,
}

#[uniffi::export]
pub fn douyin_device_keys() -> FfiDouyinDeviceKeys {
    FfiDouyinDeviceKeys {
        verify_fp: device::KEY_VERIFY_FP.to_string(),
        install_time: device::KEY_INSTALL_TIME.to_string(),
    }
}
