//! 本地向量的**调度**（算哪些、什么时候算、一次算几条、按 token 分桶取批）。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/EmbeddingScheduler.kt` 的 Rust 版。
//! 这一层是状态机：**三个队列的优先级、去重、补算分块、按 token 分桶、MRL 截断 + f16 落库**都在这里；
//! 宿主（Kotlin）只做四件事——
//!
//! 1. **推理**（ONNX Runtime / QNN，一次一批，向量一次过边界）；
//! 2. **分词**（`tokenLengths` / `truncateToTokens` 是宿主的 `Embedder`，见 [`Step::Prepare`]）；
//! 3. **读库 / 写库**（[`EmbedStore`]：查已有向量、查是否收藏、写一批行）；
//! 4. **协程 / 定时器 / 前后台开关**（宿主告诉 Rust 现在是不是前台，Rust 决定排不排）。
//!
//! ## 三条输入、三个队列（优先级从上到下）
//!
//! 1. **首页 / 关注的新卡片**（[`EmbedScheduler::offer`]，来源 `feed`）——永远优先；
//! 2. **点开过的条目 + 正文**（[`EmbedScheduler::offer_detail`]）——用"标题 + 正文开头"重算一份、覆盖原向量，
//!    来源按"在不在收藏里"记 `favorite` / `history`；
//! 3. **收藏 / 历史的补算**（[`EmbedScheduler::request_backfill`]）——队列空了才取下一块（[`Step::NeedBackfill`]）。
//!
//! ## 节制条件（一条都没改）
//!
//! 一次 `batch_size`（8）条、已经有向量的不算（按当前 `model` 查）、18+ 不算、没有文字的不算、
//! 补算只看最近 `backfill_window_ms`（30 天）、同一个 `model` 下同一条详情只会覆盖一次。
//!
//! ## 为什么是"宿主拉一步、Rust 回一步"（[`Step`]）
//!
//! 分词和推理都是宿主的 **suspend** 函数（安卓上 `withContext(Dispatchers.Default)`），Rust 不能从
//! 同步回调里调它们。所以把一轮拆成"Rust 说下一步做什么 → 宿主做完把结果交回来"：
//! `Prepare`（要 token 数 / 要按 token 截断）→ `supply_prep`，`Batch`（要一批向量）→ `finish_batch` /
//! `fail_batch`，`NeedBackfill`（要一块补算候选）→ `supply_backfill`。每次都是**一整批**过边界。

use std::collections::{HashMap, HashSet, VecDeque};

use super::item::EmbedItem;
use super::text::{
    detail_embedding_text, embedding_text, has_embedding_text, mrl, EMBEDDING_SOURCE_FEED,
    EMBEDDING_SOURCE_HISTORY, MAX_SUMMARY_CHARS,
};

/// 一批向量的账（性能记录 `embed_batch` 的口径：后端 / 条数 / 耗时 / 模型 / 图长度，**不含内容**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDone {
    pub backend: i32,
    pub count: i32,
    pub d_ms: i64,
    pub model: String,
    pub len: i32,
}

/// 一批的**来源拆分**（日志 / 统计用：详情覆盖几条、补算几条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    pub count: i32,
    /// 这一批里用正文覆盖的几条。
    pub from_detail: i32,
    /// 这一批里补算的几条（不含详情）。
    pub from_backfill: i32,
    /// 详情覆盖 / 补算 / 首页（`""` / `"detail"` / `"backfill"`，给日志选措辞用）。
    pub kind: String,
    /// 这一批**写进库的**模型标识（设置页按 `(model, source)` 累计用）。
    pub model: String,
    /// 每条写进库的来源（和 `rows` 等长等序）。
    pub sources: Vec<String>,
}

/// 一条向量（写库用；`vec` 是 `dim` 个 f16，小端 2 字节一个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRow {
    pub key: String,
    pub model: String,
    pub dim: i32,
    pub vec: Vec<u8>,
    pub created_at: i64,
    pub source: String,
}

