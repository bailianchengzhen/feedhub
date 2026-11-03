//! **翻译的 UniFFI 边界**。
//!
//! 三个对象 + 一组纯函数，对应 `feedhub_core::translate`：
//!
//! | 这里 | core | Kotlin 门面 |
//! |---|---|---|
//! | [`FfiTrStore`] | [`feedhub_core::translate::cache`] / [`lang`] | `RustTranslateStore`（缓存 + 门控 + 键） |
//! | [`FfiTrQueue`] | [`feedhub_core::translate::queue`] | `RustTranslateQueue`（`TranslationScheduler` 的状态机） |
//! | [`FfiTrEngine`] | [`feedhub_core::translate::engine`] | `RustTranslateEngine`（`LlamaTranslationEngine`） |
//! | `tr_*` 纯函数 | `text` / `server` / `engine::chat_payload` | `RustTranslate`（富文本 / 设备 / 请求体） |
//!
//! **这次唯一的新宿主能力**是 [`FfiTrStatusHost`]（Rust 把 `STARTING → READY` 推给宿主，
//! 界面上那个"正在加载模型"靠它）。**HTTP 不过宿主**：到本机 `llama-server`
//! 的那一跳由 `feedhub-net` 的一份专用客户端发（直连 + 2 秒连接超时 + 120 秒读超时，见
//! `local_translate_client`），和平台的代理 / 限速 / Cookie 一点关系都没有。
//!
//! 别在日志 / 异常里带模型路径以外的隐私：这条路上没有 Cookie，也没有 token。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use feedhub_core::detail as core_detail;
use feedhub_core::translate as core_tr;
use feedhub_core::translate::engine::Config as CoreConfig;
use feedhub_core::translate::lang::Lingua;
use feedhub_core::translate::queue::Queue;
use feedhub_net::{NetClient, NetError, NoCookies, NoLimiter, ProxyPlan};

use crate::host::{HostStore, StoreAdapter};
use crate::net::FfiNetError;
use crate::net_host::NetHttpHost;
use crate::FfiDetailBlock;

// ===========================================================================
// 状态 / 配置 / 队列的形状
// ===========================================================================

/// 服务状态（Kotlin `TranslationStatus` 四档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTrStatus {
    Stopped,
    Starting,
    Ready,
    Failed,
}

impl From<core_tr::Status> for FfiTrStatus {
    fn from(s: core_tr::Status) -> Self {
        match s {
            core_tr::Status::Stopped => FfiTrStatus::Stopped,
            core_tr::Status::Starting => FfiTrStatus::Starting,
            core_tr::Status::Ready => FfiTrStatus::Ready,
            core_tr::Status::Failed => FfiTrStatus::Failed,
        }
    }
}

/// **状态变化的出口**（宿主实现：写进 `MutableStateFlow<TranslationStatus>`）。
///
/// 为什么不让宿主自己算：`STOPPED → STARTING` 发生在"模型正在加载"那一刻，宿主不在这次调用里
/// （它在 IO 线程上等），只能由 Rust 推。调用可能在任意线程上，宿主的实现要线程安全。
#[uniffi::export(with_foreign)]
pub trait FfiTrStatusHost: Send + Sync {
    fn status(&self, status: FfiTrStatus);
}

struct StatusAdapter(Arc<dyn FfiTrStatusHost>);

impl core_tr::StatusHost for StatusAdapter {
    fn status(&self, status: core_tr::Status) {
        self.0.status(status.into());
    }
}

/// 门控的输入（本机设置，每次现传）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTrGate {
    pub enabled: bool,
    pub target: String,
    /// 「自动翻译点亮的平台」的平台 id（`X` / `YOUTUBE` …，只做"在不在里面"）。
    pub auto_platforms: Vec<String>,
}

/// 一次翻译的本机设置。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTrConfig {
    pub enabled: bool,
    pub model_path: String,
    pub server_path: String,
    /// 缓存目录（日志落在 `<cache_dir>/llama-server.log`）；`null` = 用系统临时目录。
    pub cache_dir: Option<String>,
    /// RADV 的 ICD 清单；`null` = 默认的 `/usr/share/vulkan/icd.d/radeon_icd.json`（测试用假的）。
    /// **桌面上必须有**（只用 AMD 核显）；手机上不看它（安卓没有 Vulkan 后端，
    /// 设备由 `translate::device` 按 NPU → GPU → NPU+GPU → CPU 挑）。
    pub icd_file: Option<String>,
    /// 当前 App 版本：手机上那条"降到哪一档"的记录按它判失效（升级后重新从 NPU 试）。
    /// 桌面上不参与任何判断，传空串也行。
    pub app_version: String,
    /// 宿主是不是跑在手机上（安卓 = `true`）。
    ///
    /// **这是宿主的事实，不让 Rust 从设备列表猜**：x86_64 模拟器上一个加速设备都没有，
    /// 但仍然该走"候选档位 + CPU 兜底"，而不是桌面的"没有 AMD 核显 就不启动"。
    pub mobile: bool,
    /// **强制只用这一档**（`null` = 自动降级，默认）：`"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"`，
    /// 大小写不敏感，认不出来 = 自动。只给排错和 `tools/translate/phone_bench.sh` 用。
    ///
    /// **老值 `"CPU"` 故意不认**。
    pub forced_backend: Option<String>,
}

