//! 本地向量（调度 / 画像 / 行为信号 / 封面向量 / 行为导出）的 UniFFI 边界。
//!
//! 这里只做三件事：
//!
//! 1. **过边界的 record**（前缀 `FfiEmbed*` / `FfiTaste*` / `FfiCover*`）；
//! 2. **两个宿主能力**：`FfiEmbedStore`（查已有向量 / 查收藏 / 写一批行——生产就是 `RustLibrary`，
//!    测试是 `MemoryLibrary`）和"分词由宿主调"（[`FfiEmbedScheduler::next_step`] 的 `Prepare` 那一步：
//!    宿主拿 `truncate_to` / 要数 token 的文字，做完 `supply_prep` 交回来）；
//! 3. **状态机对象**（`FfiEmbedScheduler` / `FfiCoverScheduler`）：宿主拉一步、Rust 回一步。
//!
//! 为什么 `next_step` 是"宿主拉"而不是 Rust 回调宿主的 suspend 函数：分词和推理在安卓上都是
//! `withContext(Dispatchers.Default)` 的 suspend 调用，Rust 的同步回调里调不了它们。拆成
//! "Rust 说下一步做什么 → 宿主做完把一整批结果交回来"之后，每次过边界的都是**一整批**（文字 / 向量），
//! 不是一条一次。
//!
//! 时间（`now`）、模型标识、包里那几张图的长度、前后台 / 充电状态**全由宿主注入**：`.so` 里不读时钟、
//! 不取随机数、没有后台线程（和这一层其它模块一个规矩）。

use std::sync::{Arc, Mutex, PoisonError};

use feedhub_core::embed as core;

// ===========================================================================
// 错误
// ===========================================================================

/// 这一层的错误（现在只有一个来源：像素尺寸不对）。字段不叫 `message`（会和 `Exception.message` 撞）。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiEmbedError {
    /// 参数不对（`coverPatchify` 的边长 / 像素数）。Kotlin 侧折成 `IllegalArgumentException`
    /// （搬进 Rust 之前 `require()` 抛的就是它）。
    #[error("{detail}")]
    Invalid { detail: String },
}

// ===========================================================================
// 过边界的 record：卡片 / 向量 / 信号 / 画像
// ===========================================================================

/// 一条卡片的投影（算向量 / 信号 / 封面 / 导出只用到这些字段；比 `FeedItem` 小得多）。
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedItem {
    pub key: String,
    pub id: String,
    pub platform: String,
    /// `ItemKind.name`（`VIDEO` / `ARTICLE` / …）。
    pub kind: String,
    pub published_at: Option<i64>,
    pub language: Option<String>,
    pub author_name: Option<String>,
    pub adult: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub community: Option<String>,
    pub reason: Option<String>,
    pub author_id: Option<String>,
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    /// 视频封面的原地址。
    pub video_cover: Option<String>,
    /// 正文里第一张图的地址。
    pub first_image: Option<String>,
}

impl From<FfiEmbedItem> for core::EmbedItem {
    fn from(v: FfiEmbedItem) -> Self {
        core::EmbedItem {
            key: v.key,
            id: v.id,
            platform: v.platform,
            kind: v.kind,
            published_at: v.published_at,
            language: v.language,
            author_name: v.author_name,
            adult: v.adult,
            title: v.title,
            summary: v.summary,
            community: v.community,
            reason: v.reason,
            author_id: v.author_id,
            repost_title: v.repost_title,
            repost_summary: v.repost_summary,
            video_cover: v.video_cover,
            first_image: v.first_image,
        }
    }
}

impl From<core::EmbedItem> for FfiEmbedItem {
    fn from(v: core::EmbedItem) -> Self {
        FfiEmbedItem {
            key: v.key,
            id: v.id,
            platform: v.platform,
            kind: v.kind,
            published_at: v.published_at,
            language: v.language,
            author_name: v.author_name,
            adult: v.adult,
            title: v.title,
            summary: v.summary,
            community: v.community,
            reason: v.reason,
            author_id: v.author_id,
            repost_title: v.repost_title,
            repost_summary: v.repost_summary,
            video_cover: v.video_cover,
            first_image: v.first_image,
        }
    }
}

fn to_core_items(items: Vec<FfiEmbedItem>) -> Vec<core::EmbedItem> {
    items.into_iter().map(Into::into).collect()
}

/// 一条卡片向量（`item_embedding` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedRow {
    pub key: String,
    pub model: String,
    pub dim: i32,
    /// f16 小端原始字节（2 字节一个分量）。
    pub vec: Vec<u8>,
    pub created_at: i64,
    pub source: String,
}

impl From<core::EmbeddingRow> for FfiEmbedRow {
    fn from(r: core::EmbeddingRow) -> Self {
        FfiEmbedRow {
            key: r.key,
            model: r.model,
            dim: r.dim,
            vec: r.vec,
            created_at: r.created_at,
            source: r.source,
        }
    }
}

impl From<FfiEmbedRow> for core::EmbeddingRow {
    fn from(r: FfiEmbedRow) -> Self {
        core::EmbeddingRow {
            key: r.key,
            model: r.model,
            dim: r.dim,
            vec: r.vec,
            created_at: r.created_at,
            source: r.source,
        }
    }
}

/// 一条补算候选（宿主从 `Library.embeddingBackfill` 里取出来交回来）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedBackfill {
    pub item: FfiEmbedItem,
    pub source: String,
    pub at: i64,
}

/// 调度参数（默认值 = 线上那一套；宿主可以按构建覆盖）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedConfig {
    pub batch_size: i32,
    pub dim: i32,
    pub backfill_chunk: i32,
    pub backfill_window_ms: i64,
    pub detail_max_tokens: i32,
    pub detail_max_chars: i32,
    pub max_summary_chars: i32,
}

impl Default for FfiEmbedConfig {
    fn default() -> Self {
        let cfg = core::EmbedConfig::default();
        FfiEmbedConfig {
            batch_size: cfg.batch_size as i32,
            dim: cfg.dim as i32,
            backfill_chunk: cfg.backfill_chunk,
            backfill_window_ms: cfg.backfill_window_ms,
            detail_max_tokens: cfg.detail_max_tokens,
            detail_max_chars: cfg.detail_max_chars as i32,
            max_summary_chars: cfg.max_summary_chars as i32,
        }
    }
}

impl From<FfiEmbedConfig> for core::EmbedConfig {
    fn from(c: FfiEmbedConfig) -> Self {
        core::EmbedConfig {
            batch_size: c.batch_size.max(0) as usize,
            dim: c.dim.max(0) as usize,
            backfill_chunk: c.backfill_chunk,
            backfill_window_ms: c.backfill_window_ms,
            detail_max_tokens: c.detail_max_tokens,
            detail_max_chars: c.detail_max_chars.max(0) as usize,
            max_summary_chars: c.max_summary_chars.max(0) as usize,
        }
    }
}

/// 宿主拉下一步的入参（现在几点、当前模型、包里带的那几张固定长度图）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedNext {
    pub now: i64,
    pub model: String,
    pub graph_lengths: Vec<i32>,
}

/// 下一步做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiEmbedStepKind {
    /// 没东西可算：当前结束。
    Empty,
    /// 请宿主取一块补算候选，`supply_backfill` 交回来。
    NeedBackfill,
    /// 请宿主按 `truncate_to` 截断（> 0）并数 token（`texts` 等长等序），`supply_prep` 交回来。
    Prepare,
    /// 这一批可以算了：`texts` 等序送进 `Embedder.embed(texts, max_tokens)`，算完 `finish_batch`。
    Batch,
}

/// 一步的完整形状（不用带数据的 enum，UniFFI 那边映射成 record 更稳）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedStep {
    pub kind: FfiEmbedStepKind,
    /// `Prepare` / `Batch` 的文字（等长等序）。
    pub texts: Vec<String>,
    /// `Prepare` 每条要不要按 token 截断（> 0 = 截到这个长度；0 = 只数 token）。
    pub truncate_to: Vec<i32>,
    /// `Batch` 这批里最长那条的 token 数（`i32::MAX` = 不知道 / 不分组）。
    pub max_tokens: i32,
    /// `Batch` 几条。
    pub count: i32,
    /// `NeedBackfill` 的模型 / 时间窗口 / 条数上限。
    pub model: String,
    pub since_ms: i64,
    pub limit: i32,
}

