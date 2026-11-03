//! `llama-server` 的启停 + 一次 chat 请求（`app/.../translate/LlamaTranslationEngine.kt` 的 Rust 版）。
//!
//! 生命周期（和 Kotlin 原版逐条一致）：
//! - 第一次 [`Engine::translate`] 才启动，状态从 [`Status::Stopped`] 到 [`Status::Starting`] 再到
//!   [`Status::Ready`]（状态由 [`StatusHost`] 推给宿主，界面上要看得见"正在加载模型"）；
//! - **启动前先按名字选设备**（[`crate::translate::server::list_devices`] / `choose_device`）：
//!   只加载 RADV 驱动、只认名字里带 `RADV` / `Radeon` 的设备，找不到就报错不启动，绝不退回独显；
//! - **并发**由调用方控制（`-np` = [`PARALLELISM`]；排队在 [`crate::translate::queue`]）；
//! - 空闲 5 分钟或累计 150 次请求后停掉（[`crate::translate::server::Policy`]），**只在没有请求在跑时停**，
//!   重启不会打断正在跑的请求（下次再起——`llama-server` 的内存会随请求数增长）；
//! - App 退出时 [`Engine::close`] 收进程。
//!
//! HTTP **不在这一层发**：到 `127.0.0.1` 的 `/health` 和 `/v1/chat/completions` 都走
//! [`crate::host::HttpHost`]（宿主那边是 `Proxy.NO_PROXY` 的专用 OkHttp 客户端）。
//! 所以 `.so` 里没有 tokio / reqwest / rustls，闲置时这条路上一个线程都没有。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::host::{HostError, HttpHost, HttpRequest, HttpResponse, StoreHost};
use crate::translate::device::{
    self, parse_backend_devices, usable_tiers, Memory, Platform, Tier,
};
use crate::translate::server::{
    choose_device, free_port, list_devices, list_devices_raw, log_path, mobile_env, Device, Policy,
    Process, SpawnSpec, RADV_ICD_FILE,
};
use crate::translate::{lang, Status, StatusHost};

/// 同时能处理几个请求（= `llama-server -np`）。
///
/// **手机上固定 1**：NPU 的 HTP session 是独占的，两个槽会把 VTCM / 权重窗口切开，
/// 单条延迟翻倍而吞吐上不去（这条要靠 `tools/translate/phone_bench.sh` 在真机上复量）。
pub const PARALLELISM: u32 = 2;

/// 手机上同时几个请求。
///
/// **固定 1**：NPU 的 HTP session 是独占的，两个槽会把 VTCM / 权重窗口切开，单条延迟翻倍而吞吐
/// 上不去；CPU 档也只有 4 个大核、模型还要占 1 GB 内存。桌面那个 2 是拿 AMD 核显 的显存换吞吐量出来的
/// （见 [`PARALLELISM`]），手机上不成立。
pub const PARALLELISM_MOBILE: u32 = 1;

/// 一个槽的上下文（原来 `-np 1` 时的 `-c`），总上下文 = 这个 × 槽数。
pub const CONTEXT_PER_SLOT: u32 = 4096;

/// 空闲这么久就停掉 server（下次再起）。桌面 5 分钟。
pub const IDLE_MS: i64 = 5 * 60 * 1000;

/// **安卓上的空闲回收时间：2 分钟**。
///
/// 为什么比桌面短：手机上一个 1.8B 的 Q4 模型在 CPU 上常驻就是 1.1 GB 内存 + 4 个核在跑 warm 轮，
/// 用户翻完一条内容很快就不看了；2 分钟足够覆盖"连着翻几条"，又不至于让模型白占着内存和电。
/// 退到后台是**立刻**停（不走这个计时器，见 `AndroidTranslationEngine.onForeground`）。
pub const IDLE_MS_ANDROID: i64 = 2 * 60 * 1000;

/// 累计这么多次请求后重启。
pub const MAX_REQUESTS: u32 = 150;

