//! **封面向量**的调度状态机（一次一个"封面窗口"，纯文字闲着就算）。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/CoverScheduler.kt` 的 Rust 版。
//! 这里管：队列（上限 96 条，满了丢最老的）、够不够格（[`super::cover::has_cover_vector`]）、
//! 一个窗口取几条、已经有向量的不再算、图片取不到就跳过并记账（连续 4 次放弃）、失败之后**本次进程不可用**、
//! 排队 / 在算 / 不可用这些状态、以及"纯文字忙时等多久再看一次"的累计上限。
//!
//! 宿主（Kotlin）仍然做：**取图**（Coil 缓存 + 不按流量计费的网络 / 计费网络判断）、**推理**
//! （视觉 → 文本+图 两张 HTP 图，会话常驻，见 `NpuOnnxCoverEmbedder`）、**协程 / 前后台**、以及
//! `CardThumbnails.of` 的缩略图地址改写（那是图片加载层唯一的一份口径，不能在 Rust 再抄一遍）。

use std::collections::{HashSet, VecDeque};

use super::cover::{
    cover_embedding_text, cover_image_url, has_cover_vector, CoverStatus, CoverWindowStats,
};
use super::item::EmbedItem;
use super::schedule::{EmbedStore, EmbeddingRow};
use super::text::{float_vec_to_half_bytes, mrl, EMBEDDING_SOURCE_FEED};

/// 封面调度的参数（默认值和线上一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverConfig {
    /// 一个窗口最多几条。
    pub window_size: usize,
    /// 队列上限（首页刷很久也只有这么多个待算，超了就丢最老的）。
    pub max_queued: usize,
    pub dim: usize,
    /// 一条内容最多试几次取封面图。
    pub max_image_attempts: i32,
    /// 纯文字在算时多久看一次。
    pub text_busy_poll_ms: i64,
    /// 一轮最多盯多久（纯文字一直忙时）。
    pub max_wait_ms: i64,
}

impl Default for CoverConfig {
    fn default() -> Self {
        CoverConfig {
            window_size: 32,
            max_queued: 96,
            dim: super::text::EMBEDDING_DIM,
            max_image_attempts: 4,
            text_busy_poll_ms: 5_000,
            max_wait_ms: 30 * 60_000,
        }
    }
}

/// 队列里的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    item: EmbedItem,
    source: String,
    text: String,
    attempts: i32,
}

/// 一个窗口要宿主去取图的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverPick {
    pub key: String,
    /// 送进文本+图那张图的文字（`title: … | text: …`）。
    pub text: String,
    /// 封面**原地址**（宿主按 [`super::cover::CACHE_WIDTHS`] 各档改写成缩略图地址去试）。
    pub cover_url: String,
}

/// 一个窗口的计划。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverPlan {
    pub picks: Vec<CoverPick>,
}

/// 一条算出来的向量（窗口里成功的那几条）。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverVector {
    pub key: String,
    pub vec: Vec<f32>,
}

/// 等纯文字的状态（返回给宿主去 `delay`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverWait {
    pub delay_ms: i64,
    /// 盯太久了（默认半小时）：歇着，等下一次叫醒。
    pub stop: bool,
}

/// 等哪种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverWaitKind {
    TextBusy,
}

/// 一轮的输入（宿主每轮回一次：设置 / 前后台 / 纯文字闲不闲 / 带没带图）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFlags {
    /// 「记录浏览行为」+「封面向量」+「用 NPU 算」三个开关都开着。
    pub enabled: bool,
    pub foreground: bool,
    /// 纯文字那条一条都不在算、队列也空。
    pub text_idle: bool,
    /// 这个构建带了两张图（`CoverEmbedder.bundled`）。
    pub bundled: bool,
}

