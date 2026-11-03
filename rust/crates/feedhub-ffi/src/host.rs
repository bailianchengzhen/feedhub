//! **宿主回调接口（UniFFI 的 `with_foreign`）**：Rust 反过来调 Kotlin。
//!
//! 三件宿主能力（HTTP / 键值存储 / Cookie 罐）为什么留在宿主、为什么是同步的，
//! 见 `feedhub_core::host` 的模块说明。这里只做 UniFFI 那一层：
//!
//! - [`FfiHttpRequest`] / [`FfiHttpResponse`] / [`FfiCookie`]：过边界的 record（字段全是简单类型，**有序**的头用 `Vec`）；
//! - [`HostHttp`] / [`HostStore`] / [`HostCookies`]：Kotlin 侧实现的 trait；
//! - 适配器：把 `dyn HostHttp`（FFI 侧）包成 `feedhub_core::host::HttpHost`（平台对象只认识 core 的类型）。
//!
//! **同步**：这三个 trait 的方法都是阻塞的。Kotlin 在 `Dispatchers.IO` 上调 Rust，Rust 同步回调宿主执行
//! OkHttp，宿主阻塞等响应。代价是 `.so` 里不能有 `await`（所以不需要 tokio），
//! 换来的是"线程模型只有调用方那一个线程"——没有执行器、没有后台线程、闲置时零唤醒。

use std::sync::Arc;

use feedhub_core::host as core_host;

/// 一个有序的请求头 / 响应头。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiHeader {
    pub name: String,
    pub value: String,
}

/// 要发的请求：方法 + 完整 URL + **有序**头 + body 字节。宿主只负责发，不许改。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<FfiHeader>,
    pub body: Vec<u8>,
}

/// 宿主的响应：状态码 + 有序响应头 + body；`finalUrl` 是跟随重定向之后的地址（排障用）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiHttpResponse {
    pub status: u16,
    pub headers: Vec<FfiHeader>,
    pub body: Vec<u8>,
    pub final_url: String,
}

/// 宿主执行请求时的错误。**必须分类**：风控红线和 cli 的退出码都靠这个区分（`network` vs `proxyUnavailable`）。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiHostError {
    /// 连不上 / 超时 / 连接被重置（Kotlin 侧的 `java.io.IOException`）。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// fail-closed：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题）。别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Other { detail: String },
}

/// 要种进宿主 Cookie 罐的一条 Cookie。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCookie {
    pub name: String,
    pub value: String,
    /// `bilibili.com`（不带前导点 = host-only，和 Kotlin `Cookie.Builder` 的默认一致）。
    pub domain: String,
    pub path: String,
    /// 毫秒时间戳。
    pub expires_at_ms: i64,
}

/// **发一个请求、拿一个响应**（同步）。Kotlin 侧用 `ctx.client(platform)` 派生的那个 OkHttpClient 执行，
/// **不能新建客户端**（否则拦截器 / 代理 / Cookie / 限速全丢）。
#[uniffi::export(with_foreign)]
pub trait HostHttp: Send + Sync {
    fn execute(&self, request: FfiHttpRequest) -> Result<FfiHttpResponse, FfiHostError>;
}

/// 键值存储（`ctx.store`）。键和值都原样传，Rust 不解释宿主的格式。
#[uniffi::export(with_foreign)]
pub trait HostStore: Send + Sync {
    fn get(&self, key: String) -> Option<String>;
    fn put(&self, key: String, value: Option<String>);
}

/// Cookie 罐（`ctx.cookies`）。登录态的唯一真相在宿主，Rust 只按 URL + 名字读、按 URL 种。
#[uniffi::export(with_foreign)]
pub trait HostCookies: Send + Sync {
    fn find(&self, url: String, name: String) -> Option<String>;
    fn put(&self, url: String, cookies: Vec<FfiCookie>);
}

// ===========================================================================
// FFI → core 的适配器（平台对象只认识 `feedhub_core::host` 里的 trait）
// ===========================================================================

pub(crate) struct HttpAdapter(pub Arc<dyn HostHttp>);

