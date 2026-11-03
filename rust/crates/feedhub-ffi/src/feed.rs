//! **信息流 / 搜索 / 账号 / 关注管理**过 UniFFI 边界。
//!
//! Rust 侧是 [`feedhub_core::feed`]；这里只有 record / enum、四个 `uniffi::Object`（状态机）和一层薄薄的转发。
//! Kotlin 门面在 `rust-bridge/.../feedhub/feed/FeedEngine.kt` / `feedhub/search/SearchEngine.kt` /
//! `feedhub/mine/AccountEngine.kt` / `FollowingEngine.kt`，业务代码只看见 `feedhub.core` 的类型。
//!
//! **为什么是对象而不是纯函数**：这几个都是**状态机**（游标、去重集合、批 id、预取、确认框那一格），
//! 状态必须在 Rust 里只有一份。对象跟着 App 的 `FeedState` / `SearchState` / `AccountsState` /
//! `FollowingState` 活（一个实例一个），方法都是同步的、内部一把 `Mutex`——宿主在 Compose 的主线程上调，
//! 和原来"状态只在 scope 里改、不用加锁"是同一个线程模型，`Mutex` 只是让 UniFFI 的 `Send + Sync` 成立。
//!
//! **时间由宿主注入**（`now_ms`），Rust 不读时钟；**请求一个字节都不发**（收发在宿主，先挑 `requests`，
//! 回来再把结果喂给 `on_page` / `on_result` / `on_list`）。

use std::sync::{Arc, Mutex};

use feedhub_core::brain::filter::Rules;
// `FfiFeedKind`（Recommend / Following）是 B站 平台对象那一期就有的类型，形状一样，
// UniFFI 的类型名不能重复，所以这里直接复用，不再造第二个。
pub use crate::bili_source::FfiFeedKind;
use feedhub_core::feed::{
    accounts, exposure, feeds, following, mix, preload, search,
};

// ===========================================================================
// 信息流（FfiFeed*）
// ===========================================================================

/// 内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedContentType {
    Mixed,
    Video,
    Text,
}

/// 首页的批内顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedOrder {
    Interleave,
    Time,
}

/// 引擎的固定参数（宿主建 `FeedState` 时给一次）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedConfig {
    pub wait_ms: i64,
    pub resume_stale_ms: i64,
    pub max_blank_pages: u32,
    pub refresh_min_animation_ms: i64,
}

impl FfiFeedConfig {
    fn to_core(self) -> feeds::EngineConfig {
        feeds::EngineConfig {
            wait_ms: self.wait_ms,
            resume_stale_ms: self.resume_stale_ms,
            max_blank_pages: self.max_blank_pages,
            refresh_min_animation_ms: self.refresh_min_animation_ms,
        }
    }
}

/// 每一批现读的规则。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedBatchCfg {
    pub content_type: FfiFeedContentType,
    pub max_per_platform: i32,
    pub order: FfiFeedOrder,
    pub rules: FfiFeedRules,
}

/// 内容过滤第一层（和设置里的四项一一对应）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedRules {
    pub block_words: Vec<String>,
    pub outing_mode: bool,
    pub outing_keywords: Vec<String>,
    pub outing_communities: Vec<String>,
}

impl FfiFeedRules {
    fn to_core(&self) -> Rules {
        Rules {
            block_words: self.block_words.clone(),
            outing_mode: self.outing_mode,
            outing_keywords: self.outing_keywords.clone(),
            outing_communities: self.outing_communities.clone(),
        }
    }
}

/// 宿主送来的一页里的一条（排序字段 + 过滤字段 + 内容类型的两个布尔）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedRef {
    pub key: String,
    pub platform: String,
    pub published_at: Option<i64>,
    pub adult: bool,
    pub is_video: bool,
    pub is_live: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub community: Option<String>,
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    pub repost_community: Option<String>,
}

impl FfiFeedRef {
    fn to_core(&self) -> feeds::FeedRef {
        feeds::FeedRef {
            key: self.key.clone(),
            platform: self.platform.clone(),
            published_at: self.published_at,
            adult: self.adult,
            is_video: self.is_video,
            is_live: self.is_live,
            title: self.title.clone(),
            summary: self.summary.clone(),
            community: self.community.clone(),
            repost_title: self.repost_title.clone(),
            repost_summary: self.repost_summary.clone(),
            repost_community: self.repost_community.clone(),
        }
    }
}

/// 一条要宿主去发的请求。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedRequest {
    pub round: u64,
    pub platform: String,
    pub cursor: Option<String>,
    pub background: bool,
}

/// 当前刷新走哪条路。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedMode {
    #[default]
    Skipped,
    UsePrefetched,
    Request,
}

/// 宿主拿到这次回答之后该做什么（引擎指定的，宿主不要自己猜）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedFlow {
    #[default]
    None,
    /// 等 `remain_ms` 之后调 `commit_prefetched`。
    CommitPrefetched,
    /// 现场请求的刷新：发请求 → 等齐 / 超时 → `finish_round` → 等 `remain_ms` → `commit_round`。
    RefreshRound,
    /// 一轮（翻页 / 预取 / 重试）：发请求（可能是空的）→ 等齐 / 超时 → `finish_round`。
    Round,
}

/// 引擎返回给宿主"这一趟要做什么"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedAction {
    pub requests: Vec<FfiFeedRequest>,
    pub flow: FfiFeedFlow,
    pub mode: FfiFeedMode,
    pub remain_ms: i64,
    pub new_content: Vec<String>,
    pub screen_changed: bool,
    pub prefetched_changed: bool,
    pub save_snapshot: bool,
    pub started: bool,
    pub refresh_round: Option<i64>,
    pub auth_expired: Vec<String>,
    pub log: Vec<String>,
}

