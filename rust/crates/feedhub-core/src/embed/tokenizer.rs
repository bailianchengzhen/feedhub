//! Gemma 分词器（EmbeddingGemma 2 的 `tokenizer.json`）：**BPE + byte fallback**。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/GemmaTokenizer.kt` 的 Rust 版。
//! Kotlin 那份搬到这里之后只剩一层薄门面（`app/.../GemmaTokenizer.kt` 转调 [`crate::embed::GemmaTokenizer`]
//! 的 UniFFI 对象 `FfiTokTokenizer`），Kotlin 原实现退到 `:app` 的 desktopTest 里当对拍参照
//! （`LegacyGemmaTokenizer`）。
//!
//! 为什么自己写（没有引第三方分词库）：`tokenizer.json` 里就是标准的 HF 配置
//! （`normalizer = Replace(" " → "▁")`、`pre_tokenizer = Split(" ", MergedWithPrevious)`、BPE 的
//! `vocab` + `merges`、byte fallback、后处理模板 `<bos> … <eos>`），照着实现一遍不到 400 行；而现成的
//! Rust 分词库（`tokenizers`）要拉一堆依赖、还要把词表塞进两份不同的实现里对拍。
//!
//! 内存 / 速度：**不把整份 32 MB 的 JSON 留在内存里**（也不建 DOM）。加载时临时建一个 `token → id` 表，
//! merges **边读边查表**（不等整份读完再解，省掉 515k 对临时字符串），之后常驻的只有：
//! - 单码点 → id：BMP（`< 0x10000`，中文 / 全角 / 假名都在这儿）走 **65536 项直查表**（256 KB），
//!   增补平面（emoji）走一张有序表；
//! - merge 表：按 `idA` 分桶（`merge_first` 前缀表 + 桶内按 `idB` 有序的两个数组），
//!   查一次是"一次下标 + 桶内线性扫"（桶里平均 2 条），比 515k 条上二分快得多（约 6 MB）。
//!
//! 编码流程（和 HF 的 pipeline 一致）：
//! 1. 按 `added_tokens`（特殊符号，如 `<bos>`）切段，命中的直接出它的 id；
//! 2. 每段把空格换成 `▁`（U+2581）——normalizer；
//! 3. 每个码点：在词表里就用它的 id，否则按 UTF-8 逐字节换成 `<0xXX>`（byte fallback）；
//! 4. 反复合并"merge 序号最小"的相邻对，直到没有可合并的（`ignore_merges = false`）；
//! 5. 按 `post_processor` 的模板加 `<bos>` / `<eos>`，按需截断（每条最多 [`DEFAULT_MAX_TOKENS`] 个，含特殊符号）。
//!
//! **不是通用 tokenizer 实现**：只认这一份配置的形状（BPE、空 `continuing_subword_prefix` /
//! `end_of_word_suffix`、`fuse_unk = true`），遇到别的形状加载时直接报错，不静默算错。
//!
//! 三处已知的口径差（都只在"不是合法 UTF-8 文本"时看得出来，正常内容不受影响）：
//! - Kotlin 的 `String` 能装半个代理对（lone surrogate），UniFFI 过边界时按 UTF-8 编。调用方
//!   （`feedhub.rust.RustGemmaTokenizer`）在过边界**之前**把半个代理对换成 U+FFFD（和 HuggingFace
//!   `tokenizers` 的 UTF-8 口径一致），不整批抛异常；换过之后两边逐 id 相同（Kotlin 版对 U+FFFD 也是同一套编码）。
//! - [`GemmaTokenizer::truncate_to_tokens`] 的二分是在 UTF-16 单元上做的（和 Kotlin 逐步对齐，连"切在代理对
//!   中间"那次探测也算进去了）；唯一不可能对齐的是**返回值本身**——Kotlin 能返回一个以半个代理对结尾的前缀，
//!   Rust 的 `String` 表示不了，那种情况下退到前一个字符边界。
//! - 特殊符号（`added_tokens`）只在**字符边界**上匹配（Kotlin 是按 UTF-16 下标逐个试的）。现成的
//!   `added_tokens` 全是 ASCII（`<bos>` / `<eos>` / `<start_of_turn>` …），两种口径一样。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// 一条内容最多算多少个 token。
pub const DEFAULT_MAX_TOKENS: usize = 64;

/// normalizer 把空格换成这个（U+2581，HF 的 SentencePiece 空白标记）。
const SPACE_MARK: char = '\u{2581}';
/// 后处理模板里"正文"那一格的占位（其余格子是特殊符号 id）。
const BODY: i32 = -1;
const MAX_RANK: usize = (1 << 19) - 1;
const RANK_SHIFT: u32 = 18;
const MERGED_MASK: u64 = (1 << 18) - 1;
/// 直查表里"这个码点在词表里没有单字符 token"。
const ABSENT: u32 = u32::MAX;

/// 加载 / 编码的错误。
///
/// 分两种：**形状不对**（`model.type` 不是 BPE、没开 byte fallback、normalizer / pre_tokenizer 不认识）和
/// **内容坏了**（JSON 不合法、词表里缺 `<0x00>` / `<bos>`）。调用方据此说清楚"是这份词表不认"还是"文件坏了"。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenizeError {
    /// `tokenizer.json` 的形状不是这一份配置（换模型 / 换配置了，这个实现不认）。
    #[error("{0}")]
    Unsupported(String),
    /// JSON 不合法 / 词表里缺东西。
    #[error("{0}")]
    Parse(String),
}

impl TokenizeError {
    fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    fn parse(msg: impl Into<String>) -> Self {
        Self::Parse(msg.into())
    }
}

type Result<T> = std::result::Result<T, TokenizeError>;

/// 一个特殊符号（`added_tokens` 里的条目）。
struct AddedToken {
    text: String,
    id: u32,
}

