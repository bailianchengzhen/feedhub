//! **限速的排队决策**：`core/RateLimit.kt` 的 `RateLimitQueue` + cli 的跨进程限速 / 冷却。
//!
//! 搬进来的是**决策**，不是"睡"：
//! - 请求排在哪（前台插队、后台保护期）、下一张票什么时候能发、取消后谁往前挪——[`RateLimitQueue`]；
//! - 跨进程限速（`~/.cache/feedhub/cli-rate/<平台>.rate`）：该等多久、记下这次的时间——[`CrossProcess`]；
//! - 风控冷却（`~/.cache/feedhub/cooldown/<平台>.json`）：到没到、还剩多久、标记 / 清掉 / 列出来——[`CooldownStore`]。
//!
//! **留在宿主**：OkHttp 的 `Interceptor` 薄适配——问 Rust "要等多久"，然后 `Thread.sleep` / 看 `Call` 取消
//! （见 `rust-bridge/.../feedhub/ratelimit/RustRateLimiter.kt`）。宿主每 100ms 醒一次问一次，
//! Rust 这边一次调用只算一个时间点，**不睡、不占线程**。
//!
//! ## 口径（和 Kotlin 逐条对齐）
//!
//! - 间隔按"后一个票的抖动"算：相邻两张票的时间差 = `min_interval + 后一张的 jitter`，所以
//!   **任意两个真正发出去的请求之间 ≥ 最小间隔**；
//! - 抖动**排进来时摇一次**（重排不变）：`(random.nextDouble() * (jitterMs + 1)).toLong()`；
//! - 前台插到"第一个还没等太久的后台请求"前面，后台一律排到最后（同一档先来后到）；
//! - 取消的票退出队列、后面的往前挪（仍然不早于"上一个真发出去的 + 间隔"）；
//! - 时钟 / 随机数**都是注入的**：Rust 里不读时钟、不取随机数（和这一层其余模块的规矩一样）。
//!
//! 随机量按 Kotlin `java.util.Random(seed)` 的 `nextDouble()` 复刻（同一个种子 → 同一串抖动），
//! 见下面的 [`KotlinRandom`]。对拍（`RateLimitQueueParityTest`）拿 Kotlin 原版逐票对拍。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 一个请求的优先级。
///
/// 没打 tag 的请求就是 [`RequestPriority::Background`]——和 Kotlin 一样，没改过的平台一个字不变
/// （只有一道先来后到的队）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPriority {
    /// 用户正盯着结果等的（详情正文、评论、楼中楼、播放地址）：排到后台前面。
    Interactive,
    /// 顺手取的（刷新 / 作者页 / 沉浸预取 / 表情表）。
    Background,
}

/// 一张排队的票。字段只在队列的锁里改（Rust 这里靠 `&mut` 队列本身保证）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub priority: RequestPriority,
    /// 这个请求自己的抖动（毫秒），排进来时摇一次；重排时不变（间隔不会越来越整齐）。
    pub jitter_ms: i64,
    /// 排进来的时刻（毫秒时间戳）。
    pub enqueued_at: i64,
    /// 按现在的排队，它该在什么时刻发出去（毫秒时间戳）。
    pub scheduled_at: i64,
    /// 已经放行了（票已离开队列）。
    pub granted: bool,
}

impl Ticket {
    /// 票的稳定 id（队列内部用它找票；JNA 那边票是一个不透明数字，见 ffi）。
    pub fn id(&self) -> u64 {
        // (enqueued_at, jitter, priority) 不足以唯一（同一毫秒可以排好几张），所以外面再套一个自增号，
        // 这里只给测试用一个人能看的标签。
        self.enqueued_at as u64
    }
}

/// 排队状态（Kotlin `RateLimitQueue`）：**纯逻辑**，不睡、不联网、不碰 OkHttp。
///
/// 内部按"票的序号"记账（[`TicketHandle`]），这样宿主那边可以只持有一个数字、不用把整个结构搬过 FFI。
#[derive(Debug)]
pub struct RateLimitQueue {
    min_interval_ms: i64,
    jitter_ms: i64,
    background_grace_ms: i64,
    random: KotlinRandom,
    /// 自增序号：票的 id。
    next_id: u64,
    /// 排队的票（按放行顺序）。
    pending: Vec<Entry>,
    /// 上一个真正放行的时刻；`None` = 还没放行过（第一个请求不用等）。
    last_grant_at: Option<i64>,
}