/// 一次平台请求的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiPageOutcome {
    Ok {
        items: Vec<FfiFeedRef>,
        next: Option<String>,
    },
    Failed {
        not_logged_in: bool,
        describe: String,
    },
}

/// `on_page` 的答复。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPageStep {
    pub next: Option<String>,
    pub action: FfiFeedAction,
}

/// 宿主抄回 Compose 状态用的快照。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedView {
    pub refreshing: bool,
    pub loading_more: bool,
    pub loaded_once: bool,
    pub refreshed_times: i32,
    pub retrying: Option<String>,
    pub errors: Vec<FfiFeedErrorRow>,
    pub not_logged_in: Vec<String>,
    pub has_prefetched: bool,
}

/// 一条"平台 + 一行原因"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedErrorRow {
    pub platform: String,
    pub message: String,
}

/// 一条内容属于哪一批、平台第几条。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiFeedOrigin {
    pub batch_id: i64,
    pub platform_rank: i32,
}

fn to_core_batch_cfg(cfg: &FfiFeedBatchCfg) -> feeds::BatchCfg {
    feeds::BatchCfg {
        content_type: match cfg.content_type {
            FfiFeedContentType::Mixed => feeds::ContentType::Mixed,
            FfiFeedContentType::Video => feeds::ContentType::Video,
            FfiFeedContentType::Text => feeds::ContentType::Text,
        },
        max_per_platform: cfg.max_per_platform,
        order: match cfg.order {
            FfiFeedOrder::Interleave => feeds::FeedOrder::Interleave,
            FfiFeedOrder::Time => feeds::FeedOrder::Time,
        },
        rules: cfg.rules.to_core(),
    }
}

fn to_ffi_action(action: feeds::Action) -> FfiFeedAction {
    FfiFeedAction {
        requests: action
            .requests
            .into_iter()
            .map(|r| FfiFeedRequest {
                round: r.round,
                platform: r.platform,
                cursor: r.cursor,
                background: r.background,
            })
            .collect(),
        flow: match action.flow {
            feeds::Flow::None => FfiFeedFlow::None,
            feeds::Flow::CommitPrefetched => FfiFeedFlow::CommitPrefetched,
            feeds::Flow::RefreshRound => FfiFeedFlow::RefreshRound,
            feeds::Flow::Round => FfiFeedFlow::Round,
        },
        mode: match action.mode {
            feeds::RefreshMode::Skipped => FfiFeedMode::Skipped,
            feeds::RefreshMode::UsePrefetched => FfiFeedMode::UsePrefetched,
            feeds::RefreshMode::Request => FfiFeedMode::Request,
        },
        remain_ms: action.remain_ms,
        new_content: action.new_content,
        screen_changed: action.screen_changed,
        prefetched_changed: action.prefetched_changed,
        save_snapshot: action.save_snapshot,
        started: action.started,
        refresh_round: action.refresh_round,
        auth_expired: action.auth_expired,
        log: action.log,
    }
}

fn to_ffi_view(view: feeds::View) -> FfiFeedView {
    FfiFeedView {
        refreshing: view.refreshing,
        loading_more: view.loading_more,
        loaded_once: view.loaded_once,
        refreshed_times: view.refreshed_times,
        retrying: view.retrying,
        errors: view
            .errors
            .into_iter()
            .map(|(platform, message)| FfiFeedErrorRow { platform, message })
            .collect(),
        not_logged_in: view.not_logged_in,
        has_prefetched: view.has_prefetched,
    }
}

/// 信息流的状态机（一个 `FeedState` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiFeedState {
    inner: Mutex<feeds::Engine>,
}

#[uniffi::export]
impl FfiFeedState {
    #[uniffi::constructor]
    pub fn new(kind: FfiFeedKind, cfg: FfiFeedConfig) -> Arc<Self> {
        let kind = match kind {
            FfiFeedKind::Recommend => feeds::FeedKind::Recommend,
            FfiFeedKind::Following => feeds::FeedKind::Following,
        };
        Arc::new(Self {
            inner: Mutex::new(feeds::Engine::new(kind, cfg.to_core())),
        })
    }

