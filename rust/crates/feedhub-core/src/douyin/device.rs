//! 抖音的**设备环境 + 本地设备身份**。
//!
//! 两件东西，边界很清楚：
//!
//! - **环境常量**（[`env`]）：UA / 平台 / 屏幕 / 窗口 / 核数 / 内存 / 实测指纹原串。
//! query 里再报另一套就自相矛盾；a_bogus 的 `sdkMinor` 是拿那条 UA 解出来的。
//!   所以改一处就要把整套（UA / `sec-ch-ua-platform` / 指纹串平台段 / query 里的 `browser_platform`）
//!   一起改。这些值只是**数据**，Rust 不生成、不按机器探测。
//! - **本地标识**：[`verify_fp`]（Cookie 里没有 `s_v_web_id` 时用的兜底值）和 [`install_time`]：
//!   **第一次用的时候生成一次，存进 `ctx.store`（键 `douyin.verify_fp` / `douyin.install_time`），
//!   之后一直原样沿用**。风控红线"不伪造设备或身份"：store 里已经有值的时候**一个字节都不许重新生成**
//!   （[`verify_fp`] / [`install_time`] 先读后写，读到了就直接返回）。
//!   [`trace_id`]（`biz_trace_id`）是每个扫码请求现生成的一个 8 位十六进制串，**不落盘**。
//!
//! **随机量和时钟都由宿主注入**（`RandomHost` / `now_ms`）：Rust 不读系统时钟、不自己造随机源，
//! 对拍才能用同一条流逐字节比。随机数的取用顺序写死在 [`generate_verify_fp`] / [`trace_id`] 里
//! （先 8 位、再 4 位、再 3 位 + 1 位固定前缀、再 4 位、最后 12 位；`trace_id` 取 4 个字节），
//! 宿主按这个顺序喂同一串 `[0,1)` 的随机数，两边结果就一样。
//!
//! **和 Kotlin 原实现唯一的差别**：原版用 `java.util.Random.nextInt(n)` 取下标，这里按
//! `floor(draw * n)` 从宿主给的随机流里取——`RandomHost::draw` 是 `Math.random()` 的口径
//! （a_bogus 那条路一直是这么做的），`java.util.Random` 的内部状态没法从 `f64` 流复现。
//! 生成规则（长度 / 分组 / 字符表 / 时间戳 base36）逐字照抄。

use crate::host::{RandomHost, StoreHost};

/// `ctx.store` 里 `s_v_web_id` 兜底值的键（和 Kotlin `DouyinDevice.KEY_VERIFY_FP` 一样，定了就别改）。
pub const KEY_VERIFY_FP: &str = "douyin.verify_fp";
/// `ctx.store` 里安装时间的键（秒）。
pub const KEY_INSTALL_TIME: &str = "douyin.install_time";

/// cv-cat 的 `_SV_CHARSET`：数字 + 大小写字母（`utils/dy_util.py` 的 `generate_s_v_web_id`）。
const CHARSET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 生成 `verify_fp` 要几个随机数：`8 + 4 + 3 + 1 + 3 + 12`（第 3 组固定以 `4` 开头，第 4 组取 `8/9/a/b`）。
pub const VERIFY_FP_DRAWS: usize = 8 + 4 + 3 + 1 + 3 + 12;

/// 生成 `trace_id` 要几个随机数（4 个字节 = 8 位十六进制）。
pub const TRACE_ID_DRAWS: usize = 4;

/// 设备环境（[`env`] 的返回值）。
///
/// 字段和 Kotlin `DouyinDevice` 的属性一一对应，**一个都不加工**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEnv {
    /// 和 UA 里的 Chrome 主版本必须一致（`sec-ch-ua` 要从 UA 抽主版本回填）。
    pub chrome_version: String,
    /// [`DeviceEnv::chrome_version`] 的第一段（`sec-ch-ua` 用）。
    pub browser_major: String,
    /// 和这次抓包的 UA 逐字节一致（`sdkMinor` 就是拿它解出来的）。
    pub user_agent: String,
    /// 窗口指纹串的最后一段 / query 的 `browser_platform` / `os_version` 的来源。
    pub platform: String,
    /// `sec-ch-ua-platform` 的写法（带引号）。
    pub ua_platform: String,
    pub os_name: String,
    pub os_version: String,
    pub libra_divert: String,
    pub screen_width: i32,
    pub screen_height: i32,
    pub cpu_core_num: i32,
    pub device_memory: i32,
    /// 浏览器窗口（比屏幕小：有标题栏和地址栏），只进窗口指纹串。
    pub window_width: i32,
    pub window_height: i32,
    /// 窗口指纹串的**实测原串**。
    pub fingerprint: String,
}

/// 抓包那台的 Chrome 版本（和 UA 里的主版本一起改）。
pub const CHROME_VERSION: &str = "155.0.0.0";
/// 抓包那台的平台串（UA 里是同一段）。
pub const PLATFORM: &str = "Linux x86_64";

/// 调用方只读）。
pub fn env() -> DeviceEnv {
    let chrome_version = CHROME_VERSION.to_string();
    let browser_major = chrome_version
        .split('.')
        .next()
        .unwrap_or(CHROME_VERSION)
        .to_string();
    let user_agent = format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{chrome_version} Safari/537.36"
    );
    DeviceEnv {
        chrome_version,
        browser_major,
        user_agent,
        platform: PLATFORM.to_string(),
        ua_platform: "\"Linux\"".to_string(),
        os_name: "Linux".to_string(),
        os_version: "x86_64".to_string(),
        libra_divert: "Linux".to_string(),
        screen_width: 1920,
        screen_height: 1080,
        cpu_core_num: 16,
        device_memory: 16,
        window_width: 1920,
        window_height: 915,
        fingerprint: "1920|915|0|0|1920|1080|1920|1080|Linux x86_64".to_string(),
    }
}

