//! 预加载策略（`app/.../feed/Preload.kt` 的 `PreloadPolicy` / `FeedPreloadTrigger`）。
//!
//! 平台接口请求不比正常滑动多**。
//!
//! - 信息流下一页（平台接口）：触发点按屏算——插电剩不到 [`FEED_SCREENS_PLUGGED`] 屏、电池剩不到
//!   [`FEED_SCREENS_BATTERY`] 屏就取，但至少和原来一样"剩不到 [`MIN_REMAINING`] 张"就取。**一次只取一页**，
//!   而且**取完一页后用户不往下滑就不取下一页**（[`FeedPreloadTrigger`]）：总请求数 = 实际滑过的页数 + 最多 1 页。
//! - 卡片图片（只打图片 CDN，不打平台接口）：往下 [`IMAGE_SCREENS_PLUGGED`] / [`IMAGE_SCREENS_BATTERY`] 屏。
//! - Coil 内存缓存：插电时放大到 [`PLUGGED_MEMORY_CACHE_BYTES`]，电池时回到 Coil 的默认值。
//!
//! **谁是宿主**：屏幕尺寸 / 滚动位置 / 是否插电 / 现在一屏几张都是宿主测出来的，这里只做判断
//! （`near_end` / `prefetch_range` / `memory_cache_bytes` / [`FeedPreloadTrigger::on_frame`]）。
//! `start()` 那个回调（真的发起 loadMore）在宿主：Rust 只能回答"该不该发起"，发起本身是协程。

/// 插电时：剩不到 1.5 屏就取下一页。
pub const FEED_SCREENS_PLUGGED: f32 = 1.5;
/// 电池时：剩不到 1 屏就取下一页。
pub const FEED_SCREENS_BATTERY: f32 = 1.0;
/// 插电时：图片预取到可见区下面 2 屏。
pub const IMAGE_SCREENS_PLUGGED: f32 = 2.0;
/// 电池时：图片预取到可见区下面 1 屏。
pub const IMAGE_SCREENS_BATTERY: f32 = 1.0;
/// 原来的触发点：剩不到 8 张。按屏算的触发点不会比它晚。
pub const MIN_REMAINING: i32 = 8;
/// 插电时 Coil 内存缓存的上限（1.5 GiB）。
pub const PLUGGED_MEMORY_CACHE_BYTES: i64 = 1536 * 1024 * 1024;
/// 图片预取同时最多几个（低于正在显示的图片：显示的不受这个限制）。
pub const IMAGE_PARALLEL: i32 = 2;

/// 信息流触发点用几屏（[`feed_screens`]）。
pub fn feed_screens(plugged: bool) -> f32 {
    if plugged {
        FEED_SCREENS_PLUGGED
    } else {
        FEED_SCREENS_BATTERY
    }
}

/// 图片预取用几屏（[`image_screens`]）。
pub fn image_screens(plugged: bool) -> f32 {
    if plugged {
        IMAGE_SCREENS_PLUGGED
    } else {
        IMAGE_SCREENS_BATTERY
    }
}

/// Kotlin `Int.coerceAtLeast(1)`。
fn at_least_one(value: i32) -> i32 {
    if value < 1 {
        1
    } else {
        value
    }
}

/// 最后一张可见卡片是 `last_visible`（下标），一共 `total` 张，一屏大约 `per_screen` 张：
/// 剩下的不到 `screens` 屏了吗。
///
/// 阈值 = `max(MIN_REMAINING, ceil(per_screen.coerceAtLeast(1) * screens))`；`total <= 0` 或者
/// `last_visible < 0` 一律 false（还没布局出来，不触发）。
pub fn near_end(last_visible: i32, total: i32, per_screen: i32, screens: f32) -> bool {
    if total <= 0 || last_visible < 0 {
        return false;
    }
    let remaining = total - 1 - last_visible;
    let scaled = (at_least_one(per_screen) as f32 * screens).ceil();
    // Kotlin 的 `ceil(...).toInt()` 对 NaN 给 0；这里照抄，免得 NaN 屏数变成一个巨大阈值
    let scaled = if scaled.is_nan() { 0 } else { scaled as i32 };
    let threshold = if MIN_REMAINING > scaled {
        MIN_REMAINING
    } else {
        scaled
    };
    remaining < threshold
}

