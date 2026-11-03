//! 贴吧**关注流**的调度 + 存盘（Kotlin `ForumStore` / `ForumMerge` / `ForumFreshness` 的 Rust 版）。
//!
//! 现在搬进来之后，Kotlin 只剩三件：把 `ctx.store` 交给 [`ForumStore`]、把"抓某个吧的一页"
//! 做成 [`ForumPager`] 的实现、把 `FeedItem` 的模型搬运交给 `RustModel`。
//!
//! ## 惰性 k 路归并（为什么要这么复杂）
//!
//! 关注的吧有几十个，每个吧的帖子列表是"从最新开始的一段连续前缀"。缓冲里最旧那条的键叫**前沿**
//! （frontier），前沿之下的帖子是未知的——除非这个吧已经 [`ForumState::exhausted`]。
//!
//! 要从全局取"下一条"（比边界 `before` 更旧的最新一条）：先看各吧缓冲里低于边界的最新帖子，取其中
//! 最新的 `best`；然后看有没有哪个吧"未知区域可能比 best 更新"（没耗尽、前沿比 best 还新）。
//! 有的话先抓那个吧的下一页，再重新比；没有才能放心取 `best`。所以冷清的吧（缓冲里的帖子跨好几个月）
//! 几乎永远不会触发额外请求，繁忙的吧才会被翻页。
//!
//! ## 存盘格式（**一个字节都不能变**）
//!
//! - 吧列表（几 KB）：明文 JSON，`{"v":2,"at":<毫秒>,"forums":[…]}`，最多一天刷新一次；
//! - 各吧缓冲：`{"v":1,"states":[…]}` 的 JSON 再 `gzip` + `base64`，前面加 `gz1:`，整体一个键。
//!
//! 两条都照 Kotlin（`kotlinx.serialization` 的 `StoredForums` / `StoredBuffers`，`encodeDefaults = true`
//! `explicitNulls = false`），所以 `feeditem_json`（小红书关注流那份）在这里**直接复用**：
//! `FeedItem` 的写法只有一份，别再造第二个。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::host::StoreHost;
use crate::model::{BiliError, FeedItem};
use crate::xhs::feeditem_json;
use crate::xhs::jsonord::{self, J};

// ===========================================================================
// 常量（和 Kotlin `ForumStore` / `ForumMerger` / `ForumFreshness` 一样）
// ===========================================================================

/// `ctx.store` 里吧列表的键。
pub const KEY_FORUMS: &str = "tieba.following.forums";
/// `ctx.store` 里各吧缓冲的键。
pub const KEY_BUFFERS: &str = "tieba.following.buffers";
/// 缓冲的格式版本。
pub const VERSION: i64 = 1;
/// 吧列表的格式版本。
pub const FORUMS_VERSION: i64 = 2;
/// 每个吧存盘时只留最新这么多条。
pub const KEEP_PER_FORUM: usize = 20;
/// gzip 前缀（和 Kotlin 一样）。
const GZIP_PREFIX: &str = "gz1:";

/// 连续这么多个吧读不了就认为不是某个吧的问题，把原始错误抛出去。
pub const MAX_FAILURES: usize = 3;
/// 吧读不了之后关多久（24 小时）。
pub const CLOSED_FOR_MS: i64 = 24 * 3_600_000;

/// 一天有多少毫秒（活跃度估算用）。
pub const DAY_MS: f64 = 24.0 * 3_600_000.0;
/// 再冷清的吧也不会超过这么久没看过（6 小时）。
pub const MAX_STALENESS_MS: i64 = 6 * 3_600_000;
/// 连着下拉刷新时不会反复打同一个吧（20 秒）。
pub const MIN_GAP_MS: i64 = 20_000;

// ===========================================================================
// ThreadKey：帖子的排序键
// ===========================================================================

/// 帖子的排序键：先比发帖时间（秒），同一秒的比 tid（tid 随发帖递增）。**越大越新**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadKey {
    pub time: i64,
    pub tid: i64,
}

impl ThreadKey {
    /// 没有更新帖子时的上界（Kotlin `ThreadKey.MAX`）。
    pub const MAX: ThreadKey = ThreadKey {
        time: i64::MAX,
        tid: i64::MAX,
    };

    pub fn new(time: i64, tid: i64) -> Self {
        Self { time, tid }
    }

    /// 游标里的写法：`<秒>:<tid>`。
    pub fn encode(&self) -> String {
        format!("{}:{}", self.time, self.tid)
    }

