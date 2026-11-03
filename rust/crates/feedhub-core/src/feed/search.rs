//! 跨平台搜索的**状态机**（`app/.../search/SearchState.kt`）。
//!
//! 搜索页的规则——搬过来的是**全部决策**：
//!
//! - 一次搜索**每个选中平台只发第一页**（请求数 = 平台数，不随结果多少变）；
//! - 每家一页只取 [`crate::brain::search::quota`] 条进自己的桶，按平台顺序轮转拼成一批；
//! 剩下的平台继续跑；超过 `first_batch_ms` 还没回来的
//!   不等了、记一条错误，它们晚到的内容留在桶里，下次翻页时接在后面（**屏幕上已有的位置不插东西**）；
//! - 翻页：每家取下一页（同样每家一页），轮转追加在后面；每家一次搜索最多 [`PAGE_BUDGET`] 页；
//! - 18+ 整条隐藏、内容过滤第一层在**进桶之前**过（`brain::shown`）；
//! - **平台标签就是筛选**：只有亮着的才请求、
//!   结果里也只有它们；最后一家点不灭；点灭立刻拿掉、点亮立刻补搜那一家；选择只在这次搜索页里有效
//!   （`close` 回到全亮，不写进设置）。
//!
//! **留宿主（Kotlin）的**：Compose 状态容器、协程 / 超时 / Channel（`withTimeoutOrNull` + `joinAll`
//! 换成"引擎收齐 / 宿主超时时报一声"）、真正的收发（`SearchSource.search`）、错误文案（`describe`）。

use std::collections::HashSet;

use crate::brain;

use super::feeds::{FeedRef, PageOutcome};

pub const READY_PLATFORMS: i32 = 3;

/// 每家一次搜索最多翻几页（和 `SearchPlan.PAGE_BUDGET` 一个数）。
pub const PAGE_BUDGET: i32 = 3;

/// 一批的目标条数。
pub const TARGET_BATCH: i32 = 20;

/// 搜索列表里的一条（只要 key + 平台；整条内容在宿主）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub platform: String,
}

/// 一条要宿主去发的搜索请求（**一家一页**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// 属于哪一轮（宿主回报结果时原样带回来）。
    pub round: u64,
    pub platform: String,
    pub cursor: Option<String>,
}

/// 一次搜索请求的结果（`Ok` 要带上这家一页给多少条，配额按它截）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    Ok {
        items: Vec<FeedRef>,
        next: Option<String>,
        /// 这家 `SearchCapability.pageSize`（配额的上限）。
        page_size: i32,
    },
    Failed {
        describe: String,
    },
}

/// 引擎返回给宿主"这一趟要做什么"。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    pub requests: Vec<SearchRequest>,
    /// 列表变了（宿主重新取 [`SearchEngine::screen_keys`]）。
    pub screen_changed: bool,
    /// 新追加的条目（宿主拿来预取图片之类；顺序就是上屏顺序）。
    pub appended: Vec<String>,
    /// `loadMore` / `toggle` 真的干了活没有。
    pub started: bool,
    /// 可以收掉"正在搜索"的转圈了。
    pub first_batch_done: bool,
    /// 调试日志（只记数量）。
    pub log: Vec<String>,
}

/// 宿主抄回 Compose 状态用的快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub query: String,
    pub loading: bool,
    pub loading_more: bool,
    /// 这次搜索页上摆出来的平台标签（按插件顺序）。
    pub platforms: Vec<String>,
    /// 亮着的平台（只有它们会被请求）。
    pub enabled: Vec<String>,
    /// 至少回了一条内容的平台数（"已到 N / M 家"里的 N）。
    pub answered: i32,
    pub exhausted: bool,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Bucket {
    platform: String,
    pages: i32,
    cursor: Option<String>,
    done: bool,
    had_items: bool,
    /// 这一家现在有请求在飞（避免翻页时又给它发一个）。
    busy: bool,
    buffer: Vec<Entry>,
}

/// 第几批、收了几家。
///
/// 不用 `Copy`：`Next` 带一个 `Vec<String>`。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Idle,
    First { got: i32, enough: i32 },
    /// 翻页：收齐 `expected` 家（或者超时）。
    ///
    /// `pending` 是**当前真的发出去的那几家**：超时那一步只给它们记错误。Kotlin 原版
    /// （`nextBatch(pending)`）就是这么做的——它遍历的是 `loadMore` 时抓下来的那个 `pending` 列表，
    /// 不是"现在所有还没到过的家"。两者会不一样：某个桶在 `load_more` 那一刻 `busy`
    /// 或者 `pages > 0`，它就不在当前里，超时跟它无关。
    Next {
        got: i32,
        expected: i32,
        pending: Vec<String>,
    },
}

