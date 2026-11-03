//! **内置代理核心**过 UniFFI 边界。
//!
//! Rust 侧是 [`feedhub_core::proxy_core`]；这一层只有 record / enum、一个平台对象
//! [`FfiProxyCore`]、以及**两个宿主端口**：
//!
//! **定时器不在 Rust**：宿主按 [`FfiProxyCore::poll_interval_ms`] 的节奏调 [`FfiProxyCore::poll`]；
//! 「只在前台跑」的宽限期也由宿主 `delay` + [`FfiProxyCore::foreground_poll`] 推进。
//! `.so` 里因此**没有常驻线程**。起 sing-box **不等它 bind**：起完进程就回
//! `Starting`、放开 `inner` 锁，探活由宿主的 `poll` 推进（`next_wake_delay_ms` 这时给 60ms）——
//! 原来整段同步等最多 15 秒、一直持锁，主线程上任何一次调用都会跟着卡。
//!
//! ## 机密
//!
//! `proxy-core.json` 的内容（订阅地址、节点 uuid / 密码）**会过边界**——宿主本来就要把它写到磁盘、
//! 也没打算藏起来（文件权限 600）。但：
//!
//! - 错误文案、日志、`FfiProxyCoreSkip.reason` 里**绝不出现**订阅地址 / 密码 / uuid（见各处的文案）；
//! - 节点出站的 JSON 用**字符串**过边界（`FfiProxyCoreNode.outbound_json`），宿主拿它去展示 / 算 id 时
//!   不解析也行；Rust 侧不为此写第二套 JSON 解析器（[`feedhub_core::xhs::jsonord`] 那份就够了）。

use std::sync::{Arc, Mutex, PoisonError};

use feedhub_net::{NetClient, NetError, NetRequest, Proxy, ProxyPlan};
use feedhub_core::proxy_core::{
    self as core, config, foreground::ForegroundGate, json_text, model, subscription, subscriptions,
    supervisor,
};

// ===========================================================================
// 错误
// ===========================================================================

/// 代理核心的错误。字段不叫 `message`。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiProxyCoreError {
    /// 状态 / 参数不对（"没有这个订阅" / "端口不在范围" / "一个节点都没有"）：界面直接显示。
    #[error("{detail}")]
    Invalid { detail: String },

    /// 写 `proxy-core.json` / 生成的配置 / 探针配置失败。**不含文件内容**。
    #[error("{detail}")]
    Io { detail: String },

    /// 内部错误（锁中毒 / panic 兜底）。
    #[error("{detail}")]
    Internal { detail: String },
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ===========================================================================
// record / enum
// ===========================================================================

/// 取订阅走哪条路（Kotlin `SubscriptionRoute`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProxyCoreRoute {
    Direct,
    Core,
    System,
}

impl FfiProxyCoreRoute {
    fn from_core(route: core::SubscriptionRoute) -> Self {
        match route {
            core::SubscriptionRoute::Direct => Self::Direct,
            core::SubscriptionRoute::Core => Self::Core,
            core::SubscriptionRoute::System => Self::System,
        }
    }

    fn to_core(self) -> core::SubscriptionRoute {
        match self {
            Self::Direct => core::SubscriptionRoute::Direct,
            Self::Core => core::SubscriptionRoute::Core,
            Self::System => core::SubscriptionRoute::System,
        }
    }
}

/// 一个节点。**`outbound_json` 是机密**（里面有 uuid / 密码）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreNode {
    pub id: String,
    pub name: String,
    /// sing-box 出站类型（`vless` / `shadowsocks` / …）。
    pub kind: String,
    pub server: String,
    pub port: i32,
    /// sing-box 的出站对象（**保序、紧凑**的 JSON 文本，不含 `tag`）。**机密**。
    pub outbound_json: String,
    pub subscription_id: Option<String>,
    pub last_latency_ms: Option<i32>,
    pub last_latency_at: Option<i64>,
}

impl FfiProxyCoreNode {
    fn from_core(node: &core::ProxyNode) -> Self {
        FfiProxyCoreNode {
            id: node.id.clone(),
            name: node.name.clone(),
            kind: node.kind.clone(),
            server: node.server.clone(),
            port: node.port,
            // 紧凑写（不 pretty）：宿主只在极少的地方需要原文（截图 / 排障），不为此多写一份漂亮版本
            outbound_json: compact_json(&node.outbound),
            subscription_id: node.subscription_id.clone(),
            last_latency_ms: node.last_latency_ms,
            last_latency_at: node.last_latency_at,
        }
    }

    fn to_core(&self) -> Result<core::ProxyNode, FfiProxyCoreError> {
        let outbound = parse_json(&self.outbound_json)?;
        Ok(core::ProxyNode {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            server: self.server.clone(),
            port: self.port,
            outbound,
            subscription_id: self.subscription_id.clone(),
            last_latency_ms: self.last_latency_ms,
            last_latency_at: self.last_latency_at,
        })
    }
}

/// 订阅的流量 / 到期信息（HTTP 响应头 `subscription-userinfo`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreUserInfo {
    pub upload: Option<i64>,
    pub download: Option<i64>,
    pub total: Option<i64>,
    /// Unix 秒。
    pub expire: Option<i64>,
}

impl FfiProxyCoreUserInfo {
    fn from_core(info: &core::SubscriptionUserInfo) -> Self {
        FfiProxyCoreUserInfo {
            upload: info.upload,
            download: info.download,
            total: info.total,
            expire: info.expire,
        }
    }

    fn to_core(&self) -> core::SubscriptionUserInfo {
        core::SubscriptionUserInfo {
            upload: self.upload,
            download: self.download,
            total: self.total,
            expire: self.expire,
        }
    }
}

/// 一个订阅。**`url` 是机密**（订阅地址本身就是凭据）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreSubscription {
    pub id: String,
    pub name: String,
    pub url: String,
    pub added_at: i64,
    pub last_update_at: Option<i64>,
    pub last_error: Option<String>,
    pub node_count: i32,
    pub user_info: Option<FfiProxyCoreUserInfo>,
    pub auto_update_hours: i32,
}