impl FfiEmbedStep {
    fn empty() -> Self {
        FfiEmbedStep {
            kind: FfiEmbedStepKind::Empty,
            texts: Vec::new(),
            truncate_to: Vec::new(),
            max_tokens: 0,
            count: 0,
            model: String::new(),
            since_ms: 0,
            limit: 0,
        }
    }
}

impl From<core::Step> for FfiEmbedStep {
    fn from(step: core::Step) -> Self {
        let mut out = FfiEmbedStep::empty();
        match step {
            core::Step::Empty => {}
            core::Step::NeedBackfill {
                model,
                since_ms,
                limit,
            } => {
                out.kind = FfiEmbedStepKind::NeedBackfill;
                out.model = model;
                out.since_ms = since_ms;
                out.limit = limit;
            }
            core::Step::Prepare { texts, truncate_to } => {
                out.kind = FfiEmbedStepKind::Prepare;
                out.texts = texts;
                out.truncate_to = truncate_to;
            }
            core::Step::Batch {
                texts,
                max_tokens,
                count,
            } => {
                out.kind = FfiEmbedStepKind::Batch;
                out.texts = texts;
                out.max_tokens = max_tokens;
                out.count = count;
            }
        }
        out
    }
}

/// 一批算完的账（来源拆分；日志 / 统计用）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedOutcome {
    pub count: i32,
    pub from_detail: i32,
    pub from_backfill: i32,
    pub kind: String,
    /// 这一批写进库的模型标识。
    pub model: String,
    /// 每条写进库的来源（和写进去的行等长等序）。
    pub sources: Vec<String>,
}

// ===========================================================================
// 宿主能力：库操作（同步）
// ===========================================================================

/// 调度要宿主做的库操作。**全是同步的**（Kotlin 那边就是 `Library.embeddingKeys` /
/// `isFavorite` / `insertEmbeddings`）；宿主在 `Dispatchers.IO` 上调 `next_step` / `finish_batch`。
#[uniffi::export(with_foreign)]
pub trait FfiEmbedStore: Send + Sync {
    /// 这批 key 里已经有这个模型向量的那些。
    fn embedding_keys(&self, keys: Vec<String>, model: String) -> Vec<String>;
    /// 这批 key 里哪些现在在收藏里。
    fn is_favorite(&self, keys: Vec<String>) -> Vec<bool>;
    /// 写一批行。
    fn insert_embeddings(&self, rows: Vec<FfiEmbedRow>);
}

/// FFI trait → core trait 的适配器。
struct StoreAdapter(Arc<dyn FfiEmbedStore>);

impl core::EmbedStore for StoreAdapter {
    fn embedding_keys(&self, keys: &[String], model: &str) -> Vec<String> {
        self.0.embedding_keys(keys.to_vec(), model.to_string())
    }

    fn is_favorite(&self, keys: &[String]) -> Vec<bool> {
        self.0.is_favorite(keys.to_vec())
    }

    fn insert_embeddings(&self, rows: &[core::EmbeddingRow]) {
        self.0
            .insert_embeddings(rows.iter().cloned().map(Into::into).collect());
    }
}

// ===========================================================================
// 对象：纯文字向量的调度
// ===========================================================================

/// 纯文字向量的调度状态机（`EmbeddingScheduler.kt` 的 Rust 版）。
#[derive(uniffi::Object)]
pub struct FfiEmbedScheduler {
    inner: Mutex<core::EmbedScheduler>,
}

#[uniffi::export]
impl FfiEmbedScheduler {
    /// 用宿主给的一份假库（**测试 / demo 用**：不碰真库，行只活在这个对象里）。
    #[uniffi::constructor]
    pub fn new_with_memory(config: FfiEmbedConfig) -> Arc<Self> {
        Arc::new(FfiEmbedScheduler {
            inner: Mutex::new(core::EmbedScheduler::new(config.into())),
        })
    }

    /// 新内容到了（首页 / 关注）。关着 / 在后台时宿主**不要调**（连队列都不该进，和以前一样）。
    pub fn offer(&self, items: Vec<FfiEmbedItem>) -> i32 {
        self.lock().offer(&to_core_items(items))
    }

    /// 点开过的条目 + 正文：用正文重算一份、覆盖这条的向量。
    pub fn offer_detail(&self, item: FfiEmbedItem, body: Option<String>) -> bool {
        let item: core::EmbedItem = item.into();
        self.lock().offer_detail(&item, body.as_deref())
    }

    /// 叫醒补算（真正的候选由 [`Step::NeedBackfill`] 要）。
    pub fn request_backfill(&self) {
        self.lock().request_backfill();
    }

    /// 退到后台 / 关开关：清空三个队列（正在算的那批由宿主取消，结果不写库）。
    pub fn cancel(&self) {
        self.lock().cancel();
    }

    /// 清空"这份 model 下已经用正文算过"的记录 + 计数器。
    pub fn reset_stats(&self) {
        self.lock().reset_stats();
    }

    /// 拉下一步。
    pub fn next_step(&self, req: FfiEmbedNext, store: Arc<dyn FfiEmbedStore>) -> FfiEmbedStep {
        let adapter = StoreAdapter(store);
        let inner = self.lock();
        let mut guard = inner;
        guard
            .next_step(
                &core::NextRequest {
                    now: req.now,
                    model: req.model,
                    graph_lengths: req.graph_lengths,
                },
                &adapter,
            )
            .into()
    }

    /// 宿主做完 `Prepare` 之后交回来（`texts` / `tokens` 和请求等长等序；`tokens < 0` = 数不出来）。
    pub fn supply_prep(&self, texts: Vec<String>, tokens: Vec<i32>) {
        self.lock().supply_prep(&texts, &tokens);
    }

    /// 宿主取回来的补算候选。
    pub fn supply_backfill(&self, candidates: Vec<FfiEmbedBackfill>) -> i32 {
        let rows: Vec<core::BackfillCandidate> = candidates
            .into_iter()
            .map(|c| core::BackfillCandidate {
                item: c.item.into(),
                source: c.source,
                at: c.at,
            })
            .collect();
        self.lock().supply_backfill(&rows)
    }

    /// 这一批算完了：`vectors` 是拼平的一批 f32（`count * vec_dim`），`stored_model` 是**实际**写进库的标识。
    ///
    /// 返回 `None` = 这一批不算数（条数对不上 / 没有在等的一批），不写库。
    pub fn finish_batch(
        &self,
        vectors: Vec<f32>,
        vec_dim: i32,
        stored_model: String,
        now: i64,
        store: Arc<dyn FfiEmbedStore>,
    ) -> Option<FfiEmbedOutcome> {
        let adapter = StoreAdapter(store);
        let mut guard = self.lock();
        guard
            .finish_batch(&vectors, vec_dim, &stored_model, now, &adapter)
            .map(|(outcome, _rows)| FfiEmbedOutcome {
                count: outcome.count,
                from_detail: outcome.from_detail,
                from_backfill: outcome.from_backfill,
                kind: outcome.kind,
                model: outcome.model,
                sources: outcome.sources,
            })
    }

    /// 这一批算失败（`embed` 返回 null / 抛异常 / 条数对不上）：丢掉，不写库。
    pub fn fail_batch(&self) {
        self.lock().fail_batch();
    }

    /// 现在有没有批在算或者排队（封面向量那条用它让路）。
    pub fn busy(&self) -> bool {
        self.lock().busy()
    }

    pub fn queued_count(&self) -> i32 {
        self.lock().queued_count()
    }

    pub fn computed(&self) -> i32 {
        self.lock().computed()
    }

    pub fn backfilled(&self) -> i32 {
        self.lock().backfilled()
    }

    pub fn detailed(&self) -> i32 {
        self.lock().detailed()
    }

    pub fn last_batch_size(&self) -> i32 {
        self.lock().last_batch_size()
    }
}

impl FfiEmbedScheduler {
    fn lock(&self) -> std::sync::MutexGuard<'_, core::EmbedScheduler> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// ===========================================================================
// 对象：封面向量的调度
// ===========================================================================

/// 封面向量的调度参数。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverConfig {
    pub window_size: i32,
    pub max_queued: i32,
    pub dim: i32,
    pub max_image_attempts: i32,
    pub text_busy_poll_ms: i64,
    pub max_wait_ms: i64,
}

