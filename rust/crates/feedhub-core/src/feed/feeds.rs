//! 信息流的**状态机本体**（`app/.../feed/FeedState.kt` 的非 Compose 部分）。
//!
//! 搬过来的是**全部记账和决策**：每个平台的游标、去重集合、批 id、平台序号、预取（含它自己那份游标）、
//! 晚到的内容 / 超时的平台、刷新最短动画、"一次只备一批"、看过一半才预取、出错 / 没登录的分流、
//! 冷启动摆上一批、翻页不吃预取……这些原来都是 `FeedState`（957 行）里的字段和分支，现在只有一份在这里。
//!
//! **留宿主（Kotlin）的**：
//! - Compose 状态容器：`items` / `refreshing` / `errors` … 还是 Kotlin 的 `mutableStateOf`，值从 [`Engine::view`] 抄；
//! - 协程触发：`scope.launch` / `delay` / `withTimeoutOrNull` / 取消（`Job.cancel`）在 Kotlin；
//! - 真正的收发：`Source.feed(kind, cursor)` 走 OkHttp，Rust 一个字节都不发；
//! - 资料库（`seen_item` / 曝光）/ 快照文件 / 平台对象（`Source`）/ 设置（现读）都在宿主；
//! - 界面上的文案（`describe(e)`：把异常翻成一行字）——它认的是宿主异常类型。
//!
//! 时间都由宿主注入（`now_ms`），Rust 不读时钟、不取随机数。
//!
//! ## 一轮怎么走（宿主那边的协程骨架，和原 Kotlin 的挂起点一一对应）
//!
//! ```text
//! out = engine.refresh(fresh, now)                     // 决定：用预取 / 现场请求 / 直接返回
//! if out.mode == UsePrefetched:
//!     delay(out.remain_ms)                             // 最短动画（此时还**没**去重）
//!     engine.commit_prefetched(now)
//! else if out.mode == Request:
//!     for (req in out.requests) launch { chain(req) }   // chain：请求 → on_page → 要就再取一页
//!     with_timeout_or_null(wait_ms) { join_all() }
//!     if 超时: engine.on_timeout()
//!     fin = engine.finish_round(cfg, now)               // 去重 → 封顶 → 排序（**这一步就吃掉 key**）
//!     delay(fin.remain_ms)
//!     engine.commit_round(fin.refresh_round, now)       // 上屏 + 存快照 + loadedOnce + refreshedTimes
//! ```
//!
//! 三个"不能搞反"的时序（照抄 Kotlin，`FeedStateTest` 守着）：
//! 1. **用预取的那一路不去重**：预取时已经按当时的顺序排好、key 也在那时候算过；去重放到
//!    `commit_prefetched` 那一步（最短动画之后），这样中途退到后台不会白丢一批；
//! 2. **现场请求那一路先去重再等动画**：`finish_round` 就吃掉 key，`commit_round` 才上屏；
//! 3. **超时之后到的内容**：刷新 / 翻页进 `late`（下次翻页一起排），预取直接丢掉；错误 / 慢平台只由
//!    "前台的"那一趟写，预取什么都不写。

use std::collections::{HashMap, HashSet};

use crate::brain::{self, filter::Rules, MixItem, OrderKind};

/// 首页 / 关注。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

impl FeedKind {
    /// 日志里的名字（用户看的是"首页 / 关注"，不是 enum 的"推荐 / 关注"）。
    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Recommend => "首页",
            FeedKind::Following => "关注",
        }
    }
}

/// 内容类型（设置里的「信息流 → 内容类型」）。直播两个都算"不是"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Mixed,
    Video,
    Text,
}

/// 首页的批内顺序（设置里的「首页排序」）。关注页忽略它（永远按时间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedOrder {
    /// 各平台穿插（默认）。
    Interleave,
    /// 整批按发布时间倒序（老行为）。
    Time,
}

/// 刷新动画至少显示多久。
/// 400 ms 是用户给的范围里的中间值。**以后要调就改这一个数。**
pub const REFRESH_MIN_ANIMATION_MS: i64 = 400;

/// 一页全被过滤掉时最多连续补几页。
pub const MAX_BLANK_PAGES: u32 = 3;

/// 上一批最多存几条（`FeedSnapshot.MAX_ITEMS`；真正落盘的是宿主）。
pub const SNAPSHOT_MAX_ITEMS: usize = 120;

/// 引擎的固定参数（和 `FeedState` 构造函数的默认值一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// 一批最多等多久（毫秒）。
    pub wait_ms: i64,
    /// 回到前台时，离上次取到新内容超过这么久就只预取、不自动替换屏幕上的内容。
    pub resume_stale_ms: i64,
    /// 一页全空时最多连续补几页。
    pub max_blank_pages: u32,
    /// 刷新最短动画。
    pub refresh_min_animation_ms: i64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            wait_ms: 5_000,
            resume_stale_ms: 5 * 60_000,
            max_blank_pages: MAX_BLANK_PAGES,
            refresh_min_animation_ms: REFRESH_MIN_ANIMATION_MS,
        }
    }
}

impl EngineConfig {
    /// 超时那一行字（界面 / 错误表里的文案，和 Kotlin 一字不差）。
    pub fn timeout_message(&self) -> String {
        format!("超过 {} 秒没返回，这次没排进来", self.wait_ms / 1000)
    }
}

/// 每一批现读的规则。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchCfg {
    pub content_type: ContentType,
    /// 每个平台一批最多几条（0 = 不限）。
    pub max_per_platform: i32,
    pub order: FeedOrder,
    /// 内容过滤第一层（屏蔽词 / 外出模式）。
    pub rules: Rules,
}

impl Default for BatchCfg {
    fn default() -> Self {
        Self {
            content_type: ContentType::Mixed,
            max_per_platform: 0,
            order: FeedOrder::Interleave,
            rules: Rules::default(),
        }
    }
}

/// 屏幕上的一条（排序 / 去重 / 记序号只要这三个字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub platform: String,
    pub published_at: Option<i64>,
}