/// 并发槽数 / 空闲回收 / 重启阈值（`feedhub_core::translate::engine::TimeoutPolicy` 的 FFI 版）。
///
/// **默认 = 桌面口径**（并发 2 / 空闲 5 分钟 / 150 次重启 / 线程交给 llama.cpp）：
/// 老代码不传这一栏，行为不变。安卓传 [`tr_timeouts_android`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiTrTimeouts {
    pub parallelism: u32,
    pub idle_ms: i64,
    pub max_requests: u32,
    /// `-t` 线程数；`null` = 不传（让 llama.cpp 自己按核数定，桌面就是这样）。
    ///
    /// **手机上这一栏没用**：手机每一档的 `-t` 是 `translate::device::launch_for` 算出来的
    /// （前三档 4、CPU 档按核数一半），拼在 `SpawnSpec::extra_args` 里。
    pub threads: Option<u32>,
}

impl From<FfiTrTimeouts> for core_tr::engine::TimeoutPolicy {
    fn from(t: FfiTrTimeouts) -> Self {
        core_tr::engine::TimeoutPolicy {
            parallelism: t.parallelism,
            idle_ms: t.idle_ms,
            max_requests: t.max_requests,
            threads: t.threads,
        }
    }
}

/// 桌面口径的时间常量。
///
/// **自由函数不是关联函数**：`#[uniffi::export]` 不能挂在 `impl` 里的关联函数上
/// （宏会生成模块级的元数据常量，关联函数的作用域里找不到）。
#[uniffi::export]
pub fn tr_timeouts_desktop() -> FfiTrTimeouts {
    let p = core_tr::engine::TimeoutPolicy::desktop();
    FfiTrTimeouts {
        parallelism: p.parallelism,
        idle_ms: p.idle_ms,
        max_requests: p.max_requests,
        threads: p.threads,
    }
}

/// 安卓口径的时间常量：并发 1 / 空闲 2 分钟 / 30 次重启。
#[uniffi::export]
pub fn tr_timeouts_android() -> FfiTrTimeouts {
    let p = core_tr::engine::TimeoutPolicy::android();
    FfiTrTimeouts {
        parallelism: p.parallelism,
        idle_ms: p.idle_ms,
        max_requests: p.max_requests,
        threads: p.threads,
    }
}

/// 排队中的一条请求。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTrJob {
    pub id: u64,
    pub key: String,
    pub text: String,
    pub target: String,
}

/// [`FfiTrQueue::submit`] 的答复。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiTrSubmit {
    /// `true` = 这次新建了一条（调用方要登记回调）；`false` = 已经在排队 / 在跑（共用）。
    pub created: bool,
    pub id: u64,
}

/// 一个 Vulkan 设备（`--list-devices` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTrDevice {
    pub index: i32,
    pub name: String,
}

impl From<feedhub_core::translate::server::Device> for FfiTrDevice {
    fn from(d: feedhub_core::translate::server::Device) -> Self {
        FfiTrDevice {
            index: d.index,
            name: d.name,
        }
    }
}

/// 手机上翻译实际用的后端。顺序就是候选顺序：NPU → GPU → NPU+GPU → CPU。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTrBackend {
    /// Hexagon HTP（NPU）。
    Npu,
    /// Adreno OpenCL（GPU）。
    Gpu,
    /// NPU + GPU 分层（`--split-mode layer`）。
    NpuGpu,
    /// 兜底。
    Cpu,
}

impl From<core_tr::device::Tier> for FfiTrBackend {
    fn from(t: core_tr::device::Tier) -> Self {
        match t {
            core_tr::device::Tier::Npu => FfiTrBackend::Npu,
            core_tr::device::Tier::Gpu => FfiTrBackend::Gpu,
            core_tr::device::Tier::NpuGpu => FfiTrBackend::NpuGpu,
            core_tr::device::Tier::Cpu => FfiTrBackend::Cpu,
        }
    }
}

impl From<FfiTrBackend> for core_tr::device::Tier {
    fn from(b: FfiTrBackend) -> Self {
        match b {
            FfiTrBackend::Npu => core_tr::device::Tier::Npu,
            FfiTrBackend::Gpu => core_tr::device::Tier::Gpu,
            FfiTrBackend::NpuGpu => core_tr::device::Tier::NpuGpu,
            FfiTrBackend::Cpu => core_tr::device::Tier::Cpu,
        }
    }
}

/// `--list-devices` 里的一行（手机上挑后端用；和 [`FfiTrDevice`] 的区别是不限 `VulkanN`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTrBackendDevice {
    /// `--device` 要的那个串（`HTP0` / `GPUOpenCL` / `Vulkan0`）。
    pub name: String,
    /// 描述（`Hexagon` / `Adreno (TM) 830`），尾巴上的内存信息已去掉。
    pub description: String,
}

impl From<core_tr::device::BackendDevice> for FfiTrBackendDevice {
    fn from(d: core_tr::device::BackendDevice) -> Self {
        FfiTrBackendDevice {
            name: d.name,
            description: d.description,
        }
    }
}

/// 翻译这条路上的错误（一条就够：文案直接给界面看，和 Kotlin 原版 `error("…")` 一样）。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiTrError {
    /// 引擎这边失败：起不来 / 提前退出 / HTTP 错 / 模型没给译文 / 翻译已关闭。
    /// **字段不能叫 `message`**（和 `Exception.message` 撞，Kotlin 编译不过），叫 `detail`。
    #[error("{detail}")]
    Failed { detail: String },
}

