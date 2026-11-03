//! **CPU / NPU 的后端路由状态机**（`RoutingEmbedder` 的判定与状态）。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/NpuBackend.kt` 的 Rust 版。
//! 搬的是：开关 / 前后台 / 分时释放会话带来的**状态迁移**、`generation` 代号、`prepareJob` 的准备进度
//! （逐张编图、探设备只探一次、等"能编图"的窗口与 2 分钟超时）、`inFlight` 计数（"空闲"的判据）、
//! 每批选哪张图、回退 / 不可用的判定、以及"这一批的账"（后端 / 模型标识 / 图长度）。
//!
//! **留宿主**（`RoutingEmbedder` 那边只剩这些）：`CoroutineScope` / `Job` 的取消与重来、引擎句柄
//! （`NpuEngine` 的 map）、探设备 / 编图 / 推理这些 ORT / QNN 调用、Compose 状态字段、以及日志的落地。
//!
//! ## 为什么是"宿主拉一步、Rust 回一步"
//!
//! 探设备 / 编图 / 推理在安卓上都是 **suspend** 的框架调用（`withContext(Dispatchers.Default)`），
//! Rust 的同步调用里调不了。所以和 [`super::schedule`] 一个套路：Rust 说"下一步做什么"，
//! 宿主做完把结果报回来。三种步骤：
//!
//! | 步骤 | 宿主做什么 | 报回来 |
//! |---|---|---|
//! | [`PrepareStep::Wait`] | `delay(delay_ms)` | — |
//! | [`PrepareStep::Probe`] | 造引擎 + `probe()` | [`Router::report_probe`] |
//! | [`PrepareStep::Compile`] | 放掉 CPU 会话 + `prepare()` | [`Router::report_compile`] |
//! | [`PrepareStep::Commit`] | 把引擎放进 map | [`Router::commit`] |
//! | [`PrepareStep::Discard`] | 关掉这张引擎 | — |
//!
//! 一批（[`EmbedStep::RunNpu`]）只过一次边界，向量本来就是一整批交回的。
//!
//! 时间（`charging` / 前后台）、图长度、模型标识、`cpu` 在不在**全由宿主注入**：这里不读时钟、
//! 不建会话、不碰 ORT。

use std::collections::BTreeMap;

use super::npu;

/// 一批实际用的后端（写进性能记录 / 设置页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Npu,
}

/// 状态（文案在宿主；这里只给**是哪一种**和它带的数字 / 原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpuStatus {
    /// 这个包没带 NPU 模型。
    NoModel,
    /// 带了模型，但开关关着：用 CPU。
    Off,
    /// **只带 NPU 模型**的包，开关关着（= 本地向量的总开关关着）。
    Disabled { reason: String },
    /// **只带 NPU 模型**的包，NPU 真用不了（没有 CPU 可退）。
    Unavailable { reason: String },
    /// 开关开着，正在探设备 / 编图。
    Preparing { step: String },
    /// 已经在用 NPU 算；`lens` = 现在有几张图就绪（升序）。
    Active { detail: String, lens: Vec<i32> },
    /// 用不了，已回退 CPU。
    Fallback { reason: String },
}

