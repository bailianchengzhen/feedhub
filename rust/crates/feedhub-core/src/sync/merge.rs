//! **合并引擎**（`core/sync/LibraryMerge.kt` 的逐行移植）。
//!
//! 纯函数：不碰数据库 / 网络 / 界面。规则：
//!
//! - 每条记录按 `(revHlc, revDev)` 取最后一次修改（LWW），墓碑（`deleted = true`）也参与比较；
//!   墓碑比"加回来"旧时允许复活。
//! - 删除收藏夹（不删条目）时，导出方已经给夹内每条写了新 HLC、`folderUid = null`；删条目时每条都是墓碑。
//!   引擎只做逐条 LWW，不猜"收藏夹没了条目该去哪"。
//! - 收藏夹用稳定 uid；**首次同步两边各有一个同名收藏夹**时按名字认领：合并结果里只留一个
//!   （uid 由 `(revHlc, revDev, uid)` 最大者胜），其余 uid 通过 [`MergeOutcome::folder_aliases`] 指向它。
//! - 历史：访问时间和播放进度分开比（`revHlc` / `progressHlc`）。墓碑行不留进度。
//! - "清空历史"是全局标记，取两端最大值；`max(revHlc, progressHlc) <= cleared` 的历史行直接丢掉。
//! - 保留期是**本机策略**：只在导入时把远端太老的历史过滤掉，不写墓碑、不传播。
//!
//! **交换律 / 幂等**：合并结果只依赖两边记录的集合，不依赖顺序；同名收藏夹的 uid 选择是确定的。
//! `merge` 的每一步都对应 Kotlin 的同一行——改这里之前先看那边的注释（为什么这么定规则）。

use std::collections::{HashMap, HashSet};

use super::crypto::hlc;
use super::snapshot::{
    SyncBlockedRow, SyncFavoriteRow, SyncFolderRow, SyncHistoryRow, SyncState,
};
use super::text::trim;

/// 合并结果里各类记录改了多少条（相对**本机**）。
///
/// `added` = 本机没有（或本机是墓碑）而合并后活着；`deleted` = 本机活着而合并后是墓碑 / 没了；
/// `updated` = 两边都活着但内容不同（历史的进度变化也算）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncCounts {
    pub favorites_added: i32,
    pub favorites_updated: i32,
    pub favorites_deleted: i32,
    pub folders_added: i32,
    pub folders_updated: i32,
    pub folders_deleted: i32,
    pub history_added: i32,
    pub history_updated: i32,
    pub history_deleted: i32,
    pub blocked_added: i32,
    pub blocked_updated: i32,
    pub blocked_deleted: i32,
}

impl SyncCounts {
    pub fn added(&self) -> i32 {
        self.favorites_added + self.folders_added + self.history_added + self.blocked_added
    }

    pub fn updated(&self) -> i32 {
        self.favorites_updated + self.folders_updated + self.history_updated + self.blocked_updated
    }

    pub fn deleted(&self) -> i32 {
        self.favorites_deleted + self.folders_deleted + self.history_deleted + self.blocked_deleted
    }

    pub fn total(&self) -> i32 {
        self.added() + self.updated() + self.deleted()
    }
}

/// [`merge`] 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct MergeOutcome {
    /// 合并后的完整状态（含墓碑；被清空标记去掉的历史行已经不在里面）。
    pub state: SyncState,
    /// 相对本机的增 / 改 / 删条数。
    pub counts: SyncCounts,
    /// 本机收藏夹 uid → 合并后的 uid（同名认领时才会出现；正常情况下是空的）。**顺序同插入**。
    pub folder_aliases: Vec<(String, String)>,
    /// 合并后在**本机**新建的收藏夹数。
    pub folders_created: i32,
    /// 远端"清空历史"标记比本机大（本机要按它清一次）。
    pub history_cleared_by_remote: bool,
    /// 被清空标记去掉的本机历史行数。
    pub cleared_rows: i32,
}