/// 窗口循环的下一步（宿主照着做，做完用对应的 `report_*` 报回来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowStep {
    /// 回循环开头再拉一步（没东西可算 / 这个窗口结束了）。
    Continue,
    /// 等一等（纯文字忙）；`stop` = 当前歇着，等下一次叫醒。
    Wait {
        kind: CoverWaitKind,
        delay_ms: i64,
        stop: bool,
    },
    /// 换句话说 `begin_window(model, store)`，然后 `report_plan(model)`。
    BeginWindow,
    /// 去取这一条的封面图（按 `cover_url` 的各宽度档试缓存），取到没有都 `report_image`。
    LoadImage {
        key: String,
        text: String,
        cover_url: String,
    },
    /// 图都取到了：`prepare()` 两张图，结果 `report_prepare`。
    Prepare,
    /// `embed(inputs)` 一个窗口，结果 `report_embed`。
    Embed,
    /// 写库（`finish_window`）、`close()` 两张会话、记窗口的账，然后 `report_finished`。
    Finish,
    /// 当前结束（开关关了 / 退到后台 / 不可用）。
    Stop,
}

/// 窗口循环内部走到哪一步了（[`CoverScheduler::report_plan`] 之后由 Rust 自己记住）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 在循环开头（判定那四条）。
    Loop,
    /// 已经 `begin_window`，正在等 `report_plan`。
    Begin,
    /// 正在逐条取图。
    Loading,
    Prepare,
    Embed,
    Finish,
}

/// 封面向量的调度状态机。
pub struct CoverScheduler {
    cfg: CoverConfig,
    queue: VecDeque<Pending>,
    queued: HashSet<String>,
    /// 这个进程里已经写进库的 key（省一次查库）。
    done: HashSet<String>,
    /// 当前窗口里还没算的那些（按窗口顺序）。
    window: Vec<Pending>,
    pending_count: i32,
    skipped: i32,
    computed: i32,
    status: CoverStatus,
    /// 这个进程里的失败原因（`None` = 还好）。
    broken: Option<String>,
    /// 当前已经"干等"了多久。
    waited_ms: i64,
    /// 窗口循环走到哪一步了。
    phase: Phase,
    /// `begin_window` 拿到的这一批（`CoverPick`）。
    picks: Vec<CoverPick>,
    /// 这一批里正在取第几条。
    pick_at: usize,
    /// 这一批里成功取到图的那些 key（顺序 = 取到的顺序）。
    keep: Vec<String>,
    /// 这个窗口中途停了（没轮到的要放回队首）。
    aborted: bool,
    /// 最近一个窗口的账。
    last_window: Option<CoverWindowStats>,
    /// 累计平均每条多少毫秒。
    avg_ms: Option<i64>,
    /// 日志行（宿主接走；**不含内容**）。
    logs: Vec<String>,
}

impl CoverScheduler {
    pub fn new(cfg: CoverConfig) -> Self {
        CoverScheduler {
            cfg,
            queue: VecDeque::new(),
            queued: HashSet::new(),
            done: HashSet::new(),
            window: Vec::new(),
            pending_count: 0,
            skipped: 0,
            computed: 0,
            status: CoverStatus::Idle,
            broken: None,
            waited_ms: 0,
            phase: Phase::Loop,
            picks: Vec::new(),
            pick_at: 0,
            keep: Vec::new(),
            aborted: false,
            last_window: None,
            avg_ms: None,
            logs: Vec::new(),
        }
    }

    pub fn config(&self) -> &CoverConfig {
        &self.cfg
    }

    pub fn status(&self) -> &CoverStatus {
        &self.status
    }

    pub fn pending(&self) -> i32 {
        self.pending_count
    }

    pub fn computed(&self) -> i32 {
        self.computed
    }

    pub fn skipped(&self) -> i32 {
        self.skipped
    }

    pub fn is_broken(&self) -> bool {
        self.broken.is_some()
    }