/// 要预取图片的卡片下标区间（**闭区间**，和 Kotlin 的 `IntRange` 一样；空区间用 `start > end` 表示）：
/// 可见区下面 `screens` 屏。
///
/// Kotlin 原版返回 `(lastVisible + 1) until minOf(total, lastVisible + 1 + n)`，空的时候是
/// `IntRange.EMPTY`（`1..0`）。这里返回 `(start, end)`，`end < start` 就是空。
pub fn prefetch_range(last_visible: i32, total: i32, per_screen: i32, screens: f32) -> (i32, i32) {
    if last_visible < 0 || total <= 0 {
        return (1, 0);
    }
    let scaled = (at_least_one(per_screen) as f32 * screens).ceil();
    let n = if scaled.is_nan() { 0 } else { scaled as i32 };
    let start = last_visible + 1;
    let upper = last_visible + 1 + n;
    let end = if total < upper { total } else { upper };
    (start, end - 1)
}

/// Coil 内存缓存的上限：插电放大，电池用 Coil 默认的 `initial`（默认比放大的还大就不动）。
pub fn memory_cache_bytes(plugged: bool, initial: i64) -> i64 {
    if plugged {
        if initial > PLUGGED_MEMORY_CACHE_BYTES {
            initial
        } else {
            PLUGGED_MEMORY_CACHE_BYTES
        }
    } else {
        initial
    }
}

/// 信息流"该取下一页了吗"（`FeedPreloadTrigger`）的状态机：剩的不到 N 屏、没有一页在飞，
/// 并且**从上次发起之后可见位置变过**（用户停着不动，取完一页就不再取；原来的写法要等"快到底"
/// 这个条件变一次才再取，一页太少时会卡住不再加载）。
///
/// 宿主每帧调 [`on_frame`](Self::on_frame)，`start` 是"真的调一次 loadMore"——返回 false 表示这次没发
/// （正在加载 / 到底了），不算数，下次还能发。
#[derive(Debug, Default)]
pub struct FeedPreloadTrigger {
    last_load_at: i32,
}

impl FeedPreloadTrigger {
    pub fn new() -> Self {
        Self { last_load_at: -1 }
    }

    /// 这一帧**该不该**发起一次 loadMore（前两条判断，宿主还没真的发）。
    ///
    /// 条件：没有一页在飞、可见位置和上次发起时不同、剩的不到 N 屏。
    pub fn should_start(
        &self,
        last_visible: i32,
        total: i32,
        per_screen: i32,
        screens: f32,
        loading: bool,
    ) -> bool {
        if loading || last_visible == self.last_load_at {
            return false;
        }
        near_end(last_visible, total, per_screen, screens)
    }

    /// 宿主**真的**发起了一次（`loadMore()` 返回 true）：记下这次的位置，用户不动就不再取。
    pub fn note_started(&mut self, last_visible: i32) {
        self.last_load_at = last_visible;
    }

    /// 一次问完（老接口）：`should_start` 为真时由调用方调 `start()`，成功就记位置。
    ///
    /// 留着它是给测试 / 单测用的（宿主那条路走 `should_start` + `note_started` 两步，
    /// 因为"发起"是宿主的协程，Rust 里没有回调）。
    pub fn on_frame(
        &mut self,
        last_visible: i32,
        total: i32,
        per_screen: i32,
        screens: f32,
        loading: bool,
        start: impl FnOnce() -> bool,
    ) -> bool {
        if !self.should_start(last_visible, total, per_screen, screens, loading) {
            return false;
        }
        if !start() {
            return false;
        }
        self.note_started(last_visible);
        true
    }

    /// 冷启动 / 换了一批（宿主在"新的一批上屏"时调）：把"上次发起的位置"清回初始值。
    ///
    /// Kotlin 那边是 `remember(state) { FeedPreloadTrigger() }` 跟着 FeedState 活，不清；
    /// 这里留着这个入口给"换了一个信息流"的场合，行为和不调完全一样。
    pub fn reset(&mut self) {
        self.last_load_at = -1;
    }
}
