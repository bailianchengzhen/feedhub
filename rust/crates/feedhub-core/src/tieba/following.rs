//! 贴吧**关注流**的整条链路（Kotlin `TiebaSource.following` 的 Rust 版）：
//! 吧列表缓存（一天 TTL）→ 哪些吧该重抓第一页（`ForumFreshness`）→ 惰性 k 路归并（`ForumMerger`）
//! → 真的发请求（`TiebaHostedSource::frs_page`）→ 存盘（`ForumStore`）。
//!
//! 逐条对照 Kotlin：
//!
//! | 这里 | Kotlin |
//! |---|---|
//! | [`FollowingFeed::page`] | `TiebaSource.following` |
//! | [`FollowingFeed::sync_forums`] | `TiebaSource.syncFollowedForums` |
//! | [`HostedPager`] | `TiebaSource.FrsPager` + `frsPageBytes` |
//! | [`crate::tieba::forum::ForumMerger`] | `ForumMerger` |
//! | [`crate::tieba::forum::due`] / `is_due` | `ForumFreshness` |
//! | [`crate::tieba::forum::ForumStore`] | `ForumStore` |
//!
//! 三条口径要保住：
//! 1. **首屏只花一个有上限的时间预算刷新 due 的吧**（[`HEAD_REFRESH_BUDGET_MS`]）：每发一个请求前看一眼，
//!    到点不再发新的；已发出的那个会跑完，剩下的吧下次刷新接着来（避免几十个吧串行把首屏吃满）；
//! 2. **中途出错也把已经抓到的存下来**（Kotlin 的 `finally`）：Rust 里是"先把结果收进局部变量、
//!    统一落盘、再把错误抛出去"；
//! 3. **纯缓存翻页（一个请求都没发）不重写整份缓存**（省一次几 MB 的 gzip + 写盘）。

use std::collections::HashMap;

use crate::model::BiliError;
use crate::tieba::forum::{
    due, ForumList, ForumMerger, ForumPager, ForumPage, ForumState, ForumStore, PagerError, ThreadKey,
    FollowedForum,
};
use crate::tieba::source::{TiebaHostedSource, FRS_PAGE_SIZE};

/// 吧列表缓存的有效期（Kotlin `FORUM_LIST_TTL_MS`）。
pub const FORUM_LIST_TTL_MS: i64 = 24 * 3_600_000;
/// 首屏刷新第一页的时间预算（Kotlin `HEAD_REFRESH_BUDGET_MS`）。
pub const HEAD_REFRESH_BUDGET_MS: i64 = 2_500;
/// 关注流一页几条（Kotlin `FOLLOWING_PAGE_SIZE`）。
pub const FOLLOWING_PAGE_SIZE: usize = 20;

/// 关注流要的两件宿主能力：抓某个吧的一页 + 取关注的吧列表。
///
/// 生产只有一份实现（[`HostedPager`]）；测试用脚本回放（`following_tests.rs`），
/// 所以整条链路（调度 + 归并 + 存盘）能离线跑。
pub trait ForumSource: ForumPager {
    /// 关注的吧（Kotlin `fetchFollowedForums`）。平台错误已按 `classify_error` 归类过；
    /// `is_login == 0` → `NotLoggedIn`。
    fn forums(&self) -> Result<Vec<FollowedForum>, BiliError>;
}

/// [`TiebaHostedSource::frs_page`] 的 [`ForumPager`] 实现（Kotlin `FrsPager`）。
///
/// 错误归类照 Kotlin：风控 / 没登录 / 签名错是**全局问题**、原样抛；其余（例如 340001"该吧神秘升级中"）
/// 只是这个吧读不了 → [`PagerError::Closed`]（合并器会把它关 24 小时再继续下一个）。
pub struct HostedPager<'a> {
    source: &'a TiebaHostedSource,
    now_ms: i64,
}

impl<'a> HostedPager<'a> {
    pub fn new(source: &'a TiebaHostedSource, now_ms: i64) -> Self {
        Self { source, now_ms }
    }
}

impl ForumPager for HostedPager<'_> {
    fn page_size(&self) -> i32 {
        FRS_PAGE_SIZE
    }

    fn fetch(&self, forum: &ForumState, page: i32) -> Result<ForumPage, PagerError> {
        let feed = self
            .source
            .frs_page(&forum.name, page, self.now_ms)
            .map_err(PagerError::Fatal)?;
        if let Some(error) = feed.error {
            let classified = crate::tieba::source::classify_error(error.code, &error.error_msg);
            return Err(match classified {
                // 风控、没登录、签名错是全局问题，原样抛
                BiliError::RiskControl { .. }
                | BiliError::NotLoggedIn { .. }
                | BiliError::Signature { .. } => PagerError::Fatal(classified),
                // 其余（例如 340001）只是这个吧读不了
                other => PagerError::Closed(crate::tieba::forum::ForumClosed {
                    message: format!(
                        "{}吧：{}",
                        forum.name,
                        match other {
                            BiliError::Api { detail, .. } => detail,
                            e => e.to_string(),
                        }
                    ),
                }),
            });
        }
        Ok(ForumPage {
            items: feed.items,
            last: feed.last,
        })
    }
}

impl ForumSource for HostedPager<'_> {
    fn forums(&self) -> Result<Vec<FollowedForum>, BiliError> {
        fetch_followed_forums(self.source)
    }
}

/// 关注流的进程内状态：各吧缓冲（第一次用时从 `ctx.store` 读）。
///
/// 和 Kotlin `TiebaSource.followingStates` 一样是**进程内一份**（不每页重读）；存盘只在这条链路的
/// 「首屏」或「当前真的发过请求」时发生。
pub struct FollowingFeed {
    states: Option<HashMap<i64, ForumState>>,
}

