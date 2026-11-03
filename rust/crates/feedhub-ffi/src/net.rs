//!
//! 这一份**还没有接进产品路径**：`HostHttp` / OkHttp 那条链路一行不动。
//! 它现在的职责只有两个：
//!
//! 1. 让 `feedhub-net` 真的被编进 `libfeedhub_ffi.so`（量体积、验证交叉编译能过）；
//!
//! # 和 `host.rs` 的关系
//!
//! `host.rs` 定义的是"Rust 调宿主发请求"（`HttpHost` trait）。这一份是**反过来**：
//! Rust 自己发。
//! 不经过 FFI），然后删掉 Kotlin 的 `RustHttpHost`。
//! 所以这一份的形状**故意和 `FfiHttpRequest` 一样**（方法 / URL / 有序头 / body），
//! 多出来的是 `platform`（查代理 / 限速）和 `priority`。
//!
//! # 限制
//!
//! 所以现在这个客户端**不带 Cookie、不限速**——只用来量体积 /
//!   做联调，**不要**拿它去发真实平台请求。
//! - 取消令牌暂时只在 Rust 侧有用。
//! - 错误分类走 `FfiNetError`，和 `FfiError` **分开**：传输错误（网络 / 超时 / 代理不可用 / 取消）和
//!   业务错误（`FfiError`）是两类东西，上层映射成不同的异常。

use std::sync::Arc;

use feedhub_net::{CancelToken, NetClient, NetRequest, Proxy, ProxyPlan};

/// 传输层的错误分类（和 `feedhub_net::NetError` 一一对应）。
///
/// **和 `FfiError` 分开**：这一档是"请求没发出去 / 没拿到响应"，那一档是"平台说不行"。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FfiNetError {
    /// 连不上 / 被重置 / 证书 / DNS / HTTP 协议错。
    #[error("网络错误：{detail}")]
    Network { detail: String },
    /// 超时（连接 / 读 / 写）。
    #[error("超时：{detail}")]
    Timeout { detail: String },
    /// 被取消（`Call.cancel()`）。
    #[error("已取消")]
    Cancelled,
    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，一个字节都没发。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },
    /// 请求本身不合法（URL / 头名 / body）。
    #[error("请求不合法：{detail}")]
    BadRequest { detail: String },
    /// 这一层自己的意外。
    #[error("内部错误：{detail}")]
    Other { detail: String },
}

impl From<feedhub_net::NetError> for FfiNetError {
    fn from(e: feedhub_net::NetError) -> Self {
        use feedhub_net::NetError as N;
        match e {
            N::Network { detail, .. } => Self::Network { detail },
            N::Timeout { detail } => Self::Timeout { detail },
            N::Cancelled => Self::Cancelled,
            N::ProxyUnavailable { detail } => Self::ProxyUnavailable { detail },
            N::BadRequest { detail } => Self::BadRequest { detail },
            N::Other { detail } => Self::Other { detail },
        }
    }
}

/// 一个要发出去的请求（**字段和 `host::HttpRequest` 一样**，多了平台 / 优先级 / 读超时）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiNetRequest {
    /// 平台 id（`bilibili` / `zhihu` …）：查代理、限速分桶。
    pub platform: String,
    /// 方法名（`GET` / `POST`）。
    pub method: String,
    /// **完整 URL**（带 query、已经签好名）。
    pub url: String,
    /// **有序**请求头：按这个顺序写进去，且**不再补同名头**。重名允许。
    pub headers: Vec<String>,
    /// body 字节；空 = 没有 body。
    pub body: Vec<u8>,
    /// `true` = 用户正等着的（详情 / 评论）；`false` = 后台刷流。
    pub interactive: bool,
    /// 这一跳的读超时（毫秒；`0` = 用默认值 30 s）。贴吧 `unicast` 长轮询用。
    pub read_timeout_ms: u32,
    /// 这一跳**显式指定**的代理档（`None` = 按 `platform` 查建客户端时给的那份 `ProxyPlan`）。
    ///
    /// 为什么需要：图片加载器是"一个客户端服务全部域名"的，而代理是**按域名**选的
    /// （`AppGraph` 的 `imageProxySelector`：图属于哪个平台就用那个平台的代理）。用一份客户端 +
    /// 逐请求给档位，既保住了连接池（Rust 按代理档分池），又不用每个域名建一份栈。
    /// `Unavailable` 和以前那个死代理一样是 **fail-closed**：一个字节都不发。
    pub proxy: Option<FfiNetProxy>,
}

/// 一个响应（**字段和 `host::HttpResponse` 一样**，多了协议）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiNetResponse {
    pub status: u16,
    /// **有序**响应头（同名多次按原顺序保留；`Set-Cookie` 不合并）。
    pub headers: Vec<String>,
    pub body: Vec<u8>,
    /// 跟随后得到的最终 URL（重定向之后）。
    pub final_url: String,
    /// `"h2"` / `"http/1.1"`（协商到的协议）。
    pub protocol: String,
}

/// 一份代理快照里的一档（对应 `feedhub_net::Proxy`）。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiNetProxy {
    Direct,
    Http { host: String, port: u16 },
    Socks5 { host: String, port: u16 },
    Socks5h { host: String, port: u16 },
    /// **fail-closed**：这个平台这次要用代理但没有可用的。
    Unavailable { reason: String },
}

impl From<FfiNetProxy> for Proxy {
    fn from(p: FfiNetProxy) -> Self {
        match p {
            FfiNetProxy::Direct => Proxy::Direct,
            FfiNetProxy::Http { host, port } => Proxy::Http { host, port },
            FfiNetProxy::Socks5 { host, port } => Proxy::Socks5 { host, port },
            FfiNetProxy::Socks5h { host, port } => Proxy::Socks5h { host, port },
            FfiNetProxy::Unavailable { reason } => Proxy::Unavailable { reason },
        }
    }
}

