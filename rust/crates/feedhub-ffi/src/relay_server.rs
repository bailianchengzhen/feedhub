//! **本机回环上的视频分片中转**。
//!
//! 搬之前这套东西在 `app/desktopMain/.../video/`（`VideoRelay` + `MpdServer` + `ManifestRewrite`），
//! 三份加起来 424 行 Kotlin：一个 `com.sun.net.httpserver.HttpServer`、一个 8 线程池、OkHttp 收发、
//! 清单改写的两条薄壳。现在**整个服务和取回都在这一层**，
//! Kotlin 只剩"起 / 停"。
//!
//! ## 它解决什么
//!
//! GStreamer 的 dashdemux2 / hlsdemux2 用它们**自己的** libsoup session 下载分片，不会产生 `souphttpsrc`、
//! 也不会回调 `source-setup`，所以"给清单设代理"对分片没用——配了代理也是分片直连 CDN。
//! 这里把清单里所有远端地址改写成**本机回环**地址（`http://127.0.0.1:<port>/…`），由播放器去这里取；
//! 中转再用**这个平台的代理 / 请求头**去取真正的分片（走 `feedhub-net`）。于是平台代理、fail-closed、
//! Referer / UA 全都跟着同一条判定走。
//!
//! ## 分工
//!
//! | 这类事 | 在哪儿 |
//! | 登记表、本机地址形状、`Range` 解析、`Route` 判定 | `feedhub_core::player::relay` |
//! | 清单改写（DASH / HLS） | `feedhub_core::player::manifest` |
//! | **HTTP 服务器 + 线程 + 取回 + 边收边转发** | **这里** |
//!
//! ## 线程模型
//!
//! 一个监听线程（`std::net::TcpListener`）+ **每个请求一个处理线程**（上限 [`RelayConfig::threads`]，
//! 和原来 `Executors.newFixedThreadPool(8)` 同一个数量级）。处理线程里同步地调
//! `NetClient::execute_stream`——和搬之前"一个请求占一个 OkHttp 工作线程"逐条对齐。
//! 没有 tokio 线程池、没有异步任务表。
//!
//! ## 红线（和搬之前一样）
//!
//! - 只监听 `127.0.0.1`（端口由内核选）；
//! - 上游只用这个平台的 `ProxyPlan` / `HostSpec`：**代理不可用时一个字节都不发**，
//!   播放器收到 502（而不是悄悄直连 CDN）；
//! - 播放器断开（写 socket 失败）→ 立刻掐掉上游（`NetStream` 的 `cancel` / `drop`）；
//! - `Range` / `206` / `Content-Range` 原样透传（DASH 的 SegmentBase / indexRange 靠它）。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use feedhub_core::player::manifest::{rewrite_dash, rewrite_hls};
use feedhub_core::player::relay::{slice_for, Entry, Relay, Route};

use feedhub_net::{CancelToken, NetClient, NetError, NetRequest, NetStream, Priority, Proxy};

/// 一次请求最多读多少字节的清单（清单本来就小；超过就是取错了东西）。
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

/// 中转自己的内容类型（我们拼 / 改写的清单）。
const DASH_TYPE: &str = "application/dash+xml";
const HLS_TYPE: &str = "application/vnd.apple.mpegurl";

/// 排队等处理线程的连接上限（超了直接回 503，不无限排队）。
const MAX_QUEUE: usize = 256;

/// 一次上游请求的记账（检查工具 / 吞吐测量台用）。
#[derive(Debug, Clone)]
pub struct UpstreamStat {
    pub bytes: i64,
    pub ms: i64,
    /// 播放器要的 `Range`（没要就是 `None`）。
    pub range: Option<String>,
    pub ok: bool,
}

/// 一条上游请求怎么发（代理 / 平台 / 超时）。
///
/// `proxy` 是**逐请求**给的一档：代理设置变了整份 `MediaSource` 会换掉（`AppGraph.watchProxySettings`），
/// 所以档位由宿主在起这个服务的时候算好、每条请求照用（和接口那条路同一份判定）。
#[derive(Debug, Clone)]
pub struct UpstreamPlan {
    pub platform: String,
    pub proxy: Proxy,
    /// 读超时（毫秒；`0` = 客户端默认）。
    pub read_timeout_ms: u32,
}

impl Default for UpstreamPlan {
    fn default() -> Self {
        Self {
            platform: "player".into(),
            proxy: Proxy::Direct,
            read_timeout_ms: 0,
        }
    }
}