/// 队列里的一张票 + 它的 id。
#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    ticket: Ticket,
}

/// 票的句柄（过 FFI 就是一个数字）。
pub type TicketHandle = u64;

impl RateLimitQueue {
    /// 造一个队列。`random` 用 [`KotlinRandom::new`]（种子由调用方给，测试里固定）。
    pub fn new(min_interval_ms: i64, jitter_ms: i64, background_grace_ms: i64, seed: i64) -> Self {
        Self {
            min_interval_ms,
            jitter_ms,
            background_grace_ms,
            random: KotlinRandom::new(seed),
            next_id: 1,
            pending: Vec::new(),
            last_grant_at: None,
        }
    }

    /// 排进队列，返回票的句柄（Kotlin `enqueue`）。
    pub fn enqueue(&mut self, priority: RequestPriority, now: i64) -> TicketHandle {
        let jitter = if self.jitter_ms > 0 {
            // Kotlin: `(random.nextDouble() * (jitterMs + 1)).toLong()`
            (self.random.next_double() * (self.jitter_ms + 1) as f64) as i64
        } else {
            0
        };
        let id = self.next_id;
        self.next_id += 1;
        let ticket = Ticket {
            priority,
            jitter_ms: jitter,
            enqueued_at: now,
            scheduled_at: now,
            granted: false,
        };
        let at = self.insert_at(priority, now);
        self.pending.insert(at, Entry { id, ticket });
        self.reschedule(now);
        id
    }

    /// 队头到点就放行；返回它在队列里等了多久（毫秒）。还没轮到返回 `None`（票留在队列里）。
    pub fn try_grant(&mut self, handle: TicketHandle, now: i64) -> Option<i64> {
        let Some(first) = self.pending.first() else {
            return None;
        };
        if first.id != handle {
            return None;
        }
        if now < first.ticket.scheduled_at {
            return None;
        }
        let entry = self.pending.remove(0);
        let waited = (now - entry.ticket.enqueued_at).max(0);
        self.last_grant_at = Some(now);
        self.reschedule(now);
        Some(waited)
    }

    /// 还要等多久（毫秒）。非队头给的是"按现在的排法"的预计时刻；队列变了会变（Kotlin `waitMs`）。
    pub fn wait_ms(&self, handle: TicketHandle, now: i64) -> i64 {
        match self.pending.iter().find(|e| e.id == handle) {
            None => 0, // 已经放行（或者根本不在队里）
            Some(e) => (e.ticket.scheduled_at - now).max(0),
        }
    }

    /// 没发出去的请求退出（取消 / 出错）：让出时间槽，后面的往前挪。已经放行的票是空操作。
    pub fn release(&mut self, handle: TicketHandle, now: i64) {
        if let Some(pos) = self.pending.iter().position(|e| e.id == handle) {
            self.pending.remove(pos);
            self.reschedule(now);
        }
    }

    /// 还在这张票手里吗（放行 / 取消之后就没有了）。
    pub fn is_pending(&self, handle: TicketHandle) -> bool {
        self.pending.iter().any(|e| e.id == handle)
    }

    /// 队列里有几个（排障 / 测试看状态）。
    pub fn size(&self) -> usize {
        self.pending.len()
    }

    /// 队列里第一张票的句柄（排障用）。
    pub fn head(&self) -> Option<TicketHandle> {
        self.pending.first().map(|e| e.id)
    }

    /// 某张票现在的样子（测试 / 排障用；已经被放行 / 取消就是 `None`）。
    pub fn ticket(&self, handle: TicketHandle) -> Option<&Ticket> {
        self.pending.iter().find(|e| e.id == handle).map(|e| &e.ticket)
    }

    /// 前台请求插到哪：插在**第一个还没等太久的后台请求**前面；已经等了 `background_grace_ms` 以上的
    /// 后台请求不会被新来的前台请求插队（不让后台被饿死）。后台请求一律排到最后。
    fn insert_at(&self, priority: RequestPriority, now: i64) -> usize {
        if priority == RequestPriority::Background {
            return self.pending.len();
        }
        for (i, e) in self.pending.iter().enumerate() {
            if e.ticket.priority == RequestPriority::Background
                && now - e.ticket.enqueued_at < self.background_grace_ms
            {
                return i;
            }
        }
        self.pending.len()
    }

