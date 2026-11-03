//! **小红书关注流的调度**（Kotlin `XhsFollowFeed` 的 Rust 版）：关注列表 TTL、谁该重拉、
//! 一次刷新的网络预算、extra 分批问"未看数"、出错时照样把已经拉到的存盘。
//!
//! 逐条对照 `source-xiaohongshu/.../XhsFollowFeed.kt`，一次刷新（游标为空）的顺序是：
//!
//! 1. **关注列表** `GET user/followings/self`：翻页取全（`has_more` 在最后一页之前就可能是 true，
//!    所以也看是不是空页 / 游标没变），存进 `ctx.store`，**一天刷新一次**；取不到但有旧缓存就用旧的
//!    （只对 `IOException` 这样，其它错误照抛）。
//! 2. **先拉一部分作者的第一页** `GET user_posted`（每次最多 [`FollowConfig::max_authors`] 个，
//!    剩下的下次刷新再拉）。某个作者的主页读不了（**接口报错**）就把他关 6 小时再继续，
//!    连着 [`FollowConfig::max_failures`] 个读不了就把错误抛出去。
//! 3. **谁有新笔记** `GET user/followings/extra`（逗号分隔，每批 [`FollowConfig::extra_batch`] 个）：
//!    两次之间至少隔 [`FollowConfig::extra_min_gap_ms`]，**只有第 2 步之后还有剩余预算才问**，
//!    没问完 `extraAt` 不推进。这一步出错（非风控）就不管它，只按"多久没拉"来选。
//! 4. **按发布时间归并**（[`crate::xhs::follow::next_page`]）。
//!
//! 翻页（游标非空）**不联网**，只在缓存里继续往下排。
//!
//! **风控（461 / 471 / 300011 / 300012）原样往外抛、不重试**；抛之前已经拉到的照常存盘
//! （Kotlin 的 `finally`，Rust 里是"先把结果收进局部变量、统一落盘、再把错误抛出去"）。
//!
//! 随机量 / 时钟：**一次刷新要发 N 个请求**，每个请求的签名随机量都不一样，所以由
//! [`crate::host::SignRandomHost`] 在"要发请求的那一刻"问宿主抽；时间由
//! [`crate::host::ClockHost`] 给。

use std::collections::HashSet;

use crate::host::{ClockHost, SelfIdHost, SignRandomHost};
use crate::model::{BiliError, FeedItem, Page};
use crate::xhs::follow::{
    due, integrate, note_key, next_page, AuthorState, Authors, FollowStore, FollowedAuthor, NoteKey,
    UserList, KEEP_PER_AUTHOR, PAGE_SIZE,
};
use crate::xhs::source::XhsHostedSource;
use crate::xhs::{self};

/// `followings/self` 的键名（和 Kotlin `XhsFollowFeed` 的 companion 一样）。
pub const LIST_TTL_MS: i64 = 24 * 3_600_000;
pub const EXTRA_MIN_GAP_MS: i64 = 120_000;
pub const EXTRA_BATCH: usize = 10;
pub const MAX_LIST_PAGES: usize = 30;
pub const MAX_FAILURES: usize = 3;
pub const CLOSED_FOR_MS: i64 = 6 * 3_600_000;
pub const HEAD_REFRESH_BUDGET_MS: i64 = 2_000;
pub const MAX_AUTHORS_PER_REFRESH: usize = 10;

/// 一次刷新的可调参数（生产用 [`Self::default`]，测试里注入小预算 / 少作者）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowConfig {
    pub page_size: usize,
    pub max_authors: usize,
    pub keep_per_author: usize,
    pub list_ttl_ms: i64,
    pub extra_min_gap_ms: i64,
    pub extra_batch: usize,
    pub max_list_pages: usize,
    pub max_failures: usize,
    pub closed_for_ms: i64,
    pub refresh_budget_ms: i64,
}