/// 中转服务的配置。
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// 上游怎么发（代理 / 平台 / 超时）。
    pub plan: UpstreamPlan,
    /// 拉流要带的请求头（`MediaSource.headers`：B站是 Referer / User-Agent）。
    ///
    /// 为什么由中转加、而不是靠 demux 的 `http-headers` 事件：分片请求是 demux 自己发的，
    /// 那个事件根本不会触发（这就是整个中转存在的理由）。
    pub headers: Vec<(String, String)>,
    /// 最多同时处理几个请求（和搬之前 `THREADS = 8` 一样）。
    pub threads: usize,
    /// 调试日志（`-Dfeedhub.relay.debug=1`）。
    pub debug: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            plan: UpstreamPlan::default(),
            headers: Vec::new(),
            threads: 8,
            debug: false,
        }
    }
}

/// 上游请求结束时的回调（检查工具 / 吞吐测量台）。
pub type UpstreamHook = Arc<dyn Fn(UpstreamStat) + Send + Sync>;

/// **本机回环上的分片中转服务**（`FfiRelayServer` 的 Rust 内核）。
pub struct RelayServer {
    relay: Arc<Mutex<Relay>>,
    client: Arc<NetClient>,
    config: Arc<RelayConfig>,
    /// 在飞的那些上游请求的取消令牌（**每条请求一个**：`close()` 要把它们一起掐掉，
    /// 但一条请求被取消**不能**影响别的——播放器拖动进度条时正好是这个场景）。
    cancels: Arc<Mutex<Vec<Arc<CancelToken>>>>,
    closed: Arc<AtomicBool>,
    addr: SocketAddr,
    upstream: Arc<AtomicI64>,
    hosts: Arc<Mutex<Vec<String>>>,
    hook: Arc<Mutex<Option<UpstreamHook>>>,
    in_flight: Arc<AtomicI64>,
    max_in_flight: Arc<AtomicI64>,
    /// 排队 + 处理线程的账（`close` 只置标志，不等它们跑完）。
    queue: Arc<(Mutex<VecDeque<TcpStream>>, Condvar)>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// 起过几个处理线程（`close` 之后不再起新的）。
    workers: Arc<AtomicUsize>,
}