impl Default for FfiCoverConfig {
    fn default() -> Self {
        let cfg = core::CoverConfig::default();
        FfiCoverConfig {
            window_size: cfg.window_size as i32,
            max_queued: cfg.max_queued as i32,
            dim: cfg.dim as i32,
            max_image_attempts: cfg.max_image_attempts,
            text_busy_poll_ms: cfg.text_busy_poll_ms,
            max_wait_ms: cfg.max_wait_ms,
        }
    }
}

impl From<FfiCoverConfig> for core::CoverConfig {
    fn from(c: FfiCoverConfig) -> Self {
        core::CoverConfig {
            window_size: c.window_size.max(0) as usize,
            max_queued: c.max_queued.max(0) as usize,
            dim: c.dim.max(0) as usize,
            max_image_attempts: c.max_image_attempts,
            text_busy_poll_ms: c.text_busy_poll_ms,
            max_wait_ms: c.max_wait_ms,
        }
    }
}

/// 封面向量的状态（设置页那行；`label` 由 Rust 给，措辞只有一份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCoverStatusKind {
    NoModel,
    Off,
    Idle,
    Running,
    Unavailable,
}

/// 状态 + 附带的数字 / 原因。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverStatus {
    pub kind: FfiCoverStatusKind,
    pub label: String,
    pub done: i32,
    pub total: i32,
    pub reason: Option<String>,
}

impl From<core::CoverStatus> for FfiCoverStatus {
    fn from(s: core::CoverStatus) -> Self {
        let label = s.label().to_string();
        match s {
            core::CoverStatus::NoModel => FfiCoverStatus {
                kind: FfiCoverStatusKind::NoModel,
                label,
                done: 0,
                total: 0,
                reason: None,
            },
            core::CoverStatus::Off => FfiCoverStatus {
                kind: FfiCoverStatusKind::Off,
                label,
                done: 0,
                total: 0,
                reason: None,
            },
            core::CoverStatus::Idle => FfiCoverStatus {
                kind: FfiCoverStatusKind::Idle,
                label,
                done: 0,
                total: 0,
                reason: None,
            },
            core::CoverStatus::Running { done, total } => FfiCoverStatus {
                kind: FfiCoverStatusKind::Running,
                label,
                done,
                total,
                reason: None,
            },
            core::CoverStatus::Unavailable { reason } => FfiCoverStatus {
                kind: FfiCoverStatusKind::Unavailable,
                label,
                done: 0,
                total: 0,
                reason: Some(reason),
            },
        }
    }
}

/// 一个窗口要宿主去取图的一条。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverPick {
    pub key: String,
    pub text: String,
    /// 封面**原地址**（宿主按各档宽度改写成缩略图地址去试）。
    pub cover_url: String,
}

/// 一条算出来的向量（窗口里成功的那几条）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCoverVector {
    pub key: String,
    pub vec: Vec<f32>,
}

/// 等纯文字：宿主按 `delay_ms` 等，`stop` 就歇着。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverWait {
    pub delay_ms: i64,
    pub stop: bool,
}

/// 等哪种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCoverWaitKind {
    TextBusy,
}

/// 一轮的输入（宿主每轮回一次）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiWindowFlags {
    pub enabled: bool,
    pub foreground: bool,
    pub text_idle: bool,
    pub bundled: bool,
}

impl From<FfiWindowFlags> for core::WindowFlags {
    fn from(f: FfiWindowFlags) -> Self {
        core::WindowFlags {
            enabled: f.enabled,
            foreground: f.foreground,
            text_idle: f.text_idle,
            bundled: f.bundled,
        }
    }
}

/// 窗口循环的一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiWindowStepKind {
    Continue,
    Wait,
    BeginWindow,
    LoadImage,
    Prepare,
    Embed,
    Finish,
    Stop,
}

/// 窗口循环的下一步（`kind` + 附带的东西）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWindowStep {
    pub kind: FfiWindowStepKind,
    /// `Wait` 等多久。
    pub delay_ms: i64,
    /// `Wait`：盯太久了，歇着等下一次叫醒。
    pub stop: bool,
    /// `LoadImage` 这是哪一条。
    pub key: String,
    /// `LoadImage` 送进文本+图的文字。
    pub text: String,
    /// `LoadImage` 封面**原地址**。
    pub cover_url: String,
    /// `Wait` 等的是哪种（现在只有 `text_busy`）。
    pub wait_kind: String,
}

impl From<core::WindowStep> for FfiWindowStep {
    fn from(s: core::WindowStep) -> Self {
        let none = |kind| FfiWindowStep {
            kind,
            delay_ms: 0,
            stop: false,
            key: String::new(),
            text: String::new(),
            cover_url: String::new(),
            wait_kind: String::new(),
        };
        match s {
            core::WindowStep::Continue => none(FfiWindowStepKind::Continue),
            core::WindowStep::Wait {
                kind,
                delay_ms,
                stop,
            } => FfiWindowStep {
                kind: FfiWindowStepKind::Wait,
                delay_ms,
                stop,
                key: String::new(),
                text: String::new(),
                cover_url: String::new(),
                wait_kind: match kind {
                    core::CoverWaitKind::TextBusy => "text_busy".to_string(),
                },
            },
            core::WindowStep::BeginWindow => none(FfiWindowStepKind::BeginWindow),
            core::WindowStep::LoadImage {
                key,
                text,
                cover_url,
            } => FfiWindowStep {
                kind: FfiWindowStepKind::LoadImage,
                delay_ms: 0,
                stop: false,
                key,
                text,
                cover_url,
                wait_kind: String::new(),
            },
            core::WindowStep::Prepare => none(FfiWindowStepKind::Prepare),
            core::WindowStep::Embed => none(FfiWindowStepKind::Embed),
            core::WindowStep::Finish => none(FfiWindowStepKind::Finish),
            core::WindowStep::Stop => none(FfiWindowStepKind::Stop),
        }
    }
}

/// 一个窗口的账。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverWindowStats {
    pub count: i32,
    pub vision_images: i32,
    pub vision_ms: i64,
    pub text_batches: i32,
    pub text_ms: i64,
    pub total_ms: i64,
    pub compiled: i32,
}

impl From<FfiCoverWindowStats> for core::CoverWindowStats {
    fn from(s: FfiCoverWindowStats) -> Self {
        core::CoverWindowStats {
            count: s.count,
            vision_images: s.vision_images,
            vision_ms: s.vision_ms,
            text_batches: s.text_batches,
            text_ms: s.text_ms,
            total_ms: s.total_ms,
            compiled: s.compiled,
        }
    }
}

/// 封面向量的调度状态机（`CoverScheduler.kt` 的 Rust 版）。
#[derive(uniffi::Object)]
pub struct FfiCoverScheduler {
    inner: Mutex<core::CoverScheduler>,
}

#[uniffi::export]
impl FfiCoverScheduler {
    #[uniffi::constructor]
    pub fn new(config: FfiCoverConfig) -> Arc<Self> {
        Arc::new(FfiCoverScheduler {
            inner: Mutex::new(core::CoverScheduler::new(config.into())),
        })
    }

    /// 首页 / 关注来了新卡片（够不够格由 Rust 判：平台表 + 有图 + 不是 18+ + 有文字）。
    pub fn offer(&self, items: Vec<FfiEmbedItem>) -> i32 {
        self.lock().offer(&to_core_items(items))
    }

    /// 取一个窗口（不超过 windowSize 条），已经有封面向量的 / 这个进程算过的去掉。
    pub fn begin_window(&self, model: String, store: Arc<dyn FfiEmbedStore>) -> Vec<FfiCoverPick> {
        let adapter = StoreAdapter(store);
        let mut guard = self.lock();
        guard
            .begin_window(&adapter, &model)
            .picks
            .into_iter()
            .map(|p| FfiCoverPick {
                key: p.key,
                text: p.text,
                cover_url: p.cover_url,
            })
            .collect()
    }

    /// 一条取不到封面图：记一次、排回队尾（连续 `max_image_attempts` 次就放弃）。
    pub fn image_missing(&self, key: String) -> bool {
        self.lock().image_missing(&key)
    }

    /// 窗口中途停了（退到后台 / 关开关）：没轮到的放回队首，返回几条。
    pub fn abort_window(&self) -> i32 {
        self.lock().abort_window()
    }

    /// 这个窗口整体失败（QNN 6001 / 编图失败 / 推理返回 null）：封面口径标成本次进程不可用。
    pub fn window_failed(&self, reason: String) {
        self.lock().window_failed(&reason);
    }

