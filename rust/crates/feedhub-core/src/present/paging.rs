//! **分页状态机与平台规则**。
//!
//! 对应 Kotlin 原版：
//! - `app/src/commonMain/kotlin/feedhub/app/detail/Paged.kt` 的 `PagedState` 状态机；
//! - `CommentsState` 的排序切换与平台评论 / 楼中楼支持度判定规则。

use std::collections::HashSet;

/// 一列分页数据的纯状态机（评论、楼中楼、各种分页列表共用）。
///
/// 内部管理：
/// - `keys` 去重集合；
/// - 游标 `cursor` 与是否到底 `ended`；
/// - 是否加载过 `loaded` / 是否正在加载 `loading`；
/// - 缓存过期后台静默刷新标志 `reseed`；
/// - 友好人话错误 `error` 与技术细节错误 `technical_error`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedEngine {
    keys: HashSet<String>,
    cursor: Option<String>,
    ended: bool,
    loaded: bool,
    loading: bool,
    reseed: bool,
    error: Option<String>,
    technical_error: Option<String>,
}

/// 发起一次加载请求前计算出的步骤信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedBeginLoad {
    /// 传给数据获取接口的游标（若后台静默刷新第一页，传 `None`）。
    pub cursor: Option<String>,
    /// 是否在重新刷新第一页（覆盖模式）。
    pub refetch_first: bool,
    /// 这一次是不是"第一页"（`refetch_first || !has_loaded_first`）。
    pub is_first_page: bool,
}

/// 一页数据返回后计算出的合并结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedCompleteResult {
    /// 要保留 / 追加的新增项下标（相对于传入的当前页条目列表）。
    pub kept_indices: Vec<u32>,
    /// 是否为第一页整列替换（若为 true，宿主清空旧列表直接赋新列表；否则追加 kept_indices）。
    pub replace: bool,
    /// 列表是否到底。
    pub ended: bool,
    /// 是否至少成功拿过一页。
    pub loaded: bool,
    /// 更新后的游标。
    pub next_cursor: Option<String>,
    /// 这一次请求是不是第一页。
    pub is_first_page: bool,
}

impl Default for PagedEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PagedEngine {
    pub fn new() -> Self {
        PagedEngine {
            keys: HashSet::new(),
            cursor: None,
            ended: false,
            loaded: false,
            loading: false,
            reseed: false,
            error: None,
            technical_error: None,
        }
    }

    /// 用缓存里的数据把状态摆成"已拿过第一页"。
    ///
    /// @param fresh 够不够新。若不够新（过期了），先显示它，第一次加载时先原地刷新第一页。
    pub fn seed(&mut self, keys: Vec<String>, next: Option<String>, ended: bool, fresh: bool) {
        self.keys.extend(keys);
        self.cursor = next;
        self.ended = ended;
        self.loaded = true;
        self.reseed = !fresh;
    }

    /// 准备发起一次加载。
    ///
    /// 若 `loading || (ended && !reseed)` 则跳过返回 `None`。
    /// 否则清理错误，设置 `loading = true`，并返回游标和刷新标志。
    pub fn begin_load(&mut self) -> Option<PagedBeginLoad> {
        if self.loading || (self.ended && !self.reseed) {
            return None;
        }
        self.error = None;
        self.loading = true;
        let refetch_first = self.reseed;
        self.reseed = false;
        let has_loaded_first = self.loaded;
        let is_first_page = refetch_first || !has_loaded_first;
        let cursor = if refetch_first {
            None
        } else {
            self.cursor.clone()
        };
        Some(PagedBeginLoad {
            cursor,
            refetch_first,
            is_first_page,
        })
    }

    /// 一页数据加载成功后更新状态。
    pub fn complete_page(
        &mut self,
        item_keys: Vec<String>,
        next: Option<String>,
        refetch_first: bool,
        is_first_page: bool,
    ) -> PagedCompleteResult {
        let replace = refetch_first;
        let mut kept_indices = Vec::new();
        if refetch_first {
            self.keys.clear();
            for (i, key) in item_keys.into_iter().enumerate() {
                self.keys.insert(key);
                kept_indices.push(i as u32);
            }
        } else {
            for (i, key) in item_keys.into_iter().enumerate() {
                if self.keys.insert(key) {
                    kept_indices.push(i as u32);
                }
            }
            if next.is_none() || kept_indices.is_empty() {
                self.ended = true;
            }
        }
        self.cursor = next.clone();
        self.loaded = true;
        if next.is_none() {
            self.ended = true;
        }
        self.loading = false;
        PagedCompleteResult {
            kept_indices,
            replace,
            ended: self.ended,
            loaded: self.loaded,
            next_cursor: self.cursor.clone(),
            is_first_page,
        }
    }

    /// 一页数据加载失败后更新状态。
    ///
    /// **关键语义**：若是后台静默重新加载过期第一页（`refetch_first == true`），
    /// **不设 error**（保留旧内容显示！底部也不弹大红字）。
    /// 真的翻页失败（第二页起）或首次无缓存加载失败时，才设置 `error`。
    pub fn fail_page(&mut self, friendly_err: String, technical_err: String, refetch_first: bool) {
        self.technical_error = Some(technical_err);
        if !refetch_first {
            self.error = Some(friendly_err);
        }
        self.loading = false;
    }

    /// 取消当前正在加载的请求。
    pub fn cancel(&mut self) {
        self.loading = false;
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn is_reseed(&self) -> bool {
        self.reseed
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    pub fn technical_error(&self) -> Option<String> {
        self.technical_error.clone()
    }

    pub fn cursor(&self) -> Option<String> {
        self.cursor.clone()
    }

    pub fn keys_count(&self) -> usize {
        self.keys.len()
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.keys.contains(key)
    }
}

// ===========================================================================
// 平台能力与规则
// ===========================================================================

pub const COMMENTS_OFF: &str =
    "这个平台的评论暂时看不了（接口被平台风控拦了，见 source-coolapk/README.md）";

/// 平台是否支持评论拉取（目前全部放开）。
pub fn comments_supported(_platform: &str) -> bool {
    true
}

/// 楼中楼能不能拉。
pub fn replies_supported(capability: Option<bool>) -> bool {
    capability.unwrap_or(true)
}

/// 评论排序切换是否可用。
pub fn comments_sortable(capability: Option<bool>) -> bool {
    capability.unwrap_or(true)
}

/// 排序切换判断：相同排序不重拉，返回 true 表示需要切换。
pub fn should_switch_sort(current: &str, next: &str) -> bool {
    current != next
}