impl RelayServer {
    /// 起一个：绑回环、内核选端口、起监听线程和 [RelayConfig::threads] 个处理线程。
    pub fn start(client: Arc<NetClient>, config: RelayConfig) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        // 播放器看到的基地址：**一律 127.0.0.1**（`InetAddress.getLoopbackAddress()` 在 Linux 上就是它）
        let base = format!("http://127.0.0.1:{}", addr.port());
        let threads = config.threads.max(1);
        let server = Self {
            relay: Arc::new(Mutex::new(Relay::new(&base))),
            client,
            config: Arc::new(config),
            cancels: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(AtomicBool::new(false)),
            addr,
            upstream: Arc::new(AtomicI64::new(0)),
            hosts: Arc::new(Mutex::new(Vec::new())),
            hook: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(AtomicI64::new(0)),
            max_in_flight: Arc::new(AtomicI64::new(0)),
            queue: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())),
            threads: Mutex::new(Vec::new()),
            workers: Arc::new(AtomicUsize::new(0)),
        };
        server.spawn(listener, threads);
        Ok(server)
    }

    /// 播放器看到的基地址（`http://127.0.0.1:<port>`）。
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// 装一个"上游请求结束"的回调（检查工具用）。
    pub fn set_on_upstream_done(&self, f: Option<UpstreamHook>) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = f;
    }

    /// 已经向上游取过多少次。
    pub fn upstream_requests(&self) -> i64 {
        self.upstream.load(Ordering::Relaxed)
    }

    /// 向上游取过的地址的主机名（检查工具用）。
    pub fn upstream_hosts(&self) -> Vec<String> {
        self.hosts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 曾经同时有几个上游请求在飞（检查工具用：看 demux 是串行取还是并发取）。
    pub fn max_in_flight(&self) -> i64 {
        self.max_in_flight.load(Ordering::Relaxed)
    }

    pub fn in_flight(&self) -> i64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    // ---------------------------------------------------------------- 登记

    /// 登记一个远端资源（分片 / 单文件 / 密钥），返回本机地址。
    pub fn resource(&self, remote_url: &str, id: &str) -> String {
        self.with_relay(|r| r.register_resource(remote_url, id))
    }

    /// 登记一个远端清单（`.mpd` 现取现改写，其余当 m3u8）。
    pub fn playlist(&self, remote_url: &str, id: &str) -> String {
        self.with_relay(|r| r.register_playlist(remote_url, id))
    }

    /// 登记一个远端分片模板（`$Number$` 这类占位符）。
    pub fn template(&self, remote_template: &str, id: &str) -> String {
        self.with_relay(|r| r.register_template(remote_template, id))
    }

    /// 放一份我们自己拼好、已经改写过的清单。
    pub fn local(&self, body: &str, content_type: Option<&str>, id: &str) -> String {
        self.with_relay(|r| r.register_local(body, content_type, id))
    }

    /// 我们自己的 DASH 清单（`DashManifest` 拼的那份）：把里面的远端地址改写成本机地址后放进来。
    pub fn publish_mpd(&self, mpd: &str, base_url: Option<&str>, id: &str) -> String {
        let rewritten = self.with_relay(|r| rewrite_dash(mpd, base_url, r));
        self.local(&rewritten, Some(DASH_TYPE), id)
    }

    /// 登记表里有多少条 / 有哪些 id / 某个 id 的条目（检查工具 / 测试用）。
    pub fn len(&self) -> usize {
        self.with_relay(|r| r.len())
    }

    pub fn registered_ids(&self) -> Vec<String> {
        self.with_relay(|r| r.registered_ids())
    }

    /// 某个 id 的条目摘要（检查工具用；**不含 URL 的 query**）。
    pub fn entry_debug(&self, id: &str) -> Option<String> {
        self.with_relay(|r| {
            r.entry(id).map(|e| match e {
                Entry::Local { content_type, .. } => format!("Local({content_type:?})"),
                Entry::Remote { dash, .. } => format!("Remote(dash={dash})"),
                Entry::Resource { .. } => "Resource".to_string(),
                Entry::Template { tokens, .. } => format!("Template({tokens:?})"),
            })
        })
    }

    fn with_relay<T>(&self, f: impl FnOnce(&mut Relay) -> T) -> T {
        let mut guard = self.relay.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    // ---------------------------------------------------------------- 线程

    fn spawn(&self, listener: TcpListener, threads: usize) {
        let ctx = self.ctx();
        let mut handles = self.threads.lock().unwrap_or_else(|e| e.into_inner());

        // 处理线程：从队列里取连接，一个请求一个（在自己的线程里同步取上游）
        for i in 0..threads {
            let ctx = ctx.clone();
            handles.push(
                std::thread::Builder::new()
                    .name(format!("video-relay-{i}"))
                    .spawn(move || loop {
                        let stream = {
                            let (lock, cv) = &*ctx.queue;
                            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                            while q.is_empty() {
                                if ctx.closed.load(Ordering::SeqCst) {
                                    return;
                                }
                                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                            }
                            match q.pop_front() {
                                Some(s) => s,
                                None => continue,
                            }
                        };
                        ctx.handle(stream);
                    })
                    .expect("起不了中转的处理线程"),
            );
        }
        self.workers.store(threads, Ordering::SeqCst);

        // 监听线程：accept 之后丢进队列
        let ctx = ctx.clone();
        handles.push(
            std::thread::Builder::new()
                .name("video-relay-accept".into())
                .spawn(move || {
                    let _ = listener.set_nonblocking(false);
                    loop {
                        if ctx.closed.load(Ordering::SeqCst) {
                            return;
                        }
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let (lock, cv) = &*ctx.queue;
                                let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                                if q.len() >= MAX_QUEUE {
                                    drop(q);
                                    let _ = write_simple(
                                        &mut stream,
                                        503,
                                        "中转太忙",
                                        "text/plain; charset=utf-8",
                                    );
                                    let _ = stream.shutdown(Shutdown::Both);
                                    continue;
                                }
                                q.push_back(stream);
                                cv.notify_one();
                            }
                            Err(_) => {
                                if ctx.closed.load(Ordering::SeqCst) {
                                    return;
                                }
                                // 别空转：等一下再来（正常只在 close 的连一下之后走到这里）
                                std::thread::sleep(Duration::from_millis(20));
                            }
                        }
                    }
                })
                .expect("起不了中转的监听线程"),
        );
    }

    /// 处理线程要用到的那份共享状态（拷 `Arc` 进线程）。
    fn ctx(&self) -> Handler {
        Handler {
            relay: self.relay.clone(),
            client: self.client.clone(),
            config: self.config.clone(),
            cancels: self.cancels.clone(),
            closed: self.closed.clone(),
            upstream: self.upstream.clone(),
            hosts: self.hosts.clone(),
            hook: self.hook.clone(),
            in_flight: self.in_flight.clone(),
            max_in_flight: self.max_in_flight.clone(),
            queue: self.queue.clone(),
        }
    }

    /// 关掉：掐在飞的上游请求、停 accept。**幂等**。
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // 在飞的上游请求一起掐掉（播放器关页面 / 拆管线时）
        for t in self.cancels.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            t.cancel();
        }
        // 连一下自己的监听端口：阻塞在 `accept` 上的那条线程立刻返回（Linux 上 `shutdown` 对
        // 阻塞中的 `accept` 不一定生效，这一下是可靠的）
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        // 叫醒排队等活的处理线程
        let (lock, cv) = &*self.queue;
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        cv.notify_all();
    }
}

