//! 曝光 / 停留的几何与判定（`app/.../feed/ExposureTracker.kt`）。
//!
//! 从达标那一刻起累计停留，到不再达标（滚出视口 / 被盖住 / 窗口失焦）时结算。
//!
//! 这一块搬进 Rust 的是**全部判定和计时**：
//! - [`visible_fraction`]：一条卡片落在视口里的高度比例（瀑布流每列高度不一样，所以按**卡片自己的
//!   位置和尺寸**算，不能按行算；卡片横向完整可见，所以高度比例就是面积比例）；
//! - [`visible_items`] / [`related_visible_items`]：从"哪些下标在屏幕上、各自在哪、多高"算出可见的卡片
//!   （下标对不上内容的——网格末尾整行的 footer、历史页的日期分组头——直接跳过）；
//! - [`exposure_inputs`]：把界面那三个原始输入（设置开关 / 被全屏详情盖住 / 窗口有没有焦点）折成
//!   `enabled` + `foreground` 两个值；
//! - [`ExposureTracker`]：跟踪哪个 key 在可见、停留多久、什么时候结算（**状态机**）。
//!
//! **留宿主（Kotlin）的**：滚动 / 可见性的**测量**（`LazyStaggeredGridState.layoutInfo` 那类框架接口）、
//! 定时器（[`EXPOSURE_TICK_MS`] 的循环、后台挂起等回前台）、以及结算时**写资料库**
//! （`Library.recordExposure` / `recordSeenItem`）——那是宿主的 IO，Rust 把要写的几行算好交回去。
//!
//! 两个语义要点：
//! - `enabled = false` 是**丢弃**（用户不想记了），`foreground = false` 是**结算**（正在看的这一段照常写库）；
//! - `exposed_at` = 这一段的开始、`last_seen_at` = 这一段的结束。

/// 卡片要有多少比例落在视口里才算"看到了"。
pub const EXPOSURE_MIN_VISIBLE_FRACTION: f32 = 0.5;

/// 要连续可见多久（毫秒）才算一次曝光。
pub const EXPOSURE_MIN_VISIBLE_MS: i64 = 1_000;

/// 界面多久喂一次当前可见情况（毫秒）。门槛只要 1 秒的精度，500ms 够用，也不费电。
/// 循环本身在宿主（定时器），这个常数只是"两边记的是同一个数"。
pub const EXPOSURE_TICK_MS: i64 = 500;

/// 一个可见项在网格里的位置：下标（用来找回条目）、顶边、高度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleEntry {
    pub index: i32,
    pub top: i32,
    pub height: i32,
}

/// 一条内容的最小身份（曝光只看 key 和平台；整条 `FeedItem` 在宿主那边）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRef {
    pub key: String,
    pub platform: String,
}

/// 这一刻屏幕上的一条内容：身份 + 它落在视口里的高度比例（0~1）。
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleItem {
    pub key: String,
    pub platform: String,
    pub visible_fraction: f32,
}

/// 一次结算要写的那几行（宿主拿去写资料库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settle {
    pub key: String,
    pub platform: String,
    /// 这一段可见的开始时刻（`exposed_at`）。
    pub started_at_ms: i64,
    /// 这一段一共停留了多少毫秒。
    pub dwell_ms: i64,
    /// 这一段的结束时刻（`last_seen_at`）。
    pub last_seen_at_ms: i64,
}

/// 一条卡片落在视口里的高度比例（0~1）。
///
/// [item_top] / [viewport_top] / [viewport_bottom] 在同一个坐标空间（列表的滚动坐标）。
pub fn visible_fraction(item_top: i32, item_height: i32, viewport_top: i32, viewport_bottom: i32) -> f32 {
    if item_height <= 0 || viewport_bottom <= viewport_top {
        return 0.0;
    }
    let top = if item_top > viewport_top { item_top } else { viewport_top };
    let bottom = if item_top + item_height < viewport_bottom {
        item_top + item_height
    } else {
        viewport_bottom
    };
    let visible = bottom - top;
    if visible <= 0 {
        0.0
    } else {
        let fraction = visible as f32 / item_height as f32;
        if fraction > 1.0 {
            1.0
        } else {
            fraction
        }
    }
}