    /// 从队头开始重排时间槽：第一个 ≥ `max(now, 上次放行 + 间隔 + 自己的抖动)`，后面每个再往后推。
    fn reschedule(&mut self, now: i64) {
        let mut previous = self.last_grant_at;
        for e in self.pending.iter_mut() {
            let at = match previous {
                None => now,
                Some(p) => now.max(p + self.min_interval_ms + e.ticket.jitter_ms),
            };
            e.ticket.scheduled_at = at;
            previous = Some(at);
        }
    }
}

// ===========================================================================
// Kotlin 的随机数（只要 nextDouble）
// ===========================================================================

/// `java.util.Random(seed)` 的 `nextDouble()`（**只要这一个**，够抖动用）。
///
/// 口径（JDK 的实现，逐位照抄）：
/// - 种子：`(seed xor 0x5DEECE66D) & ((1<<48)-1)`；
/// - `next(bits)`：`seed = (seed * 0x5DEECE66D + 0xB) & mask`，返回 `seed >>> (48 - bits)`；
/// - `nextDouble()`：`((next(26) as long) << 27) + next(27)` 再除以 `2^53`。
///
/// 为什么不用 Kotlin `Random(seed)`（`brain/rng.rs` 那份 `XorWowRandom`）：限速器的 Kotlin 版用的是
/// **`java.util.Random()`**（不是 `kotlin.random.Random`），两套算法完全不一样。这里并排放着两个是对的，
/// 别"统一"成一份。
#[derive(Debug, Clone)]
pub struct KotlinRandom {
    seed: u64,
}

impl KotlinRandom {
    const MULTIPLIER: u64 = 0x5DEECE66D;
    const ADDEND: u64 = 0xB;
    const MASK: u64 = (1 << 48) - 1;