/// 合并两边，返回合并后的状态和相对 `local` 的改动计数。
pub fn merge(local: &SyncState, remote: &SyncState, retention_cutoff_ms: Option<i64>) -> MergeOutcome {
    let cleared = local.history_cleared_hlc.max(remote.history_cleared_hlc);

    // 1. 收藏夹：逐 uid LWW，再按名字认领同名夹
    let mut folder_by_uid = insert_order_map();
    for row in &local.folders {
        put_folder(&mut folder_by_uid, row);
    }
    for row in &remote.folders {
        put_folder(&mut folder_by_uid, row);
    }

    let mut aliases: Vec<(String, String)> = Vec::new();
    // 同名认领只在"两边各恰好一个同名收藏夹"时做
    let local_alive_by_name = group_alive(&local.folders, |r| r.name.as_str());
    let remote_alive_by_name = group_alive(&remote.folders, |r| r.name.as_str());
    for (name, local_group) in &local_alive_by_name {
        let Some(remote_group) = remote_alive_by_name
            .iter()
            .find(|(other, _)| other == name)
            .map(|(_, group)| group)
        else {
            continue;
        };
        if local_group.len() != 1 || remote_group.len() != 1 {
            continue;
        }
        let mine = local_group[0];
        let theirs = remote_group[0];
        if mine.uid == theirs.uid {
            continue;
        }
        // canonical = (revHlc, revDev, uid) 最大者
        let canonical = if folder_order(mine) >= folder_order(theirs) { mine } else { theirs };
        let loser = if canonical.uid == mine.uid { theirs } else { mine };
        // 顺序：Kotlin 用的是 `mutableMapOf`（LinkedHashMap），按**本机名字分组**的遍历顺序插入
        aliases.push((loser.uid.clone(), canonical.uid.clone()));
    }

    let alias_map: HashMap<&str, &str> = aliases.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();

    let mut merged_folders: Vec<SyncFolderRow> = folder_by_uid
        .into_iter()
        .filter(|row| !alias_map.contains_key(row.uid.as_str()))
        .collect();
    merged_folders.sort_by(|a, b| a.uid.cmp(&b.uid));
    let folder_uids: HashSet<&str> = merged_folders.iter().map(|r| r.uid.as_str()).collect();
    let deleted_folder_uids: HashSet<&str> = merged_folders
        .iter()
        .filter(|r| r.deleted)
        .map(|r| r.uid.as_str())
        .collect();

    // 2. 收藏：逐 key LWW（整行取一边），folderUid 做同名认领映射；指向不存在的收藏夹就当默认收藏夹
    let mut favorite_by_key = insert_order_map();
    for row in &local.favorites {
        put_favorite(&mut favorite_by_key, row);
    }
    for row in &remote.favorites {
        put_favorite(&mut favorite_by_key, row);
    }
    let mut merged_favorites: Vec<SyncFavoriteRow> = favorite_by_key
        .into_iter()
        .map(|mut row| {
            let mapped = row
                .folder_uid
                .as_ref()
                .map(|uid| alias_map.get(uid.as_str()).map(|s| s.to_string()).unwrap_or_else(|| uid.clone()));
            row.folder_uid = mapped.filter(|uid| {
                folder_uids.contains(uid.as_str()) && !deleted_folder_uids.contains(uid.as_str())
            });
            row
        })
        .collect();
    merged_favorites.sort_by(|a, b| a.key.cmp(&b.key));

    // 3. 历史：访问 / 进度分开 LWW，再按清空标记 / 保留期过滤
    let local_history: HashMap<&str, &SyncHistoryRow> =
        local.history.iter().map(|r| (r.key.as_str(), r)).collect();
    let remote_history: HashMap<&str, &SyncHistoryRow> = remote
        .history
        .iter()
        .filter(|row| match retention_cutoff_ms {
            // 保留期只过滤**远端**：本机已有这条就照常参与，墓碑也不过滤（删除要传播）
            None => true,
            Some(cutoff) => row.deleted || row.visited_at >= cutoff || local_history.contains_key(row.key.as_str()),
        })
        .map(|r| (r.key.as_str(), r))
        .collect();
    let mut history_by_key = insert_order_map();
    for row in &local.history {
        put_history(&mut history_by_key, row);
    }
    for row in remote_history.values() {
        put_history(&mut history_by_key, row);
    }

    let mut cleared_rows = 0i32;
    let mut merged_history: Vec<SyncHistoryRow> = Vec::with_capacity(history_by_key.len());
    for row in history_by_key {
        let mut merged = merge_history_row(
            local_history.get(row.key.as_str()).copied(),
            remote_history.get(row.key.as_str()).copied(),
        );
        if merged.deleted {
            // 墓碑不留进度：复活时从零开始
            merged.progress_ms = None;
            merged.duration_ms = None;
            merged.progress_hlc = 0;
            merged.progress_dev = String::new();
        }
        // 清空标记：两条时间线都在清空之前 / 当时的行丢掉
        if cleared > 0 && merged.rev_hlc.max(merged.progress_hlc) <= cleared {
            if let Some(before) = local_history.get(row.key.as_str()) {
                if !before.deleted {
                    cleared_rows += 1;
                }
            }
            continue;
        }
        merged_history.push(merged);
    }
    merged_history.sort_by(|a, b| a.key.cmp(&b.key));

    // 4. 屏蔽：逐 key LWW
    let mut blocked_by_key = insert_order_map();
    for row in &local.blocked {
        put_blocked(&mut blocked_by_key, row);
    }
    for row in &remote.blocked {
        put_blocked(&mut blocked_by_key, row);
    }
    let mut merged_blocked: Vec<SyncBlockedRow> = blocked_by_key.into_iter().collect();
    merged_blocked.sort_by(|a, b| a.key.cmp(&b.key));

    let state = SyncState {
        history_cleared_hlc: cleared,
        folders: merged_folders,
        favorites: merged_favorites,
        history: merged_history,
        blocked: merged_blocked,
    };

    // 5. 计数：先把本机按别名"改名"，再和合并结果比
    let local_remapped = remap_local(local, &aliases);
    let counts = count_changes(&local_remapped, &state);
    let local_folder_uids: HashSet<&str> = local.folders.iter().map(|r| r.uid.as_str()).collect();
    // 被"同名认领"接过去的本地收藏夹不算新建
    let claimed_uids: HashSet<&str> = aliases
        .iter()
        .filter(|(old, _)| local_folder_uids.contains(old.as_str()))
        .map(|(_, new)| new.as_str())
        .collect();
    let folders_created = state
        .folders
        .iter()
        .filter(|r| !r.deleted && !local_folder_uids.contains(r.uid.as_str()) && !claimed_uids.contains(r.uid.as_str()))
        .count() as i32;
    MergeOutcome {
        state,
        counts,
        folder_aliases: aliases,
        folders_created,
        history_cleared_by_remote: remote.history_cleared_hlc > local.history_cleared_hlc,
        cleared_rows,
    }
}