/// 从"哪些下标在屏幕上、各自在哪、多高"算出可见的卡片（顺序 = `entries` 的顺序）。
pub fn visible_items(
    entries: &[VisibleEntry],
    items: &[Option<FeedRef>],
    viewport_top: i32,
    viewport_bottom: i32,
) -> Vec<VisibleItem> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.index < 0 {
            continue;
        }
        let Some(item) = items.get(entry.index as usize).and_then(|it| it.as_ref()) else {
            continue;
        };
        let fraction = visible_fraction(entry.top, entry.height, viewport_top, viewport_bottom);
        if fraction > 0.0 {
            out.push(VisibleItem {
                key: item.key.clone(),
                platform: item.platform.clone(),
                visible_fraction: fraction,
            });
        }
    }
    out
}

/// **行式**网格里现在可见的卡片：`LazyColumn` 的一项里塞一整行卡片（视频详情的"相关推荐"），
/// 整行的可见比例当作行内每张卡片的可见比例（卡片横向完整可见）。`entries` 的 `index` 是**行号**。
pub fn related_visible_items(
    entries: &[VisibleEntry],
    rows: &[Vec<FeedRef>],
    viewport_top: i32,
    viewport_bottom: i32,
) -> Vec<VisibleItem> {
    let mut out = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        if entry.index < 0 {
            continue;
        }
        let Some(cards) = rows.get(entry.index as usize) else {
            continue;
        };
        let fraction = visible_fraction(entry.top, entry.height, viewport_top, viewport_bottom);
        if fraction > 0.0 {
            for card in cards {
                out.push(VisibleItem {
                    key: card.key.clone(),
                    platform: card.platform.clone(),
                    visible_fraction: fraction,
                });
            }
        }
    }
    out
}

/// 行为数据的接线结果：就是 [`ExposureTracker`] 要的两个入参（构造时的 `enabled` 和 `update` 的 `foreground`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposureInputs {
    pub enabled: bool,
    pub foreground: bool,
}

/// 把界面那三个原始输入折成 [`ExposureInputs`]——**B2 的接线就这一处**。
///
/// - `record_behavior` 是设置里的开关 → **原样**进 `enabled`：被盖住 / 失焦都不该改它。
/// - `covered`（全屏详情盖住了这个页签）/ `window_focused`（窗口有没有焦点）→ 都进 `foreground` 的 false，
///   也就是**结算**，不是**丢弃**。丢弃的话"看了 3 秒 → 点开详情"这条曝光会丢，或者被记成一条晚于
///   `visited_at` 的新曝光、反转成"曝光未点"这个负信号。
pub fn exposure_inputs(record_behavior: bool, covered: bool, window_focused: bool) -> ExposureInputs {
    ExposureInputs {
        enabled: record_behavior,
        foreground: !covered && window_focused,
    }
}

/// 正在跟踪的一次可见：[started_at_ms] 是开始可见的时刻，[dwell_ms] 一直累加。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tracked {
    key: String,
    platform: String,
    started_at_ms: i64,
    last_at_ms: i64,
    dwell_ms: i64,
}

/// 曝光的判定和停留计时。
///
/// 宿主每隔 [`EXPOSURE_TICK_MS`] 调一次 [`update`](Self::update)（界面侧的循环在 Kotlin：那是定时器 + 协程），
/// 离开组合时调 [`settle_all`](Self::settle_all)。返回的 [`Settle`] 列表按**开始跟踪的先后**排
/// （对应 Kotlin 的 `LinkedHashMap` 迭代顺序，写库顺序也就一致）。
///
/// 只在调用方的线程上改，所以不加锁（和 Kotlin 原版"只在 `scope`（主线程）里改"一样）。
#[derive(Debug, Default)]
pub struct ExposureTracker {
    /// 保持插入顺序：结算的写库顺序和 Kotlin 一致。
    tracked: Vec<Tracked>,
}