/// **安卓上的重启阈值：30 次**（桌面 150 次）。
///
/// 手机上 llama.cpp 的 KV cache / 计算图缓存在 CPU 后端上增长得更快（没有显存可以重分配），
/// 而且手机本来就要频繁停启；30 次大约是"翻十几条长正文"，重启成本（模型加载 3～8 秒）
/// 分摊到每条上还算划算。**没有真机实测数据**，第一版按这个数字，量出来再改。
pub const MAX_REQUESTS_ANDROID: u32 = 30;

/// `llama-server` 起来最多等这么久（模型加载）。
pub const START_TIMEOUT_MS: i64 = 120_000;

/// 就绪轮询的间隔。
pub const READY_POLL_MS: u64 = 300;

/// 一次翻译要用到的本机设置（宿主每次现传；Rust 不存设置）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// 翻译总开关。关着时 [`Engine::translate`] 直接报"翻译已关闭"，不会起进程。
    pub enabled: bool,
    /// GGUF 模型路径。
    pub model_path: String,
    /// `llama-server` 可执行文件路径。
    pub server_path: String,
    /// 缓存目录（日志落在 `<cache_dir>/llama-server.log`）。
    pub cache_dir: Option<String>,
    /// RADV 的 ICD 清单（默认 [`RADV_ICD_FILE`]；测试用假的）。
    ///
    /// **桌面上必须存在**（只用 AMD 核显）。手机上不看它——安卓没有 Vulkan 后端，
    /// 设备靠 `--device HTP0` / `GPUOpenCL`。
    pub icd_file: Option<String>,
    /// 当前 App 版本（`ctx.store` 里那条"降到哪一档"的记录用它判失效：升级后重新从 NPU 试）。
    pub app_version: String,
    /// 宿主是不是跑在**手机**上（安卓 = `true`）。
    ///
    /// 这是宿主的事实，不让 Rust 从设备列表猜：x86_64 模拟器上一个非 CPU 设备都没有，
    /// 但它仍然该走"候选档位 + CPU 兜底"，而不是桌面的"没有 AMD 核显 就不启动"。
    pub mobile: bool,
    /// **强制只用这一档**（`null` = 自动降级，正常状态）。
    ///
    /// 只给排错和 `tools/translate/phone_bench.sh` 用（四档要逐档量）。认的口径见
    /// [`Tier::from_override`]：`"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"`，大小写不敏感；
    /// 认不出来 = 自动（不报错，免得一个手改坏的存档让翻译整个不可用）。
    /// **老值 `"CPU"` 不认**。
    ///
    /// 强制的那一档起不来就是**真失败**（不降级）——不然"我强制了 CPU"变成一句谎话。
    pub forced_backend: Option<String>,
}

/// 并发槽数 + 空闲回收 + 重启阈值这一组时间 / 数量常量。
///
/// **为什么单独抽出来**：桌面和手机的取舍完全不同（显存 vs 内存 / 常插电 vs 电池），
/// 但规则本身（"空闲超过 N 毫秒就停"、"累计 M 次就重启"）是一条。抽成一个值，
/// 测试可以拿任意组合去测，不用改常量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    /// `llama-server -np`。
    pub parallelism: u32,
    /// 空闲这么久就停（毫秒）。
    pub idle_ms: i64,
    /// 累计这么多次请求后重启。
    pub max_requests: u32,
    /// `-t` 线程数；`None` = 不传（让 llama.cpp 自己按核数定）。
    ///
    /// **这一项只对桌面那条有影响**：手机上 `-t` 是 [`crate::translate::device::launch_for`] 按档位
    /// 算出来的（前三档固定 4、CPU 档按核数的一半），拼在 [`SpawnSpec::extra_args`] 里，
    /// 不从这里走（否则 `-t` 会出现两次）。
    pub threads: Option<u32>,
}

impl TimeoutPolicy {
    /// 桌面。
    pub const fn desktop() -> TimeoutPolicy {
        TimeoutPolicy {
            parallelism: PARALLELISM,
            idle_ms: IDLE_MS,
            max_requests: MAX_REQUESTS,
            threads: None,
        }
    }

    /// 安卓：并发 1、空闲 2 分钟、30 次重启（线程数由 device 层给，这里不填）。
    pub const fn android() -> TimeoutPolicy {
        TimeoutPolicy {
            parallelism: PARALLELISM_MOBILE,
            idle_ms: IDLE_MS_ANDROID,
            max_requests: MAX_REQUESTS_ANDROID,
            threads: None,
        }
    }
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        TimeoutPolicy::desktop()
    }
}