impl core_host::HttpHost for HttpAdapter {
    fn execute(
        &self,
        request: core_host::HttpRequest,
    ) -> Result<core_host::HttpResponse, core_host::HostError> {
        let ffi = FfiHttpRequest {
            method: request.method,
            url: request.url,
            headers: request
                .headers
                .into_iter()
                .map(|(name, value)| FfiHeader { name, value })
                .collect(),
            body: request.body,
        };
        match self.0.execute(ffi) {
            Ok(r) => Ok(core_host::HttpResponse {
                status: r.status,
                headers: r
                    .headers
                    .into_iter()
                    .map(|h| (h.name, h.value))
                    .collect(),
                body: r.body,
                final_url: r.final_url,
            }),
            Err(FfiHostError::Network { detail }) => Err(core_host::HostError::Network { detail }),
            Err(FfiHostError::ProxyUnavailable { detail }) => {
                Err(core_host::HostError::ProxyUnavailable { detail })
            }
            Err(FfiHostError::Other { detail }) => Err(core_host::HostError::Other { detail }),
        }
    }
}

pub(crate) struct StoreAdapter(pub Arc<dyn HostStore>);

impl core_host::StoreHost for StoreAdapter {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key.to_string())
    }

    fn put(&self, key: &str, value: Option<&str>) {
        self.0.put(key.to_string(), value.map(|v| v.to_string()));
    }
}

pub(crate) struct CookiesAdapter(pub Arc<dyn HostCookies>);

impl core_host::CookieHost for CookiesAdapter {
    fn find(&self, url: &str, name: &str) -> Option<String> {
        self.0.find(url.to_string(), name.to_string())
    }

    fn put(&self, url: &str, cookies: Vec<core_host::CookieSpec>) {
        self.0.put(
            url.to_string(),
            cookies
                .into_iter()
                .map(|c| FfiCookie {
                    name: c.name,
                    value: c.value,
                    domain: c.domain,
                    path: c.path,
                    expires_at_ms: c.expires_at_ms,
                })
                .collect(),
        );
    }
}

// ===========================================================================
// 扫码登录的宿主能力
// ===========================================================================

/// 一条**带完整属性**的 Cookie（`okhttp3.Cookie` 的字段一对一，见 `feedhub_core::host::JarCookie`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiQrCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires_at_ms: i64,
    pub secure: bool,
    pub http_only: bool,
    /// `true` = host-only（`Cookie.Builder.hostOnlyDomain`），`false` = 域 Cookie（`Cookie.Builder.domain`）。
    pub host_only: bool,
}

/// **扫码登录的宿主能力**：临时 Cookie 罐 + 落地 + 二维码图解码。
///
/// 为什么不是第三条端口（`HostHttp` / `HostStore` / `HostCookies`）里的一个方法：临时罐必须挂在
/// "发请求的那个客户端"上（`newBuilder().cookieJar(临时罐)`），而且要和 `ctx.cookies` **完全隔离**——
/// 扫码成功之前一格都不动（`core/QrLoginSupport.kt` 的 `QrSessions`）。语义和理由见 `feedhub_core::host::QrHost`。
#[uniffi::export(with_foreign)]
pub trait HostQrLogin: Send + Sync {
    /// `ctx.cookies.all()`（知乎 / 抖音的扫码要从本机设备 Cookie 起步）。
    fn all(&self) -> Vec<FfiQrCookie>;

    /// 开一个空的临时罐，返回句柄（宿主内部编号；平台 token 由 Rust 记）。
    fn begin(&self) -> String;

    /// 用临时罐发请求。[`timeout_ms`] 只给贴吧的长轮询用（`None` = 客户端自己的超时）。
    fn execute(
        &self,
        session: String,
        request: FfiHttpRequest,
        timeout_ms: Option<i64>,
    ) -> Result<FfiHttpResponse, FfiHostError>;

    /// 临时罐里的全部 Cookie（`jar.all()` 的顺序）。
    fn cookies(&self, session: String) -> Vec<FfiQrCookie>;

    /// `jar.find(url, name)`。
    fn find(&self, session: String, url: String, name: String) -> Option<String>;

    /// `jar.put(url, cookies)`。
    fn put(&self, session: String, url: String, cookies: Vec<FfiQrCookie>);

    /// 丢掉临时罐。
    fn end(&self, session: String);

    /// 落地：先删旧登录 Cookie（域 + 名单），再按顺序写新的（`commitQrLogin` 的语义）。
    fn commit(
        &self,
        url: String,
        remove_domain: String,
        remove_names: Vec<String>,
        cookies: Vec<FfiQrCookie>,
    );