/// Gemma 的 BPE 分词器（词表加载一次，之后只读；`Send + Sync`，可以跨线程共用）。
pub struct GemmaTokenizer {
    bos_id: u32,
    eos_id: u32,
    /// 单码点 token 的个数（和 Kotlin 版的 `vocabSize` 同口径：只算"正好一个码点"的那些）。
    single_cp_count: usize,
    /// BMP（码点 `< 0x10000`）→ id；[`ABSENT`] = 没有。
    bmp_ids: Vec<u32>,
    /// 增补平面（emoji 等）的单码点表，按码点升序（[`Self::astral_ids`] 与之一一对应）。
    astral_cps: Vec<u32>,
    astral_ids: Vec<u32>,
    /// merge 表按 `idA` 分桶：桶 `a` 的条目在 `merge_first[a]..merge_first[a + 1]`。
    merge_first: Vec<u32>,
    /// 每个条目的 `idB`（桶内升序）。
    merge_b: Vec<u32>,
    /// 每个条目的 `(rank << 18) | mergedId`。
    merge_value: Vec<u64>,
    /// `<0x00>` … `<0xFF>` 的 id；byte fallback 用。
    byte_ids: [u32; 256],
    /// 特殊符号（按长度从长到短，切段时先试长的）。
    added: Vec<AddedToken>,
    /// `added` 里出现过的首字节（绝大多数位置一眼就能跳过切段检查）。
    added_first: [bool; 256],
    /// 后处理模板：`BODY` 那条是正文，其余是特殊符号 id（含 `<bos>` / `<eos>`）。
    template: Vec<i32>,
}

impl GemmaTokenizer {
    /// 词表里有多少个 token（只算单码点的那张表，和 Kotlin 版的 `vocabSize` 同口径）。
    pub fn vocab_size(&self) -> usize {
        self.single_cp_count
    }

    /// 有几个 merge（测试 / 内存口径用）。
    pub fn merge_count(&self) -> usize {
        self.merge_b.len()
    }

    /// 表本身占多少字节（测试 / 内存口径用；不含 `String` 的堆分配）。
    pub fn table_bytes(&self) -> usize {
        self.bmp_ids.len() * 4
            + self.astral_cps.len() * 4
            + self.astral_ids.len() * 4
            + self.merge_first.len() * 4
            + self.merge_b.len() * 4
            + self.merge_value.len() * 8
            + self.byte_ids.len() * 4
            + self.template.len() * 4
            + self.added.iter().map(|t| t.text.len() + 4).sum::<usize>()
    }

    /// 编码成**含特殊符号**的 token id（`<bos> … <eos>`），最多 `max_tokens` 个。
    ///
    /// 截断按 HF 的规矩：特殊符号保留，正文从尾部截掉（`max_tokens = 64` 时正文最多 62 个）。
    /// `max_tokens = 0` 表示不截断（测试里对拍整条输出用）。
    pub fn encode(&self, text: &str, max_tokens: usize) -> Vec<u32> {
        let body = self.encode_body(text);
        let specials = self.template.len().saturating_sub(1);
        let limit = if max_tokens == 0 {
            body.len()
        } else {
            max_tokens.saturating_sub(specials)
        };
        let mut out = Vec::with_capacity(if body.len() <= limit { body.len() + specials } else { limit + specials });
        for &slot in &self.template {
            if slot == BODY {
                let n = body.len().min(limit);
                out.extend_from_slice(&body[..n]);
            } else {
                out.push(slot as u32);
            }
        }
        out
    }

    /// 只要正文（不含 `<bos>` / `<eos>`），给对拍夹具用。
    pub fn encode_body(&self, text: &str) -> Vec<u32> {
        self.encode_body_tail(text, None)
    }

    /// 同 [`Self::encode_body`]，但最后一段的符号末尾多一个**半个代理对**（高位代理码元）。
    ///
    /// 只给 [`Self::truncate_to_tokens`] 用：Kotlin 的 `substring(0, mid)` 可能切在代理对中间，那半个高位代理
    /// 会走 byte fallback（3 个字节 token，等于 CESU-8）。为了让二分和 Kotlin 一个 bit 都不差，这里要能复现它。
    fn encode_body_tail(&self, text: &str, tail: Option<u32>) -> Vec<u32> {
        if text.is_empty() && tail.is_none() {
            return Vec::new();
        }
        if self.added.is_empty() {
            return self.bpe(self.segment_symbols(text, tail));
        }
        let mut out: Vec<u32> = Vec::with_capacity(text.len() / 2 + 8);
        let mut seg_start = 0usize;
        let mut i = 0usize;
        while i < text.len() {
            if let Some(hit) = self.match_added(text, i) {
                self.append_segment(text, seg_start, i, &mut out, None);
                out.push(hit.id);
                i += hit.text.len();
                seg_start = i;
            } else {
                // 前进一个字符（Kotlin 是前进一个 UTF-16 单元；特殊符号都是 ASCII，落点一样）
                i += char_len_at(text, i);
            }
        }
        self.append_segment(text, seg_start, text.len(), &mut out, tail);
        out
    }

    /// 一共几个 token（含特殊符号、**不截断**）：调度器"按长度分桶"用。
    pub fn token_length(&self, text: &str) -> usize {
        self.encode(text, 0).len()
    }