    /// 冷启动把上一批摆出来。
    pub fn load_snapshot(
        &self,
        cached: Vec<FfiFeedRef>,
        platforms: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let items: Vec<feeds::FeedRef> = cached.iter().map(|r| r.to_core()).collect();
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.load_snapshot(&items, &platforms, &cfg, now_ms))
    }

    pub fn ensure_loaded(
        &self,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.ensure_loaded(foreground, &sources, &cfg, now_ms))
    }

    pub fn on_background(&self, foreground: bool) {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        engine.on_background(foreground);
    }

    pub fn on_foreground(
        &self,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.on_foreground(foreground, &sources, &cfg, now_ms))
    }

    pub fn on_scrolled_to(
        &self,
        last_visible: i32,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.on_scrolled_to(last_visible, foreground, &sources, &cfg, now_ms))
    }

    pub fn on_open_detail(
        &self,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.on_open_detail(foreground, &sources, &cfg, now_ms))
    }

    pub fn refresh(
        &self,
        fresh: bool,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.refresh(fresh, foreground, &sources, &cfg, now_ms))
    }

    pub fn commit_prefetched(&self, cfg: FfiFeedBatchCfg, now_ms: i64) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.commit_prefetched(&cfg, now_ms))
    }

    pub fn refresh_done(&self, round: i64) {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        engine.refresh_done(round);
    }

    pub fn load_more(
        &self,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.load_more(foreground, &sources, &cfg, now_ms))
    }

    pub fn retry_platform(
        &self,
        platform: String,
        foreground: bool,
        sources: Vec<String>,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.retry_platform(&platform, foreground, &sources, &cfg, now_ms))
    }

    pub fn on_page(
        &self,
        round: u64,
        platform: String,
        outcome: FfiPageOutcome,
        cfg: FfiFeedBatchCfg,
        now_ms: i64,
    ) -> FfiPageStep {
        let outcome = match outcome {
            FfiPageOutcome::Ok { items, next } => feeds::PageOutcome::Ok {
                items: items.iter().map(|r| r.to_core()).collect(),
                next,
            },
            FfiPageOutcome::Failed {
                not_logged_in,
                describe,
            } => feeds::PageOutcome::Failed {
                not_logged_in,
                describe,
            },
        };
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        let step = engine.on_page(round, &platform, outcome, &cfg, now_ms);
        FfiPageStep {
            next: step.next,
            action: to_ffi_action(step.action),
        }
    }

    pub fn on_timeout(&self) {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        engine.on_timeout();
    }

    pub fn finish_round(&self, cfg: FfiFeedBatchCfg, now_ms: i64) -> FfiFeedAction {
        let cfg = to_core_batch_cfg(&cfg);
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.finish_round(&cfg, now_ms))
    }

    pub fn commit_round(&self, round: i64, now_ms: i64) -> FfiFeedAction {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.commit_round(round, now_ms))
    }

    pub fn abort_round(&self) {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        engine.abort_round();
    }

    pub fn drop_hidden(&self, kept_keys: Vec<String>) -> FfiFeedAction {
        let mut engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_action(engine.drop_hidden(&kept_keys))
    }

    pub fn screen_keys(&self) -> Vec<String> {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.screen_keys()
    }

    pub fn prefetched_keys(&self) -> Vec<String> {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.prefetched_keys()
    }

    pub fn origin_of(&self, key: String) -> Option<FfiFeedOrigin> {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.origin_of(&key).map(|o| FfiFeedOrigin {
            batch_id: o.batch_id,
            platform_rank: o.platform_rank,
        })
    }

    pub fn view(&self) -> FfiFeedView {
        let engine = self.inner.lock().expect("feed engine poisoned");
        to_ffi_view(engine.view())
    }

    pub fn round_id(&self) -> Option<u64> {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.round_id()
    }

    /// 超时那一行字（界面 / 错误表里的文案）。
    pub fn timeout_message(&self) -> String {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.config().timeout_message()
    }

    pub fn wait_ms(&self) -> i64 {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.config().wait_ms
    }

    pub fn refresh_min_animation_ms(&self) -> i64 {
        let engine = self.inner.lock().expect("feed engine poisoned");
        engine.config().refresh_min_animation_ms
    }
}

// ===========================================================================
// 搜索（FfiSearch*）
// ===========================================================================

/// 一次搜索请求的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSearchOutcome {
    Ok {
        items: Vec<FfiFeedRef>,
        next: Option<String>,
        page_size: i32,
    },
    Failed {
        describe: String,
    },
}

/// 一条要宿主去发的搜索请求。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchRequest {
    pub round: u64,
    pub platform: String,
    pub cursor: Option<String>,
}

/// 搜索引擎返回给宿主"这一趟要做什么"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchAction {
    pub requests: Vec<FfiSearchRequest>,
    pub screen_changed: bool,
    pub appended: Vec<String>,
    pub started: bool,
    pub first_batch_done: bool,
    pub log: Vec<String>,
}

/// 宿主抄回 Compose 状态用的快照。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchView {
    pub query: String,
    pub loading: bool,
    pub loading_more: bool,
    pub platforms: Vec<String>,
    pub enabled: Vec<String>,
    pub answered: i32,
    pub exhausted: bool,
    pub errors: Vec<FfiFeedErrorRow>,
}

fn to_ffi_search_action(action: search::Action) -> FfiSearchAction {
    FfiSearchAction {
        requests: action
            .requests
            .into_iter()
            .map(|r| FfiSearchRequest {
                round: r.round,
                platform: r.platform,
                cursor: r.cursor,
            })
            .collect(),
        screen_changed: action.screen_changed,
        appended: action.appended,
        started: action.started,
        first_batch_done: action.first_batch_done,
        log: action.log,
    }
}

fn to_ffi_search_view(view: search::View) -> FfiSearchView {
    FfiSearchView {
        query: view.query,
        loading: view.loading,
        loading_more: view.loading_more,
        platforms: view.platforms,
        enabled: view.enabled,
        answered: view.answered,
        exhausted: view.exhausted,
        errors: view
            .errors
            .into_iter()
            .map(|(platform, message)| FfiFeedErrorRow { platform, message })
            .collect(),
    }
}

/// 搜索页的状态机（一个 `SearchState` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiSearchState {
    inner: Mutex<search::SearchEngine>,
}