    /// [`Self::encode`] 的逆运算。`None` = 游标无效。
    ///
    /// 口径照 Kotlin `substringBefore(':')` + `substringAfter(':')`：**第一个** `:` 处切
    /// （`tid` 里本来就没有 `:`，这里照抄）。
    pub fn decode(text: &str) -> Option<ThreadKey> {
        let (time_text, tid_text) = match text.find(':') {
            Some(i) => (&text[..i], &text[i + 1..]),
            None => (text, ""),
        };
        Some(ThreadKey {
            time: time_text.parse::<i64>().ok()?,
            tid: tid_text.parse::<i64>().ok()?,
        })
    }
}

/// 缓冲里的帖子一定有发布时间（入缓冲前过滤过）。
///
/// Kotlin 是 `publishedAt ?: error(...)`；这里给 `i64::MIN`（排到最老）而不是崩——能走到这一步说明
/// 上游解析有 bug，但存盘 / 翻页不该因此炸掉。
pub fn thread_key(item: &FeedItem) -> ThreadKey {
    ThreadKey::new(item.published_at.unwrap_or(i64::MIN), item.id.parse::<i64>().unwrap_or(0))
}

// ===========================================================================
// FollowedForum / ForumList / ForumState
// ===========================================================================

/// 关注的一个吧（`forumGuide` 的 `like_forum[]`，也是吧列表缓存里的一项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedForum {
    pub id: i64,
    pub name: String,
    pub day_threads: i32,
    /// 头像。
    pub avatar: Option<String>,
    /// 关注人数。
    pub member_count: Option<i64>,
    /// 主题帖数。
    pub thread_count: Option<i64>,
}

impl FollowedForum {
    /// 从平台层那份（`crate::tieba::FollowedForum`）转过来。
    pub fn from_parsed(f: &crate::tieba::FollowedForum) -> Self {
        Self {
            id: f.id,
            name: f.name.clone(),
            day_threads: f.day_threads,
            avatar: f.avatar.clone(),
            member_count: f.member_count,
            thread_count: f.thread_count,
        }
    }
}

/// 吧列表和它的抓取时间（毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForumList {
    pub at: i64,
    pub forums: Vec<FollowedForum>,
}

/// 一个吧的缓冲和调度信息。不可变，改动用 `copy`。
///
/// - `items`：已抓到的普通主题帖，新的在前，按 [`ThreadKey`] 严格降序、无重复；
/// - `tail_pos`：服务端列表里"缓冲末尾"对应的位置数（1 起算，包含置顶帖），下一页就是 `tail_pos / rn + 1`。
///   不用 `items.len()` 推算，因为置顶帖、直播推广卡片也占位置，而它们不在 `items` 里；
/// - `exhausted`：缓冲末尾已经是这个吧的最后一条；
/// - `head_fetched_at`：上次抓第一页的时间（毫秒），0 = 从没抓过；
/// - `per_day`：上次抓第一页时从帖子时间估出来的"每天新帖数"；
/// - `closed_until`：吧不可用（例如 340001"神秘升级中"）时，到这个时间之前不再请求（毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct ForumState {
    pub id: i64,
    pub name: String,
    pub day_threads: i32,
    pub items: Vec<FeedItem>,
    pub tail_pos: i32,
    pub exhausted: bool,
    pub head_fetched_at: i64,
    pub per_day: Option<f64>,
    pub closed_until: i64,
}

impl ForumState {
    /// 一个空状态（第一次读不到缓存 / 新关注的吧）。
    pub fn new(id: i64, name: impl Into<String>, day_threads: i32) -> Self {
        Self {
            id,
            name: name.into(),
            day_threads,
            items: Vec::new(),
            tail_pos: 0,
            exhausted: false,
            head_fetched_at: 0,
            per_day: None,
            closed_until: 0,
        }
    }

    pub fn is_closed(&self, now: i64) -> bool {
        self.closed_until > now
    }

    /// 未知区域的上界：缓冲之下的帖子键都严格小于它。没抓过 = 无穷大；已耗尽或暂时关闭 = 没有（`None`）。
    pub fn frontier(&self, now: i64) -> Option<ThreadKey> {
        if self.exhausted || self.is_closed(now) {
            return None;
        }
        match self.items.last() {
            Some(last) => Some(thread_key(last)),
            None => Some(ThreadKey::MAX),
        }
    }