impl NpuStatus {
    /// 状态种类（Kotlin 那边的 `<Kind>Kind`，映射成密封接口用）。
    pub fn kind(&self) -> StatusKind {
        match self {
            NpuStatus::NoModel => StatusKind::NoModel,
            NpuStatus::Off => StatusKind::Off,
            NpuStatus::Disabled { .. } => StatusKind::Disabled,
            NpuStatus::Unavailable { .. } => StatusKind::Unavailable,
            NpuStatus::Preparing { .. } => StatusKind::Preparing,
            NpuStatus::Active { .. } => StatusKind::Active,
            NpuStatus::Fallback { .. } => StatusKind::Fallback,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    NoModel,
    Off,
    Disabled,
    Unavailable,
    Preparing,
    Active,
    Fallback,
}

/// 路由参数（宿主按这个构建里的实际情况填）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterConfig {
    /// 包里带的 NPU 图长度（升序；空 = 没带）。
    pub npu_lengths: Vec<i32>,
    /// 这个包的向量空间长度（最长那张）。
    pub space_len: i32,
    /// 有**能用的** CPU 实现吗（`cpu.available`）。
    pub has_cpu: bool,
    /// 有没有 CPU 实现这个对象（`cpu != null`；NPU 包是 false）。决定"回退时这一批算不算 CPU"。
    pub cpu_present: bool,
    /// 写进 `item_embedding.model` 的 CPU 标识。
    pub cpu_model: String,
    /// 写进 `item_embedding.model` 的 NPU 空间标识。
    pub npu_space_model: String,
    /// 等"能编图"的间隔（默认 300 ms）。
    pub wait_step_ms: i64,
    /// 等"能编图"的上限（默认 2 分钟）。
    pub max_wait_ms: i64,
    /// 等 NPU 就绪的间隔（默认 300 ms）。
    pub ready_poll_ms: i64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        RouterConfig {
            npu_lengths: Vec::new(),
            space_len: 0,
            has_cpu: false,
            cpu_present: false,
            cpu_model: super::EMBEDDING_MODEL_ID.to_string(),
            npu_space_model: String::new(),
            wait_step_ms: 300,
            max_wait_ms: 120_000,
            ready_poll_ms: 300,
        }
    }
}

/// 准备阶段的下一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareStep {
    /// 等一个能编图的窗口：宿主 `delay(delay_ms)` 之后再来拉一步。
    Wait { delay_ms: i64 },
    /// 造这个长度的引擎并 `probe()`（**一次准备只探一次**），结果用 [`Router::report_probe`] 报回来。
    Probe { len: i32, which: String },
    /// 放掉 CPU 会话、`prepare()` 这个长度的图，结果用 [`Router::report_compile`] 报回来。
    Compile { len: i32, which: String },
    /// 把引擎放进宿主的 map，然后调 [`Router::commit`]。
    Commit { len: i32 },
    /// 这张图作废（关掉引擎）：开关关了 / 代号变了 / 编完已经退到后台。
    Discard { len: i32 },
    /// 当前准备结束（编完了 / 被打断 / 回退了）。
    Finished,
}

/// 一批的下一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedStep {
    /// 这批文字是空的。
    Empty,
    /// 用 CPU 算（[`Router::begin_cpu`] / [`Router::end_cpu`] 把"在算"记上）。
    UseCpu { model: String, seq_len: i32 },
    /// 用这个长度的 NPU 图算；算完用 [`Router::report_embed`] 报结果。
    RunNpu { len: i32 },
    /// 还没就绪：宿主 `delay(delay_ms)` 再拉一步；`start_prepare` = 现在该把准备拉起来。
    WaitReady { delay_ms: i64, start_prepare: bool },
}

/// 分词 / 截断该问谁（按顺序试，第一个给答案的赢）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// 已经就绪的 NPU 引擎（按长度升序）。
    Engine(i32),
    /// CPU 实现。
    Cpu,
    /// 工厂的"只要分词"那条路。
    Factory,
}

/// 这一批的账（性能记录的口径：后端 / 模型 / 图长度）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchAccount {
    pub backend: Backend,
    pub model: String,
    pub seq_len: i32,
}

/// CPU / NPU 的后端路由状态机。
pub struct Router {
    cfg: RouterConfig,
    requested: bool,
    broken: bool,
    foreground: bool,
    in_flight: i32,
    /// 就绪的图长度 → 那张图的说明（设置页 / 日志用）。
    ready: BTreeMap<i32, String>,
    /// 准备 / 编译的代号：取消、关开关、重新开始准备都 +1。
    generation: u64,
    /// 一轮准备在跑吗（宿主那边的 `prepareJob?.isActive`）。
    running_prepare: bool,
    /// 当前还剩哪几张没编（按长度升序）。
    pending: Vec<i32>,
    idx: usize,
    /// 当前探过设备了吗（**一次准备只探一次**）。
    probed: bool,
    /// 等窗口累计了多少毫秒。
    waited_ms: i64,
    /// 正在准备的那张（`Probe` / `Compile` 之间宿主手里的引擎）。
    current: Option<i32>,
    status: NpuStatus,
    last: BatchAccount,
    logs: Vec<String>,
    /// 编译成功到 `commit` 之间先拿着那张图的说明。
    pending_details: BTreeMap<i32, String>,
}