impl FfiProxyCoreSubscription {
    fn from_core(sub: &core::Subscription) -> Self {
        FfiProxyCoreSubscription {
            id: sub.id.clone(),
            name: sub.name.clone(),
            url: sub.url.clone(),
            added_at: sub.added_at,
            last_update_at: sub.last_update_at,
            last_error: sub.last_error.clone(),
            node_count: sub.node_count,
            user_info: sub.user_info.as_ref().map(FfiProxyCoreUserInfo::from_core),
            auto_update_hours: sub.auto_update_hours,
        }
    }

    fn to_core(&self) -> core::Subscription {
        core::Subscription {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            added_at: self.added_at,
            last_update_at: self.last_update_at,
            last_error: self.last_error.clone(),
            node_count: self.node_count,
            user_info: self.user_info.as_ref().map(FfiProxyCoreUserInfo::to_core),
            auto_update_hours: self.auto_update_hours,
        }
    }
}

/// 内置核心的完整状态（`proxy-core.json` 的内容）。**机密**。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreState {
    pub subscriptions: Vec<FfiProxyCoreSubscription>,
    pub nodes: Vec<FfiProxyCoreNode>,
    pub selected_node_id: Option<String>,
    pub test_url: String,
    pub log_level: String,
    pub subscription_route: FfiProxyCoreRoute,
    pub subscription_ua: String,
}

impl FfiProxyCoreState {
    fn from_core(state: &core::ProxyCoreState) -> Self {
        FfiProxyCoreState {
            subscriptions: state
                .subscriptions
                .iter()
                .map(FfiProxyCoreSubscription::from_core)
                .collect(),
            nodes: state.nodes.iter().map(FfiProxyCoreNode::from_core).collect(),
            selected_node_id: state.selected_node_id.clone(),
            test_url: state.test_url.clone(),
            log_level: state.log_level.clone(),
            subscription_route: FfiProxyCoreRoute::from_core(state.subscription_route),
            subscription_ua: state.subscription_ua.clone(),
        }
    }

    fn to_core(&self) -> Result<core::ProxyCoreState, FfiProxyCoreError> {
        let nodes = self
            .nodes
            .iter()
            .map(FfiProxyCoreNode::to_core)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(core::ProxyCoreState {
            subscriptions: self.subscriptions.iter().map(|s| s.to_core()).collect(),
            nodes,
            selected_node_id: self.selected_node_id.clone(),
            test_url: self.test_url.clone(),
            log_level: self.log_level.clone(),
            subscription_route: self.subscription_route.to_core(),
            subscription_ua: self.subscription_ua.clone(),
        })
    }
}

/// 一条被跳过的条目：第几条、为什么（**不回显原文**）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreSkip {
    pub index: i32,
    pub reason: String,
}

/// 导入结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreImportReport {
    pub added: i32,
    pub updated: i32,
    pub skipped: Vec<FfiProxyCoreSkip>,
    /// 界面上那一句（`新增 2 条，更新 1 条，跳过 1 条`）。
    pub summary: String,
}

/// 订阅内容的三种格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProxyCoreSourceFormat {
    SingBoxJson,
    ClashYaml,
    ShareLinks,
}

impl FfiProxyCoreSourceFormat {
    fn from_core(format: subscription::NodeSourceFormat) -> Self {
        match format {
            subscription::NodeSourceFormat::SingBoxJson => Self::SingBoxJson,
            subscription::NodeSourceFormat::ClashYaml => Self::ClashYaml,
            subscription::NodeSourceFormat::ShareLinks => Self::ShareLinks,
            subscription::NodeSourceFormat::Unknown => Self::ShareLinks,
        }
    }
}

/// 一次解析的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreParsed {
    pub nodes: Vec<FfiProxyCoreNode>,
    pub skipped: Vec<FfiProxyCoreSkip>,
    pub format: FfiProxyCoreSourceFormat,
}

/// 核心的运行状态（Kotlin `CoreStatus`）。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiProxyCoreStatus {
    Stopped,
    Starting { attempt: i32 },
    Running {
        port: i32,
        version: Option<String>,
        pid: i64,
        started_at: i64,
    },
    Restarting {
        attempt: i32,
        delay_ms: i64,
        reason: String,
    },
    Failed { message: String },
}

impl FfiProxyCoreStatus {
    fn from_core(status: &supervisor::CoreStatus) -> Self {
        match status {
            supervisor::CoreStatus::Stopped => Self::Stopped,
            supervisor::CoreStatus::Starting { attempt } => Self::Starting { attempt: *attempt },
            supervisor::CoreStatus::Running {
                port,
                version,
                pid,
                started_at,
            } => Self::Running {
                port: *port,
                version: version.clone(),
                pid: *pid,
                started_at: *started_at,
            },
            supervisor::CoreStatus::Restarting {
                attempt,
                delay_ms,
                reason,
            } => Self::Restarting {
                attempt: *attempt,
                delay_ms: *delay_ms,
                reason: reason.clone(),
            },
            supervisor::CoreStatus::Failed { message } => Self::Failed {
                message: message.clone(),
            },
        }
    }
}

/// 一次延迟测试的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreLatency {
    pub node_id: String,
    pub ms: Option<i32>,
    pub error: Option<String>,
}

/// 「内置核心」这一档的走法：`port` 有值 = 在跑，`None` = 没在跑（`reason` 是不含机密的文案，宿主据此 fail-closed）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreMode {
    pub port: Option<i32>,
    pub reason: String,
}

/// 取订阅这一步宿主该做什么。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiProxyCoreFetchPlan {
    /// 发这个请求。
    Fetch {
        url: String,
        user_agent: String,
        route: FfiProxyCoreRoute,
        /// `route = Core` 时用哪个端口派生客户端（别的路线忽略）。
        core_port: i32,
    },
    /// 不发（"经核心"但核心没在跑）。`reason` 已经是不含机密的文案。
    Refused { reason: String },
    /// 订阅不在列表里了。
    NotFound,
}

/// 取订阅拿回来的东西（宿主填）。`status` 是 HTTP 状态码。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreFetched {
    pub status: u16,
    pub body: String,
    /// 响应头 `subscription-userinfo` 的原值（没有就是 `None`）。
    pub user_info_header: Option<String>,
}