    /// 缓冲里严格比 `bound` 旧的最新一条；`bound` 为 `None` 表示不限。
    pub fn newest_below(&self, bound: Option<ThreadKey>) -> Option<&FeedItem> {
        match bound {
            None => self.items.first(),
            Some(b) => self.items.iter().find(|item| thread_key(item) < b),
        }
    }
}

// ===========================================================================
// ForumFreshness：哪些吧该重抓第一页
// ===========================================================================

/// 关注的吧有几十个，活跃度差很多。每次刷新都把所有吧的第一页抓一遍（约 50 个请求）不划算，
/// 所以只抓"很可能有新帖"的吧：预计新帖数 = 每天新帖数 × 距上次抓取的天数 >= 1。
///
/// "每天新帖数"取两个估计里大的那个：服务端的 `day_thread_num`（今日新帖数，凌晨偏低）和上次抓第一页
/// 时从帖子时间算出来的 [`observed_per_day`]（不受零点影响）。估计偏高只会多抓几个吧，偏低由
/// [`MAX_STALENESS_MS`] 兜底。
pub fn rate_per_day(f: &ForumState) -> f64 {
    (f.day_threads as f64).max(f.per_day.unwrap_or(0.0))
}

/// 这个吧现在该不该重抓第一页。
pub fn is_due(f: &ForumState, now: i64) -> bool {
    if f.is_closed(now) {
        return false;
    }
    if f.head_fetched_at <= 0 || (f.items.is_empty() && !f.exhausted) {
        return true;
    }
    let elapsed = now - f.head_fetched_at;
    if elapsed < MIN_GAP_MS {
        return false;
    }
    if elapsed >= MAX_STALENESS_MS {
        return true;
    }
    rate_per_day(f) * elapsed as f64 / DAY_MS >= 1.0
}