/// 调度要宿主做的库操作（**全是同步的**：Kotlin 那边就是 `Library.embeddingKeys` / `insertEmbeddings`）。
pub trait EmbedStore: Send + Sync {
    /// 这批 key 里已经有这个模型向量的那些。
    fn embedding_keys(&self, keys: &[String], model: &str) -> Vec<String>;
    /// 这批 key 里哪些**现在在收藏里**（详情覆盖定来源用）。
    fn is_favorite(&self, keys: &[String]) -> Vec<bool>;
    /// 写一批行。
    fn insert_embeddings(&self, rows: &[EmbeddingRow]);
}

/// 进程内的假库（Rust 测试 / demo 用）：语义和 Kotlin `MemoryLibrary` 在这几件事上一致。
#[derive(Debug, Default)]
pub struct MemStore {
    inner: std::sync::Mutex<MemStoreInner>,
}

#[derive(Debug, Default)]
struct MemStoreInner {
    rows: Vec<EmbeddingRow>,
    favorites: HashSet<String>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_favorite(&self, key: &str) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .favorites
            .insert(key.to_string());
    }

    pub fn rows(&self) -> Vec<EmbeddingRow> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .clone()
    }

    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .len()
    }
}

impl EmbedStore for MemStore {
    fn embedding_keys(&self, keys: &[String], model: &str) -> Vec<String> {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        keys.iter()
            .filter(|k| inner.rows.iter().any(|r| &r.key == *k && r.model == model))
            .cloned()
            .collect()
    }

    fn is_favorite(&self, keys: &[String]) -> Vec<bool> {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        keys.iter().map(|k| inner.favorites.contains(k)).collect()
    }

    fn insert_embeddings(&self, rows: &[EmbeddingRow]) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for row in rows {
            // 主键 (key, model)：同一个 key + model 整行替换
            if let Some(slot) = inner
                .rows
                .iter_mut()
                .find(|r| r.key == row.key && r.model == row.model)
            {
                *slot = row.clone();
            } else {
                inner.rows.push(row.clone());
            }
        }
    }
}

/// 调度参数（对应 Kotlin `EmbeddingScheduler` 的构造参数；默认值和线上一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    pub batch_size: usize,
    pub dim: usize,
    pub backfill_chunk: i32,
    pub backfill_window_ms: i64,
    pub detail_max_tokens: i32,
    pub detail_max_chars: usize,
    pub max_summary_chars: usize,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        EmbedConfig {
            batch_size: 8,
            dim: super::text::EMBEDDING_DIM,
            backfill_chunk: 24,
            backfill_window_ms: 30 * 86_400_000,
            detail_max_tokens: super::text::DETAIL_MAX_TOKENS as i32,
            detail_max_chars: super::text::DETAIL_MAX_CHARS,
            max_summary_chars: MAX_SUMMARY_CHARS,
        }
    }
}

/// 队列里的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    item: EmbedItem,
    source: String,
    /// 详情正文（**只用来算向量**，不存库）；`None` = 用卡片自己带的文字。
    body: Option<String>,
    /// 拼好的文字（缓存：只拼一次）。
    text: Option<String>,
    /// 含 `<bos>` / `<eos>` 的 token 数；`-1` = 还没数，`i32::MAX` = 数不出来（按最长那张算）。
    tokens: i32,
    id: u64,
}

impl Pending {
    fn overrides(&self) -> bool {
        self.body.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Feed,
    Detail,
    Backfill,
}

/// 宿主拉一步的回话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// 三个队列都空、补算也没了：当前结束。
    Empty,
    /// 请宿主从库里取一块补算候选（[`EmbedScheduler::supply_backfill`]）。
    NeedBackfill {
        model: String,
        since_ms: i64,
        limit: i32,
    },
    /// 请宿主把这些文字按 token 截断（`truncate_to > 0`）并数 token（[`EmbedScheduler::supply_prep`]）。
    Prepare {
        texts: Vec<String>,
        truncate_to: Vec<i32>,
    },
    /// 这一批可以算了：`texts` 等序送进 `Embedder.embed(texts, max_tokens)`。
    Batch {
        texts: Vec<String>,
        max_tokens: i32,
        count: i32,
    },
}