impl ExposureTracker {
    pub fn new() -> Self {
        Self { tracked: Vec::new() }
    }

    /// 现在跟踪着几条（测试 / 调试用）。
    pub fn tracked_count(&self) -> usize {
        self.tracked.len()
    }

    /// 界面每隔一段时间喂一次"现在可见的卡片"。
    ///
    /// - `enabled = false`：不再跟踪，已经在跟踪的**直接丢掉、不写库**（"用户不想记了"）；
    /// - `foreground = false`：正在跟踪的按当前时刻**结算**，之后重新开始计。
    pub fn update(
        &mut self,
        visible: &[VisibleItem],
        enabled: bool,
        foreground: bool,
        now_ms: i64,
    ) -> Vec<Settle> {
        if !enabled {
            self.tracked.clear();
            return Vec::new();
        }
        // 这一刻真正"在看着"的：前台 + 可见比例够门槛（同 key 只算一次，位置取第一次出现）
        let mut active: Vec<(&str, &str)> = Vec::new();
        if foreground {
            for item in visible {
                if item.visible_fraction < EXPOSURE_MIN_VISIBLE_FRACTION {
                    continue;
                }
                if active.iter().any(|(key, _)| *key == item.key) {
                    continue;
                }
                active.push((item.key.as_str(), item.platform.as_str()));
            }
        }

        let mut settled = Vec::new();
        // 1) 还在跟踪的：把上次到现在的这段时间累加进停留
        let mut leaving: Vec<usize> = Vec::new();
        for i in 0..self.tracked.len() {
            let elapsed = now_ms - self.tracked[i].last_at_ms;
            self.tracked[i].dwell_ms += if elapsed < 0 { 0 } else { elapsed };
            self.tracked[i].last_at_ms = now_ms;
            if !active.iter().any(|(key, _)| *key == self.tracked[i].key) {
                leaving.push(i);
            }
        }
        // 2) 不再可见（或切到后台 / 被盖住）的：够 1 秒就写库，然后停止跟踪
        //    （倒着删，下标失效也不影响前面的）
        for &i in leaving.iter().rev() {
            let entry = self.tracked.remove(i);
            if now_ms - entry.started_at_ms >= EXPOSURE_MIN_VISIBLE_MS {
                settled.push(Settle {
                    key: entry.key,
                    platform: entry.platform,
                    started_at_ms: entry.started_at_ms,
                    dwell_ms: entry.dwell_ms,
                    last_seen_at_ms: now_ms,
                });
            }
        }
        // 3) 新看到的开始跟踪；已经在跟踪的接着算（1 秒门槛不在这一步判，结算时按 started_at 看）
        for (key, platform) in active {
            if self.tracked.iter().any(|t| t.key == key) {
                continue;
            }
            self.tracked.push(Tracked {
                key: key.to_string(),
                platform: platform.to_string(),
                started_at_ms: now_ms,
                last_at_ms: now_ms,
                dwell_ms: 0,
            });
        }
        // leaving 是按"开始跟踪的先后"删的，倒着删之后 settled 的顺序要反过来，才对得上 Kotlin 的迭代顺序
        settled.reverse();
        settled
    }

    /// 界面离开组合（切页签、关窗口）时调：把还在跟踪的按当前时刻结算，别把这最后一次停留丢了。
    pub fn settle_all(&mut self, now_ms: i64) -> Vec<Settle> {
        if self.tracked.is_empty() {
            return Vec::new();
        }
        let mut settled = Vec::with_capacity(self.tracked.len());
        for entry in self.tracked.drain(..) {
            let elapsed = now_ms - entry.last_at_ms;
            let dwell_ms = entry.dwell_ms + if elapsed < 0 { 0 } else { elapsed };
            if now_ms - entry.started_at_ms >= EXPOSURE_MIN_VISIBLE_MS {
                settled.push(Settle {
                    key: entry.key,
                    platform: entry.platform,
                    started_at_ms: entry.started_at_ms,
                    dwell_ms,
                    last_seen_at_ms: now_ms,
                });
            }
        }
        settled
    }
}