/// 搜索页状态机。同步、不加锁：宿主在 Compose 的 `scope`（主线程）上调。
#[derive(Debug)]
pub struct SearchEngine {
    platforms: Vec<String>,
    enabled: Vec<String>,
    buckets: Vec<Bucket>,
    keys: HashSet<String>,
    items: Vec<Entry>,
    errors: Vec<(String, String)>,
    query: String,
    loading: bool,
    loading_more: bool,
    first_batch_done: bool,
    answered: i32,
    exhausted: bool,
    page_budget: i32,
    round: u64,
    next_round: u64,
    phase: Phase,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new(PAGE_BUDGET)
    }
}

impl SearchEngine {
    pub fn new(page_budget: i32) -> Self {
        Self {
            platforms: Vec::new(),
            enabled: Vec::new(),
            buckets: Vec::new(),
            keys: HashSet::new(),
            items: Vec::new(),
            errors: Vec::new(),
            query: String::new(),
            loading: false,
            loading_more: false,
            first_batch_done: false,
            answered: 0,
            exhausted: false,
            page_budget,
            round: 0,
            next_round: 1,
            phase: Phase::Idle,
        }
    }

    // ------------------------------------------------------------------ 读

    pub fn screen_keys(&self) -> Vec<String> {
        self.items.iter().map(|e| e.key.clone()).collect()
    }

    pub fn view(&self) -> View {
        View {
            query: self.query.clone(),
            loading: self.loading,
            loading_more: self.loading_more,
            platforms: self.platforms.clone(),
            enabled: self.enabled.clone(),
            answered: self.answered,
            exhausted: self.exhausted,
            errors: self.errors.clone(),
        }
    }

    pub fn round_id(&self) -> u64 {
        self.round
    }

    // ------------------------------------------------------------------ 标签

    /// 打开搜索页时摆出"这次会搜哪些平台"的标签（只画 chips，**不发请求**）。
    ///
    /// 已经选过（点灭了几家）就不动，免得一打开又把选择清掉。
    pub fn prepare_platforms(&mut self, sources: &[String]) -> Action {
        let mut action = Action::default();
        if self.platforms.is_empty() {
            self.platforms = sources.to_vec();
            self.enabled = self.platforms.clone();
            action.screen_changed = true;
        }
        action
    }

    // ------------------------------------------------------------------ 搜索

    /// 开始一次搜索（用户显式提交才会调）。换词 / 重新搜索会取消上一轮在飞的请求，结果清空。
    ///
    /// **亮着的那几家保留**：点灭了几家之后换词重搜，不会把点灭的又点亮；一个都没亮 / 还没选过时全亮。
    pub fn search(&mut self, text: &str, sources: &[String], rules: &brain::Rules) -> Action {
        let _ = rules;
        let mut action = Action::default();
        let q = text.trim().to_string();
        if q.is_empty() {
            return action;
        }
        self.cancel_round();
        self.query = q;
        self.items.clear();
        self.keys.clear();
        self.errors.clear();
        self.answered = 0;
        self.exhausted = false;
        self.loading_more = false;
        self.first_batch_done = false;
        let all = sources.to_vec();
        // 能搜的平台集合变了（设置里点亮 / 熄灭了平台）：当成新的一页，回到全亮；否则保留这次点灭的选择
        let same_set = all.len() == self.platforms.len()
            && all.iter().all(|p| self.platforms.iter().any(|q| q == p));
        let kept: Vec<String> = self
            .enabled
            .iter()
            .filter(|p| all.iter().any(|q| q == *p))
            .cloned()
            .collect();
        self.platforms = all.clone();
        self.enabled = if same_set && !kept.is_empty() {
            kept
        } else {
            self.platforms.clone()
        };
        self.buckets = all
            .iter()
            .filter(|p| self.enabled.iter().any(|q| q == *p))
            .map(|p| Bucket::new(p.clone()))
            .collect();
        if self.buckets.is_empty() {
            self.loading = false;
            self.exhausted = true;
            return action;
        }
        self.loading = true;
        let round = self.new_round();
        let enough = READY_PLATFORMS.min(self.buckets.len() as i32);
        self.phase = Phase::First { got: 0, enough };
        action.requests = self.start_fetch_all(round);
        action
    }