/// `next_step` 的入参（宿主每轮现读：现在几点、当前模型、包里带的那几张图的长度）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextRequest {
    pub now: i64,
    pub model: String,
    pub graph_lengths: Vec<i32>,
}

/// 一条补算候选（宿主从 `Library.embeddingBackfill` 里取出来给回来的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillCandidate {
    pub item: EmbedItem,
    pub source: String,
    pub at: i64,
}

/// 已取出、等宿主算的一批。
struct Batch {
    items: Vec<Pending>,
}

/// 本地向量的调度器（Rust 侧状态机）。
pub struct EmbedScheduler {
    cfg: EmbedConfig,
    queue: VecDeque<Pending>,
    queued: HashSet<String>,
    detail: VecDeque<Pending>,
    detail_queued: HashSet<String>,
    backfill: VecDeque<Pending>,
    backfill_queued: HashSet<String>,
    /// 已经用**正文**算过向量的 key（同一个 model 下再点开同一条不重复算）。
    detail_done: HashSet<String>,
    detail_done_model: Option<String>,
    /// 上一次问库补算返回空：当前不用再问了。
    backfill_exhausted: bool,
    /// 等宿主准备文字的那些（[`Step::Prepare`] 之后）。
    prep_pending: Vec<(Which, Pending)>,
    /// 等宿主算的那一批。
    batch: Option<Batch>,
    next_id: u64,
    /// 从启动到现在算过多少条（含这次运行已经删掉的）。
    computed: i32,
    /// 从启动到现在**补算**了多少条。
    backfilled: i32,
    /// 从启动到现在用**详情正文**覆盖了多少条。
    detailed: i32,
    last_batch_size: i32,
}

impl EmbedScheduler {
    pub fn new(cfg: EmbedConfig) -> Self {
        EmbedScheduler {
            cfg,
            queue: VecDeque::new(),
            queued: HashSet::new(),
            detail: VecDeque::new(),
            detail_queued: HashSet::new(),
            backfill: VecDeque::new(),
            backfill_queued: HashSet::new(),
            detail_done: HashSet::new(),
            detail_done_model: None,
            backfill_exhausted: false,
            prep_pending: Vec::new(),
            batch: None,
            next_id: 0,
            computed: 0,
            backfilled: 0,
            detailed: 0,
            last_batch_size: 0,
        }
    }

    pub fn config(&self) -> &EmbedConfig {
        &self.cfg
    }

    pub fn computed(&self) -> i32 {
        self.computed
    }

    pub fn backfilled(&self) -> i32 {
        self.backfilled
    }

    pub fn detailed(&self) -> i32 {
        self.detailed
    }

    pub fn last_batch_size(&self) -> i32 {
        self.last_batch_size
    }

    /// 队列里还有多少条（三个队列合计；设置页 / 测试用）。
    pub fn queued_count(&self) -> i32 {
        (self.queue.len() + self.detail.len() + self.backfill.len() + self.prep_pending.len()) as i32
    }

    /// 现在有没有批**在算或者排队**（封面向量那条用它让路）。
    pub fn busy(&self) -> bool {
        self.batch.is_some() || self.queued_count() > 0
    }