/// 该重抓第一页的吧，**活跃的在前**（先抓活跃的，万一中途被风控拦下也已经拿到了最要紧的）。
pub fn due(states: &[ForumState], now: i64) -> Vec<i64> {
    let mut list: Vec<&ForumState> = states.iter().filter(|f| is_due(f, now)).collect();
    // `sortedByDescending { ratePerDay(it) }`：Rust 的 `sort_by` 是稳定的，和 Kotlin 一样
    // （活跃度相同的吧保持原来的顺序）。
    list.sort_by(|a, b| {
        rate_per_day(b)
            .partial_cmp(&rate_per_day(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    list.into_iter().map(|f| f.id).collect()
}

/// 从一页帖子（新的在前）估每天新帖数：(条数 - 1) / 最新到最旧的时间跨度。
/// 少于 5 条或者跨度为 0（同一秒发的）时估不出来，返回 `None`。
pub fn observed_per_day(newest_first: &[FeedItem]) -> Option<f64> {
    if newest_first.len() < 5 {
        return None;
    }
    let span = newest_first.first()?.published_at? - newest_first.last()?.published_at?;
    if span <= 0 {
        return None;
    }
    Some((newest_first.len() - 1) as f64 * 86_400.0 / span as f64)
}

// ===========================================================================
// ForumMerge：并入一页
// ===========================================================================

/// 抓回来的一页：只含普通主题帖（置顶帖、推广卡片已丢），新的在前。`last` = 这是最后一页。
#[derive(Debug, Clone, PartialEq)]
pub struct ForumPage {
    pub items: Vec<FeedItem>,
    pub last: bool,
}

/// 并入一个吧新抓的第一页（`page` 是整个第一页）。
pub fn integrate_head(f: &ForumState, page: &ForumPage, now: i64, page_size: i32) -> ForumState {
    let fresh = &page.items;
    let per_day = observed_per_day(fresh).or(f.per_day);
    if fresh.is_empty() {
        // 第一页一条普通帖子都没有：没东西可看，也别再往下翻
        return ForumState {
            items: Vec::new(),
            tail_pos: 0,
            exhausted: true,
            head_fetched_at: now,
            per_day,
            ..f.clone()
        };
    }
    let old = &f.items;
    let fresh_oldest = thread_key(fresh.last().expect("非空"));
    let old_ids: HashSet<&str> = old.iter().map(|i| i.id.as_str()).collect();
    // 新抓的页和旧缓冲接得上：有相同的帖子，或者新页一直翻到了旧缓冲最新那条之下
    let connected = !old.is_empty()
        && (fresh.iter().any(|i| old_ids.contains(i.id.as_str()))
            || fresh_oldest <= thread_key(old.first().expect("非空")));
    if !connected {
        // 新帖子多到一页装不下，中间的空档不知道是什么：丢掉旧缓冲，只留这一页
        return ForumState {
            items: fresh.clone(),
            tail_pos: if page.last { 0 } else { page_size },
            exhausted: page.last,
            head_fetched_at: now,
            per_day,
            ..f.clone()
        };
    }
    // 新页覆盖的时间范围（>= fresh_oldest）里，旧缓冲的帖子以新页为准：还在的被替换，不在的是被删了
    let below: Vec<FeedItem> = old
        .iter()
        .filter(|i| thread_key(i) < fresh_oldest)
        .cloned()
        .collect();
    let fresh_ids: HashSet<&str> = fresh.iter().map(|i| i.id.as_str()).collect();
    let purged = old
        .iter()
        .filter(|i| thread_key(i) >= fresh_oldest && !fresh_ids.contains(i.id.as_str()))
        .count();
    let newest_old = thread_key(old.first().expect("非空"));
    let tail_pos = if page.last {
        0
    } else if below.is_empty() {
        // 新页一直翻到了旧缓冲末尾之下：合并后的末尾就是新页的末尾
        page_size
    } else {
        // 否则旧缓冲末尾被新增的帖子往下顶、被删掉的往上提
        let added = fresh
            .iter()
            .filter(|i| !old_ids.contains(i.id.as_str()) && thread_key(i) > newest_old)
            .count();
        i32::max(f.tail_pos + added as i32 - purged as i32, page_size)
    };
    let mut items = fresh.clone();
    items.extend(below);
    ForumState {
        items,
        tail_pos,
        exhausted: f.exhausted || page.last,
        head_fetched_at: now,
        per_day,
        ..f.clone()
    }
}

/// 并入后面的一页（第 `page_no` 页，>= 2）。和缓冲重叠的部分按帖子去重。
pub fn integrate_tail(f: &ForumState, page_no: i32, page: &ForumPage, page_size: i32) -> ForumState {
    let have: HashSet<&str> = f.items.iter().map(|i| i.id.as_str()).collect();
    let frontier = f.items.last().map(thread_key);
    // 只收比前沿更旧的：整页都落在缓冲范围内（前面有大量新帖把列表顶下去了）时没有新东西，
    // 但 tail_pos 照样前进
    let older: Vec<FeedItem> = page
        .items
        .iter()
        .filter(|i| {
            !have.contains(i.id.as_str()) && frontier.map(|k| thread_key(i) < k).unwrap_or(true)
        })
        .cloned()
        .collect();
    let mut items = f.items.clone();
    items.extend(older);
    ForumState {
        items,
        tail_pos: if page.last {
            f.tail_pos
        } else {
            i32::max(f.tail_pos, page_no * page_size)
        },
        exhausted: page.last,
        ..f.clone()
    }
}

/// 存盘前压缩：每个吧只留最新的 `keep` 条。被丢掉的那部分变回"未知区域"，`tail_pos` 往回退相应的位置数，
/// 下次需要时从对应的页重新抓。
pub fn trimmed(f: &ForumState, keep: usize) -> ForumState {
    if f.items.len() <= keep {
        return f.clone();
    }
    let dropped = (f.items.len() - keep) as i32;
    ForumState {
        items: f.items[..keep].to_vec(),
        tail_pos: i32::max(f.tail_pos - dropped, 0),
        exhausted: false,
        ..f.clone()
    }
}

// ===========================================================================
// ForumPager：宿主实现"抓某个吧的一页"
// ===========================================================================

/// 这个吧现在读不了（例如 340001"神秘升级中"），不是网络问题或风控。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForumClosed {
    /// 给用户看的说明（不含 Cookie / 密钥）。
    pub message: String,
}

/// 抓一页帖子的宿主端口（Kotlin `FrsPager`）。网络 / 风控错误原样往外传，吧本身读不了返回
/// [`Err`] 里的 [`ForumClosed`] 变体。
pub trait ForumPager: Send + Sync {
    /// 服务端每页的位置数（`rn`）。
    fn page_size(&self) -> i32;

    /// 取 `forum` 的第 `page` 页（1 起）。
    fn fetch(&self, forum: &ForumState, page: i32) -> Result<ForumPage, PagerError>;
}

/// 抓一页可能的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagerError {
    /// 这个吧读不了（`ForumClosedException`）：标记它暂时关闭、继续下一个。
    Closed(ForumClosed),
    /// 别的错误（网络 / 风控 / 没登录 / 签名 / 平台错误）：**连类型**原样往外抛，**不吞**
    /// （宿主靠 `RiskControl` / `NotLoggedIn` 决定停不停、提示什么，不能压成字符串）。
    Fatal(BiliError),
}

// ===========================================================================
// ForumMerger：惰性 k 路归并
// ===========================================================================

/// 合并出的一页。`more` 为 true 表示后面至少还有一条。
#[derive(Debug, Clone, PartialEq)]
pub struct MergePage {
    pub items: Vec<FeedItem>,
    pub more: bool,
}

/// 惰性 k 路归并（见模块说明）。状态「各吧缓冲」由调用方持有（Kotlin 是 `followingStates`），
/// 这里只做"怎么选、什么时候去抓"。
pub struct ForumMerger<'a> {
    pager: &'a dyn ForumPager,
    /// 连续这么多个吧读不了就认为不是某个吧的问题。
    failures: usize,
    /// 从上次 [`Self::reset_stats`] 以来真正发出去的抓取请求数（调用方据此决定要不要存盘）。
    pub fetches: usize,
}

impl<'a> ForumMerger<'a> {
    pub fn new(pager: &'a dyn ForumPager) -> Self {
        Self {
            pager,
            failures: 0,
            fetches: 0,
        }
    }

    pub fn reset_stats(&mut self) {
        self.fetches = 0;
    }

    /// 让 `ids` 里这些吧的第一页变新（调度器认为该刷新的）。
    ///
    /// 单个吧读不了时标记它暂时关闭、继续下一个；连续 [`MAX_FAILURES`] 个读不了就把原始错误抛出。
    /// `deadline` 到点就不再发新请求（剩下的吧下次刷新还在 due 里）。
    pub fn refresh_heads(
        &mut self,
        states: &mut HashMap<i64, ForumState>,
        ids: &[i64],
        now: impl Fn() -> i64,
        deadline: i64,
    ) -> Result<(), BiliError> {
        self.failures = 0;
        for id in ids {
            // 预算到点就不再发新请求；剩下的吧下次刷新还在 due 里（排序把活跃的放前面）
            if now() >= deadline {
                break;
            }
            self.fetch_into(states, *id, 1, &now)?;
        }
        Ok(())
    }

    /// 取比 `before` 更旧的 `limit` 条（`before` 为 `None` = 从最新开始）。按需抓取各吧的后续页。
    ///
    /// 状态更新写回 `states`；中途出错时已经完成的抓取也留在 `states` 里，调用方照常存盘即可。
    pub fn next_page(
        &mut self,
        states: &mut HashMap<i64, ForumState>,
        before: Option<ThreadKey>,
        limit: usize,
        now: impl Fn() -> i64,
    ) -> Result<MergePage, BiliError> {
        self.failures = 0;
        let mut out: Vec<FeedItem> = Vec::with_capacity(limit);
        let mut bound = before;
        while out.len() < limit {
            let Some(item) = self.select_next(states, bound, &now)? else {
                return Ok(MergePage {
                    items: out,
                    more: false,
                });
            };
            bound = Some(thread_key(&item));
            out.push(item);
        }
        let more = self.select_next(states, bound, &now)?.is_some();
        Ok(MergePage { items: out, more })
    }

    /// 比 `bound` 旧的全局最新一条；必要时先抓取，保证不会有更新的帖子藏在某个吧的未知区域里。
    fn select_next(
        &mut self,
        states: &mut HashMap<i64, ForumState>,
        bound: Option<ThreadKey>,
        now: &impl Fn() -> i64,
    ) -> Result<Option<FeedItem>, BiliError> {
        loop {
            let t = now();
            let mut ids: Vec<i64> = states.keys().copied().collect();
            ids.sort_unstable();
            let mut best: Option<(ThreadKey, FeedItem)> = None;
            for id in &ids {
                let f = states.get(id).expect("刚拿到的 key");
                if let Some(candidate) = f.newest_below(bound) {
                    let key = thread_key(candidate);
                    if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                        best = Some((key, candidate.clone()));
                    }
                }
            }
            let best_key = best.as_ref().map(|(k, _)| *k);
            // 未知区域可能比 best 新的吧：前沿 > best（best 不存在时，所有没耗尽的吧都算）。前沿最高的先抓。
            let mut blocker: Option<(i64, ThreadKey)> = None;
            for id in &ids {
                let f = states.get(id).expect("刚拿到的 key");
                let Some(frontier) = f.frontier(t) else { continue };
                if let Some(best_key) = best_key {
                    if frontier <= best_key {
                        continue;
                    }
                }
                if blocker.map(|(_, k)| frontier > k).unwrap_or(true) {
                    blocker = Some((*id, frontier));
                }
            }
            let Some((id, _)) = blocker else {
                return Ok(best.map(|(_, item)| item));
            };
            let f = states.get(&id).expect("刚拿到的 key");
            let page = if f.items.is_empty() {
                1
            } else {
                f.tail_pos / self.pager.page_size() + 1
            };
            self.fetch_into(states, id, page, now)?;
        }
    }

    fn fetch_into(
        &mut self,
        states: &mut HashMap<i64, ForumState>,
        id: i64,
        page: i32,
        now: &impl Fn() -> i64,
    ) -> Result<(), BiliError> {
        let Some(f) = states.get(&id).cloned() else {
            return Ok(());
        };
        self.fetches += 1;
        let page_size = self.pager.page_size();
        let result = self.pager.fetch(&f, page);
        let now_ms = now();
        match result {
            Err(PagerError::Closed(e)) => {
                self.failures += 1;
                if self.failures >= MAX_FAILURES {
                    // 连续这么多个吧读不了：不是某个吧的问题，按平台错误抛（和改之前给宿主的一样）
                    return Err(BiliError::Api {
                        code: "-".to_string(),
                        detail: e.message,
                    });
                }
                let mut next = f.clone();
                next.closed_until = now_ms + CLOSED_FOR_MS;
                states.insert(id, next);
                Ok(())
            }
            Err(PagerError::Fatal(e)) => Err(e),
            Ok(page_result) => {
                self.failures = 0;
                let next = if page == 1 {
                    integrate_head(&f, &page_result, now_ms, page_size)
                } else {
                    integrate_tail(&f, page, &page_result, page_size)
                };
                states.insert(id, next);
                Ok(())
            }
        }
    }
}

