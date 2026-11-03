//! 内置代理核心的**状态机 + 子进程监管**（`core/proxy/ProxyCore.kt` 的 Rust 版）。
//!
//! 只搬"决策"和"起子进程"这两件：什么时候起、起不来怎么办（退避 / 次数上限 / 跑够多久清零）、
//! 健康检查什么时候算成功、端口怎么选、什么时候该停、状态怎么变、日志留哪些行。
//! **真正发请求（取订阅）走宿主**（`HostProxyFetch`），**前台的定时触发**（定时查自动更新 / 等宽限期）
//! 也留在宿主——那是"定时器 / 协程触发"，
//!
//! ## 线程模型
//!
//! 一个监管对象里面只有"数据 + 一把锁"，**没有后台线程**（和 `.so` 里其它层一样：闲置零唤醒）。
//! 宿主按自己的节奏调这些方法：
//!
//! | 宿主调 | Rust 干的事 |
//! |---|---|
//! | `set_enabled(true)` | 校验 sha256 / 读版本号 / 生成配置 / 挑端口 / **起子进程** / 等健康检查 |
//! | `set_enabled(false)` | kill 子进程，状态回 `Stopped` |
//! | `set_foreground(true/false)` | 「只在前台跑」判定表：告诉宿主**现在该不该跑**（含宽限期） |
//! | `poll()` | 子进程死了？→ 退避重启 / 到上限进 `Failed`；跑够 `stable_ms` → 连续重启次数清零 |
//! | `test_latency(node_id)` | 另起一个一次性 sing-box（`tools fetch`），量墙钟时间 |
//!
//! `poll()` 由宿主的定时器（桌面是协程、安卓是同一个）每几百毫秒调一次——**不是 Rust 里的睡眠循环**：
//! 那样 `.so` 里就多一个常驻线程，而且取消不掉。
//!
//! ## 状态
//!
//! [`CoreStatus`] 和 Kotlin 的 `sealed interface CoreStatus` 一一对应（`Stopped` / `Starting` /
//! `Running` / `Restarting` / `Failed`），过 FFI 之后 Kotlin 照原样用。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::proxy_core::binary;
use crate::proxy_core::config;
use crate::proxy_core::model::{save_state, ProxyCoreState, ProxyNode};

/// 核心入站只监听这个 IPv4 地址。
pub const CORE_LOOPBACK: [u8; 4] = [127, 0, 0, 1];

/// 最近的日志留多少行（进阶设置页里看）。
pub const MAX_LOG_LINES: usize = 200;

/// 生成的 sing-box 配置文件放在 `data_dir` 里的名字。
pub const CONFIG_FILE: &str = "sing-box-config.json";

/// 内置核心的运行状态（Kotlin `sealed interface CoreStatus`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreStatus {
    /// 没启用（设置里没选「内置核心」）。
    Stopped,
    /// 正在起（`attempt` 是第几次，0 = 第一次）。
    Starting { attempt: i32 },
    Running {
        port: i32,
        /// `sing-box version` 读出来的版本号（读不到就是 `None`）。
        version: Option<String>,
        pid: i64,
        started_at: i64,
    },
    /// 挂了，正在退避重试。
    Restarting {
        attempt: i32,
        delay_ms: i64,
        reason: String,
    },
    /// 起不来 / 连续重启到上限。
    Failed { message: String },
}

impl CoreStatus {
    /// 没在运行时给用户看的原因（会出现在 `ProxyConfigGuardInterceptor` 抛出的异常文案里）。
    /// 文案和 Kotlin 的 `CoreStatus.unavailableReason()` 逐字一样。
    pub fn unavailable_reason(&self) -> String {
        match self {
            CoreStatus::Failed { message } => message.clone(),
            CoreStatus::Starting { attempt } => format!("正在启动（第 {} 次）", attempt + 1),
            CoreStatus::Restarting { reason, .. } => format!("正在重启：{reason}"),
            CoreStatus::Running { .. } => String::new(),
            CoreStatus::Stopped => {
                "没有运行（在「设置 → 网络 → 代理」里选「内置核心」后会自动启动）".to_string()
            }
        }
    }

    /// 在跑的那个端口（没有就是 `None`）。
    pub fn port(&self) -> Option<i32> {
        match self {
            CoreStatus::Running { port, .. } => Some(*port),
            _ => None,
        }
    }
}

/// 一次延迟测试的结果。`error` 里不含密码 / uuid。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyResult {
    pub node_id: String,
    pub ms: Option<i32>,
    pub error: Option<String>,
}