    /// 窗口算完了：MRL 截断 + f16 落库，返回写了几条。
    pub fn finish_window(
        &self,
        results: Vec<FfiCoverVector>,
        stored_model: String,
        now: i64,
        store: Arc<dyn FfiEmbedStore>,
    ) -> i32 {
        let rows: Vec<core::CoverVector> = results
            .into_iter()
            .map(|v| core::CoverVector {
                key: v.key,
                vec: v.vec,
            })
            .collect();
        let adapter = StoreAdapter(store);
        let mut guard = self.lock();
        guard.finish_window(&rows, &stored_model, now, &adapter)
    }

    /// 等一等（纯文字忙）。
    pub fn wait_tick(&self, kind: FfiCoverWaitKind) -> FfiCoverWait {
        let wait = self.lock().wait_tick(match kind {
            FfiCoverWaitKind::TextBusy => core::CoverWaitKind::TextBusy,
        });
        FfiCoverWait {
            delay_ms: wait.delay_ms,
            stop: wait.stop,
        }
    }

    // -------------------------------------------------------- 窗口循环

    /// 拉一轮的下一步（`flags` = 设置 / 前后台 / 充电 / 纯文字闲不闲）。
    pub fn window_step(&self, flags: FfiWindowFlags) -> FfiWindowStep {
        self.lock().window_step(flags.into()).into()
    }

    /// `begin_window` 之后报一声：返回"第一条取图"或"回循环开头"。
    pub fn report_plan(&self, model: String) -> FfiWindowStep {
        self.lock().report_plan(&model).into()
    }

    /// 这一条图取到没有。
    pub fn report_image(&self, flags: FfiWindowFlags, key: String, found: bool) -> FfiWindowStep {
        self.lock().report_image(flags.into(), &key, found).into()
    }

    /// `prepare()` 的结果。
    pub fn report_prepare(&self, failure: Option<String>) -> FfiWindowStep {
        self.lock().report_prepare(failure).into()
    }

    /// `embed()` 的结果（`outer_failed` = 整个窗口失败；`returned` = 拿到几条）。
    pub fn report_embed(&self, outer_failed: bool, returned: i32) -> FfiWindowStep {
        self.lock().report_embed(outer_failed, returned).into()
    }

    /// 写库 / 关会话都做完了：回循环开头。
    pub fn report_finished(&self) -> FfiWindowStep {
        self.lock().report_finished().into()
    }

    /// 这个窗口里成功取到图的那些 key（按取到的顺序）。
    pub fn window_keys(&self) -> Vec<String> {
        self.lock().window_keys().to_vec()
    }

    /// 记一个窗口的账，返回累计平均每条多少毫秒（`None` = 还没算过）。
    pub fn observe_window(&self, stats: FfiCoverWindowStats) -> Option<i64> {
        let stats: core::CoverWindowStats = stats.into();
        self.lock().observe_window(&stats)
    }

    /// 最近一个窗口的账。
    pub fn last_window(&self) -> Option<FfiCoverWindowStats> {
        self.lock().last_window().map(|s| FfiCoverWindowStats {
            count: s.count,
            vision_images: s.vision_images,
            vision_ms: s.vision_ms,
            text_batches: s.text_batches,
            text_ms: s.text_ms,
            total_ms: s.total_ms,
            compiled: s.compiled,
        })
    }

    /// 累计平均每条多少毫秒。
    pub fn avg_ms(&self) -> Option<i64> {
        self.lock().avg_ms()
    }

    /// 把攒下来的日志行取走。
    pub fn drain_logs(&self) -> Vec<String> {
        self.lock().drain_logs()
    }

    /// 当前真的开始算了：把"干等"的累计清零。
    pub fn clear_wait(&self) {
        self.lock().clear_wait();
    }

    /// 取消当前、清空队列（不动"这个进程算过的"）。
    pub fn cancel(&self) {
        self.lock().cancel();
    }

    /// 清空统计（不含库里已有的行）。
    pub fn reset_stats(&self) {
        self.lock().reset_stats();
    }

    /// 重新算状态（包里带没带两张图 / 「记录浏览行为」开没开）。
    pub fn reset_status(&self, bundled: bool, enabled: bool) {
        self.lock().reset_status(bundled, enabled);
    }

    pub fn status(&self) -> FfiCoverStatus {
        self.lock().status().clone().into()
    }

    pub fn pending(&self) -> i32 {
        self.lock().pending()
    }

    pub fn computed(&self) -> i32 {
        self.lock().computed()
    }

    pub fn skipped(&self) -> i32 {
        self.lock().skipped()
    }

    pub fn is_broken(&self) -> bool {
        self.lock().is_broken()
    }
}

impl FfiCoverScheduler {
    fn lock(&self) -> std::sync::MutexGuard<'_, core::CoverScheduler> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// ===========================================================================
// 对象：CPU / NPU 的后端路由状态机
// ===========================================================================

/// 路由参数（宿主按这个构建的实际情况填；默认值和线上一致）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiRouterConfig {
    /// 包里带的 NPU 图长度（升序；空 = 没带）。
    pub npu_lengths: Vec<i32>,
    /// 这个包的向量空间长度（最长那张；没模型时 0）。
    pub space_len: i32,
    /// 有**能用的** CPU 实现吗（`cpu.available`）。
    pub has_cpu: bool,
    /// 有没有 CPU 实现这个对象（NPU 包是 false）。
    pub cpu_present: bool,
    pub cpu_model: String,
    pub npu_space_model: String,
    pub wait_step_ms: i64,
    pub max_wait_ms: i64,
    pub ready_poll_ms: i64,
}

impl From<FfiRouterConfig> for core::RouterConfig {
    fn from(c: FfiRouterConfig) -> Self {
        core::RouterConfig {
            npu_lengths: c.npu_lengths,
            space_len: c.space_len,
            has_cpu: c.has_cpu,
            cpu_present: c.cpu_present,
            cpu_model: c.cpu_model,
            npu_space_model: c.npu_space_model,
            wait_step_ms: c.wait_step_ms,
            max_wait_ms: c.max_wait_ms,
            ready_poll_ms: c.ready_poll_ms,
        }
    }
}

/// 状态种类（宿主映射回自己的密封接口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiNpuStatusKind {
    NoModel,
    Off,
    Disabled,
    Unavailable,
    Preparing,
    Active,
    Fallback,
}

impl From<core::StatusKind> for FfiNpuStatusKind {
    fn from(k: core::StatusKind) -> Self {
        match k {
            core::StatusKind::NoModel => FfiNpuStatusKind::NoModel,
            core::StatusKind::Off => FfiNpuStatusKind::Off,
            core::StatusKind::Disabled => FfiNpuStatusKind::Disabled,
            core::StatusKind::Unavailable => FfiNpuStatusKind::Unavailable,
            core::StatusKind::Preparing => FfiNpuStatusKind::Preparing,
            core::StatusKind::Active => FfiNpuStatusKind::Active,
            core::StatusKind::Fallback => FfiNpuStatusKind::Fallback,
        }
    }
}

/// 状态 + 它带的数字 / 原因。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiNpuStatus {
    pub kind: FfiNpuStatusKind,
    /// `Preparing` / `Active` 的那句说明（别的为空）。
    pub detail: String,
    /// `Active` 时现在有几张图就绪（升序）。
    pub lens: Vec<i32>,
    /// `Disabled` / `Unavailable` / `Fallback` 的原因（别的为空）。
    pub reason: String,
}

impl From<&core::NpuStatus> for FfiNpuStatus {
    fn from(s: &core::NpuStatus) -> Self {
        let kind = FfiNpuStatusKind::from(s.kind());
        match s {
            core::NpuStatus::Preparing { step } => FfiNpuStatus {
                kind,
                detail: step.clone(),
                lens: Vec::new(),
                reason: String::new(),
            },
            core::NpuStatus::Active { detail, lens } => FfiNpuStatus {
                kind,
                detail: detail.clone(),
                lens: lens.clone(),
                reason: String::new(),
            },
            core::NpuStatus::Disabled { reason }
            | core::NpuStatus::Unavailable { reason }
            | core::NpuStatus::Fallback { reason } => FfiNpuStatus {
                kind,
                detail: String::new(),
                lens: Vec::new(),
                reason: reason.clone(),
            },
            _ => FfiNpuStatus {
                kind,
                detail: String::new(),
                lens: Vec::new(),
                reason: String::new(),
            },
        }
    }
}