    /// 新内容到了：只留"值得算 + 还没算 + 还没排在队里"的。
    ///
    /// 同一条如果正在补算队列里，会被**提到首页队列**；已经在**详情队列**里的跳过（正文那份更全）。
    pub fn offer(&mut self, items: &[EmbedItem]) -> i32 {
        let mut added = 0;
        for item in items {
            if !has_embedding_text(item) {
                continue;
            }
            if self.detail_queued.contains(&item.key) {
                continue;
            }
            if self.queued.contains(&item.key) {
                continue;
            }
            // 从补算队列（含正在准备的那批）挪过来：首页新卡片优先
            if self.backfill_queued.remove(&item.key) {
                self.backfill.retain(|p| p.item.key != item.key);
                self.prep_pending.retain(|(w, p)| {
                    !(*w == Which::Backfill && p.item.key == item.key)
                });
            }
            if !self.queued.insert(item.key.clone()) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.queue.push_back(Pending {
                item: item.clone(),
                source: EMBEDDING_SOURCE_FEED.to_string(),
                body: None,
                text: None,
                tokens: -1,
                id,
            });
            added += 1;
        }
        if added > 0 {
            self.backfill_exhausted = false;
        }
        added
    }

    pub fn request_backfill(&mut self) {
        self.backfill_exhausted = false;
    }

    /// 点开过的条目 + 正文加载出来了：用正文重算一份、覆盖这条的向量。
    pub fn offer_detail(&mut self, item: &EmbedItem, body: Option<&str>) -> bool {
        if item.adult {
            return false;
        }
        let text = body.filter(|b| !crate::hosted::is_kotlin_blank(b));
        if text.is_none() && !has_embedding_text(item) {
            return false;
        }
        // 从别的队列里挪过来（正文比卡片文字全）
        if self.queued.remove(&item.key) {
            self.queue.retain(|p| p.item.key != item.key);
        }
        if self.backfill_queued.remove(&item.key) {
            self.backfill.retain(|p| p.item.key != item.key);
        }
        if self.detail_queued.remove(&item.key) {
            self.detail.retain(|p| p.item.key != item.key);
        }
        self.prep_pending
            .retain(|(_, p)| p.item.key != item.key);
        self.detail_queued.insert(item.key.clone());
        let id = self.next_id;
        self.next_id += 1;
        self.detail.push_back(Pending {
            item: item.clone(),
            source: EMBEDDING_SOURCE_HISTORY.to_string(),
            body: text.map(str::to_string),
            text: None,
            tokens: -1,
            id,
        });
        self.backfill_exhausted = false;
        true
    }

    /// 退到后台 / 关开关：清空三个队列（正在算的那批由宿主取消，结果丢掉不写库）。
    pub fn cancel(&mut self) {
        self.queue.clear();
        self.queued.clear();
        self.detail.clear();
        self.detail_queued.clear();
        self.backfill.clear();
        self.backfill_queued.clear();
        self.prep_pending.clear();
        self.batch = None;
        self.backfill_exhausted = false;
    }

    /// 清空"这份 model 下已经用正文算过"的记录（清除行为数据之后调；库里的行由宿主清）。
    pub fn reset_stats(&mut self) {
        self.detail_done.clear();
        self.detail_done_model = None;
        self.computed = 0;
        self.backfilled = 0;
        self.detailed = 0;
        self.last_batch_size = 0;
    }

    /// 库里取回来的补算候选（[`Step::NeedBackfill`] 的回话）。
    pub fn supply_backfill(&mut self, candidates: &[BackfillCandidate]) -> i32 {
        if candidates.is_empty() {
            self.backfill_exhausted = true;
            return 0;
        }
        let mut added = 0;
        for c in candidates {
            if !has_embedding_text(&c.item) {
                continue;
            }
            if self.queued.contains(&c.item.key) || self.detail_queued.contains(&c.item.key) {
                continue;
            }
            if !self.backfill_queued.insert(c.item.key.clone()) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.backfill.push_back(Pending {
                item: c.item.clone(),
                source: c.source.clone(),
                body: None,
                text: None,
                tokens: -1,
                id,
            });
            added += 1;
        }
        if added == 0 {
            // 这一块候选全被挡掉了（已经在队列里 / 没文字）：当前不用再问了。
            // 老实现是 `pulled == 0 -> backfillExhausted = true`，逐位对齐。
            self.backfill_exhausted = true;
        }
        added
    }

