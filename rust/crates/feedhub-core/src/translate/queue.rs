//! 翻译请求的排队与并发（`app/.../translate/TranslationScheduler.kt` 的状态机那一半）。
//!
//! 规则：
//! - **不占线程**：等并发位 / 等结果都在宿主那边靠协程挂起（`CompletableDeferred.await()`），
//!   这边只回答"现在能不能派发"；
//! - **并发上限** = llama-server 的 `-np`；
//! - **新的优先**：还没开跑的请求排在队首（[`Queue::submit`] 用 `push_front`），最近划到屏幕上的先翻；
//! - **跟着界面走**：调用方的协程被取消（卡片 / 评论离开组合）时，**还没派发**的那条从队列里拿掉；
//!   已经派发的照常跑完（结果由宿主照常写缓存）；
//! - **同一个键的多个请求共用一份工作**（去重），谁先来谁登记回调。
//!
//! 为什么拆成 submit / pump / finish 三步：Rust 这边**不能自己跑协程**（`.so` 里没有执行器），
//! 真正的"跑"必须在宿主上（要给 HTTP 让出线程）。所以这里是**状态机**：
//! 宿主问 [`Queue::pump`]"现在该跑哪几个"，跑完调 [`Queue::finish`]，取消调 [`Queue::abandon`]。
//!
//! 线程安全：一个 `Mutex` 包住全部状态（宿主可能从不同线程调；`pump` 返回的是快照，
//! 跑的时候不持锁）。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// 一条要翻的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// 队列内的自增编号（宿主用它把"跑完了 / 取消了"对回来）。
    pub id: u64,
    /// 去重键（[`crate::translate::cache::key_for`] 算出来的 SHA-256）。
    pub key: String,
    /// 要翻的正文。
    pub text: String,
    /// 目标语言（设置里的显示名）。
    pub target: String,
}

/// [`Queue::submit`] 的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Submit {
    /// `true` = 这次新建了一条（调用方要登记回调）；`false` = 这个键已经在排队 / 在跑（共用）。
    pub created: bool,
    /// 这一条在队列里的编号。
    pub id: u64,
}

struct Item {
    job: Job,
    /// 有几个调用方在等这一条（Kotlin 的 `waiters`）。
    waiters: u32,
    /// 已经派发出去（在跑了）。
    dispatched: bool,
}

impl Item {
    fn new(job: Job) -> Item {
        Item {
            job,
            waiters: 1,
            dispatched: false,
        }
    }
}

struct State {
    next_id: u64,
    /// 还没派发的请求；**队首是最近请求的**。
    pending: VecDeque<u64>,
    /// 全部活着（排队 + 在跑）的条目。
    items: HashMap<u64, Item>,
    /// 去重用：键 → 编号。
    active: HashMap<String, u64>,
    /// 正在跑的条数。
    running: usize,
}

/// 排队器的状态机（见模块说明）。
pub struct Queue {
    parallelism: usize,
    state: Mutex<State>,
}

impl Queue {
    /// [`parallelism`] = llama-server 的 `-np`；`0` 按 1 算（和 Kotlin 的 `coerceAtLeast` 一样不会卡死）。
    pub fn new(parallelism: usize) -> Queue {
        Queue {
            parallelism: parallelism.max(1),
            state: Mutex::new(State {
                next_id: 1,
                pending: VecDeque::new(),
                items: HashMap::new(),
                active: HashMap::new(),
                running: 0,
            }),
        }
    }

    pub fn parallelism(&self) -> usize {
        self.parallelism
    }

    /// 正在跑的条数。
    pub fn running(&self) -> usize {
        self.state.lock().unwrap().running
    }

    /// 排队中的条数（不含在跑的）。
    pub fn pending(&self) -> usize {
        self.state.lock().unwrap().pending.len()
    }

    /// 登记一条请求。已经有同一个键在排队 / 在跑时**共用**那一份（`created == false`）。
    pub fn submit(&self, key: &str, text: &str, target: &str) -> Submit {
        let mut st = self.state.lock().unwrap();
        if let Some(id) = st.active.get(key).copied() {
            if let Some(item) = st.items.get_mut(&id) {
                item.waiters += 1;
                return Submit {
                    created: false,
                    id,
                };
            }
            // 理论上到不了这里（active 和 items 一起维护）；防御一下，别把键永久占住
            st.active.remove(key);
        }
        let id = st.next_id;
        st.next_id += 1;
        let item = Item::new(Job {
            id,
            key: key.to_string(),
            text: text.to_string(),
            target: target.to_string(),
        });
        st.items.insert(id, item);
        st.active.insert(key.to_string(), id);
        st.pending.push_front(id);
        Submit { created: true, id }
    }

    /// 在并发上限内把队列里**最近的**几条派发出去（返回要跑的作业，可能为空）。
    pub fn pump(&self) -> Vec<Job> {
        let mut st = self.state.lock().unwrap();
        let mut out = Vec::new();
        while st.running < self.parallelism {
            let Some(id) = st.pending.pop_front() else {
                break;
            };
            let job = match st.items.get_mut(&id) {
                Some(item) => {
                    item.dispatched = true;
                    item.job.clone()
                }
                None => continue,
            };
            st.running += 1;
            out.push(job);
        }
        out
    }

    /// 一条跑完了（成功 / 失败都算）：腾出并发位、把键还回去。
    pub fn finish(&self, id: u64) {
        let mut st = self.state.lock().unwrap();
        if let Some(item) = st.items.remove(&id) {
            st.running = st.running.saturating_sub(1);
            if st.active.get(&item.job.key) == Some(&id) {
                st.active.remove(&item.job.key);
            }
        }
    }

    /// 调用方在"还没派发"时走了：把排队项拿掉（返回 `true` = 真的拿掉了）。
    ///
    /// 已经派发的不动（让它跑完，结果仍由宿主写缓存）；同一个键还有别人在等时也不拿掉。
    pub fn abandon(&self, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        let Some(item) = st.items.get_mut(&id) else {
            return false;
        };
        item.waiters = item.waiters.saturating_sub(1);
        if item.waiters > 0 || item.dispatched {
            return false;
        }
        let key = item.job.key.clone();
        st.items.remove(&id);
        st.pending.retain(|it| *it != id);
        if st.active.get(&key) == Some(&id) {
            st.active.remove(&key);
        }
        true
    }

    /// 进程退出 / 关翻译：把还排着队的全部取消，返回被取消的编号（在跑的不动）。
    pub fn cancel_pending(&self) -> Vec<u64> {
        let mut st = self.state.lock().unwrap();
        let ids: Vec<u64> = st.pending.drain(..).collect();
        for id in &ids {
            if let Some(item) = st.items.remove(id) {
                if st.active.get(&item.job.key) == Some(id) {
                    st.active.remove(&item.job.key);
                }
            }
        }
        ids
    }
}