/// 取订阅的结果。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiProxyCoreUpdate {
    /// 成功：新状态 + 解析到几条。
    Ok { state: FfiProxyCoreState, parsed: i32 },
    /// 失败：新状态（记了 `last_error`）+ 给界面看的原因。
    Failed { state: FfiProxyCoreState, reason: String },
}

// ===========================================================================
// 宿主端口
// ===========================================================================

/// **取订阅的收发**：Rust 决定"发什么请求 / 走哪条路"，
/// 这一层用 [`feedhub_net::NetClient`] 发。
///
/// 三条路线各一档代理（和 `ProxyClients` 一一对应）：
///
/// | `route` | 代理档 | 以前宿主用的客户端 |
/// |---|---|---|
/// | `Direct` | [`Proxy::Direct`]（**显式直连**，绝不掉回系统代理） | `direct`（`Proxy.NO_PROXY`） |
/// | `System` | 宿主按 `ProxyMode.System` 的同一口径解析好传进来 | `system`（`ctx.http`） |
/// | `Core` | [`Proxy::Socks5`] → `127.0.0.1:<core_port>` | `base` + SOCKS5 代理 |
///
/// 失败**只报一个分类串**（`unknown_host` / `connect` / `timeout` / 别的），由
/// `subscriptions::subscription_network_error` 换成界面文案——**绝不把错误正文透给用户**：
/// 里面常带订阅主机名。
pub struct SubscriptionFetcher {
    client: Arc<NetClient>,
}

impl SubscriptionFetcher {
    fn new() -> Result<Self, NetError> {
        Ok(Self {
            client: Arc::new(NetClient::builder().proxies(ProxyPlan::direct()).build()?),
        })
    }

    /// 发这一趟。`route` 是 Rust 定的；`core_port` 只在 `Core` 时有意义；
    /// `system_proxy` 是宿主为**这条订阅的地址**解析出来的 `System` 档（见 `FfiProxyCore::set_system_proxy`）。
    fn fetch(
        &self,
        url: String,
        user_agent: String,
        route: FfiProxyCoreRoute,
        core_port: i32,
        system_proxy: Proxy,
    ) -> Result<FfiProxyCoreFetched, NetError> {
        let proxy = match route {
            FfiProxyCoreRoute::Direct => Proxy::Direct,
            FfiProxyCoreRoute::System => system_proxy,
            FfiProxyCoreRoute::Core => Proxy::Socks5 {
                host: "127.0.0.1".to_string(),
                port: u16::try_from(core_port).unwrap_or(0),
            },
        };
        let mut req = NetRequest::get("subscription", &url);
        // **只有 UA**：这条路上没有 Cookie、没有平台默认头、不占平台限速（和宿主那份 OkHttp 一样）。
        req.headers = vec![("User-Agent".to_string(), user_agent)];
        req.proxy = Some(proxy);
        let resp = self.client.execute(req)?;
        Ok(FfiProxyCoreFetched {
            status: resp.status as u16,
            body: String::from_utf8_lossy(&resp.body).into_owned(),
            user_info_header: resp
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("subscription-userinfo"))
                .map(|(_, v)| v.clone()),
        })
    }
}

/// **状态 / 日志的变化**推给宿主（界面用）。同一个值不重复推。
///
/// 为什么不用 `poll()` 的返回值：界面要在**日志**变化时也刷新（日志是第二条流），而 `poll()` 只回答
/// "状态变了没有"。两条流各推各的，宿主接成两个 `MutableStateFlow` 就行了。
#[uniffi::export(with_foreign)]
pub trait FfiProxyCoreNotify: Send + Sync {
    fn status_changed(&self, status: FfiProxyCoreStatus);

    /// 最近若干行日志（**全量**，最多 200 行）——比"追加一行"简单，也不怕宿主漏一条。
    fn logs_changed(&self, lines: Vec<String>);

    /// 子进程意外退出或（在观察日志时）新增了子进程输出时唤醒宿主事件协程。
    fn wake(&self);
}

// ===========================================================================
// 平台对象
// ===========================================================================

/// 内置代理核心的宿主侧对象。**线程安全**（内部一把锁；宿主在任意线程上同步调）。
///
/// 一个 App 只建一个（`ProxyCoreStore` 也只有一份）。
#[derive(uniffi::Object)]
pub struct FfiProxyCore {
    inner: Mutex<Inner>,
    /// 取订阅的收发。
    fetcher: SubscriptionFetcher,
    /// `System` 那一档取订阅时用的代理（宿主**按订阅地址的主机名**解析好后喂进来；`None` = 直连）。
    system_proxy: Mutex<Option<Proxy>>,
    logs_subscribed: std::sync::atomic::AtomicBool,
    notify: Arc<dyn FfiProxyCoreNotify>,
}

struct Inner {
    supervisor: supervisor::CoreSupervisor,
    gate: ForegroundGate,
    /// 上一次推给宿主的状态 / 日志序号（变化了才推）。
    last_status: Option<supervisor::CoreStatus>,
    last_logs_seq: u64,
    /// 宿主注入的时钟（毫秒）：`advance_clock` / `now_ms` 由宿主传，宽限期和"跑够 stable_ms"都靠它。
    clock_ms: i64,
}

impl FfiProxyCore {
    fn with_inner<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = lock(&self.inner);
        f(&mut guard)
    }
}