    /// 把攒下来的日志行取走。
    pub fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// 这个组装现在是什么状态（`bundled` = 包里带没带两张图，`enabled` = 「记录浏览行为」）。
    pub fn reset_status(&mut self, bundled: bool, enabled: bool) {
        self.status = if !bundled {
            CoverStatus::NoModel
        } else if !enabled {
            CoverStatus::Off
        } else {
            CoverStatus::Idle
        };
    }

    /// 首页 / 关注来了新卡片：把够条件的排进队列（**只在内存里排队，不建会话、不取图**）。
    pub fn offer(&mut self, items: &[EmbedItem]) -> i32 {
        if items.is_empty() || self.broken.is_some() {
            return 0;
        }
        let mut added = 0;
        for item in items {
            if !has_cover_vector(item) {
                continue;
            }
            if self.queued.contains(&item.key) || self.done.contains(&item.key) {
                continue;
            }
            if self.queue.len() >= self.cfg.max_queued {
                if let Some(old) = self.queue.pop_front() {
                    self.queued.remove(&old.item.key);
                }
            }
            self.queued.insert(item.key.clone());
            self.queue.push_back(Pending {
                item: item.clone(),
                source: EMBEDDING_SOURCE_FEED.to_string(),
                text: cover_embedding_text(item),
                attempts: 0,
            });
            added += 1;
        }
        if added > 0 {
            self.pending_count = self.queue.len() as i32;
        }
        added
    }

    /// 取一个窗口（不超过 `window_size` 条），已经有封面向量的 / 这个进程算过的去掉。
    ///
    /// 返回的 `picks` 是**要宿主取图 + 算**的那几条；空 = 当前没东西可算（宿主据此停下）。
    pub fn begin_window(&mut self, store: &dyn EmbedStore, model: &str) -> CoverPlan {
        self.picks.clear();
        self.pick_at = 0;
        self.keep.clear();
        self.aborted = false;
        if self.broken.is_some() {
            return CoverPlan::default();
        }
        let mut picked: Vec<Pending> = Vec::with_capacity(self.cfg.window_size);
        while picked.len() < self.cfg.window_size {
            match self.queue.pop_front() {
                Some(p) => {
                    self.queued.remove(&p.item.key);
                    picked.push(p);
                }
                None => break,
            }
        }
        if picked.is_empty() {
            self.pending_count = 0;
            return CoverPlan::default();
        }
        let keys: Vec<String> = picked.iter().map(|p| p.item.key.clone()).collect();
        let existing: HashSet<String> = store.embedding_keys(&keys, model).into_iter().collect();
        let todo: Vec<Pending> = picked
            .into_iter()
            .filter(|p| !existing.contains(&p.item.key) && !self.done.contains(&p.item.key))
            .collect();
        self.pending_count = self.queue.len() as i32;
        if todo.is_empty() {
            return CoverPlan::default();
        }
        self.status = CoverStatus::Running {
            done: 0,
            total: todo.len() as i32,
        };
        let picks: Vec<CoverPick> = todo
            .iter()
            .filter_map(|p| {
                cover_image_url(&p.item).map(|url| CoverPick {
                    key: p.item.key.clone(),
                    text: p.text.clone(),
                    cover_url: url.to_string(),
                })
            })
            .collect();
        self.window = todo;
        // 窗口循环靠这份 `picks` 逐条要宿主去取图
        self.picks = picks.clone();
        self.pick_at = 0;
        self.keep.clear();
        self.aborted = false;
        CoverPlan { picks }
    }

    /// 一条取不到封面图：记一次、排到队尾（连续 `max_image_attempts` 次就放弃）。
    ///
    /// 返回 `true` = 又排回去了（下次再试）。
    pub fn image_missing(&mut self, key: &str) -> bool {
        self.skipped += 1;
        let index = match self.window.iter().position(|p| p.item.key == key) {
            Some(i) => i,
            None => return false,
        };
        let mut p = self.window.remove(index);
        p.attempts += 1;
        if p.attempts < self.cfg.max_image_attempts {
            self.queued.insert(p.item.key.clone());
            self.queue.push_back(p);
            true
        } else {
            false
        }
    }