/// 宿主送来的一页里的条目：排序字段 + **过滤要看的字段**（18+ / 标题 / 摘要 / 社区 / 转发）+ 内容类型的两个布尔。
///
/// 为什么一页一次带全：18+ / 屏蔽词 / 内容类型都在 Rust 里过，一页只过一次 FFI；屏幕上的条目只留
/// [`Entry`]（三个字段），所以翻页 / 排序不为长文本付复制成本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRef {
    pub key: String,
    pub platform: String,
    pub published_at: Option<i64>,
    /// 平台标的 18+。
    pub adult: bool,
    /// `kind == VIDEO || media.firstOrNull() is Media.Video`（宿主按 `isVideoPost()` 算好）。
    pub is_video: bool,
    /// `kind == LIVE`（直播哪个内容类型都不算）。
    pub is_live: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub community: Option<String>,
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    pub repost_community: Option<String>,
}

impl FeedRef {
    fn entry(&self) -> Entry {
        Entry {
            key: self.key.clone(),
            platform: self.platform.clone(),
            published_at: self.published_at,
        }
    }

    fn text(&self) -> brain::MixText {
        brain::MixText {
            adult: self.adult,
            title: self.title.clone(),
            summary: self.summary.clone(),
            community: self.community.clone(),
            repost_title: self.repost_title.clone(),
            repost_summary: self.repost_summary.clone(),
            repost_community: self.repost_community.clone(),
        }
    }
}

/// 一条要宿主去发的请求（**一页**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRequest {
    /// 属于哪一轮（宿主回报结果时原样带回来，过期的轮次直接丢掉）。
    pub round: u64,
    pub platform: String,
    pub cursor: Option<String>,
    /// 后台预取（晚到的内容丢掉、超时 / 失败不写错误）。
    pub background: bool,
}

/// 一次平台请求的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageOutcome {
    Ok {
        items: Vec<FeedRef>,
        next: Option<String>,
    },
    /// 失败：`not_logged_in` 是宿主判出来的（`NotLoggedInException`），`describe` 是给用户看的一行字
    /// （宿主用 `describe(e)` 算好；Rust 不认异常类型）。
    Failed {
        not_logged_in: bool,
        describe: String,
    },
}

/// [`Engine::on_page`] 的答复：这一家还要不要再来一页。
#[derive(Debug, Clone, PartialEq)]
pub struct PageStep {
    /// `Some(cursor)` = 这家接着取下一页（整页都被过滤掉了、还没到 [`MAX_BLANK_PAGES`]）。
    pub next: Option<String>,
    /// 这一趟顺带产生的状态变化（上屏 / 新增 / 日志 / 账号过期）。
    pub action: Action,
}

/// 宿主拿到这次回答之后该做什么（**由引擎指定**，宿主不要自己猜）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Flow {
    /// 什么都不用做。
    #[default]
    None,
    /// 用备好的那批上屏：等 `remain_ms` 之后调 `commit_prefetched`。
    CommitPrefetched,
    /// 现场请求的刷新：发 `requests`，等齐 / 超时之后 `finish_round` → 等 `remain_ms` → `commit_round`。
    RefreshRound,
    /// 一轮（翻页 / 预取 / 重试）：发 `requests`（**可能是空的**，比如只剩晚到内容），
    /// 等齐 / 超时之后 `finish_round`（它自己会落地）。
    Round,
}

/// 当前刷新走哪条路。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RefreshMode {
    /// 没进当前（后台 / 最短动画还没到）。
    #[default]
    Skipped,
    /// 手上备好的那一批直接上屏（不再请求）。
    UsePrefetched,
    /// 现场请求。
    Request,
}

/// 引擎返回给宿主"这一趟要做什么"。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    /// 要发的请求（可能为空）。
    pub requests: Vec<FeedRequest>,
    /// 宿主接下来做什么（见 [`Flow`]）。
    pub flow: Flow,
    /// 当前怎么走（只有 `refresh` 用它）。
    pub mode: RefreshMode,
    /// 最短动画还要等多久（毫秒；0 = 不用等）。
    pub remain_ms: i64,
    /// 屏幕上**新出现**的条目 key（交给向量调度器 / 封面调度器）。
    pub new_content: Vec<String>,
    /// 屏幕列表变了（宿主重新取 [`Engine::screen_keys`]）。
    pub screen_changed: bool,
    /// 备好的那批变了（宿主重新取 [`Engine::prefetched_keys`]）。
    pub prefetched_changed: bool,
    /// 可以把最上面 [`SNAPSHOT_MAX_ITEMS`] 条存成"上一批"。
    pub save_snapshot: bool,
    /// `loadMore` / `retryPlatform` 真的发起了没有。
    pub started: bool,
    /// 当前刷新的序号（宿主原样交给 [`Engine::refresh_done`] / [`Engine::commit_round`]；只有 `refresh` 会填）。
    pub refresh_round: Option<i64>,
    /// 平台报了"没登录"（宿主转给 `AccountsState.markExpired`，**不发请求**）。
    pub auth_expired: Vec<String>,
    /// 调试日志（只记数量，不记标题 / 关键词）。
    pub log: Vec<String>,
}

/// 宿主抄回 Compose 状态用的快照（都是小标量 / 短列表）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub refreshing: bool,
    pub loading_more: bool,
    pub loaded_once: bool,
    pub refreshed_times: i32,
    pub retrying: Option<String>,
    /// 这一批失败 / 超时的平台和原因（一行字）。
    pub errors: Vec<(String, String)>,
    /// 明确回了"没登录"的平台。
    pub not_logged_in: Vec<String>,
    pub has_prefetched: bool,
}

/// 一条内容属于哪一批、在它平台的原始顺序里排第几（`seen_item` 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub batch_id: i64,
    pub platform_rank: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Refresh,
    LoadMore,
    Prefetch,
    Retry,
}

#[derive(Debug, Clone, Default)]
struct Pending {
    cursor: Option<String>,
    /// 这一家连着取了几页空页。
    blank: u32,
    /// 这一家这一趟攒下来的条目（多页拼起来，**已经过 18+ / 名单 / 内容类型**）。
    acc: Vec<Entry>,
    /// 这一家这一趟有没有结论（成功 / 失败都算）。
    done: bool,
    /// 这一家这一趟**真的取到页了**（失败不写游标：Kotlin 的 `fetch` 返回 null 时什么都不写）。
    got_page: bool,
}