impl Drop for RelayServer {
    fn drop(&mut self) {
        self.close();
    }
}

/// 一个处理线程能用到的那份状态。
#[derive(Clone)]
struct Handler {
    relay: Arc<Mutex<Relay>>,
    client: Arc<NetClient>,
    config: Arc<RelayConfig>,
    cancels: Arc<Mutex<Vec<Arc<CancelToken>>>>,
    closed: Arc<AtomicBool>,
    upstream: Arc<AtomicI64>,
    hosts: Arc<Mutex<Vec<String>>>,
    hook: Arc<Mutex<Option<UpstreamHook>>>,
    in_flight: Arc<AtomicI64>,
    max_in_flight: Arc<AtomicI64>,
    queue: Arc<(Mutex<VecDeque<TcpStream>>, Condvar)>,
}

/// 一个解析出来的 HTTP 请求（只处理中转要用的那几样）。
struct Req {
    method: String,
    path: String,
    raw_query: Option<String>,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn is_head(&self) -> bool {
        self.method.eq_ignore_ascii_case("HEAD")
    }
}

/// 一个响应头（有序）。
struct OutHeader {
    name: String,
    value: String,
}

/// 响应体的写法：定长 / chunked / 不带体。
enum BodyMode {
    Length(u64),
    Chunked,
    None,
}

impl Handler {
    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_nodelay(true);
        let req = match read_request(&mut stream) {
            Some(r) => r,
            None => {
                let _ = write_simple(&mut stream, 400, "请求不合法", "text/plain; charset=utf-8");
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
        };
        if self.config.debug {
            eprintln!(
                "video-relay: << {} {}?{} range={:?}",
                req.method,
                req.path,
                req.raw_query.as_deref().unwrap_or(""),
                req.header("Range")
            );
        }
        if let Err(e) = self.dispatch(&mut stream, &req) {
            if self.config.debug {
                eprintln!("video-relay: !! 处理 {} 失败：{e}", req.path);
            }
        }
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    }

    fn dispatch(&self, stream: &mut TcpStream, req: &Req) -> std::io::Result<()> {
        let route = {
            let guard = self.relay.lock().unwrap_or_else(|e| e.into_inner());
            guard.route(&req.path, req.raw_query.as_deref())
        };
        match route {
            Route::NotFound => {
                write_simple(stream, 404, "没有这个中转地址", "text/plain; charset=utf-8")
            }
            Route::BadTemplate => {
                write_simple(stream, 400, "分片模板的占位符对不上", "text/plain; charset=utf-8")
            }
            Route::Body { body, content_type } => {
                let ty = content_type.unwrap_or_else(|| DASH_TYPE.to_string());
                write_body(stream, req, body.as_bytes(), &ty)
            }
            Route::RemoteManifest { url, dash } => {
                let text = match self.fetch_text(&url) {
                    Ok(t) => t,
                    Err(e) => {
                        return write_simple(
                            stream,
                            502,
                            &format!("中转取不到清单：{e}"),
                            "text/plain; charset=utf-8",
                        )
                    }
                };
                let rewritten = {
                    let mut guard = self.relay.lock().unwrap_or_else(|e| e.into_inner());
                    if dash {
                        rewrite_dash(&text, Some(&url), &mut *guard)
                    } else {
                        rewrite_hls(&url, &text, &mut *guard)
                    }
                };
                let ty = if dash { DASH_TYPE } else { HLS_TYPE };
                write_body(stream, req, rewritten.as_bytes(), ty)
            }
            Route::Upstream { url } => self.proxy_fetch(stream, req, &url),
        }
    }