// ===========================================================================
// ForumStore：存盘
// ===========================================================================

/// 关注流的持久化（键都以 `tieba.following.` 开头），照 Kotlin `ForumStore`。
pub struct ForumStore {
    store: Arc<dyn StoreHost>,
}

impl ForumStore {
    pub fn new(store: Arc<dyn StoreHost>) -> Self {
        Self { store }
    }

    // -----------------------------------------------------------------------
    // 吧列表（明文 JSON）
    // -----------------------------------------------------------------------

    /// `ForumStore.loadForums`：版本不对 / 读不出来 → `None`。
    pub fn load_forums(&self) -> Option<ForumList> {
        let text = self.store.get(KEY_FORUMS)?;
        let root = jsonord::parse(&text).ok()?;
        if root.get("v")?.as_i64()? != FORUMS_VERSION {
            return None;
        }
        let at = root.get("at")?.as_i64()?;
        let forums = root.get("forums")?.as_arr()?;
        let list: Vec<FollowedForum> = forums.iter().filter_map(decode_forum).collect();
        Some(ForumList { at, forums: list })
    }

    /// `ForumStore.saveForums`：`{"v":2,"at":<毫秒>,"forums":[…]}`（明文，字段顺序照 Kotlin）。
    pub fn save_forums(&self, list: &ForumList) {
        self.store
            .put(KEY_FORUMS, Some(&encode_forums(list)));
    }