    /// 窗口中途停了（退到后台 / 关掉开关）：**没轮到的放回队首**（保持原来的先后）。
    pub fn abort_window(&mut self) -> i32 {
        let remaining = std::mem::take(&mut self.window);
        let n = remaining.len() as i32;
        for p in remaining.into_iter().rev() {
            self.queued.insert(p.item.key.clone());
            self.queue.push_front(p);
        }
        self.pending_count = self.queue.len() as i32;
        n
    }

    /// 这个窗口整体失败了（QNN 6001 / 编图失败 / 推理返回 null）：封面口径标成本次进程不可用。
    pub fn window_failed(&mut self, reason: &str) {
        self.broken = Some(reason.to_string());
        self.window.clear();
        self.status = CoverStatus::Unavailable {
            reason: reason.to_string(),
        };
        self.pending_count = self.queue.len() as i32;
    }

    /// 窗口算完了：MRL 截断 + f16 落库。`results` 是成功的那几条（`key` 对不上窗口的忽略）。
    ///
    /// 返回写了几条。
    pub fn finish_window(
        &mut self,
        results: &[CoverVector],
        stored_model: &str,
        now: i64,
        store: &dyn EmbedStore,
    ) -> i32 {
        if results.is_empty() {
            self.window.clear();
            self.pending_count = self.queue.len() as i32;
            self.status = CoverStatus::Idle;
            return 0;
        }
        let by_key: std::collections::HashMap<&str, &CoverVector> =
            results.iter().map(|v| (v.key.as_str(), v)).collect();
        let mut rows: Vec<EmbeddingRow> = Vec::with_capacity(self.window.len());
        for p in &self.window {
            let vec = match by_key.get(p.item.key.as_str()) {
                Some(v) => v.vec.as_slice(),
                None => continue,
            };
            let vec = mrl(vec, self.cfg.dim);
            rows.push(EmbeddingRow {
                key: p.item.key.clone(),
                model: stored_model.to_string(),
                dim: self.cfg.dim as i32,
                vec: float_vec_to_half_bytes(&vec),
                created_at: now,
                source: p.source.clone(),
            });
        }
        let total = self.window.len() as i32;
        self.window.clear();
        if !rows.is_empty() {
            store.insert_embeddings(&rows);
            for row in &rows {
                self.done.insert(row.key.clone());
            }
            self.computed += rows.len() as i32;
            self.status = CoverStatus::Running {
                done: total,
                total,
            };
        }
        self.pending_count = self.queue.len() as i32;
        rows.len() as i32
    }

    /// 等一等（纯文字忙）：累计"干等"的时长，超了就让宿主歇着。
    pub fn wait_tick(&mut self, kind: CoverWaitKind) -> CoverWait {
        let step = match kind {
            CoverWaitKind::TextBusy => self.cfg.text_busy_poll_ms,
        };
        self.waited_ms += step;
        CoverWait {
            delay_ms: step,
            stop: self.waited_ms >= self.cfg.max_wait_ms,
        }
    }

    /// 当前真的开始算了：把"干等"的累计清零。
    pub fn clear_wait(&mut self) {
        self.waited_ms = 0;
    }

    /// 取消当前、清空队列（回前台后由 `offer` 重新开始）。
    pub fn cancel(&mut self) {
        self.queue.clear();
        self.queued.clear();
        self.window.clear();
        self.pending_count = 0;
    }

    /// 清空统计（设置里"清除行为数据"之后调；库里的行由宿主清）。
    pub fn reset_stats(&mut self) {
        self.computed = 0;
        self.skipped = 0;
        self.done.clear();
        // Kotlin 的 `resetStats` 还清这几样
        self.avg_ms = None;
        self.last_window = None;
        self.logs.clear();
    }