    /// 上游拿数据、**边收边转发**给播放器；播放器断开就把上游停掉。
    fn proxy_fetch(&self, stream: &mut TcpStream, req: &Req, url: &str) -> std::io::Result<()> {
        let started = Instant::now();
        let range = req.header("Range").map(|s| s.to_string());
        self.count_upstream(url);
        let now = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_in_flight.fetch_max(now, Ordering::Relaxed);
        let mut bytes = 0i64;
        let mut ok = false;
        let mut cancel_tokens: Vec<Arc<CancelToken>> = Vec::new();
        let outcome = self.stream_upstream(stream, req, url, &mut bytes, &mut ok, &mut cancel_tokens);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        {
            let hook = self.hook.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(h) = hook {
                h(UpstreamStat {
                    bytes,
                    ms: started.elapsed().as_millis() as i64,
                    range,
                    ok,
                });
            }
        }
        if let Err(e) = &outcome {
            if self.config.debug {
                eprintln!("video-relay: !! 上游失败 {url}：{e}");
            }
        }
        self.forget_cancels(&cancel_tokens);
        // 播放器断开 / 上游出错：上游已经被 `NetStream` 的 drop 掐掉了，这里没有别的可做
        Ok(())
    }

    fn stream_upstream(
        &self,
        stream: &mut TcpStream,
        req: &Req,
        url: &str,
        bytes: &mut i64,
        ok: &mut bool,
        cancel_tokens: &mut Vec<Arc<CancelToken>>,
    ) -> std::io::Result<()> {
        // 平台头在前、播放器的 `Range` / `If-Range` 在后（顺序和搬之前一样）
        let mut headers: Vec<(String, String)> = self.config.headers.clone();
        if let Some(v) = req.header("Range") {
            headers.push(("Range".into(), v.to_string()));
        }
        if let Some(v) = req.header("If-Range") {
            headers.push(("If-Range".into(), v.to_string()));
        }
        let (net_req, token) = self.net_request(req.method.clone(), url, headers);
        cancel_tokens.push(token);
        let mut up: NetStream = match self.client.execute_stream(net_req) {
            Ok(s) => s,
            Err(e) => {
                let msg = match e {
                    NetError::ProxyUnavailable { detail } => format!("中转取不到上游：{detail}"),
                    other => format!("中转取不到上游：{other}"),
                };
                return write_simple(stream, 502, &msg, "text/plain; charset=utf-8");
            }
        };
        *ok = (200..300).contains(&up.status);
        // 只透传搬之前那四个头（Content-Type / Content-Range / Accept-Ranges / Content-Disposition）
        let mut out = Vec::with_capacity(4);
        for name in ["Content-Type", "Content-Range", "Accept-Ranges", "Content-Disposition"] {
            if let Some(v) = up.header(name) {
                out.push(OutHeader {
                    name: name.to_string(),
                    value: v.to_string(),
                });
            }
        }
        let no_body = up.status == 204 || up.status == 304 || req.is_head();
        if no_body {
            write_head(stream, up.status, &out, BodyMode::None)?;
            return Ok(());
        }
        // 有 Content-Length 就照抄（播放器靠它算进度），没有就 chunked
        let stated = up
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok());
        write_head(
            stream,
            up.status,
            &out,
            match stated {
                Some(n) => BodyMode::Length(n),
                None => BodyMode::Chunked,
            },
        )?;
        if self.config.debug {
            eprintln!("video-relay: >> {} {} len={:?}", up.status, req.path, stated);
        }
        loop {
            match up.read_chunk() {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    if let Err(e) = stream.write_all(&chunk).and_then(|_| stream.flush()) {
                        // 播放器走了：**把上游停掉**
                        up.cancel();
                        return Err(e);
                    }
                    *bytes += chunk.len() as i64;
                }
                Err(e) => {
                    if self.config.debug {
                        eprintln!("video-relay: !! 读上游失败 {url}：{e}");
                    }
                    up.cancel();
                    return Ok(());
                }
            }
        }
        if stated.is_none() {
            // chunked 的结束块
            let _ = stream.write_all(b"0\r\n\r\n");
        }
        Ok(())
    }

    /// 清单 / 播放列表：整段取（它们本来就小）。
    fn fetch_text(&self, url: &str) -> Result<String, String> {
        self.count_upstream(url);
        // `token` 不读，但**必须拿着**：`net_request` 把它记进 `cancels`，`cancel()` 靠它掐在飞的请求。
        let (net_req, _token) = self.net_request("GET".into(), url, self.config.headers.clone());
        let resp = self
            .client
            .execute(net_req)
            .map_err(|e| match e {
                NetError::ProxyUnavailable { detail } => detail,
                other => other.to_string(),
            })?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("上游 {}", resp.status));
        }
        if resp.body.len() > MAX_MANIFEST_BYTES {
            return Err(format!("清单太大（{} 字节）", resp.body.len()));
        }
        String::from_utf8(resp.body).map_err(|e| format!("清单不是 UTF-8：{e}"))
    }

    /// 一条上游请求（**各自一个取消令牌**：`close()` 掐全部，单条被取消只影响自己——播放器
    /// 拖动进度条时会同时有"旧的那块被丢掉"和"新的那块在取"，共用一个令牌会把新的一起掐死）。
    fn net_request(
        &self,
        method: String,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> (NetRequest, Arc<CancelToken>) {
        let token = Arc::new(CancelToken::new());
        {
            let mut all = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
            all.push(token.clone());
        }
        let req = NetRequest {
            platform: self.config.plan.platform.clone(),
            method,
            url: url.to_string(),
            headers,
            body: Vec::new(),
            // 播放器正等着这一块（详情 / 播放都是用户在等），走"前台插队"
            priority: Priority::Interactive,
            cancel: token.clone(),
            read_timeout: if self.config.plan.read_timeout_ms > 0 {
                Some(Duration::from_millis(self.config.plan.read_timeout_ms as u64))
            } else {
                None
            },
            proxy: Some(self.config.plan.proxy.clone()),
            observer: None,
            extra_default_headers: Vec::new(),
            default_headers_hosts: Vec::new(),
            follow_redirects: Some(true),
            // 中转**不带 Cookie**：分片在 CDN 上，登录凭据没必要（也绝不给）发过去
            cookies: Some(Arc::new(feedhub_net::NoCookies)),
            limiter: None,
            gate: None,
            after_limit: None,
            after_headers: None,
        };
        (req, token)
    }

    /// 请求结束就把它的令牌从"关服务要掐的那一批"里摘掉（不然这个表会随着播放一直涨）。
    fn forget_cancels(&self, tokens: &[Arc<CancelToken>]) {
        if tokens.is_empty() {
            return;
        }
        let mut all = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|t| !tokens.iter().any(|x| Arc::ptr_eq(x, t)));
    }

    fn count_upstream(&self, url: &str) {
        self.upstream.fetch_add(1, Ordering::Relaxed);
        if let Ok(u) = url.parse::<http::Uri>() {
            if let Some(h) = u.host() {
                let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
                if !hosts.iter().any(|x| x == h) {
                    hosts.push(h.to_string());
                }
            }
        }
    }
}