    /// `ForumStore.invalidateForums`：下次关注流刷新时重新取。各吧的缓冲不动。
    pub fn invalidate_forums(&self) {
        self.store.put(KEY_FORUMS, None);
    }

    // -----------------------------------------------------------------------
    // 各吧缓冲（gz1: + base64(gzip(JSON))）
    // -----------------------------------------------------------------------

    /// `ForumStore.loadBuffers`：**读不出来就当全空**（版本不对也一样）。
    pub fn load_buffers(&self) -> HashMap<i64, ForumState> {
        self.try_load_buffers().unwrap_or_default()
    }

    fn try_load_buffers(&self) -> Option<HashMap<i64, ForumState>> {
        let raw = self.store.get(KEY_BUFFERS)?;
        let text = match raw.strip_prefix(GZIP_PREFIX) {
            Some(b64) => {
                let bytes = crate::coolapk::token::base64_decode(b64.as_bytes()).ok()?;
                let unzipped = crate::xhs::deflate::gunzip(&bytes).ok()?;
                String::from_utf8(unzipped).ok()?
            }
            None => raw,
        };
        parse_buffers_json(&text)
    }

    /// `ForumStore.saveBuffers`：每个吧先压到 [`KEEP_PER_FORUM`] 条，再 gzip + base64。
    pub fn save_buffers(&self, states: &[ForumState]) {
        let trimmed: Vec<ForumState> = states.iter().map(|f| trimmed(f, KEEP_PER_FORUM)).collect();
        let text = encode_buffers(&trimmed);
        match crate::xhs::deflate::gzip(text.as_bytes(), 0, 6) {
            Ok(gz) => self.store.put(
                KEY_BUFFERS,
                Some(&format!(
                    "{GZIP_PREFIX}{}",
                    crate::coolapk::token::base64(&gz, true)
                )),
            ),
            Err(_) => {
                // gzip 失败（理论上不会）：退回明文，读的时候 `starts_with("gz1:")` 为假就当明文解析
                self.store.put(KEY_BUFFERS, Some(&text));
            }
        }
    }
}