impl FollowingFeed {
    pub fn new() -> Self {
        Self { states: None }
    }

    /// 手上那份缓冲（排障 / 测试用；没读过就是 `None`）。
    pub fn states(&self) -> Option<&HashMap<i64, ForumState>> {
        self.states.as_ref()
    }

    /// 关注流一页（Kotlin `TiebaSource.following`）。
    ///
    /// `before` 是上一页最后一条的 `"<秒>:<tid>"`（[`ThreadKey::encode`]）：`None` = 刷新（会联网），
    /// 非空 = **只在缓存里继续往下排**（不联网，除非缓冲里那段真的不够了）。
    ///
    /// `pager` 是"抓某个吧的一页"，`forums()` 是"关注的吧列表"——生产用 [`HostedPager`]
    /// （真的发 `frs/page` / `forumGuide`），测试用脚本回放。
    pub fn page(
        &mut self,
        store: &ForumStore,
        pager: &dyn ForumSource,
        before: Option<ThreadKey>,
        now: i64,
    ) -> Result<MergeResult, BiliError> {
        // 第一次用：从 `ctx.store` 读一次（读不出来 = 空）
        if self.states.is_none() {
            self.states = Some(store.load_buffers());
        }
        let mut states = self.states.take().unwrap_or_default();
        let mut merger = ForumMerger::new(pager);
        let result = (|| -> Result<MergeResult, BiliError> {
            self.sync_forums(store, pager, &mut states, now, before.is_none())?;
            if before.is_none() {
                // 首屏只花一个有上限的时间预算刷新 due 的吧
                let ids = due(&ordered(&states), now);
                merger.reset_stats();
                merger
                    .refresh_heads(&mut states, &ids, || now, now + HEAD_REFRESH_BUDGET_MS)?;
            }
            let page = merger
                .next_page(&mut states, before, FOLLOWING_PAGE_SIZE, || now)?;
            let next = if page.more {
                page.items
                    .last()
                    .map(|item| crate::tieba::forum::thread_key(item).encode())
            } else {
                None
            };
            Ok(MergeResult {
                items: page.items,
                next,
            })
        })();
        // 中途出错（风控、断网）时已经抓到的也存下来；纯缓存翻页（一个请求都没发）不重写整份缓存。
        if before.is_none() || merger.fetches > 0 {
            store.save_buffers(&ordered(&states));
        }
        self.states = Some(states);
        result
    }

    /// 让 `states` 和关注的吧列表一致：新关注的加进来，取关的删掉，吧名和今日新帖数更新。
    ///
    /// 列表缓存一天；`allow_refresh` 为 false（翻页）时只要有缓存就不联网。
    ///
    /// **只有网络 / 代理不可用**才回落旧缓存（Kotlin `catch (e: IOException)` 的口径）；
    /// 风控 / 没登录 / 签名错 / 平台错误原样往外抛，当前一个 `frs/page` 都不再发（风控红线）。
    fn sync_forums(
        &self,
        store: &ForumStore,
        pager: &dyn ForumSource,
        states: &mut HashMap<i64, ForumState>,
        now: i64,
        allow_refresh: bool,
    ) -> Result<(), BiliError> {
        let cached = store.load_forums();
        let stale = cached
            .as_ref()
            .map(|c| allow_refresh && now - c.at >= FORUM_LIST_TTL_MS)
            .unwrap_or(true);
        let list = if !stale {
            cached.expect("stale 为假说明有缓存")
        } else {
            match pager.forums() {
                Ok(forums) => {
                    let list = ForumList { at: now, forums };
                    store.save_forums(&list);
                    list
                }
                Err(e @ (BiliError::Network { .. } | BiliError::ProxyUnavailable { .. })) => {
                    match cached {
                        Some(c) => c,
                        None => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let ids: std::collections::HashSet<i64> = list.forums.iter().map(|f| f.id).collect();
        states.retain(|id, _| ids.contains(id));
        for f in &list.forums {
            match states.get(&f.id) {
                Some(old) => {
                    let next = ForumState {
                        name: f.name.clone(),
                        day_threads: f.day_threads,
                        ..old.clone()
                    };
                    states.insert(f.id, next);
                }
                None => {
                    states.insert(f.id, ForumState::new(f.id, f.name.clone(), f.day_threads));
                }
            }
        }
        Ok(())
    }
}

impl Default for FollowingFeed {
    fn default() -> Self {
        Self::new()
    }
}

/// 关注流一页的结果：条目 + 下一页游标（`None` = 到底了）。
#[derive(Debug, Clone, PartialEq)]
pub struct MergeResult {
    pub items: Vec<crate::model::FeedItem>,
    pub next: Option<String>,
}

/// 按 id 排好序的一份缓冲（存盘 / 调度的顺序要稳定）。
fn ordered(states: &HashMap<i64, ForumState>) -> Vec<ForumState> {
    let mut ids: Vec<i64> = states.keys().copied().collect();
    ids.sort_unstable();
    ids.iter().filter_map(|id| states.get(id).cloned()).collect()
}

/// 关注的吧（`forumGuide`）。`error` 非空时按平台错误码归类（`classify_error`），`is_login == 0` → 未登录。
fn fetch_followed_forums(source: &TiebaHostedSource) -> Result<Vec<FollowedForum>, BiliError> {
    let forums = source.forum_guide()?;
    if let Some(error) = forums.error {
        return Err(crate::tieba::source::classify_error(
            error.code,
            &error.error_msg,
        ));
    }
    if forums.is_login == Some(0) {
        return Err(BiliError::NotLoggedIn { detail: None });
    }
    Ok(forums
        .forums
        .iter()
        .map(FollowedForum::from_parsed)
        .collect())
}