#[uniffi::export]
impl FfiProxyCore {
    /// 建一个核心对象。
    ///
    /// - `binary`：sing-box 可执行文件（宿主按 `binary_locate` 找好再传进来）；
    /// - `data_dir`：放生成的配置和 `proxy-core.json` 的目录；
    /// - `state_file`：`proxy-core.json` 的路径（`None` = 只在内存里，测试 / 截图用）；
    /// - `expected_sha256`：期望的哈希（`None` = 不校验；宿主用 `binary_expected_sha256` 算好传进来）；
    /// - `now_ms`：宿主的时钟（毫秒）；
    /// **取订阅的收发不经过宿主**：`feedhub-net` 直接发。`System` 那一档的代理由
    /// [`FfiProxyCore::set_system_proxy`] 单独喂（它要**按订阅地址的主机名**去问 JVM 的选择器，
    /// 不能在建对象时一次定死）。
    #[uniffi::constructor]
    pub fn new(
        binary: String,
        data_dir: String,
        state_file: Option<String>,
        expected_sha256: Option<String>,
        now_ms: i64,
        notify: Arc<dyn FfiProxyCoreNotify>,
    ) -> Result<Arc<Self>, FfiProxyCoreError> {
        let data_dir = std::path::PathBuf::from(data_dir);
        // 状态从磁盘读（文件不在 / 坏了 → 空状态，和 Kotlin 的 `ProxyCoreStore` 一样）
        let state = match &state_file {
            Some(path) => model::load_state(std::path::Path::new(path)),
            None => core::ProxyCoreState::default(),
        };
        let options = supervisor::CoreOptions {
            expected_sha256,
            // **这个不能忘**：忘了它 `proxy-core.json` 就永远不落盘（状态只在内存里，
            // 重开 App 全丢）。`ProxyCoreDualRunTest` 的"文件权限 600"那条守着。
            state_file: state_file.as_ref().map(std::path::PathBuf::from),
            ..supervisor::CoreOptions::new(binary, &data_dir)
        };
        let mut supervisor = supervisor::CoreSupervisor::new(options, state);
        let notify_wake = notify.clone();
        supervisor.set_wake_callback(Arc::new(move || {
            notify_wake.wake();
        }));
        let fetcher = SubscriptionFetcher::new().map_err(|e| FfiProxyCoreError::Internal {
            detail: e.to_string(),
        })?;
        Ok(Arc::new(FfiProxyCore {
            inner: Mutex::new(Inner {
                supervisor,
                gate: ForegroundGate::default(),
                last_status: None,
                last_logs_seq: 0,
                clock_ms: now_ms,
            }),
            fetcher,
            system_proxy: Mutex::new(None),
            logs_subscribed: std::sync::atomic::AtomicBool::new(false),
            notify,
        }))
    }

    // ------------------------------------------------------------------ 状态

    /// 现在的完整状态（**机密**）。
    pub fn state(&self) -> FfiProxyCoreState {
        self.with_inner(|inner| FfiProxyCoreState::from_core(inner.supervisor.state()))
    }

    /// 整份替换状态（"改完再存"的那条路，和 `ProxyCoreStore.update` 一样：变了就落盘）。
    /// 返回 `true` = 真的变了。
    pub fn set_state(&self, state: FfiProxyCoreState) -> Result<bool, FfiProxyCoreError> {
        let next = state.to_core()?;
        let changed = self.with_inner(|inner| {
            let config_before = (
                inner.supervisor.state().test_url.clone(),
                inner.supervisor.state().log_level.clone(),
            );
            let changed = inner.supervisor.set_state(next);
            if changed {
                let after = (
                    inner.supervisor.state().test_url.clone(),
                    inner.supervisor.state().log_level.clone(),
                );
                if after != config_before {
                    let now = inner.clock_ms;
                    inner.supervisor.restart_for_config_change(now);
                }
            }
            changed
        });
        self.publish_changes();
        Ok(changed)
    }

    /// `proxy-core.json` 的文本（**逐字节等于 Kotlin 写出来的那份**）。宿主写盘 / 对拍用。
    ///
    /// 生产路径**不需要**它：`set_state` / 各个改状态的方法自己会落盘。它是给"宿主想自己写一份"
    /// 和"对拍比字节"用的。
    pub fn state_json_text(&self) -> String {
        self.with_inner(|inner| inner.supervisor.state().to_json_text())
    }

    /// 从一段 `proxy-core.json` 文本读状态（坏了就是空状态，和 `ProxyCoreStore.load` 一样）。
    pub fn parse_state_json(&self, text: String) -> FfiProxyCoreState {
        FfiProxyCoreState::from_core(&core::ProxyCoreState::from_json_text(&text))
    }

    // ------------------------------------------------------------------ 起停