#[uniffi::export]
impl FfiSearchState {
    /// `page_budget` = 每家一次搜索最多翻几页（`SearchPlan.PAGE_BUDGET`）。
    #[uniffi::constructor]
    pub fn new(page_budget: i32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(search::SearchEngine::new(page_budget)),
        })
    }

    pub fn prepare_platforms(&self, sources: Vec<String>) -> FfiSearchAction {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.prepare_platforms(&sources))
    }

    pub fn search(&self, text: String, sources: Vec<String>, rules: FfiFeedRules) -> FfiSearchAction {
        let rules = rules.to_core();
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.search(&text, &sources, &rules))
    }

    pub fn first_batch_timeout(&self, first_batch_ms: i64) -> FfiSearchAction {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.first_batch_timeout(first_batch_ms))
    }

    pub fn next_batch_timeout(&self, next_batch_ms: i64) -> FfiSearchAction {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.next_batch_timeout(next_batch_ms))
    }

    pub fn on_result(
        &self,
        round: u64,
        platform: String,
        outcome: FfiSearchOutcome,
        rules: FfiFeedRules,
    ) -> FfiSearchAction {
        let outcome = match outcome {
            FfiSearchOutcome::Ok {
                items,
                next,
                page_size,
            } => search::SearchOutcome::Ok {
                items: items.iter().map(|r| r.to_core()).collect(),
                next,
                page_size,
            },
            FfiSearchOutcome::Failed { describe } => search::SearchOutcome::Failed { describe },
        };
        let rules = rules.to_core();
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.on_result(round, &platform, outcome, &rules))
    }

    pub fn load_more(&self, foreground: bool) -> FfiSearchAction {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.load_more(foreground))
    }

    pub fn toggle(&self, platform: String, sources: Vec<String>) -> FfiSearchAction {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_action(engine.toggle(&platform, &sources))
    }

    /// 关掉搜索页：取消在飞的请求、回到全亮。
    ///
    /// 不叫 `close`：UniFFI 给每个 `Object` 生成的 Kotlin 类都有一个 `close()`（释放句柄），
    /// 同名会撞成 "Conflicting overloads"。
    pub fn end_search(&self) {
        let mut engine = self.inner.lock().expect("search engine poisoned");
        engine.close();
    }

    pub fn screen_keys(&self) -> Vec<String> {
        let engine = self.inner.lock().expect("search engine poisoned");
        engine.screen_keys()
    }

    pub fn view(&self) -> FfiSearchView {
        let engine = self.inner.lock().expect("search engine poisoned");
        to_ffi_search_view(engine.view())
    }

    pub fn round_id(&self) -> u64 {
        let engine = self.inner.lock().expect("search engine poisoned");
        engine.round_id()
    }
}

// ===========================================================================
// 账号（FfiAcct*）
// ===========================================================================

/// 平台内的用户（只有平台 / 昵称 / 头像 / id / 主页）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctUser {
    /// 平台 id（`UserRef.platform`，写进 `app.accounts` 的那一个）。
    pub platform: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub url: Option<String>,
}

/// 一个平台账号现在的状态。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAcctStatus {
    Checking,
    LoggedIn { user: FfiAcctUser },
    LoggedOut { expired: bool },
    Failed { message: String },
}

fn to_core_user(user: &FfiAcctUser) -> accounts::UserRef {
    accounts::UserRef {
        platform: user.platform.clone(),
        id: user.id.clone(),
        name: user.name.clone(),
        avatar: user.avatar.clone(),
        url: user.url.clone(),
    }
}

fn to_ffi_user(user: accounts::UserRef) -> FfiAcctUser {
    FfiAcctUser {
        platform: user.platform,
        id: user.id,
        name: user.name,
        avatar: user.avatar,
        url: user.url,
    }
}

fn to_ffi_status(status: accounts::AccountStatus) -> FfiAcctStatus {
    match status {
        accounts::AccountStatus::Checking => FfiAcctStatus::Checking,
        accounts::AccountStatus::LoggedIn(user) => FfiAcctStatus::LoggedIn {
            user: to_ffi_user(user),
        },
        accounts::AccountStatus::LoggedOut { expired } => FfiAcctStatus::LoggedOut { expired },
        accounts::AccountStatus::Failed { message } => FfiAcctStatus::Failed { message },
    }
}

/// 平台 id → 状态（保持插入顺序）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctRow {
    pub platform: String,
    pub status: FfiAcctStatus,
}

/// 一次决策：要问哪几家 + 状态表变没变。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctPlan {
    pub queries: Vec<String>,
    pub status_changed: bool,
    pub checked_at_changed: bool,
}

fn to_ffi_plan(plan: accounts::Plan) -> FfiAcctPlan {
    FfiAcctPlan {
        queries: plan.queries.into_iter().map(|q| q.platform).collect(),
        status_changed: plan.status_changed,
        checked_at_changed: plan.checked_at_changed,
    }
}

/// 一次 `whoami()` 的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAcctWhoami {
    User { user: FfiAcctUser },
    NoUser,
    NotLoggedIn,
    Failed { message: String },
}

/// 账号页的状态机（一个 `AccountsState` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiAcctState {
    inner: Mutex<accounts::AccountEngine>,
}