    /// 二维码图片 → 里面的文字（zxing 只有 Java 版，所以留在宿主）。
    fn decode_qr_image(&self, png: Vec<u8>) -> Option<String>;
}

/// **二维码图解码**（zxing 只有 Java 版，所以留在宿主）。
///
/// **只剩解码要回 Kotlin**。
/// 所以它变形一档：从"扫码宿主的一个方法"改成"宿主额外提供的一件事"——`NetQrHost` 拿到的就是它。
#[uniffi::export(with_foreign)]
pub trait HostQrDecoder: Send + Sync {
    /// 二维码图片 → 里面的文字（不支持 / 解不出来给 `None`，调用方按平台原来的报错处理）。
    fn decode_qr_image(&self, png: Vec<u8>) -> Option<String>;
}

/// `HostQrLogin`（FFI）→ `feedhub_core::host::QrHost`（平台对象只认识 core 的类型）。
pub(crate) struct QrAdapter(pub Arc<dyn HostQrLogin>);

fn to_ffi_request(request: core_host::HttpRequest) -> FfiHttpRequest {
    FfiHttpRequest {
        method: request.method,
        url: request.url,
        headers: request
            .headers
            .into_iter()
            .map(|(name, value)| FfiHeader { name, value })
            .collect(),
        body: request.body,
    }
}

fn from_ffi_response(r: FfiHttpResponse) -> core_host::HttpResponse {
    core_host::HttpResponse {
        status: r.status,
        headers: r
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect(),
        body: r.body,
        final_url: r.final_url,
    }
}

fn from_ffi_host_error(e: FfiHostError) -> core_host::HostError {
    match e {
        FfiHostError::Network { detail } => core_host::HostError::Network { detail },
        FfiHostError::ProxyUnavailable { detail } => core_host::HostError::ProxyUnavailable { detail },
        FfiHostError::Other { detail } => core_host::HostError::Other { detail },
    }
}

pub(crate) fn to_ffi_qr_cookie(c: &core_host::JarCookie) -> FfiQrCookie {
    FfiQrCookie {
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.clone(),
        path: c.path.clone(),
        expires_at_ms: c.expires_at_ms,
        secure: c.secure,
        http_only: c.http_only,
        host_only: c.host_only,
    }
}

impl core_host::QrHost for QrAdapter {
    fn all(&self) -> Vec<core_host::JarCookie> {
        self.0
            .all()
            .into_iter()
            .map(|c| core_host::JarCookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                expires_at_ms: c.expires_at_ms,
                secure: c.secure,
                http_only: c.http_only,
                host_only: c.host_only,
            })
            .collect()
    }

    fn begin(&self) -> String {
        self.0.begin()
    }

    fn execute(
        &self,
        session: &str,
        request: core_host::HttpRequest,
        timeout_ms: Option<i64>,
    ) -> Result<core_host::HttpResponse, core_host::HostError> {
        match self.0.execute(session.to_string(), to_ffi_request(request), timeout_ms) {
            Ok(r) => Ok(from_ffi_response(r)),
            Err(e) => Err(from_ffi_host_error(e)),
        }
    }

    fn cookies(&self, session: &str) -> Vec<core_host::JarCookie> {
        self.0
            .cookies(session.to_string())
            .into_iter()
            .map(|c| core_host::JarCookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                expires_at_ms: c.expires_at_ms,
                secure: c.secure,
                http_only: c.http_only,
                host_only: c.host_only,
            })
            .collect()
    }

    fn find(&self, session: &str, url: &str, name: &str) -> Option<String> {
        self.0.find(session.to_string(), url.to_string(), name.to_string())
    }

    fn put(&self, session: &str, url: &str, cookies: Vec<core_host::JarCookie>) {
        self.0.put(
            session.to_string(),
            url.to_string(),
            cookies.iter().map(to_ffi_qr_cookie).collect(),
        );
    }

    fn end(&self, session: &str) {
        self.0.end(session.to_string());
    }

    fn commit(
        &self,
        url: &str,
        remove_domain: &str,
        remove_names: Vec<String>,
        cookies: Vec<core_host::JarCookie>,
    ) {
        self.0.commit(
            url.to_string(),
            remove_domain.to_string(),
            remove_names,
            cookies.iter().map(to_ffi_qr_cookie).collect(),
        );
    }

    fn decode_qr_image(&self, png: Vec<u8>) -> Option<String> {
        self.0.decode_qr_image(png)
    }
}