// ===========================================================================
// HTTP/1.1 最小实现（只要够播放器用：GET / HEAD + Range）
// ===========================================================================

/// 读一个请求头（读到空行为止）。中转只处理 GET / HEAD，所以不读请求体。
fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let (path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target, None),
    };
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).ok()? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(Req {
        method,
        path,
        raw_query,
        headers,
    })
}

/// 写状态行 + 头（`mode` 决定带不带 `Content-Length` / `Transfer-Encoding`）。
fn write_head(
    stream: &mut TcpStream,
    status: u16,
    headers: &[OutHeader],
    mode: BodyMode,
) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(format!("HTTP/1.1 {status} {}\r\n", reason(status)).as_bytes());
    for h in headers {
        out.extend_from_slice(h.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    match mode {
        BodyMode::Length(n) => {
            out.extend_from_slice(format!("Content-Length: {n}\r\n").as_bytes());
        }
        BodyMode::Chunked => out.extend_from_slice(b"Transfer-Encoding: chunked\r\n"),
        // 204 / 304 / HEAD：**不带长度也不带 chunked**（和 `sendResponseHeaders(code, -1)` 一样）
        BodyMode::None => out.extend_from_slice(b"Connection: close\r\n"),
    }
    out.extend_from_slice(b"\r\n");
    stream.write_all(&out)
}

/// 我们自己生成的内容（清单 / 错误页）：支持 `Range`（206）+ `HEAD`，和搬之前 `sendBytes` 一样。
fn write_body(
    stream: &mut TcpStream,
    req: &Req,
    body: &[u8],
    content_type: &str,
) -> std::io::Result<()> {
    let mut status = 200u16;
    let mut slice: &[u8] = body;
    let mut extra = vec![
        OutHeader {
            name: "Accept-Ranges".into(),
            value: "bytes".into(),
        },
        OutHeader {
            name: "Content-Type".into(),
            value: content_type.to_string(),
        },
        OutHeader {
            name: "Cache-Control".into(),
            value: "no-store".into(),
        },
    ];
    if let Some((from, to)) = slice_for(req.header("Range"), body.len()) {
        status = 206;
        slice = &body[from..=to];
        extra.push(OutHeader {
            name: "Content-Range".into(),
            value: format!("bytes {from}-{to}/{}", body.len()),
        });
    }
    write_head(stream, status, &extra, BodyMode::Length(slice.len() as u64))?;
    if req.is_head() {
        return Ok(());
    }
    stream.write_all(slice)
}

/// 一段纯文本（错误页 / 太忙）。
fn write_simple(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
    content_type: &str,
) -> std::io::Result<()> {
    let body = format!("{message}\n");
    let headers = vec![OutHeader {
        name: "Content-Type".into(),
        value: content_type.to_string(),
    }];
    write_head(stream, status, &headers, BodyMode::Length(body.len() as u64))?;
    stream.write_all(body.as_bytes())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

// ===========================================================================
// UniFFI 边界（`FfiRelayServer`）
// ===========================================================================

/// **中转服务的句柄**（Kotlin 侧 `feedhub.player.RustRelayServer` 就是它的门面）。
///
/// `id` 由**宿主**生成（URL 里的 id 要能对回远端地址，而"生成随机串"留在宿主——Rust 这边
/// 不该为了一个 UUID 把随机源搬进来，那会让"同一份输入两次调用结果一样"不成立）。
#[derive(uniffi::Object)]
pub struct FfiRelayServer {
    inner: Arc<crate::relay_server::RelayServer>,
}

/// 一条上游请求怎么发（对应 [`crate::relay_server::UpstreamPlan`]）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRelayPlan {
    /// 平台 id（只影响限速 / 记账；中转**不挂腾速器**，见 `net_request`）。
    pub platform: String,
    /// 这一档代理（`None` = 直连）。**`Unavailable` = fail-closed：一个字节都不发**。
    pub proxy: Option<crate::net::FfiNetProxy>,
    /// 读超时（毫秒；`0` = 客户端默认）。
    pub read_timeout_ms: u32,
}

/// 起一个中转服务要的参数。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRelayConfig {
    pub plan: FfiRelayPlan,
    /// 拉流要带的请求头（**有序**：`Name: Value`，和 `FfiHostSpec.default_headers` 同一个写法）。
    pub headers: Vec<String>,
    /// 最多同时处理几个请求（`0` = 8，和搬之前一样）。
    pub threads: u32,
    /// 调试日志。
    pub debug: bool,
}