impl Config {
    fn icd(&self) -> PathBuf {
        PathBuf::from(self.icd_file.clone().unwrap_or_else(|| RADV_ICD_FILE.to_string()))
    }

    fn cache(&self) -> PathBuf {
        self.cache_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    /// OpenCL kernel 的缓存目录（手机 GPU 档用）：`<缓存目录>/cl-cache`。
    fn opencl_cache(&self) -> PathBuf {
        self.cache().join(device::OPENCL_CACHE_DIR)
    }
}

struct Lifecycle {
    process: Option<Process>,
    port: u16,
    policy: Policy,
    /// 这个进程是用哪一档起来的（手机上降级要用；桌面上永远是 [`Tier::Cpu`] 这个占位，
    /// 桌面只有 AMD 核显 一档，不看它）。
    tier: Tier,
    /// 这个引擎这批走哪条路（都是 [`Platform::from_host(cfg.mobile)`] 算出来的）。
    /// 进程还活着时它不变；`parallelism` 也读它（手机 1 / 桌面 2）。
    platform: Platform,
}

impl Lifecycle {
    fn new(policy: TimeoutPolicy) -> Lifecycle {
        Lifecycle {
            process: None,
            port: 0,
            policy: Policy::new(policy.max_requests, policy.idle_ms),
            tier: Tier::Cpu,
            platform: Platform::DesktopVulkan,
        }
    }
}

/// 翻译引擎（一个 App 只有一份；`llama-server` 的启停全在这里、互斥）。
pub struct Engine {
    http: Arc<dyn HttpHost>,
    status: Arc<dyn StatusHost>,
    /// `ctx.store`：记"手机上降到哪一档了"（[`KEY_BACKEND`]）。`None` = 不记（测试 / 只读场景）。
    store: Option<Arc<dyn StoreHost>>,
    life: Mutex<Lifecycle>,
    /// 正在跑的请求数（空闲回收 / 重启要等它归零，不能打断）。
    in_flight: AtomicUsize,
    closed: AtomicBool,
    current: Mutex<Status>,
    /// 当前**实际**在用的后端（手机上降级之后会变；桌面上是 `None` = 只有 AMD 核显）。
    backend: Mutex<Option<Tier>>,
    /// 这套引擎的时间 / 数量参数（构造时定；换它要新建引擎）。
    tuning: TimeoutPolicy,
}

impl Engine {
    /// 桌面口径（并发 2 / 空闲 5 分钟 / 150 次重启 / 线程交给 llama.cpp）。
    pub fn new(http: Arc<dyn HttpHost>, status: Arc<dyn StatusHost>) -> Engine {
        Engine::with_policy(http, status, TimeoutPolicy::desktop())
    }

    /// 指定时间 / 数量参数（安卓用 [`TimeoutPolicy::android`]）。
    pub fn with_policy(
        http: Arc<dyn HttpHost>,
        status: Arc<dyn StatusHost>,
        policy: TimeoutPolicy,
    ) -> Engine {
        Engine {
            http,
            status,
            store: None,
            life: Mutex::new(Lifecycle::new(policy)),
            in_flight: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            current: Mutex::new(Status::Stopped),
            backend: Mutex::new(None),
            tuning: policy,
        }
    }

    /// 带 `ctx.store` 的构造（手机：记降级档位；桌面也可以传，记了也不看）。
    ///
    /// 时间常量用**桌面**那套；安卓要手机那套就传
    /// [`with_policy_and_store`](Self::with_policy_and_store)。
    pub fn with_store(
        http: Arc<dyn HttpHost>,
        status: Arc<dyn StatusHost>,
        store: Arc<dyn StoreHost>,
    ) -> Engine {
        Engine::with_policy_and_store(http, status, TimeoutPolicy::desktop(), store)
    }

