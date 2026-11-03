//! **详情正文与评论第一页的会话级缓存**。
//!
//! 对应 Kotlin 原版 `app/src/commonMain/kotlin/feedhub/app/detail/DetailCache.kt`。
//!
//! 核心语义与口径：
//! - `MAX_ENTRIES = 16`；
//! - `FRESH_MS = 10 * 60 * 1000L`（10 分钟）；
//! - 访问序 LRU：任何命中 `key` 的读取 / 写入（`get`、`is_fresh`、`put`、`comments`、
//!   `comments_fresh`、`put_comments`）都会将该条目提至 LRU 最新位置，与 Java `LinkedHashMap(accessOrder = true)` 100% 对齐；
//! - 内部统一分配句柄 ID，淘汰时将淘汰句柄返回给宿主释放，避免内存泄漏。

use std::collections::{HashMap, VecDeque};

pub const MAX_ENTRIES: usize = 16;
pub const FRESH_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentSeedData {
    pub handle: u64,
    pub next: Option<String>,
    pub ended: bool,
    pub at: i64,
}

#[derive(Debug, Clone)]
pub struct DetailEntry {
    pub detail_handle: Option<u64>,
    pub at: i64,
    pub comments: HashMap<String, CommentSeedData>,
}

/// 被淘汰或替换掉的句柄（通知宿主从对象池中移除）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictedHandles {
    pub detail_handles: Vec<u64>,
    pub comments_handles: Vec<u64>,
}

impl EvictedHandles {
    pub fn is_empty(&self) -> bool {
        self.detail_handles.is_empty() && self.comments_handles.is_empty()
    }

    pub fn extend(&mut self, other: EvictedHandles) {
        self.detail_handles.extend(other.detail_handles);
        self.comments_handles.extend(other.comments_handles);
    }
}

#[derive(Debug, Clone)]
pub struct DetailCache {
    max_entries: usize,
    fresh_ms: i64,
    order: VecDeque<String>,
    entries: HashMap<String, DetailEntry>,
    next_handle: u64,
}

impl Default for DetailCache {
    fn default() -> Self {
        Self::new(MAX_ENTRIES, FRESH_MS)
    }
}

impl DetailCache {
    pub fn new(max_entries: usize, fresh_ms: i64) -> Self {
        DetailCache {
            max_entries: max_entries.max(1),
            fresh_ms,
            order: VecDeque::new(),
            entries: HashMap::new(),
            next_handle: 1,
        }
    }

    pub fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        h
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }

    fn trim(&mut self) -> EvictedHandles {
        let mut evicted = EvictedHandles::default();
        while self.order.len() > self.max_entries {
            if let Some(old_key) = self.order.pop_front() {
                if let Some(entry) = self.entries.remove(&old_key) {
                    if let Some(h) = entry.detail_handle {
                        evicted.detail_handles.push(h);
                    }
                    for (_, seed) in entry.comments {
                        evicted.comments_handles.push(seed.handle);
                    }
                }
            }
        }
        evicted
    }

    // ---------------- 正文 ----------------

    pub fn get(&mut self, key: &str) -> Option<u64> {
        if self.entries.contains_key(key) {
            self.touch(key);
            self.entries.get(key).and_then(|e| e.detail_handle)
        } else {
            None
        }
    }

    pub fn is_fresh(&mut self, key: &str, now: i64) -> bool {
        if self.entries.contains_key(key) {
            self.touch(key);
            let entry = self.entries.get(key).unwrap();
            let diff = now - entry.at;
            (0..=self.fresh_ms).contains(&diff)
        } else {
            false
        }
    }

    pub fn put(&mut self, key: &str, detail_handle: u64, now: i64) -> EvictedHandles {
        let mut evicted = EvictedHandles::default();
        if let Some(entry) = self.entries.get_mut(key) {
            if let Some(old_h) = entry.detail_handle.replace(detail_handle) {
                if old_h != detail_handle {
                    evicted.detail_handles.push(old_h);
                }
            }
            entry.at = now;
            self.touch(key);
        } else {
            let entry = DetailEntry {
                detail_handle: Some(detail_handle),
                at: now,
                comments: HashMap::new(),
            };
            self.entries.insert(key.to_string(), entry);
            self.touch(key);
        }
        evicted.extend(self.trim());
        evicted
    }

    pub fn remove(&mut self, key: &str) -> EvictedHandles {
        let mut evicted = EvictedHandles::default();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        if let Some(entry) = self.entries.remove(key) {
            if let Some(h) = entry.detail_handle {
                evicted.detail_handles.push(h);
            }
            for (_, seed) in entry.comments {
                evicted.comments_handles.push(seed.handle);
            }
        }
        evicted
    }

    // ---------------- 评论第一页 ----------------

    pub fn comments(&mut self, key: &str, sort: &str) -> Option<CommentSeedData> {
        if self.entries.contains_key(key) {
            self.touch(key);
            self.entries
                .get(key)
                .and_then(|e| e.comments.get(sort).cloned())
        } else {
            None
        }
    }

    pub fn comments_fresh(&mut self, key: &str, sort: &str, now: i64) -> bool {
        if self.entries.contains_key(key) {
            self.touch(key);
            if let Some(seed) = self.entries.get(key).and_then(|e| e.comments.get(sort)) {
                let diff = now - seed.at;
                (0..=self.fresh_ms).contains(&diff)
            } else {
                false
            }
        } else {
            false
        }
    }

    pub fn put_comments(
        &mut self,
        key: &str,
        sort: &str,
        comments_handle: u64,
        next: Option<String>,
        ended: bool,
        now: i64,
    ) -> EvictedHandles {
        let mut evicted = EvictedHandles::default();
        let seed = CommentSeedData {
            handle: comments_handle,
            next,
            ended,
            at: now,
        };
        if let Some(entry) = self.entries.get_mut(key) {
            if let Some(old_seed) = entry.comments.insert(sort.to_string(), seed) {
                if old_seed.handle != comments_handle {
                    evicted.comments_handles.push(old_seed.handle);
                }
            }
            self.touch(key);
        } else {
            let mut comments = HashMap::new();
            comments.insert(sort.to_string(), seed);
            let entry = DetailEntry {
                detail_handle: None,
                at: now,
                comments,
            };
            self.entries.insert(key.to_string(), entry);
            self.touch(key);
        }
        evicted.extend(self.trim());
        evicted
    }

    pub fn clear(&mut self) -> EvictedHandles {
        let mut evicted = EvictedHandles::default();
        for (_, entry) in self.entries.drain() {
            if let Some(h) = entry.detail_handle {
                evicted.detail_handles.push(h);
            }
            for (_, seed) in entry.comments {
                evicted.comments_handles.push(seed.handle);
            }
        }
        self.order.clear();
        evicted
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 调试 / 测试：查看当前 LRU 顺序（最老到最新）。
    pub fn keys_in_lru_order(&self) -> Vec<String> {
        self.order.iter().cloned().collect()
    }
}

pub fn detail_cache_key(platform: &str, id: &str) -> String {
    format!("{platform}/{id}")
}