    /// `java.util.Random(seed)`（**注意** Kotlin 的 `Random(seed)` 是另一台机器，别混）。
    pub fn new(seed: i64) -> Self {
        Self {
            seed: ((seed as u64) ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    /// `java.util.Random()` 无参构造那种"按当前时间播种"——**不用**：时钟由调用方注入，
    /// 要"随机种子"就传一个（生产里宿主传 `System.nanoTime()` 之类）。
    fn next(&mut self, bits: u32) -> u32 {
        self.seed = (self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::ADDEND))
            & Self::MASK;
        (self.seed >> (48 - bits)) as u32
    }

    /// `nextDouble()`：`[0, 1)`。
    pub fn next_double(&mut self) -> f64 {
        let hi = self.next(26) as u64;
        let lo = self.next(27) as u64;
        ((hi << 27) + lo) as f64 / (1u64 << 53) as f64
    }
}

// ===========================================================================
// 跨进程限速（cli）
// ===========================================================================

/// 跨进程限速的**决策**（Kotlin `CrossProcessRateLimit`）。
///
/// 文件格式一个字都没变：`~/.cache/feedhub/cli-rate/<平台>.rate` 里就一个十进制毫秒时间戳
/// （没有换行以外的装饰），`<平台>.rate.lock` 是空文件（`flock` 的门闩）。
///
/// **谁睡、谁拿锁**：宿主（cli）在拿锁之后问 Rust "还要等多久"（[`CrossProcess::plan`]），睡完再问一次
/// "记下了吗"（[`CrossProcess::commit`]）。这样 Rust 这边只有纯计算 + 读写那个小文件，不睡线程。
/// 读改写整段都在宿主的文件锁里（和 Kotlin 的 `FileLocks.withLock` 一个位置）。
#[derive(Debug, Clone)]
pub struct CrossProcess {
    dir: PathBuf,
    intervals: Vec<(String, i64)>,
    default_interval_ms: i64,
}

impl CrossProcess {
    /// 国内平台 1.5 秒；境外几家更长（值见 [`Self::default_intervals`]）。
    pub const DEFAULT_INTERVAL_MS: i64 = 1_500;

    /// 按平台的最小间隔（**只往长调**，变短要先问用户）。
    pub fn default_intervals() -> Vec<(String, i64)> {
        [
            ("bili", 1_500),
            ("zhihu", 1_500),
            ("tieba", 1_500),
            ("heybox", 1_500),
            ("coolapk", 1_500),
            ("yt", 2_000),
            ("reddit", 2_000),
            ("x", 3_000),
            ("xhs", 3_000),
            ("douyin", 3_000),
            ("tiktok", 3_000),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            intervals: Self::default_intervals(),
            default_interval_ms: Self::DEFAULT_INTERVAL_MS,
        }
    }

    /// 测试 / 调参用：换一张间隔表。
    pub fn with_intervals(mut self, intervals: Vec<(String, i64)>, default_interval_ms: i64) -> Self {
        self.intervals = intervals;
        self.default_interval_ms = default_interval_ms;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn interval_for(&self, platform: &str) -> i64 {
        self.intervals
            .iter()
            .find(|(k, _)| k == platform)
            .map(|(_, v)| *v)
            .unwrap_or(self.default_interval_ms)
    }

    fn state_file(&self, platform: &str) -> PathBuf {
        self.dir.join(format!("{platform}.rate"))
    }

    /// 这个平台上次请求记在什么时候（没有记录 / 读不出来返回 `None`）。
    pub fn last_request_at(&self, platform: &str) -> Option<i64> {
        read_i64(&self.state_file(platform)).filter(|v| *v > 0)
    }

    /// 还要等多久（毫秒）才轮到下一次（0 = 现在就能发）。**不写文件**。
    ///
    /// 等价于 Kotlin `acquire` 里 `wait = (last + intervalFor - now).coerceAtLeast(0)` 那一步。
    pub fn wait_ms(&self, platform: &str, now: i64) -> i64 {
        let last = self.last_request_at(platform).unwrap_or(0);
        (last + self.interval_for(platform) - now).max(0)
    }

    /// 记下这次请求的时刻（原子替换：先写 `<名字>.tmp-<pid>` 再改名，和 Kotlin `writeAtomically` 一样）。
    ///
    /// 调用方必须**先拿到文件锁**、而且在 [`Self::wait_ms`] 返回 0 之后调（否则会写下一个还没发出去的
    /// 时间，把下一个进程推得更晚）。
    pub fn commit(&self, platform: &str, now: i64) -> Result<(), String> {
        write_atomically(&self.state_file(platform), &now.to_string())
    }
}

// ===========================================================================
// 风控冷却（cli）
// ===========================================================================

/// 一条冷却记录（Kotlin `CooldownStore.Entry`，一条 JSON 对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CooldownEntry {
    pub platform: String,
    /// 冷却到什么时候（Unix 毫秒）。
    pub until: i64,
    /// 什么时候写下的（Unix 毫秒）。
    pub at: i64,
    /// 为什么：`risk_control` / `captcha_required` / `http_403` 这类。
    pub reason: String,
}

/// 风控冷却（Kotlin `CooldownStore`）：`~/.cache/feedhub/cooldown/<平台>.json`。
///
/// `App` **不受影响**：App 是用户本人在用，这里只是 cli 自己的闸。所以这一份只在 cli 的装配里用。
///
/// 文件的 JSON 格式照 kotlinx.serialization 的 `prettyPrint`（`FeedJson` 那边 prettyPrint = true）：
/// ```text
/// {
///     "platform": "bili",
///     "until": 1700000000000,
///     "at": 1699998200000,
///     "reason": "http_403"
/// }
/// ```
/// **读的时候宽容**（未知字段忽略、缺字段报错），写的时候按上面这一份（顺序 = 声明顺序）。
pub struct CooldownStore {
    dir: PathBuf,
    ttl_ms: i64,
}

/// 冷却判定结果（`check` 的结论）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CooldownCheck {
    /// 没在冷却。
    Clear,
    /// 正在冷却：还剩 `retry_after_ms`，到 `until`。
    Cooling {
        until: i64,
        retry_after_ms: i64,
        reason: String,
    },
}

impl CooldownStore {
    /// 冷却 30 分钟：够风控消气，又不至于把 agent 卡太久。
    pub const DEFAULT_TTL_MS: i64 = 30 * 60 * 1000;

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ttl_ms: Self::DEFAULT_TTL_MS,
        }
    }

    pub fn with_ttl_ms(mut self, ttl_ms: i64) -> Self {
        self.ttl_ms = ttl_ms;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ttl_ms(&self) -> i64 {
        self.ttl_ms
    }

    fn file(&self, platform: &str) -> PathBuf {
        self.dir.join(format!("{platform}.json"))
    }

    /// 正在冷却就返回还差多久；过期了就**顺手删掉**（和 Kotlin `check` 一样）。
    pub fn check(&self, platform: &str, now: i64) -> CooldownCheck {
        let Some(entry) = self.read(platform) else {
            return CooldownCheck::Clear;
        };
        if entry.until > now {
            return CooldownCheck::Cooling {
                until: entry.until,
                retry_after_ms: entry.until - now,
                reason: entry.reason,
            };
        }
        let _ = std::fs::remove_file(self.file(platform));
        CooldownCheck::Clear
    }

    /// 写一笔冷却（覆盖这个平台原来的标记）。失败返回错误，调用方只打一行 stderr
    /// （不能盖住原来那个错误）。
    pub fn mark(&self, platform: &str, reason: &str, now: i64) -> Result<(), String> {
        let entry = CooldownEntry {
            platform: platform.to_string(),
            until: now + self.ttl_ms,
            at: now,
            reason: reason.to_string(),
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        write_atomically(&self.file(platform), &encode_entry(&entry))
    }

    /// 现在在冷却的平台，按 `until` 从早到晚（Kotlin `list`）。
    pub fn list(&self, now: i64) -> Vec<CooldownEntry> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<CooldownEntry> = entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|e| self.read_file(&e.path()))
            .filter(|e| e.until > now)
            .collect();
        out.sort_by_key(|e| e.until);
        out
    }

    /// 清掉一个平台的冷却。清掉了（文件本来在）返回 true。
    pub fn clear(&self, platform: &str, now: i64) -> bool {
        let path = self.file(platform);
        let existed = path.exists();
        let _ = std::fs::remove_file(&path);
        // 顺手收掉过期的，免得 list 上还挂着
        if self.read(platform).map(|e| e.until <= now).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
        existed
    }

    fn read(&self, platform: &str) -> Option<CooldownEntry> {
        self.read_file(&self.file(platform))
    }

    fn read_file(&self, path: &Path) -> Option<CooldownEntry> {
        let text = std::fs::read_to_string(path).ok()?;
        decode_entry(&text).ok()
    }
}