/// 网络错误 → 一句不含订阅地址的话（**不要把异常 message 直接给用户**：里面常带订阅主机名）。
///
/// 宿主把网络库的异常按这几类报上来（`kind` 是小写串：`unknown_host` / `connect` / `timeout` /
/// 其它任意值），这里换成 Kotlin 的原文案。**和 Kotlin 的 `networkReason` 逐字一样。**
pub fn network_reason(kind: &str) -> String {
    match kind {
        "unknown_host" | "unknowhostexception" => "域名解析失败".to_string(),
        "connect" | "connectexception" => "连不上".to_string(),
        "timeout" | "sockettimeoutexception" => "超时".to_string(),
        other => format!("请求失败（{other}）"),
    }
}

/// 去掉 sing-box 输出里的 ANSI 颜色码（`\u001B\[[0-9;]*m`）。
pub fn clean_log_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && i + 1 < chars.len() && chars[i + 1] == '[' {
            // Kotlin 的正则是 `\u001B\[[0-9;]*m`（分号也算，`\u001b[1;32m` 这种复合的要去掉）
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ';') {
                j += 1;
            }
            if j < chars.len() && chars[j] == 'm' {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 监管理对象的配置（Kotlin `ProxyCore` 的构造参数）。
#[derive(Debug, Clone)]
pub struct CoreOptions {
    /// sing-box 可执行文件。
    pub binary: PathBuf,
    /// 放生成的配置 / 探针配置的目录（权限 600 的文件写在里面）。
    pub data_dir: PathBuf,
    /// `proxy-core.json` 的路径（`None` = 只在内存里，测试 / 截图用）。
    pub state_file: Option<PathBuf>,
    /// 二进制的 sha256 期望值（`None` = 不校验）。
    pub expected_sha256: Option<String>,
    /// 健康检查超时（毫秒）。
    pub health_timeout_ms: u64,
    /// 连续重启次数上限。
    pub max_restarts: i32,
    /// 跑够这么久没挂就把"连续重启次数"清零（毫秒）。
    pub stable_ms: i64,
    /// 单个节点的延迟测试超时（毫秒）。
    pub latency_timeout_ms: u64,
    /// 宿主报上来的当前时间（毫秒）。**注入时钟**，测试才能断言"跑够 60 秒"。
    pub now_ms: i64,
}

impl CoreOptions {
    pub fn new(binary: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> CoreOptions {
        CoreOptions {
            binary: binary.into(),
            data_dir: data_dir.into(),
            state_file: None,
            // 和 Kotlin 的默认参数一样：按体系结构推（`SingBoxBinary.expectedSha256(binary)`）
            expected_sha256: binary::expected_sha256(std::env::consts::ARCH, None),
            health_timeout_ms: 15_000,
            max_restarts: 5,
            stable_ms: 60_000,
            latency_timeout_ms: 8_000,
            now_ms: 0,
        }
    }
}

#[derive(Default)]
struct LogRing {
    lines: Vec<String>,
    seq: u64,
}

impl LogRing {
    fn push(&mut self, line: String) {
        self.lines.push(line);
        if self.lines.len() > MAX_LOG_LINES {
            let drop_count = self.lines.len() - MAX_LOG_LINES;
            self.lines.drain(..drop_count);
        }
        self.seq = self.seq.wrapping_add(1);
    }

    fn clear(&mut self) {
        if !self.lines.is_empty() {
            self.lines.clear();
            self.seq = self.seq.wrapping_add(1);
        }
    }
}

struct ChildExitState {
    child: Option<Child>,
    exit_code: Option<i32>,
    stopping: bool,
}

struct RunningChild {
    shared: Arc<(Mutex<ChildExitState>, Condvar)>,
}

impl RunningChild {
    fn spawn(
        mut child: Child,
        logs: Arc<Mutex<LogRing>>,
        logs_subscribed: Arc<AtomicBool>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let pid = child.id();
        for (name, stream) in [
            (
                "singbox-stdout",
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            ),
            (
                "singbox-stderr",
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            ),
        ] {
            let Some(stream) = stream else { continue };
            let logs = logs.clone();
            let logs_subscribed = logs_subscribed.clone();
            let wake = wake.clone();
            let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
                use std::io::BufRead;
                let reader = std::io::BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(raw) = line else { break };
                    let cleaned = clean_log_line(&raw);
                    if crate::hosted::is_kotlin_blank(&cleaned) {
                        continue;
                    }
                    logs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(cleaned);
                    if logs_subscribed.load(Ordering::Acquire) {
                        if let Some(w) = &wake {
                            w();
                        }
                    }
                }
            });
        }

        let shared = Arc::new((
            Mutex::new(ChildExitState {
                child: Some(child),
                exit_code: None,
                stopping: false,
            }),
            Condvar::new(),
        ));
        let shared_wait = shared.clone();
        let _ = std::thread::Builder::new()
            .name("singbox-wait".into())
            .spawn(move || {
                #[cfg(unix)]
                {
                    loop {
                        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                        let rc = unsafe {
                            libc::waitid(
                                libc::P_PID,
                                pid as libc::id_t,
                                &mut info,
                                libc::WEXITED | libc::WNOWAIT,
                            )
                        };
                        if rc == 0 {
                            break;
                        }
                        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                        {
                            break;
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    loop {
                        let done = {
                            let (lock, _) = &*shared_wait;
                            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                            if st.exit_code.is_some() {
                                true
                            } else if let Some(c) = st.child.as_mut() {
                                match c.try_wait() {
                                    Ok(Some(status)) => {
                                        st.exit_code = Some(status.code().unwrap_or(-1));
                                        st.child = None;
                                        true
                                    }
                                    Ok(None) => false,
                                    Err(_) => true,
                                }
                            } else {
                                true
                            }
                        };
                        if done {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
                let notify_host = {
                    let (lock, cv) = &*shared_wait;
                    let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                    if st.exit_code.is_none() {
                        let code = match st.child.as_mut() {
                            Some(c) => c.wait().ok().and_then(|s| s.code()).unwrap_or(-1),
                            None => -1,
                        };
                        st.child = None;
                        st.exit_code = Some(code);
                    }
                    cv.notify_all();
                    !st.stopping
                };
                if notify_host {
                    if let Some(w) = &wake {
                        w();
                    }
                }
            });

        Self { shared }
    }

    fn try_exit_code(&self) -> Option<i32> {
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(code) = st.exit_code {
            return Some(code);
        }
        if let Some(c) = st.child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                let code = status.code().unwrap_or(-1);
                st.child = None;
                st.exit_code = Some(code);
                cv.notify_all();
                return Some(code);
            }
        }
        None
    }

    fn kill_and_wait(&self) {
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
        st.stopping = true;
        if st.exit_code.is_some() {
            return;
        }
        if let Some(c) = st.child.as_mut() {
            let _ = c.kill();
        }
        let _ = cv.wait_timeout_while(st, Duration::from_secs(2), |s| s.exit_code.is_none());
    }
}

/// 核心监管对象。**不是** `uniffi::Object`：FFI 那边有一个薄壳（`FfiProxyCore`），
/// 这一层是纯 Rust、可测。
pub struct CoreSupervisor {
    options: CoreOptions,
    state: ProxyCoreState,
    status: CoreStatus,
    logs: Arc<Mutex<LogRing>>,
    logs_subscribed: Arc<AtomicBool>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    desired_running: bool,
    attempts: i32,
    version: Option<String>,
    /// 上一次 sha256 校验通过时的（大小, mtime 毫秒）。
    verified_stamp: Option<(u64, i64)>,
    child: Option<RunningChild>,
    /// 当前这个子进程是什么时候健康起来的（`stable_ms` 计时用）。
    healthy_at_ms: Option<i64>,
    /// 退避等待：到点了该重启（宿主每次 `poll()` 时看）。
    restart_at_ms: Option<i64>,
    /// 退避等待的原因（到点重启时写日志）。
    restart_reason: Option<String>,
    /// 上一次 `poll()` 报给宿主的「该不该跑」，重试时用。
    clock_base: Option<Instant>,
    /// 子进程已经起了、还没等到入站（[`CoreStatus::Starting`]）：探活在 [`CoreSupervisor::poll`] 里一步一步做。
    pending: Option<PendingStart>,
}

/// 一次"起了进程、还在等它 bind"。
#[derive(Debug, Clone, Copy)]
struct PendingStart {
    port: i32,
    pid: i64,
    /// 起进程那一刻（宿主时钟）：`Running.started_at` 用它，不用探活成功那一刻。
    started_ms: i64,
    /// 到这个时刻（宿主时钟）还连不上就算没起来。
    deadline_ms: i64,
}

/// 等 sing-box bind 时宿主多久 `poll` 一次（毫秒）：和原来同步等待里每轮 60ms 的节奏一样。
pub const STARTUP_PROBE_MS: i64 = 60;

impl CoreSupervisor {
    pub fn new(options: CoreOptions, state: ProxyCoreState) -> CoreSupervisor {
        CoreSupervisor {
            options,
            state,
            status: CoreStatus::Stopped,
            logs: Arc::new(Mutex::new(LogRing::default())),
            logs_subscribed: Arc::new(AtomicBool::new(false)),
            wake: None,
            desired_running: false,
            attempts: 0,
            version: None,
            verified_stamp: None,
            child: None,
            healthy_at_ms: None,
            restart_at_ms: None,
            restart_reason: None,
            clock_base: None,
            pending: None,
        }
    }

    /// 注册子进程退出 / 订阅日志到达时的事件唤醒回调（无子进程时不触发任何唤醒）。
    pub fn set_wake_callback(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    /// 告知当前是否有界面在观察日志流（未观察时子进程日志仅写入环形缓冲，不唤醒宿主）。
    pub fn set_logs_subscribed(&self, subscribed: bool) {
        self.logs_subscribed.store(subscribed, Ordering::Release);
    }

    // -----------------------------------------------------------------------
    // 读
    // -----------------------------------------------------------------------

    pub fn status(&self) -> &CoreStatus {
        &self.status
    }

    pub fn state(&self) -> &ProxyCoreState {
        &self.state
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lines
            .clone()
    }

    pub fn logs_seq(&self) -> u64 {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    /// 「内置核心」这一档的请求走法：在跑给端口，没在跑给 `None` + 原因（宿主据此 fail-closed）。
    ///
    /// 对应 Kotlin 的 `ProxyCore.proxyMode()`：`Some(port)` = `ProxyMode.BuiltinCore(port)`；
    /// `None` = `ProxyMode.BuiltinCore(0, reason)`。
    pub fn proxy_mode(&self) -> (Option<i32>, String) {
        match &self.status {
            CoreStatus::Running { port, .. } => (Some(*port), String::new()),
            other => (None, other.unavailable_reason()),
        }
    }

    pub fn running(&self) -> bool {
        self.status.port().is_some()
    }

    pub fn port(&self) -> Option<i32> {
        self.status.port()
    }

    /// 生成的 sing-box 配置的路径（**有机密**，权限 600）。
    pub fn config_path(&self) -> PathBuf {
        self.options.data_dir.join(CONFIG_FILE)
    }

    pub fn data_dir(&self) -> &Path {
        &self.options.data_dir
    }

    pub fn binary(&self) -> &Path {
        &self.options.binary
    }

    pub fn clear_logs(&mut self) {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    // -----------------------------------------------------------------------
    // 前后台 / 维护
    // -----------------------------------------------------------------------

    /// 宿主把"现在在前台吗"喂进来（安卓从 Activity 生命周期，桌面永远是 `true`）。
    ///
    /// **这里不做定时**：宽限期由 [`crate::proxy_core::foreground::ForegroundGate`] 算
    /// （它是纯判定表，宿主负责 `delay`）。
    pub fn set_desired_running(&mut self, enabled: bool, now_ms: i64) {
        if enabled == self.desired_running && (enabled || self.child.is_none()) {
            return;
        }
        self.desired_running = enabled;
        if !enabled {
            self.log("内置代理核心：停止");
            self.stop_process();
            self.status = CoreStatus::Stopped;
            self.restart_at_ms = None;
            self.restart_reason = None;
            self.healthy_at_ms = None;
            return;
        }
        self.log("内置代理核心：启动");
        self.start_loop(now_ms);
    }

    /// 手动重启（进阶设置页的按钮；挂了以后自动重启到了上限也用它）。
    pub fn restart(&mut self, now_ms: i64) {
        self.desired_running = true;
        self.stop_process();
        self.attempts = 0;
        self.restart_at_ms = None;
        self.restart_reason = None;
        self.start_loop(now_ms);
    }

    /// 节点 / 选中项 / 日志级别变了：在跑就换一份新配置重启（很快就回来）。
    pub fn restart_for_config_change(&mut self, now_ms: i64) {
        if !self.desired_running {
            return;
        }
        self.stop_process();
        self.healthy_at_ms = None;
        self.start_loop(now_ms);
    }

    /// 换一份状态（`ProxyCoreStore.update` 的 Rust 侧：改状态 + 落盘）。
    pub fn set_state(&mut self, next: ProxyCoreState) -> bool {
        if next == self.state {
            return false;
        }
        self.state = next;
        if let Some(path) = &self.options.state_file {
            // 写盘失败不算致命（Kotlin 那边 `writePrivate` 抛出去会被上层 runCatching 兜住）；
            // 这里记一行日志，别把界面搞挂。
            if let Err(e) = save_state(path, &self.state) {
                self.log(&format!("写 proxy-core.json 失败：{}", e.kind()));
            }
        }
        true
    }

    /// 换状态并返回"配置要不要重启"（测试 / 门面用）。
    pub fn update_state(&mut self, f: impl FnOnce(&ProxyCoreState) -> ProxyCoreState, now_ms: i64) -> bool {
        let before = self.state.clone();
        let changed = self.set_state(f(&self.state));
        if !changed {
            return false;
        }
        if self.state.test_url != before.test_url || self.state.log_level != before.log_level {
            self.restart_for_config_change(now_ms);
        }
        true
    }

    // -----------------------------------------------------------------------
    // 起停
    // -----------------------------------------------------------------------

    fn start_loop(&mut self, now_ms: i64) {
        // 校验过一次、文件的大小和修改时间都没变就不再算（83 MB 的 sha256 在手机上几百毫秒；
        // 安卓上核心每次回前台都会起一次）。
        let stamp = std::fs::metadata(&self.options.binary)
            .ok()
            .map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                (m.len(), mtime)
            });
        if stamp.is_none() || stamp != self.verified_stamp {
            if let Some(problem) = binary::verify(&self.options.binary, self.options.expected_sha256.as_deref()) {
                self.fail(problem);
                return;
            }
            self.verified_stamp = stamp;
        }
        if self.version.is_none() {
            self.version = binary::version(&self.options.binary, 5_000);
        }
        self.start_process(now_ms);
    }

    fn start_process(&mut self, now_ms: i64) {
        if !self.desired_running {
            return;
        }
        let issues = config::core_config_issues(&self.state);
        if !issues.is_empty() {
            self.fail(issues[0].clone());
            return;
        }
        let Some(port) = free_loopback_port() else {
            self.fail("找不到空闲端口（本机回环）".to_string());
            return;
        };
        let config_value = match config::build_core_config(&self.state, port) {
            Ok(v) => v,
            Err(e) => {
                self.fail(e);
                return;
            }
        };
        let config_text = config::to_config_text(&config_value);
        if let Err(e) = crate::proxy_core::model::write_private(&self.config_path(), &config_text) {
            self.fail(format!("写不了 sing-box 配置：{}", e.kind()));
            return;
        }
        self.status = CoreStatus::Starting {
            attempt: self.attempts,
        };
        self.log(&format!(
            "启动 sing-box（127.0.0.1:{port}，{} 个节点，日志级别 {}）",
            self.state.nodes.len(),
            self.state.log_level
        ));
        let child = match Command::new(&self.options.binary)
            .arg("--disable-color")
            .arg("run")
            .arg("-c")
            .arg(self.config_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                self.fail(format!("启动 sing-box 失败：{}", io_kind_name(&e)));
                return;
            }
        };
        let pid = child.id() as i64;
        self.child = Some(RunningChild::spawn(
            child,
            self.logs.clone(),
            self.logs_subscribed.clone(),
            self.wake.clone(),
        ));
        // **不在这里等它 bind**：原来同步等最多 `health_timeout_ms`，
        // FFI 那边整段持着 `inner` 锁，回前台时主线程上的 `setForeground` / 设置页的任何调用都跟着卡。
        // 现在起完进程就返回 `Starting`，探活交给 `poll()`（[`STARTUP_PROBE_MS`] 一次，见 `next_wake_delay_ms`）。
        let timeout = i64::try_from(self.options.health_timeout_ms).unwrap_or(i64::MAX);
        self.pending = Some(PendingStart {
            port,
            pid,
            started_ms: now_ms,
            deadline_ms: now_ms.saturating_add(timeout),
        });
        self.probe_pending(now_ms);
    }

    /// 探一次正在起的核心：进程死了 → 退避重启；端口能连上 → `Running`；过了期限 → 退避重启；
    /// 否则还是 `Starting`。只做一次本机回环的连接（连不上是立刻被拒），不睡。
    fn probe_pending(&mut self, now_ms: i64) {
        let Some(p) = self.pending else { return };
        let exit = match self.child.as_ref() {
            Some(child) => child.try_exit_code(),
            None => {
                self.pending = None;
                return;
            }
        };
        let reason = if let Some(code) = exit {
            format!("核心没起来（进程已退出，exit={code}）")
        } else if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from((CORE_LOOPBACK, p.port as u16)),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            self.pending = None;
            self.status = CoreStatus::Running {
                port: p.port,
                version: self.version.clone(),
                pid: p.pid,
                started_at: p.started_ms,
            };
            self.healthy_at_ms = Some(now_ms);
            self.log(&format!("内置代理核心已就绪：socks5://127.0.0.1:{}", p.port));
            return;
        } else if now_ms >= p.deadline_ms {
            format!("核心没起来（端口 {} 上没等到入站）", p.port)
        } else {
            return;
        };
        self.pending = None;
        self.stop_process();
        self.schedule_restart(&reason, now_ms);
    }

    /// 正在起（还没等到入站）吗。
    pub fn starting(&self) -> bool {
        self.pending.is_some()
    }

    fn schedule_restart(&mut self, reason: &str, now_ms: i64) {
        self.attempts += 1;
        if self.attempts > self.options.max_restarts {
            self.fail(format!(
                "{reason}；已连续重启 {} 次还是不行，不再自动重启（进阶设置页可以手动重启）",
                self.options.max_restarts
            ));
            return;
        }
        // 0.5s → 1s → 2s → 4s → 8s
        let delay_ms = 500i64 << (self.attempts - 1).min(4);
        let delay_ms = delay_ms.min(8_000);
        self.status = CoreStatus::Restarting {
            attempt: self.attempts,
            delay_ms,
            reason: reason.to_string(),
        };
        self.log(&format!(
            "{reason}：{delay_ms}ms 后第 {} 次自动重启",
            self.attempts
        ));
        self.restart_at_ms = Some(now_ms + delay_ms);
        self.restart_reason = Some(reason.to_string());
    }

    fn stop_process(&mut self) {
        self.pending = None;
        let Some(child) = self.child.take() else {
            return;
        };
        child.kill_and_wait();
    }

    fn fail(&mut self, message: String) {
        self.restart_at_ms = None;
        self.restart_reason = None;
        self.healthy_at_ms = None;
        self.status = CoreStatus::Failed {
            message: message.clone(),
        };
        self.log(&format!("内置代理核心不可用：{message}"));
    }

    // -----------------------------------------------------------------------
    // poll + 事件驱动下一次唤醒时刻
    // -----------------------------------------------------------------------

    /// 计算状态机距离下一次需要时间推进还剩多少毫秒（`None` = 不需要任何定时器唤醒，
    /// 如 `Stopped`、`Failed` 或稳定 `Running` 且 `attempts == 0` 时子进程退出由 `singbox-wait` 事件通知）。
    pub fn next_wake_delay_ms(&self, now_ms: i64) -> Option<i64> {
        if !self.desired_running {
            return None;
        }
        let mut earliest: Option<i64> = None;
        let mut take_min = |d: i64| {
            let clamped = d.max(0);
            earliest = Some(match earliest {
                Some(prev) => prev.min(clamped),
                None => clamped,
            });
        };
        if let Some(at) = self.restart_at_ms {
            take_min(at - now_ms);
        }
        if self.pending.is_some() {
            take_min(STARTUP_PROBE_MS);
        }
        if self.attempts > 0 {
            if let (CoreStatus::Running { .. }, Some(healthy_at)) =
                (&self.status, self.healthy_at_ms)
            {
                take_min((healthy_at + self.options.stable_ms) - now_ms);
            }
        }
        earliest
    }

    /// 推进状态机。返回 `true` = 状态变了（宿主据此刷新界面 / 换客户端）。
    ///
    /// 干这几件事：
    /// 0. 正在起（`Starting`）→ 探一次活（见 `probe_pending`）；
    /// 1. 子进程死了（`try_exit_code` 有结果）→ 不是我们主动停的就自动重启（退避 + 次数上限）；
    /// 2. 到点的退避重启 → 真的重启；
    /// 3. 跑够 `stable_ms` → 连续重启次数清零。
    pub fn poll(&mut self, now_ms: i64) -> bool {
        let before = self.status.clone();
        // 0. 正在起：探一次
        if self.pending.is_some() {
            self.probe_pending(now_ms);
        }
        // 1. 子进程还活着吗
        if let Some(code) = self.child.as_ref().and_then(RunningChild::try_exit_code) {
            self.child = None;
            self.healthy_at_ms = None;
            if self.desired_running && matches!(self.status, CoreStatus::Running { .. }) {
                self.schedule_restart(&format!("核心进程退出了（exit={code}）"), now_ms);
            }
        }
        // 3. 跑够了就把连续重启次数清零
        if let Some(healthy_at) = self.healthy_at_ms {
            if matches!(self.status, CoreStatus::Running { .. })
                && now_ms - healthy_at >= self.options.stable_ms
            {
                if self.attempts != 0 {
                    self.attempts = 0;
                }
                // 只清一次：把计时往前推，免得每次 poll 都算（无害，但没必要）
                self.healthy_at_ms = Some(now_ms);
            }
        }
        // 2. 到点的退避重启
        if let Some(at) = self.restart_at_ms {
            if now_ms >= at {
                self.restart_at_ms = None;
                if self.desired_running {
                    let _ = self.restart_reason.take();
                    self.start_process(now_ms);
                }
            }
        }
        self.status != before
    }

    // -----------------------------------------------------------------------
    // 延迟测试
    // -----------------------------------------------------------------------

    /// 测一个节点的延迟：**另起一个一次性 sing-box 进程**（只带这个节点，`tools fetch` 直接走它）去取
    /// `state.test_url`，量的是整个进程的墙钟时间。结果写回节点并落盘。
    ///
    /// 好处是不用把每个节点都挂到正在跑的核心里，坏处是每次都多算一次进程启动。**和 Kotlin 版同一条命令、同一个口径。**
    pub fn test_latency(&mut self, node_id: &str, now_ms: i64) -> LatencyResult {
        let Some(node) = self.state.nodes.iter().find(|n| n.id == node_id).cloned() else {
            return LatencyResult {
                node_id: node_id.to_string(),
                ms: None,
                error: Some("节点不在了".to_string()),
            };
        };
        let probe_file = self.probe_path(&node);
        let result = run_probe(
            &self.options.binary,
            &probe_file,
            &node,
            &self.effective_test_url(),
            self.options.latency_timeout_ms,
        );
        self.record_latency(&result.node_id, result.ms, now_ms);
        result
    }

    /// 全部测一遍，**最多 `concurrency` 个同时测**（默认 4，和 Kotlin 的 `Semaphore(4)` 一样）。
    /// 返回测了几个。
    ///
    /// 而"一次测几个"就是排队策略。
    /// 每个探针是一个独立的一次性 sing-box 进程，互相不共享状态，所以开几个线程跑就行——
    /// **不是 tokio**，就是 `std::thread`，宿主调这个函数时同步等它全部跑完。
    pub fn test_all_latencies(&mut self, concurrency: i32, now_ms: i64) -> i32 {
        let nodes: Vec<ProxyNode> = self.state.nodes.clone();
        if nodes.is_empty() {
            return 0;
        }
        let binary = self.options.binary.clone();
        let dir = self.options.data_dir.clone();
        let url = self.effective_test_url();
        let timeout = self.options.latency_timeout_ms;
        let results: Vec<LatencyResult> = run_probes(
            &binary,
            &nodes,
            move |node| probe_path_in(&dir, node),
            &url,
            timeout,
            concurrency.max(1) as usize,
        );
        for result in &results {
            self.record_latency(&result.node_id, result.ms, now_ms);
        }
        results.len() as i32
    }

    fn effective_test_url(&self) -> String {
        if crate::hosted::is_kotlin_blank(&self.state.test_url) {
            crate::proxy_core::model::DEFAULT_TEST_URL.to_string()
        } else {
            self.state.test_url.clone()
        }
    }

    fn probe_path(&self, node: &ProxyNode) -> PathBuf {
        probe_path_in(&self.options.data_dir, node)
    }

    fn record_latency(&mut self, node_id: &str, ms: Option<i32>, now_ms: i64) {
        let mut next = self.state.clone();
        for node in next.nodes.iter_mut() {
            if node.id == node_id {
                node.last_latency_ms = ms;
                node.last_latency_at = Some(now_ms);
            }
        }
        self.set_state(next);
    }

    // -----------------------------------------------------------------------
    // 日志
    // -----------------------------------------------------------------------

    fn log(&mut self, line: &str) {
        self.logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line.to_string());
    }

    /// 让宿主（或者测试）直接塞一行日志进来（子进程的 stdout 由宿主线程读，见 FFI 层的说明）。
    pub fn push_log(&mut self, line: &str) {
        self.log(line);
    }

    /// 本地时钟基准：`now_ms` 由宿主注入，这里只是给 `poll()` 一个默认值（测试里不会用到）。
    pub fn clock_ms(&mut self) -> i64 {
        let base = *self
            .clock_base
            .get_or_insert_with(Instant::now);
        base.elapsed().as_millis() as i64
    }
}