/// 准备阶段的下一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPrepareKind {
    Wait,
    Probe,
    Compile,
    Commit,
    Discard,
    Finished,
}

/// 准备阶段的一步（`kind` + 附带的长度 / 等待 / 说明）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPrepareStep {
    pub kind: FfiPrepareKind,
    /// `Probe` / `Compile` / `Commit` / `Discard` 是哪张图（token 数；别的为 0）。
    pub len: i32,
    /// `Wait` 等多久（毫秒）。
    pub delay_ms: i64,
    /// 给人看的一句（"1/2：每条 32 token"；别的为空）。
    pub which: String,
}

impl From<core::PrepareStep> for FfiPrepareStep {
    fn from(s: core::PrepareStep) -> Self {
        match s {
            core::PrepareStep::Wait { delay_ms } => FfiPrepareStep {
                kind: FfiPrepareKind::Wait,
                len: 0,
                delay_ms,
                which: String::new(),
            },
            core::PrepareStep::Probe { len, which } => FfiPrepareStep {
                kind: FfiPrepareKind::Probe,
                len,
                delay_ms: 0,
                which,
            },
            core::PrepareStep::Compile { len, which } => FfiPrepareStep {
                kind: FfiPrepareKind::Compile,
                len,
                delay_ms: 0,
                which,
            },
            core::PrepareStep::Commit { len } => FfiPrepareStep {
                kind: FfiPrepareKind::Commit,
                len,
                delay_ms: 0,
                which: String::new(),
            },
            core::PrepareStep::Discard { len } => FfiPrepareStep {
                kind: FfiPrepareKind::Discard,
                len,
                delay_ms: 0,
                which: String::new(),
            },
            core::PrepareStep::Finished => FfiPrepareStep {
                kind: FfiPrepareKind::Finished,
                len: 0,
                delay_ms: 0,
                which: String::new(),
            },
        }
    }
}

/// 一批的下一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiEmbedRunKind {
    Empty,
    Cpu,
    Npu,
    WaitReady,
}

/// 一批的一步。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedRun {
    pub kind: FfiEmbedRunKind,
    /// `Npu` 用哪张图（token 数）。
    pub len: i32,
    /// `WaitReady` 等多久 / `Cpu` 时的模型标识。
    pub delay_ms: i64,
    /// `Cpu` 写进库的模型标识。
    pub model: String,
    pub seq_len: i32,
    /// `WaitReady`：现在该把准备拉起来吗。
    pub start_prepare: bool,
}

impl From<core::EmbedStep> for FfiEmbedRun {
    fn from(s: core::EmbedStep) -> Self {
        match s {
            core::EmbedStep::Empty => FfiEmbedRun {
                kind: FfiEmbedRunKind::Empty,
                len: 0,
                delay_ms: 0,
                model: String::new(),
                seq_len: 0,
                start_prepare: false,
            },
            core::EmbedStep::UseCpu { model, seq_len } => FfiEmbedRun {
                kind: FfiEmbedRunKind::Cpu,
                len: 0,
                delay_ms: 0,
                model,
                seq_len,
                start_prepare: false,
            },
            core::EmbedStep::RunNpu { len } => FfiEmbedRun {
                kind: FfiEmbedRunKind::Npu,
                len,
                delay_ms: 0,
                model: String::new(),
                seq_len: 0,
                start_prepare: false,
            },
            core::EmbedStep::WaitReady {
                delay_ms,
                start_prepare,
            } => FfiEmbedRun {
                kind: FfiEmbedRunKind::WaitReady,
                len: 0,
                delay_ms,
                model: String::new(),
                seq_len: 0,
                start_prepare,
            },
        }
    }
}

/// 这一批的账。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBatchAccount {
    /// `CPU` / `NPU`。
    pub backend: String,
    pub model: String,
    pub seq_len: i32,
}

/// 分词 / 截断该问谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTokenSourceKind {
    Engine,
    Cpu,
    Factory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiTokenSource {
    pub kind: FfiTokenSourceKind,
    pub len: i32,
}

/// CPU / NPU 的后端路由状态机（`RoutingEmbedder` 的 Rust 版）。
///
/// 宿主那一半是：协程 + `Job`、引擎句柄的 map、探设备 / 编图 / 推理的框架调用、Compose 状态字段。
#[derive(uniffi::Object)]
pub struct FfiRouter {
    inner: Mutex<core::Router>,
}

#[uniffi::export]
impl FfiRouter {
    #[uniffi::constructor]
    pub fn new(config: FfiRouterConfig) -> Arc<Self> {
        Arc::new(FfiRouter {
            inner: Mutex::new(core::Router::new(config.into())),
        })
    }

    // -------------------------------------------------------- 只读

    pub fn status(&self) -> FfiNpuStatus {
        FfiNpuStatus::from(self.lock().status())
    }

    pub fn npu_bundled(&self) -> bool {
        self.lock().npu_bundled()
    }

    pub fn cpu_bundled(&self) -> bool {
        self.lock().cpu_bundled()
    }

    pub fn npu_lengths(&self) -> Vec<i32> {
        self.lock().npu_lengths().to_vec()
    }

    pub fn ready_lengths(&self) -> Vec<i32> {
        self.lock().ready_lengths()
    }

    pub fn available(&self) -> bool {
        self.lock().available()
    }

    pub fn model_id(&self) -> String {
        self.lock().model_id()
    }

    pub fn is_broken(&self) -> bool {
        self.lock().is_broken()
    }

    pub fn requested(&self) -> bool {
        self.lock().requested()
    }

    pub fn last_batch(&self) -> FfiBatchAccount {
        let last = self.lock().last_batch().clone();
        FfiBatchAccount {
            backend: match last.backend {
                core::RouterBackend::Cpu => "CPU".to_string(),
                core::RouterBackend::Npu => "NPU".to_string(),
            },
            model: last.model,
            seq_len: last.seq_len,
        }
    }

    /// 把攒下来的日志行取走（宿主接到自己的日志口；**不含任何内容**）。
    pub fn drain_logs(&self) -> Vec<String> {
        self.lock().drain_logs()
    }

    /// 分词 / 截断该按什么顺序问。
    pub fn token_sources(&self) -> Vec<FfiTokenSource> {
        self.lock()
            .token_sources()
            .into_iter()
            .map(|s| match s {
                core::TokenSource::Engine(len) => FfiTokenSource {
                    kind: FfiTokenSourceKind::Engine,
                    len,
                },
                core::TokenSource::Cpu => FfiTokenSource {
                    kind: FfiTokenSourceKind::Cpu,
                    len: 0,
                },
                core::TokenSource::Factory => FfiTokenSource {
                    kind: FfiTokenSourceKind::Factory,
                    len: 0,
                },
            })
            .collect()
    }

    // -------------------------------------------------------- 开关 / 前后台 / 会话

    /// 「用 NPU 算」变了；返回 `true` = 该把准备协程拉起来。
    pub fn set_requested(&self, on: bool) -> bool {
        self.lock().set_requested(on)
    }

    /// 前后台变了；返回 `true` = 该把准备协程拉起来。
    pub fn on_foreground(&self, active: bool) -> bool {
        self.lock().on_foreground(active)
    }

    /// 释放所有会话（不退开关、不清 broken）。
    pub fn close_engines(&self) {
        self.lock().close_engines();
    }

    /// 分时：封面那侧要独占 QNN 时把纯文字会话放掉。
    pub fn release_sessions(&self) {
        self.lock().release_sessions();
    }

    /// `Embedder.close()`。
    pub fn close_sessions(&self) {
        self.lock().close_sessions();
    }

    // -------------------------------------------------------- 准备

    /// 该不该把准备拉起来（前置条件都在 Rust；`true` = 宿主 `launch`）。
    pub fn try_start_prepare(&self) -> bool {
        self.lock().try_start_prepare()
    }

    /// 取消准备。
    pub fn cancel_prepare(&self) {
        self.lock().cancel_prepare();
    }

    /// 准备协程被取消（`job.cancel()` 之后报一声）。
    pub fn prepare_cancelled(&self) {
        self.lock().prepare_cancelled();
    }

    /// 拉准备阶段的下一步（`charging` = 在不在充电）。
    pub fn prepare_step(&self, charging: bool) -> FfiPrepareStep {
        self.lock().prepare_step(charging).into()
    }

    /// 宿主探完设备了（`reason` 非空 = 失败原因）。
    pub fn report_probe(&self, reason: Option<String>) {
        self.lock().report_probe(reason);
    }

