//! **feedhub 的联网核心**。
//!
//! 这一 crate 是"真正的收发进 Rust"那一条：TLS 握手（BoringSSL）、HTTP/2、HTTP/1.1、连接池、
//! 重定向、代理、解压、超时、取消。**它是整个仓库里唯一允许出现网络栈的地方**
//! （`tokio` / `wreq` / `btls` / `http2`）；`feedhub-core` / `feedhub-ffi` 仍然一条网络依赖都没有。
//!
//! # 分工
//!
//! **当前不接进产品路径**：`HostHttp` / OkHttp 那条链路一行不动，
//! Cookie 和限速以 trait 注入（[`CookieHost`] / [`LimiterHost`]）。
//!
//! # 依赖为什么是这几个
//!
//! - **`wreq`**（Apache-2.0，`=0.16.1`）：reqwest 的硬 fork，带指纹模拟的旋钮（TLS 套件 / 扩展顺序 /
//!   h2 SETTINGS 顺序 / 伪头顺序 / 首流号）。连接池 / 重定向 / 代理 / 解压 / 超时**全部用它自带的**，
//!   这一层不再重写一遍。
//! - **`btls` / `btls-sys`**（Apache-2.0 / MIT，`=0.5.6`）：BoringSSL 绑定。它的 BoringSSL 打了补丁，
//!   带 **ClientHello padding**（上游 `boring` 已经删了那段，少了它 JA3 就和 Conscrypt 不一样）和
//!   **固定扩展顺序**（`SSL_CTX_set_extension_order`）。
//! - **`http2`**（MIT，`=0.5.20`）：wreq 维护的 h2 fork，有 `SettingsOrder` / `PseudoOrder` /
//!   `initial_stream_id`。**打了一个一行的补丁**（Cookie 也进 HPACK 动态表），见
//!   `patches/http2-cookie-index.patch`。
//! - **`tokio`**：wreq 要它。规矩是**闲置零唤醒**：一个全局共享的小运行时（[`rt`]），
//!   从同步入口 `block_on`，不起周期定时器、不发 ping。
//!
//! # 红线
//!
//! 风控（403 / 412 / 429 / 验证码）在这一层**不处理、不重试、不换参数**：这一层只负责把请求发出去、
//! 把响应原样带回来；判定在平台层（`feedhub-core/src/<平台>/`）。这一层唯一会重试的是
//! "连接建立阶段失败"和"刚复用的连接立刻 EOF"（OkHttp `retryOnConnectionFailure` 的语义）。

pub mod client;
pub mod error;
pub mod hosts;
pub mod profile;
pub mod proxy;
pub mod roots;
pub mod rt;

pub use self::{
    client::{
        offline_blocked_via, offline_blocked_with, AfterLimit, CancelToken, NetClient, NetClientBuilder, NetRequest,
        NetResponse, NetStream, PostHeaders, PreSend, Protocol,
    },
    error::{NetError, NetworkKind},
    hosts::{
        CookieHost, CookieSpec, LimitRejected, LimiterHost, NoCookies, NoLimiter, Priority,
    },
    profile::TlsProfile,
    proxy::{Proxy, ProxyKind, ProxyPlan},
};
