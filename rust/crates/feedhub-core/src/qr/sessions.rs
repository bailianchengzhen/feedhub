//! 扫码会话的 **LRU**。
//!
//! 语义逐条照 Kotlin 的 `LinkedHashMap<String, MemoryCookieJar>(16, 0.75f, /* accessOrder = */ true)`
//! + `removeEldestEntry { size > maxSize }`：
//!
//! - [`Sessions::begin`]（`jars[token] = jar`）：插入或**替换**（替换也算一次访问，排到最新），
//!   然后超出容量就把**最久没访问过**的那条挤掉，返回被挤掉的键；
//! - [`Sessions::jar`]（`jars.getOrPut(token) { MemoryCookieJar() }`）：**已有的算一次访问**（排到最新、
//!   不挤人）；没有的就新建并排到最新（这时可能挤掉别的），返回被挤掉的键；
//! - [`Sessions::end`]（`jars.remove(token)`）：删掉，返回它原本在不在。
//!
//! **值（`MemoryCookieJar`）在宿主**：Rust 只持有键和顺序，宿主按返回值删自己那份 map。
//! 所以"临时罐"的所有权只有一份，Rust 这边不可能和宿主的内存罐子不同步出错。
//!
//! 容量默认 [`DEFAULT_MAX_SIZE`]（Kotlin `QrSessions(maxSize = 8)`）。

use std::collections::VecDeque;

/// Kotlin `QrSessions()` 的默认容量（8 条扫码会话）。
pub const DEFAULT_MAX_SIZE: usize = 8;

/// 扫码会话的 LRU（键 = 平台给的 token / 轮询 key）。
#[derive(Debug, Clone)]
pub struct Sessions {
    max: usize,
    /// 最久没访问的在最前，最近访问的在最后。
    order: VecDeque<String>,
}

impl Default for Sessions {
    fn default() -> Self {
        Sessions::new(DEFAULT_MAX_SIZE)
    }
}

impl Sessions {
    /// `QrSessions(maxSize)`。容量 0 当成 1（Kotlin 那边传 0 等于"登记一条就挤一条"，
    /// 但 `jar()` 的 `getOrPut` 仍然能用；这里按"至少留一条"实现，和 maxSize=0 的实际效果一致）。
    pub fn new(max: usize) -> Self {
        Sessions {
            max: max.max(1),
            order: VecDeque::new(),
        }
    }

    pub fn max_size(&self) -> usize {
        self.max
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn contains(&self, token: &str) -> bool {
        self.order.iter().any(|t| t == token)
    }

    /// 当前顺序（最久没访问的在前）——排障 / 测试用。
    pub fn tokens(&self) -> Vec<String> {
        self.order.iter().cloned().collect()
    }

    /// 把 [token] 排到最新（`LinkedHashMap` 的 access order）。
    fn touch(&mut self, token: &str) {
        if let Some(at) = self.order.iter().position(|t| t == token) {
            self.order.remove(at);
        }
        self.order.push_back(token.to_string());
    }

    /// 插入 / 替换后挤掉最久没访问的那条（返回被挤掉的键）。
    fn trim(&mut self) -> Option<String> {
        if self.order.len() > self.max {
            self.order.pop_front()
        } else {
            None
        }
    }

    /// `QrSessions.begin(token, jar)`：登记（同一个 token 重复登记会覆盖并排到最新）。
    /// 返回被挤掉的 token（宿主要把它那份罐子删掉）。
    pub fn begin(&mut self, token: &str) -> Option<String> {
        self.touch(token);
        self.trim()
    }

    /// `QrSessions.jar(token)`：已有的算一次访问、不挤人（宿主那边罐子不变）；
    /// 没有的排到最新，可能挤掉别人（宿主那边新建一个空罐）。
    ///
    /// 返回 `(had_it, evicted)`：`had_it` = 登记表原来有没有它（宿主要不要新建罐子），
    /// `evicted` = 被挤掉的 token。
    pub fn jar(&mut self, token: &str) -> (bool, Option<String>) {
        let had = self.contains(token);
        self.touch(token);
        (had, self.trim())
    }

    /// `QrSessions.end(token)`：终态后丢掉，返回它原来在不在。
    pub fn end(&mut self, token: &str) -> bool {
        match self.order.iter().position(|t| t == token) {
            Some(at) => {
                self.order.remove(at);
                true
            }
            None => false,
        }
    }
}