/// 一条"平台 → 代理档"的映射（UniFFI 不支持元组，所以包一层 record）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiNetPlatformProxy {
    pub platform: String,
    pub proxy: FfiNetProxy,
}

/// **一份客户端**（= 一个代理快照 + 一个连接池）。
///
/// 旧的那份让它的连接自然回收。
#[derive(uniffi::Object)]
pub struct FfiNetClient {
    inner: NetClient,
    /// 取消令牌。
    cancel: Arc<CancelToken>,
}

#[uniffi::export]
impl FfiNetClient {
    /// 建一份客户端。
    ///
    /// - `proxies`：`(平台 id, 那一档)` 的列表；没列出的平台用 `default_proxy`。
    /// - `default_proxy`：`None` = 直连。
    #[uniffi::constructor]
    pub fn new(
        proxies: Vec<FfiNetPlatformProxy>,
        default_proxy: Option<FfiNetProxy>,
    ) -> Result<Arc<Self>, FfiNetError> {
        let mut plan = ProxyPlan::direct();
        if let Some(d) = default_proxy {
            plan = plan.with_default(d.into());
        }
        for entry in proxies {
            plan = plan.set(&entry.platform, entry.proxy.into());
        }
        let inner = NetClient::builder()
            .proxies(plan)
            .build()
            .map_err(FfiNetError::from)?;
        Ok(Arc::new(Self { inner, cancel: Arc::new(CancelToken::new()) }))
    }

    /// 取消这一份客户端上**所有**在飞的请求（对应 `Call.cancel()` 的批量语义）。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// 发一个请求、等响应，但用**这一趟自己的**取消令牌（`Call.cancel()` 的逐请求语义）。
    ///
    /// 图片加载器要靠它：Coil 取消一次请求时只该掐掉那一张，不能波及同时在飞的别的图。
    pub fn execute_cancellable(
        &self,
        request: FfiNetRequest,
        cancel: Arc<FfiNetCancel>,
    ) -> Result<FfiNetResponse, FfiNetError> {
        let mut req = self.to_request(request, cancel.inner.clone());
        if req.method.is_empty() {
            req.method = "GET".to_string();
        }
        let resp = self.inner.execute(req).map_err(FfiNetError::from)?;
        Ok(to_response(resp))
    }

    /// 发一个请求、等响应（**同步阻塞**；Kotlin 在 `Dispatchers.IO` 上调）。
    pub fn execute(&self, request: FfiNetRequest) -> Result<FfiNetResponse, FfiNetError> {
        let mut req = self.to_request(request, self.cancel.clone());
        if req.method.is_empty() {
            req.method = "GET".to_string();
        }
        let resp = self.inner.execute(req).map_err(FfiNetError::from)?;
        Ok(to_response(resp))
    }
}

impl FfiNetClient {
    /// 把边界上的 record 变成本层的请求（两个入口共用）。
    fn to_request(&self, request: FfiNetRequest, cancel: Arc<CancelToken>) -> NetRequest {
        let mut headers: Vec<(String, String)> = Vec::new();
        // 头是 `"Name: Value"` 的字符串（UniFFI 的 record 里用 tuple 要额外写 `FfiNetHeader`，
        // 这里照 `FfiHttpRequest` 的形状保持简单）。
        for line in &request.headers {
            match line.split_once(':') {
                Some((k, v)) => headers.push((k.trim().to_string(), v.trim().to_string())),
                None => {}
            }
        }
        let req = NetRequest {
            platform: request.platform,
            method: request.method,
            url: request.url,
            headers,
            body: request.body,
            priority: if request.interactive {
                feedhub_net::Priority::Interactive
            } else {
                feedhub_net::Priority::Background
            },
            cancel,
            read_timeout: if request.read_timeout_ms == 0 {
                None
            } else {
                Some(std::time::Duration::from_millis(request.read_timeout_ms as u64))
            },
            proxy: request.proxy.map(Proxy::from),
            observer: None,
            extra_default_headers: Vec::new(),
            default_headers_hosts: Vec::new(),
            follow_redirects: None,
            gate: None,
            after_headers: None,
            cookies: None,
            limiter: None,
            after_limit: None,
        };
        req
    }
}

/// 把响应搬过边界（两个入口共用）。
fn to_response(resp: feedhub_net::NetResponse) -> FfiNetResponse {
    FfiNetResponse {
        status: resp.status,
        headers: resp
            .headers
            .into_iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect(),
        body: resp.body,
        final_url: resp.final_url,
        protocol: match resp.protocol {
            Some(feedhub_net::Protocol::Http2) => "h2".to_string(),
            Some(feedhub_net::Protocol::Http1_1) => "http/1.1".to_string(),
            None => String::new(),
        },
    }
}

/// 一次请求的取消令牌（`Call.cancel()` 的逐请求版本）。
///
/// 为什么单独一个对象：`FfiNetClient.cancel()` 掐的是整个客户端上所有在飞的请求（那是给"整个平台对象
/// 停掉"用的）；图片加载器里每一张图是一趟独立的请求，取消只能掐它自己。
#[derive(uniffi::Object)]
pub struct FfiNetCancel {
    inner: Arc<CancelToken>,
}

#[uniffi::export]
impl FfiNetCancel {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(CancelToken::new()),
        })
    }

    /// 掐掉挂着这个令牌的那一趟（幂等；发完之后再掐没有任何效果，和 `Call.cancel()` 一样）。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// 已经掐过了吗（宿主用它决定要不要把这次失败报成"取消"）。
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}