/// 把本机状态按同名认领的映射改一下（只用于计数比较）。
pub fn remap_local(local: &SyncState, aliases: &[(String, String)]) -> SyncState {
    if aliases.is_empty() {
        return local.clone();
    }
    let map: HashMap<&str, &str> = aliases.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    SyncState {
        history_cleared_hlc: local.history_cleared_hlc,
        folders: local
            .folders
            .iter()
            .map(|row| {
                let mut row = row.clone();
                if let Some(new_uid) = map.get(row.uid.as_str()) {
                    row.uid = new_uid.to_string();
                }
                row
            })
            .collect(),
        favorites: local
            .favorites
            .iter()
            .map(|row| {
                let mut row = row.clone();
                if let Some(uid) = &row.folder_uid {
                    if let Some(mapped) = map.get(uid.as_str()) {
                        row.folder_uid = Some(mapped.to_string());
                    }
                }
                row
            })
            .collect(),
        history: local.history.clone(),
        blocked: local.blocked.clone(),
    }
}

// ---------------------------------------------------------------- 逐条合并

/// 一个"插入顺序 + 按 key 覆盖"的表。行为和 Kotlin 的 `LinkedHashMap` 一致：
/// 已有的 key 换值**不动位置**，新 key 追加到末尾。合并结果之后还会按 key 排序，
/// 但 `folders` 那条路要靠它先稳定（同名认领的 loser 选择）。
struct InsertOrderMap<T> {
    entries: Vec<T>,
    index: HashMap<String, usize>,
}