    /// 时间常量 + `ctx.store` 都要的那条（**安卓生产路径**：手机的时间常量 + 降级记忆）。
    pub fn with_policy_and_store(
        http: Arc<dyn HttpHost>,
        status: Arc<dyn StatusHost>,
        policy: TimeoutPolicy,
        store: Arc<dyn StoreHost>,
    ) -> Engine {
        let mut e = Engine::with_policy(http, status, policy);
        e.store = Some(store);
        e
    }

    /// 当前实际在用的后端（设置页那一行；`None` = 还没起过 / 桌面）。
    pub fn backend(&self) -> Option<Tier> {
        *self.backend.lock().unwrap()
    }

    /// 当前状态（宿主读它显示设置页那一行）。
    pub fn status(&self) -> Status {
        *self.current.lock().unwrap()
    }

    /// 同时能处理几个请求（宿主用它给排队器设并发上限）。
    ///
    /// 桌面 2（[`PARALLELISM`]）/ 手机 1（[`PARALLELISM_MOBILE`]）。
    ///
    /// **手机那个 1 优先于 [`TimeoutPolicy::parallelism`]**：一旦当前判定成手机形态
    /// （[`Platform::from_host(cfg.mobile)`]），槽数就是 1——HTP 的 session 是独占的，
    /// 两个槽会把 VTCM / 权重窗口切开。宿主传了桌面那套 timeouts 也不会把 2 个槽带到手机上。
    pub fn parallelism(&self) -> u32 {
        if self.life.lock().unwrap().platform == Platform::Mobile {
            PARALLELISM_MOBILE
        } else {
            self.tuning.parallelism
        }
    }

    /// 有没有正在跑的 `llama-server`（界面 / 排障用；不改变任何状态）。
    pub fn running(&self) -> bool {
        let mut life = self.life.lock().unwrap();
        match life.process.as_mut() {
            Some(p) => p.is_alive(),
            None => false,
        }
    }

    /// server 现在监听哪个端口（没在跑是 0）。
    pub fn port(&self) -> u16 {
        self.life.lock().unwrap().port
    }

    fn set_status(&self, status: Status) {
        *self.current.lock().unwrap() = status;
        self.status.status(status);
    }

    /// 一次翻译：拿到能用的 server 端口 → 发一次 chat 请求 → 记一笔请求数。
    pub fn translate(&self, cfg: &Config, text: &str, target: &str, now_ms: i64) -> Result<String, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("翻译已关闭".to_string());
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let result = (|| {
            let port = self.server_port(cfg, now_ms)?;
            let out = self.chat(port, text, target)?;
            let mut life = self.life.lock().unwrap();
            life.policy.note_request(now_ms);
            Ok(out)
        })();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if result.is_err() {
            // 请求层的失败不改状态（只有启停失败才 FAILED）——和 Kotlin 一样：
            // `fail()` 只在 startServer 里调，一次 HTTP 500 不该把设置页写成"不可用"。
        }
        result
    }