    /// 宿主算完的 token 数 / 截断结果（[`Step::Prepare`] 的回话）。`texts` / `tokens` 和请求等长等序。
    ///
    /// `tokens < 0` 或 `text` 是空 = 这个宿主数不出来：按"最长那张图"处理（宁可走长图，也别截断长文本）。
    pub fn supply_prep(&mut self, texts: &[String], tokens: &[i32]) {
        let pending = std::mem::take(&mut self.prep_pending);
        for (i, (which, mut p)) in pending.into_iter().enumerate() {
            let text = texts.get(i).cloned().unwrap_or_default();
            if !text.is_empty() {
                p.text = Some(text);
            }
            let count = tokens.get(i).copied().unwrap_or(-1);
            p.tokens = if count < 0 { i32::MAX } else { count };
            self.push_back(which, p);
        }
    }

    /// 宿主这一批算失败了（`embed` 返回 null / 抛异常 / 条数对不上）：丢掉这一批，不写库。
    pub fn fail_batch(&mut self) {
        self.batch = None;
    }

    /// 拉下一步。
    pub fn next_step(&mut self, req: &NextRequest, store: &dyn EmbedStore) -> Step {
        // 上一轮让宿主准备、但没交回来的（不该发生）：按"数不出来"处理，别把条目丢了
        if !self.prep_pending.is_empty() {
            let pending = std::mem::take(&mut self.prep_pending);
            for (which, mut p) in pending {
                p.tokens = i32::MAX;
                self.push_back(which, p);
            }
        }
        if self.batch.is_some() {
            // 上一批还没交回来（宿主重复拉）：先当失败处理
            self.batch = None;
        }

        loop {
            if self.queued_count() == 0 {
                if self.backfill_exhausted {
                    return Step::Empty;
                }
                return Step::NeedBackfill {
                    model: req.model.clone(),
                    since_ms: req.now - self.cfg.backfill_window_ms,
                    limit: self.cfg.backfill_chunk,
                };
            }
            // 1) 需要宿主帮忙的文字（详情按 token 截断 / 数 token）
            let mut lens = req.graph_lengths.clone();
            lens.sort_unstable();
            let need_tokens = lens.len() > 1;
            let mut texts: Vec<String> = Vec::new();
            let mut truncate_to: Vec<i32> = Vec::new();
            let mut prep: Vec<(Which, Pending)> = Vec::new();
            for which in [Which::Feed, Which::Detail, Which::Backfill] {
                let drained: Vec<Pending> = match which {
                    Which::Feed => self.queue.drain(..).collect(),
                    Which::Detail => self.detail.drain(..).collect(),
                    Which::Backfill => self.backfill.drain(..).collect(),
                };
                for mut p in drained {
                    let mut take_out = false;
                    if p.text.is_none() {
                        if p.overrides() {
                            // 详情正文：拼好之后要交给宿主按 token 截断
                            let full = detail_embedding_text(
                                &p.item,
                                p.body.as_deref(),
                                self.cfg.detail_max_chars,
                            );
                            texts.push(full);
                            truncate_to.push(self.cfg.detail_max_tokens);
                            take_out = true;
                        } else {
                            p.text = Some(embedding_text(&p.item, self.cfg.max_summary_chars));
                        }
                    }
                    if need_tokens && p.tokens < 0 && !take_out {
                        let text = p.text.clone().unwrap_or_default();
                        texts.push(text);
                        truncate_to.push(0);
                        take_out = true;
                    }
                    if take_out {
                        prep.push((which, p));
                    } else {
                        self.push_back(which, p);
                    }
                }
            }
            if !texts.is_empty() {
                // 这三条队列已经被搬空了，等宿主交回 token 数之后按原顺序放回去
                self.prep_pending = prep;
                return Step::Prepare { texts, truncate_to };
            }

            // 2) 按 token 分桶（只在包里有多张图时做；CPU / 单图构建一次都不动队列顺序）
            if need_tokens {
                Self::sort_by_bucket(&mut self.queue, &lens);
                Self::sort_by_bucket(&mut self.detail, &lens);
                Self::sort_by_bucket(&mut self.backfill, &lens);
            }
            let front = self
                .queue
                .front()
                .or_else(|| self.detail.front())
                .or_else(|| self.backfill.front());
            let front = match front {
                Some(p) => p.clone(),
                None => return Step::Empty,
            };
            let max_tokens = if !need_tokens || front.tokens < 0 {
                i32::MAX
            } else {
                lens.iter()
                    .copied()
                    .find(|len| *len >= front.tokens)
                    .unwrap_or_else(|| *lens.last().unwrap())
            };

            // 3) 取一批（装不下这张图的跳过、留在队列里）
            let mut out: Vec<Pending> = Vec::with_capacity(self.cfg.batch_size);
            Self::take(&mut self.queue, &mut self.queued, max_tokens, front.id, &mut out, self.cfg.batch_size);
            Self::take(&mut self.detail, &mut self.detail_queued, max_tokens, front.id, &mut out, self.cfg.batch_size);
            Self::take(&mut self.backfill, &mut self.backfill_queued, max_tokens, front.id, &mut out, self.cfg.batch_size);
            if out.is_empty() {
                return Step::Empty;
            }

            // 4) 已经有这个模型向量的去掉（按当前后端查；换后端之后另一份模型的向量不算"算过"）
            let keys: Vec<String> = out.iter().map(|p| p.item.key.clone()).collect();
            let existing: HashSet<String> = store.embedding_keys(&keys, &req.model).into_iter().collect();
            if self.detail_done_model.as_deref() != Some(req.model.as_str()) {
                self.detail_done.clear();
                self.detail_done_model = Some(req.model.clone());
            }
            let before = out.len();
            out.retain(|p| {
                if !p.overrides() {
                    !existing.contains(&p.item.key)
                } else {
                    !existing.contains(&p.item.key) || !self.detail_done.contains(&p.item.key)
                }
            });
            if out.is_empty() {
                // 这一批全是已经算过的：继续取下一批（队列在缩短，不会死循环）
                let _ = before;
                continue;
            }
            let count = out.len() as i32;
            let texts: Vec<String> = out
                .iter()
                .map(|p| p.text.clone().unwrap_or_else(|| embedding_text(&p.item, self.cfg.max_summary_chars)))
                .collect();
            self.batch = Some(Batch { items: out });
            return Step::Batch {
                texts,
                max_tokens,
                count,
            };
        }
    }