    /// 窗口的账（宿主把 `CoverEmbedder.lastWindow` 传进来时用它算"每条摊多少毫秒"）。
    pub fn ms_per_item(stats: &CoverWindowStats) -> i64 {
        stats.ms_per_item()
    }

    // ==================================================================
    // 窗口循环：顺序和分支都在这里
    // ==================================================================

    /// 一步的输入（宿主每轮把这几件事告诉 Rust：设置 / 前后台 / 纯文字闲不闲）。
    ///
    /// 为什么要一次全给：循环开头的**判定顺序**是行为的一部分（先看开关、再看前后台、再看 broken、
    /// 再看队列空不空、最后才看纯文字），顺序变了状态就不一样。
    pub fn window_step(&mut self, f: WindowFlags) -> WindowStep {
        match self.phase {
            // 已经 `begin_window`、正在等宿主 `report_plan`：别重复开窗口
            Phase::Begin => return WindowStep::BeginWindow,
            Phase::Loading => return self.next_image_step(f),
            Phase::Prepare => return WindowStep::Prepare,
            Phase::Embed => return WindowStep::Embed,
            Phase::Finish => return WindowStep::Finish,
            Phase::Loop => {}
        }
        // 循环开头的四条检查（顺序照 Kotlin `runWorker`）
        if !f.enabled {
            self.cancel();
            self.phase = Phase::Loop;
            self.status = CoverStatus::Off;
            return WindowStep::Stop;
        }
        if !f.foreground {
            self.cancel();
            self.phase = Phase::Loop;
            return WindowStep::Stop;
        }
        if self.broken.is_some() {
            self.status = CoverStatus::Unavailable {
                reason: self.broken.clone().unwrap_or_default(),
            };
            return WindowStep::Stop;
        }
        if self.pending() == 0 {
            self.status = CoverStatus::Idle;
            self.pending_count = 0;
            return WindowStep::Stop;
        }
        if !f.text_idle {
            // 纯文字优先：它在算（或者队列里还有），等等再看（同样有上限）
            let wait = self.wait_tick(CoverWaitKind::TextBusy);
            return WindowStep::Wait {
                kind: CoverWaitKind::TextBusy,
                delay_ms: wait.delay_ms,
                stop: wait.stop,
            };
        }
        self.clear_wait();
        self.phase = Phase::Begin;
        WindowStep::BeginWindow
    }

    /// 宿主取完一个窗口（调 [`Self::begin_window`] 之后）报一声。
    ///
    /// 返回下一步：没有可算的 → [`WindowStep::Continue`]（回循环开头）；否则第一条取图。
    pub fn report_plan(&mut self, model: &str) -> WindowStep {
        let _ = model;
        self.keep.clear();
        self.pick_at = 0;
        self.aborted = false;
        if self.picks.is_empty() {
            self.phase = Phase::Loop;
            self.status = CoverStatus::Idle;
            self.pending_count = self.queue.len() as i32;
            return WindowStep::Continue;
        }
        self.status = CoverStatus::Running {
            done: 0,
            total: self.picks.len() as i32,
        };
        self.phase = Phase::Loading;
        self.next_image_step(WindowFlags {
            enabled: true,
            foreground: true,
            text_idle: true,
            bundled: true,
        })
    }

    /// 往回退一步（[`Self::report_plan`] 里"图都取完了"那一支要回循环开头）。
    fn next_image_step(&mut self, f: WindowFlags) -> WindowStep {
        // 中途退到后台 / 关掉开关：**没轮到的放回队首**，别在窗口里悄悄丢掉
        if self.aborted || !f.enabled || !f.foreground {
            self.aborted = true;
            // 一直走到这一批的末尾才开始 abort（一次 abort_window 就把整批放回去）
            if self.pick_at < self.picks.len() {
                self.pick_at = self.picks.len();
                return self.next_image_step(f);
            }
            self.phase = Phase::Loop;
            let back = self.abort_window();
            self.logs.push(format!("[封面] 窗口中途停了，{back} 条放回队列"));
            return WindowStep::Continue;
        }
        if self.pick_at >= self.picks.len() {
            self.phase = Phase::Prepare;
            if self.keep.is_empty() {
                // 这个窗口一条图都没取到（全是离线 / 计费网络 / 404）：回到循环，让重试计数往下走
                self.phase = Phase::Loop;
                self.status = CoverStatus::Idle;
                self.pending_count = self.queue.len() as i32;
                return WindowStep::Continue;
            }
            return WindowStep::Prepare;
        }
        let p = self.picks[self.pick_at].clone();
        WindowStep::LoadImage {
            key: p.key,
            text: p.text,
            cover_url: p.cover_url,
        }
    }