    /// 把文本截到最多 `max_tokens` 个 token。
    ///
    /// 返回的是**原文本的前缀**（不重新拼 token），保证 `encode(结果, 0).len() <= max_tokens`；
    /// 二分找最长的那个前缀，二分的位置和 Kotlin 版**一模一样**（UTF-16 单元数，允许切在代理对中间——
    /// 那样半个代理对按 byte fallback 算，正是 Kotlin `substring` 的行为）。
    /// `max_tokens = 0` 或本来就装得下时原样返回。
    ///
    /// **两边唯一的差别**：Kotlin 的 `String` 能返回"以半个代理对结尾"的前缀，Rust 的 `String` 表示不了
    /// （不是合法 UTF-8），那种情况下这里退到前一个字符边界。正常文本（含 emoji）不会走到那儿。
    pub fn truncate_to_tokens(&self, text: &str, max_tokens: usize) -> String {
        if max_tokens == 0 || text.is_empty() {
            return text.to_string();
        }
        if self.encode(text, 0).len() <= max_tokens {
            return text.to_string();
        }
        let total = text.encode_utf16().count();
        let fits = |units: usize| self.encoded_len_at_units(text, units) <= max_tokens;
        let mut lo = 0usize;
        let mut hi = total;
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        // 别切在代理对中间：半个 emoji 会走 byte fallback，变成几个垃圾 token（Kotlin 同款）
        if lo >= 1 && lo < total && unit_cut(text, lo).1.is_some() {
            lo -= 1;
        }
        // BPE 在边界上不是严格单调的（前缀的 token 数可能比整条多 1 个）：往前退到真的装得下
        while lo > 0 && !fits(lo) {
            lo -= 1;
        }
        // 上面两步可能停在一个"半个代理对"上（Kotlin 能返回，Rust 不能）：退到前一个字符边界再确认装得下
        while lo > 0 && unit_cut(text, lo).1.is_some() {
            lo -= 1;
            while lo > 0 && !fits(lo) {
                lo -= 1;
            }
        }
        let (cut, _) = unit_cut(text, lo);
        // Kotlin 的 `String.trimEnd()`：`Char.isWhitespace()`（不是 Rust 的 White_Space，见 crate::html）
        crate::html::kotlin_trim_end(&text[..cut]).to_string()
    }

    /// 把文本截到 `units` 个 UTF-16 单元时，一共几个 token（含特殊符号、不截断）——Kotlin
    /// `encode(text.substring(0, units), 0).size` 的等价物。
    fn encoded_len_at_units(&self, text: &str, units: usize) -> usize {
        let (cut, lone) = unit_cut(text, units);
        let specials = self.template.len().saturating_sub(1);
        self.encode_body_tail(&text[..cut], lone).len() + specials
    }

    // ------------------------------------------------------------------ 内部

    #[inline]
    fn match_added(&self, text: &str, at: usize) -> Option<&AddedToken> {
        // 绝大多数位置这一步就能跳过（这份词表的特殊符号都以 `<` 开头）
        if !self.added_first[text.as_bytes()[at] as usize] {
            return None;
        }
        self.added.iter().find(|t| text[at..].starts_with(&t.text))
    }

    fn append_segment(&self, text: &str, from: usize, to: usize, out: &mut Vec<u32>, tail: Option<u32>) {
        if to <= from && tail.is_none() {
            return;
        }
        out.extend(self.bpe(self.segment_symbols(&text[from..to], tail)));
    }

    /// 一段（没有特殊符号）：normalizer（空格 → `▁`）→ 码点 → byte fallback。返回 BPE 之前的符号序列。
    ///
    /// [tail] 是"半个代理对"（见 [`Self::encode_body_tail`]）：按 Kotlin 的 byte fallback 口径拆成 3 个字节 token
    /// （`0xED / 0x80|((hs>>6)&0x3F) / 0x80|(hs&0x3F)`，就是 CESU-8 的那三个字节）。
    fn segment_symbols(&self, segment: &str, tail: Option<u32>) -> Vec<u32> {
        let mut symbols: Vec<u32> = Vec::with_capacity(segment.len() / 2 + 8);
        for ch in segment.chars() {
            // normalizer：Replace(" " → "▁")。直接按符号查，省掉一次整段字符串的拷贝
            let cp = if ch == ' ' { SPACE_MARK as u32 } else { ch as u32 };
            if let Some(id) = self.char_id(cp) {
                symbols.push(id);
            } else {
                // byte fallback：这个码点在词表里没有单字符 token，就按 UTF-8 拆成字节
                let mut buf = [0u8; 4];
                let ch = char::from_u32(cp).expect("cp 来自 char");
                let bytes = ch.encode_utf8(&mut buf).as_bytes();
                symbols.extend(bytes.iter().map(|&b| self.byte_ids[b as usize]));
            }
        }
        if let Some(hs) = tail {
            symbols.push(self.byte_ids[(0xE0 | (hs >> 12)) as usize]);
            symbols.push(self.byte_ids[(0x80 | ((hs >> 6) & 0x3F)) as usize]);
            symbols.push(self.byte_ids[(0x80 | (hs & 0x3F)) as usize]);
        }
        symbols
    }

    /// BPE：**永远先合"merge 序号最小"的那一对**（和 HF / Kotlin 版的结果一样）。
    ///
    /// 实现是"双向链表 + 最小堆"，不是 Kotlin 那种"每轮全表扫一遍再合掉所有出现"：
    /// 那种写法是 O(n²) 的（长正文 300 个字 + byte fallback 就是 900 个符号，一轮一轮扫很贵），
    /// 这里是 O(n log n)——每合一次只往堆里放新产生的两个 pair。
    ///
    /// 两种写法的**结果**一样（`TokenizerParityTest` / `fixtures_match_hugging_face` 逐 id 守着）：
    /// 堆按 `(rank, 位置)` 排，rank 就是 merge 在文件里的下标，同一个 rank 多次出现时按位置从左往右合，
    /// 和 Kotlin 的 `mergePair` 一致；合完新产生的 pair 带着自己的 rank 进堆，下一轮自然会先合更小的。
    fn bpe(&self, symbols: Vec<u32>) -> Vec<u32> {
        let n = symbols.len();
        if n <= 1 {
            return symbols;
        }
        let mut ids = symbols;
        // 链表：prev / next 存节点下标，`usize::MAX` = 没有；`alive` 标记被合掉的节点
        let mut prev: Vec<usize> = (0..n).map(|i| i.wrapping_sub(1)).collect();
        let mut next: Vec<usize> = (0..n).map(|i| i + 1).collect();
        let mut alive = vec![true; n];
        // 最小堆：`(rank, 位置, 期望的左 id, 期望的右 id)`——后两个用来丢弃过期的堆项
        let mut heap: BinaryHeap<Reverse<(u32, usize, u32, u32)>> = BinaryHeap::with_capacity(2 * n + 4);
        for i in 0..n - 1 {
            Self::push_pair(&mut heap, &ids, &next, n, i, |a, b| self.merge_of(a, b));
        }
        while let Some(Reverse((_rank, at, want_left, want_right))) = heap.pop() {
            if !alive[at] {
                continue;
            }
            let right = next[at];
            if right >= n || !alive[right] {
                continue;
            }
            // 堆项过期（这一格后来被合成了别的）就别动它，新的 pair 早就有自己的堆项了
            if ids[at] != want_left || ids[right] != want_right {
                continue;
            }
            let merged = (self.merge_of(want_left, want_right).expect("建堆项时查过") & MERGED_MASK) as u32;
            ids[at] = merged;
            alive[right] = false;
            let after = next[right];
            next[at] = after;
            if after < n {
                prev[after] = at;
            }
            let before = prev[at];
            if before != usize::MAX && alive[before] {
                Self::push_pair(&mut heap, &ids, &next, n, before, |a, b| self.merge_of(a, b));
            }
            if after < n {
                Self::push_pair(&mut heap, &ids, &next, n, at, |a, b| self.merge_of(a, b));
            }
        }
        // 顺着链表收结果
        let mut out = Vec::with_capacity(n);
        let mut i = 0usize;
        while i < n && !alive[i] {
            i += 1;
        }
        while i < n {
            out.push(ids[i]);
            let nx = next[i];
            if nx >= n {
                break;
            }
            i = nx;
        }
        out
    }