    /// 这一批算完了（[`Step::Batch`] 的回话）：MRL 截断 + f16 落库，返回这一批的账。
    ///
    /// `vectors` 是拼平的一批（`count * vec_dim` 个 f32）；条数对不上按失败处理（返回 `None`，不写库）。
    /// `stored_model` 是**这一批实际**写进 `item_embedding.model` 的标识（NPU 中途回退 CPU 时由宿主如实报）。
    pub fn finish_batch(
        &mut self,
        vectors: &[f32],
        vec_dim: i32,
        stored_model: &str,
        now: i64,
        store: &dyn EmbedStore,
    ) -> Option<(BatchOutcome, Vec<EmbeddingRow>)> {
        let batch = self.batch.take()?;
        let dim = vec_dim.max(0) as usize;
        if dim == 0 || vectors.len() != batch.items.len() * dim {
            return None;
        }
        // 详情覆盖：来源由"现在在不在收藏里"决定（一次问一批）
        let override_keys: Vec<String> = batch
            .items
            .iter()
            .filter(|p| p.overrides())
            .map(|p| p.item.key.clone())
            .collect();
        let favorites: HashMap<String, bool> = if override_keys.is_empty() {
            HashMap::new()
        } else {
            store
                .is_favorite(&override_keys)
                .into_iter()
                .zip(override_keys.iter().cloned())
                .map(|(on, key)| (key, on))
                .collect()
        };

        let mut rows = Vec::with_capacity(batch.items.len());
        for (i, p) in batch.items.iter().enumerate() {
            let source = if p.overrides() {
                if favorites.get(&p.item.key).copied().unwrap_or(false) {
                    super::text::EMBEDDING_SOURCE_FAVORITE.to_string()
                } else {
                    EMBEDDING_SOURCE_HISTORY.to_string()
                }
            } else {
                p.source.clone()
            };
            let vec = mrl(&vectors[i * dim..(i + 1) * dim], self.cfg.dim);
            rows.push(EmbeddingRow {
                key: p.item.key.clone(),
                model: stored_model.to_string(),
                dim: self.cfg.dim as i32,
                vec: super::text::float_vec_to_half_bytes(&vec),
                created_at: now,
                source,
            });
        }
        store.insert_embeddings(&rows);

        let from_detail = batch.items.iter().filter(|p| p.overrides()).count() as i32;
        let from_backfill = batch
            .items
            .iter()
            .filter(|p| !p.overrides() && p.source != EMBEDDING_SOURCE_FEED)
            .count() as i32;
        let count = rows.len() as i32;
        self.computed += count;
        self.last_batch_size = count;
        if from_detail > 0 {
            for p in batch.items.iter().filter(|p| p.overrides()) {
                self.detail_done.insert(p.item.key.clone());
            }
            self.detailed += from_detail;
        }
        self.backfilled += from_backfill;
        let kind = if from_detail > 0 {
            "detail"
        } else if from_backfill > 0 {
            "backfill"
        } else {
            ""
        };
        let sources: Vec<String> = rows.iter().map(|r| r.source.clone()).collect();
        let model = rows.first().map(|r| r.model.clone()).unwrap_or_default();
        Some((
            BatchOutcome {
                count,
                from_detail,
                from_backfill,
                kind: kind.to_string(),
                model,
                sources,
            },
            rows,
        ))
    }