/// 一次要把需要的随机数全抽出来，然后按**固定顺序**消费（宿主那边是同一串 `[0,1)`）。
struct Picks {
    values: Vec<f64>,
    at: usize,
}

impl Picks {
    fn new(random: &dyn RandomHost, count: usize) -> Picks {
        Picks {
            values: random.draw(count),
            at: 0,
        }
    }

    /// 取一个 `[0, bound)` 的下标：`floor(x * bound)`，`x` 越界（宿主给了 <0 / >=1 的值）时夹住。
    ///
    /// 和 Kotlin 参照实现的 `(draw * bound).toInt()` 同一个口径（`.toInt()` 也是截断）。
    fn pick(&mut self, bound: usize) -> usize {
        let value = self.values.get(self.at).copied().unwrap_or(0.0);
        self.at += 1;
        if bound == 0 || !value.is_finite() || value <= 0.0 {
            return 0;
        }
        let scaled = value * bound as f64;
        let index = scaled as usize;
        index.min(bound - 1)
    }
}

/// `verify_` + 毫秒时间戳的 base36 + 四组随机，**形态照 cv-cat 的实测**（`utils/dy_util.py` 的
/// `generate_s_v_web_id`，只读参考）：`verify_<base36>_<8>_<4>_<4>_<12>`，后四组是 UUIDv4 的形状
/// （第 3 组固定以 `4` 开头，第 4 组取 `8/9/a/b` 之一），但每位从 62 字符表里取，不是十六进制。
///
/// 有明显相关性。
pub fn generate_verify_fp(now_ms: i64, random: &dyn RandomHost) -> String {
    let mut picks = Picks::new(random, VERIFY_FP_DRAWS);
    let mut out = String::with_capacity(8 + 16 + 4 + 2 + 4 + 2 + 4 + 2 + 4 + 2 + 12);
    out.push_str("verify_");
    out.push_str(&long_to_string_radix(now_ms, 36));
    out.push('_');
    for _ in 0..8 {
        out.push(charset_at(picks.pick(CHARSET.len())));
    }
    out.push('_');
    for _ in 0..4 {
        out.push(charset_at(picks.pick(CHARSET.len())));
    }
    out.push_str("_4");
    for _ in 0..3 {
        out.push(charset_at(picks.pick(CHARSET.len())));
    }
    out.push('_');
    out.push("89ab".as_bytes()[picks.pick(4)] as char);
    for _ in 0..3 {
        out.push(charset_at(picks.pick(CHARSET.len())));
    }
    out.push('_');
    for _ in 0..12 {
        out.push(charset_at(picks.pick(CHARSET.len())));
    }
    out
}

/// `ctx.store` 里的 `s_v_web_id` 兜底值：**有就用**（哪怕是空串），没有才生成一次并写回去。
pub fn verify_fp(store: &dyn StoreHost, now_ms: i64, random: &dyn RandomHost) -> String {
    if let Some(existing) = store.get(KEY_VERIFY_FP) {
        return existing;
    }
    let value = generate_verify_fp(now_ms, random);
    store.put(KEY_VERIFY_FP, Some(&value));
    value
}

/// 装"装了这个 App / 第一次打开"的时间（秒）：有就用，没有才取当前时间并写回去（十进制字符串）。
///
/// 读不出整数（存档被改成垃圾）时和 Kotlin `toLongOrNull()` 一样当成没有 → 重新取一次。
pub fn install_time(store: &dyn StoreHost, now_ms: i64) -> i64 {
    if let Some(existing) = store.get(KEY_INSTALL_TIME) {
        if let Ok(value) = existing.parse::<i64>() {
            return value;
        }
    }
    let value = now_ms / 1000;
    store.put(KEY_INSTALL_TIME, Some(&value.to_string()));
    value
}

/// `biz_trace_id`：8 位十六进制（参考实现是 UUID 去掉横线后取前 8 位）。**不落盘、每次现生成**。
pub fn trace_id(random: &dyn RandomHost) -> String {
    let mut picks = Picks::new(random, TRACE_ID_DRAWS);
    let mut out = String::with_capacity(8);
    for _ in 0..TRACE_ID_DRAWS {
        out.push_str(&format!("{:02x}", picks.pick(256)));
    }
    out
}

fn charset_at(index: usize) -> char {
    CHARSET[index.min(CHARSET.len() - 1)] as char
}

/// `java.lang.Long.toString(value, radix)`：负数是 `-` + 绝对值，十进制以外的字母小写。
///
/// 只在 `verify_fp` 的时间戳上用到（正数），但按原口径写全，免得以后有人拿它拼别的串。
pub fn long_to_string_radix(value: i64, radix: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let radix = radix.clamp(2, 36) as u64;
    if value == 0 {
        return "0".to_string();
    }
    let negative = value < 0;
    // i64::MIN 取绝对值会溢出，用 u64 的无符号绝对值
    let mut n = if negative {
        (value as i128).unsigned_abs() as u64
    } else {
        value as u64
    };
    let mut buf = Vec::with_capacity(16);
    while n > 0 {
        buf.push(DIGITS[(n % radix) as usize]);
        n /= radix;
    }
    if negative {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