// ===========================================================================
// 编解码（逐字节照 Kotlin：字段顺序、encodeDefaults、explicitNulls=false）
// ===========================================================================

/// 吧列表 → JSON（`{"v":2,"at":…,"forums":[…],"avatars":…}` 那种形状**不带**额外字段）。
///
/// Kotlin 的 `StoredForums(v, at, forums)` + `FollowedForum(id, name, dayThreads=0, avatar=null,
/// memberCount=null, threadCount=null)`，`encodeDefaults = true` 所以 `dayThreads` **总是写**，
/// `avatar` / `memberCount` / `threadCount` 是 null 就**不写**。
fn encode_forums(list: &ForumList) -> String {
    let mut out = String::with_capacity(64 + list.forums.len() * 128);
    out.push_str("{\"v\":");
    out.push_str(&FORUMS_VERSION.to_string());
    out.push_str(",\"at\":");
    out.push_str(&list.at.to_string());
    out.push_str(",\"forums\":[");
    for (i, f) in list.forums.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        out.push_str(&f.id.to_string());
        out.push_str(",\"name\":");
        jsonord::quote(&mut out, &f.name);
        out.push_str(",\"dayThreads\":");
        out.push_str(&f.day_threads.to_string());
        if let Some(v) = &f.avatar {
            out.push_str(",\"avatar\":");
            jsonord::quote(&mut out, v);
        }
        if let Some(v) = f.member_count {
            out.push_str(",\"memberCount\":");
            out.push_str(&v.to_string());
        }
        if let Some(v) = f.thread_count {
            out.push_str(",\"threadCount\":");
            out.push_str(&v.to_string());
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// 吧列表 JSON 里的一项。`id` / `name` 缺一个就跳过这一项（Kotlin 那边会让整份解码失败，
/// 这里宽容一点：吧列表丢了会重新取一次，不影响缓冲）。
fn decode_forum(value: &J) -> Option<FollowedForum> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    Some(FollowedForum {
        id: field("id")?.as_i64()?,
        name: field("name")?.as_str()?.to_string(),
        day_threads: field("dayThreads").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        avatar: field("avatar").and_then(|v| v.as_str()).map(|s| s.to_string()),
        member_count: field("memberCount").and_then(|v| v.as_i64()),
        thread_count: field("threadCount").and_then(|v| v.as_i64()),
    })
}

/// 缓冲 → JSON（`{"v":1,"states":[…]}`，字段顺序照 Kotlin `ForumState` 的声明顺序）。
pub fn encode_buffers(states: &[ForumState]) -> String {
    let mut out = String::with_capacity(256 + states.len() * 2048);
    out.push_str("{\"v\":");
    out.push_str(&VERSION.to_string());
    out.push_str(",\"states\":[");
    for (i, f) in states.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        out.push_str(&f.id.to_string());
        out.push_str(",\"name\":");
        jsonord::quote(&mut out, &f.name);
        out.push_str(",\"dayThreads\":");
        out.push_str(&f.day_threads.to_string());
        out.push_str(",\"items\":[");
        for (j, item) in f.items.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&feeditem_json::encode(item));
        }
        out.push_str("],\"tailPos\":");
        out.push_str(&f.tail_pos.to_string());
        out.push_str(",\"exhausted\":");
        out.push_str(if f.exhausted { "true" } else { "false" });
        out.push_str(",\"headFetchedAt\":");
        out.push_str(&f.head_fetched_at.to_string());
        if let Some(v) = f.per_day {
            out.push_str(",\"perDay\":");
            out.push_str(&json_number(v));
        }
        out.push_str(",\"closedUntil\":");
        out.push_str(&f.closed_until.to_string());
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// 双精度浮点写成 JSON 数字，**口径照 kotlinx.serialization**（`Double.toString()` 那一套：
/// 整数也写 `.0`，正负零、无穷、NaN 有各自的写法，指数用 `E`）。
///
/// 这里只处理 `perDay`（每天新帖数，0～几千，正常是有限的正数），所以：
/// - 有限值走 Rust 的 `{:?}`（`f64` 的最短往返表示，和 Java 的 `Double.toString` 在正常范围内一致，
///   都是"最短能唯一还原的十进制"），整数补 `.0`；
/// - `NaN` / 无穷写 `"NaN"` / `"Infinity"` / `"-Infinity"`（kotlinx 会写 JSON 字符串，不写裸数字）；
/// - 负零写 `-0.0`。
fn json_number(v: f64) -> String {
    if v.is_nan() {
        return "\"NaN\"".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "\"Infinity\"".to_string()
        } else {
            "\"-Infinity\"".to_string()
        };
    }
    let text = format!("{v:?}");
    if text.contains('.') || text.contains('e') || text.contains('E') {
        text
    } else {
        format!("{text}.0")
    }
}