/// 编一条冷却记录（**逐字节**照 kotlinx.serialization `prettyPrint` 的那一份）。
pub fn encode_entry(e: &CooldownEntry) -> String {
    let mut out = String::with_capacity(160);
    out.push_str("{\n    \"platform\": ");
    crate::hosted::push_json_string(&mut out, &e.platform);
    out.push_str(",\n    \"until\": ");
    out.push_str(&e.until.to_string());
    out.push_str(",\n    \"at\": ");
    out.push_str(&e.at.to_string());
    out.push_str(",\n    \"reason\": ");
    crate::hosted::push_json_string(&mut out, &e.reason);
    out.push_str("\n}");
    out
}

/// 解一条冷却记录（未知字段忽略）。缺字段 / 类型不对就是读不懂（调用方当"没有"）。
pub fn decode_entry(text: &str) -> Result<CooldownEntry, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or_else(|| "不是 JSON 对象".to_string())?;
    let platform = obj
        .get("platform")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "缺 platform".to_string())?
        .to_string();
    let until = obj
        .get("until")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "缺 until".to_string())?;
    let at = obj.get("at").and_then(|v| v.as_i64()).ok_or_else(|| "缺 at".to_string())?;
    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "缺 reason".to_string())?
        .to_string();
    Ok(CooldownEntry {
        platform,
        until,
        at,
        reason,
    })
}

// ===========================================================================
// 小工具
// ===========================================================================

/// 读一个只有整数的文件（读不出来 / 不是数字 → `None`）。
fn read_i64(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse::<i64>().ok()
}

/// 先写临时文件再原子改名（Kotlin `writeAtomically`：`<名字>.tmp-<pid>` → `rename`）。
pub fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}

/// 一队请求的"现在谁该发"快照（给宿主一次性问完，少过几次 FFI）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    /// 队头还要等多久（毫秒，0 = 现在就能发）。
    pub head_wait_ms: i64,
    /// 队列里有几张票。
    pub size: usize,
}

impl RateLimitQueue {
    /// 队头还要等多久 + 队列长度（宿主每 100ms 问一次这个就够）。
    pub fn snapshot(&self, now: i64) -> QueueSnapshot {
        QueueSnapshot {
            head_wait_ms: self.head().map(|h| self.wait_ms(h, now)).unwrap_or(0),
            size: self.pending.len(),
        }
    }
}

/// 清掉名字里的重复（写 `cleared_domains` 时用的口径和 Kotlin `HashSet<String>` 一样）。
pub fn unique_domains(domains: &[String]) -> HashSet<String> {
    domains.iter().map(|d| d.trim_start_matches('.').to_string()).collect()
}