impl From<String> for FfiTrError {
    fn from(detail: String) -> Self {
        FfiTrError::Failed { detail }
    }
}

// ===========================================================================
// 语言识别（可注入）
// ===========================================================================

/// **语言识别器**（"这段是哪种语言"）。生产路径不传这个（[`FfiTrStore::new`] 的 `builtin` 用 Rust 版
/// lingua），它留给对拍 / 测试：要能数"识别被调了几次"、要能固定答案。
#[uniffi::export(with_foreign)]
pub trait FfiTrDetector: Send + Sync {
    /// BCP 47 标签（`zh` / `en`）；识别不出来是 `null`（上层按外语处理）。
    fn detect(&self, text: String) -> Option<String>;
}

struct DetectorAdapter(Arc<dyn FfiTrDetector>);

impl core_tr::Detector for DetectorAdapter {
    fn detect(&self, text: &str) -> Option<String> {
        self.0.detect(text.to_string())
    }
}

enum Builtin {
    None,
    Lingua(Lingua),
    Injected(DetectorAdapter),
}

impl Builtin {
    fn as_dyn(&self) -> Option<&dyn core_tr::Detector> {
        match self {
            Builtin::None => None,
            Builtin::Lingua(l) => Some(l),
            Builtin::Injected(d) => Some(d),
        }
    }
}

/// 翻译的**缓存 + 语言识别 + 门控**（一个 App 一份；`.so` 加载后按需建）。
#[derive(uniffi::Object)]
pub struct FfiTrStore {
    inner: Mutex<core_tr::cache::Cache>,
    detector: Builtin,
}

#[uniffi::export]
impl FfiTrStore {
    /// [`cache_dir`] 为 `null` = 只放内存（安卓 / 测试 / 截图）。
    ///
    /// [`detector`] 传了就用它；
    /// 否则看 [`builtin`]：`true` = 用 Rust 版 lingua（生产），`false` = 不识别
    /// （和 Kotlin 的 `identifier == null` 一样：没有平台语言就直接当外语翻）。
    #[uniffi::constructor]
    pub fn new(
        cache_dir: Option<String>,
        detector: Option<Arc<dyn FfiTrDetector>>,
        builtin: bool,
    ) -> Arc<Self> {
        let dir = cache_dir.map(std::path::PathBuf::from);
        let cache = core_tr::cache::Cache::load(dir.as_deref());
        let detector = match detector {
            Some(d) => Builtin::Injected(DetectorAdapter(d)),
            None if builtin => Builtin::Lingua(Lingua::new()),
            None => Builtin::None,
        };
        Arc::new(FfiTrStore {
            inner: Mutex::new(cache),
            detector,
        })
    }

    /// 缓存键：`SHA-256("lid2\0<模型路径>\0<目标语言>\0<正文>")` 的小写十六进制。
    pub fn key_for(&self, model_path: String, target: String, text: String) -> String {
        core_tr::cache::key_for(
            core_tr::cache::CACHE_VERSION,
            &model_path,
            &target,
            &text,
        )
    }

    /// 缓存里有没有这条（`null` = 没翻过）。
    pub fn cache_get(&self, key: String) -> Option<String> {
        self.inner.lock().unwrap().get(&key).map(|s| s.to_string())
    }

    /// 写一条缓存（同步写文件；宿主在它自己的写线程上调）。
    pub fn cache_put(&self, key: String, text: String) {
        let _ = self.inner.lock().unwrap().put(&key, &text);
    }

    /// 清掉内存和磁盘缓存。
    pub fn cache_clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// 缓存里现在有多少条（测试 / 排障）。
    pub fn cache_len(&self) -> u64 {
        self.inner.lock().unwrap().len() as u64
    }

    /// 一次语言识别（`zh` / `en` / `null`）。对拍用；生产走 [`Self::should_auto_translate`]。
    pub fn detect(&self, text: String) -> Option<String> {
        self.detector.as_dyn().and_then(|d| d.detect(&text))
    }

    /// 这条内容要不要**自动**翻译（`TranslationStore.shouldAutoTranslate` 的整体搬迁，
    /// 含"没点亮的平台在语言识别之前就返回"那条）。
    pub fn should_auto_translate(
        &self,
        text: String,
        language: Option<String>,
        platform: Option<String>,
        gate: FfiTrGate,
    ) -> bool {
        let gate = core_tr::Gate {
            enabled: gate.enabled,
            target: gate.target,
            auto_platforms: gate.auto_platforms,
        };
        core_tr::lang::should_auto_translate(
            &text,
            language.as_deref(),
            platform.as_deref(),
            &gate,
            self.detector.as_dyn(),
        )
    }

    /// 这条内容要不要出**手动**的「翻译」入口：不看平台门控、不看 25 字节门槛，
    /// 只看"这本来就不是目标语言"。
    ///
    /// 和 [`Self::should_auto_translate`] 共用同一份识别器（`builtin` 的 lingua 或注入的假识别器），
    /// 所以"两个判断对同一段文字的结论"在同一份模型上是稳定的。
    pub fn entry_visible(&self, text: String, language: Option<String>, target: String) -> bool {
        core_tr::entry::entry_visible(&text, language.as_deref(), &target, self.detector.as_dyn())
    }
}

// ===========================================================================
// 排队
// ===========================================================================

/// 翻译请求的排队器（状态机在 Rust，协程挂起在宿主）。
#[derive(uniffi::Object)]
pub struct FfiTrQueue {
    inner: Queue,
}