/// 一次上游请求的记账（检查工具 / 吞吐测量台用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRelayUpstreamStat {
    pub bytes: i64,
    pub ms: i64,
    pub range: Option<String>,
    pub ok: bool,
}

#[uniffi::export]
impl FfiRelayServer {
    /// 起一个：绑回环、内核选端口。
    ///
    /// 失败只有一种（绑不上回环端口），抛 [`FfiNetError::Other`]——那件事在正常机器上不会发生。
    #[uniffi::constructor]
    pub fn new(config: FfiRelayConfig) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let mut headers = Vec::with_capacity(config.headers.len());
        for line in &config.headers {
            match line.split_once(':') {
                Some((k, v)) => headers.push((k.trim().to_string(), v.trim().to_string())),
                None => {
                    return Err(crate::net::FfiNetError::BadRequest {
                        detail: format!("请求头要写成 \"Name: Value\"：{line}"),
                    })
                }
            }
        }
        let plan = crate::relay_server::UpstreamPlan {
            platform: config.plan.platform.clone(),
            proxy: config.plan.proxy.map(Into::into).unwrap_or(feedhub_net::Proxy::Direct),
            read_timeout_ms: config.plan.read_timeout_ms,
        };
        // 中转要自己的客户端：**不带 Cookie**（分片在 CDN 上，登录凭据不该发过去）、不限速
        // （播放器要的是"这一块现在就到"，限速那套是给平台接口用的）、不挂任何宿主钩子。
        let client = feedhub_net::NetClient::builder()
            .proxies(feedhub_net::ProxyPlan::direct())
            .build()
            .map_err(|e| crate::net::FfiNetError::Other {
                detail: format!("建不了中转的联网客户端：{e}"),
            })?;
        let threads = if config.threads == 0 { 8 } else { config.threads as usize };
        let server = crate::relay_server::RelayServer::start(
            Arc::new(client),
            crate::relay_server::RelayConfig {
                plan,
                headers,
                threads,
                debug: config.debug,
            },
        )
        .map_err(|e| crate::net::FfiNetError::Other {
            detail: format!("起不了本机中转：{e}"),
        })?;
        Ok(Arc::new(Self {
            inner: Arc::new(server),
        }))
    }

    /// 播放器看到的基地址（`http://127.0.0.1:<port>`）。
    pub fn base(&self) -> String {
        self.inner.base()
    }

    pub fn port(&self) -> u16 {
        self.inner.port()
    }

    /// 登记一个远端资源（分片 / 单文件 / 密钥），返回本机地址。
    pub fn resource(&self, remote_url: String, id: String) -> String {
        self.inner.resource(&remote_url, &id)
    }

    /// 登记一个远端清单（`.mpd` 现取现改写，其余当 m3u8）。
    pub fn playlist(&self, remote_url: String, id: String) -> String {
        self.inner.playlist(&remote_url, &id)
    }

    /// 登记一个远端分片模板（`$Number$` 这类占位符）。
    pub fn template(&self, remote_template: String, id: String) -> String {
        self.inner.template(&remote_template, &id)
    }

    /// 放一份我们自己拼好、已经改写过的清单。
    pub fn local(&self, body: String, content_type: Option<String>, id: String) -> String {
        self.inner.local(&body, content_type.as_deref(), &id)
    }

    /// 我们自己的 DASH 清单：改写远端地址之后放进来，返回本机地址。
    pub fn publish_mpd(&self, mpd: String, base_url: Option<String>, id: String) -> String {
        self.inner.publish_mpd(&mpd, base_url.as_deref(), &id)
    }

    /// 登记表里有多少条。
    pub fn len(&self) -> u64 {
        self.inner.len() as u64
    }

    /// 登记过的 id（登记顺序）。
    pub fn registered_ids(&self) -> Vec<String> {
        self.inner.registered_ids()
    }

    /// 某个 id 的条目摘要（检查工具用；**不含 URL 的 query**）。
    pub fn entry_debug(&self, id: String) -> Option<String> {
        self.inner.entry_debug(&id)
    }

    /// 已经向上游取过多少次。
    pub fn upstream_requests(&self) -> i64 {
        self.inner.upstream_requests()
    }

    /// 向上游取过的地址的主机名。
    pub fn upstream_hosts(&self) -> Vec<String> {
        self.inner.upstream_hosts()
    }

    /// 曾经同时有几个上游请求在飞。
    pub fn max_in_flight(&self) -> i64 {
        self.inner.max_in_flight()
    }

    /// 现在有几个上游请求在飞（测试用：播放器断开之后它必须很快归零）。
    pub fn in_flight(&self) -> i64 {
        self.inner.in_flight()
    }

    /// 装一个"一次上游请求结束"的回调（检查工具 / 吞吐测量台用；`null` = 卸掉）。
    ///
    /// 在**处理线程**里调：实现方别做重活（搬之前 `VideoRelay` 那条也是这个口径）。
    pub fn set_on_upstream_done(&self, hook: Option<Arc<dyn FfiRelayUpstreamHook>>) {
        match hook {
            None => self.inner.set_on_upstream_done(None),
            Some(h) => self.inner.set_on_upstream_done(Some(Arc::new(move |stat| {
                h.upstream_done(FfiRelayUpstreamStat {
                    bytes: stat.bytes,
                    ms: stat.ms,
                    range: stat.range.clone(),
                    ok: stat.ok,
                });
            }))),
        }
    }

    /// 关掉（幂等）：掐在飞的上游请求、停 accept。
    ///
    /// **不叫 `close`**：UniFFI 会给每个对象生成 `AutoCloseable.close`，同名会撞车
    pub fn shutdown(&self) {
        self.inner.close();
    }
}

/// 上游请求结束的回调（`set_on_upstream_done` 用）。
///
/// **不许从这里往回抛异常**：这是 `with_foreign` 的 trait，抛出去会被 UniFFI 包成
/// `UnexpectedUniFFICallbackError`。实现里自己 `runCatching`。
#[uniffi::export(with_foreign)]
pub trait FfiRelayUpstreamHook: Send + Sync {
    fn upstream_done(&self, stat: FfiRelayUpstreamStat);
}