    /// 往堆里放一个 `at` 位置的相邻对（没有这个 merge 就不放）。抽出来是为了不借住 `ids` / `next`。
    #[inline]
    fn push_pair<F>(
        heap: &mut BinaryHeap<Reverse<(u32, usize, u32, u32)>>,
        ids: &[u32],
        next: &[usize],
        n: usize,
        at: usize,
        merge_of: F,
    ) where
        F: Fn(u32, u32) -> Option<u64>,
    {
        let right = next[at];
        if right >= n {
            return;
        }
        if let Some(v) = merge_of(ids[at], ids[right]) {
            heap.push(Reverse(((v >> RANK_SHIFT) as u32, at, ids[at], ids[right])));
        }
    }

    /// `(idA, idB)` 的 merge 值 `(rank << 18) | mergedId`；没有这个 merge 返回 `None`。
    ///
    /// 一次下标拿桶 + **桶内二分**（桶里按 `idB` 升序）。桶的分布很偏：平均 6 条，但
    /// `<0xE4>`（大部分汉字的 UTF-8 首字节）那个桶有 **4 万多条**——所以桶内必须二分，不能线性扫。
    #[inline]
    fn merge_of(&self, a: u32, b: u32) -> Option<u64> {
        let ai = a as usize;
        if ai + 1 >= self.merge_first.len() {
            return None;
        }
        let start = self.merge_first[ai] as usize;
        let end = self.merge_first[ai + 1] as usize;
        if start == end {
            return None;
        }
        let bucket = &self.merge_b[start..end];
        match bucket.binary_search(&b) {
            Ok(i) => Some(self.merge_value[start + i]),
            Err(_) => None,
        }
    }

    #[inline]
    fn char_id(&self, cp: u32) -> Option<u32> {
        if let Some(&id) = self.bmp_ids.get(cp as usize) {
            return if id == ABSENT { None } else { Some(id) };
        }
        match self.astral_cps.binary_search(&cp) {
            Ok(i) => Some(self.astral_ids[i]),
            Err(_) => None,
        }
    }