impl Default for FollowConfig {
    fn default() -> Self {
        Self {
            page_size: PAGE_SIZE,
            max_authors: MAX_AUTHORS_PER_REFRESH,
            keep_per_author: KEEP_PER_AUTHOR,
            list_ttl_ms: LIST_TTL_MS,
            extra_min_gap_ms: EXTRA_MIN_GAP_MS,
            extra_batch: EXTRA_BATCH,
            max_list_pages: MAX_LIST_PAGES,
            max_failures: MAX_FAILURES,
            closed_for_ms: CLOSED_FOR_MS,
            refresh_budget_ms: HEAD_REFRESH_BUDGET_MS,
        }
    }
}

/// 一次调度的"外部输入"：钟、随机源、自己的 user_id。
///
/// `self_id` 由 Kotlin 算好传进来（`whoami` 没搬进 Rust）：`xhs.user_id` 有就直接用，
/// 没有的话 Kotlin 侧先调一次 `whoami` 拿到（位置和 Kotlin 参照实现里的 `selfId()` 一样）。
pub struct FollowDrive<'a> {
    pub clock: &'a dyn ClockHost,
    pub random: &'a dyn SignRandomHost,
    /// 自己的 user_id（Kotlin 算好的）。给了就不问 [`Self::self_id_lazy`]。
    pub self_id: Option<String>,
    /// **惰性**的 user_id：`self_id` 没给时才问（Kotlin 那边"没有存过就先问一次 `user/me`"）。
    ///
    /// 为什么不做成"调用前先算好"：那一步**可能要发一个请求**（`user/me`），而 Kotlin 参照实现
    /// 只在真要拉关注列表时才问——翻页 / 缓存还新鲜的那一轮它一个请求都不发，这里也必须一样。
    pub self_id_lazy: Option<&'a dyn SelfIdHost>,
}



impl FollowDrive<'_> {
    fn now(&self) -> i64 {
        self.clock.now_ms()
    }
}

/// 一页关注流。
#[derive(Debug)]
pub struct FollowPage {
    pub page: Page,
    /// 这次刷新有没有真的联网（排障 / 测试看这个）。
    pub refreshed: bool,
}

impl XhsHostedSource {
    /// 关注流一页（`XhsSource.feed(FOLLOWING)` 的实现）。
    pub fn follow_page(
        &self,
        cursor: Option<&str>,
        drive: &FollowDrive<'_>,
        config: &FollowConfig,
    ) -> Result<FollowPage, BiliError> {
        // 1. 游标先解：无效游标在发任何请求之前就报错（Kotlin `NoteKey.decode ?: throw`）
        let before = match cursor {
            Some(text) => Some(NoteKey::decode(text).ok_or_else(|| BiliError::Parse {
                detail: "关注流游标无效".to_string(),
            })?),
            None => None,
        };
        // 2. 登录态：`a1` + `web_session` 都要有（Kotlin `XhsApi.hasLogin()`），少一个就不发请求
        if !self.has_login() {
            return Err(BiliError::NotLoggedIn { detail: None });
        }

        // 3. Kotlin 的 `Mutex`：一次只跑一个页面（关注流可能被首页预取和用户下拉同时触发）
        let _serial = self.follow_lock();

        let mut state = self.cached_follow_state();
        let mut extra_at = state.extra_at;
        let mut dirty = false;
        let mut refreshed = false;

        // 4. 干活：**结果先收进变量**，落盘放在后面（= Kotlin 的 `finally`），最后才把错误抛出去
        let result = (|| -> Result<FollowPage, BiliError> {
            let synced = self.sync_users(&mut state, before.is_none(), drive, config)?;
            refreshed = synced.0;
            dirty = synced.1;
            if before.is_none() {
                let deadline = drive.now() + config.refresh_budget_ms;
                // 先拉作者：首刷 / 关注 >10 人时页面一定有内容；unread 是辅助信息，只有还剩预算才问
                let fetched = self.fetch_due(&mut state, deadline, drive, config)?;
                if drive.now() < deadline {
                    extra_at = self.refresh_unread(&mut state, extra_at, deadline, drive, config)?;
                    // unread 是在作者拉取之后拿到的：这轮拉过的作者要把当前 unread 记成"拉取时已看到"
                    for id in &fetched {
                        if let Some(s) = state.get(id) {
                            let unread = s.unread;
                            let next = s.copy_with(|x| x.unread_at_fetch = unread);
                            state.insert(next);
                        }
                    }
                }
                dirty = true;
            }
            let merge = next_page(&state.ordered(), before.as_ref(), config.page_size);
            let next = if merge.more {
                merge.items.last().map(|item| note_key(item).encode())
            } else {
                None
            };
            Ok(FollowPage {
                page: Page {
                    items: merge.items,
                    next,
                },
                refreshed,
            })
        })();

        // 5. Kotlin 的 `finally`：**中途出错（风控、断网）时已经拉到的也存下来**。
        //    纯缓存翻页（没联网、状态没变）不重写整份缓存。
        state.extra_at = extra_at;
        self.store_follow_state(state.clone());
        if before.is_none() || dirty {
            self.follow_persist(&state);
        }
        result
    }