#[uniffi::export]
impl FfiTrQueue {
    /// [`parallelism`] = `llama-server -np`（0 按 1 算）。
    #[uniffi::constructor]
    pub fn new(parallelism: u32) -> Arc<Self> {
        Arc::new(FfiTrQueue {
            inner: Queue::new(parallelism as usize),
        })
    }

    /// 登记一条请求（同键共用）。
    pub fn submit(&self, key: String, text: String, target: String) -> FfiTrSubmit {
        let s = self.inner.submit(&key, &text, &target);
        FfiTrSubmit {
            created: s.created,
            id: s.id,
        }
    }

    /// 在并发上限内把最近的几条派发出去（宿主拿到之后自己起协程跑）。
    pub fn pump(&self) -> Vec<FfiTrJob> {
        self.inner
            .pump()
            .into_iter()
            .map(|j| FfiTrJob {
                id: j.id,
                key: j.key,
                text: j.text,
                target: j.target,
            })
            .collect()
    }

    /// 一条跑完了（成功 / 失败都算）。
    pub fn finish(&self, id: u64) {
        self.inner.finish(id);
    }

    /// 调用方走了：还没派发的拿掉（返回 `true`）；已经派发的不动。
    pub fn abandon(&self, id: u64) -> bool {
        self.inner.abandon(id)
    }

    /// 关翻译 / 退出：排队中的全部取消，返回被取消的编号。
    pub fn cancel_pending(&self) -> Vec<u64> {
        self.inner.cancel_pending()
    }

    pub fn running(&self) -> u32 {
        self.inner.running() as u32
    }

    pub fn pending(&self) -> u32 {
        self.inner.pending() as u32
    }

    pub fn parallelism(&self) -> u32 {
        self.inner.parallelism() as u32
    }
}

// ===========================================================================
// 重启策略
// ===========================================================================

/// `llama-server` 什么时候该停 / 该重启（Kotlin `LlamaServerPolicy` 的搬迁）：
/// 空闲 5 分钟、累计 150 次请求。**重启只在没有请求在跑时发生**——那一半在 [`FfiTrEngine`] 里
/// （in-flight 计数）；这个对象只回答"该不该"，谁去停是引擎的事。
///
/// 单独暴露出来是给对拍用的：`TranslatePolicyParityTest` 拿同一串随机操作和 Kotlin 原版对拍。
#[derive(uniffi::Object)]
pub struct FfiTrPolicy {
    inner: Mutex<feedhub_core::translate::server::Policy>,
}

#[uniffi::export]
impl FfiTrPolicy {
    #[uniffi::constructor]
    pub fn new(max_requests: u32, idle_ms: i64) -> Arc<Self> {
        Arc::new(FfiTrPolicy {
            inner: Mutex::new(feedhub_core::translate::server::Policy::new(
                max_requests,
                idle_ms,
            )),
        })
    }

    /// server 刚起来 / 刚重启：计数和时间戳归零。
    pub fn mark_started(&self, now_ms: i64) {
        self.inner.lock().unwrap().mark_started(now_ms);
    }

    /// 成功完成一次请求。
    pub fn note_request(&self, now_ms: i64) {
        self.inner.lock().unwrap().note_request(now_ms);
    }

    /// 累计请求数到上限：该重启了。
    pub fn should_restart(&self) -> bool {
        self.inner.lock().unwrap().should_restart()
    }

    /// 空闲超过 `idle_ms`（还没发过请求时不算）。
    pub fn idle_too_long(&self, now_ms: i64) -> bool {
        self.inner.lock().unwrap().idle_too_long(now_ms)
    }

    pub fn request_count(&self) -> u32 {
        self.inner.lock().unwrap().request_count()
    }
}

// ===========================================================================
// 引擎（llama-server 的启停 + 一次 chat 请求）
// ===========================================================================

/// 本地翻译引擎：`llama-server` 的进程、设备选择、就绪判断、chat 请求的拼装与解析。
#[derive(uniffi::Object)]
pub struct FfiTrEngine {
    inner: core_tr::engine::Engine,
    /// 到 `llama-server` 的那个 host（**只为了取消**：`NetHttpHost::cancel` 掐在飞的那一条）。
    ///
    /// 为什么需要它：切到这一层之前，取消是宿主 `HostCallScope` 里 `Call.cancel()` 干的；现在请求在
    /// Rust 里发，Kotlin 那侧没有 Call 可掐，只能把"掐这一条"作为一个动作传进 `HostedCall.blocking`
    /// （和 11 家平台的 `cancelInFlight = ::cancelInFlight` 同一个口径）。
    host: Arc<NetHttpHost>,
}