    /// 从 `tokenizer.json` 的字节加载。**只认 EmbeddingGemma 2 那份配置的形状**（见模块说明）。
    pub fn from_json(json: &[u8]) -> Result<Self> {
        let mut scanner = JsonScanner::new(json);
        let mut added: Vec<AddedToken> = Vec::new();
        let mut template: Option<Vec<i32>> = None;
        let mut model_type: Option<String> = None;
        let mut byte_fallback = false;
        let mut continuing_prefix: Option<String> = None;
        let mut end_of_word_suffix: Option<String> = None;
        let mut normalizer_ok = false;
        let mut pre_tokenizer_ok = false;
        let mut bos_name: Option<String> = None;
        let mut eos_name: Option<String> = None;
        let mut vocab: Option<HashMap<String, u32>> = None;
        let mut merges: Option<MergeInput> = None;

        scanner.object_entries(|s, key| {
            match key {
                "added_tokens" => added = read_added_tokens(s)?,
                "normalizer" => normalizer_ok = read_normalizer_ok(s)?,
                "pre_tokenizer" => pre_tokenizer_ok = read_pre_tokenizer_ok(s)?,
                "post_processor" => {
                    let pp = read_post_processor(s)?;
                    if let Some((tpl, bos, eos)) = pp {
                        template = Some(tpl);
                        bos_name = bos;
                        eos_name = eos;
                    }
                }
                "model" => {
                    let model = read_model(s)?;
                    model_type = Some(model.kind);
                    byte_fallback = model.byte_fallback;
                    continuing_prefix = model.continuing_prefix;
                    end_of_word_suffix = model.end_of_word_suffix;
                    vocab = model.vocab;
                    merges = model.merges;
                }
                _ => s.skip_value()?,
            }
            Ok(())
        })?;

        if model_type.as_deref() != Some("BPE") {
            return Err(TokenizeError::unsupported(format!(
                "tokenizer.json 的 model.type 是 {}，这个实现只支持 BPE",
                model_type.as_deref().unwrap_or("（没有）")
            )));
        }
        if !byte_fallback {
            return Err(TokenizeError::unsupported("tokenizer.json 没有开 byte_fallback"));
        }
        if continuing_prefix.as_deref().is_some_and(|p| !p.is_empty()) {
            return Err(TokenizeError::unsupported(format!(
                "不支持 continuing_subword_prefix={}",
                continuing_prefix.as_deref().unwrap_or("")
            )));
        }
        if end_of_word_suffix.as_deref().is_some_and(|p| !p.is_empty()) {
            return Err(TokenizeError::unsupported(format!(
                "不支持 end_of_word_suffix={}",
                end_of_word_suffix.as_deref().unwrap_or("")
            )));
        }
        if !normalizer_ok {
            return Err(TokenizeError::unsupported(
                "tokenizer.json 的 normalizer 不是 Replace(\" \" → \"▁\")",
            ));
        }
        if !pre_tokenizer_ok {
            return Err(TokenizeError::unsupported(
                "tokenizer.json 的 pre_tokenizer 不是 Split(\" \", MergedWithPrevious)",
            ));
        }
        let table = vocab.ok_or_else(|| TokenizeError::parse("tokenizer.json 里没有 model.vocab"))?;
        if table.is_empty() {
            return Err(TokenizeError::parse("tokenizer.json 的 vocab 是空的"));
        }
        let merges = merges.ok_or_else(|| TokenizeError::parse("tokenizer.json 里没有 model.merges"))?;
        let merge_rows = resolve_merges(merges, &table)?;

        // 单码点 → id（BPE 的起点只用得到单字符）：BMP 直查、增补平面有序表
        let mut bmp_ids = vec![ABSENT; 0x10000];
        let mut astral: Vec<(u32, u32)> = Vec::new();
        let mut single_cp_count = 0usize;
        for (token, id) in &table {
            let Some(cp) = single_code_point(token) else { continue };
            single_cp_count += 1;
            if (cp as usize) < bmp_ids.len() {
                bmp_ids[cp as usize] = *id;
            } else {
                astral.push((cp, *id));
            }
        }
        astral.sort_unstable_by_key(|p| p.0);
        let (astral_cps, astral_ids): (Vec<u32>, Vec<u32>) = astral.into_iter().unzip();

        // merge 表：按 idA 分桶（桶内按 idB 升序）
        let (merge_first, merge_b, merge_value) = build_merge_table(merge_rows);

        // byte fallback 用的 <0x00> … <0xFF>
        let mut byte_ids = [0u32; 256];
        for (b, slot) in byte_ids.iter_mut().enumerate() {
            let name = format!("<0x{b:02X}>");
            *slot = *table
                .get(&name)
                .ok_or_else(|| TokenizeError::parse(format!("词表里没有 {name}（byte fallback 需要的字节 token）")))?;
        }
        let bos = *table
            .get(bos_name.as_deref().unwrap_or("<bos>"))
            .ok_or_else(|| TokenizeError::parse("词表里没有 <bos>"))?;
        let eos = *table
            .get(eos_name.as_deref().unwrap_or("<eos>"))
            .ok_or_else(|| TokenizeError::parse("词表里没有 <eos>"))?;

        let mut final_template = match template {
            // 没有 post_processor：用默认的 <bos> 正文 <eos>
            None => vec![bos as i32, BODY, eos as i32],
            Some(mut tpl) => {
                for slot in tpl.iter_mut() {
                    *slot = match *slot {
                        -2 => bos as i32,
                        -3 => eos as i32,
                        other => other,
                    };
                }
                tpl
            }
        };
        let body_slots = final_template.iter().filter(|s| **s == BODY).count();
        if body_slots != 1 {
            return Err(TokenizeError::parse(format!(
                "post_processor 的模板里正文出现 {body_slots} 次"
            )));
        }
        // `added` 按长度从长到短（和 Kotlin 的 `sortedByDescending { it.text.length }` 同口径：UTF-16 长度、稳定排序）
        added.sort_by_key(|t| std::cmp::Reverse(t.text.encode_utf16().count()));
        let mut added_first = [false; 256];
        for token in &added {
            if let Some(&b) = token.text.as_bytes().first() {
                added_first[b as usize] = true;
            }
        }
        final_template.shrink_to_fit();

        Ok(Self {
            bos_id: bos,
            eos_id: eos,
            single_cp_count,
            bmp_ids,
            astral_cps,
            astral_ids,
            merge_first,
            merge_b,
            merge_value,
            byte_ids,
            added,
            added_first,
            template: final_template,
        })
    }

    /// `<bos>` 的 id（测试 / 对拍用）。
    pub fn bos_id(&self) -> u32 {
        self.bos_id
    }

    /// `<eos>` 的 id（测试 / 对拍用）。
    pub fn eos_id(&self) -> u32 {
        self.eos_id
    }
}

/// merge 一条：`(idA, idB, (rank << 18) | mergedId)`。
type MergeRow = (u32, u32, u64);

/// 读到的 merge：正常情况下**边读边查表**解成 id（省掉 515k 对临时字符串）；
/// 如果 `merges` 出现在 `vocab` 之前，先留着原始字符串，等 vocab 读完再解。
enum MergeInput {
    Rows(Vec<MergeRow>),
    Raw(Vec<(String, String)>),
}

fn resolve_merges(input: MergeInput, table: &HashMap<String, u32>) -> Result<Vec<MergeRow>> {
    match input {
        MergeInput::Rows(rows) => Ok(rows),
        MergeInput::Raw(raw) => {
            let mut rows = Vec::with_capacity(raw.len());
            for (rank, (a, b)) in raw.iter().enumerate() {
                if let Some(row) = merge_row(table, a, b, rank)? {
                    rows.push(row);
                }
            }
            Ok(rows)
        }
    }
}

/// 一条 merge 能不能用（两边和合并后的 token 都在词表里）；不在词表里的按 HF 的规矩丢掉。
fn merge_row(table: &HashMap<String, u32>, a: &str, b: &str, rank: usize) -> Result<Option<MergeRow>> {
    let (Some(&id_a), Some(&id_b)) = (table.get(a), table.get(b)) else {
        return Ok(None);
    };
    // 合并后的 token 要先拼出来再查：这一步在大词表上是加载时最贵的一小块（515k 次短字符串分配）
    let Some(&merged_id) = table.get(&format!("{a}{b}")) else {
        return Ok(None);
    };
    if rank > MAX_RANK {
        return Err(TokenizeError::unsupported(format!(
            "merge 序号超过 19 位（{rank}），打包方式要改"
        )));
    }
    Ok(Some((
        id_a,
        id_b,
        ((rank as u64) << RANK_SHIFT) | (merged_id as u64),
    )))
}

