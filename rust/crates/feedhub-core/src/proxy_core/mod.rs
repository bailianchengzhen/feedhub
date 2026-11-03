//! **内置代理核心**：`core/src/main/kotlin/feedhub/core/proxy/` 那一包的 Rust 版。
//!
//! 和 [`crate::proxy`] 的区别（**别混**）：
//! - [`crate::proxy`] 回答"**这个平台这次该走哪个代理**"；
//! - 这一层是"**内置那个 core 本身**"：订阅解析、分享链接、sing-box 配置生成、`proxy-core.json` 的读写、
//!   二进制校验、子进程的起停与退避重启、延迟测试、「只在前台跑」的判定表。
//!
//! ## 边界
//!
//! 其余全在这里：**决策、状态机、缓存、持久化格式、解析 / 生成、排队 / 退避策略**。
//!
//! ## 模块
//!
//! - [`json_text`]：给 `proxy-core.json` 用的保序 JSON 写出（4 空格缩进 + Java 的 `Double.toString` 口径）；
//! - [`link_text`]：百分号解码 / base64 / 按行取条目 / 拆分享链接；
//! - [`share_links`]：六种分享链接 → sing-box 出站；
//! - [`clash`]：Clash / mihomo YAML 子集 → 出站；
//! - [`subscription`]：三种格式的识别与解析、`subscription-userinfo`；
//! - [`model`]：状态 / 节点 / 订阅的模型 + `proxy-core.json` 的逐字节读写 + 节点 id；
//! - [`config`]：sing-box 配置生成 + 起核心之前的自检；
//! - [`binary`]：二进制定位 / sha256 / 版本号；
//! - [`subscriptions`]：订阅与节点的增删改合并、取订阅的请求计划与响应处理；
//! - [`foreground`]：「只在前台跑」的判定表（纯状态机，时钟注入）；
//! - [`supervisor`]：核心子进程的状态机（起停 / 健康检查 / 退避重启 / 延迟测试 / 日志）。

pub mod binary;
pub mod clash;
pub mod config;
pub mod foreground;
pub mod json_text;
pub mod link_text;
pub mod model;
pub mod share_links;
pub mod subscription;
pub mod subscriptions;
pub mod supervisor;

pub use model::{
    is_blank, node_id_of, read_text, save_state, write_private, ImportReport, ProxyCoreState,
    ProxyNode, SkippedEntry, Subscription, SubscriptionRoute, SubscriptionUserInfo, AUTO_UPDATE_HOURS,
    CORE_LOG_LEVELS, DEFAULT_SUBSCRIPTION_UA, DEFAULT_TEST_URL, NO_NODES_REASON,
};
pub use subscription::{detect_node_source_format, parse_node_source, NodeSourceFormat, ParsedNodes};
pub use subscriptions::FetchedSubscription;
pub use supervisor::{
    clean_log_line, network_reason, CoreStatus, CoreSupervisor, LatencyResult, MAX_LOG_LINES,
    POLL_INTERVAL_MS,
};