#[derive(Debug)]
struct Round {
    id: u64,
    purpose: Purpose,
    background: bool,
    /// 收集中（`waiting == true`）：到了的内容直接进 `got`；超时之后到的进 `late`。
    waiting: bool,
    got: Vec<Entry>,
    pending: Vec<(String, Pending)>,
    /// `finish_round` 已经跑过；之后到的内容按"晚到"分派。
    finished: bool,
}

/// 一批落地的结果：上屏顺序 + 每条的平台序号。
struct PreparedBatch {
    items: Vec<Entry>,
    ranks: HashMap<String, i32>,
}

/// 去重用的 seen：`Consume` 直接改屏幕的 key 集合，`Copy` 只改副本（预取还没上屏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeenMode {
    Consume,
    Copy,
}

/// 信息流状态机。
///
/// 所有方法都是同步的、不加锁：宿主在**主线程（Compose 的 `scope`）**上调它（和原 Kotlin
/// "所有状态只在 scope 里改，所以不用加锁"一样）。
#[derive(Debug)]
pub struct Engine {
    kind: FeedKind,
    cfg: EngineConfig,

    /// 屏幕上的条目（屏幕顺序）。
    items: Vec<Entry>,
    keys: HashSet<String>,
    origins: HashMap<String, Origin>,
    last_batch_id: i64,
    top_batch_size: i32,

    /// 各平台的下一页游标；**没有键** = 第一页没拿到，值为 `None` = 到底了。
    cursors: HashMap<String, Option<String>>,

    /// 备好的那一批 + 它自己的翻页游标。
    prefetched: Vec<Entry>,
    prefetched_cursors: HashMap<String, Option<String>>,
    prefetching: bool,
    prefetch_tried: bool,

    refreshing: bool,
    loading_more: bool,
    loaded_once: bool,
    refreshed_times: i32,
    retrying: Option<String>,
    errors: Vec<(String, String)>,
    not_logged_in: Vec<String>,

    started: bool,
    asked_refresh: bool,
    last_refresh_at: i64,
    refresh_started_at: i64,
    refresh_rounds: i64,

    late: Vec<Entry>,
    slow: HashSet<String>,

    round: Option<Round>,
    next_round_id: u64,
    /// `finish_round` 算好的那一批 + 它是哪一轮，等最短动画之后由 `commit_round` 上屏。
    staged: Option<(i64, Vec<Entry>)>,
}

impl Engine {
    pub fn new(kind: FeedKind, cfg: EngineConfig) -> Self {
        Self {
            kind,
            cfg,
            items: Vec::new(),
            keys: HashSet::new(),
            origins: HashMap::new(),
            last_batch_id: 0,
            top_batch_size: 0,
            cursors: HashMap::new(),
            prefetched: Vec::new(),
            prefetched_cursors: HashMap::new(),
            prefetching: false,
            prefetch_tried: false,
            refreshing: false,
            loading_more: false,
            loaded_once: false,
            refreshed_times: 0,
            retrying: None,
            errors: Vec::new(),
            not_logged_in: Vec::new(),
            started: false,
            asked_refresh: false,
            last_refresh_at: 0,
            refresh_started_at: 0,
            refresh_rounds: 0,
            late: Vec::new(),
            slow: HashSet::new(),
            round: None,
            next_round_id: 1,
            staged: None,
        }
    }