#[uniffi::export]
impl FfiAcctState {
    #[uniffi::constructor]
    pub fn new(platforms: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(accounts::AccountEngine::new(&platforms)),
        })
    }

    /// 进页面：把上次存下来的摆出来、一个请求都不发。`text` 是 `store.get("app.accounts")` 的值。
    pub fn ensure_checked(&self, text: Option<String>) -> FfiAcctPlan {
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.ensure_checked(text.as_deref()))
    }

    /// 用户点了刷新：问全部。
    pub fn refresh(&self) -> FfiAcctPlan {
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.refresh())
    }

    /// 问一个平台（登录 / 退出 / 导入 / 同步都走它）。
    pub fn check(&self, platform: String) -> FfiAcctPlan {
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.check(&platform))
    }

    /// 别处报"没登录"：标过期、**不发请求**。
    pub fn mark_expired(
        &self,
        platform: String,
        has_login_cookies: bool,
        now_ms: i64,
    ) -> FfiAcctPlan {
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.mark_expired(&platform, has_login_cookies, now_ms))
    }

    /// 一次 `whoami()` 的结果。
    pub fn on_whoami(
        &self,
        platform: String,
        outcome: FfiAcctWhoami,
        has_login_cookies: bool,
        now_ms: i64,
    ) -> FfiAcctPlan {
        let outcome = match outcome {
            FfiAcctWhoami::User { user } => accounts::WhoamiOutcome::User(to_core_user(&user)),
            FfiAcctWhoami::NoUser => accounts::WhoamiOutcome::NoUser,
            FfiAcctWhoami::NotLoggedIn => accounts::WhoamiOutcome::NotLoggedIn,
            FfiAcctWhoami::Failed { message } => accounts::WhoamiOutcome::Failed { message },
        };
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.on_whoami(&platform, outcome, has_login_cookies, now_ms))
    }

    /// 退出登录：先把确定的"未登录"存下来。
    pub fn logged_out(&self, platform: String, now_ms: i64) -> FfiAcctPlan {
        let mut engine = self.inner.lock().expect("account engine poisoned");
        to_ffi_plan(engine.logged_out(&platform, now_ms))
    }

    /// 内存里那份存档编码成 `store.put("app.accounts", …)` 的文本（宿主去落盘）。
    pub fn encode_store(&self) -> String {
        let engine = self.inner.lock().expect("account engine poisoned");
        engine.encode_store()
    }

    pub fn status(&self) -> Vec<FfiAcctRow> {
        let engine = self.inner.lock().expect("account engine poisoned");
        engine
            .status()
            .iter()
            .map(|(platform, status)| FfiAcctRow {
                platform: platform.clone(),
                status: to_ffi_status(status.clone()),
            })
            .collect()
    }

    pub fn checked_at(&self) -> Option<i64> {
        let engine = self.inner.lock().expect("account engine poisoned");
        engine.checked_at()
    }
}

/// 存档里那一项的键（宿主落盘用；和 `app.settings` 同一层）。
#[uniffi::export]
pub fn acct_store_key() -> String {
    accounts::STORE_KEY.to_string()
}

/// 存档格式版本（现在只会写 1）。
#[uniffi::export]
pub fn acct_schema() -> i32 {
    accounts::SCHEMA
}

/// `Platform.toStored`：写进存储的写法（老的 11 个平台写 enum 名）。
#[uniffi::export]
pub fn acct_platform_to_stored(platform: String) -> String {
    accounts::to_stored(&platform).to_string()
}

/// `Platform.fromStored`：存储里的写法 → 平台 id。
#[uniffi::export]
pub fn acct_platform_from_stored(text: String) -> String {
    accounts::from_stored(&text)
}

// ===========================================================================
// 关注管理
// ===========================================================================

/// 可关注的对象。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctFollowTarget {
    pub platform: String,
    /// `USER` / `FORUM`。
    pub kind: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

impl FfiAcctFollowTarget {
    fn to_core(&self) -> following::FollowTarget {
        following::FollowTarget {
            platform: self.platform.clone(),
            kind: self.kind.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            avatar: self.avatar.clone(),
            description: self.description.clone(),
            url: self.url.clone(),
        }
    }
}

fn to_ffi_target(target: following::FollowTarget) -> FfiAcctFollowTarget {
    FfiAcctFollowTarget {
        platform: target.platform,
        kind: target.kind,
        id: target.id,
        name: target.name,
        avatar: target.avatar,
        description: target.description,
        url: target.url,
    }
}

/// 一页关注列表的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAcctListOutcome {
    Ok {
        items: Vec<FfiAcctFollowTarget>,
        next: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// 取关的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAcctUnfollowOutcome {
    Ok,
    Failed { message: String },
}

/// 关注管理返回给宿主"这一趟要做什么"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctFollowAction {
    /// 要不要去取一页（**单独一个布尔**：UniFFI 会把 `Option<Option<String>>` 压平，
    /// `Some(None)` 和 `None` 到 Kotlin 都是 `null`，分不出"取第一页"和"不发请求"）。
    pub wants_load: bool,
    /// 取哪一页；`None` = 第一页（只有 `wants_load` 为真时才有意义）。
    pub load_cursor: Option<String>,
    /// 去取关这一个（**只有确认之后才可能非空**）。
    pub unfollow: Option<FfiAcctFollowTarget>,
    pub items_changed: bool,
    pub started: bool,
}

/// 宿主抄回 Compose 状态用的快照。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAcctFollowView {
    pub loading: bool,
    pub loaded: bool,
    pub has_more: bool,
    pub error: Option<String>,
    pub action_error: Option<String>,
    pub busy: Vec<String>,
    pub pending_unfollow: Option<FfiAcctFollowTarget>,
    pub item_count: u32,
}

fn to_ffi_follow_action(action: following::Action) -> FfiAcctFollowAction {
    FfiAcctFollowAction {
        wants_load: action.load.is_some(),
        load_cursor: action.load.flatten(),
        unfollow: action.unfollow.map(to_ffi_target),
        items_changed: action.items_changed,
        started: action.started,
    }
}

fn to_ffi_follow_view(view: following::View) -> FfiAcctFollowView {
    FfiAcctFollowView {
        loading: view.loading,
        loaded: view.loaded,
        has_more: view.has_more,
        error: view.error,
        action_error: view.action_error,
        busy: view.busy,
        pending_unfollow: view.pending_unfollow.map(to_ffi_target),
        item_count: view.item_count as u32,
    }
}