impl Drop for CoreSupervisor {
    fn drop(&mut self) {
        self.desired_running = false;
        self.stop_process();
    }
}

/// 探针配置放在哪儿：`<data_dir>/probe-<节点 id 前 40 个字符>.json`。
///
/// 节点 id 是按出站内容算的哈希，所以同一个节点永远同一个文件名——两次测速覆盖同一份文件，
/// 不会越攒越多（这点和 Kotlin 版一样）。
pub fn probe_path_in(data_dir: &Path, node: &ProxyNode) -> PathBuf {
    let name: String = node.id.chars().take(40).collect();
    data_dir.join(format!("probe-{name}.json"))
}

/// 跑一个"只测这一个节点"的一次性 sing-box 探针：写探针配置 → `tools fetch` → 量墙钟时间。
///
/// **不碰任何共享状态**（探针文件的名字按节点 id 定，节点 id 又按出站内容算，所以同一个节点
/// 跑两次也只会覆盖同一份文件），因此可以并发跑（[`run_probes`]）。
pub fn run_probe(
    binary: &Path,
    probe_file: &Path,
    node: &ProxyNode,
    url: &str,
    timeout_ms: u64,
) -> LatencyResult {
    let probe = config::build_latency_probe_config(node);
    if let Err(e) = crate::proxy_core::model::write_private(probe_file, &config::to_config_text(&probe)) {
        return LatencyResult {
            node_id: node.id.clone(),
            ms: None,
            error: Some(format!("写不了探针配置（{}）", io_kind_name(&e))),
        };
    }
    let started = Instant::now();
    let mut child = match Command::new(binary)
        .arg("--disable-color")
        .arg("-c")
        .arg(probe_file)
        .arg("tools")
        .arg("fetch")
        .arg("-o")
        .arg(&node.id)
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return LatencyResult {
                node_id: node.id.clone(),
                ms: None,
                error: Some(format!("起不了 sing-box（{}）", io_kind_name(&e))),
            }
        }
    };
    // 读输出（失败时第一行拿来当原因），同时等进程
    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut readers = Vec::new();
    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let output = output.clone();
        readers.push(std::thread::spawn(move || {
            use std::io::Read;
            let mut reader = std::io::BufReader::new(stream);
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            if let Ok(mut slot) = output.lock() {
                slot.push_str(&text);
            }
        }));
    }
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let deadline = Instant::now() + timeout;
    let mut exit: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit = Some(status.code().unwrap_or(-1));
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for reader in readers {
        let _ = reader.join();
    }
    let ms = started.elapsed().as_millis() as i32;
    match exit {
        Some(0) => LatencyResult {
            node_id: node.id.clone(),
            ms: Some(ms),
            error: None,
        },
        Some(code) => {
            let text = output.lock().map(|s| s.clone()).unwrap_or_default();
            let line = text
                .lines()
                .find(|l| !crate::hosted::is_kotlin_blank(l))
                .map(clean_log_line)
                .unwrap_or_default();
            let error = if line.is_empty() {
                format!("失败（exit={code}）")
            } else {
                line.chars().take(160).collect()
            };
            LatencyResult {
                node_id: node.id.clone(),
                ms: None,
                error: Some(error),
            }
        }
        None => LatencyResult {
            node_id: node.id.clone(),
            ms: None,
            error: Some(format!("超时（>{timeout_ms}ms）")),
        },
    }
}