    pub fn kind(&self) -> FeedKind {
        self.kind
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    // ------------------------------------------------------------------ 读

    /// 屏幕上的 key（屏幕顺序）。
    pub fn screen_keys(&self) -> Vec<String> {
        self.items.iter().map(|e| e.key.clone()).collect()
    }

    /// 备好的那批的 key（屏幕顺序）。
    pub fn prefetched_keys(&self) -> Vec<String> {
        self.prefetched.iter().map(|e| e.key.clone()).collect()
    }

    /// 一条内容属于哪一批、平台第几条；`None` = 不在当前信息流里（没记过，或者已经被过滤掉了）。
    pub fn origin_of(&self, key: &str) -> Option<Origin> {
        self.origins.get(key).copied()
    }

    /// 现在有没有在飞的轮次（测试 / 调试用）。
    pub fn round_id(&self) -> Option<u64> {
        self.round.as_ref().map(|r| r.id)
    }

    pub fn view(&self) -> View {
        View {
            refreshing: self.refreshing,
            loading_more: self.loading_more,
            loaded_once: self.loaded_once,
            refreshed_times: self.refreshed_times,
            retrying: self.retrying.clone(),
            errors: self.errors.clone(),
            not_logged_in: self.not_logged_in.clone(),
            has_prefetched: !self.prefetched.is_empty(),
        }
    }

    // ------------------------------------------------------------------ 冷启动

    /// 冷启动把上一批摆出来（`FeedState` 的 `init`）。
    ///
    /// 读到时页面还是空的才摆上（刷新先回来了就不摆）；现在不看的平台、18+、不是现在内容类型的不摆；
    /// **按存下来的屏幕顺序摆回去**（快照存的就是屏幕顺序），不再按时间重排。摆出来的这批也要能记
    /// "曝光过的内容"：给一个合成 batch id，平台序号按它在这个屏幕顺序里的位置。
    pub fn load_snapshot(
        &mut self,
        cached: &[FeedRef],
        platforms: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        let mut action = Action::default();
        if !self.items.is_empty() || cached.is_empty() {
            return action;
        }
        let mut displayed = Vec::with_capacity(cached.len());
        for item in admitted(cached, cfg, self.kind, &mut action) {
            if !type_ok(item, cfg.content_type) || !platforms.iter().any(|p| *p == item.platform) {
                continue;
            }
            if self.keys.insert(item.key.clone()) {
                displayed.push(item.entry());
            }
        }
        if displayed.is_empty() || !self.items.is_empty() {
            return action;
        }
        let batch_id = self.new_batch_id(now_ms);
        let mut counted: HashMap<String, i32> = HashMap::new();
        for entry in &displayed {
            let n = *counted.get(&entry.platform).unwrap_or(&0);
            self.origins.insert(
                entry.key.clone(),
                Origin {
                    batch_id,
                    platform_rank: n,
                },
            );
            counted.insert(entry.platform.clone(), n + 1);
        }
        action.new_content = displayed.iter().map(|e| e.key.clone()).collect();
        self.top_batch_size = displayed.len() as i32;
        self.prefetch_tried = false;
        // 已经有内容摆在屏幕上了：之后说的"空"是有依据的
        self.loaded_once = true;
        self.items = displayed;
        action.screen_changed = true;
        action
    }

    // ------------------------------------------------------------------ 入口

    /// 进页面时调：页面是空的就刷新；摆着上一批、还没刷新过，就只在后台预取新的一批。
    pub fn ensure_loaded(
        &mut self,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        if !foreground {
            return Action::default();
        }
        if self.refreshing || self.prefetching {
            return Action::default();
        }
        if self.items.is_empty() {
            return self.refresh(false, foreground, sources, cfg, now_ms);
        }
        if !self.started {
            return self.prefetch(foreground, sources);
        }
        Action::default()
    }

    /// App 退到后台：取消在飞的（宿主 `Job.cancel`）、清干净晚到 / 慢平台、状态全部落下。
    pub fn on_background(&mut self, foreground: bool) {
        if foreground {
            return; // 桌面 / 已经在后台：什么都不用做
        }
        self.round = None;
        self.staged = None;
        self.refreshing = false;
        self.loading_more = false;
        self.prefetching = false;
        self.retrying = None;
        self.late.clear();
        self.slow.clear();
    }

    /// App 回到前台：**不自动替换屏幕上的内容**。
    pub fn on_foreground(
        &mut self,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        if !foreground {
            return Action::default();
        }
        if self.refreshing || self.prefetching {
            return Action::default();
        }
        if self.items.is_empty() {
            return self.refresh(false, true, sources, cfg, now_ms);
        }
        if !self.started {
            return self.prefetch(true, sources);
        }
        if !self.prefetched.is_empty() {
            return Action::default();
        }
        if now_ms - self.last_refresh_at >= self.cfg.resume_stale_ms {
            return self.prefetch(true, sources);
        }
        Action::default()
    }

    /// 用户往下滑到了"最后一张可见卡片"是第 `last_visible` 条：看过当前这批的一半就备下一批。
    pub fn on_scrolled_to(
        &mut self,
        last_visible: i32,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        if self.top_batch_size < 2 || last_visible < self.top_batch_size / 2 {
            return Action::default();
        }
        // 这一批已经备过一次（成没成都算）：不要每滑一格就重发一轮请求
        if self.prefetch_tried {
            return Action::default();
        }
        let action = self.prefetch_if_idle(foreground, sources, cfg, now_ms);
        if action.started {
            self.prefetch_tried = true;
        }
        action
    }

    pub fn on_open_detail(
        &mut self,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        self.prefetch_tried = true;
        self.prefetch_if_idle(foreground, sources, cfg, now_ms)
    }

    /// 该不该在后台备下一批：后台一个请求都不发、一次只备一批、有在飞的（刷新 / 翻页 / 预取 / 重试）就不取。
    fn prefetch_if_idle(
        &mut self,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        let _ = (cfg, now_ms);
        if !foreground || !self.prefetched.is_empty() || self.prefetching {
            return Action::default();
        }
        if self.refreshing || self.loading_more || self.retrying.is_some() {
            return Action::default();
        }
        self.prefetch(foreground, sources)
    }

    /// 刷新：手上**有备好的批次**就直接整批放到最上面（不管放了多久、不再请求）；否则照常请求。
    /// `fresh = true`（设置里改了平台 / 内容类型）时一定重新请求，备好的那批丢掉。
    pub fn refresh(
        &mut self,
        fresh: bool,
        foreground: bool,
        sources: &[String],
        _cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        let mut action = Action::default();
        if !foreground {
            return action; // 后台：一个平台请求都不发
        }
        // 最短动画还没到：这次刷新还在"转圈"里，不再接第二次
        if self.refreshing && now_ms - self.refresh_started_at < self.cfg.refresh_min_animation_ms {
            return action;
        }
        self.refresh_started_at = now_ms;
        self.asked_refresh = true;
        let ready = !fresh
            && !self.prefetching
            && self.prefetched.iter().any(|e| !self.keys.contains(&e.key));
        if ready {
            // 备好的那一批：预取时已经排好、批 id / 平台序号也记好了，这里只等最短动画时间再整批放上去。
            // 这段时间里 prefetched **先留着**：中途被取消（退到后台）的话它还在这儿，不会白白丢掉一批内容。
            self.started = true;
            self.refreshing = true;
            // 在飞的翻页 / 重试取消掉（宿主 cancel 协程）：它回来会回写旧游标、盖掉刚接管的预取游标
            self.round = None;
            self.staged = None;
            self.loading_more = false;
            self.retrying = None;
            self.late.clear();
            self.slow.clear();
            self.errors.clear();
            self.not_logged_in.clear();
            self.begin_round(Purpose::Refresh, false, Vec::new());
            action.flow = Flow::CommitPrefetched;
            action.mode = RefreshMode::UsePrefetched;
            action.refresh_round = Some(self.refresh_rounds);
            // 这一路上「已经过的时间」是 0（刚设的 refresh_started_at），所以就是整个最短时间
            action.remain_ms = self.hold_animation(now_ms);
            return action;
        }
        let had_prefetched = !self.prefetched.is_empty();
        self.drop_prefetched();
        // 在飞的预取也取消（它备的是"下一次刷新"的内容，这次现场请求已经顶掉了它）
        self.prefetching = false;
        if had_prefetched {
            action.prefetched_changed = true;
        }
        self.started = true;
        self.round = None;
        self.staged = None;
        self.late.clear();
        self.slow.clear();
        self.refreshing = true;
        self.loading_more = false;
        self.retrying = None;
        self.errors.clear();
        self.not_logged_in.clear();
        let todo: Vec<(String, Option<String>)> =
            sources.iter().map(|p| (p.clone(), None)).collect();
        let round = self.begin_round(Purpose::Refresh, false, todo);
        action.flow = Flow::RefreshRound;
        action.mode = RefreshMode::Request;
        action.refresh_round = Some(self.refresh_rounds);
        action.requests = self.requests_of(round);
        // 最短动画在 `finish_round` 里算
        action
    }

    /// 用备好的那批上屏（最短动画之后调）：去掉已经在屏幕上的，再按当前上限夹一次。
    pub fn commit_prefetched(&mut self, cfg: &BatchCfg, now_ms: i64) -> Action {
        let mut action = Action::default();
        let batch = self.take_prefetched(cfg);
        if batch.is_empty() {
            // 一条都上不了屏：它的游标也没有意义了
            self.prefetched_cursors.clear();
        } else {
            let keys: Vec<String> = batch.iter().map(|e| e.key.clone()).collect();
            let mut screen = batch;
            screen.extend(self.items.drain(..));
            self.items = screen;
            // 这批真的上屏了：它的翻页游标从此接管
            self.adopt_prefetched_cursors();
            self.top_batch_size = keys.len() as i32;
            self.prefetch_tried = false; // 新的一批：看过它的一半可以再备下一批
            self.trim_origins();
            self.refreshed_times += 1;
            self.last_refresh_at = now_ms;
            action.new_content = keys;
            action.screen_changed = true;
            action.save_snapshot = true;
        }
        // 这一刷到底了（一条都没上屏也算）：界面从此可以说"这次没有内容"
        self.loaded_once = true;
        self.refreshing = false;
        self.round = None;
        action.prefetched_changed = true;
        action
    }

    /// 刷新当前结束了：**最后一轮**才清掉"正在刷"。
    ///
    /// 被顶掉 / 被取消的上一轮晚一点才跑到这里，它一清就会把新一轮刚点亮的"正在刷新"灭掉，
    /// 所以这里要比一下轮次（宿主在 `finally` 里调）。
    pub fn refresh_done(&mut self, round: i64) {
        if round != self.refresh_rounds {
            return;
        }
        self.refreshing = false;
    }

    /// 取下一页（各平台一页，一批）；真的发起了返回 `started = true`。
    pub fn load_more(
        &mut self,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        let _ = now_ms;
        let mut action = Action::default();
        if !foreground {
            return action;
        }
        if self.refreshing || self.loading_more {
            return action;
        }
        // 预取在飞、又还没点过刷新（冷启动）：等它接上（不白等那一批）
        if self.prefetching && !self.asked_refresh {
            return action;
        }
        if !self.asked_refresh && !self.prefetched.is_empty() {
            // 冷启动摆着上一批、还没刷新过：预取的那批接在后面当下一页
            let batch = self.take_prefetched(cfg);
            if !batch.is_empty() {
                let keys: Vec<String> = batch.iter().map(|e| e.key.clone()).collect();
                self.items.extend(batch);
                // 接上预取那批之后，游标就是"预取那批后面那页"
                self.adopt_prefetched_cursors();
                action.new_content = keys;
                action.screen_changed = true;
            } else {
                self.prefetched_cursors.clear();
            }
            action.prefetched_changed = true;
            action.started = true;
            return action;
        }
        let todo: Vec<(String, Option<String>)> = sources
            .iter()
            .filter_map(|p| match self.cursors.get(p) {
                Some(Some(cursor)) => Some((p.clone(), Some(cursor.clone()))),
                _ => None,
            })
            .filter(|(p, _)| !self.slow.contains(p))
            .collect();
        if todo.is_empty() && self.late.is_empty() {
            return action;
        }
        self.loading_more = true;
        let round = self.begin_round(Purpose::LoadMore, false, todo);
        action.flow = Flow::Round;
        action.requests = self.requests_of(round);
        action.started = true;
        action
    }

    /// **只重试一个平台**：不清空屏幕上的内容、不动别的平台的错误、不整批重排。
    pub fn retry_platform(
        &mut self,
        platform: &str,
        foreground: bool,
        sources: &[String],
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> Action {
        let _ = (cfg, now_ms);
        let mut action = Action::default();
        if !foreground || self.refreshing || self.loading_more || self.prefetching {
            return action;
        }
        if self.retrying.is_some() {
            return action;
        }
        if !sources.iter().any(|p| p == platform) {
            return action;
        }
        self.errors.retain(|(p, _)| p != platform);
        self.not_logged_in.retain(|p| p != platform);
        self.slow.remove(platform);
        self.retrying = Some(platform.to_string());
        let round = self.begin_round(Purpose::Retry, false, vec![(platform.to_string(), None)]);
        action.flow = Flow::Round;
        action.requests = self.requests_of(round);
        action.started = true;
        action
    }

    /// 后台预取第一页（各平台一页，规则和刷新一样），结果先放在 `prefetched`。
    fn prefetch(&mut self, foreground: bool, sources: &[String]) -> Action {
        let mut action = Action::default();
        if !foreground {
            return action;
        }
        self.started = true;
        self.prefetching = true;
        let todo: Vec<(String, Option<String>)> =
            sources.iter().map(|p| (p.clone(), None)).collect();
        let round = self.begin_round(Purpose::Prefetch, true, todo);
        action.flow = Flow::Round;
        action.requests = self.requests_of(round);
        action.started = true;
        action
    }

    // ------------------------------------------------------------------ 请求回来的那一步

    /// 一次平台请求回来了（成功或失败），回答"这个平台还要不要再来一页"。
    ///
    /// 宿主那边一家的请求是个**小循环**（和 Kotlin 的 `fetch` 一样）：拿到 [`PageStep::next`] 是
    /// `Some(cursor)` 就再发一页，`None` 就收工。
    #[allow(clippy::too_many_arguments)]
    pub fn on_page(
        &mut self,
        round: u64,
        platform: &str,
        outcome: PageOutcome,
        cfg: &BatchCfg,
        now_ms: i64,
    ) -> PageStep {
        let mut action = Action::default();
        if self.round.as_ref().map(|r| r.id) != Some(round) {
            // 过期的轮次（被刷新 / 退后台顶掉了）：结果丢掉
            return PageStep { next: None, action };
        }
        match outcome {
            PageOutcome::Failed {
                not_logged_in,
                describe,
            } => {
                if not_logged_in {
                    // 顺手把「我的 → 账号」里这个平台标成过期（不发请求）；预取失败也照调
                    action.auth_expired.push(platform.to_string());
                }
                let background = self.round.as_ref().map(|r| r.background).unwrap_or(false);
                if !background {
                    self.set_error(platform, &describe);
                    if not_logged_in && !self.not_logged_in.iter().any(|p| p == platform) {
                        self.not_logged_in.push(platform.to_string());
                    }
                }
                if let Some(r) = self.round.as_mut() {
                    if let Some((_, pending)) = r.pending.iter_mut().find(|(p, _)| p == platform) {
                        pending.done = true;
                        pending.got_page = false;
                    }
                }
                self.finish_chain(platform, false, cfg, now_ms, &mut action);
                PageStep { next: None, action }
            }
            PageOutcome::Ok { items, next } => {
                // 18+ / 名单这一步决定"这一页算不算空页"，内容类型过滤在它之后（和 Kotlin 的
                // `fetch` 里 `kept` → `gather` 里 `filter` 同一个顺序）
                let shown = admitted(&items, cfg, self.kind, &mut action);
                let page_blank = shown.is_empty();
                let entries: Vec<Entry> = shown
                    .into_iter()
                    .filter(|item| type_ok(item, cfg.content_type))
                    .map(|item| item.entry())
                    .collect();
                let mut cont = None;
                if let Some(r) = self.round.as_mut() {
                    if let Some((_, pending)) = r.pending.iter_mut().find(|(p, _)| p == platform) {
                        pending.cursor = next.clone();
                        pending.acc.extend(entries);
                        pending.got_page = true;
                        // 这一页有能显示的，或者到底了，就不接着翻了
                        if page_blank && next.is_some() {
                            pending.blank += 1;
                            if pending.blank < self.cfg.max_blank_pages {
                                cont = next.clone();
                            }
                        }
                        if cont.is_none() {
                            pending.done = true;
                        }
                    }
                }
                if cont.is_some() {
                    return PageStep { next: cont, action };
                }
                self.finish_chain(platform, true, cfg, now_ms, &mut action);
                PageStep { next: None, action }
            }
        }
    }

    /// 一家这一趟的链条结束了：写游标，把攒的条目按"当时在不在收集中"分派（`gather` 的 `when` + `finally`）。
    fn finish_chain(
        &mut self,
        platform: &str,
        success: bool,
        cfg: &BatchCfg,
        now_ms: i64,
        action: &mut Action,
    ) {
        let Some(round) = self.round.as_ref() else {
            return;
        };
        let waiting = round.waiting;
        let background = round.background;
        let purpose = round.purpose;
        let (cursor, chain, got_page) = match round.pending.iter().find(|(p, _)| p == platform) {
            Some((_, pending)) => (
                pending.cursor.clone(),
                pending.acc.clone(),
                pending.got_page,
            ),
            None => (None, Vec::new(), false),
        };
        if got_page {
            match purpose {
                Purpose::Prefetch => {
                    self.prefetched_cursors.insert(platform.to_string(), cursor);
                }
                _ => {
                    self.cursors.insert(platform.to_string(), cursor);
                }
            }
        }
        let finished = round.finished;
        if !chain.is_empty() {
            if waiting && !finished {
                if let Some(r) = self.round.as_mut() {
                    r.got.extend(chain);
                }
            } else if background {
                // 预取的晚到内容直接丢掉（预取的那批已经交了，再塞进来游标也对不上）
            } else if self.items.is_empty() && !self.refreshing && !self.loading_more {
                // 页面还是空的：直接显示（不和别的平台合批、不排序，和 Kotlin 一样）
                let batch_id = self.new_batch_id(now_ms);
                let prepared = self.prepare_batch(chain, batch_id, SeenMode::Consume, cfg, &mut action.log);
                for entry in &prepared.items {
                    self.origins.insert(
                        entry.key.clone(),
                        Origin {
                            batch_id,
                            platform_rank: *prepared.ranks.get(&entry.key).unwrap_or(&0),
                        },
                    );
                }
                action.new_content = prepared.items.iter().map(|e| e.key.clone()).collect();
                action.screen_changed = true;
                self.items = prepared.items;
            } else {
                self.late.extend(chain);
            }
        }
        if !background {
            if success && !waiting {
                self.errors.retain(|(p, _)| p != platform);
                self.not_logged_in.retain(|p| p != platform);
            }
            self.slow.remove(platform);
        }
    }

    /// 超时了：`waiting = false`，没回来的平台记进 `slow` / `errors`（预取不记）。
    ///
    /// 宿主在 `withTimeoutOrNull(wait_ms) { joinAll() }` 之后调一次。
    pub fn on_timeout(&mut self) {
        let Some(round) = self.round.as_ref() else {
            return;
        };
        let background = round.background;
        let unfinished: Vec<String> = round
            .pending
            .iter()
            .filter(|(_, p)| !p.done)
            .map(|(platform, _)| platform.clone())
            .collect();
        if let Some(r) = self.round.as_mut() {
            r.waiting = false;
        }
        if background {
            return;
        }
        let message = self.cfg.timeout_message();
        for platform in unfinished {
            self.slow.insert(platform.clone());
            self.set_error(&platform, &message);
        }
    }

    // ------------------------------------------------------------------ 落地

    /// 一轮收完：去重 → 封顶 → 排序（**这一步就吃掉 key**），刷新 / 重试的结果暂存等最短动画。
    ///
    /// 预取和翻页直接在这里落地（它们不需要动画）；刷新要等 [`commit_round`](Self::commit_round)。
    pub fn finish_round(&mut self, cfg: &BatchCfg, now_ms: i64) -> Action {
        let mut action = Action::default();
        // **不把 round 拿走**：超时之后还在飞的请求回来时还要靠它分派（进 `late` / 写游标 / 消错误），
        // 和 Kotlin 那边"超时的 job 继续跑、回来照样改 late / cursors / errors"一样。
        // 下一轮开始（`begin_round`）才换掉它。
        let Some(round) = self.round.as_mut() else {
            return action;
        };
        let purpose = round.purpose;
        round.finished = true;
        let got = std::mem::take(&mut round.got);
        let batch_id = self.new_batch_id(now_ms);
        let mut batch = Vec::new();
        match purpose {
            Purpose::LoadMore => {
                // 晚到的和这一批一起排
                batch.append(&mut self.late);
                batch.extend(got);
            }
            Purpose::Refresh | Purpose::Retry | Purpose::Prefetch => batch.extend(got),
        }
        match purpose {
            Purpose::Prefetch => {
                // 预取用 keys 的副本去重（还没上屏，不能算消费掉）
                let prepared =
                    self.prepare_batch(batch, batch_id, SeenMode::Copy, cfg, &mut action.log);
                if prepared.items.is_empty() {
                    // 一条都没取回来（都失败 / 都超时）：不算"备好了一批"，游标也不要
                    self.prefetched.clear();
                    self.prefetched_cursors.clear();
                } else {
                    self.prefetched = prepared.items;
                    self.last_refresh_at = now_ms;
                }
                self.prefetching = false;
                action.prefetched_changed = true;
                return action;
            }
            Purpose::Refresh => {
                let prepared =
                    self.prepare_batch(batch, batch_id, SeenMode::Consume, cfg, &mut action.log);
                for entry in &prepared.items {
                    self.origins.insert(
                        entry.key.clone(),
                        Origin {
                            batch_id,
                            platform_rank: *prepared.ranks.get(&entry.key).unwrap_or(&0),
                        },
                    );
                }
                self.staged = Some((self.refresh_rounds, prepared.items));
                // 刷新最短动画：从当前开始算（现场请求超过 400 ms 的不额外等）
                action.remain_ms = self.hold_animation(now_ms);
                action.refresh_round = Some(self.refresh_rounds);
                action.flow = Flow::RefreshRound;
                return action;
            }
            Purpose::LoadMore => {
                let prepared =
                    self.prepare_batch(batch, batch_id, SeenMode::Consume, cfg, &mut action.log);
                for entry in &prepared.items {
                    self.origins.insert(
                        entry.key.clone(),
                        Origin {
                            batch_id,
                            platform_rank: *prepared.ranks.get(&entry.key).unwrap_or(&0),
                        },
                    );
                }
                if !prepared.items.is_empty() {
                    action.new_content = prepared.items.iter().map(|e| e.key.clone()).collect();
                    self.items.extend(prepared.items);
                    action.screen_changed = true;
                }
                self.loading_more = false;
                return action;
            }
            Purpose::Retry => {
                let prepared =
                    self.prepare_batch(batch, batch_id, SeenMode::Consume, cfg, &mut action.log);
                for entry in &prepared.items {
                    self.origins.insert(
                        entry.key.clone(),
                        Origin {
                            batch_id,
                            platform_rank: *prepared.ranks.get(&entry.key).unwrap_or(&0),
                        },
                    );
                }
                if !prepared.items.is_empty() {
                    action.new_content = prepared.items.iter().map(|e| e.key.clone()).collect();
                    let mut screen = prepared.items;
                    screen.extend(self.items.drain(..));
                    self.items = screen;
                    action.screen_changed = true;
                }
                self.retrying = None;
                return action;
            }
        }
    }

    /// 把暂存的那一批放到最上面（最短动画之后）。`round` 是 `finish_round` 给回来的轮次号。
    pub fn commit_round(&mut self, round: i64, now_ms: i64) -> Action {
        let mut action = Action::default();
        // 被顶掉的那一轮：它算好的那批**作废**（新的那一轮会自己排），不能上屏
        let staged = match self.staged.take() {
            Some((r, batch)) if r == round => Some(batch),
            _ => None,
        };
        if let Some(batch) = staged {
            if !batch.is_empty() {
                let keys: Vec<String> = batch.iter().map(|e| e.key.clone()).collect();
                let mut screen = batch;
                screen.extend(self.items.drain(..));
                self.items = screen;
                self.top_batch_size = keys.len() as i32;
                self.prefetch_tried = false; // 新的一批：看过它的一半可以再备下一批
                self.trim_origins();
                self.refreshed_times += 1;
                self.last_refresh_at = now_ms;
                action.new_content = keys;
                action.screen_changed = true;
                action.save_snapshot = true;
            }
            // 这一刷到底了（一条都没上屏、全失败 / 超时也算）
            self.loaded_once = true;
            self.refresh_done(round);
        }
        action
    }

    /// 当前被打断（退到后台 / 被顶掉）：暂存的那批丢掉。
    pub fn abort_round(&mut self) {
        self.staged = None;
        self.round = None;
    }

    /// 内容过滤设置改了：把屏幕上**已经显示、按现在的规则应该藏掉**的条目去掉。
    ///
    /// 18+ / 名单那一步已经是 Rust（`brain_filter_shown`，宿主按 [`FeedRef`] 算好保留下来的 key），
    /// 引擎只负责把屏幕列表 / 去重集合 / 批序号一起收拢（`dropHidden` 的后半段）。**解除不了**：被藏掉的
    /// 条目不在列表里，关掉外出模式后要等下一批才回来。
    pub fn drop_hidden(&mut self, kept_keys: &[String]) -> Action {
        let mut action = Action::default();
        if self.items.is_empty() {
            return action;
        }
        let kept: HashSet<&str> = kept_keys.iter().map(|k| k.as_str()).collect();
        let before = self.items.len();
        self.items.retain(|e| kept.contains(e.key.as_str()));
        if self.items.len() == before {
            return action;
        }
        // 去掉的条目也从"已显示"里拿走：以后再刷到它们（比如关掉外出模式）还能重新出现
        self.keys.clear();
        for entry in &self.items {
            self.keys.insert(entry.key.clone());
        }
        // 它们的"哪一批、平台第几条"也一起作废
        let screen: HashSet<&str> = self.items.iter().map(|e| e.key.as_str()).collect();
        self.origins.retain(|key, _| screen.contains(key.as_str()));
        action.screen_changed = true;
        action
    }

    // ------------------------------------------------------------------ 内部

    fn begin_round(
        &mut self,
        purpose: Purpose,
        background: bool,
        todo: Vec<(String, Option<String>)>,
    ) -> u64 {
        let id = self.next_round_id;
        self.next_round_id += 1;
        // 只有刷新算"一轮刷新"（`refresh_done` 靠它认最后一轮）
        if purpose == Purpose::Refresh {
            self.refresh_rounds += 1;
        }
        self.round = Some(Round {
            id,
            purpose,
            background,
            waiting: true,
            got: Vec::new(),
            finished: false,
            pending: todo
                .into_iter()
                .map(|(platform, cursor)| {
                    (
                        platform,
                        Pending {
                            cursor,
                            ..Pending::default()
                        },
                    )
                })
                .collect(),
        });
        id
    }

    fn requests_of(&self, id: u64) -> Vec<FeedRequest> {
        let Some(round) = self.round.as_ref().filter(|r| r.id == id) else {
            return Vec::new();
        };
        round
            .pending
            .iter()
            .map(|(platform, pending)| FeedRequest {
                round: id,
                platform: platform.clone(),
                cursor: pending.cursor.clone(),
                background: round.background,
            })
            .collect()
    }

    fn new_batch_id(&mut self, now_ms: i64) -> i64 {
        self.last_batch_id = if now_ms > self.last_batch_id {
            now_ms
        } else {
            self.last_batch_id + 1
        };
        self.last_batch_id
    }

    /// 最短动画还差多久：`(min - 已经过的时间)` 夹在 `[0, min]`（时钟往回跳时也不会变成一个特别大的数）。
    fn hold_animation(&self, now_ms: i64) -> i64 {
        let elapsed = now_ms - self.refresh_started_at;
        let remain = self.cfg.refresh_min_animation_ms - elapsed;
        remain.clamp(0, self.cfg.refresh_min_animation_ms)
    }

    /// 一批：去重 → 封顶 → 排序（`BatchMerge.prepare`）。
    fn prepare_batch(
        &mut self,
        batch: Vec<Entry>,
        batch_id: i64,
        seen_mode: SeenMode,
        cfg: &BatchCfg,
        log: &mut Vec<String>,
    ) -> PreparedBatch {
        let mut seen = self.keys.clone();
        let fresh: Vec<Entry> = batch
            .into_iter()
            .filter(|e| seen.insert(e.key.clone()))
            .collect();
        if fresh.is_empty() {
            if seen_mode == SeenMode::Consume {
                self.keys = seen;
            }
            return PreparedBatch {
                items: Vec::new(),
                ranks: HashMap::new(),
            };
        }
        let order_kind = match (self.kind, cfg.order) {
            (FeedKind::Following, _) => OrderKind::Following,
            (_, FeedOrder::Time) => OrderKind::Time,
            _ => OrderKind::Interleave,
        };
        let mix = mix_items(&fresh);
        let prepared = brain::prepare(&mix, cfg.max_per_platform, order_kind, batch_id);
        // 被封顶砍掉的也算"当前消费掉"（它们的游标已经往前走了，再拉同一批也不该回来）
        for &i in &prepared.dropped {
            seen.insert(fresh[i as usize].key.clone());
        }
        let items: Vec<Entry> = prepared
            .order
            .iter()
            .map(|&i| fresh[i as usize].clone())
            .collect();
        let mut ranks: HashMap<String, i32> = HashMap::with_capacity(items.len() * 2);
        for (j, entry) in items.iter().enumerate() {
            ranks.insert(entry.key.clone(), prepared.ranks[j] as i32);
        }
        if !prepared.dropped.is_empty() {
            log.push(format!(
                "[{}] 每平台封顶砍掉 {} 条",
                self.kind.label(),
                prepared.dropped.len()
            ));
        }
        if seen_mode == SeenMode::Consume {
            self.keys = seen;
        }
        PreparedBatch { items, ranks }
    }

    fn adopt_prefetched_cursors(&mut self) {
        if self.prefetched_cursors.is_empty() {
            return;
        }
        for (platform, cursor) in self.prefetched_cursors.drain() {
            self.cursors.insert(platform, cursor);
        }
    }

    fn drop_prefetched(&mut self) {
        self.prefetched.clear();
        self.prefetched_cursors.clear();
    }

    /// 把预取的那一批拿出来（一次性）：去掉已经在屏幕上的，再按当前上限夹一次。
    ///
    /// **不再重排**：预取时已经按当时的顺序排好（批 id / 平台序号也在那时候记下了），这里重排一次会换种子、
    /// 甚至换成另一种设置，和屏幕上将要看到的顺序对不上。
    fn take_prefetched(&mut self, cfg: &BatchCfg) -> Vec<Entry> {
        let ready: Vec<Entry> = self
            .prefetched
            .drain(..)
            .filter(|e| self.keys.insert(e.key.clone()))
            .collect();
        if ready.is_empty() || cfg.max_per_platform <= 0 {
            return ready;
        }
        let mix = mix_items(&ready);
        let capped = brain::cap(&mix, cfg.max_per_platform);
        capped
            .kept
            .iter()
            .map(|&i| ready[i as usize].clone())
            .collect()
    }

    fn trim_origins(&mut self) {
        if self.origins.len() <= self.items.len() {
            return;
        }
        let screen: HashSet<&str> = self.items.iter().map(|e| e.key.as_str()).collect();
        self.origins.retain(|key, _| screen.contains(key.as_str()));
    }

    fn set_error(&mut self, platform: &str, message: &str) {
        if let Some(slot) = self.errors.iter_mut().find(|(p, _)| p == platform) {
            slot.1 = message.to_string();
        } else {
            self.errors.push((platform.to_string(), message.to_string()));
        }
    }
}

fn mix_items(entries: &[Entry]) -> Vec<MixItem> {
    entries
        .iter()
        .map(|e| MixItem {
            key: e.key.clone(),
            platform: e.platform.clone(),
            published_at: e.published_at,
        })
        .collect()
}

/// 一页里**过了 18+ + 名单**的那些（`FeedState.kept`）。
///
/// **不看内容类型**：Kotlin 的"这一页算不算空页、要不要再翻一页"只由这一步决定（`fetch` 里的 `kept`），
/// 内容类型过滤是后面 `gather` 里的 `filter`。顺序搞反会让"只看视频"模式下整页图文反复翻页。
fn admitted<'a>(
    items: &'a [FeedRef],
    cfg: &BatchCfg,
    kind: FeedKind,
    action: &mut Action,
) -> Vec<&'a FeedRef> {
    if items.is_empty() {
        return Vec::new();
    }
    let texts: Vec<brain::MixText> = items.iter().map(|r| r.text()).collect();
    let shown = brain::shown(&texts, &cfg.rules);
    if shown.hidden > 0 {
        action
            .log
            .push(format!("[{}] 这一批藏掉 {} 条", kind.label(), shown.hidden));
    }
    shown
        .kept
        .iter()
        .map(|&i| &items[i as usize])
        .collect()
}

/// 内容类型过不过（`FeedState.filter`）：视频看 `isVideoPost`，直播两个都不算。
fn type_ok(item: &FeedRef, content_type: ContentType) -> bool {
    match content_type {
        ContentType::Mixed => true,
        ContentType::Video => !item.is_live && item.is_video,
        ContentType::Text => !item.is_live && !item.is_video,
    }
}