/// 关注管理的状态机（一个 `FollowingState` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiAcctFollowState {
    inner: Mutex<following::FollowEngine>,
}

#[uniffi::export]
impl FfiAcctFollowState {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(following::FollowEngine::new()),
        })
    }

    pub fn ensure_loaded(&self) -> FfiAcctFollowAction {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.ensure_loaded())
    }

    pub fn refresh(&self) -> FfiAcctFollowAction {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.refresh())
    }

    pub fn load_more(&self) -> FfiAcctFollowAction {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.load_more())
    }

    pub fn retry(&self) -> FfiAcctFollowAction {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.retry())
    }

    pub fn on_list(&self, outcome: FfiAcctListOutcome) -> FfiAcctFollowAction {
        let outcome = match outcome {
            FfiAcctListOutcome::Ok { items, next } => following::ListOutcome::Ok {
                items: items.iter().map(|t| t.to_core()).collect(),
                next,
            },
            FfiAcctListOutcome::Failed { message } => following::ListOutcome::Failed { message },
        };
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.on_list(outcome))
    }

    /// 用户点了"取关"（**还没确认**）：界面弹确认框。
    pub fn ask_unfollow(&self, target: FfiAcctFollowTarget) {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        engine.ask_unfollow(target.to_core());
    }

    pub fn cancel_unfollow(&self) {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        engine.cancel_unfollow();
    }

    /// 确认框里点了"取消关注"：**唯一**会让宿主真的调 `setFollowing(target, false)` 的入口。
    pub fn confirm_unfollow(&self) -> FfiAcctFollowAction {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.confirm_unfollow())
    }

    pub fn on_unfollow(
        &self,
        target: FfiAcctFollowTarget,
        outcome: FfiAcctUnfollowOutcome,
    ) -> FfiAcctFollowAction {
        let outcome = match outcome {
            FfiAcctUnfollowOutcome::Ok => following::UnfollowOutcome::Ok,
            FfiAcctUnfollowOutcome::Failed { message } => {
                following::UnfollowOutcome::Failed { message }
            }
        };
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_action(engine.on_unfollow(&target.to_core(), outcome))
    }

    pub fn dismiss_action_error(&self) {
        let mut engine = self.inner.lock().expect("follow engine poisoned");
        engine.dismiss_action_error();
    }

    pub fn items(&self) -> Vec<FfiAcctFollowTarget> {
        let engine = self.inner.lock().expect("follow engine poisoned");
        engine.items().iter().cloned().map(to_ffi_target).collect()
    }

    pub fn view(&self) -> FfiAcctFollowView {
        let engine = self.inner.lock().expect("follow engine poisoned");
        to_ffi_follow_view(engine.view())
    }
}

// ===========================================================================
// 预加载 / 曝光 / 混排（纯函数）
// ===========================================================================

/// 信息流触发点用几屏（插电 / 电池）。
#[uniffi::export]
pub fn feed_preload_feed_screens(plugged: bool) -> f32 {
    preload::feed_screens(plugged)
}

/// 图片预取用几屏（插电 / 电池）。
#[uniffi::export]
pub fn feed_preload_image_screens(plugged: bool) -> f32 {
    preload::image_screens(plugged)
}

/// 最后一张可见卡片是 `last_visible`、一共 `total` 张、一屏 `per_screen` 张：剩的不到 `screens` 屏了吗。
#[uniffi::export]
pub fn feed_preload_near_end(
    last_visible: i32,
    total: i32,
    per_screen: i32,
    screens: f32,
) -> bool {
    preload::near_end(last_visible, total, per_screen, screens)
}

/// 要预取图片的卡片下标区间（`start`..`end` **闭区间**；`end < start` = 空）。
#[uniffi::export]
pub fn feed_preload_range(
    last_visible: i32,
    total: i32,
    per_screen: i32,
    screens: f32,
) -> Vec<i32> {
    let (start, end) = preload::prefetch_range(last_visible, total, per_screen, screens);
    if end < start {
        Vec::new()
    } else {
        (start..=end).collect()
    }
}

/// Coil 内存缓存的上限：插电放大，电池用默认值。
#[uniffi::export]
pub fn feed_preload_memory_cache_bytes(plugged: bool, initial: i64) -> i64 {
    preload::memory_cache_bytes(plugged, initial)
}

/// 信息流"该取下一页了吗"的状态机（一个 `FeedPreloadTrigger` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiFeedPreloadTrigger {
    inner: Mutex<preload::FeedPreloadTrigger>,
}

#[uniffi::export]
impl FfiFeedPreloadTrigger {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(preload::FeedPreloadTrigger::new()),
        })
    }

    /// 这一帧**该不该**发起一次 `loadMore`（宿主还没真的发）。
    ///
    /// 两步走：先问这个，宿主真的发起成功（`loadMore()` 返回 true）之后再调
    /// [`note_started`](Self::note_started)——"发起"是宿主的协程，Rust 里没有回调。
    pub fn should_start(
        &self,
        last_visible: i32,
        total: i32,
        per_screen: i32,
        screens: f32,
        loading: bool,
    ) -> bool {
        let trigger = self.inner.lock().expect("preload trigger poisoned");
        trigger.should_start(last_visible, total, per_screen, screens, loading)
    }

    /// 宿主真的发起了一次：记下这次的位置（用户不动就不再取）。
    pub fn note_started(&self, last_visible: i32) {
        let mut trigger = self.inner.lock().expect("preload trigger poisoned");
        trigger.note_started(last_visible);
    }

    pub fn reset(&self) {
        let mut trigger = self.inner.lock().expect("preload trigger poisoned");
        trigger.reset();
    }
}