/// **到本机 `llama-server` 的那个 `HttpHost`**。
///
/// 以前这一跳是宿主的 OkHttp（`Proxy.NO_PROXY` + `connectTimeout(2s)` + `readTimeout(120s)`），
/// 现在是 `feedhub-net` 的一份**专用客户端**：
///
/// | 这一档 | 值 | 为什么 |
/// |---|---|---|
/// | 代理 | **`ProxyPlan::direct()`** | 只连 `127.0.0.1`。用户设置的代理**绝不**用在这一跳上（和 `Proxy.NO_PROXY` 一样） |
/// | 连接超时 | **2 秒** | 本机端口连不上要立刻说，不该等 15 秒 |
/// | 读超时 | **120 秒** | 模型生成一段译文要几十秒（`-np 2` 时单条 p50 已 500 ms，长文更久） |
/// | Cookie / 限速 / 默认头 | **都没有** | 这一跳不是"平台请求"：不占平台限速、不带任何头 |
///
/// 不复用平台的 `NetClient`：那上面挂着平台的代理快照（境外平台是 SOCKS5），
/// 拿它发回环请求在语义上就是"走代理去连本机"，会出现代理不可用时翻译也连带失败。
fn local_translate_client() -> Result<Arc<NetClient>, NetError> {
    let client = NetClient::builder()
        .proxies(ProxyPlan::direct())
        .cookies(Arc::new(NoCookies))
        .limiter(Arc::new(NoLimiter))
        .connect_timeout(Duration::from_secs(2))
        .read_timeout(Duration::from_secs(120))
        .build()?;
    Ok(Arc::new(client))
}

#[uniffi::export]
impl FfiTrEngine {
    /// 桌面口径（并发 2 / 空闲 5 分钟 / 150 次重启）。
    ///
    /// **不发请求给宿主**：到 `llama-server` 的 HTTP 由 `feedhub-net` 直接发（见
    /// [`local_translate_client`]）；宿主只剩"状态 Flow"和"空闲心跳"。
    ///
    /// 不传 `store`：手机上就不会记住"降到哪一档"（每次从 NPU 重新试）。
    #[uniffi::constructor]
    pub fn new(status: Arc<dyn FfiTrStatusHost>) -> Result<Arc<Self>, FfiNetError> {
        Self::with_timeouts(status, tr_timeouts_desktop())
    }

    /// 指定时间 / 数量参数（安卓传 [`FfiTrTimeouts::android`]）。
    #[uniffi::constructor]
    pub fn with_timeouts(
        status: Arc<dyn FfiTrStatusHost>,
        timeouts: FfiTrTimeouts,
    ) -> Result<Arc<Self>, FfiNetError> {
        let client = local_translate_client()?;
        let host = Arc::new(NetHttpHost::new(client, "local-translate"));
        Ok(Arc::new(FfiTrEngine {
            inner: core_tr::engine::Engine::with_policy(
                host.clone(),
                Arc::new(StatusAdapter(status)),
                timeouts.into(),
            ),
            host,
        }))
    }

    /// 取消这一跳**正在飞**的请求（协程被取消时调；`Call.cancel()` 的语义）。
    ///
    /// 幂等、可重复用：取消之后下一个请求会拿到一个新令牌，照常能发。
    pub fn cancel(&self) {
        self.host.cancel();
    }

