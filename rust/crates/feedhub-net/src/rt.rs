//! **一个全局共享的小 tokio 运行时**。
//!
//! wreq 要 tokio。规矩是**闲置零唤醒**：所以
//!
//! - **一个** runtime，进程级一份（[`shared`]），懒建、建一次；
//! 真正的解析和
//!   业务都在调用线程上；
//! - **不起**周期定时器、**不发** PING（h2 keep-alive 关着）、**不跑**后台任务。
//!   空闲时除了"调用方 block_on 结束"以外没有任何唤醒源。
//!
//! 调用方式：同步入口用 [`block_on`]。
//! 这和现在"一次请求占一个 Kotlin `Dispatchers.IO` 线程"的形状一致。
//!
//! 为什么不是 `current_thread` runtime：`current_thread` 要求所有 IO 都从 `block_on` 那一个线程驱动，
//! 而我们要让"调用线程 A 正在 block_on 请求 1"时"线程 B 也能同时 block_on 请求 2"——
//! 那需要一个能被多线程共享的计时器 / IO 驱动器。
//! N 个调用线程各自 block_on**。

use std::sync::OnceLock;
use std::time::Duration;

use tokio::runtime::{Builder, Handle, Runtime};

pub const WORKER_THREADS: usize = 1;

/// 进程级那一份 runtime。
static RT: OnceLock<Runtime> = OnceLock::new();

/// 取（必要时建）共享 runtime 的 [`Handle`]。
///
/// 建失败（线程起不来）会 panic——这是进程级的基础设施，起不来就什么都做不了，
/// 让它响亮地失败比"悄悄退化成同步阻塞"好。
pub fn shared() -> &'static Handle {
    static HANDLE: OnceLock<Handle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let rt = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(WORKER_THREADS)
            .thread_name("feedhub-net")
            .build()
            .expect("建不了 feedhub-net 的 tokio 运行时（线程 / epoll 不可用）");
        RT.get_or_init(|| rt).handle().clone()
    })
}

/// 在共享 runtime 上把 `future` 跑到完成（**阻塞当前调用线程**）。
///
/// 这是同步入口唯一该用的入口：和 OkHttp 的 `execute()` 一样"一次请求占一个线程"。
///
/// 这一层所有调用都来自 `block_on` 的调用方线程，
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    shared().block_on(future)
}

/// 跑 `future`，但最多等 `timeout`。
///
/// 超时返回 `None`（调用方翻成 [`crate::NetError::Timeout`]）。
pub fn block_on_timeout<F: std::future::Future>(
    timeout: Duration,
    future: F,
) -> Option<F::Output> {
    block_on(async move { tokio::time::timeout(timeout, future).await.ok() })
}

/// 这个进程的 runtime 起来了没有（测试用：断言"没有网络活动就不建 runtime"）。
pub fn is_started() -> bool {
    RT.get().is_some()
}