/// 一批探针，**最多 `concurrency` 个同时跑**（和 Kotlin 的 `Semaphore(concurrency)` 一样）。
///
/// 用 `std::thread` 而不是线程池：一批最多几十个节点、每个几十毫秒，用完就没了。
pub fn run_probes(
    binary: &Path,
    nodes: &[ProxyNode],
    probe_path: impl Fn(&ProxyNode) -> PathBuf + Send + Sync + 'static,
    url: &str,
    timeout_ms: u64,
    concurrency: usize,
) -> Vec<LatencyResult> {
    let concurrency = concurrency.max(1).min(nodes.len().max(1));
    let probe_path = std::sync::Arc::new(probe_path);
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        nodes.iter().cloned().enumerate().collect::<Vec<_>>(),
    ));
    let results = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<(usize, LatencyResult)>::new(),
    ));
    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let queue = queue.clone();
        let results = results.clone();
        let binary = binary.to_path_buf();
        let url = url.to_string();
        let probe_path = probe_path.clone();
        workers.push(std::thread::spawn(move || loop {
            let next = {
                let mut q = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                q.pop()
            };
            let Some((index, node)) = next else { return };
            let probe_file = probe_path(&node);
            let result = run_probe(&binary, &probe_file, &node, &url, timeout_ms);
            results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((index, result));
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    let mut collected = results
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default();
    collected.sort_by_key(|(i, _)| *i);
    collected.into_iter().map(|(_, r)| r).collect()
}

/// 找一个本机空闲端口（只监听回环）。
///
/// 绑到 `127.0.0.1:0` 让内核挑一个，拿到端口号之后**立刻关掉**——之后 sing-box 去 bind 同一个端口，
/// 中间的窗口在整个流程里只有几毫秒（Kotlin 版是同一个做法）。
pub fn free_loopback_port() -> Option<i32> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::from(CORE_LOOPBACK), 0)).ok()?;
    let port = listener.local_addr().ok()?.port() as i32;
    drop(listener);
    Some(port)
}

/// `std::io::Error` → Kotlin 那边的异常类名（**不带 message**：里面可能有路径）。
fn io_kind_name(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundException",
        std::io::ErrorKind::PermissionDenied => "AccessDeniedException",
        std::io::ErrorKind::TimedOut => "SocketTimeoutException",
        std::io::ErrorKind::ConnectionRefused => "ConnectException",
        _ => "IOException",
    }
}

/// 宿主定时器要的"多久 poll 一次"（毫秒）。**只是给宿主的建议值**，Rust 不起定时器。
pub const POLL_INTERVAL_MS: i64 = 250;

/// 一个原子的"该不该跑"标记：宿主在多个线程上问（界面线程 / 请求线程），不为此加锁。
pub struct ShouldRunFlag(AtomicBool);

impl ShouldRunFlag {
    pub fn new(value: bool) -> ShouldRunFlag {
        ShouldRunFlag(AtomicBool::new(value))
    }

    pub fn set(&self, value: bool) {
        self.0.store(value, Ordering::SeqCst);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