    /// 设置里选了「内置核心」/ 换成别的档位。宿主**不要**直接调它——走 [`Self::set_enabled`] 的前台判定
    /// （安卓）；桌面没有"退到后台"这回事，可以直接调。
    pub fn set_desired_running(&self, enabled: bool) -> FfiProxyCoreStatus {
        let status = self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.set_desired_running(enabled, now);
            inner.supervisor.status().clone()
        });
        self.publish_changes();
        FfiProxyCoreStatus::from_core(&status)
    }

    /// **「只在前台跑」的入口**：设置里选了「内置核心」/ 换档位（`enabled`）。
    ///
    /// 返回"现在该不该跑"（`Some` + `bool` 表示**变了**，`None` = 没变）。宿主据此调
    /// [`Self::set_desired_running`]。前台 / 后景事件用 [`Self::set_foreground`]。
    pub fn set_enabled(&self, enabled: bool) -> Option<bool> {
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.gate.set_enabled(enabled, now)
        })
    }

    /// App 进前台 / 退到后台。返回"现在该不该跑"（含义同 [`Self::set_enabled`]）。
    pub fn set_foreground(&self, foreground: bool) -> Option<bool> {
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.gate.set_foreground(foreground, now)
        })
    }

    /// 推进宽限期（宿主每几百毫秒调一次）。返回 `Some(false)` = 该停了。
    pub fn foreground_poll(&self) -> Option<bool> {
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.gate.poll(now)
        })
    }

    /// 宽限期还剩多少毫秒（宿主可以据此少醒几次）。
    pub fn foreground_pending_stop_ms(&self) -> Option<i64> {
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.gate.pending_stop_in_ms(now)
        })
    }

    /// 宿主报时钟（毫秒）。**宽限期 / "跑够 stable_ms" / 测速时间戳都靠它**，所以宿主每次 `poll` 前
    /// 都要喂一次当前时间。
    pub fn set_clock(&self, now_ms: i64) {
        self.with_inner(|inner| {
            inner.clock_ms = now_ms;
        });
    }

    /// 推进状态机（子进程死了？退避到点了？跑够 stable_ms 了？）。宿主按 [`Self::poll_interval_ms`] 的
    /// 节奏调，`set_clock` 先喂时间。返回 `true` = 状态变了。
    pub fn poll(&self) -> bool {
        let changed = self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.poll(now)
        });
        self.publish_changes();
        changed
    }

    /// 宿主定时器该多久调一次 `poll`（毫秒）。**只是一个建议值**，Rust 不起定时器。
    pub fn poll_interval_ms(&self) -> i64 {
        supervisor::POLL_INTERVAL_MS
    }

    /// 距离下一次定时状态迁移（退避重启 / 稳定清零 / 后台宽限期停机）还剩多少毫秒；
    /// `None` 表示当前没有任何待到期的定时器，宿主可以完全挂起等待事件通知（零唤醒）。
    pub fn next_wake_delay_ms(&self, now_ms: i64) -> Option<i64> {
        self.with_inner(|inner| {
            inner.clock_ms = now_ms;
            let sup = inner.supervisor.next_wake_delay_ms(now_ms);
            let gate = inner.gate.pending_stop_in_ms(now_ms);
            match (sup, gate) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        })
    }

    /// 告知当前是否有界面在观察日志（未观察时不跨 FFI 推送最多 200 行日志，子进程输出也不唤醒宿主）。
    pub fn set_logs_subscribed(&self, subscribed: bool) {
        self.logs_subscribed
            .store(subscribed, std::sync::atomic::Ordering::Release);
        self.with_inner(|inner| inner.supervisor.set_logs_subscribed(subscribed));
        if subscribed {
            self.publish_changes();
        }
    }

    /// 手动重启。
    pub fn restart(&self) -> FfiProxyCoreStatus {
        let status = self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart(now);
            inner.supervisor.status().clone()
        });
        self.publish_changes();
        FfiProxyCoreStatus::from_core(&status)
    }

    /// 当前状态。
    pub fn status(&self) -> FfiProxyCoreStatus {
        self.with_inner(|inner| FfiProxyCoreStatus::from_core(inner.supervisor.status()))
    }

    /// 「内置核心」这一档的走法（`port` 有值 = 在跑）。
    pub fn proxy_mode(&self) -> FfiProxyCoreMode {
        self.with_inner(|inner| {
            let (port, reason) = inner.supervisor.proxy_mode();
            FfiProxyCoreMode { port, reason }
        })
    }

    /// 在跑没有。
    pub fn running(&self) -> bool {
        self.with_inner(|inner| inner.supervisor.running())
    }

    /// 在跑的端口（没有就是 `None`）。
    pub fn port(&self) -> Option<i32> {
        self.with_inner(|inner| inner.supervisor.port())
    }

    /// 最近 200 行日志。
    pub fn logs(&self) -> Vec<String> {
        self.with_inner(|inner| inner.supervisor.logs())
    }

    pub fn clear_logs(&self) {
        self.with_inner(|inner| inner.supervisor.clear_logs());
        self.publish_changes();
    }

    /// 数据目录（界面显示用）。
    pub fn data_dir(&self) -> String {
        self.with_inner(|inner| inner.supervisor.data_dir().to_string_lossy().into_owned())
    }

    /// 生成的 sing-box 配置路径（**有机密**，权限 600）。
    pub fn config_path(&self) -> String {
        self.with_inner(|inner| inner.supervisor.config_path().to_string_lossy().into_owned())
    }

    /// 关掉（停子进程）。App 退出时调。
    ///
    /// 名字不叫 `close`：UniFFI 生成的 Kotlin 类自带一个 `AutoCloseable.close()`（释放指针），同名编不过。
    pub fn shutdown(&self) {
        self.with_inner(|inner| {
            inner.supervisor.set_desired_running(false, inner.clock_ms);
        });
        self.publish_changes();
    }

    // ------------------------------------------------------------------ 订阅 / 节点

    /// 加一个订阅（同 URL 重复加不会重复）。返回加进去的那条。
    pub fn add_subscription(
        &self,
        name: String,
        url: String,
        auto_update_hours: i32,
    ) -> FfiProxyCoreSubscription {
        let sub = self.with_inner(|inner| {
            let now = inner.clock_ms;
            let state = inner.supervisor.state().clone();
            let sub = subscriptions::add_subscription(&state, &name, &url, auto_update_hours, now);
            let next = subscriptions::with_subscription_added(&state, &sub);
            inner.supervisor.set_state(next);
            sub
        });
        self.publish_changes();
        FfiProxyCoreSubscription::from_core(&sub)
    }

    /// 改某条订阅的自动更新间隔（小时，0 = 关）。
    pub fn set_subscription_auto_update(&self, id: String, hours: i32) {
        self.mutate_state(|state| subscriptions::set_subscription_auto_update(state, &id, hours));
    }

    pub fn rename_subscription(&self, id: String, name: String) {
        self.mutate_state(|state| subscriptions::rename_subscription(state, &id, &name));
    }

    /// 删订阅（它的节点一起删，选中的节点没了就清掉）。
    pub fn remove_subscription(&self, id: String) {
        self.mutate_state(|state| subscriptions::remove_subscription(state, &id));
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart_for_config_change(now);
        });
        self.publish_changes();
    }

    pub fn remove_node(&self, id: String) {
        self.mutate_state(|state| subscriptions::remove_node(state, &id));
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart_for_config_change(now);
        });
        self.publish_changes();
    }

    /// 手动选节点；`None` = 自动（延迟最低，`urltest`）。
    pub fn select_node(&self, id: Option<String>) {
        self.mutate_state(|state| subscriptions::select_node(state, id.as_deref()));
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart_for_config_change(now);
        });
        self.publish_changes();
    }

    /// 改测试地址 / 日志级别 / UA / 取订阅路线。
    ///
    /// **不重启核心**（那几个键不影响正在跑的配置）——除了 `testUrl` / `logLevel`：那两个由
    /// [`Self::set_state`] 那条路处理（它会重启）。这个入口是给"改 UA / 路线"用的。
    pub fn update_prefs(
        &self,
        test_url: Option<String>,
        log_level: Option<String>,
        subscription_route: Option<FfiProxyCoreRoute>,
        subscription_ua: Option<String>,
    ) {
        self.with_inner(|inner| {
            let before = inner.supervisor.state().clone();
            let mut next = before.clone();
            if let Some(test_url) = test_url {
                next.test_url = test_url;
            }
            if let Some(log_level) = log_level {
                next.log_level = log_level;
            }
            if let Some(route) = subscription_route {
                next.subscription_route = route.to_core();
            }
            if let Some(ua) = subscription_ua {
                next.subscription_ua = ua;
            }
            if next == before {
                return;
            }
            let restart = next.test_url != before.test_url || next.log_level != before.log_level;
            inner.supervisor.set_state(next);
            if restart {
                let now = inner.clock_ms;
                inner.supervisor.restart_for_config_change(now);
            }
        });
        self.publish_changes();
    }

    // ------------------------------------------------------------------ 导入 / 解析

    /// 粘贴一条或多条分享链接、或一段 sing-box 出站 JSON。**不发请求**。
    pub fn import_text(&self, text: String) -> FfiProxyCoreImportReport {
        let report = self.with_inner(|inner| {
            let state = inner.supervisor.state().clone();
            let (next, report) = subscriptions::import_text(&state, &text);
            inner.supervisor.set_state(next);
            report
        });
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart_for_config_change(now);
        });
        self.publish_changes();
        to_ffi_report(&report)
    }

    /// 只解析不合并（"导入前预览"）：返回解析出来的节点 + 跳过原因 + 认出来的格式。
    pub fn parse_source(&self, text: String) -> FfiProxyCoreParsed {
        let parsed = subscription::parse_node_source(&text, None);
        FfiProxyCoreParsed {
            nodes: parsed.nodes.iter().map(FfiProxyCoreNode::from_core).collect(),
            skipped: parsed
                .skipped
                .iter()
                .map(|s| FfiProxyCoreSkip {
                    index: s.index,
                    reason: s.reason.clone(),
                })
                .collect(),
            format: FfiProxyCoreSourceFormat::from_core(parsed.format),
        }
    }

    /// 认订阅格式（`{` 开头 / 有 `proxies:` / 其余当链接列表）。
    pub fn detect_source_format(&self, text: String) -> FfiProxyCoreSourceFormat {
        FfiProxyCoreSourceFormat::from_core(subscription::detect_node_source_format(&text))
    }

    // ------------------------------------------------------------------ 取订阅

    /// **喂 `System` 那一档的代理**（宿主在更新订阅之前调；`None` = 直连）。
    ///
    /// 为什么不在建对象时定死：`ProxyMode.System` 的语义是"拿**这个主机**去问 JVM 的选择器"
    /// （`PlatformProxySelector.select(uri)`），而订阅的地址是用户可以随时改的。宿主收到
    /// [`FfiProxyCore::plan_subscription_fetch`] 之后按 `url` 的主机名解析好，再在这里喂进来——
    /// 口径和 `ProxyTarget.of(ProxyMode.System, listOf(host))` 完全一样（问 `https://<host>/`）。
    pub fn set_system_proxy(&self, proxy: Option<crate::net::FfiNetProxy>) {
        let mut slot = self
            .system_proxy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = proxy.map(Proxy::from);
    }

    /// **决定"这次取订阅要发什么请求"**（宿主照它去发）。不发请求。
    pub fn plan_subscription_fetch(&self, subscription_id: String) -> FfiProxyCoreFetchPlan {
        self.with_inner(|inner| {
            let core_port = inner.supervisor.port();
            match subscriptions::plan_subscription_fetch(
                inner.supervisor.state(),
                &subscription_id,
                core_port,
            ) {
                subscriptions::FetchDecision::Fetch {
                    url,
                    user_agent,
                    route,
                    core_port,
                } => FfiProxyCoreFetchPlan::Fetch {
                    url,
                    user_agent,
                    route: FfiProxyCoreRoute::from_core(route),
                    core_port,
                },
                subscriptions::FetchDecision::Refused { reason } => {
                    FfiProxyCoreFetchPlan::Refused { reason }
                }
                subscriptions::FetchDecision::NotFound => FfiProxyCoreFetchPlan::NotFound,
            }
        })
    }

    /// **更新一个订阅的完整一趟**：先问 `plan`（Rust 决定），发（[`SubscriptionFetcher`]），
    /// 回来之后判状态码 / 解析 / 合并 / 记账（Rust）。
    ///
    /// 这是生产路径：宿主只看到"开始 / 结束"，中间的决策和收发全在 Rust。
    pub fn update_subscription(&self, id: String) -> Result<FfiProxyCoreUpdate, FfiProxyCoreError> {
        let (plan, now) = self.with_inner(|inner| {
            (
                subscriptions::plan_subscription_fetch(
                    inner.supervisor.state(),
                    &id,
                    inner.supervisor.port(),
                ),
                inner.clock_ms,
            )
        });
        let fetch = match plan {
            subscriptions::FetchDecision::NotFound => {
                return Err(FfiProxyCoreError::Invalid {
                    detail: "订阅不在列表里了".to_string(),
                })
            }
            subscriptions::FetchDecision::Refused { reason } => {
                let state = self.mutate_state(|state| {
                    subscriptions::mark_subscription_error(state, &id, &reason)
                });
                return Ok(FfiProxyCoreUpdate::Failed { state, reason });
            }
            subscriptions::FetchDecision::Fetch {
                url,
                user_agent,
                route,
                core_port,
            } => {
                let system_proxy = self
                    .system_proxy
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                    .unwrap_or(Proxy::Direct);
                let result = self.fetcher.fetch(
                    url,
                    user_agent,
                    FfiProxyCoreRoute::from_core(route),
                    core_port,
                    system_proxy,
                );
                match result {
                    Ok(fetched) => fetched,
                    Err(e) => {
                        // 分类串的口径和 Kotlin 那份 `classify` / Rust 的 `NetworkKind` 一样：
                        // `unknown_host` / `connect` / `timeout` / 其余按这一层自己的档位。
                        let kind = match e.network_kind() {
                            feedhub_net::NetworkKind::Other => e.kind().to_string(),
                            k => k.as_str().to_string(),
                        };
                        let reason = subscriptions::subscription_network_error(&kind);
                        let state = self.mutate_state(|state| {
                            subscriptions::mark_subscription_error(state, &id, &reason)
                        });
                        return Ok(FfiProxyCoreUpdate::Failed { state, reason });
                    }
                }
            }
        };
        let result = self.with_inner(|inner| {
            let state = inner.supervisor.state().clone();
            let fetched = core::FetchedSubscription {
                status: fetch.status,
                body: fetch.body.clone(),
                user_info_header: fetch.user_info_header.clone(),
            };
            subscriptions::finish_subscription_fetch(&state, &id, &fetched, now)
        });
        let out = match result {
            subscriptions::SubscriptionUpdate::Ok { state, parsed } => {
                let current = self.with_inner(|inner| {
                    inner.supervisor.set_state(state);
                    inner.supervisor.state().clone()
                });
                FfiProxyCoreUpdate::Ok {
                    state: FfiProxyCoreState::from_core(&current),
                    parsed: parsed as i32,
                }
            }
            subscriptions::SubscriptionUpdate::Failed { state, reason } => {
                let current = self.with_inner(|inner| {
                    inner.supervisor.set_state(state);
                    inner.supervisor.state().clone()
                });
                FfiProxyCoreUpdate::Failed {
                    state: FfiProxyCoreState::from_core(&current),
                    reason,
                }
            }
        };
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.restart_for_config_change(now);
        });
        self.publish_changes();
        Ok(out)
    }

    /// 有没有该自动更新的订阅（每条按自己的间隔）。返回订阅 id 表。
    pub fn due_subscriptions(&self) -> Vec<String> {
        self.with_inner(|inner| {
            let now = inner.clock_ms;
            subscriptions::due_subscriptions(inner.supervisor.state(), now)
        })
    }

    /// 全部到点的订阅各更新一次（宿主定时器每 10 分钟调一次）。返回更新成功的 id 表。
    pub fn update_due_subscriptions(&self) -> Vec<String> {
        let due = self.due_subscriptions();
        let mut done = Vec::new();
        for id in due {
            if let Ok(FfiProxyCoreUpdate::Ok { .. }) = self.update_subscription(id.clone()) {
                done.push(id);
            }
        }
        done
    }

    // ------------------------------------------------------------------ 延迟测试

    /// 测一个节点的延迟（另起一个一次性 sing-box，量墙钟时间）。结果写回节点并落盘。
    pub fn test_latency(&self, node_id: String) -> FfiProxyCoreLatency {
        let result = self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.test_latency(&node_id, now)
        });
        self.publish_changes();
        FfiProxyCoreLatency {
            node_id: result.node_id,
            ms: result.ms,
            error: result.error,
        }
    }

    /// 全部测一遍，**最多 `concurrency` 个同时测**（默认 4，和 Kotlin 的 `Semaphore(4)` 一样）。
    /// 返回测了几个。
    ///
    /// 宿主只等这一趟返回。一次测几十个节点会占住调用线程几秒——宿主在 `Dispatchers.IO` 上等。
    pub fn test_all_latencies(&self, concurrency: i32) -> i32 {
        let count = self.with_inner(|inner| {
            let now = inner.clock_ms;
            inner.supervisor.test_all_latencies(concurrency, now)
        });
        self.publish_changes();
        count
    }
}