impl Router {
    pub fn new(cfg: RouterConfig) -> Self {
        let npu_bundled = cfg.npu_lengths.is_empty() == false;
        let status = if !npu_bundled {
            NpuStatus::NoModel
        } else if cfg.has_cpu {
            NpuStatus::Off
        } else {
            // 只带 NPU 模型的包：开关默认关着 = 本地向量整个关着（没有 CPU 顶着）
            NpuStatus::Disabled {
                reason: "「用 NPU 算」关着，这个包只带 NPU 模型".to_string(),
            }
        };
        Router {
            last: BatchAccount {
                backend: Backend::Cpu,
                model: cfg.cpu_model.clone(),
                seq_len: 0,
            },
            cfg,
            requested: false,
            broken: false,
            foreground: true,
            in_flight: 0,
            ready: BTreeMap::new(),
            generation: 0,
            running_prepare: false,
            pending: Vec::new(),
            idx: 0,
            probed: false,
            waited_ms: 0,
            current: None,
            status,
            logs: Vec::new(),
            pending_details: BTreeMap::new(),
        }
    }

    // ---------------------------------------------------------------- 只读

    pub fn config(&self) -> &RouterConfig {
        &self.cfg
    }

    pub fn status(&self) -> &NpuStatus {
        &self.status
    }

    pub fn npu_bundled(&self) -> bool {
        !self.cfg.npu_lengths.is_empty()
    }

    pub fn cpu_bundled(&self) -> bool {
        self.cfg.has_cpu
    }

    pub fn npu_lengths(&self) -> &[i32] {
        &self.cfg.npu_lengths
    }