    /// 先上屏。
    ///
    /// `first_batch_ms` 只用来拼那句"超过 N 秒没返回"。
    pub fn first_batch_timeout(&mut self, first_batch_ms: i64) -> Action {
        let mut action = Action::default();
        if !matches!(self.phase, Phase::First { .. }) {
            return action;
        }
        let message = format!("超过 {} 秒没返回，这次没排进来", first_batch_ms / 1000);
        let busy: Vec<String> = self
            .buckets
            .iter()
            .filter(|b| b.busy)
            .map(|b| b.platform.clone())
            .collect();
        for platform in busy {
            self.set_error(&platform, &message);
        }
        self.finish_first(&mut action);
        action
    }

    /// 翻页一批的收尾（宿主 `withTimeoutOrNull(next_batch_ms)` 之后调）。
    pub fn next_batch_timeout(&mut self, next_batch_ms: i64) -> Action {
        let mut action = Action::default();
        let Phase::Next { pending, .. } = self.phase.clone() else {
            return action;
        };
        let message = format!("超过 {} 秒没返回，这次没排进来", next_batch_ms / 1000);
        // 只给**当前发出去的**那几家记错误（Kotlin 原版 `for (b in pending) if (b.pages == 0 && !b.done)`）
        for platform in pending {
            let untouched = self
                .buckets
                .iter()
                .find(|b| b.platform == platform)
                .map(|b| b.pages == 0 && !b.done)
                .unwrap_or(false);
            if untouched {
                self.set_error(&platform, &message);
            }
        }
        self.phase = Phase::Idle;
        self.append_batch(&mut action);
        self.update_progress();
        action
    }

    /// 一次请求回来了（成功 / 失败）。
    pub fn on_result(
        &mut self,
        round: u64,
        platform: &str,
        outcome: SearchOutcome,
        rules: &brain::Rules,
    ) -> Action {
        let mut action = Action::default();
        if round != self.round {
            return action; // 过期的轮次：结果丢掉
        }
        let present = self.buckets.iter().any(|b| b.platform == platform);
        if let Some(bucket) = self.buckets.iter_mut().find(|b| b.platform == platform) {
            bucket.busy = false;
        }
        match outcome {
            SearchOutcome::Ok {
                items,
                next,
                page_size,
            } => {
                if present {
                    let kept = filter_out(items, rules, &mut action);
                    let quota = brain::quota(self.buckets.len() as i32, page_size, TARGET_BATCH);
                    let take: Vec<Entry> = kept
                        .into_iter()
                        .take(if quota < 0 { 0 } else { quota as usize })
                        .collect();
                    let bucket = self
                        .buckets
                        .iter_mut()
                        .find(|b| b.platform == platform)
                        .expect("bucket present");
                    bucket.pages += 1;
                    bucket.cursor = next.clone();
                    if !take.is_empty() {
                        bucket.buffer.extend(take);
                        bucket.had_items = true;
                    }
                    if next.is_none() || bucket.pages >= self.page_budget {
                        bucket.done = true;
                    }
                }
            }
            SearchOutcome::Failed { describe } => {
                if present {
                    self.set_error(platform, &describe);
                    if let Some(bucket) = self.buckets.iter_mut().find(|b| b.platform == platform) {
                        bucket.done = true;
                    }
                }
            }
        }
        if self.first_batch_done {
            let has_buffer = self
                .buckets
                .iter()
                .find(|b| b.platform == platform)
                .map(|b| !b.buffer.is_empty())
                .unwrap_or(false);
            if has_buffer {
                self.append_batch(&mut action);
            }
            self.update_progress();
        }
        // 收了几家
        match self.phase.clone() {
            Phase::First { got, enough } => {
                let got = got + 1;
                self.phase = Phase::First { got, enough };
                if got >= enough || got >= self.buckets.len() as i32 {
                    self.finish_first(&mut action);
                }
            }
            Phase::Next {
                got,
                expected,
                pending,
            } => {
                let got = got + 1;
                self.phase = Phase::Next {
                    got,
                    expected,
                    pending,
                };
                if got >= expected {
                    self.phase = Phase::Idle;
                    self.append_batch(&mut action);
                    self.update_progress();
                }
            }
            Phase::Idle => {}
        }
        action
    }