    /// 宿主编完图了；返回下一步（通常是 `Commit` 或 `Discard` / `Finished`）。
    pub fn report_compile(&self, failure: Option<String>, detail: String) -> FfiPrepareStep {
        self.lock().report_compile(failure, detail).into()
    }

    /// 宿主把引擎放进 map 了。
    pub fn commit(&self, len: i32) {
        self.lock().commit(len);
    }

    // -------------------------------------------------------- 推理

    /// 拉这一批的下一步。
    ///
    /// 返回 `Npu` 时宿主算完**必须**调 [Self::report_embed]（成功或失败都要），否则 `in_flight` 会漏账、
    /// 编图永远等不到空闲。
    pub fn embed_step(&self, max_tokens: i32) -> FfiEmbedRun {
        self.lock().embed_step(max_tokens).into()
    }

    pub fn report_embed(&self, len: i32, ran_ok: bool) {
        self.lock().report_embed(len, ran_ok);
    }

    /// CPU 那一批开始算了。
    pub fn begin_cpu(&self) {
        self.lock().begin_cpu();
    }

    /// CPU 那一批算完了。
    pub fn end_cpu(&self) {
        self.lock().end_cpu();
    }

    /// CPU 这一批的账（模型标识 / 图长度 = 0）。
    pub fn account_cpu(&self) -> FfiBatchAccount {
        self.last_batch()
    }
}

impl FfiRouter {
    fn lock(&self) -> std::sync::MutexGuard<'_, core::Router> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// ===========================================================================
// 纯函数：文字 / 数学 / 信号 / 画像 / 封面 / 导出
// ===========================================================================

/// 卡片 → 送去算向量的那段文字（`title: … | text: …`）。
#[uniffi::export]
pub fn ffi_embed_text(item: FfiEmbedItem, max_summary_chars: i32) -> String {
    let item: core::EmbedItem = item.into();
    core::embedding_text(&item, max_summary_chars.max(0) as usize)
}

/// 点开过的条目 + 正文：同一格式，`text` 换成正文开头（正文空就退回卡片文字）。
#[uniffi::export]
pub fn ffi_embed_detail_text(item: FfiEmbedItem, body: Option<String>, max_chars: i32) -> String {
    let item: core::EmbedItem = item.into();
    core::detail_embedding_text(&item, body.as_deref(), max_chars.max(0) as usize)
}

/// 这条卡片值不值得算向量（18+ / 没有文字都不算）。
#[uniffi::export]
pub fn ffi_embed_has_text(item: FfiEmbedItem) -> bool {
    let item: core::EmbedItem = item.into();
    core::has_embedding_text(&item)
}

/// 空白折叠（标题 / 摘要进模型之前）。
#[uniffi::export]
pub fn ffi_embed_collapse(value: String) -> String {
    core::collapse_whitespace(&value)
}

/// MRL 截前 `dim` 维 + 重新 L2 归一化。
#[uniffi::export]
pub fn ffi_embed_mrl(vec: Vec<f32>, dim: i32) -> Vec<f32> {
    core::mrl(&vec, dim.max(0) as usize)
}

/// L2 归一化（原地改，返回同一个向量）。
#[uniffi::export]
pub fn ffi_embed_l2_normalize(vec: Vec<f32>) -> Vec<f32> {
    let mut out = vec;
    core::text::l2_normalize_in_place(&mut out);
    out
}

/// 两个已归一化向量的余弦（长度不一致取公共前缀）。
#[uniffi::export]
pub fn ffi_embed_cosine(a: Vec<f32>, b: Vec<f32>) -> f32 {
    core::text::cosine(&a, &b)
}

/// 一整个向量 → f16 字节（小端，2 字节一个）。
#[uniffi::export]
pub fn ffi_embed_half_bytes(vec: Vec<f32>) -> Vec<u8> {
    core::float_vec_to_half_bytes(&vec)
}

/// f16 字节 → f32（长度不对返回 `None`）。
#[uniffi::export]
pub fn ffi_embed_vec_from_half(bytes: Vec<u8>, dim: i32) -> Option<Vec<f32>> {
    core::text::vec_from_half_bytes(&bytes, dim.max(0) as usize)
}

/// f16 字节里的第 `i` 个分量（小端）→ float（测试 / 单点读取用；整条走 [`ffi_embed_vec_from_half`]）。
#[uniffi::export]
pub fn ffi_embed_half_at(bytes: Vec<u8>, index: i32) -> Option<f32> {
    let at = index.max(0) as usize;
    if bytes.len() < at * 2 + 2 {
        return None;
    }
    Some(core::text::half_byte_to_float(&bytes, at))
}

/// NPU 纯文字向量的空间标识（`embeddinggemma-2-w8a16-htp-l<len>`）。
#[uniffi::export]
pub fn ffi_embed_npu_model_id(seq_len: i32) -> String {
    core::cover::npu_model_id(seq_len.max(0) as usize)
}

/// 封面向量的空间标识（`…-img8x8`）。
#[uniffi::export]
pub fn ffi_embed_cover_model_id(seq_len: i32) -> String {
    core::cover::npu_cover_model_id(seq_len.max(0) as usize)
}

// 但 FFI 上那五个
// `ffi_npu_*` 已经删掉：`FfiRouter` 把它们收进了路由状态机（宿主不再自己拼这几个判定，
// 否则"Rust 说可用 / Kotlin 算出来不可用"这种事又会出现）。

/// 纯文字向量写进 `item_embedding.model` 的默认标识（CPU 的 q4 图）。
#[uniffi::export]
pub fn ffi_embed_default_model() -> String {
    core::EMBEDDING_MODEL_ID.to_string()
}

/// 存多少维（MRL 截断之后）。
#[uniffi::export]
pub fn ffi_embed_dim() -> i32 {
    core::EMBEDDING_DIM as i32
}

/// 详情正文最多算到几个 token。
#[uniffi::export]
pub fn ffi_embed_detail_max_tokens() -> i32 {
    core::DETAIL_MAX_TOKENS as i32
}

/// 三种来源（`feed` / `favorite` / `history`）。
#[uniffi::export]
pub fn ffi_embed_source_feed() -> String {
    core::EMBEDDING_SOURCE_FEED.to_string()
}

#[uniffi::export]
pub fn ffi_embed_source_favorite() -> String {
    core::EMBEDDING_SOURCE_FAVORITE.to_string()
}

#[uniffi::export]
pub fn ffi_embed_source_history() -> String {
    core::EMBEDDING_SOURCE_HISTORY.to_string()
}

// ---------------------------------------------------------------- 行为信号

/// 一种行为信号（名字 / 权重 / 半衰期；设置页和测试读它，规则只有 Rust 一份）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiEmbedSignalKind {
    /// Kotlin 枚举名（`FAVORITE` / `VISIT` / …）。
    pub name: String,
    /// 给人看的名字。
    pub label: String,
    pub base_weight: f32,
    /// 多少天衰减一半；0 = 不衰减。
    pub half_life_days: f64,
}