    /// 对**已经在跑**的 server 发一次 chat 请求（HTTP 那一半单独可用：测试 / 排障用）。
    pub fn chat(&self, port: u16, text: &str, target: &str) -> Result<String, String> {
        let body = chat_payload(text, target);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            headers: vec![(
                "Content-Type".to_string(),
                "application/json; charset=utf-8".to_string(),
            )],
            body: body.into_bytes(),
        };
        let response = self.send(request)?;
        if !(200..300).contains(&response.status) {
            let text = String::from_utf8_lossy(&response.body).to_string();
            let head: String = text.chars().take(300).collect();
            return Err(format!("翻译服务 HTTP {}：{}", response.status, head));
        }
        let text = String::from_utf8_lossy(&response.body).to_string();
        parse_chat_response(&text)
    }

    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        self.http.execute(request).map_err(|e| match e {
            HostError::Network { detail } => detail,
            HostError::ProxyUnavailable { detail } => {
                // 本机回环不该出现这一档（宿主的翻译客户端是 Proxy.NO_PROXY）；真出现了也照实说
                format!("本地翻译的请求被代理拦住了：{detail}")
            }
            HostError::Other { detail } => detail,
        })
    }

    /// 拿到能用的 server 端口；需要时启动 / 重启。**只有这里动 server**（[`Engine::life`] 互斥）。
    ///
    /// 累计请求到上限时先停再起，但只在没有别的请求在跑时（`in_flight <= 1` = 只有自己）。
    fn server_port(&self, cfg: &Config, now_ms: i64) -> Result<u16, String> {
        let mut life = self.life.lock().unwrap();
        if !cfg.enabled {
            return Err("翻译已关闭".to_string());
        }
        let alive = match life.process.as_mut() {
            Some(p) => p.is_alive(),
            None => false,
        };
        let config_changed = match life.process.as_ref() {
            Some(p) if alive => p.model != cfg.model_path || p.server != cfg.server_path,
            _ => false,
        };
        // 手机上"该用哪一档"变了也要重启：上次记的档位和现在这个进程的档位不一样，
        // 说明中间发生过降级、或者 App 升级后 store 被清掉了。桌面永远只有一档，不触发。
        let tier_changed = life.platform == Platform::Mobile
            && alive
            && self
                .store
                .as_deref()
                .map(|store| Memory::load(store, &cfg.app_version).tier)
                .is_some_and(|want| want != life.tier);
        if alive
            && (config_changed
                || tier_changed
                || (life.policy.should_restart() && self.in_flight.load(Ordering::SeqCst) <= 1))
        {
            self.stop_locked(&mut life, false);
        }
        let alive_now = match life.process.as_mut() {
            Some(p) => p.is_alive(),
            None => false,
        };
        if !alive_now {
            if let Err(e) = self.start_locked(&mut life, cfg) {
                // 启停失败一律 FAILED（Kotlin 的 `fail()` 就是这个语义）；请求层的失败不改状态。
                self.set_status(Status::Failed);
                return Err(e);
            }
            life.policy.mark_started(now_ms);
            self.set_status(Status::Ready);
        }
        Ok(life.port)
    }

    fn start_locked(&self, life: &mut Lifecycle, cfg: &Config) -> Result<(), String> {
        let server = PathBuf::from(&cfg.server_path);
        let model = PathBuf::from(&cfg.model_path);
        if !server.is_file() {
            return Err(format!("找不到 llama-server：{}", server.display()));
        }
        if !model.is_file() {
            return Err(format!("找不到翻译模型：{}", model.display()));
        }
        // 先看一眼这台机器上有什么后端（手机）或者是不是还是 AMD 核显（桌面）。
        // 手机会把 `--list-devices` 的完整输出拿去过一遍；桌面走老的 RADV 红线。
        let raw = list_devices_raw(&server, &mobile_env(&server))
            .ok_or_else(|| format!("跑不了 llama-server --list-devices：{}", server.display()))?;
        let devices = parse_backend_devices(&raw);
        let platform = device::Platform::from_host(cfg.mobile);
        life.platform = platform;
        match platform {
            Platform::DesktopVulkan => self.start_desktop(life, cfg, &server, &model),
            Platform::Mobile => self.start_mobile(life, cfg, &server, &model, &devices),
        }
    }

    /// 桌面：按名字挑 AMD 核显（[`choose_device`]），挑不到**不启动**（不碰独显）。
    fn start_desktop(
        &self,
        life: &mut Lifecycle,
        cfg: &Config,
        server: &Path,
        model: &Path,
    ) -> Result<(), String> {
        let icd = cfg.icd();
        if !icd.is_file() {
            return Err(format!(
                "找不到 RADV 的 Vulkan ICD：{}（翻译只用 AMD 核显）",
                icd.display()
            ));
        }
        let devices = list_devices(server, &icd)
            .ok_or_else(|| format!("跑不了 llama-server --list-devices：{}", server.display()))?;
        let device: Device = choose_device(&devices)
            .cloned()
            .ok_or_else(|| {
                let listing = if devices.is_empty() {
                    "（一台设备都没有）".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| format!("{}={}", d.argument(), d.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("翻译只用 AMD 核显（RADV），但 --list-devices 里没有它：{listing}")
            })?;
        let spec = SpawnSpec::desktop(&device, server, &icd);
        self.spawn_and_wait(life, cfg, server, model, &spec, Tier::Cpu)?;
        *self.backend.lock().unwrap() = None;
        Ok(())
    }

    /// 手机：从 store 里记的那一档开始，NPU → GPU → NPU+GPU → CPU 一路降级。
    ///
    /// 每一档都要过三道判据才算成功：**进程起来了 + `/health` 通了 + 真翻一句话能得到像样的译文**
    /// （[`device::looks_broken`]）。只看 `/health` 不够——HTP 内核出问题时 server 是健康的，
    /// 但吐出来的是空串或一把 `\u{FFFD}`。失败就把"降到哪一档"写进 `ctx.store`，下次直接从那儿开始；
    /// 升到 CPU 还不行就是真失败（状态 FAILED，界面显示原因）。
    fn start_mobile(
        &self,
        life: &mut Lifecycle,
        cfg: &Config,
        server: &Path,
        model: &Path,
        devices: &[device::BackendDevice],
    ) -> Result<(), String> {
        let cache = cfg.opencl_cache();
        let _ = std::fs::create_dir_all(&cache);
        let app = cfg.app_version.clone();
        let mut memory = match self.store.as_deref() {
            Some(store) => Memory::load(store, &app),
            None => Memory::fresh(),
        };
        let mut tiers = usable_tiers(devices);
        // 强制档位（排错 / 测速）：**只留那一档**，不降级。
        //
        // 强制的档位不在 `usable_tiers` 里（比如强制 NPU 但这台机器 `--list-devices` 里没有 HTP0）
        // 就是真失败——`launch_for` 会返回 `None`，下面的循环一档都跑不到，最后那条文案说清楚。
        if let Some(forced) = Tier::from_override(cfg.forced_backend.as_deref()) {
            tiers.retain(|t| *t == forced);
            if tiers.is_empty() {
                return Err(format!(
                    "强制用{}，但这台机器的 --list-devices 里没有它（设备：{}）",
                    forced.label(),
                    if devices.is_empty() {
                        "一台都没有".to_string()
                    } else {
                        devices
                            .iter()
                            .map(|d| d.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
            }
        }
        // 记的那一档不在这次能用的列表里（比如上次降到 GPU、这次 OpenCL 没加载上）→ 从头开始。
        let start = tiers.iter().position(|t| *t == memory.tier).unwrap_or(0);
        let mut last_err = String::new();
        for tier in &tiers[start..] {
            let Some(launch) =
                device::launch_for(*tier, devices, Some(&cache), PARALLELISM_MOBILE)
            else {
                continue;
            };
            let mut spec = SpawnSpec {
                extra_args: launch.args,
                env: mobile_env(server),
                env_remove: vec!["GGML_VK_VISIBLE_DEVICES".to_string()],
                device: tier.name().to_string(),
            };
            spec.env.extend(launch.env);
            // 用到 OpenCL 的档位：把厂商的 libOpenCL.so 指给 ICD loader（安卓上没有 .icd 清单目录）。
            // 有 GPU 档才需要；`--list-devices` 那次探测**不带**这个（那时还不知道有没有）。
            if matches!(tier, Tier::Gpu | Tier::NpuGpu) {
                if let Some(env) = device::opencl_vendor_env(device::OPENCL_VENDOR_CANDIDATES.iter().copied()) {
                    spec.env.push(env);
                }
            }
            match self.spawn_and_wait(life, cfg, server, model, &spec, *tier) {
                Ok(()) => {
                    // 真翻一句看看（走的是生产路径的同一个请求），坏输出算这一档失败
                    match self.smoke_test(life.port) {
                        Ok(()) => {
                            *self.backend.lock().unwrap() = Some(*tier);
                            if memory.tier != *tier {
                                memory.tier = *tier;
                                memory.save_if(self.store.as_deref());
                            }
                            return Ok(());
                        }
                        Err(e) => {
                            last_err = format!("{}：{e}", tier.label());
                            self.stop_locked(life, true);
                        }
                    }
                }
                Err(e) => {
                    last_err = format!("{}：{e}", tier.label());
                }
            }
            // 强制的档位失败 = 真失败，**不写降级记忆**（不然"强制 CPU 试了一次没起来"会
            // 把正常路径的起点也改掉，用户下次不强制了会从错误的档位开始）
            if Tier::from_override(cfg.forced_backend.as_deref()).is_some() {
                break;
            }
            // 这一档不行：降一档、记进 store，接着试
            memory = memory.degrade(*tier, &last_err, &app);
            memory.save_if(self.store.as_deref());
        }
        Err(if last_err.is_empty() {
            "这台机器上没有能用的翻译后端（NPU / GPU / CPU 都起不来）".to_string()
        } else {
            format!("翻译后端全部不可用，最后一档：{last_err}")
        })
    }

    /// 起进程 + 等 `/health`。`tier` 只用来记在 [`Lifecycle`] 上（`process` 本身不看它）。
    fn spawn_and_wait(
        &self,
        life: &mut Lifecycle,
        cfg: &Config,
        server: &Path,
        model: &Path,
        spec: &SpawnSpec,
        tier: Tier,
    ) -> Result<(), String> {
        self.stop_locked(life, false);
        self.set_status(Status::Starting);
        let port = free_port().ok_or_else(|| "找不到空闲端口".to_string())?;
        // 槽数从 tuning 来（桌面 2 / 手机 1）；手机每一档的 `-t` 已经由 `launch_for` 拼进 extra_args，
        // 桌面的 `-t` 由 tuning.threads 决定（`None` = 不传，让 llama.cpp 自己按核数定）。
        let log = log_path(&cfg.cache());
        let process = Process::spawn(
            server,
            model,
            spec,
            port,
            CONTEXT_PER_SLOT,
            // **手机形态固定 1**（HTP session 独占）：宿主要是不小心传了桌面那套 timeouts，
            // 也不会在这一层泄漏成 2。**直接从已经拿着的 `life` 读，不能再调 `self.parallelism()`**
            // ——那个要再锁一次 `self.life`，std 的 Mutex 不可重入，会当场死锁。
            if life.platform == Platform::Mobile {
                PARALLELISM_MOBILE
            } else {
                self.tuning.parallelism
            },
            self.tuning.threads,
            &log,
        )?;
        life.process = Some(process);
        life.port = port;
        life.tier = tier;
        // 就绪轮询：进程提前退了 / 超时都算失败（状态置 FAILED 由调用方看到的错误决定）
        if let Err(e) = self.wait_ready(life, &log) {
            self.stop_locked(life, true);
            self.set_status(Status::Failed);
            return Err(e);
        }
        Ok(())
    }

    /// 一句话的自检（手机降级用）：**只看译文像不像话**，不看内容对不对。
    ///
    /// 用一句固定的短英文（不是用户内容，和缓存无关），目标语言固定中文——这一层只回答
    /// "这个后端能不能算出东西"。20 秒超时：HTP 第一句要编图 + 搬权重，1.8B 一般 < 5 秒，
    /// 20 秒还没回来说明这档基本不能用（真机数字见 `tools/translate/phone_bench.sh`）。
    fn smoke_test(&self, port: u16) -> Result<(), String> {
        let out = self.chat(port, "Hello, world.", "中文").map_err(|e| format!("自检失败：{e}"))?;
        if device::looks_broken(&out) {
            return Err(format!(
                "自检译文是坏的：{:?}",
                out.chars().take(40).collect::<String>()
            ));
        }
        Ok(())
    }

    fn wait_ready(&self, life: &mut Lifecycle, log: &Path) -> Result<(), String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(START_TIMEOUT_MS as u64);
        let port = life.port;
        loop {
            let Some(process) = life.process.as_mut() else {
                return Err("llama-server 没有起来".to_string());
            };
            if !process.is_alive() {
                let rc = process.exit_code();
                let rc = rc.map(|c| c.to_string()).unwrap_or_else(|| "未知".to_string());
                return Err(format!(
                    "llama-server 提前退出（rc={rc}），日志：{}",
                    log.display()
                ));
            }
            let ok = self
                .send(HttpRequest {
                    method: "GET".to_string(),
                    url: format!("http://127.0.0.1:{port}/health"),
                    headers: Vec::new(),
                    body: Vec::new(),
                })
                .map(|r| (200..300).contains(&r.status))
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "llama-server {} 秒未就绪，日志：{}",
                    START_TIMEOUT_MS / 1000,
                    log.display()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(READY_POLL_MS));
        }
    }

    fn stop_locked(&self, life: &mut Lifecycle, force: bool) {
        if let Some(mut process) = life.process.take() {
            process.stop(force);
        }
        life.port = 0;
        self.set_status(Status::Stopped);
    }

    /// 宿主每 60 秒问一次：现在该不该把 server 停掉（空闲 5 分钟 / 累计 150 次请求）。
    ///
    /// **只在没有请求在跑时停**。返回 `true` = 这次停掉了。
    pub fn reap(&self, now_ms: i64) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        if self.in_flight.load(Ordering::SeqCst) != 0 {
            return false;
        }
        let mut life = self.life.lock().unwrap();
        let Some(p) = life.process.as_mut() else {
            return false;
        };
        if !p.is_alive() {
            // 自己挂了：清掉，状态回 STOPPED（下次 translate 会重起）
            life.process = None;
            life.port = 0;
            self.set_status(Status::Stopped);
            return true;
        }
        let stop = life.policy.should_restart() || life.policy.idle_too_long(now_ms);
        if stop {
            self.stop_locked(&mut life, false);
        }
        stop
    }

    /// **只停 `llama-server`，不关引擎**：安卓退到后台时用（下次回前台要接着翻）。
    ///
    /// 和 [`Engine::close`] 的区别：`close` 之后这个引擎永远报"翻译已关闭"（App 退出用）；
    /// 这个只是把进程收掉、状态回 `STOPPED`，下一次 [`Engine::translate`] 自己再起一个。
    ///
    /// **只读不写 `llama-server` 那个进程**（不动 in_flight 计数）：正在跑的请求会看到连接断掉，
    /// 那一条按失败处理——退到后台时本来就不该有请求在跑（`AppGraph.onForeground(false)` 先停界面）。
    pub fn stop_server(&self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut life = self.life.lock().unwrap();
        self.stop_locked(&mut life, true);
    }

    /// App 退出：收进程（最多等 300 ms，然后 SIGKILL，别让窗口关掉之后进程还挂着）。
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut life = self.life.lock().unwrap();
        self.stop_locked(&mut life, true);
    }
}

/// 一次 chat 请求的 body（**逐字节照 Kotlin `LlamaTranslationEngine.request` 的写法**）。
///
/// Kotlin 用 `buildJsonObject { … }.toString()`，键顺序就是插入顺序：
/// `messages` → `temperature` → `max_tokens` → `chat_template_kwargs`。
/// 这里手拼（不走 serde 的结构体）才能保证和 Kotlin 逐字节一致：
/// `{"messages":[{"role":"user","content":"…"}],"temperature":0.0,"max_tokens":64,"chat_template_kwargs":{"enable_thinking":false}}`
pub fn chat_payload(text: &str, target: &str) -> String {
    let prompt = format!("将以下文本翻译为{target}，注意只需要输出翻译后的结果，不要额外解释：\n\n{text}");
    // Kotlin `text.length` 是 **UTF-16 码元**数（emoji 算 2），`(len*2).coerceIn(64, 512)`
    let max_tokens = (text.encode_utf16().count() as i64 * 2).clamp(64, 512);
    let mut out = String::with_capacity(prompt.len() + 128);
    out.push_str("{\"messages\":[{\"role\":\"user\",\"content\":");
    crate::hosted::push_json_string(&mut out, &prompt);
    out.push_str("}],\"temperature\":0.0,\"max_tokens\":");
    out.push_str(&max_tokens.to_string());
    out.push_str(",\"chat_template_kwargs\":{\"enable_thinking\":false}}");
    out
}

/// 解析 chat 响应（OpenAI 形状）：`choices[0].message.content`，空 / 缺字段报"模型没有返回译文"。
pub fn parse_chat_response(body: &str) -> Result<String, String> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("翻译服务的响应不是 JSON：{e}"))?;
    let content = root
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str());
    match content {
        Some(text) if !crate::hosted::is_kotlin_blank(text) => Ok(lang::trim(text).to_string()),
        _ => Err("模型没有返回译文".to_string()),
    }
}