/// 把 merge 行整理成"按 idA 分桶 + 桶内按 idB 升序"的两张数组（外加前缀表）。
fn build_merge_table(mut rows: Vec<MergeRow>) -> (Vec<u32>, Vec<u32>, Vec<u64>) {
    if rows.is_empty() {
        return (vec![0, 0], Vec::new(), Vec::new());
    }
    rows.sort_unstable_by_key(|r| (r.0, r.1));
    let max_a = rows.last().map(|r| r.0).unwrap_or(0) as usize;
    let mut first = vec![0u32; max_a + 2];
    for row in &rows {
        first[row.0 as usize + 1] += 1;
    }
    for a in 1..first.len() {
        first[a] += first[a - 1];
    }
    let mut merge_b = Vec::with_capacity(rows.len());
    let mut merge_value = Vec::with_capacity(rows.len());
    for row in rows {
        merge_b.push(row.1);
        merge_value.push(row.2);
    }
    (first, merge_b, merge_value)
}

/// `token` 是不是"正好一个码点"，是就返回它。
fn single_code_point(token: &str) -> Option<u32> {
    let mut chars = token.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(first as u32)
}

/// `text[at..]` 的第一个字符有几个字节（`at` 一定在字符边界上）。
fn char_len_at(text: &str, at: usize) -> usize {
    text[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

/// 把文本截到 `units` 个 **UTF-16 单元**时的切点：返回 (字节下标, 半个代理对)。
///
/// 第二个返回值只在"切点正好落在一个代理对中间"时是 `Some(高位代理码元)`——Kotlin 的
/// `String.substring(0, units)` 会切出这样一个字符串（半个 emoji），它的 byte fallback 就是 3 个字节 token。
fn unit_cut(text: &str, units: usize) -> (usize, Option<u32>) {
    let mut used = 0usize;
    for (i, ch) in text.char_indices() {
        let w = ch.len_utf16();
        if used + w > units {
            if w == 2 && used + 1 == units {
                let cp = ch as u32;
                return (i, Some(0xD800 + ((cp - 0x10000) >> 10)));
            }
            return (i, None);
        }
        used += w;
    }
    (text.len(), None)
}

// ===========================================================================
// 解析（够用就好的 JSON 扫描器）
// ===========================================================================

struct BpeModel {
    kind: String,
    byte_fallback: bool,
    continuing_prefix: Option<String>,
    end_of_word_suffix: Option<String>,
    vocab: Option<HashMap<String, u32>>,
    merges: Option<MergeInput>,
}

/// 够用就好的 JSON 扫描器：只处理 UTF-8 字节、只提供"按已知结构读"和"跳过不认识的"两种操作。
/// 不建 DOM（`tokenizer.json` 有 32 MB，建树在手机上要几百 MB）。
struct JsonScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn err_at(&self, what: impl std::fmt::Display) -> TokenizeError {
        TokenizeError::parse(format!("JSON 第 {} 个字节{what}", self.pos))
    }

    fn peek(&mut self) -> Result<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied().ok_or_else(|| TokenizeError::parse("JSON 提前结束"))
    }

    fn try_consume(&mut self, c: u8) -> Result<bool> {
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&c) {
            self.pos += 1;
            return Ok(true);
        }
        Ok(false)
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        self.skip_ws();
        let got = self.bytes.get(self.pos).copied();
        if got != Some(c) {
            return Err(self.err_at(format!(
                "是 {}，期望 '{}'",
                got.map(|b| format!("'{}'", b as char)).unwrap_or_else(|| "（结束）".to_string()),
                c as char
            )));
        }
        self.pos += 1;
        Ok(())
    }

    fn read_int(&mut self) -> Result<i64> {
        self.skip_ws();
        let mut neg = false;
        if self.bytes.get(self.pos) == Some(&b'-') {
            neg = true;
            self.pos += 1;
        }
        let mut v: i64 = 0;
        let mut any = false;
        while let Some(&b) = self.bytes.get(self.pos) {
            if !b.is_ascii_digit() {
                break;
            }
            v = v * 10 + (b - b'0') as i64;
            self.pos += 1;
            any = true;
        }
        if !any {
            return Err(self.err_at("开始不是整数"));
        }
        Ok(if neg { -v } else { v })
    }

    fn read_bool(&mut self) -> Result<bool> {
        self.skip_ws();
        if self.starts_with(b"true") {
            self.pos += 4;
            return Ok(true);
        }
        if self.starts_with(b"false") {
            self.pos += 5;
            return Ok(false);
        }
        Err(self.err_at("不是 true / false"))
    }

    fn read_nullable_string(&mut self) -> Result<Option<String>> {
        self.skip_ws();
        if self.starts_with(b"null") {
            self.pos += 4;
            return Ok(None);
        }
        self.read_string().map(Some)
    }

    fn read_string(&mut self) -> Result<String> {
        self.skip_ws();
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err(self.err_at("不是字符串开头"));
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(&b) = self.bytes.get(self.pos) {
                if b == b'"' || b == b'\\' {
                    break;
                }
                self.pos += 1;
            }
            if self.pos > start {
                let run = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|e| TokenizeError::parse(format!("JSON 字符串里不是合法的 UTF-8：{e}")))?;
                out.push_str(run);
            }
            match self.bytes.get(self.pos).copied() {
                None => return Err(TokenizeError::parse("JSON 字符串没有结束")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = *self
                        .bytes
                        .get(self.pos)
                        .ok_or_else(|| TokenizeError::parse("JSON 字符串里的转义不完整"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.hex4(self.pos)?;
                            self.pos += 4;
                            // 代理对：紧跟着 `\uDCxx` 就合成一个码点（和 Kotlin 的口径一样）
                            if (0xD800..=0xDBFF).contains(&cp)
                                && self.bytes.get(self.pos) == Some(&b'\\')
                                && self.bytes.get(self.pos + 1) == Some(&b'u')
                            {
                                let low = self.hex4(self.pos + 2)?;
                                if (0xDC00..=0xDFFF).contains(&low) {
                                    let full = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                                    out.push(char::from_u32(full).unwrap_or('\u{fffd}'));
                                    self.pos += 6;
                                } else {
                                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                        }
                        other => {
                            return Err(TokenizeError::parse(format!(
                                "不认识的转义 \\{}",
                                other as char
                            )))
                        }
                    }
                }
                Some(_) => unreachable!(),
            }
        }
    }

    /// 读 `{ key: … }`：每读到一项就把 key 交给 `handler`，由它决定读值还是跳值。
    fn object_entries<F>(&mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(&mut Self, &str) -> Result<()>,
    {
        self.expect(b'{')?;
        if self.try_consume(b'}')? {
            return Ok(());
        }
        loop {
            let key = self.read_string()?;
            self.expect(b':')?;
            handler(self, &key)?;
            if self.try_consume(b',')? {
                continue;
            }
            self.expect(b'}')?;
            return Ok(());
        }
    }

    fn skip_value(&mut self) -> Result<()> {
        match self.peek()? {
            b'{' => self.object_entries(|s, _| s.skip_value()),
            b'[' => {
                self.expect(b'[')?;
                if self.try_consume(b']')? {
                    return Ok(());
                }
                loop {
                    self.skip_value()?;
                    if self.try_consume(b',')? {
                        continue;
                    }
                    self.expect(b']')?;
                    return Ok(());
                }
            }
            b'"' => {
                self.read_string()?;
                Ok(())
            }
            b't' | b'f' => {
                self.read_bool()?;
                Ok(())
            }
            b'n' => {
                self.skip_ws();
                if !self.starts_with(b"null") {
                    return Err(self.err_at("不是 null"));
                }
                self.pos += 4;
                Ok(())
            }
            _ => {
                // 数字（可能有小数 / 指数）
                self.skip_ws();
                while let Some(&b) = self.bytes.get(self.pos) {
                    if matches!(b, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                        break;
                    }
                    self.pos += 1;
                }
                Ok(())
            }
        }
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if matches!(b, b' ' | b'\n' | b'\r' | b'\t') {
                self.pos += 1;
            } else {
                return;
            }
        }
    }

    fn starts_with(&self, text: &[u8]) -> bool {
        self.bytes.len() >= self.pos + text.len() && &self.bytes[self.pos..self.pos + text.len()] == text
    }

    fn hex4(&self, at: usize) -> Result<u32> {
        let mut v = 0u32;
        for i in 0..4 {
            let c = *self
                .bytes
                .get(at + i)
                .ok_or_else(|| TokenizeError::parse("\\u 后面不够 4 位"))?;
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(TokenizeError::parse("\\u 后面不是 4 位十六进制")),
            };
            v = (v << 4) | d as u32;
        }
        Ok(v)
    }
}