// ===========================================================================
// 签名随机量 / 时钟（关注流：一次调度要发 N 个请求）
// ===========================================================================

/// 一次签名里**补位长度之前**的那几笔随机量（`feedhub_core::host::SignRandomHead` 的边界版）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiXhsSignRandomHead {
    pub version: i32,
    pub xy_random: i32,
    pub xray_tail: String,
    pub b3_trace_id: String,
    pub rap_mask: i32,
    pub rap_session_key: String,
    pub rap_key: String,
    pub rap_salt: String,
}

/// **补位长度之后**的那两笔。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiXhsSignRandomTail {
    pub rap_pad: String,
    pub rap_enc_time_ms: i32,
}

/// **签名随机量的来源**（小红书关注流专用）。
///
/// Kotlin 侧的实现就是 `XhsSignRandomDrawer`：**从同一个 `Random` 里按 `XhsSigner.sign` 的取用顺序抽**，
/// 所以 Rust 这边两个方法先后被调时，随机流和 Kotlin 参照实现逐位一致。
/// 为什么要分两步：补位长度取决于 gzip 之后的长度（要先知道掩码 + 会话密钥才能算）。
#[uniffi::export(with_foreign)]
pub trait HostXhsSignRandom: Send + Sync {
    /// 抽"补位长度之前"的那几笔。[xy] 为真且 [xy_seed] 是空串时才会用上 `xy_random`
    /// （有种子时 `xy-direction` 是 murmur3 算的，不占随机流）。
    fn draw_head(&self, xy: bool, xy_seed: Option<String>, rap: bool) -> FfiXhsSignRandomHead;

    /// 抽"补位长度之后"的那两笔：`pad_len` 个补位字符 + 加密耗时。
    fn draw_tail(&self, pad_len: u32) -> FfiXhsSignRandomTail;
}

/// **a_bogus 的随机数来源**。
///
/// 谁在用：抖音的平台对象。表情表搬进 Rust 之后，"这一趟要不要多签一个 `emoji/list`"只有 Rust
/// 解析完响应才知道，所以随机数改成**签的那一刻问宿主抽**（Kotlin 侧就是那个 `AbogusRandom` /
/// `Random` 实例），抽取的个数与顺序天然和 Kotlin 的 `AbogusSigner` 逐位一致。
#[uniffi::export(with_foreign)]
pub trait HostRandom: Send + Sync {
    /// 抽 `count` 个 `[0, 1)` 的随机数（`Math.random()` 的口径）。
    fn draw(&self, count: u32) -> Vec<f64>;
}

pub(crate) struct RandomAdapter(pub Arc<dyn HostRandom>);

impl core_host::RandomHost for RandomAdapter {
    fn draw(&self, count: usize) -> Vec<f64> {
        self.0.draw(count as u32)
    }
}

/// **小黑盒签名的 nonce 来源**：一次调用里第二个起的请求各问宿主要一个新的
/// （Kotlin 就是 `RustHeyboxSource.nonceFor`：`SecureRandom` + MD5，32 位大写十六进制）。
#[uniffi::export(with_foreign)]
pub trait HostNonce: Send + Sync {
    fn nonce(&self, timestamp: i64) -> String;
}

pub(crate) struct NonceAdapter(pub Arc<dyn HostNonce>);

impl feedhub_core::heybox::sign::NonceSource for NonceAdapter {
    fn fresh(&self, timestamp: i64) -> String {
        self.0.nonce(timestamp)
    }
}

/// **时钟**：`System.currentTimeMillis()`。一次刷新里 Kotlin 会调很多次 `now()`，
/// 对拍要注入同一个假钟才能逐格比 `fetchedAt` / `extraAt` / `closedUntil`。
#[uniffi::export(with_foreign)]
pub trait HostXhsClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub(crate) struct SignRandomAdapter(pub Arc<dyn HostXhsSignRandom>);