    /// 带 `ctx.store` 的构造（**手机上用这个**）：NPU 起不来 / 自检失败时把"降到哪一档"记下来，
    /// 下次直接从那一档开始；`app_version` 变了就整条作废、重新从 NPU 试。
    ///
    /// 时间常量是**桌面**那套（并发 2 / 空闲 5 分钟 / 150 次）。安卓要手机那套就传
    /// [`timeouts`](Self::with_store_and_timeouts)。
    #[uniffi::constructor]
    pub fn with_store(
        status: Arc<dyn FfiTrStatusHost>,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, FfiNetError> {
        Self::with_store_and_timeouts(status, store, tr_timeouts_desktop())
    }

    /// **安卓生产路径用的那个**：`ctx.store`（降级记忆）+ 手机的时间常量。
    #[uniffi::constructor]
    pub fn with_store_and_timeouts(
        status: Arc<dyn FfiTrStatusHost>,
        store: Arc<dyn HostStore>,
        timeouts: FfiTrTimeouts,
    ) -> Result<Arc<Self>, FfiNetError> {
        let client = local_translate_client()?;
        let host = Arc::new(NetHttpHost::new(client, "local-translate"));
        Ok(Arc::new(FfiTrEngine {
            inner: core_tr::engine::Engine::with_policy_and_store(
                host.clone(),
                Arc::new(StatusAdapter(status)),
                timeouts.into(),
                Arc::new(StoreAdapter(store)),
            ),
            host,
        }))
    }

    /// 一次翻译（会按需启动 / 重启 `llama-server`）。
    ///
    /// 阻塞：模型加载时最长可以等到 120 秒，宿主要放在 IO 线程上（`HostedCall.blocking`）。
    pub fn translate(
        &self,
        config: FfiTrConfig,
        text: String,
        target: String,
        now_ms: i64,
    ) -> Result<String, FfiTrError> {
        let cfg = CoreConfig {
            enabled: config.enabled,
            model_path: config.model_path,
            server_path: config.server_path,
            cache_dir: config.cache_dir,
            icd_file: config.icd_file,
            app_version: config.app_version,
            mobile: config.mobile,
            forced_backend: config.forced_backend,
        };
        Ok(self.inner.translate(&cfg, &text, &target, now_ms)?)
    }

    /// 对**已经在跑**的端口发一次 chat 请求（请求等价比对 / 排障用，不碰进程）。
    pub fn chat(&self, port: u16, text: String, target: String) -> Result<String, FfiTrError> {
        Ok(self.inner.chat(port, &text, &target)?)
    }

    /// 当前状态。
    pub fn status(&self) -> FfiTrStatus {
        self.inner.status().into()
    }

    /// 有没有正在跑的 `llama-server`。
    pub fn running(&self) -> bool {
        self.inner.running()
    }

    /// server 现在监听哪个端口（没在跑是 0）。
    pub fn port(&self) -> u16 {
        self.inner.port()
    }

    /// 同时能处理几个请求（= `-np`，排队器用它设并发上限）。桌面 2 / 手机 1。
    pub fn parallelism(&self) -> u32 {
        self.inner.parallelism()
    }

    /// 当前**实际**在用的后端（手机上降级之后会变）；`null` = 桌面（只有 AMD 核显）或者还没起过。
    pub fn backend(&self) -> Option<FfiTrBackend> {
        self.inner.backend().map(FfiTrBackend::from)
    }

    /// 宿主每 60 秒问一次：该不该停（空闲 5 分钟 / 累计 150 次请求）。返回 `true` = 这次停掉了。
    pub fn reap(&self, now_ms: i64) -> bool {
        self.inner.reap(now_ms)
    }

    /// **只停 `llama-server`，不关引擎**（安卓退到后台时用）。
    ///
    /// 和 [`Self::shutdown`] 的区别：`shutdown` 之后这个引擎永远报"翻译已关闭"（App 退出用）；
    /// 这个只是把进程收掉、状态回 `STOPPED`，下一次 [`Self::translate`] 自己再起一个。
    pub fn stop_server(&self) {
        self.inner.stop_server()
    }

    /// App 退出：收进程（最多等 300 ms，然后 SIGKILL）。
    ///
    /// 叫 `shutdown` 不叫 `close`：UniFFI 会给"有 `close` 方法的对象"额外生成一个 `AutoCloseable`
    /// 实现，和显式声明的 `fun close()` 撞成 "Conflicting overloads"（`FfiLibrary` 当初踩的同一个坑）。
    pub fn shutdown(&self) {
        self.inner.close()
    }
}

// ===========================================================================
// 纯函数
// ===========================================================================

/// 解析 `--list-devices` 的完整输出（`HTP0` / `GPUOpenCL` / `VulkanN` 都认；日志行不算）。
///
/// 和 [`tr_parse_devices`] 的分工：那个只认 `VulkanN`（桌面红线用），这个给手机挑后端用。
#[uniffi::export]
pub fn tr_parse_backend_devices(output: String) -> Vec<FfiTrBackendDevice> {
    core_tr::device::parse_backend_devices(&output)
        .into_iter()
        .map(FfiTrBackendDevice::from)
        .collect()
}

/// 这台机器是手机（有 HTP / GPUOpenCL）还是桌面（只有 Vulkan）。`true` = 手机。
#[uniffi::export]
pub fn tr_is_mobile(devices: Vec<FfiTrBackendDevice>) -> bool {
    let devices: Vec<core_tr::device::BackendDevice> = devices
        .into_iter()
        .map(|d| core_tr::device::BackendDevice {
            name: d.name,
            description: d.description,
        })
        .collect();
    core_tr::device::platform_of(&devices) == core_tr::device::Platform::Mobile
}

/// 这台机器上**能用**的档位（顺序固定：NPU → GPU → NPU+GPU → CPU；CPU 一定在）。
#[uniffi::export]
pub fn tr_usable_backends(devices: Vec<FfiTrBackendDevice>) -> Vec<FfiTrBackend> {
    let devices: Vec<core_tr::device::BackendDevice> = devices
        .into_iter()
        .map(|d| core_tr::device::BackendDevice {
            name: d.name,
            description: d.description,
        })
        .collect();
    core_tr::device::usable_tiers(&devices)
        .into_iter()
        .map(FfiTrBackend::from)
        .collect()
}

/// 设置页那一行显示的字（`NPU（Hexagon HTP）` / …）。
#[uniffi::export]
pub fn tr_backend_label(backend: FfiTrBackend) -> String {
    core_tr::device::Tier::from(backend).label().to_string()
}

/// "上次降到哪一档"那个记忆的键（宿主清缓存 / 排障用）。
#[uniffi::export]
pub fn tr_backend_key() -> String {
    core_tr::device::KEY_BACKEND.to_string()
}

/// 一句话的输出看起来是不是坏的（空串 / 一大半是替换字符或控制字符）。降级判据之一。
#[uniffi::export]
pub fn tr_looks_broken(text: String) -> bool {
    core_tr::device::looks_broken(&text)
}

/// [`tag`] 是不是目标语言（`languageMatchesTarget` 的搬迁）。
#[uniffi::export]
pub fn tr_matches_target(tag: String, target: String) -> bool {
    core_tr::lang::matches_target(&tag, &target)
}

/// 解析 `llama-server --list-devices` 的输出（只认 `VulkanN: 名字` 这种行）。
#[uniffi::export]
pub fn tr_parse_devices(output: String) -> Vec<FfiTrDevice> {
    core_tr::server::parse_devices(&output)
        .into_iter()
        .map(FfiTrDevice::from)
        .collect()
}

/// 按名字挑 AMD 核显（含 `RADV` 或 `Radeon`）；挑不到是 `null`（调用方必须报错、不启动）。
#[uniffi::export]
pub fn tr_choose_device(devices: Vec<FfiTrDevice>) -> Option<FfiTrDevice> {
    let devices: Vec<feedhub_core::translate::server::Device> = devices
        .into_iter()
        .map(|d| feedhub_core::translate::server::Device {
            index: d.index,
            name: d.name,
        })
        .collect();
    core_tr::server::choose_device(&devices).cloned().map(FfiTrDevice::from)
}

/// 启动 `llama-server` 时的 Vulkan 环境变量（有序，`VK_DRIVER_FILES` / `VK_ICD_FILENAMES` /
/// `LD_LIBRARY_PATH`）。**不带** `GGML_VK_VISIBLE_DEVICES`（按名字挑设备，不按序号）。
#[uniffi::export]
pub fn tr_vk_driver_env(server: String, icd_file: Option<String>) -> Vec<crate::host::FfiHeader> {
    let icd = icd_file.unwrap_or_else(|| core_tr::server::RADV_ICD_FILE.to_string());
    core_tr::server::vk_driver_env(
        std::path::Path::new(&server),
        std::path::Path::new(&icd),
    )
    .into_iter()
    .map(|(name, value)| crate::host::FfiHeader { name, value })
    .collect()
}

/// 一次 chat 请求的 body（**逐字节**和 Kotlin 原版一样；请求等价的验收门用它）。
#[uniffi::export]
pub fn tr_chat_payload(text: String, target: String) -> String {
    core_tr::engine::chat_payload(&text, &target)
}

/// 解析 chat 响应（`choices[0].message.content`）；空 / 缺字段是"模型没有返回译文"。
#[uniffi::export]
pub fn tr_parse_chat_response(body: String) -> Result<String, FfiTrError> {
    Ok(core_tr::engine::parse_chat_response(&body)?)
}

/// 富文本里每一块要翻的原文（不可翻的给 `null`）。
#[uniffi::export]
pub fn tr_plan_blocks(blocks: Vec<FfiDetailBlock>) -> Vec<Option<String>> {
    let blocks: Vec<core_detail::Block> = blocks.iter().map(from_ffi_block).collect();
    core_tr::text::plan(&blocks)
}

/// 把译文拼回富文本：`texts[i]` 是第 `i` 块的译文（`null` = 不翻 / 没译 / 越界）。
#[uniffi::export]
pub fn tr_apply_blocks(
    blocks: Vec<FfiDetailBlock>,
    texts: Vec<Option<String>>,
) -> Vec<FfiDetailBlock> {
    let core_blocks: Vec<core_detail::Block> = blocks.iter().map(from_ffi_block).collect();
    core_tr::text::apply(&core_blocks, &texts)
        .iter()
        .map(crate::to_ffi_detail_block)
        .collect()
}

/// 整段富文本里所有可翻的文字（顺序和块顺序一致）。
#[uniffi::export]
pub fn tr_translatable_texts(blocks: Vec<FfiDetailBlock>) -> Vec<String> {
    let blocks: Vec<core_detail::Block> = blocks.iter().map(from_ffi_block).collect();
    core_tr::text::translatable_texts(&core_detail::RichText { blocks })
}

// ===========================================================================
// 入口的判定与状态
// ===========================================================================
//
// **只在判定为外语时出现**；点了变「正在翻译…」/「正在加载翻译模型…」；翻好就地显示译文 + 一行灰字
// 「由本地模型翻译自 <语言> · 显示原文」；失败一行原因 +「重试」。**判断在 Rust，Compose 只画**
//
// 和自动翻那一套（`FfiTrGate` / `should_auto_translate`）的区别：那边带**平台门控**和 25 字节门槛，
// 回答"要不要自动翻"；这里回答"要不要给用户一个手动的入口"——不看平台、不看字节数，
// 只看"这本来就不是目标语言"。两条口径的共用部分在 `core::translate::lang`。

/// 入口现在长什么样（Kotlin `TranslationPhase` 一一对应）。
///
/// 文案由界面定（`正在翻译…` / `正在加载翻译模型…`）；这里只说"该显示哪一档"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTrPhase {
    /// 不显示（不是外语 / 译文和原文一样 / 译文为空）。
    Hidden,
    /// 一行可点的「翻译」。
    Idle,
    /// 「正在加载翻译模型…」（`llama-server` 还在起）。
    ModelLoading,
    /// 「正在翻译…」。
    Translating,
    /// 有译文了（显示译文 + 灰字那一行）。
    Done,
    /// 失败（一行灰字原因 +「重试」）。
    Failed,
}