    /// 现在有几个引擎在内存里（升序）。
    pub fn ready_lengths(&self) -> Vec<i32> {
        self.ready.keys().copied().collect()
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    pub fn requested(&self) -> bool {
        self.requested
    }

    pub fn last_batch(&self) -> &BatchAccount {
        &self.last
    }

    /// 现在有没有一个**能用的后端**（调度器据此决定"进不进队列"）。
    pub fn available(&self) -> bool {
        npu::available(self.cfg.has_cpu, self.npu_bundled(), self.requested, self.broken)
    }

    /// 写进 `item_embedding.model` 的标识（每批现读）。
    pub fn model_id(&self) -> String {
        npu::model_id(
            &self.ready_lengths(),
            self.npu_bundled(),
            self.cfg.has_cpu,
            &self.cfg.npu_space_model,
            &self.cfg.cpu_model,
        )
    }

    /// 把攒下来的日志行取走（宿主接到自己的日志口）。
    pub fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// 分词 / 截断该按什么顺序问（就绪的引擎 → CPU → 工厂）。
    pub fn token_sources(&self) -> Vec<TokenSource> {
        let mut out: Vec<TokenSource> = self.ready.keys().copied().map(TokenSource::Engine).collect();
        if self.cfg.cpu_present {
            out.push(TokenSource::Cpu);
        }
        out.push(TokenSource::Factory);
        out
    }

    // ---------------------------------------------------------------- 开关 / 前后台 / 会话

    /// 设置里的「用 NPU 算」变了。返回 `true` = 该把准备拉起来（宿主 `launch`）。
    pub fn set_requested(&mut self, on: bool) -> bool {
        if self.requested == on {
            return false;
        }
        self.requested = on;
        if !on {
            self.cancel_prepare();
            self.broken = false;
            self.close_engines();
            self.status = if !self.npu_bundled() {
                NpuStatus::NoModel
            } else if self.cfg.has_cpu {
                NpuStatus::Off
            } else {
                NpuStatus::Disabled {
                    reason: "「用 NPU 算」关着，这个包只带 NPU 模型".to_string(),
                }
            };
            let line = if self.cfg.has_cpu {
                "[向量] NPU 关掉了，回到 CPU".to_string()
            } else {
                "[向量] NPU 关掉了，这个包没有 CPU 模型：本地向量不算".to_string()
            };
            self.logs.push(line);
            return false;
        }
        if !self.npu_bundled() {
            self.status = NpuStatus::NoModel;
            return false;
        }
        self.broken = false;
        self.try_start_prepare()
    }

    /// 前后台变了。返回 `true` = 该把准备协程拉起来（回前台、并且还有图没就绪）。
    pub fn on_foreground(&mut self, active: bool) -> bool {
        self.foreground = active;
        if !active {
            // 退到后台：会话全放掉（context binary 在磁盘上，回前台加载很快）
            self.close_engines();
            if self.requested && self.npu_bundled() && !self.broken {
                self.status = NpuStatus::Preparing {
                    step: "回前台后接着准备（编好的图已经在磁盘上）".to_string(),
                };
            }
            return false;
        }
        if self.requested
            && !self.broken
            && self.cfg.npu_lengths.iter().any(|len| !self.ready.contains_key(len))
        {
            return self.try_start_prepare();
        }
        false
    }

    /// 释放所有会话（不退开关、不清 `broken`）：下次准备会重新加载 context binary。
    pub fn close_engines(&mut self) {
        self.ready.clear();
    }

    /// 分时（封面向量那侧在编图 / 算封面之前把纯文字会话放掉）。
    pub fn release_sessions(&mut self) {
        if self.ready.is_empty() {
            return;
        }
        self.close_engines();
        if self.requested && self.npu_bundled() && !self.broken {
            self.status = NpuStatus::Preparing {
                step: "封面算完再加载回来（context 已经在磁盘上）".to_string(),
            };
        }
        self.logs
            .push("[向量] 纯文字会话放掉了（让封面窗口的两张图独占 QNN）".to_string());
    }

    /// `Embedder.close()`：会话释放（CPU 那侧由宿主自己关）。
    pub fn close_sessions(&mut self) {
        self.close_engines();
        if self.requested && self.npu_bundled() && !self.broken {
            self.status = NpuStatus::Preparing {
                step: "会话已释放（回前台后重新加载 context binary）".to_string(),
            };
        }
    }

    // ---------------------------------------------------------------- 准备

    /// 该不该 / 能不能把准备拉起来（一串前置条件都在 Rust）。
    pub fn try_start_prepare(&mut self) -> bool {
        if !self.requested || self.broken || !self.npu_bundled() {
            return false;
        }
        if self.running_prepare {
            return false;
        }
        if self.cfg.npu_lengths.iter().all(|len| self.ready.contains_key(len)) {
            return false;
        }
        self.generation += 1;
        self.running_prepare = true;
        self.pending = self
            .cfg
            .npu_lengths
            .iter()
            .copied()
            .filter(|len| !self.ready.contains_key(len))
            .collect();
        self.idx = 0;
        self.probed = false;
        self.waited_ms = 0;
        self.current = None;
        true
    }

    /// 取消正在进行的准备（"编完才发现过期"那条路径靠代号认出自己过期）。
    pub fn cancel_prepare(&mut self) {
        self.generation += 1;
        self.running_prepare = false;
        self.pending.clear();
        self.idx = 0;
        self.probed = false;
        self.waited_ms = 0;
        self.current = None;
    }

    /// 准备协程被取消（宿主那边 `job.cancel()`）：状态清掉，下次重新开始。
    pub fn prepare_cancelled(&mut self) {
        self.cancel_prepare();
    }

    /// 现在能不能开始编图：前台 + （在充电 或 没有批在算）。
    fn can_compile_now(&self, charging: bool) -> bool {
        npu::can_compile_now(self.foreground, charging, self.in_flight)
    }

    fn which(&self) -> String {
        let len = self.current.unwrap_or(0);
        if self.pending.len() == 1 {
            format!("每条 {len} token")
        } else {
            format!("{}/{}：每条 {len} token", self.idx + 1, self.pending.len())
        }
    }

    /// 拉准备阶段的下一步。
    pub fn prepare_step(&mut self, charging: bool) -> PrepareStep {
        loop {
            if !self.running_prepare {
                return PrepareStep::Finished;
            }
            if !self.requested || self.broken {
                self.running_prepare = false;
                return PrepareStep::Finished;
            }
            match self.current {
                None => {
                    // 取下一张
                    let Some(len) = self.pending.get(self.idx).copied() else {
                        self.running_prepare = false;
                        return PrepareStep::Finished;
                    };
                    self.current = Some(len);
                    self.waited_ms = 0;
                    let which = self.which();
                    self.status = NpuStatus::Preparing {
                        step: format!("检查这台设备…（{which}）"),
                    };
                }
                Some(len) => {
                    if !self.can_compile_now(charging) {
                        // 等一个窗口：累计"干等"的时长，超了按真失败处理
                        self.waited_ms += self.cfg.wait_step_ms;
                        if npu::compile_wait_exceeded(self.waited_ms, self.cfg.max_wait_ms) {
                            self.fallback("等了 2 分钟也没有空闲（在前台 / 充电）来编图");
                            self.running_prepare = false;
                            return PrepareStep::Finished;
                        }
                        return PrepareStep::Wait {
                            delay_ms: self.cfg.wait_step_ms,
                        };
                    }
                    // 能编了：先探设备（一次准备只探一次），再编图
                    let which = self.which();
                    if !self.probed {
                        // 探过之后由 report_probe 推进（成功就直接进 Compile）
                        return PrepareStep::Probe { len, which };
                    }
                    self.status = NpuStatus::Preparing {
                        step: format!("编图 {which}（第一次要十几秒到一分钟）…"),
                    };
                    return PrepareStep::Compile { len, which };
                }
            }
        }
    }

    /// 宿主探完设备了（`reason` = `Some` 就是失败原因）。
    pub fn report_probe(&mut self, reason: Option<String>) {
        if let Some(reason) = reason {
            self.fallback(&reason);
            self.running_prepare = false;
            return;
        }
        self.probed = true;
    }

    /// 宿主编完图了（`failure` = `Some` 就是失败原因）。
    pub fn report_compile(&mut self, failure: Option<String>, detail: String) -> PrepareStep {
        let Some(len) = self.current else {
            return PrepareStep::Finished;
        };
        if !self.running_prepare {
            return PrepareStep::Discard { len };
        }
        if let Some(reason) = failure {
            self.fallback(&reason);
            self.running_prepare = false;
            return PrepareStep::Finished;
        }
        if !self.requested {
            self.running_prepare = false;
            return PrepareStep::Discard { len };
        }
        if !self.foreground {
            // 编完之后已经退到后台：会话关掉，context binary 已经在磁盘上，回前台再加载很快
            self.status = NpuStatus::Preparing {
                step: "回前台后接着准备（编图已经完成并缓存）".to_string(),
            };
            self.running_prepare = false;
            return PrepareStep::Discard { len };
        }
        self.pending_details.insert(len, detail);
        PrepareStep::Commit { len }
    }

    /// 宿主把引擎放好了：这时才算"就绪"（避免 Rust 说就绪而宿主 map 里还没有）。
    pub fn commit(&mut self, len: i32) {
        let detail = self.pending_details.remove(&len).unwrap_or_default();
        self.ready.insert(len, detail);
        self.current = None;
        self.idx += 1;
        self.waited_ms = 0;
        self.status = NpuStatus::Active {
            detail: self.ready_detail(),
            lens: self.ready_lengths(),
        };
        let lens = self
            .ready_lengths()
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(" / ");
        let own = self.ready.get(&len).cloned().unwrap_or_default();
        self.logs.push(format!(
            "[向量] NPU 就绪：每条 {len} token 的图（{lens}；{own}）"
        ));
    }

    /// 设置页那句 detail：把每张就绪的图的说明拼起来。
    fn ready_detail(&self) -> String {
        let parts: Vec<&str> = self.ready.values().map(String::as_str).collect();
        if parts.is_empty() {
            "HTP · w8a16".to_string()
        } else {
            parts.join("；")
        }
    }

    fn fallback(&mut self, reason: &str) {
        self.broken = true;
        self.close_engines();
        if self.cfg.has_cpu {
            self.status = NpuStatus::Fallback {
                reason: reason.to_string(),
            };
            self.logs
                .push(format!("[向量] NPU 用不了，回退到 CPU：{reason}"));
        } else {
            // 这个包没有 CPU 模型：不回退，安静地不可用，不算、不存
            self.status = NpuStatus::Unavailable {
                reason: reason.to_string(),
            };
            self.logs.push(format!(
                "[向量] NPU 用不了，这个包没有 CPU 模型：本地向量不可用（{reason}）"
            ));
        }
    }

    // ---------------------------------------------------------------- 推理

    /// 拉这一批的下一步。
    ///
    /// [`EmbedStep::RunNpu`] 返回之前 `in_flight` 已经 +1（宿主算完必须 [`Router::report_embed`]）。
    pub fn embed_step(&mut self, max_tokens: i32) -> EmbedStep {
        if !self.requested || self.broken || !self.npu_bundled() {
            return self.use_cpu();
        }
        if let Some(len) = npu::pick_ready(&self.ready_lengths(), &self.cfg.npu_lengths, max_tokens) {
            self.in_flight += 1;
            return EmbedStep::RunNpu { len };
        }
        if !self.requested || self.broken || !self.npu_bundled() {
            return self.use_cpu();
        }
        // 还没就绪：宿主等一下再拉（准备没在跑就让宿主拉起来）
        EmbedStep::WaitReady {
            delay_ms: self.cfg.ready_poll_ms,
            start_prepare: !self.running_prepare,
        }
    }

    /// NPU 这一批算完了（`ran_ok` = 拿到条数对得上的结果）。
    ///
    /// 失败分两种：**引擎在这期间被关掉了**（不算失败，宿主重新拉一步会重选）/ **真的推理失败**
    pub fn report_embed(&mut self, len: i32, ran_ok: bool) {
        self.in_flight -= 1;
        if ran_ok {
            self.last = BatchAccount {
                backend: Backend::Npu,
                model: self.cfg.npu_space_model.clone(),
                seq_len: len,
            };
            return;
        }
        if !self.ready.contains_key(&len) {
            return;
        }
        let detail = self.ready.get(&len).cloned().unwrap_or_default();
        self.fallback(&format!("NPU 推理失败（{detail}）"));
    }

    /// CPU 那一批开始算了（宿主在真的调 CPU 引擎之前调；`in_flight` +1）。
    pub fn begin_cpu(&mut self) {
        self.in_flight += 1;
    }

    /// CPU 那一批算完了。
    pub fn end_cpu(&mut self) {
        self.in_flight -= 1;
    }

    fn use_cpu(&mut self) -> EmbedStep {
        // 没有 CPU 实现（NPU 包）：没有"退回去算"这回事，模型标识写 NPU 那份
        let model = if self.cfg.cpu_present {
            self.cfg.cpu_model.clone()
        } else if self.npu_bundled() {
            self.cfg.npu_space_model.clone()
        } else {
            self.cfg.cpu_model.clone()
        };
        if self.cfg.cpu_present {
            self.last = BatchAccount {
                backend: Backend::Cpu,
                model,
                seq_len: 0,
            };
        } else {
            self.last.model = model;
            self.last.seq_len = 0;
        }
        EmbedStep::UseCpu {
            model: self.last.model.clone(),
            seq_len: 0,
        }
    }
}