impl core_host::SignRandomHost for SignRandomAdapter {
    fn draw_head(
        &self,
        xy: bool,
        xy_seed: Option<&str>,
        rap: bool,
    ) -> core_host::SignRandomHead {
        let head = self
            .0
            .draw_head(xy, xy_seed.map(|s| s.to_string()), rap);
        core_host::SignRandomHead {
            version: head.version,
            xy_random: head.xy_random,
            xray_tail: head.xray_tail,
            b3_trace_id: head.b3_trace_id,
            rap_mask: head.rap_mask,
            rap_session_key: head.rap_session_key,
            rap_key: head.rap_key,
            rap_salt: head.rap_salt,
        }
    }

    fn draw_tail(&self, pad_len: usize) -> core_host::SignRandomTail {
        let tail = self.0.draw_tail(pad_len as u32);
        core_host::SignRandomTail {
            rap_pad: tail.rap_pad,
            rap_enc_time_ms: tail.rap_enc_time_ms,
        }
    }
}

pub(crate) struct ClockAdapter(pub Arc<dyn HostXhsClock>);

impl core_host::ClockHost for ClockAdapter {
    fn now_ms(&self) -> i64 {
        self.0.now_ms()
    }
}

/// **"我自己是谁"**（小红书关注流的 `user_id`）：Rust 在**真要拉关注列表的那一刻**才问。
///
/// `whoami` 没搬进 Rust，而 `xhs.user_id` 没存过时 Kotlin 要现发一次 `user/me`（一个请求，
/// 占一个限速槽）。做成回调而不是构造参数，是为了让"翻页 / 缓存还新鲜"的那一轮**一个请求都不发**
/// （和 Kotlin 参照实现的位置一模一样）。
#[uniffi::export(with_foreign)]
pub trait HostXhsSelfId: Send + Sync {
    /// 没有登录 / 问不到就是 `None`（这时 Rust 报未登录，和 Kotlin 一样）。
    fn self_id(&self) -> Option<String>;
}

pub(crate) struct SelfIdAdapter(pub Arc<dyn HostXhsSelfId>);

impl core_host::SelfIdHost for SelfIdAdapter {
    fn self_id(&self) -> Option<String> {
        self.0.self_id()
    }
}

// ===========================================================================
// JS 求解
// ===========================================================================

/// **在宿主里跑外来 JS 的签名引擎**（现在只有 YouTube 的 n/sig：yt-dlp 的 EJS 跑在 quickjs-kt 里）。
///
/// 为什么留在宿主、为什么同步、为什么整批一次，见 `feedhub_core::host::ScriptHost` 的说明。
/// Kotlin 侧的实现在 `rust-bridge` 的 `RustScriptHost`（QuickJS 在 `source-youtube` 的 `YoutubeEjs`）。
///
/// 失败语义：解不出来就回**空** `Vec`（= 一个答案都没有），Rust 那边照 Kotlin 的 `catch (e: Exception)`
/// 处理（这个客户端算失败、换下一个）。**不要**在这里抛异常（跨边界抛出去的异常会把整次播放打成内部错误）。
#[uniffi::export(with_foreign)]
pub trait HostScript: Send + Sync {
    fn solve(
        &self,
        player_js: String,
        player_key: String,
        requests: Vec<FfiScriptRequest>,
    ) -> Vec<FfiScriptAnswer>;
}

/// 一次求解的请求：`kind` 是 `"sig"` 或 `"n"`，`challenges` 是挑战原文（`s` 已 URL 解码）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiScriptRequest {
    pub kind: String,
    pub challenges: Vec<String>,
}

/// 一条答案：`challenge → answer`（宿主原样回，Rust 按挑战原文查）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiScriptAnswer {
    pub challenge: String,
    pub answer: String,
}

pub(crate) struct ScriptAdapter(pub Arc<dyn HostScript>);

impl core_host::ScriptHost for ScriptAdapter {
    fn solve(
        &self,
        player_js: &str,
        player_key: &str,
        requests: Vec<(String, Vec<String>)>,
    ) -> Vec<(String, String)> {
        let ffi = requests
            .into_iter()
            .map(|(kind, challenges)| FfiScriptRequest { kind, challenges })
            .collect();
        self.0
            .solve(player_js.to_string(), player_key.to_string(), ffi)
            .into_iter()
            .map(|a| (a.challenge, a.answer))
            .collect()
    }
}