/// [`tr_phase`] 的输入（界面现成的布尔量；Rust 不存界面状态）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiTrPhaseInput {
    /// 这条内容要不要出入口（[`FfiTrStore::entry_visible`] 的结论）。
    pub visible: bool,
    /// 翻完了但译文和原文一样（专名 / 缩写 / `zsbd`）：不显示入口，也不显示灰字那一行。
    pub skipped: bool,
    /// 这一条正在翻（排队中或跑着）。
    pub translating: bool,
    /// 已经有译文。
    pub translated: bool,
    /// 上一次失败过（还没重试）。
    pub failed: bool,
    /// 模型还在加载（引擎状态 `STARTING`）。
    pub model_loading: bool,
}

/// 入口的状态。优先级：隐藏 > 失败 > 有译文 > 在翻 > 可点。
///
/// "失败"排在"在翻"前面：失败之后协程可能还没收尾（`translating` 仍是 true），那时显示「重试」才有用。
#[uniffi::export]
pub fn tr_phase(input: FfiTrPhaseInput) -> FfiTrPhase {
    match core_tr::entry::phase(core_tr::entry::Input {
        visible: input.visible,
        skipped: input.skipped,
        translating: input.translating,
        translated: input.translated,
        failed: input.failed,
        model_loading: input.model_loading,
    }) {
        core_tr::entry::Phase::Hidden => FfiTrPhase::Hidden,
        core_tr::entry::Phase::Idle => FfiTrPhase::Idle,
        core_tr::entry::Phase::ModelLoading => FfiTrPhase::ModelLoading,
        core_tr::entry::Phase::Translating => FfiTrPhase::Translating,
        core_tr::entry::Phase::Done => FfiTrPhase::Done,
        core_tr::entry::Phase::Failed => FfiTrPhase::Failed,
    }
}

