//! 分词器（GemmaTokenizer）的 UniFFI 边界。
//!
//! 形状和别的门面一样：**一次调用换一整批**。词表（32 MB 的 `tokenizer.json`）只在构造时过一次边界，
//! 之后这个对象常驻在 Kotlin 侧（`feedhub.rust.RustGemmaTokenizer` 拿着它），编码只过 id。
//!
//! 为什么批接口要把 id 拼平（[`FfiTokBatch`]）：`Vec<Vec<u32>>` 过 UniFFI 会变成一层 `List<List<Int>>`，
//! 每条再 new 一个 List、每个 id 装一个 boxed Int——一批 8 条 × 128 token 就是 1024 个箱子。
//! 拼成一维 + 每条的个数（`lens`），Kotlin 侧一次 `copyOfRange` 就得到 `IntArray`。
//!
//! 线程：对象里全是只读数据（`Send + Sync`），Kotlin 从哪个线程调都行；分词本身不碰 IO、不分配大块内存。

use std::sync::Arc;

use feedhub_core::embed::tokenizer as core;

// ===========================================================================
// 错误
// ===========================================================================

/// 词表加载的错误。字段不叫 `message`。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiTokError {
    /// `tokenizer.json` 的形状不是这一份配置（`model.type` / `byte_fallback` / normalizer / pre_tokenizer）。
    /// 换模型了、或者拿错了文件——**不静默算错**。
    #[error("{detail}")]
    Unsupported { detail: String },

    /// JSON 不合法 / 词表里缺东西（`<0x00>` / `<bos>`）。
    #[error("{detail}")]
    Parse { detail: String },
}

impl From<core::TokenizeError> for FfiTokError {
    fn from(e: core::TokenizeError) -> Self {
        match e {
            core::TokenizeError::Unsupported(detail) => FfiTokError::Unsupported { detail },
            core::TokenizeError::Parse(detail) => FfiTokError::Parse { detail },
        }
    }
}

// ===========================================================================
// record
// ===========================================================================

/// 一批编码结果：`lens[i]` 是第 i 条有几个 id，第 i 条的 id 在 `ids` 里接着往下排。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTokBatch {
    /// 每条的 id 个数（和请求的 `texts` 一一对应、等序）。
    pub lens: Vec<i32>,
    /// 所有条的 id 拼在一起（第 i 条从 `sum(lens[..i])` 开始）。
    pub ids: Vec<i32>,
}

impl FfiTokBatch {
    fn from_rows(rows: Vec<Vec<u32>>) -> Self {
        let total: usize = rows.iter().map(|r| r.len()).sum();
        let mut lens = Vec::with_capacity(rows.len());
        let mut ids = Vec::with_capacity(total);
        for row in rows {
            lens.push(row.len() as i32);
            ids.extend(row.into_iter().map(|id| id as i32));
        }
        Self { lens, ids }
    }
}

// ===========================================================================
// 对象
// ===========================================================================

/// Gemma 分词器（词表加载一次，之后只读）。
#[derive(uniffi::Object)]
pub struct FfiTokTokenizer {
    inner: core::GemmaTokenizer,
}

#[uniffi::export]
impl FfiTokTokenizer {
    /// 从 `tokenizer.json` 的**字节**加载（安卓的 assets 不是文件路径，桌面也照抄这一份）。
    ///
    /// 32 MB 的拷贝只发生这一次；加载完 Rust 只留 merge 表和单码点表（约 8 MB）。
    /// 形状不对 / JSON 坏了会抛 [FfiTokError]，**不会**退回一个"差不多能用"的分词器。
    #[uniffi::constructor]
    pub fn new(json: Vec<u8>) -> Result<Arc<Self>, FfiTokError> {
        Ok(Arc::new(FfiTokTokenizer {
            inner: core::GemmaTokenizer::from_json(&json)?,
        }))
    }

    /// 词表里有多少个 token（只用于日志 / 设置页说明）。
    pub fn vocab_size(&self) -> i32 {
        self.inner.vocab_size() as i32
    }

    /// 一批文本 → 一批 id（含 `<bos>` / `<eos>`），每条最多 `max_tokens` 个（`<= 0` = 不截断）。
    pub fn encode_batch(&self, texts: Vec<String>, max_tokens: i32) -> FfiTokBatch {
        let cap = if max_tokens <= 0 { 0 } else { max_tokens as usize };
        FfiTokBatch::from_rows(texts.iter().map(|t| self.inner.encode(t, cap)).collect())
    }

    /// 一批文本 → 一批**正文** id（不含 `<bos>` / `<eos>`，不截断）：对拍夹具用。
    pub fn encode_body_batch(&self, texts: Vec<String>) -> FfiTokBatch {
        FfiTokBatch::from_rows(texts.iter().map(|t| self.inner.encode_body(t)).collect())
    }

    /// 一批文本各几个 token（含特殊符号、**不截断**的天空）：调度器"按长度分桶"用。
    ///
    /// 只要个数、不要 id：省掉把整批 id 搬回 Kotlin（一条 128 token 的批能少搬几十 KB）。
    pub fn token_lengths(&self, texts: Vec<String>) -> Vec<i32> {
        texts.iter().map(|t| self.inner.token_length(t) as i32).collect()
    }

    /// 一批文本按 token 截断（详情正文用）：返回和 `texts` 等长、等序的**原文本前缀**。
    pub fn truncate_batch(&self, texts: Vec<String>, max_tokens: i32) -> Vec<String> {
        let cap = if max_tokens <= 0 { 0 } else { max_tokens as usize };
        texts.iter().map(|t| self.inner.truncate_to_tokens(t, cap)).collect()
    }
}