fn insert_order_map<T>() -> InsertOrderMap<T> {
    InsertOrderMap {
        entries: Vec::new(),
        index: HashMap::new(),
    }
}

impl<T> InsertOrderMap<T> {
    fn put(&mut self, key: &str, value: T) {
        match self.index.get(key) {
            Some(&i) => self.entries[i] = value,
            None => {
                self.index.insert(key.to_string(), self.entries.len());
                self.entries.push(value);
            }
        }
    }

    fn into_iter(self) -> std::vec::IntoIter<T> {
        self.entries.into_iter()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl<T> IntoIterator for InsertOrderMap<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

fn put_folder(into: &mut InsertOrderMap<SyncFolderRow>, row: &SyncFolderRow) {
    match into.index.get(row.uid.as_str()) {
        Some(&i) => {
            if hlc::compare_version(
                row.rev_hlc,
                &row.rev_dev,
                into.entries[i].rev_hlc,
                &into.entries[i].rev_dev,
            ) > 0
            {
                into.entries[i] = row.clone();
            }
        }
        None => into.put(&row.uid, row.clone()),
    }
}

fn put_favorite(into: &mut InsertOrderMap<SyncFavoriteRow>, row: &SyncFavoriteRow) {
    match into.index.get(row.key.as_str()) {
        Some(&i) => {
            if hlc::compare_version(
                row.rev_hlc,
                &row.rev_dev,
                into.entries[i].rev_hlc,
                &into.entries[i].rev_dev,
            ) > 0
            {
                into.entries[i] = row.clone();
            }
        }
        None => into.put(&row.key, row.clone()),
    }
}

fn put_history(into: &mut InsertOrderMap<SyncHistoryRow>, row: &SyncHistoryRow) {
    match into.index.get(row.key.as_str()) {
        Some(&i) => {
            if hlc::compare_version(
                row.rev_hlc,
                &row.rev_dev,
                into.entries[i].rev_hlc,
                &into.entries[i].rev_dev,
            ) > 0
            {
                into.entries[i] = row.clone();
            }
        }
        None => into.put(&row.key, row.clone()),
    }
}

fn put_blocked(into: &mut InsertOrderMap<SyncBlockedRow>, row: &SyncBlockedRow) {
    match into.index.get(row.key.as_str()) {
        Some(&i) => {
            if hlc::compare_version(
                row.rev_hlc,
                &row.rev_dev,
                into.entries[i].rev_hlc,
                &into.entries[i].rev_dev,
            ) > 0
            {
                into.entries[i] = row.clone();
            }
        }
        None => into.put(&row.key, row.clone()),
    }
}

/// 历史一行单独合：访问信息取 `revHlc` 的胜者，进度取 `progressHlc` 的胜者。
/// 墓碑那一侧不参与进度比较（它代表"看过的记录被删了"）。
fn merge_history_row(local: Option<&SyncHistoryRow>, remote: Option<&SyncHistoryRow>) -> SyncHistoryRow {
    let (local, remote) = match (local, remote) {
        (None, Some(remote)) => return remote.clone(),
        (Some(local), None) => return local.clone(),
        (Some(local), Some(remote)) => (local, remote),
        // 调用方保证了 key 只来自两边之一
        (None, None) => unreachable!("历史行至少来自一边"),
    };
    let visit = if hlc::compare_version(remote.rev_hlc, &remote.rev_dev, local.rev_hlc, &local.rev_dev) > 0 {
        remote
    } else {
        local
    };
    // `progressCandidates.maxWithOrNull(compareBy(progressHlc, progressDev))`：
    // `maxWithOrNull` 在平局时返回**第一个**（Kotlin 的 `maxWithOrNull` 是 `if (comparator.compare(a, b) < 0) b else a`，
    // 即平局保留先出现的那个——`listOf(local, remote)` 里的 local）。
    let progress = [Some(local), Some(remote)]
        .into_iter()
        .flatten()
        .filter(|r| !r.deleted)
        .reduce(|best, next| {
            if compare_progress(&next, &best) > 0 {
                next
            } else {
                best
            }
        });
    let mut merged = visit.clone();
    merged.progress_ms = progress.map(|p| p.progress_ms).unwrap_or(None);
    merged.duration_ms = progress.map(|p| p.duration_ms).unwrap_or(None);
    merged.progress_hlc = progress.map(|p| p.progress_hlc).unwrap_or(0);
    merged.progress_dev = progress.map(|p| p.progress_dev.clone()).unwrap_or_default();
    merged
}

fn compare_progress(a: &SyncHistoryRow, b: &SyncHistoryRow) -> i32 {
    if a.progress_hlc != b.progress_hlc {
        return if a.progress_hlc < b.progress_hlc { -1 } else { 1 };
    }
    match a.progress_dev.as_str().cmp(b.progress_dev.as_str()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// 同名认领时比 `(revHlc, revDev, uid)`。
fn folder_order(row: &SyncFolderRow) -> (i64, &str, &str) {
    (row.rev_hlc, row.rev_dev.as_str(), row.uid.as_str())
}

/// 按名字分组（只留活着的），保持**首次出现顺序**。
fn group_alive<'a>(rows: &'a [SyncFolderRow], key_of: impl Fn(&'a SyncFolderRow) -> &'a str) -> Vec<(String, Vec<&'a SyncFolderRow>)> {
    let mut groups: Vec<(String, Vec<&SyncFolderRow>)> = Vec::new();
    for row in rows.iter().filter(|r| !r.deleted) {
        let key = key_of(row).to_string();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(row),
            None => groups.push((key, vec![row])),
        }
    }
    groups
}

// ---------------------------------------------------------------- 计数

fn count_changes(local: &SyncState, merged: &SyncState) -> SyncCounts {
    let local_fav: HashMap<&str, &SyncFavoriteRow> = local.favorites.iter().map(|r| (r.key.as_str(), r)).collect();
    let merged_fav: HashMap<&str, &SyncFavoriteRow> = merged.favorites.iter().map(|r| (r.key.as_str(), r)).collect();
    let local_hist: HashMap<&str, &SyncHistoryRow> = local.history.iter().map(|r| (r.key.as_str(), r)).collect();
    let merged_hist: HashMap<&str, &SyncHistoryRow> = merged.history.iter().map(|r| (r.key.as_str(), r)).collect();
    let local_folders: HashMap<&str, &SyncFolderRow> = local.folders.iter().map(|r| (r.uid.as_str(), r)).collect();
    let merged_folders: HashMap<&str, &SyncFolderRow> = merged.folders.iter().map(|r| (r.uid.as_str(), r)).collect();
    let local_blocked: HashMap<&str, &SyncBlockedRow> = local.blocked.iter().map(|r| (r.key.as_str(), r)).collect();
    let merged_blocked: HashMap<&str, &SyncBlockedRow> = merged.blocked.iter().map(|r| (r.key.as_str(), r)).collect();

    let mut counts = SyncCounts::default();

    for key in union_keys(&local_fav, &merged_fav) {
        let a = local_fav.get(key.as_str()).copied();
        let b = merged_fav.get(key.as_str()).copied();
        match (a, b) {
            (_, Some(b)) if !b.deleted && (a.is_none() || a.map(|r| r.deleted).unwrap_or(false)) => {
                counts.favorites_added += 1
            }
            (Some(a), Some(b)) if b.deleted && !a.deleted => counts.favorites_deleted += 1,
            (Some(a), Some(b)) if !b.deleted && !a.deleted && !same_favorite(a, b) => counts.favorites_updated += 1,
            _ => {}
        }
    }

    for key in union_keys(&local_hist, &merged_hist) {
        let a = local_hist.get(key.as_str()).copied();
        let b = merged_hist.get(key.as_str()).copied();
        match (a, b) {
            (_, Some(b)) if !b.deleted && (a.is_none() || a.map(|r| r.deleted).unwrap_or(false)) => {
                counts.history_added += 1
            }
            // 本机活着、合并结果里没了（被"清空历史"标记去掉）也算删除
            (Some(a), None) if !a.deleted => counts.history_deleted += 1,
            (Some(a), Some(b)) if b.deleted && !a.deleted => counts.history_deleted += 1,
            (Some(a), Some(b)) if !b.deleted && !a.deleted && !same_history(a, b) => counts.history_updated += 1,
            _ => {}
        }
    }

    for uid in union_keys(&local_folders, &merged_folders) {
        let a = local_folders.get(uid.as_str()).copied();
        let b = merged_folders.get(uid.as_str()).copied();
        match (a, b) {
            (_, Some(b)) if !b.deleted && (a.is_none() || a.map(|r| r.deleted).unwrap_or(false)) => {
                counts.folders_added += 1
            }
            (Some(a), Some(b)) if b.deleted && !a.deleted => counts.folders_deleted += 1,
            (Some(a), Some(b)) if !b.deleted && !a.deleted && a.name != b.name => counts.folders_updated += 1,
            _ => {}
        }
    }

    for key in union_keys(&local_blocked, &merged_blocked) {
        let a = local_blocked.get(key.as_str()).copied();
        let b = merged_blocked.get(key.as_str()).copied();
        match (a, b) {
            (_, Some(b)) if !b.deleted && (a.is_none() || a.map(|r| r.deleted).unwrap_or(false)) => {
                counts.blocked_added += 1
            }
            (Some(a), Some(b)) if b.deleted && !a.deleted => counts.blocked_deleted += 1,
            (Some(a), Some(b)) if !b.deleted && !a.deleted && !same_blocked(a, b) => counts.blocked_updated += 1,
            _ => {}
        }
    }

    counts
}

/// 两张表的键的并集（顺序不影响计数，但为了稳定按字典序给出来）。
fn union_keys<T>(a: &HashMap<&str, T>, b: &HashMap<&str, T>) -> Vec<String> {
    let mut keys: Vec<String> = a.keys().chain(b.keys()).map(|k| k.to_string()).collect();
    keys.sort();
    keys.dedup();
    keys
}

fn same_favorite(a: &SyncFavoriteRow, b: &SyncFavoriteRow) -> bool {
    a.folder_uid == b.folder_uid && a.added_at == b.added_at && a.item == b.item
}

fn same_history(a: &SyncHistoryRow, b: &SyncHistoryRow) -> bool {
    a.visited_at == b.visited_at
        && a.item == b.item
        && a.progress_ms == b.progress_ms
        && a.duration_ms == b.duration_ms
}

fn same_blocked(a: &SyncBlockedRow, b: &SyncBlockedRow) -> bool {
    a.user == b.user && a.blocked_at == b.blocked_at
}

/// 合并结果里最大的 HLC（`Library.max_hlc`，写库时要把本机时钟抬到它之后）。
pub fn max_hlc(state: &SyncState) -> i64 {
    let mut max = state.history_cleared_hlc;
    for f in &state.folders {
        max = max.max(f.rev_hlc);
    }
    for f in &state.favorites {
        max = max.max(f.rev_hlc);
    }
    for h in &state.history {
        max = max.max(h.rev_hlc).max(h.progress_hlc);
    }
    for b in &state.blocked {
        max = max.max(b.rev_hlc);
    }
    max
}

/// 只给测试 / 排障用：`trim` 的转发（合并里不用，但别让 `text::trim` 变成死代码）。
#[allow(dead_code)]
fn _unused_trim(s: &str) -> &str {
    trim(s)
}