    // ---------------------------------------------------------------- 内部

    fn push_back(&mut self, which: Which, p: Pending) {
        match which {
            Which::Feed => self.queue.push_back(p),
            Which::Detail => self.detail.push_back(p),
            Which::Backfill => self.backfill.push_back(p),
        }
    }

    /// 按"至少要多长的图才装得下"分桶（稳定排序，桶内还是先进先出）。
    fn sort_by_bucket(q: &mut VecDeque<Pending>, lens: &[i32]) {
        if q.len() < 2 {
            return;
        }
        let mut items: Vec<Pending> = q.drain(..).collect();
        items.sort_by_key(|p| {
            lens.iter()
                .position(|len| *len >= p.tokens)
                .unwrap_or(lens.len())
        });
        q.extend(items);
    }

    /// 取一批：装不下的跳过（留着，等它排到队首时自然会用更长的那张）。
    fn take(
        from: &mut VecDeque<Pending>,
        keys: &mut HashSet<String>,
        max_tokens: i32,
        front_id: u64,
        out: &mut Vec<Pending>,
        batch_size: usize,
    ) {
        let mut kept: VecDeque<Pending> = VecDeque::with_capacity(from.len());
        while let Some(p) = from.pop_front() {
            if out.len() >= batch_size {
                kept.push_back(p);
                continue;
            }
            if p.tokens > max_tokens && p.id != front_id {
                kept.push_back(p);
                continue;
            }
            keys.remove(&p.item.key);
            out.push(p);
        }
        *from = kept;
    }
}