    /// 关注流**手上那份状态**（排障 / 测试用，不发请求）。`None` = 还没读过。
    pub fn follow_state(&self) -> Option<Authors> {
        self.cached_follow_state_opt()
    }

    // -----------------------------------------------------------------------
    // 关注列表
    // -----------------------------------------------------------------------

    /// 让 `state` 和关注列表一致：新关注的加进来，取关的删掉，token 换新（token 变了，
    /// 之前"读不了"的记录作废）。列表缓存一天；`allow_refresh` 为 false（翻页）时只要有缓存就不联网。
    ///
    /// 联网失败（`IOException` = 网络 / 代理不可用）但有旧缓存时用旧的。
    /// 返回（这次是否联网取了关注列表，状态是否可能变了）。
    fn sync_users(
        &self,
        state: &mut Authors,
        allow_refresh: bool,
        drive: &FollowDrive<'_>,
        config: &FollowConfig,
    ) -> Result<(bool, bool), BiliError> {
        let cached = self.follow_load_users();
        let stale = match &cached {
            None => true,
            Some(list) => allow_refresh && drive.now() - list.at >= config.list_ttl_ms,
        };
        let mut refreshed = false;
        let list = if !stale {
            cached.expect("stale 为假时一定有缓存")
        } else {
            // Kotlin 是 `UserList(now(), fetchFollowings())`：**先取时刻、再发请求**
            let at = drive.now();
            match self.fetch_followings(drive, config) {
                Ok(users) => {
                    let list = UserList { at, users };
                    self.follow_save_users(&list);
                    refreshed = true;
                    list
                }
                Err(e) if is_transport_error(&e) => match cached {
                    Some(old) => old,
                    None => return Err(e),
                },
                Err(e) => return Err(e),
            }
        };
        let ids: HashSet<String> = list.users.iter().map(|u| u.id.clone()).collect();
        state.retain(&ids);
        for u in &list.users {
            let next = match state.get(&u.id) {
                None => AuthorState::new(u.id.clone(), u.name.clone(), u.xsec_token.clone()),
                Some(old) => {
                    let mut next = old.clone();
                    next.name = u.name.clone();
                    if old.xsec_token != u.xsec_token {
                        next.xsec_token = u.xsec_token.clone();
                        // token 换了：之前"读不了"的记录作废
                        next.closed_until = 0;
                    }
                    next
                }
            };
            state.insert(next);
        }
        Ok((refreshed, refreshed))
    }

