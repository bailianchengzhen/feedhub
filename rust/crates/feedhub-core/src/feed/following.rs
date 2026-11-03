//! 「我的 → 关注管理」的分页 + 取关确认流程（`app/.../mine/FollowingState.kt`）。
//!
//! **写操作约定（照抄，不许改）**：取关只有一条路——[`FollowEngine::ask_unfollow`]（用户点"取关"）→
//! 界面弹确认框 → [`FollowEngine::confirm_unfollow`] 才会让宿主调 `setFollowing(target, false)`；
//! 读列表、翻页、重试都不会碰它，失败了也**不自动重试**。
//!
//! 这一层搬过来的是**流程和分页状态**：游标、去重（`kind to id`）、`hasMore` / `loaded` / `busy` /
//! `error` / `actionError`、确认框那一格（`pendingUnfollow`）。
//!
//! **留宿主（Kotlin）的**：
//! - 真正的 `followings(cursor)` / `setFollowing(target, false)`（OkHttp，走平台对象）；
//! - Compose 状态容器 + 协程；
//! - 确认框的界面（`alertdialog` 那些）。

use std::collections::HashSet;

/// 可关注的对象（`FollowTarget`，字段一对一）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowTarget {
    pub platform: String,
    /// `USER` / `FORUM`。
    pub kind: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

impl FollowTarget {
    /// 去重用的身份：`kind to id`（和 Kotlin 的 `distinctBy { it.kind to it.id }` 一样）。
    fn identity(&self) -> (String, String) {
        (self.kind.clone(), self.id.clone())
    }
}

/// 一次关注列表请求的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOutcome {
    Ok {
        items: Vec<FollowTarget>,
        next: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// 取关请求的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnfollowOutcome {
    Ok,
    Failed { message: String },
}

/// 宿主拿到它去真的发请求。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    /// 要不要去取一页关注列表（`Some(cursor)`）。
    pub load: Option<Option<String>>,
    /// 要不要去取关这一个（**只有 `confirm_unfollow` 会给**）。
    pub unfollow: Option<FollowTarget>,
    pub items_changed: bool,
    pub started: bool,
}

/// 宿主抄回 Compose 状态用的快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub loading: bool,
    pub loaded: bool,
    pub has_more: bool,
    pub error: Option<String>,
    pub action_error: Option<String>,
    /// 正在取关的对象 id。
    pub busy: Vec<String>,
    /// 确认框里那个（用户点了"取关"、还没确认）。
    pub pending_unfollow: Option<FollowTarget>,
    pub item_count: usize,
}

/// 关注管理的状态机。同步、不加锁：宿主在 Compose 的 `scope`（主线程）上调。
#[derive(Debug)]
pub struct FollowEngine {
    items: Vec<FollowTarget>,
    loading: bool,
    error: Option<String>,
    has_more: bool,
    loaded: bool,
    busy: Vec<String>,
    action_error: Option<String>,
    cursor: Option<String>,
    pending_unfollow: Option<FollowTarget>,
    /// 这一趟要取的是哪一页（`load()` 时记下，回来时对一下）。
    loading_cursor: Option<String>,
    /// 正在取关的那个（回来时对一下，别把别人的错误记到它头上）。
    unfollowing: Option<String>,
}

impl Default for FollowEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FollowEngine {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            loading: false,
            error: None,
            has_more: true,
            loaded: false,
            busy: Vec::new(),
            action_error: None,
            cursor: None,
            pending_unfollow: None,
            loading_cursor: None,
            unfollowing: None,
        }
    }

    pub fn view(&self) -> View {
        View {
            loading: self.loading,
            loaded: self.loaded,
            has_more: self.has_more,
            error: self.error.clone(),
            action_error: self.action_error.clone(),
            busy: self.busy.clone(),
            pending_unfollow: self.pending_unfollow.clone(),
            item_count: self.items.len(),
        }
    }

    pub fn items(&self) -> &[FollowTarget] {
        &self.items
    }

    /// 第一次显示时加载。
    pub fn ensure_loaded(&mut self) -> Action {
        if !self.loaded && !self.loading {
            return self.refresh();
        }
        Action::default()
    }

    /// 重新从第一页取。
    pub fn refresh(&mut self) -> Action {
        self.items.clear();
        self.cursor = None;
        self.has_more = true;
        self.error = None;
        self.action_error = None;
        self.load()
    }

    /// 滑到底时调：取下一页。
    pub fn load_more(&mut self) -> Action {
        if self.loading || !self.has_more || self.error.is_some() {
            return Action::default();
        }
        self.load()
    }

    /// 读列表失败后点"重试"：接着取当前这一页。
    pub fn retry(&mut self) -> Action {
        if self.loading {
            return Action::default();
        }
        self.error = None;
        self.load()
    }

    fn load(&mut self) -> Action {
        self.loading = true;
        self.loading_cursor = self.cursor.clone();
        Action {
            load: Some(self.cursor.clone()),
            unfollow: None,
            items_changed: false,
            started: true,
        }
    }

    /// 一页回来了。
    pub fn on_list(&mut self, outcome: ListOutcome) -> Action {
        self.loading = false;
        let mut action = Action::default();
        match outcome {
            ListOutcome::Ok { items, next } => {
                let mut seen: HashSet<(String, String)> =
                    self.items.iter().map(|t| t.identity()).collect();
                let before = self.items.len();
                for item in items {
                    if seen.insert(item.identity()) {
                        self.items.push(item);
                    }
                }
                self.cursor = next.clone();
                self.has_more = next.is_some();
                self.loaded = true;
                action.items_changed = self.items.len() != before;
                // 只有"这一趟"的游标还停在原处时才算成功（宿主可能已经重新开始新一轮了）
                self.loading_cursor = None;
            }
            ListOutcome::Failed { message } => {
                self.error = Some(message);
                self.loading_cursor = None;
            }
        }
        action
    }

    /// 用户点了某项的"取关"、还没确认：界面弹确认框。
    pub fn ask_unfollow(&mut self, target: FollowTarget) {
        self.pending_unfollow = Some(target);
    }

    pub fn cancel_unfollow(&mut self) {
        self.pending_unfollow = None;
    }

    /// 确认框里点了"取消关注"：**这是唯一会真的取关的入口**。
    pub fn confirm_unfollow(&mut self) -> Action {
        let Some(target) = self.pending_unfollow.take() else {
            return Action::default();
        };
        self.unfollow(target)
    }

    fn unfollow(&mut self, target: FollowTarget) -> Action {
        if self.busy.iter().any(|id| *id == target.id) {
            return Action::default();
        }
        self.busy.push(target.id.clone());
        self.action_error = None;
        self.unfollowing = Some(target.id.clone());
        Action {
            load: None,
            unfollow: Some(target.clone()),
            items_changed: false,
            started: true,
        }
    }

    /// 取关回来了。成功后从列表里去掉；失败记在 `actionError`，**不重试**。
    pub fn on_unfollow(&mut self, target: &FollowTarget, outcome: UnfollowOutcome) -> Action {
        self.busy.retain(|id| *id != target.id);
        self.unfollowing = None;
        let mut action = Action::default();
        match outcome {
            UnfollowOutcome::Ok => {
                let before = self.items.len();
                let identity = target.identity();
                self.items.retain(|t| t.identity() != identity);
                action.items_changed = self.items.len() != before;
            }
            UnfollowOutcome::Failed { message } => {
                self.action_error = Some(format!("取关「{}」失败：{}", target.name, message));
            }
        }
        action
    }

    pub fn dismiss_action_error(&mut self) {
        self.action_error = None;
    }
}