impl FfiProxyCore {
    fn mutate_state(
        &self,
        f: impl FnOnce(&core::ProxyCoreState) -> core::ProxyCoreState,
    ) -> FfiProxyCoreState {
        self.with_inner(|inner| {
            let next = f(inner.supervisor.state());
            inner.supervisor.set_state(next);
            FfiProxyCoreState::from_core(inner.supervisor.state())
        })
    }

    fn publish_changes(&self) {
        let subscribed = self
            .logs_subscribed
            .load(std::sync::atomic::Ordering::Acquire);
        let (status_to_push, logs_to_push) = self.with_inner(|inner| {
            let status = inner.supervisor.status().clone();
            let push_status = if inner.last_status.as_ref() != Some(&status) {
                inner.last_status = Some(status.clone());
                Some(FfiProxyCoreStatus::from_core(&status))
            } else {
                None
            };
            let push_logs = if subscribed {
                let seq = inner.supervisor.logs_seq();
                if seq != inner.last_logs_seq {
                    inner.last_logs_seq = seq;
                    Some(inner.supervisor.logs())
                } else {
                    None
                }
            } else {
                None
            };
            (push_status, push_logs)
        });
        if let Some(status) = status_to_push {
            self.notify.status_changed(status);
        }
        if let Some(logs) = logs_to_push {
            self.notify.logs_changed(logs);
        }
    }
}