    /// 这一条取到图了吗（`found` = false 就把它的重试次数 +1、排回队尾）。
    pub fn report_image(&mut self, flags: WindowFlags, key: &str, found: bool) -> WindowStep {
        if found {
            self.keep.push(key.to_string());
        } else {
            self.image_missing(key);
        }
        self.pick_at += 1;
        self.next_image_step(flags)
    }

    /// `prepare()` 的结果（`failure` = `Some` 就是失败原因）。
    pub fn report_prepare(&mut self, failure: Option<String>) -> WindowStep {
        if let Some(reason) = failure {
            self.mark_broken(&reason);
            self.phase = Phase::Loop;
            return WindowStep::Stop;
        }
        self.phase = Phase::Embed;
        WindowStep::Embed
    }

    /// `embed()` 的结果：`outer_failed` = 整个窗口失败（QNN 6001 那种）；`returned` = 拿到几条向量。
    pub fn report_embed(&mut self, outer_failed: bool, returned: i32) -> WindowStep {
        if outer_failed {
            // 把封面标成本次进程不可用（不再每批重试），纯文字照旧
            self.mark_broken("封面向量这一轮算失败（下次启动再试）");
            self.phase = Phase::Loop;
            return WindowStep::Stop;
        }
        if returned != self.keep.len() as i32 {
            self.logs.push(format!(
                "[封面] 返回 {returned} 条，和送进去的 {} 条对不上，跳过",
                self.keep.len()
            ));
        }
        self.phase = Phase::Finish;
        WindowStep::Finish
    }

    /// 宿主写完库 / 关掉两张会话了：回循环开头。
    pub fn report_finished(&mut self) -> WindowStep {
        self.phase = Phase::Loop;
        self.status = self.status.clone();
        WindowStep::Continue
    }

    /// 当前失败（`window_failed` 的便利包装；同时把阶段复位）。
    fn mark_broken(&mut self, reason: &str) {
        self.window_failed(reason);
        self.phase = Phase::Loop;
    }

    /// 记一个窗口的账：返回**累计平均每条多少毫秒**。
    ///
    /// 口径照 Kotlin：`total = avg * (computed - count) + stats.total_ms`，
    /// `avg = total / computed`（`computed` 是这个进程算成的条数，**含**这个窗口）。
    pub fn observe_window(&mut self, stats: &CoverWindowStats) -> Option<i64> {
        self.last_window = Some(stats.clone());
        let computed = self.computed;
        if computed <= 0 {
            return None;
        }
        let previous = self.avg_ms.unwrap_or(0) * i64::from(computed - stats.count);
        let total = previous + stats.total_ms;
        let avg = total / i64::from(computed);
        self.avg_ms = Some(avg);
        Some(avg)
    }

    /// 最近一个窗口的账。
    pub fn last_window(&self) -> Option<&CoverWindowStats> {
        self.last_window.as_ref()
    }

    /// 累计平均每条多少毫秒。
    pub fn avg_ms(&self) -> Option<i64> {
        self.avg_ms
    }

    /// 这个窗口里成功取到图的那些 key（宿主按这个顺序挑向量）。
    pub fn window_keys(&self) -> &[String] {
        &self.keep
    }
}