/// 一条卡片落在视口里的高度比例（0~1）。
#[uniffi::export]
pub fn feed_visible_fraction(
    item_top: i32,
    item_height: i32,
    viewport_top: i32,
    viewport_bottom: i32,
) -> f32 {
    exposure::visible_fraction(item_top, item_height, viewport_top, viewport_bottom)
}

/// 一个可见项在网格里的位置（下标 / 顶边 / 高度）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiVisibleEntry {
    pub index: i32,
    pub top: i32,
    pub height: i32,
}

/// 一条内容的最小身份（曝光只看 key 和平台）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFeedRefKey {
    pub key: String,
    pub platform: String,
}

/// 这一刻屏幕上的一条：身份 + 可见比例。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiVisibleItem {
    pub key: String,
    pub platform: String,
    pub visible_fraction: f32,
}

/// 从"哪些下标在屏幕上、各自在哪、多高"算出可见的卡片。
#[uniffi::export]
pub fn feed_visible_items(
    entries: Vec<FfiVisibleEntry>,
    items: Vec<Option<FfiFeedRefKey>>,
    viewport_top: i32,
    viewport_bottom: i32,
) -> Vec<FfiVisibleItem> {
    let entries: Vec<exposure::VisibleEntry> = entries
        .into_iter()
        .map(|e| exposure::VisibleEntry {
            index: e.index,
            top: e.top,
            height: e.height,
        })
        .collect();
    let items: Vec<Option<exposure::FeedRef>> = items
        .into_iter()
        .map(|o| {
            o.map(|k| exposure::FeedRef {
                key: k.key,
                platform: k.platform,
            })
        })
        .collect();
    exposure::visible_items(&entries, &items, viewport_top, viewport_bottom)
        .into_iter()
        .map(to_ffi_visible_item)
        .collect()
}

/// **行式**网格里现在可见的卡片（整行的可见比例当作行内每张卡片的）。
#[uniffi::export]
pub fn feed_related_visible_items(
    entries: Vec<FfiVisibleEntry>,
    rows: Vec<Vec<FfiFeedRefKey>>,
    viewport_top: i32,
    viewport_bottom: i32,
) -> Vec<FfiVisibleItem> {
    let entries: Vec<exposure::VisibleEntry> = entries
        .into_iter()
        .map(|e| exposure::VisibleEntry {
            index: e.index,
            top: e.top,
            height: e.height,
        })
        .collect();
    let rows: Vec<Vec<exposure::FeedRef>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|k| exposure::FeedRef {
                    key: k.key,
                    platform: k.platform,
                })
                .collect()
        })
        .collect();
    exposure::related_visible_items(&entries, &rows, viewport_top, viewport_bottom)
        .into_iter()
        .map(to_ffi_visible_item)
        .collect()
}

fn to_ffi_visible_item(item: exposure::VisibleItem) -> FfiVisibleItem {
    FfiVisibleItem {
        key: item.key,
        platform: item.platform,
        visible_fraction: item.visible_fraction,
    }
}

/// 行为数据的两个入参：设置开关 → `enabled`，被盖住 / 失焦 → `foreground = false`（结算，不是丢弃）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiExposureInputs {
    pub enabled: bool,
    pub foreground: bool,
}

#[uniffi::export]
pub fn feed_exposure_inputs(
    record_behavior: bool,
    covered: bool,
    window_focused: bool,
) -> FfiExposureInputs {
    let inputs = exposure::exposure_inputs(record_behavior, covered, window_focused);
    FfiExposureInputs {
        enabled: inputs.enabled,
        foreground: inputs.foreground,
    }
}

/// 一次结算要写的那几行。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiExposureSettle {
    pub key: String,
    pub platform: String,
    pub started_at_ms: i64,
    pub dwell_ms: i64,
    pub last_seen_at_ms: i64,
}

fn to_ffi_settle(settle: exposure::Settle) -> FfiExposureSettle {
    FfiExposureSettle {
        key: settle.key,
        platform: settle.platform,
        started_at_ms: settle.started_at_ms,
        dwell_ms: settle.dwell_ms,
        last_seen_at_ms: settle.last_seen_at_ms,
    }
}

/// 曝光的判定和停留计时（一个 `ExposureTracker` 建一个）。
#[derive(uniffi::Object)]
pub struct FfiExposureTracker {
    inner: Mutex<exposure::ExposureTracker>,
}

#[uniffi::export]
impl FfiExposureTracker {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(exposure::ExposureTracker::new()),
        })
    }

    /// 界面每隔 `EXPOSURE_TICK_MS` 喂一次"现在可见的卡片"，返回当前要结算的行。
    pub fn update(
        &self,
        visible: Vec<FfiVisibleItem>,
        enabled: bool,
        foreground: bool,
        now_ms: i64,
    ) -> Vec<FfiExposureSettle> {
        let visible: Vec<exposure::VisibleItem> = visible
            .into_iter()
            .map(|v| exposure::VisibleItem {
                key: v.key,
                platform: v.platform,
                visible_fraction: v.visible_fraction,
            })
            .collect();
        let mut tracker = self.inner.lock().expect("exposure tracker poisoned");
        tracker
            .update(&visible, enabled, foreground, now_ms)
            .into_iter()
            .map(to_ffi_settle)
            .collect()
    }

    /// 离开组合时调：把还在跟踪的按当前时刻结算。
    pub fn settle_all(&self, now_ms: i64) -> Vec<FfiExposureSettle> {
        let mut tracker = self.inner.lock().expect("exposure tracker poisoned");
        tracker.settle_all(now_ms).into_iter().map(to_ffi_settle).collect()
    }

    pub fn tracked_count(&self) -> u64 {
        let tracker = self.inner.lock().expect("exposure tracker poisoned");
        tracker.tracked_count() as u64
    }
}