// ===========================================================================
// 各个字段的读法（和 Kotlin 版逐条对应）
// ===========================================================================

/// 解析 `added_tokens: [{id, content, special, …}, …]`。
fn read_added_tokens(s: &mut JsonScanner<'_>) -> Result<Vec<AddedToken>> {
    let mut out = Vec::new();
    s.expect(b'[')?;
    loop {
        if s.try_consume(b']')? {
            break;
        }
        let mut id: i64 = -1;
        let mut content: Option<String> = None;
        s.object_entries(|s, key| {
            match key {
                "id" => id = s.read_int()?,
                "content" => content = Some(s.read_string()?),
                _ => s.skip_value()?,
            }
            Ok(())
        })?;
        if id >= 0 {
            if let Some(text) = content {
                out.push(AddedToken { text, id: id as u32 });
            }
        }
        if s.try_consume(b',')? {
            continue;
        }
        s.expect(b']')?;
        break;
    }
    Ok(out)
}

/// `normalizer` 是不是那个 Replace(" " → "▁")（不认识的返回 false，由调用方报错）。
fn read_normalizer_ok(s: &mut JsonScanner<'_>) -> Result<bool> {
    let mut kind: Option<String> = None;
    let mut pattern: Option<String> = None;
    let mut content: Option<String> = None;
    s.object_entries(|s, key| {
        match key {
            "type" => kind = Some(s.read_string()?),
            "pattern" => pattern = read_pattern_string(s)?,
            "content" => content = Some(s.read_string()?),
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(kind.as_deref() == Some("Replace")
        && pattern.as_deref() == Some(" ")
        && content == Some(SPACE_MARK.to_string()))
}

/// `pre_tokenizer` 是不是 Split(" ", MergedWithPrevious)。
fn read_pre_tokenizer_ok(s: &mut JsonScanner<'_>) -> Result<bool> {
    let mut kind: Option<String> = None;
    let mut pattern: Option<String> = None;
    let mut behavior: Option<String> = None;
    s.object_entries(|s, key| {
        match key {
            "type" => kind = Some(s.read_string()?),
            "pattern" => pattern = read_pattern_string(s)?,
            "behavior" => behavior = Some(s.read_string()?),
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(kind.as_deref() == Some("Split")
        && pattern.as_deref() == Some(" ")
        && behavior.as_deref() == Some("MergedWithPrevious"))
}

fn read_pattern_string(s: &mut JsonScanner<'_>) -> Result<Option<String>> {
    let mut value: Option<String> = None;
    s.object_entries(|s, key| {
        match key {
            "String" => value = Some(s.read_string()?),
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(value)
}

/// `post_processor` 的 `single` 模板：返回 (模板, bos 名字, eos 名字)。
/// 只支持 `<bos> / 正文 / <eos>` 这种形状（`Sequence` 出现一次）。
fn read_post_processor(s: &mut JsonScanner<'_>) -> Result<Option<(Vec<i32>, Option<String>, Option<String>)>> {
    let mut single: Option<Vec<i32>> = None;
    let mut bos_name: Option<String> = None;
    let mut eos_name: Option<String> = None;
    s.object_entries(|s, key| {
        match key {
            "single" => single = Some(read_template(s)?),
            "special_tokens" => {
                s.object_entries(|s, name| {
                    let spec = read_special_token(s)?;
                    match name {
                        "<bos>" => bos_name = spec,
                        "<eos>" => eos_name = spec,
                        _ => {}
                    }
                    Ok(())
                })?;
            }
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(single.map(|tpl| (tpl, bos_name, eos_name)))
}

/// 一个 `{"id": "...", "ids": [..], "tokens": [..]}` 这样的特殊符号定义，返回 `id` 字段。
fn read_special_token(s: &mut JsonScanner<'_>) -> Result<Option<String>> {
    let mut id_name: Option<String> = None;
    s.object_entries(|s, key| {
        match key {
            "id" => id_name = Some(s.read_string()?),
            "ids" => {
                s.expect(b'[')?;
                if !s.try_consume(b']')? {
                    s.read_int()?;
                    while s.try_consume(b',')? {
                        s.read_int()?;
                    }
                    s.expect(b']')?;
                }
            }
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(id_name)
}

fn read_template(s: &mut JsonScanner<'_>) -> Result<Vec<i32>> {
    let mut out = Vec::new();
    s.expect(b'[')?;
    loop {
        if s.try_consume(b']')? {
            break;
        }
        // 每一项是 {"SpecialToken": {...}} 或 {"Sequence": {...}}
        let mut body = false;
        let mut special: Option<String> = None;
        s.object_entries(|s, kind| {
            match kind {
                "SpecialToken" => special = Some(read_special_token(s)?.unwrap_or_else(|| "<bos>".to_string())),
                "Sequence" => {
                    body = true;
                    s.skip_value()?;
                }
                _ => s.skip_value()?,
            }
            Ok(())
        })?;
        if body {
            out.push(BODY);
        } else {
            // 模板里的特殊符号先用负数占位（-2 = bos，-3 = eos），加载完再换成真实 id
            out.push(match special.as_deref() {
                Some("<eos>") => -3,
                _ => -2,
            });
        }
        if s.try_consume(b',')? {
            continue;
        }
        s.expect(b']')?;
        break;
    }
    Ok(out)
}

/// `model`：形状开关 + **vocab 立刻读**，merges 边读边解（vocab 还没读到就留着原始字符串）。
fn read_model(s: &mut JsonScanner<'_>) -> Result<BpeModel> {
    let mut kind: Option<String> = None;
    let mut byte_fallback = false;
    let mut continuing_prefix: Option<String> = None;
    let mut end_of_word_suffix: Option<String> = None;
    let mut vocab: Option<HashMap<String, u32>> = None;
    let mut merges: Option<MergeInput> = None;
    s.object_entries(|s, key| {
        match key {
            "type" => kind = Some(s.read_string()?),
            "byte_fallback" => byte_fallback = s.read_bool()?,
            "continuing_subword_prefix" => continuing_prefix = s.read_nullable_string()?,
            "end_of_word_suffix" => end_of_word_suffix = s.read_nullable_string()?,
            "vocab" => vocab = Some(read_vocab(s)?),
            "merges" => {
                merges = Some(match vocab.as_ref() {
                    // 正常情况（这份 tokenizer.json 就是）：边读边查表，不留 515k 对临时字符串
                    Some(table) => MergeInput::Rows(read_merges_direct(s, table)?),
                    None => MergeInput::Raw(read_merges_raw(s)?),
                });
            }
            _ => s.skip_value()?,
        }
        Ok(())
    })?;
    Ok(BpeModel {
        kind: kind.ok_or_else(|| TokenizeError::parse("tokenizer.json 里没有 model.type"))?,
        byte_fallback,
        continuing_prefix,
        end_of_word_suffix,
        vocab,
        merges,
    })
}

fn read_vocab(s: &mut JsonScanner<'_>) -> Result<HashMap<String, u32>> {
    let mut map = HashMap::with_capacity(1 << 18);
    s.object_entries(|s, key| {
        let id = s.read_int()?;
        map.insert(key.to_string(), id as u32);
        Ok(())
    })?;
    Ok(map)
}

/// merges 的先攒后解形态（`merges` 在 `vocab` 之前时用）：`[["a","b"], …]`（老格式是 `["a b", …]`）。
fn read_merges_raw(s: &mut JsonScanner<'_>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(1 << 19);
    s.expect(b'[')?;
    loop {
        if s.try_consume(b']')? {
            break;
        }
        let (a, b) = read_merge_pair(s)?;
        out.push((a, b));
        if s.try_consume(b',')? {
            continue;
        }
        s.expect(b']')?;
        break;
    }
    Ok(out)
}

/// 同上，但**读一条解一条**（vocab 已经在了）：不攒字符串，直接把 `(idA, idB, value)` 收下来。
fn read_merges_direct(s: &mut JsonScanner<'_>, table: &HashMap<String, u32>) -> Result<Vec<MergeRow>> {
    let mut out = Vec::with_capacity(1 << 19);
    let mut rank = 0usize;
    s.expect(b'[')?;
    loop {
        if s.try_consume(b']')? {
            break;
        }
        let (a, b) = read_merge_pair(s)?;
        if let Some(row) = merge_row(table, &a, &b, rank)? {
            out.push(row);
        }
        rank += 1;
        if s.try_consume(b',')? {
            continue;
        }
        s.expect(b']')?;
        break;
    }
    Ok(out)
}

/// 一条 merge：新格式是 `["a","b"]`，老格式是一个 `"a b"` 字符串。
fn read_merge_pair(s: &mut JsonScanner<'_>) -> Result<(String, String)> {
    if s.peek()? == b'[' {
        s.expect(b'[')?;
        let a = s.read_string()?;
        s.expect(b',')?;
        let b = s.read_string()?;
        s.expect(b']')?;
        return Ok((a, b));
    }
    let line = s.read_string()?;
    let sep = line
        .find(' ')
        .ok_or_else(|| TokenizeError::parse(format!("merges 里的这一项不是 \"a b\"：{line}")))?;
    if sep == 0 || sep == line.len() - 1 {
        return Err(TokenizeError::parse(format!(
            "merges 里的这一项不是 \"a b\"：{line}"
        )));
    }
    Ok((line[..sep].to_string(), line[sep + 1..].to_string()))
}