fn to_ffi_report(report: &core::ImportReport) -> FfiProxyCoreImportReport {
    FfiProxyCoreImportReport {
        added: report.added,
        updated: report.updated,
        skipped: report
            .skipped
            .iter()
            .map(|s| FfiProxyCoreSkip {
                index: s.index,
                reason: s.reason.clone(),
            })
            .collect(),
        summary: report.summary(),
    }
}

// ===========================================================================
// 纯函数（宿主建对象之前要用的那些）
// ===========================================================================

/// 找 sing-box 可执行文件。
///
/// 宿主按自己的优先级排好 `candidates`（显式参数、环境变量、App resources、用户数据目录……），
/// `extra` 放"必须优先"的（安卓的 `nativeLibraryDir/libsingbox.so`）。Rust 加一个开发机路径兜底，
/// 然后取第一个存在的普通文件。找不到就是 `None`（界面显示"没找到 sing-box"，别的功能不受影响）。
#[uniffi::export]
pub fn proxy_binary_locate(candidates: Vec<String>, extra: Vec<String>) -> Option<String> {
    let candidates: Vec<std::path::PathBuf> =
        candidates.into_iter().map(std::path::PathBuf::from).collect();
    let extra: Vec<std::path::PathBuf> = extra.into_iter().map(std::path::PathBuf::from).collect();
    core::binary::locate(&candidates, &extra).map(|p| p.to_string_lossy().into_owned())
}

/// 这份二进制**应该**是哪个哈希（`arch` = 宿主报的体系结构，`override_sha256` = 环境变量给的）。
/// `None` = 不校验。
#[uniffi::export]
pub fn proxy_binary_expected_sha256(arch: String, override_sha256: Option<String>) -> Option<String> {
    core::binary::expected_sha256(&arch, override_sha256.as_deref())
}