/// 一张快照里的收藏 / 历史 / 曝光 / 曝光过的卡片 / 屏蔽。
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedSnapshot {
    pub favorites: Vec<FfiEmbedFavorite>,
    pub history: Vec<FfiEmbedHistory>,
    pub exposures: Vec<FfiEmbedExposure>,
    pub seen: Vec<FfiEmbedSeen>,
    pub blocked: Vec<FfiEmbedBlockedAuthor>,
    pub block_words: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedFavorite {
    pub item: FfiEmbedItem,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedHistory {
    pub item: FfiEmbedItem,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedExposure {
    pub key: String,
    pub platform: String,
    /// `ExposureSurface.key`（`home` / `detail` / …）。
    pub surface: String,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedSeen {
    pub item: FfiEmbedItem,
    pub first_seen_at: i64,
}

/// 屏蔽的作者（`author_key` = `platform:id`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedBlockedAuthor {
    pub author_key: String,
    pub blocked_at: i64,
}

/// 一条行为信号。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiEmbedSignal {
    pub key: String,
    /// 信号种类（Kotlin 枚举名，`FAVORITE` / …）。
    pub kind: String,
    pub at: i64,
    pub strength: f32,
    pub platform: Option<String>,
    /// 到这次 `now` 这一刻的权重（基础权重 × 强度 × 衰减）。
    pub weight: f32,
}

impl From<FfiEmbedSnapshot> for core::Snapshot {
    fn from(s: FfiEmbedSnapshot) -> Self {
        core::Snapshot {
            favorites: s
                .favorites
                .into_iter()
                .map(|f| core::FavoriteRow {
                    item: f.item.into(),
                    added_at: f.added_at,
                })
                .collect(),
            history: s
                .history
                .into_iter()
                .map(|h| core::HistoryRow {
                    item: h.item.into(),
                    visited_at: h.visited_at,
                    progress_ms: h.progress_ms,
                    duration_ms: h.duration_ms,
                })
                .collect(),
            exposures: s
                .exposures
                .into_iter()
                .map(|e| core::ExposureRow {
                    key: e.key,
                    platform: e.platform,
                    surface: core::Surface::from_key(&e.surface),
                    exposed_at: e.exposed_at,
                    last_seen_at: e.last_seen_at,
                    dwell_ms: e.dwell_ms,
                })
                .collect(),
            seen: s
                .seen
                .into_iter()
                .map(|r| core::SeenRow {
                    item: r.item.into(),
                    first_seen_at: r.first_seen_at,
                })
                .collect(),
            blocked_authors: s
                .blocked
                .into_iter()
                .map(|b| core::BlockedAuthorRow {
                    author_key: b.author_key,
                    blocked_at: b.blocked_at,
                })
                .collect(),
            block_words: s.block_words,
        }
    }
}

fn signal_kind_from_name(name: &str) -> Option<core::SignalKind> {
    core::SignalKind::ALL
        .into_iter()
        .find(|k| kind_name(*k) == name)
}

/// Kotlin 枚举名（`FAVORITE` / `WATCH_DONE` / …）。
fn kind_name(kind: core::SignalKind) -> &'static str {
    match kind {
        core::SignalKind::Favorite => "FAVORITE",
        core::SignalKind::Visit => "VISIT",
        core::SignalKind::WatchDone => "WATCH_DONE",
        core::SignalKind::DwellLong => "DWELL_LONG",
        core::SignalKind::QuickBack => "QUICK_BACK",
        core::SignalKind::Skip => "SKIP",
        core::SignalKind::BlockedAuthor => "BLOCKED_AUTHOR",
        core::SignalKind::BlockedWord => "BLOCKED_WORD",
    }
}

/// 八种信号的定义（权重 / 半衰期）。
#[uniffi::export]
pub fn ffi_embed_signal_kinds() -> Vec<FfiEmbedSignalKind> {
    core::SignalKind::ALL
        .into_iter()
        .map(|k| FfiEmbedSignalKind {
            name: kind_name(k).to_string(),
            label: k.label().to_string(),
            base_weight: k.base_weight(),
            half_life_days: k.half_life_days(),
        })
        .collect()
}

/// 把一份快照折成信号列表（按 `(at, key, kind)` 排好）。
#[uniffi::export]
pub fn ffi_embed_signals(snapshot: FfiEmbedSnapshot, now: i64) -> Vec<FfiEmbedSignal> {
    let snapshot: core::Snapshot = snapshot.into();
    core::build_signals(&snapshot)
        .into_iter()
        .map(|s| FfiEmbedSignal {
            weight: s.effective_weight(now),
            key: s.key,
            kind: kind_name(s.kind).to_string(),
            at: s.at,
            strength: s.strength,
            platform: s.platform,
        })
        .collect()
}

/// 每种信号各几条（`name` = Kotlin 枚举名）。
#[uniffi::export]
pub fn ffi_embed_signal_counts(snapshot: FfiEmbedSnapshot) -> Vec<FfiEmbedSignalCount> {
    let snapshot: core::Snapshot = snapshot.into();
    let signals = core::build_signals(&snapshot);
    core::signal_counts(&signals)
        .into_iter()
        .map(|(kind, count)| FfiEmbedSignalCount {
            kind: kind_name(kind).to_string(),
            count,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbedSignalCount {
    pub kind: String,
    pub count: i32,
}

/// **只数种类**：设置页那一行"各种信号几条"拿的是宿主已经算好的信号列表，
/// 只需要 `kind` 这一列——不用把每条信号的 key / at / strength 全搬过边界。
///
/// 返回**固定 8 项**（顺序 = Rust 枚举顺序，没出现的算 0）：宿主那边不用自己补零、也不用自己排顺序。
/// 不认识的种类名直接忽略（**不抛**：设置页那一行不该因为一条脏数据变空）。
#[uniffi::export]
pub fn ffi_embed_count_kinds(kinds: Vec<String>) -> Vec<FfiEmbedSignalCount> {
    let parsed: Vec<core::SignalKind> = kinds
        .iter()
        .filter_map(|name| core::kind_from_name(name))
        .collect();
    core::count_kinds(&parsed)
        .into_iter()
        .map(|(kind, count)| FfiEmbedSignalCount {
            kind: kind.name().to_string(),
            count,
        })
        .collect()
}

/// 一条信号是不是正向的（权重 > 0）。**正负的口径只有 Rust 一份**，设置页那行"正 X · 负 Y"用它判。
#[uniffi::export]
pub fn ffi_embed_signal_is_positive(kind: String) -> bool {
    core::kind_from_name(&kind).map(|k| k.is_positive()).unwrap_or(false)
}

/// 全部信号种类（枚举顺序 = Rust 的 `SignalKind::ALL`）。
#[uniffi::export]
pub fn ffi_embed_signal_names() -> Vec<String> {
    core::SignalKind::ALL
        .into_iter()
        .map(|k| k.name().to_string())
        .collect()
}

/// 时间衰减：`0.5 ^ (age / 半衰期)`。
#[uniffi::export]
pub fn ffi_embed_signal_decay(age_ms: i64, half_life_days: f64) -> f32 {
    core::signal_decay(age_ms, half_life_days)
}

/// 一条信号的权重。
#[uniffi::export]
pub fn ffi_embed_signal_weight(kind: String, strength: f32, at: i64, now: i64) -> Option<f32> {
    let kind = signal_kind_from_name(&kind)?;
    let signal = core::BehaviorSignal {
        key: String::new(),
        kind,
        at,
        strength,
        platform: None,
    };
    Some(signal.effective_weight(now))
}

// ---------------------------------------------------------------- 口味画像

/// 一个模型的口味画像。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTasteProfile {
    pub model: String,
    pub positive: Option<Vec<f32>>,
    pub negative: Option<Vec<f32>>,
    pub positive_weight: f32,
    pub negative_weight: f32,
    pub positive_signals: i32,
    pub negative_signals: i32,
}

/// 一条"这个模型下这个 key 的向量"。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTasteVector {
    pub key: String,
    pub vec: Vec<f32>,
}

fn to_taste_vectors(vectors: Vec<FfiTasteVector>) -> std::collections::HashMap<String, Vec<f32>> {
    vectors.into_iter().map(|v| (v.key, v.vec)).collect()
}

impl From<core::TasteProfile> for FfiTasteProfile {
    fn from(p: core::TasteProfile) -> Self {
        FfiTasteProfile {
            model: p.model,
            positive: p.positive,
            negative: p.negative,
            positive_weight: p.positive_weight,
            negative_weight: p.negative_weight,
            positive_signals: p.positive_signals,
            negative_signals: p.negative_signals,
        }
    }
}

/// 造一个模型的口味画像（`signals` 的权重按 `now` 算）。
#[uniffi::export]
pub fn ffi_taste_profile(
    signals: Vec<FfiEmbedSignal>,
    model: String,
    vectors: Vec<FfiTasteVector>,
    now: i64,
) -> FfiTasteProfile {
    let signals: Vec<core::BehaviorSignal> = signals
        .into_iter()
        .filter_map(|s| {
            Some(core::BehaviorSignal {
                kind: signal_kind_from_name(&s.kind)?,
                key: s.key,
                at: s.at,
                strength: s.strength,
                platform: s.platform,
            })
        })
        .collect();
    core::build_profile(&signals, &model, &to_taste_vectors(vectors), now).into()
}

/// 每个模型各造一份画像（`vectorsByModel` 用"模型名 + 一批向量"表示）。
#[uniffi::export]
pub fn ffi_taste_profiles(
    signals: Vec<FfiEmbedSignal>,
    vectors_by_model: Vec<FfiTasteModelVectors>,
    now: i64,
) -> Vec<FfiTasteProfile> {
    let signals: Vec<core::BehaviorSignal> = signals
        .into_iter()
        .filter_map(|s| {
            Some(core::BehaviorSignal {
                kind: signal_kind_from_name(&s.kind)?,
                key: s.key,
                at: s.at,
                strength: s.strength,
                platform: s.platform,
            })
        })
        .collect();
    let mut map = std::collections::HashMap::new();
    for entry in vectors_by_model {
        map.insert(entry.model, to_taste_vectors(entry.vectors));
    }
    core::build_profiles(&signals, &map, now)
        .into_iter()
        .map(Into::into)
        .collect()
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTasteModelVectors {
    pub model: String,
    pub vectors: Vec<FfiTasteVector>,
}

/// 一条卡片对一个画像的相似度（0～1，0.5 = 中性）。
#[uniffi::export]
pub fn ffi_taste_similarity(
    profile: FfiTasteProfile,
    vec: Option<Vec<f32>>,
    model: Option<String>,
    negative_penalty: f32,
) -> Option<f32> {
    let profile = core::TasteProfile {
        model: profile.model,
        positive: profile.positive,
        negative: profile.negative,
        positive_weight: profile.positive_weight,
        negative_weight: profile.negative_weight,
        positive_signals: profile.positive_signals,
        negative_signals: profile.negative_signals,
    };
    profile.similarity(vec.as_deref(), model.as_deref(), negative_penalty)
}

/// 负向中心的排斥强度（固定设计参数）。
#[uniffi::export]
pub fn ffi_taste_negative_penalty() -> f32 {
    core::NEGATIVE_PENALTY
}

// ---------------------------------------------------------------- 封面

/// 这个平台算不算封面向量。
#[uniffi::export]
pub fn ffi_cover_counts(platform: String) -> bool {
    core::cover::counts(&platform)
}

/// 封面向量规则表的**一整行**（测试要断言"每个已装平台都有一行"，所以 `false` 的也要给）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoverPlatformRule {
    pub platform: String,
    pub counts: bool,
}

/// 封面向量规则表（平台 id → 算不算）。
#[uniffi::export]
pub fn ffi_cover_platform_table() -> Vec<FfiCoverPlatformRule> {
    core::cover::PLATFORM_TABLE
        .into_iter()
        .map(|(platform, counts)| FfiCoverPlatformRule {
            platform: platform.to_string(),
            counts,
        })
        .collect()
}

/// 表里所有"算"的平台 id。
#[uniffi::export]
pub fn ffi_cover_platforms() -> Vec<String> {
    core::cover::cover_platforms()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// 这条内容的封面原地址（视频封面优先，然后正文第一张图）。
#[uniffi::export]
pub fn ffi_cover_url(item: FfiEmbedItem) -> Option<String> {
    let item: core::EmbedItem = item.into();
    core::cover::cover_image_url(&item).map(str::to_string)
}

/// 这条内容值不值得算封面向量。
#[uniffi::export]
pub fn ffi_cover_has(item: FfiEmbedItem) -> bool {
    let item: core::EmbedItem = item.into();
    core::cover::has_cover_vector(&item)
}

/// 封面向量要送的那段文字。
#[uniffi::export]
pub fn ffi_cover_text(item: FfiEmbedItem) -> String {
    let item: core::EmbedItem = item.into();
    core::cover::cover_embedding_text(&item)
}

/// 缩略图宽度档（从大到小：640 起）。
#[uniffi::export]
pub fn ffi_cover_cache_widths() -> Vec<i32> {
    core::cover::CACHE_WIDTHS.to_vec()
}

/// 视觉图的输入边长（384）。
#[uniffi::export]
pub fn ffi_cover_input_side() -> i32 {
    core::cover::COVER_INPUT_SIDE as i32
}

/// 文本图里图占几个 token（64）。
#[uniffi::export]
pub fn ffi_cover_image_tokens() -> i32 {
    core::cover::COVER_IMAGE_TOKENS as i32
}

/// 文本+图那张图里正文最多几个 token（60）。
#[uniffi::export]
pub fn ffi_cover_text_tokens() -> i32 {
    core::cover::COVER_TEXT_TOKENS as i32
}

/// 384×384 的 ARGB 像素 → `pixel_values [1, 576, 768]`。
#[uniffi::export]
pub fn ffi_cover_patchify(argb: Vec<i32>, side: i32) -> Result<Vec<f32>, FfiEmbedError> {
    core::cover::patchify(&argb, side.max(0) as usize).map_err(|detail| FfiEmbedError::Invalid { detail })
}

// ---------------------------------------------------------------- 行为导出

/// 一次导出的输入（宿主按"最近的在前"读出来）。
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportInput {
    pub seen: Vec<FfiExportSeen>,
    pub exposures: Vec<FfiExportExposure>,
    pub history: Vec<FfiExportHistory>,
    pub favorites: Vec<FfiExportFavorite>,
    pub embeddings: Vec<FfiEmbedRow>,
    pub blocked: Vec<FfiEmbedBlockedAuthor>,
    pub block_words: Vec<String>,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportSeen {
    pub item: FfiEmbedItem,
    pub first_seen_at: i64,
    pub feed_kind: String,
    pub batch_id: i64,
    pub platform_rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportExposure {
    pub key: String,
    pub platform: String,
    pub surface: String,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportHistory {
    pub item: FfiEmbedItem,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportFavorite {
    pub item: FfiEmbedItem,
    pub folder: Option<String>,
    pub added_at: i64,
}

/// 一张表几行。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportTable {
    pub table: String,
    pub rows: i32,
}

/// 导出的结果：行（不带换行）+ 每张表几行 + 字节数。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExportOutput {
    pub lines: Vec<String>,
    pub tables: Vec<FfiExportTable>,
    pub bytes: i64,
}

/// 把库里这几张表拼成 JSON Lines 的**全部行**（每个字节都和 Kotlin 老实现一致）。
#[uniffi::export]
pub fn ffi_export_behavior(input: FfiExportInput) -> FfiExportOutput {
    let input = core::ExportInput {
        seen: input
            .seen
            .into_iter()
            .map(|s| core::ExportSeen {
                item: s.item.into(),
                first_seen_at: s.first_seen_at,
                feed_kind: s.feed_kind,
                batch_id: s.batch_id,
                platform_rank: s.platform_rank,
            })
            .collect(),
        exposures: input
            .exposures
            .into_iter()
            .map(|e| core::ExportExposure {
                key: e.key,
                platform: e.platform,
                surface: e.surface,
                exposed_at: e.exposed_at,
                last_seen_at: e.last_seen_at,
                dwell_ms: e.dwell_ms,
            })
            .collect(),
        history: input
            .history
            .into_iter()
            .map(|h| core::ExportHistory {
                item: h.item.into(),
                visited_at: h.visited_at,
                progress_ms: h.progress_ms,
                duration_ms: h.duration_ms,
            })
            .collect(),
        favorites: input
            .favorites
            .into_iter()
            .map(|f| core::ExportFavorite {
                item: f.item.into(),
                folder: f.folder,
                added_at: f.added_at,
            })
            .collect(),
        embeddings: input
            .embeddings
            .into_iter()
            .map(|r| core::ExportEmbedding {
                key: r.key,
                model: r.model,
                dim: r.dim,
                vec: r.vec,
                created_at: r.created_at,
                source: r.source,
            })
            .collect(),
        blocked: input
            .blocked
            .into_iter()
            .map(|b| core::BlockedAuthorRow {
                author_key: b.author_key,
                blocked_at: b.blocked_at,
            })
            .collect(),
        block_words: input.block_words,
        now: input.now,
    };
    let out = core::export(&input);
    FfiExportOutput {
        lines: out.lines,
        tables: out
            .tables
            .into_iter()
            .map(|(table, rows)| FfiExportTable { table, rows })
            .collect(),
        bytes: out.bytes,
    }
}

/// 导出格式版本（写进 `meta` 行）。
#[uniffi::export]
pub fn ffi_export_schema_version() -> i32 {
    core::export::EXPORT_SCHEMA_VERSION
}

/// 导出时摘要最多留多少个字。
#[uniffi::export]
pub fn ffi_export_summary_max_chars() -> i32 {
    core::export::EXPORT_SUMMARY_MAX_CHARS as i32
}

/// 标准 base64（带 `=` 补齐；导出的向量就是它）。
#[uniffi::export]
pub fn ffi_export_base64(bytes: Vec<u8>) -> String {
    core::export::encode_base64(&bytes)
}