    /// 取下一批（滑到底时调）。真的有动作返回 `started = true`。
    ///
    /// 先花掉桶里攒着的，都空了才向平台要下一页。
    pub fn load_more(&mut self, foreground: bool) -> Action {
        let mut action = Action::default();
        if !foreground || self.loading || self.loading_more || self.buckets.is_empty() {
            return action;
        }
        if self.buckets.iter().any(|b| !b.buffer.is_empty()) {
            self.append_batch(&mut action);
            action.started = true;
            return action;
        }
        let pending: Vec<(String, Option<String>)> = self
            .buckets
            .iter()
            .filter(|b| !b.done && !b.busy && b.pages < self.page_budget)
            .map(|b| (b.platform.clone(), b.cursor.clone()))
            .collect();
        if pending.is_empty() {
            self.exhausted = true;
            return action;
        }
        self.loading_more = true;
        let round = self.new_round();
        self.phase = Phase::Next {
            got: 0,
            expected: pending.len() as i32,
            pending: pending.iter().map(|(platform, _)| platform.clone()).collect(),
        };
        for (platform, cursor) in pending {
            if let Some(bucket) = self.buckets.iter_mut().find(|b| b.platform == platform) {
                bucket.busy = true;
            }
            action.requests.push(SearchRequest {
                round,
                platform,
                cursor,
            });
        }
        action.started = true;
        action
    }

    /// 点一下平台标签：**亮 → 灭**（不再请求这一家，已经拿到的结果也从列表里拿掉）/
    /// **灭 → 亮**（用当前关键词补搜这一家）。
    ///
    /// 返回 `started = false` = 这次点击没被接受（**最后一家不让点灭**；平台不在标签里也不接受）。
    pub fn toggle(&mut self, platform: &str, sources: &[String]) -> Action {
        let mut action = Action::default();
        if !self.platforms.iter().any(|p| p == platform) {
            return action;
        }
        if self.enabled.iter().any(|p| p == platform) {
            // 至少留一家：最后一家点不灭
            if self.enabled.len() <= 1 {
                return action;
            }
            self.enabled.retain(|p| p != platform);
            self.buckets.retain(|b| b.platform != platform);
            self.errors.retain(|(p, _)| p != platform);
            if self.items.iter().any(|e| e.platform == platform) {
                self.items.retain(|e| e.platform != platform);
                // 重新点亮时这一家会从第 1 页重新搜，旧 key 留着会把回来的条目当重复丢掉
                self.keys.clear();
                for entry in &self.items {
                    self.keys.insert(entry.key.clone());
                }
                action.screen_changed = true;
            }
            self.update_progress();
            action.started = true;
            return action;
        }
        // 点亮：把这一家加回桶里，用当前关键词补一页
        self.enabled.push(platform.to_string());
        let known = !self.query.is_empty() && sources.iter().any(|p| p == platform);
        if known {
            let bucket = Bucket::new(platform.to_string());
            self.buckets.push(bucket);
            let round = self.round;
            if let Some(bucket) = self.buckets.iter_mut().find(|b| b.platform == platform) {
                bucket.busy = true;
            }
            action.requests.push(SearchRequest {
                round,
                platform: platform.to_string(),
                cursor: None,
            });
        }
        self.update_progress();
        action.started = true;
        action
    }

    /// 关掉搜索页：取消在飞的请求（已经发出去的不会撤回，但结果不再写状态）、回到全亮。
    pub fn close(&mut self) {
        self.cancel_round();
        self.loading = false;
        self.loading_more = false;
        self.phase = Phase::Idle;
        // 选择只在这次搜索页里有效：关掉页面就回到全亮（下次打开是全亮的）
        self.enabled = self.platforms.clone();
    }

    // ------------------------------------------------------------------ 内部

    fn new_round(&mut self) -> u64 {
        self.round = self.next_round;
        self.next_round += 1;
        self.round
    }

    fn cancel_round(&mut self) {
        self.round = 0;
        self.next_round += 1;
    }

    /// 给现在的每个桶发第一页。
    fn start_fetch_all(&mut self, round: u64) -> Vec<SearchRequest> {
        let mut requests = Vec::with_capacity(self.buckets.len());
        for bucket in self.buckets.iter_mut() {
            bucket.busy = true;
            requests.push(SearchRequest {
                round,
                platform: bucket.platform.clone(),
                cursor: bucket.cursor.clone(),
            });
        }
        requests
    }