/// 文件的 sha256（小写十六进制）；读不了就是 `None`。
#[uniffi::export]
pub fn proxy_binary_sha256(path: String) -> Option<String> {
    core::binary::sha256(std::path::Path::new(&path))
}

/// 校验哈希。`None` = 通过，`Some(reason)` = 拒绝运行的原因（**不含机密**）。
#[uniffi::export]
pub fn proxy_binary_verify(path: String, expected: Option<String>) -> Option<String> {
    core::binary::verify(std::path::Path::new(&path), expected.as_deref())
}

/// 读版本号（`sing-box version` 的第一行）。
#[uniffi::export]
pub fn proxy_binary_version(path: String, timeout_ms: u64) -> Option<String> {
    core::binary::version(std::path::Path::new(&path), timeout_ms)
}

/// 造一份 sing-box 配置的文本（**有机密**）。没有节点 / 端口不对时是 `Err`。
#[uniffi::export]
pub fn proxy_build_core_config(
    state: FfiProxyCoreState,
    port: i32,
) -> Result<String, FfiProxyCoreError> {
    let state = state.to_core()?;
    let config = config::build_core_config(&state, port).map_err(|detail| FfiProxyCoreError::Invalid {
        detail,
    })?;
    Ok(config::to_config_text(&config))
}

/// 造一份"只测这一个节点"的配置的文本（**有机密**）。
#[uniffi::export]
pub fn proxy_build_latency_probe_config(node: FfiProxyCoreNode) -> Result<String, FfiProxyCoreError> {
    let node = node.to_core()?;
    Ok(config::to_config_text(&config::build_latency_probe_config(
        &node,
    )))
}

/// 起核心之前的自检。返回空表 = 没问题。
#[uniffi::export]
pub fn proxy_core_config_issues(state: FfiProxyCoreState) -> Result<Vec<String>, FfiProxyCoreError> {
    Ok(config::core_config_issues(&state.to_core()?))
}

/// 解析 `subscription-userinfo` 响应头（`None` = 没有 / 认不出来）。
#[uniffi::export]
pub fn proxy_parse_subscription_user_info(header: Option<String>) -> Option<FfiProxyCoreUserInfo> {
    subscription::parse_subscription_user_info(header.as_deref())
        .map(|info| FfiProxyCoreUserInfo::from_core(&info))
}

/// 网络异常分类 → 界面文案（`域名解析失败` / `连不上` / `超时` / `请求失败（…）`）。
#[uniffi::export]
pub fn proxy_network_reason(kind: String) -> String {
    supervisor::network_reason(&kind)
}

/// 默认值（宿主拼设置页的默认项用，和 Kotlin 的常量一一对应）。
#[uniffi::export]
pub fn proxy_core_defaults() -> FfiProxyCoreDefaults {
    FfiProxyCoreDefaults {
        test_url: core::DEFAULT_TEST_URL.to_string(),
        log_level: "warn".to_string(),
        subscription_ua: core::DEFAULT_SUBSCRIPTION_UA.to_string(),
        log_levels: core::CORE_LOG_LEVELS.iter().map(|s| s.to_string()).collect(),
        auto_update_hours: core::AUTO_UPDATE_HOURS.to_vec(),
        no_nodes_reason: core::NO_NODES_REASON.to_string(),
        max_log_lines: supervisor::MAX_LOG_LINES as i32,
        sing_box_version: core::binary::VERSION.to_string(),
        sha256_linux_amd64: core::binary::SHA256_LINUX_AMD64.to_string(),
        sha256_android_arm64: core::binary::SHA256_ANDROID_ARM64.to_string(),
        env_path: core::binary::ENV_PATH.to_string(),
        env_sha256: core::binary::ENV_SHA256.to_string(),
        default_stop_delay_ms: core::foreground::DEFAULT_STOP_DELAY_MS,
    }
}

/// 默认值 / 常量（见 [`proxy_core_defaults`]）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProxyCoreDefaults {
    pub test_url: String,
    pub log_level: String,
    pub subscription_ua: String,
    pub log_levels: Vec<String>,
    pub auto_update_hours: Vec<i32>,
    pub no_nodes_reason: String,
    pub max_log_lines: i32,
    pub sing_box_version: String,
    pub sha256_linux_amd64: String,
    pub sha256_android_arm64: String,
    pub env_path: String,
    pub env_sha256: String,
    pub default_stop_delay_ms: i64,
}

// ===========================================================================
// JSON 的小工具（只在这一层用）
// ===========================================================================

fn compact_json(value: &json_text::V) -> String {
    let mut out = String::new();
    write_compact(&mut out, value);
    out
}

fn write_compact(out: &mut String, value: &json_text::V) {
    match value {
        json_text::V::Null => out.push_str("null"),
        json_text::V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        json_text::V::Int(n) => out.push_str(&n.to_string()),
        json_text::V::UInt(n) => out.push_str(&n.to_string()),
        json_text::V::Double(f) => out.push_str(&json_text::double_text(*f)),
        json_text::V::Num(raw) => out.push_str(&json_text::V::number_text(raw)),
        json_text::V::Str(s) => json_text::V::write_string(out, s),
        json_text::V::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, item);
            }
            out.push(']');
        }
        json_text::V::Obj(fields) => {
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_text::V::write_string(out, key);
                out.push(':');
                write_compact(out, item);
            }
            out.push('}');
        }
    }
}

fn parse_json(text: &str) -> Result<json_text::V, FfiProxyCoreError> {
    let root = feedhub_core::xhs::jsonord::parse(text).map_err(|detail| FfiProxyCoreError::Invalid {
        detail: format!("出站不是合法 JSON：{detail}"),
    })?;
    Ok(to_v(&root))
}

fn to_v(j: &feedhub_core::xhs::jsonord::J) -> json_text::V {
    use feedhub_core::xhs::jsonord::J;
    match j {
        J::Null => json_text::V::Null,
        J::Bool(b) => json_text::V::Bool(*b),
        J::Num(raw) => json_text::V::number(raw),
        J::Str(s) => json_text::V::Str(s.clone()),
        J::Arr(items) => json_text::V::Arr(items.iter().map(to_v).collect()),
        J::Obj(fields) => json_text::V::Obj(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), to_v(v)))
                .collect(),
        ),
    }
}