/// 曝光门槛：可见比例。
#[uniffi::export]
pub fn feed_exposure_min_visible_fraction() -> f32 {
    exposure::EXPOSURE_MIN_VISIBLE_FRACTION
}

/// 曝光门槛：连续可见多久（毫秒）。
#[uniffi::export]
pub fn feed_exposure_min_visible_ms() -> i64 {
    exposure::EXPOSURE_MIN_VISIBLE_MS
}

/// 界面多久喂一次（毫秒）。
#[uniffi::export]
pub fn feed_exposure_tick_ms() -> i64 {
    exposure::EXPOSURE_TICK_MS
}

/// 混排原型用的一条（`mixBatch` 的候选）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixFeedItem {
    pub key: String,
    pub platform: String,
    pub published_at: Option<i64>,
}

/// 比例配额的参数。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixQuotaConfig {
    pub batch_target: i32,
    /// 平台 → 权重（保持顺序）。
    pub weights: Vec<FfiMixWeight>,
    pub floor_per_platform: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixWeight {
    pub platform: String,
    pub weight: f32,
}

/// 个性化**过滤**原型的参数（默认关；见 `feed/mix.rs` 的说明）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixPersonalization {
    pub enabled: bool,
    pub min_positives: i32,
    pub min_positives_per_platform: i32,
    pub threshold: f32,
    pub exploration_ratio: f32,
    pub max_drop_ratio: f32,
}

/// 一批怎么筛。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixConfig {
    pub quota: FfiMixQuotaConfig,
    pub personalization: FfiMixPersonalization,
}

/// 行为摘要。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixBehavior {
    pub positives: i32,
    pub positives_by_platform: Vec<FfiMixCount>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixCount {
    pub platform: String,
    pub count: i32,
}

/// 一批筛完的结果（`order` 是输入下标）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixResult {
    pub order: Vec<u32>,
    pub dropped_low_score: u32,
    pub dropped_by_quota: u32,
    pub per_platform: Vec<FfiMixCount>,
    pub short_of_target: bool,
}

fn to_core_quota(cfg: &FfiMixQuotaConfig) -> mix::QuotaConfig {
    mix::QuotaConfig {
        batch_target: cfg.batch_target,
        weights: cfg
            .weights
            .iter()
            .map(|w| (w.platform.clone(), w.weight))
            .collect(),
        floor_per_platform: cfg.floor_per_platform,
    }
}

fn to_core_mix_config(cfg: &FfiMixConfig) -> mix::MixConfig {
    mix::MixConfig {
        quota: to_core_quota(&cfg.quota),
        personalization: mix::PersonalizationConfig {
            enabled: cfg.personalization.enabled,
            min_positives: cfg.personalization.min_positives,
            min_positives_per_platform: cfg.personalization.min_positives_per_platform,
            threshold: cfg.personalization.threshold,
            exploration_ratio: cfg.personalization.exploration_ratio,
            max_drop_ratio: cfg.personalization.max_drop_ratio,
        },
    }
}

fn to_core_mix_items(items: &[FfiMixFeedItem]) -> Vec<mix::MixFeedItem> {
    items
        .iter()
        .map(|i| mix::MixFeedItem {
            key: i.key.clone(),
            platform: i.platform.clone(),
            published_at: i.published_at,
        })
        .collect()
}

/// 每个平台留几条（水位法）。`sizes` 的键顺序就是返回值顺序。
#[uniffi::export]
pub fn feed_mix_quotas(
    sizes: Vec<FfiMixCount>,
    target: i32,
    weights: Vec<FfiMixWeight>,
    floor_per_platform: i32,
) -> Vec<FfiMixCount> {
    let sizes: Vec<(String, i32)> = sizes
        .into_iter()
        .map(|c| (c.platform, c.count))
        .collect();
    let weights: Vec<(String, f32)> = weights.into_iter().map(|w| (w.platform, w.weight)).collect();
    mix::quotas(&sizes, target, &weights, floor_per_platform)
        .into_iter()
        .map(|(platform, count)| FfiMixCount { platform, count })
        .collect()
}

/// 把一批候选筛成上屏的一批（`mixBatch` 原型）。
#[uniffi::export]
pub fn feed_mix_batch(
    candidates: Vec<FfiMixFeedItem>,
    behavior: FfiMixBehavior,
    config: FfiMixConfig,
    scores: Vec<Option<f32>>,
) -> FfiMixResult {
    let items = to_core_mix_items(&candidates);
    let behavior = mix::BehaviorSummary {
        positives: behavior.positives,
        positives_by_platform: behavior
            .positives_by_platform
            .into_iter()
            .map(|c| (c.platform, c.count))
            .collect(),
    };
    let cfg = to_core_mix_config(&config);
    let result = mix::mix_batch(&items, &behavior, &cfg, Some(&scores));
    FfiMixResult {
        order: result.order,
        dropped_low_score: result.dropped_low_score,
        dropped_by_quota: result.dropped_by_quota,
        per_platform: result
            .per_platform
            .into_iter()
            .map(|(platform, count)| FfiMixCount {
                platform,
                count: count as i32,
            })
            .collect(),
        short_of_target: result.short_of_target,
    }
}