    fn finish_first(&mut self, action: &mut Action) {
        self.phase = Phase::Idle;
        self.first_batch_done = true;
        self.append_batch(action);
        action.first_batch_done = true;
        // 一家都没取回来：不要留一个永远空着的搜索页
        if self.items.is_empty() && self.errors.is_empty() {
            self.errors = self
                .buckets
                .iter()
                .map(|b| (b.platform.clone(), "没有搜到内容".to_string()))
                .collect();
        }
        self.update_progress();
        action.log.push(format!(
            "[搜索] 第一批 {} 条，{}/{} 家",
            self.items.len(),
            self.answered,
            self.buckets.len()
        ));
    }

    /// 轮转拼一批追加上屏（`SearchMerge.roundRobin` + `appendBatch`）。
    fn append_batch(&mut self, action: &mut Action) {
        let batch = self.round_robin();
        if batch.is_empty() {
            return;
        }
        let fresh: Vec<Entry> = batch
            .into_iter()
            .filter(|e| self.keys.insert(e.key.clone()))
            .collect();
        if !fresh.is_empty() {
            action.appended = fresh.iter().map(|e| e.key.clone()).collect();
            self.items.extend(fresh);
            action.screen_changed = true;
        }
        // 桶里的东西已经交出去了（没被收下的重复条目也丢掉，免得下次又拿出来）
        for bucket in self.buckets.iter_mut() {
            bucket.buffer.clear();
        }
        self.update_progress();
    }

    fn round_robin(&self) -> Vec<Entry> {
        let mut flat: Vec<Entry> = Vec::new();
        for platform in &self.platforms {
            let Some(bucket) = self.buckets.iter().find(|b| b.platform == *platform) else {
                continue;
            };
            if bucket.buffer.is_empty() {
                continue;
            }
            flat.extend(bucket.buffer.iter().cloned());
        }
        if flat.is_empty() {
            return Vec::new();
        }
        let mix: Vec<brain::MixItem> = flat
            .iter()
            .map(|e| brain::MixItem {
                key: e.key.clone(),
                platform: e.platform.clone(),
                published_at: None,
            })
            .collect();
        brain::round_robin(&self.platforms, &mix)
            .into_iter()
            .map(|i| flat[i as usize].clone())
            .collect()
    }

    fn update_progress(&mut self) {
        self.answered = self.buckets.iter().filter(|b| b.had_items).count() as i32;
        self.exhausted = self.buckets.iter().all(|b| {
            (b.done || b.pages >= self.page_budget) && b.buffer.is_empty() && !b.busy
        });
        if self.phase == Phase::Idle {
            self.loading = false;
            self.loading_more = false;
        }
    }

    fn set_error(&mut self, platform: &str, message: &str) {
        if let Some(slot) = self.errors.iter_mut().find(|(p, _)| p == platform) {
            slot.1 = message.to_string();
        } else {
            self.errors.push((platform.to_string(), message.to_string()));
        }
    }
}

impl Bucket {
    fn new(platform: String) -> Self {
        Self {
            platform,
            pages: 0,
            cursor: None,
            done: false,
            had_items: false,
            busy: false,
            buffer: Vec::new(),
        }
    }
}

/// 进桶之前过一遍：平台标成 18+ 的整条去掉，再按内容过滤第一层去掉（`SearchPlan.keep`）。
fn filter_out(items: Vec<FeedRef>, rules: &brain::Rules, action: &mut Action) -> Vec<Entry> {
    if items.is_empty() {
        return Vec::new();
    }
    let texts: Vec<brain::MixText> = items
        .iter()
        .map(|r| brain::MixText {
            adult: r.adult,
            title: r.title.clone(),
            summary: r.summary.clone(),
            community: r.community.clone(),
            repost_title: r.repost_title.clone(),
            repost_summary: r.repost_summary.clone(),
            repost_community: r.repost_community.clone(),
        })
        .collect();
    let shown = brain::shown(&texts, rules);
    if shown.hidden > 0 {
        action
            .log
            .push(format!("[搜索] 这一页藏掉 {} 条", shown.hidden));
    }
    shown
        .kept
        .iter()
        .map(|&i| Entry {
            key: items[i as usize].key.clone(),
            platform: items[i as usize].platform.clone(),
        })
        .collect()
}

/// `PageOutcome` 在搜索里用不到失败分支之外的东西（编译期提醒：搜索只关心"失败的那一行字"）。
#[allow(dead_code)]
fn _unused(_: PageOutcome) {}