    /// `followings/self` 翻页取全。`has_more` 最后一页之前就可能是 true，所以**同时看**是不是空页、
    /// 游标是不是没变（照 Kotlin）。
    fn fetch_followings(
        &self,
        drive: &FollowDrive<'_>,
        config: &FollowConfig,
    ) -> Result<Vec<FollowedAuthor>, BiliError> {
        let me = self.resolve_self_id(drive)?;
        let mut out: Vec<FollowedAuthor> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor = String::new();
        for _ in 0..config.max_list_pages {
            let page = self.followings_driven(&me, &cursor, drive)?;
            let users = page.users;
            let empty = users.is_empty();
            for u in users {
                // Kotlin 是 `LinkedHashMap.putIfAbsent`：先出现的为准
                if seen.insert(u.id.clone()) {
                    out.push(FollowedAuthor::from_parsed(&u));
                }
            }
            let next = page.cursor;
            let next_is_empty = next.as_deref().unwrap_or("").is_empty();
            if !page.has_more || empty || next_is_empty {
                break;
            }
            let next = next.unwrap_or_default();
            if next == cursor {
                break;
            }
            cursor = next;
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // 谁有新笔记
    // -----------------------------------------------------------------------

    /// 问 `followings/extra`，把未看数记进 `state`。返回这次问的时间（没问完 / 没问就是原来的值）。
    ///
    /// **只在 `fetch_due` 之后、预算还有剩余时调用**；它只影响下一轮 due 的排序，不该挤掉当前的作者。
    fn refresh_unread(
        &self,
        state: &mut Authors,
        extra_at: i64,
        deadline: i64,
        drive: &FollowDrive<'_>,
        config: &FollowConfig,
    ) -> Result<i64, BiliError> {
        let t = drive.now();
        if t - extra_at < config.extra_min_gap_ms {
            return Ok(extra_at);
        }
        let me = self.resolve_self_id(drive)?;
        let ids: Vec<String> = state
            .values()
            .filter(|a| !a.is_closed(t))
            .map(|a| a.id.clone())
            .collect();
        let batch_size = config.extra_batch.max(1);
        let mut asked = false;
        for batch in ids.chunks(batch_size) {
            // 到预算就停；extraAt 不往后推，下次刷新接着问
            if asked && drive.now() >= deadline {
                return Ok(extra_at);
            }
            asked = true;
            let unread = match self.unread_driven(&me, batch, drive) {
                Ok(map) => map,
                // Kotlin 只吞这两种：这个接口只是帮着挑人，出错了就按"多久没拉"来
                Err(BiliError::Api { .. }) | Err(BiliError::Parse { .. }) => continue,
                Err(e) => return Err(e),
            };
            for id in batch {
                let n = unread
                    .iter()
                    .find(|(k, _)| k == id)
                    .map(|(_, v)| *v)
                    .unwrap_or(0);
                if let Some(s) = state.get(id) {
                    let next = s.with_unread(n);
                    state.insert(next);
                }
            }
        }
        Ok(t)
    }

    // -----------------------------------------------------------------------
    // 作者的第一页
    // -----------------------------------------------------------------------

    /// 返回当前成功拉到第一页的作者 id（给 unread 的 `unread_at_fetch` 对齐用）。
    fn fetch_due(
        &self,
        state: &mut Authors,
        deadline: i64,
        drive: &FollowDrive<'_>,
        config: &FollowConfig,
    ) -> Result<Vec<String>, BiliError> {
        let mut failures: usize = 0;
        let mut fetched: Vec<String> = Vec::new();
        for id in due(&state.ordered(), drive.now(), config.max_authors) {
            // 预算到点就不再发新请求；没拉到的下次刷新还在 due 里
            if drive.now() >= deadline {
                break;
            }
            let Some(a) = state.get(&id).cloned() else {
                continue;
            };
            let at_ms = drive.now();
            let page = match self.posted_driven(&a, at_ms, drive, config) {
                Ok(page) => page,
                Err(e) => {
                    // 接口错误（业务码 / 状态码）现在由解析这一层映射成 Api，所以解析也在这里
                    if !matches!(e, BiliError::Api { .. }) {
                        return Err(e);
                    }
                    failures += 1;
                    if failures >= config.max_failures {
                        return Err(e);
                    }
                    let closed = drive.now() + config.closed_for_ms;
                    let next = a.copy_with(|x| x.closed_until = closed);
                    state.insert(next);
                    continue;
                }
            };
            failures = 0;
            let items: Vec<FeedItem> = page.notes.into_iter().map(|(item, _)| item).collect();
            let next = integrate(&a, items, at_ms, config.keep_per_author);
            state.insert(next);
            fetched.push(id);
        }
        Ok(fetched)
    }

    /// 一个作者的主页第一页 + **表情贴回**（Kotlin `fetchDue` 里 `emotesFor(raw.titles)` 那一段）。
    ///
    /// 顺序照 Kotlin：先 `posted`（一个请求，要签名），再看标题里有没有表情记号、真取到表了才贴回条目。
    fn posted_driven(
        &self,
        author: &AuthorState,
        at_ms: i64,
        drive: &FollowDrive<'_>,
        _config: &FollowConfig,
    ) -> Result<xhs::PostedPage, BiliError> {
        let parsed = xhs::FollowedAuthor {
            id: author.id.clone(),
            name: author.name.clone(),
            // Kotlin 这里是 `FollowedAuthor(a.id, a.name, null, a.xsecToken)`：**头像不带**（不参与请求）
            avatar: None,
            xsec_token: author.xsec_token.clone(),
        };
        let raw = self.posted_driven_raw(&parsed, at_ms, drive)?;
        let emotes = self.emotes_for_driven(&raw.titles, drive.now(), drive)?;
        if emotes.is_empty() {
            return Ok(raw);
        }
        let notes = raw
            .notes
            .into_iter()
            .map(|(mut item, sticky)| {
                // `Emotes.used(emotes, title)`：只留标题里真出现的那几项（幂等，`emotes_for` 已经筛过一遍）
                item.emotes = xhs::emotes_used(&emotes, &[item.title.as_deref()]);
                (item, sticky)
            })
            .collect();
        Ok(xhs::PostedPage {
            notes,
            has_more: raw.has_more,
            titles: raw.titles,
        })
    }

    /// 自己的 user_id（关注列表接口的参数）。
    ///
    /// `drive.self_id` 优先（Kotlin 算好传进来的），其次问 [`FollowDrive::self_id_lazy`]
    /// （Kotlin 那边可能因此发一次 `user/me`，**只在这一步真的需要时**），再读 `ctx.store` 的
    /// `xhs.user_id`（`whoami` 存过就在那儿）；都没有 = 没登录。
    ///
    /// 为什么要问两次的可能性：Kotlin `XhsFollowFeed` 里 `selfId()` 被调两处（关注列表 / extra），
    /// 第二次走的是"`api.userId` 已经存下来了"那条短路——这里也一样，第二次问的时候 Kotlin 侧
    /// 已经从 store 里拿到值了，不会再发请求。
    fn resolve_self_id(&self, drive: &FollowDrive<'_>) -> Result<String, BiliError> {
        if let Some(id) = &drive.self_id {
            if !id.trim().is_empty() {
                return Ok(id.clone());
            }
        }
        if let Some(lazy) = drive.self_id_lazy {
            if let Some(id) = lazy.self_id() {
                if !id.trim().is_empty() {
                    return Ok(id);
                }
            }
        }
        if let Some(id) = self.store_get_user_id() {
            if !id.trim().is_empty() {
                return Ok(id);
            }
        }
        Err(BiliError::NotLoggedIn { detail: None })
    }

    // 登录态判断用 `source.rs` 的 `has_login`（账号层也用它；两份合并时留一份）。
}

/// `IOException` 那一类（Kotlin `catch (e: IOException)` 只接这两种）。
///
/// `ProxyUnavailableException` 也是 `IOException` 的子类，所以 fail-closed 也走"用旧缓存"这条路
/// （Kotlin 一样）。
fn is_transport_error(e: &BiliError) -> bool {
    matches!(e, BiliError::Network { .. } | BiliError::ProxyUnavailable { .. })
}

/// 关注流的存盘门面（`FollowStore` 要一个 `Arc<dyn StoreHost>`，平台对象那份是 `Arc`，
/// 所以每次现包一个——它只是一层壳，没有状态）。
impl XhsHostedSource {
    pub(crate) fn follow_store(&self) -> FollowStore {
        FollowStore::new(self.store_port())
    }

    pub(crate) fn follow_load_users(&self) -> Option<UserList> {
        self.follow_store().load_users()
    }

    pub(crate) fn follow_save_users(&self, list: &UserList) {
        self.follow_store().save_users(list);
    }

    pub(crate) fn follow_persist(&self, authors: &Authors) {
        self.follow_store().save_authors(authors);
    }
}