/// **解析一段缓冲的明文 JSON**（`{"v":1,"states":[…]}`）→ 各吧状态。
///
/// 读不出来（版本不对 / 坏 JSON / 缺 `states`）就是 `None`，调用方当成"缓存坏了"重新抓。
pub fn parse_buffers_json(text: &str) -> Option<HashMap<i64, ForumState>> {
    let root = jsonord::parse(text).ok()?;
    if root.get("v")?.as_i64()? != VERSION {
        return None;
    }
    let states = root.get("states")?.as_arr()?;
    let mut out = HashMap::with_capacity(states.len());
    for value in states {
        if let Some(state) = decode_state(value) {
            out.insert(state.id, upgraded(state));
        }
    }
    Some(out)
}

/// 一个 [`ForumState`] 的 JSON。
fn decode_state(value: &J) -> Option<ForumState> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let items = field("items")?
        .as_arr()?
        .iter()
        .filter_map(feeditem_json::decode)
        .collect();
    Some(ForumState {
        id: field("id")?.as_i64()?,
        name: field("name")?.as_str()?.to_string(),
        day_threads: field("dayThreads").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        items,
        tail_pos: field("tailPos").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        exhausted: field("exhausted").and_then(|v| v.as_bool()).unwrap_or(false),
        head_fetched_at: field("headFetchedAt").and_then(|v| v.as_i64()).unwrap_or(0),
        // `perDay: Double?`：数字字面量、数字字符串、kotlinx 的 `"NaN"` / `"Infinity"` 都认
        per_day: field("perDay").and_then(|v| v.as_f64()).filter(|v| !v.is_nan()),
        closed_until: field("closedUntil").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// 帖子的 `detail` 是 `DetailTarget.Web`（打开网页）；现在贴吧帖子都能原生打开
/// （`TiebaParser.thread` 直接给 `Native(tid)`），读出来时把旧的改过来，免得为此丢掉缓冲重抓几十个吧。
fn upgraded(state: ForumState) -> ForumState {
    fn native(item: &FeedItem) -> FeedItem {
        if matches!(item.detail, crate::model::DetailTarget::Native { .. }) {
            return item.clone();
        }
        FeedItem {
            detail: crate::model::DetailTarget::Native {
                id: item.id.clone(),
                platform: None,
            },
            repost: item.repost.as_ref().map(|r| Box::new(native(r))),
            ..item.clone()
        }
    }
    if state
        .items
        .iter()
        .all(|i| matches!(i.detail, crate::model::DetailTarget::Native { .. }))
    {
        state
    } else {
        ForumState {
            items: state.items.iter().map(native).collect(),
            ..state
        }
    }
}

