//! 本地向量（EmbeddingGemma 2）的**非推理部分**：全在 Rust。
//!
//! Kotlin 那边只剩门面
//! （`app/.../embed/*.kt` 转调 `feedhub-ffi` 的 `FfiEmbed*` / `FfiTaste*` / `FfiCover*`）。
//!
//! ## 每个文件的边界
//!
//! | 文件 | 是什么 | 对应 Kotlin |
//! |---|---|---|
//! | [`item`] | 卡片投影（算向量只用到十几个小字段） | 无（新加的过边界模型） |
//! | [`text`] | 拼文字 / MRL 截断 / L2 归一化 / f16 存字节 | `Embedder.kt` + `HalfFloat.kt` |
//! | [`signal`] | 行为信号（权重 / 半衰期 / 衰减 / 去重） | `BehaviorSignal.kt` |
//! | [`taste`] | 口味画像（正负中心 + 打分） | `TasteProfile.kt` |
//! | [`cover`] | 封面向量的规则（哪些平台、取哪张图、像素怎么摆） | `CoverVectors.kt` |
//! | [`npu`] | 后端选择（选哪张图 / 可不可用 / 什么时候能编图） | `NpuBackend.kt` 的判定部分 |
//! | [`schedule`] | 纯文字向量的调度状态机（三个队列 / 补算 / 按 token 分桶） | `EmbeddingScheduler.kt` |
//! | [`cover_schedule`] | 封面向量的调度状态机（窗口 / 重试 / 状态） | `CoverScheduler.kt` |
//! | [`export`] | 行为导出（JSON Lines 的**每个字节**） | `export/BehaviorExport.kt` |
//! | [`tokenizer`] | BPE + byte fallback 分词器 | `GemmaTokenizer.kt` |
//!
//! ## 什么**不在**这里（留宿主的，每一条都有理由）
//!
//! - **推理**：ONNX Runtime / QNN HTP 的会话、run options、context binary——那是框架调用；
//! - **分词调用**：词表在 Rust（[`tokenizer`]），但"调它"的入口是宿主的 `Embedder`（安卓 / 桌面各一份
//!   ORT 实现），调度器这一层不直接持有词表（一个进程里只该有一份 32 MB 的词表）；
//! - **取图 / 解码图片**：Coil 缓存、`BitmapFactory`、缩略图地址改写（`CardThumbnails.of`，图片加载层唯一一份口径）；
//! - **读库 / 写库**：`item_embedding` 表在 Rust（`library` 模块），调度器通过 [`schedule::EmbedStore`]
//!   trait 问宿主（生产就是 `RustLibrary`，测试是 `MemoryLibrary`），这样测试不用真建一个库；
//! - **协程 / 定时器 / 前后台 / 充电状态**：宿主把状态喂进来，Rust 决定排不排、等多久。
//!
//! 这一层**没有**网络栈、没有后台线程、不读时钟、不取随机数（时间一律由调用方注入）。

pub mod cover;
pub mod cover_schedule;
pub mod export;
pub mod item;
pub mod npu;
pub mod router;
pub mod schedule;
pub mod signal;
pub mod taste;
pub mod text;
pub mod tokenizer;

pub use cover::{CoverStatus, CoverWindowStats};
pub use cover_schedule::{
    CoverConfig, CoverPick, CoverPlan, CoverScheduler, CoverVector, CoverWait, CoverWaitKind,
    WindowFlags, WindowStep,
};
pub use export::{
    export, ExportEmbedding, ExportExposure, ExportFavorite, ExportHistory, ExportInput,
    ExportOutput, ExportSeen,
};
pub use item::EmbedItem;
pub use router::{
    Backend as RouterBackend, BatchAccount, EmbedStep, NpuStatus, PrepareStep, Router, RouterConfig,
    StatusKind, TokenSource,
};
pub use npu::{available as npu_available, can_compile_now, compile_wait_exceeded, model_id as npu_model_choice, pick_ready};
pub use schedule::{
    BackfillCandidate, BatchDone, BatchOutcome, EmbedConfig, EmbedScheduler, EmbedStore,
    EmbeddingRow, MemStore, NextRequest, Step,
};
pub use signal::{
    build_signals, count_kinds, kind_from_name, signal_counts, signal_decay, BehaviorSignal,
    BlockedAuthorRow, ExposureRow, FavoriteRow, HistoryRow, SeenRow, SignalKind, Snapshot, Surface,
};
pub use taste::{build_profile, build_profiles, TasteProfile, NEGATIVE_PENALTY};
pub use text::{
    collapse_whitespace, detail_embedding_text, embedding_text, float_vec_to_half_bytes,
    has_embedding_text, mrl, DETAIL_MAX_TOKENS, EMBEDDING_DIM, EMBEDDING_MODEL_ID,
    EMBEDDING_SOURCE_FAVORITE, EMBEDDING_SOURCE_FEED, EMBEDDING_SOURCE_HISTORY,
};
pub use tokenizer::{GemmaTokenizer, TokenizeError, DEFAULT_MAX_TOKENS};