/// 现在该显示译文吗（`false` = 原文：用户点了「显示原文」/ 没译文 / 译文本就是原文）。
#[uniffi::export]
pub fn tr_showing_translation(show_original: bool, translated: Option<String>, original: String) -> bool {
    core_tr::entry::showing_translation(show_original, translated.as_deref(), &original)
}

/// 现在要画哪一份文字（原文 / 译文）。
#[uniffi::export]
pub fn tr_display(original: String, translated: Option<String>, show_original: bool) -> String {
    core_tr::entry::display(&original, translated.as_deref(), show_original).to_string()
}

/// 富文本版：`texts[i]` 是第 `i` 块的译文（`null` = 这块不翻 / 没译）。
/// `true` = 现在画**译文**（至少有一块真翻好了，而且用户没点「显示原文」）。
#[uniffi::export]
pub fn tr_showing_blocks(show_original: bool, texts: Vec<Option<String>>) -> bool {
    core_tr::entry::showing_blocks(show_original, &texts)
}

/// 灰字里那个语言名（`en` → "英语"；认不出的写"外语"）。
#[uniffi::export]
pub fn tr_language_label(tag: Option<String>) -> String {
    core_tr::entry::language_label(tag.as_deref()).to_string()
}

/// 引擎状态 → "模型还在加载"（界面据此把「正在翻译…」换成「正在加载翻译模型…」）。
#[uniffi::export]
pub fn tr_model_loading(status: FfiTrStatus) -> bool {
    core_tr::entry::model_loading(match status {
        FfiTrStatus::Stopped => core_tr::Status::Stopped,
        FfiTrStatus::Starting => core_tr::Status::Starting,
        FfiTrStatus::Ready => core_tr::Status::Ready,
        FfiTrStatus::Failed => core_tr::Status::Failed,
    })
}

// ===========================================================================
// FFI → core（富文本块 / 行内片段）
// ===========================================================================

fn from_ffi_block(block: &FfiDetailBlock) -> core_detail::Block {
    use crate::FfiDetailBlock as F;
    match block {
        F::Paragraph { spans } => core_detail::Block::Paragraph {
            spans: spans.iter().map(from_ffi_span).collect(),
        },
        F::Heading { spans } => core_detail::Block::Heading {
            spans: spans.iter().map(from_ffi_span).collect(),
        },
        F::Quote { spans } => core_detail::Block::Quote {
            spans: spans.iter().map(from_ffi_span).collect(),
        },
        F::Code { text, language } => core_detail::Block::Code {
            text: text.clone(),
            language: language.clone(),
        },
        F::Image {
            url,
            width,
            height,
            original,
            caption,
        } => core_detail::Block::Image {
            url: url.clone(),
            width: *width,
            height: *height,
            original: original.clone(),
            caption: caption.clone(),
        },
        F::Card {
            title,
            subtitle,
            cover,
            url,
            target,
        } => core_detail::Block::Card {
            title: title.clone(),
            subtitle: subtitle.clone(),
            cover: cover.clone(),
            url: url.clone(),
            target: target.as_ref().map(from_ffi_target),
        },
        F::Video {
            id,
            part_id,
            cover,
            duration_sec,
            width,
            height,
            title,
            url,
        } => core_detail::Block::Video {
            id: id.clone(),
            part_id: part_id.clone(),
            cover: cover.clone(),
            duration_sec: *duration_sec,
            width: *width,
            height: *height,
            title: title.clone(),
            url: url.clone(),
        },
        F::Divider => core_detail::Block::Divider,
    }
}

fn from_ffi_span(span: &crate::FfiDetailSpan) -> core_detail::Span {
    use crate::FfiDetailSpan as F;
    match span {
        F::Text {
            text,
            bold,
            italic,
        } => core_detail::Span::Text {
            text: text.clone(),
            bold: *bold,
            italic: *italic,
        },
        F::Link { text, url, target } => core_detail::Span::Link {
            text: text.clone(),
            url: url.clone(),
            target: target.as_ref().map(from_ffi_target),
        },
        F::Mention { text, user } => core_detail::Span::Mention {
            text: text.clone(),
            user: feedhub_core::model::UserRef {
                platform: user.platform.clone(),
                id: user.id.clone(),
                name: user.name.clone(),
                avatar: user.avatar.clone(),
                url: user.url.clone(),
            },
        },
        F::Emote { text, url } => core_detail::Span::Emote {
            text: text.clone(),
            url: url.clone(),
        },
    }
}

fn from_ffi_target(target: &crate::FfiDetailTarget) -> feedhub_core::model::DetailTarget {
    use crate::FfiDetailTarget as F;
    match target {
        F::Web { url } => feedhub_core::model::DetailTarget::Web { url: url.clone() },
        F::Native { id, platform } => feedhub_core::model::DetailTarget::Native {
            id: id.clone(),
            platform: platform.clone(),
        },
    }
}
