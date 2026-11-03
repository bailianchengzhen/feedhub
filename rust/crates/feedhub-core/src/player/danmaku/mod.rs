//! **弹幕**：过滤 / 合并 / 时间线 / 装载策略 / 轨道排布 / 设置换算
//! （`app/.../video/DanmakuFilter.kt` + `DanmakuModel.kt` + `DanmakuSettings.kt` 的模型部分）。
//!
//! ## 分工
//!
//! 弹幕的一生分三段，前两段进 Rust，第三段（绘制）留在宿主：
//!
//! 1. **进时间线**（[`Filter::process`]）：丢掉不能显示的（高级弹幕 [`DanmakuMode::Other`]、空文字）→
//!    按类型 / 颜色 / 权重 / 屏蔽词过滤 → 顶部底部转滚动 → 按时间排序 → 合并重复。**一段一次过 FFI**；
//! 2. **排轨道**（[`Scheduler::plan_from`]）：随播放进度把弹幕分到轨道上，算好每条的出现时间 / 时长 / 轨道号。
//!    这是**状态机**（"这条能不能塞进这条轨道"取决于轨道上一条的尾巴进没进屏幕、会不会被追上），
//!    **一段一批**：宿主在新段取回、改设置、跳转时各调一次，返回**整段**的排布表，不是每帧一次；
//! 3. **画**：每帧只剩"从排布表里挑还活着的、按 `start` / `duration` 算 x 和 y"——纯算术，留在宿主
//!
//! ## 为什么 `plan_from` 是"重放"而不是"接着上次"
//!
//! 轨道占用的状态只受"一个周期（[`SCROLL_MS`] / 停留时长）内出现过的弹幕"影响（更早的都出屏了、
//! 轨道已经空了），所以从 `from - 一个周期` 开始按时间顺序重放一遍，结果和"一直在播"完全一样
//! 好处是**没有跨调用的隐藏状态**：宿主换清晰度、改设置、跳转只要拿同一个 `from` 再要一份就行。
//!
//! ## 线程
//!
//! Kotlin 那边的注释写着"所有方法在同一个线程调用（界面线程）"；安卓的弹幕画线程是**独立线程**，
//! 所以宿主侧按"一次计划一份只读表"用：计划在界面线程算好、交给画线程读（只读、不加锁）。
//!
//! ## 屏蔽词的正则
//!
//! Kotlin 用 `Regex(body, IGNORE_CASE)` + `containsMatchIn`（非锚定搜索），并把"能匹配空文字"的规则
//! 当成无效。这里用 `regex` crate 的等价物：`case_insensitive(true)` + `is_match`（非锚定），
//! 编译失败 / `is_match("")` 为真就当无效。`\d` 这类写法两边都认；`.` 两边默认都不含 `\n`。

use std::collections::HashMap;

use regex::RegexBuilder;

use crate::play::{Danmaku, DanmakuMode};

// ===========================================================================
// 常量（Kotlin `DanmakuDefaults`）
// ===========================================================================

/// 滚动弹幕从出现到完全消失的时间（正常速度下，视频时间）。
pub const SCROLL_MS: i64 = 8000;

/// 顶部 / 底部弹幕停留的时间。
pub const FIXED_MS: i64 = 4000;

/// 同一轨道上前后两条滚动弹幕之间至少留的空隙（像素）。
pub const LANE_GAP_PX: f32 = 16.0;

/// 播放进度往前跳超过这么多毫秒，就当成"跳转"，重新对齐（增量重放用）。
pub const JUMP_MS: i64 = 1500;

/// 取失败的段最多试几次。
pub const MAX_TRIES: u32 = 3;

/// 取失败之后隔多久再试（毫秒）。
pub const RETRY_AFTER_MS: i64 = 4000;

/// 倍速下滚动弹幕在视频时间里走完全程要多久：`base * sqrt(speed)`（速度夹在 0.25~4）。
///
/// 弹幕的出现时间跟着视频进度走（所以 2 倍速时出得也快一倍），但滚动速度只随倍速增长一半
/// （取平方根）：2 倍速时视觉上快约 √2 倍而不是 2 倍，不会快得看不清。
pub fn scroll_ms(speed: f32, base_ms: i64) -> i64 {
    let speed = (speed.clamp(0.25, 4.0)) as f64;
    (base_ms as f64 * speed.sqrt()) as i64
}

/// "固定像素速度"：宽 `width_dp` 的播放器上，滚动时间是基准的几倍——以 640 dp 宽为基准，
/// 窄到 0.75 倍、宽到 2.5 倍为止（BiliPai 也是这个范围）。
pub fn fixed_velocity_factor(width_dp: f32) -> f32 {
    (width_dp / FIXED_VELOCITY_BASE_DP).clamp(0.75, 2.5)
}

const FIXED_VELOCITY_BASE_DP: f32 = 640.0;

// ===========================================================================
// 过滤（Kotlin `DanmakuFilter` + `BlockRules`）
// ===========================================================================

/// 弹幕进时间线时做的过滤和合并。
///
/// 顺序照 BiliPai：丢掉不能显示的（[`DanmakuMode::Other`]、空文字）→ 按类型 / 颜色 / 权重 / 屏蔽词过滤 →
/// 顶部底部转滚动 → 按时间排序 → 合并重复。
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub allow_scroll: bool,
    pub allow_top: bool,
    pub allow_bottom: bool,
    pub allow_colorful: bool,
    /// 权重低于它的弹幕丢掉；0 = 不按权重丢。没有权重的弹幕不受影响。
    pub weight_level: i32,
    /// 屏蔽词原文（写法见 [`BlockRules`]）。
    pub rules: Vec<String>,
    pub static_to_scroll: bool,
    pub merge: bool,
    pub merge_window_ms: i64,
    pub merge_threshold: i32,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            allow_scroll: true,
            allow_top: true,
            allow_bottom: true,
            allow_colorful: true,
            weight_level: 0,
            rules: Vec::new(),
            static_to_scroll: false,
            merge: false,
            merge_window_ms: 500,
            merge_threshold: 2,
        }
    }
}

impl Filter {
    /// 什么都不过滤、不合并（只丢掉 OTHER 和空文字）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 过滤一段弹幕，结果按出现时间排好（**稳定**排序：同一时刻保持输入顺序，和 Kotlin `sortedBy` 一样）。
    pub fn process(&self, raw: &[Danmaku], rules: &BlockRules) -> Vec<Danmaku> {
        let mut list: Vec<Danmaku> = raw
            .iter()
            .filter(|d| self.shows(d, rules))
            .cloned()
            .collect();
        if self.static_to_scroll {
            for d in list.iter_mut() {
                if d.mode != DanmakuMode::Scroll {
                    d.mode = DanmakuMode::Scroll;
                }
            }
        }
        list.sort_by_key(|d| d.time_ms);
        if self.merge {
            merge_duplicates(&list, self.merge_window_ms, self.merge_threshold)
        } else {
            list
        }
    }

    fn shows(&self, d: &Danmaku, rules: &BlockRules) -> bool {
        if crate::hosted::is_kotlin_blank(&d.text) {
            return false;
        }
        let type_allowed = match d.mode {
            DanmakuMode::Scroll => self.allow_scroll,
            DanmakuMode::Top => self.allow_top,
            DanmakuMode::Bottom => self.allow_bottom,
            // 逆向、高级、代码弹幕：不做
            DanmakuMode::Other => false,
        };
        if !type_allowed {
            return false;
        }
        if !self.allow_colorful && (d.color & 0xFF_FFFF) != 0xFF_FFFF {
            return false;
        }
        if self.weight_level > 0 {
            if let Some(weight) = d.weight {
                if weight < self.weight_level {
                    return false;
                }
            }
        }
        !rules.blocks(&d.text)
    }
}

/// 合并重复弹幕：`sorted`（按时间排好）里内容完全一样、相邻两条相隔不超过 `window_ms` 的算一批，
/// 一批并成第一条；这一批有 `threshold` 条以上，合并后的文字后面加 `" x3"` 这样的数；不够的也合并，
/// 只是不标数（照 BiliPai）。只有一条的原样保留，结果仍然按时间排。
///
/// `threshold` 先夹到 ≥ 2——`1` 会让**每一条**都带上 `" x1"`。
pub fn merge_duplicates(sorted: &[Danmaku], window_ms: i64, threshold: i32) -> Vec<Danmaku> {
    struct Batch {
        index: usize,
        last_ms: i64,
        count: i32,
    }
    let mut out: Vec<Danmaku> = Vec::with_capacity(sorted.len());
    let mut batches: Vec<Batch> = Vec::new();
    let mut open: HashMap<&str, usize> = HashMap::new();
    for d in sorted {
        match open.get(d.text.as_str()).copied() {
            Some(bi) if d.time_ms - batches[bi].last_ms <= window_ms => {
                batches[bi].count += 1;
                batches[bi].last_ms = d.time_ms;
            }
            _ => {
                let bi = batches.len();
                batches.push(Batch {
                    index: out.len(),
                    last_ms: d.time_ms,
                    count: 1,
                });
                open.insert(d.text.as_str(), bi);
                out.push(d.clone());
            }
        }
    }
    let threshold = threshold.max(2);
    for b in &batches {
        if b.count >= threshold {
            let mut d = out[b.index].clone();
            d.text = format!("{} x{}", d.text, b.count);
            out[b.index] = d;
        }
    }
    out
}

/// 一条屏蔽词算哪种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// 关键词：弹幕里包含它就屏蔽（不分大小写）。
    Keyword,
    /// 正则：弹幕里能匹配上就屏蔽（不分大小写）。
    Regex,
    /// 写成了正则但编译不了、或者会匹配空文字（那会屏蔽所有弹幕）：不起作用。
    Invalid,
}

/// 编译好的屏蔽词。
///
/// 一条屏蔽词是关键词，或者正则：写成 `regex:第\d+集`、`re:第\d+集`，或者前后各一个斜杠
/// `/第\d+集/`（照 BiliPai；它另外支持的按用户 hash 屏蔽，我们的弹幕没有用户信息，不做）。
/// 正则只在编译时编译一次；匹配用 `is_match`（非锚定、不分大小写）。
#[derive(Debug, Clone, Default)]
pub struct BlockRules {
    keywords: Vec<String>,
    /// 统一小写的关键词（`contains(ignoreCase = true)` 在 Rust 里要自己折）。
    keywords_lower: Vec<String>,
    regexes: Vec<regex::Regex>,
}

impl BlockRules {
    /// 什么都没有（不屏蔽任何弹幕）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 编译一批屏蔽词：去掉前后空白、空的丢掉。
    pub fn compile(rules: &[String]) -> Self {
        if rules.is_empty() {
            return Self::none();
        }
        let mut out = Self::default();
        for rule in rules {
            let r = crate::hosted::kotlin_trim(rule);
            if r.is_empty() {
                continue;
            }
            match regex_body(r) {
                None => {
                    out.keywords_lower.push(r.to_lowercase());
                    out.keywords.push(r.to_string());
                }
                Some(body) => {
                    if let Some(re) = compile_regex(body) {
                        out.regexes.push(re);
                    }
                }
            }
        }
        out
    }

    /// 是不是一条都没编译出来。
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty() && self.regexes.is_empty()
    }

    /// 编译出来的关键词条数（检查工具 / 测试用）。
    pub fn keyword_count(&self) -> usize {
        self.keywords.len()
    }

    /// 编译出来的正则条数（检查工具 / 测试用）。
    pub fn regex_count(&self) -> usize {
        self.regexes.len()
    }

    /// `text` 要不要被屏蔽。
    pub fn blocks(&self, text: &str) -> bool {
        if !self.keywords.is_empty() {
            let lower = text.to_lowercase();
            if self.keywords_lower.iter().any(|k| lower.contains(k.as_str())) {
                return true;
            }
        }
        self.regexes.iter().any(|re| re.is_match(text))
    }

    /// 一条屏蔽词算哪种（弹幕设置面板里给用户看的提示用）。
    pub fn kind_of(rule: &str) -> RuleKind {
        let r = crate::hosted::kotlin_trim(rule);
        match regex_body(r) {
            None => RuleKind::Keyword,
            Some(body) => {
                if compile_regex(body).is_some() {
                    RuleKind::Regex
                } else {
                    RuleKind::Invalid
                }
            }
        }
    }
}

/// 正则的正文；不是正则写法就是 `None`。前缀不分大小写（`REGEX:` 也认）。
///
/// 前缀比较走 `as_bytes()`：规则可能是中文（`剧透`），拿字节下标切 `&str` 会切到字符中间 panic。
fn regex_body(rule: &str) -> Option<&str> {
    let bytes = rule.as_bytes();
    if starts_with_ignore_ascii_case(bytes, b"regex:") {
        return Some(crate::hosted::kotlin_trim(&rule[6..]));
    }
    if starts_with_ignore_ascii_case(bytes, b"re:") {
        return Some(crate::hosted::kotlin_trim(&rule[3..]));
    }
    if bytes.len() >= 2 && bytes[0] == b'/' && bytes[bytes.len() - 1] == b'/' {
        return Some(&rule[1..rule.len() - 1]);
    }
    None
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

/// 编译一条正则（不分大小写）；编译不了、或者会匹配空文字就 `None`（当成无效）。
fn compile_regex(body: &str) -> Option<regex::Regex> {
    if body.is_empty() {
        return None;
    }
    let re = RegexBuilder::new(body).case_insensitive(true).build().ok()?;
    if re.is_match("") {
        return None;
    }
    Some(re)
}

// ===========================================================================
// 时间线（Kotlin `DanmakuTimeline`）
// ===========================================================================

/// 一条弹幕的跨调用 id：`(段号 << 32) | 段内下标`。
///
/// 宿主按这个 id 回自己的"处理好的弹幕"表里取文字 / 颜色（那段是 [`Timeline::put`] 交回来的），
/// 于是排布表里只放 id + 排布结果，不把每条弹幕的字符串再搬一遍。
pub fn danmaku_id(segment: i32, index: usize) -> u64 {
    ((segment as u32 as u64) << 32) | index as u64
}

/// id → (段号, 段内下标)。
pub fn split_id(id: u64) -> (i32, usize) {
    ((id >> 32) as u32 as i32, (id & 0xFFFF_FFFF) as usize)
}

/// 已取回的弹幕，按时间排好、按段存。
///
/// 弹幕进来（[`put`](Self::put)）时就按 [`Filter`] 处理好——类型 / 权重 / 屏蔽词过滤、合并重复——
/// 之后只是按时间取，不再判断。**原始弹幕留着**，过滤规则变了对每一段重新处理一遍。
#[derive(Debug)]
pub struct Timeline {
    segment_ms: i64,
    filter: Filter,
    block_rules: BlockRules,
    raw: HashMap<i32, Vec<Danmaku>>,
    segments: HashMap<i32, Vec<Danmaku>>,
    /// 每段每条的文字宽度（像素，宿主量好后 [`set_widths`](Self::set_widths) 塞进来）。
    widths: HashMap<i32, Vec<f32>>,
    version: u32,
}

impl Timeline {
    /// `segment_ms`：一段多长（毫秒，B站 是 6 分钟）。
    pub fn new(segment_ms: i64, filter: Filter) -> Self {
        let block_rules = BlockRules::compile(&filter.rules);
        Self {
            segment_ms,
            filter,
            block_rules,
            raw: HashMap::new(),
            segments: HashMap::new(),
            widths: HashMap::new(),
            version: 0,
        }
    }

    pub fn segment_ms(&self) -> i64 {
        self.segment_ms
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn has(&self, segment: i32) -> bool {
        self.segments.contains_key(&segment)
    }

    /// 放进一段：按当前过滤规则处理一遍，**把处理好的列表交回宿主**（宿主拿它当"文字表"用，
    /// 排布表里的 id 指的就是这个列表的下标）。同时清掉这一段旧的宽度表（内容变了）。
    pub fn put(&mut self, segment: i32, list: Vec<Danmaku>) -> Vec<Danmaku> {
        let processed = self.filter.process(&list, &self.block_rules);
        self.raw.insert(segment, list);
        self.widths.remove(&segment);
        self.segments.insert(segment, processed.clone());
        processed
    }

    /// 塞进某一批弹幕的文字宽度（像素）：**一段一次**，顺序和 [`put`](Self::put) 交回的列表一致。
    /// 缺的按 0 处理（排布会当它宽度为 0，效果是"不会撞上别人"，不会崩）。
    pub fn set_widths(&mut self, segment: i32, widths: Vec<f32>) {
        self.widths.insert(segment, widths);
    }

    fn width_of(&self, segment: i32, index: usize) -> f32 {
        self.widths
            .get(&segment)
            .and_then(|w| w.get(index))
            .copied()
            .unwrap_or(0.0)
    }

    /// 换过滤规则。和现在的一样就什么都不做，返回 `false`；否则对已取回的每一段重新处理，返回 `true`
    /// （宿主这时候要重新量宽度、重新排）。屏蔽词没变就沿用编译好的正则，不重新编译。
    pub fn set_filter(&mut self, next: Filter) -> bool {
        self.set_filter_returning(next).is_some()
    }

    /// 换过滤规则，并把**重新处理过的每一段**交回宿主（宿主拿它重建文字表 / 重新量宽度）。
    /// `None` = 规则没变（什么都没做）。
    pub fn set_filter_returning(&mut self, next: Filter) -> Option<Vec<(i32, Vec<Danmaku>)>> {
        if next == self.filter {
            return None;
        }
        if next.rules != self.filter.rules {
            self.block_rules = BlockRules::compile(&next.rules);
        }
        self.filter = next;
        let raw = std::mem::take(&mut self.raw);
        self.segments.clear();
        self.widths.clear();
        let mut out: Vec<(i32, Vec<Danmaku>)> = Vec::with_capacity(raw.len());
        let mut segs: Vec<i32> = raw.keys().copied().collect();
        segs.sort_unstable();
        for segment in segs {
            if let Some(list) = raw.get(&segment) {
                let processed = self.filter.process(list, &self.block_rules);
                out.push((segment, processed.clone()));
                self.segments.insert(segment, processed);
            }
        }
        self.raw = raw;
        self.version += 1;
        Some(out)
    }

    /// 时间 `ms` 落在第几段（从 1 开始，和 B站 接口一致）。
    pub fn segment_of(&self, ms: i64) -> i32 {
        let ms = ms.max(0);
        (ms / self.segment_ms.max(1)) as i32 + 1
    }

    /// 处理好的弹幕一共几条。
    pub fn loaded_count(&self) -> usize {
        self.segments.values().map(|v| v.len()).sum()
    }

    /// 出现时间在 (`from_exclusive`, `to_inclusive`] 里的弹幕（带 id），按时间排。
    ///
    /// 段是按段号顺序扫的；同一段里用二分找第一条 `time_ms > from`。
    pub fn range(&self, from_exclusive: i64, to_inclusive: i64) -> Vec<(u64, Danmaku)> {
        if to_inclusive <= from_exclusive {
            return Vec::new();
        }
        let mut out: Vec<(u64, Danmaku)> = Vec::new();
        let mut segs: Vec<i32> = self.segments.keys().copied().collect();
        segs.sort_unstable();
        for seg in segs {
            // 一段整段都在范围外就跳过（避免每段都二分）
            let (seg_start, seg_end) = (
                (seg as i64 - 1) * self.segment_ms,
                seg as i64 * self.segment_ms,
            );
            if seg_end <= from_exclusive || seg_start > to_inclusive {
                continue;
            }
            let Some(list) = self.segments.get(&seg) else {
                continue;
            };
            let mut lo = 0usize;
            let mut hi = list.len();
            while lo < hi {
                let mid = (lo + hi) / 2;
                if list[mid].time_ms > from_exclusive {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            let mut i = lo;
            while i < list.len() && list[i].time_ms <= to_inclusive {
                out.push((danmaku_id(seg, i), list[i].clone()));
                i += 1;
            }
        }
        // 段号顺序 = 时间顺序，但跨段交界处仍要保证整体有序（同时间的保持段内顺序，稳定）
        out.sort_by_key(|(_, d)| d.time_ms);
        out
    }

    /// 出现时间 **严格大于** `after_ms` 的最近一条弹幕的时间（没有就是 `None`）。
    ///
    /// 宿主用它决定"下一次什么时候该再问排布"——于是排布不用每帧过一次 FFI：拿到的时刻之前
    /// 屏上的东西只会在既定的 `start` / `duration` 上走，纯算术。
    pub fn next_time_after(&self, after_ms: i64) -> Option<i64> {
        let mut best: Option<i64> = None;
        for list in self.segments.values() {
            // 二分找第一条 time_ms > after_ms
            let mut lo = 0usize;
            let mut hi = list.len();
            while lo < hi {
                let mid = (lo + hi) / 2;
                if list[mid].time_ms > after_ms {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            if let Some(d) = list.get(lo) {
                if best.map(|b| d.time_ms < b).unwrap_or(true) {
                    best = Some(d.time_ms);
                }
            }
        }
        best
    }

}

// ===========================================================================
// 装载策略（Kotlin `DanmakuLoader`）
// ===========================================================================

/// 按播放进度决定"该取哪一段"（Kotlin `DanmakuLoader.ensure` 的决策部分）。
///
/// **不发请求**：它只回答"现在要取哪几段"，宿主拿着返回的段号去取，取完（或失败）再
/// [`complete`](Self::complete) 回来。于是"什么时候取 / 隔多久重试 / 最多试几次 / 什么时候算到头"
/// 这条策略在 Rust，协程和收发在宿主。
#[derive(Debug, Clone)]
pub struct Loader {
    segment_ms: i64,
    duration_ms: Option<i64>,
    tries: HashMap<i32, u32>,
    last_try: HashMap<i32, i64>,
    in_flight: Vec<i32>,
}

impl Loader {
    pub fn new(segment_ms: i64, duration_ms: Option<i64>) -> Self {
        Self {
            segment_ms,
            duration_ms,
            tries: HashMap::new(),
            last_try: HashMap::new(),
            in_flight: Vec::new(),
        }
    }

    pub fn segment_of(&self, ms: i64) -> i32 {
        (ms.max(0) / self.segment_ms.max(1)) as i32 + 1
    }

    /// 视频一共几段；时长不知道就不设上限。
    pub fn last_segment(&self) -> i32 {
        match self.duration_ms.filter(|d| *d > 0) {
            None => i32::MAX,
            Some(d) => self.segment_of(d - 1),
        }
    }

    /// 取 `position_ms` 这一刻需要的段（当前段；过了这段的一半再带上下一段）。
    ///
    /// 返回**这次真的该去取**的段号（已有的、正在取的、试够次数的、还没到重试时间的、超出视频长度的
    /// 都不算），并把这些段标成"在飞"。宿主取完（成功 / 失败）要调 [`complete`](Self::complete)。
    pub fn ensure(&mut self, position_ms: i64, loaded: &dyn Fn(i32) -> bool, now_ms: i64) -> Vec<i32> {
        let seg = self.segment_of(position_ms);
        let into_segment = position_ms - (seg as i64 - 1) * self.segment_ms;
        let mut wanted = vec![seg];
        if into_segment * 2 >= self.segment_ms {
            wanted.push(seg + 1);
        }
        let mut out = Vec::new();
        for s in wanted {
            if s > self.last_segment() || loaded(s) || self.in_flight.contains(&s) {
                continue;
            }
            let n = self.tries.get(&s).copied().unwrap_or(0);
            if n >= MAX_TRIES {
                continue;
            }
            if let Some(last) = self.last_try.get(&s).copied() {
                if now_ms - last < RETRY_AFTER_MS {
                    continue;
                }
            }
            self.in_flight.push(s);
            self.tries.insert(s, n + 1);
            self.last_try.insert(s, now_ms);
            out.push(s);
        }
        out
    }

    /// 这一段取完了（成功或失败都算）：清掉"在飞"。
    pub fn complete(&mut self, segment: i32) {
        self.in_flight.retain(|s| *s != segment);
    }

    /// 这一段试了几次。
    pub fn tries(&self, segment: i32) -> u32 {
        self.tries.get(&segment).copied().unwrap_or(0)
    }

    /// 现在有几段在飞。
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

// ===========================================================================
// 排布（Kotlin `DanmakuLayout` + `DanmakuScheduler`）
// ===========================================================================

/// 影响轨道排布的那几项设置，**已换算成像素**（见 [`Settings::layout`]）。
///
/// 它变了，已经排好的轨道就作废、从当前时间点起重新排；透明度、描边这些只管绘制的不在里面，
/// 改它们不用重排。
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// 基础字号（像素，已含字号缩放）；每条弹幕再按自己的字号等级乘一个系数。
    pub font_px: f32,
    /// 字重 1~9（字重变了文字宽度会变，所以要重排）。
    pub font_weight: i32,
    /// 一条轨道的高度（像素）。
    pub lane_height: f32,
    /// 弹幕占画面高度的比例（0~1）。
    pub area: f32,
    /// 正常倍速下滚动弹幕走完全程的时间（毫秒）。
    pub scroll_ms: i64,
    /// 顶部 / 底部弹幕停留的时间（毫秒）。
    pub fixed_ms: i64,
    /// 要不要给顶部（底部）弹幕留轨道；这类弹幕被屏蔽了或者已经改成滚动弹幕，就不留。
    pub reserve_top: bool,
    pub reserve_bottom: bool,
}

/// 排布表里的一条：**这一条什么时候出现、走多久、在哪条轨道**（每帧只需要它 + 宽度）。
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    /// 弹幕 id（见 [`danmaku_id`]），宿主拿它回自己的文字 / 颜色表。
    pub id: u64,
    pub mode: DanmakuMode,
    /// 轨道号（滚动 / 顶部 / 底部各自的编号体系，见 [`Plan::y`]）。
    pub lane: i32,
    /// 出现时间（视频时间，毫秒）。
    pub start_ms: i64,
    pub duration_ms: i64,
    /// 文字宽度（像素）。
    pub width: f32,
    /// 排布时的视图宽度（像素），滚动弹幕的速度由它定。
    pub view_width: f32,
}

impl Planned {
    pub fn end_ms(&self) -> i64 {
        self.start_ms + self.duration_ms
    }

    /// 滚动弹幕的速度：像素 / 毫秒。
    pub fn speed(&self) -> f32 {
        (self.view_width + self.width) / self.duration_ms as f32
    }

    /// 滚动弹幕此刻左边缘的 x；顶部 / 底部弹幕居中。
    ///
    /// **每帧调用**（宿主对屏上几十条各调一次）：纯算术，不过 FFI。宿主那边有同样一行
    /// （`ActiveDanmaku.x`），两边口径由对拍盯着。
    pub fn x(&self, now_ms: i64, view_width: f32) -> f32 {
        match self.mode {
            DanmakuMode::Scroll => {
                view_width
                    - (view_width + self.width) * (now_ms - self.start_ms) as f32
                        / self.duration_ms as f32
            }
            _ => (view_width - self.width) / 2.0,
        }
    }
}

/// 一次排布过 FFI 的整批：**此刻在屏上的那些** + 这次用的轨道参数（宿主每帧只读这份，不再问 Rust）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub items: Vec<Planned>,
    pub view_width: f32,
    pub view_height: f32,
    pub lane_height: f32,
    pub usable_lanes: i32,
    pub scroll_lanes: i32,
    pub top_fixed_lanes: i32,
    pub bottom_fixed_lanes: i32,
    /// 下一次该来问的时刻 / 最早出屏的时刻（见 [`Scheduler::next_cue`]）。
    pub next_ms: Option<i64>,
}

impl Scheduler {
    /// 把"此刻的排布"打成一整批（[\`Plan\`]）交给宿主：一次过 FFI，画线程只读它。
    pub fn plan(&self, now_ms: i64, timeline: &Timeline) -> Plan {
        let cue = self.next_cue(now_ms, timeline);
        Plan {
            items: self.active.clone(),
            view_width: self.view_width,
            view_height: self.view_height,
            lane_height: self.lane_height,
            usable_lanes: self.usable_lanes,
            scroll_lanes: self.scroll_lanes,
            top_fixed_lanes: self.top_fixed_lanes,
            bottom_fixed_lanes: self.bottom_fixed_lanes,
            next_ms: cue.next_ms,
        }
    }
}

/// 下一次该来找排布器是什么时候（[`Scheduler::next_cue`] 的结果）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NextCue {
    /// 三条线索里最早的那个；`None` = 后面什么都没有了。
    pub next_ms: Option<i64>,
    /// 下一条弹幕出现的时刻。
    pub next_danmaku_ms: Option<i64>,
    /// 屏上最早出屏的时刻。
    pub next_expire_ms: Option<i64>,
}

impl Plan {
    /// 弹幕上边缘的 y（宿主每帧算一次）。
    pub fn y(&self, item: &Planned) -> f32 {
        match item.mode {
            DanmakuMode::Bottom => (self.usable_lanes - item.lane - 1) as f32 * self.lane_height,
            DanmakuMode::Scroll => (self.top_fixed_lanes + item.lane) as f32 * self.lane_height,
            _ => item.lane as f32 * self.lane_height,
        }
    }
}

/// 把到点的弹幕分到轨道上。
///
/// 过滤（类型、权重、屏蔽词、合并）在弹幕进 [`Timeline`] 时就做完了，这里只管排：
/// - 滚动弹幕：每条轨道上，新的一条出现时，前一条的尾巴必须已经完全进入屏幕，并且新的不会在前一条
///   出屏之前追上它（[`fits`](Self::fits)）；
/// - 顶部 / 底部弹幕：每条轨道同一时刻只停一条；顶部从上往下排，底部从弹幕区域的下沿往上排；
/// - 显示区域（[`Layout::area`]）：弹幕最多占画面高度的这么多，除以轨道高度就是轨道数；轨道数够多时，
///   顶部、底部各留一部分轨道（滚动弹幕在它们中间，三种不抢位置）。
#[derive(Debug)]
pub struct Scheduler {
    view_width: f32,
    view_height: f32,
    lane_height: f32,
    layout: Option<Layout>,
    /// 倍速：决定**新出现**的滚动弹幕走多快；已经排上的按自己的 duration 走完。
    pub playback_speed: f32,
    pub usable_lanes: i32,
    pub scroll_lanes: i32,
    pub top_fixed_lanes: i32,
    pub bottom_fixed_lanes: i32,
    scroll_last: Vec<Option<Planned>>,
    top_busy: Vec<i64>,
    bottom_busy: Vec<i64>,
    /// 上一次对齐 / 推进到的位置（`None` = 还没排过）。
    last_ms: Option<i64>,
    /// 上次排布时时间线的版本（过滤规则变了就要重新对齐）。
    seen_version: u32,
    /// 屏上的这些（排布的**状态**：轨道占用由"已经排上的"推算，见 [`place_scroll`](Self::place_scroll)）。
    active: Vec<Planned>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            view_width: 0.0,
            view_height: 0.0,
            lane_height: 0.0,
            layout: None,
            playback_speed: 1.0,
            usable_lanes: 0,
            scroll_lanes: 0,
            top_fixed_lanes: 0,
            bottom_fixed_lanes: 0,
            scroll_last: Vec::new(),
            top_busy: Vec::new(),
            bottom_busy: Vec::new(),
            last_ms: None,
            seen_version: 0,
            active: Vec::new(),
        }
    }

    /// 上一次排到的位置（`None` = 还没排过）。
    pub fn last_ms(&self) -> Option<i64> {
        self.last_ms
    }

    /// 交给画线程的那一份（只读）。
    pub fn active(&self) -> &[Planned] {
        &self.active
    }

    fn scroll_ms(&self) -> i64 {
        scroll_ms(
            self.playback_speed,
            self.layout.as_ref().map(|l| l.scroll_ms).unwrap_or(SCROLL_MS),
        )
    }

    fn fixed_ms(&self) -> i64 {
        self.layout.as_ref().map(|l| l.fixed_ms).unwrap_or(FIXED_MS)
    }

    pub fn view_width(&self) -> f32 {
        self.view_width
    }

    pub fn lane_height(&self) -> f32 {
        self.lane_height
    }

    /// 视图尺寸或排布变了就重新算轨道数（轨道数量变了，旧的排布作废）；都没变什么都不做。
    /// 返回 `true` = 真的变了（宿主据此重新 [`plan_from`](Self::plan_from)）。
    pub fn configure(&mut self, width: f32, height: f32, layout: Layout) -> bool {
        if width == self.view_width
            && height == self.view_height
            && self.layout.as_ref() == Some(&layout)
        {
            return false;
        }
        self.view_width = width;
        self.view_height = height;
        self.layout = Some(layout.clone());
        self.lane_height = layout.lane_height;
        let band = height * layout.area.clamp(0.0, 1.0);
        // 区域连一条轨道都放不下时：画面放得下一条就给一条，免得调小了显示区域弹幕就一条都没有
        self.usable_lanes = if self.lane_height <= 0.0 {
            0
        } else {
            let fit = (band / self.lane_height) as i32;
            let floor = if height >= self.lane_height { 1 } else { 0 };
            fit.max(floor)
        };
        self.top_fixed_lanes = if layout.reserve_top && self.usable_lanes >= 3 {
            1.max(self.usable_lanes / 5)
        } else {
            0
        };
        self.bottom_fixed_lanes = if layout.reserve_bottom && self.usable_lanes >= 4 {
            1.max(self.usable_lanes / 6)
        } else {
            0
        };
        self.scroll_lanes = if self.usable_lanes == 0 {
            0
        } else {
            (self.usable_lanes - self.top_fixed_lanes - self.bottom_fixed_lanes).max(1)
        };
        self.reset_state();
        true
    }

    fn reset_state(&mut self) {
        self.scroll_last = vec![None; self.scroll_lanes.max(0) as usize];
        self.top_busy = vec![i64::MIN; self.top_fixed_lanes.max(0) as usize];
        self.bottom_busy = vec![i64::MIN; self.bottom_fixed_lanes.max(0) as usize];
    }

    /// 从 `position_ms` 起重新对齐：清空轨道，把 `position_ms - 一个周期` 之后、`position_ms` 之前出现的
    /// 弹幕按顺序排一遍，然后去掉已经出屏的。返回**此刻在屏上的**那些（宿主拿它整批换掉画面上那一份）。
    ///
    /// 什么时候要调：换清晰度、改设置、跳转、**换了倍速**（已经排上的弹幕是照旧走完的，新出现的用新倍速）、
    /// 以及时间线被过滤规则改过（[`Timeline::version`] 变了）。**不是每帧**。
    pub fn reset(&mut self, position_ms: i64, timeline: &Timeline) -> Vec<Planned> {
        self.reset_state();
        self.last_ms = Some(position_ms);
        self.seen_version = timeline.version();
        self.active.clear();
        if self.scroll_lanes == 0 {
            return Vec::new();
        }
        let window = self.scroll_ms().max(self.fixed_ms());
        for (id, d) in timeline.range(position_ms - window, position_ms) {
            let (segment, index) = split_id(id);
            let width = timeline.width_of(segment, index);
            if let Some(a) = self.place(&d, id, width) {
                self.active.push(a);
            }
        }
        self.active.retain(|a| a.end_ms() > position_ms);
        self.active.clone()
    }

    /// 和 Kotlin `DanmakuScheduler.advance` **逐行对应**：把 [last_ms, now_ms] 之间到点的弹幕排上、
    /// 去掉出屏的，返回此刻该画的。
    ///
    /// 什么时候调：**只在"有新的弹幕要出现"时**——宿主拿 [`NextCue`] 里的 `next_ms` 排一次定时
    /// （或者到了下一条弹幕的时间），不必每帧过一次 FFI。跳转 / 往回走 / 版本变了会自动重新对齐。
    pub fn advance(&mut self, now_ms: i64, timeline: &Timeline) -> Vec<Planned> {
        let need_reset = match self.last_ms {
            None => true,
            Some(last) => {
                now_ms < last || now_ms - last > JUMP_MS || timeline.version() != self.seen_version
            }
        };
        if need_reset {
            return self.reset(now_ms, timeline);
        }
        let last = self.last_ms.unwrap_or(now_ms);
        if now_ms > last && self.scroll_lanes > 0 {
            for (id, d) in timeline.range(last, now_ms) {
                let (segment, index) = split_id(id);
                let width = timeline.width_of(segment, index);
                if let Some(a) = self.place(&d, id, width) {
                    self.active.push(a);
                }
            }
        }
        self.last_ms = Some(now_ms);
        self.active.retain(|a| a.end_ms() > now_ms);
        self.active.clone()
    }

    /// 下一次该来找我是什么时候（宿主据此排一次定时，**不用每帧问**）。
    ///
    /// 三条线索取最早的：
    /// - 时间线里下一条出现的时刻（[`Timeline::next_time_after`]）；
    /// - 屏上最早出屏的那条的 `end_ms`（它得从排布表里去掉）；
    /// - 屏上最早"该被重新考虑"的时刻（滚动弹幕的轨道在它出屏前就空出来了，但那不影响画面）。
    pub fn next_cue(&self, now_ms: i64, timeline: &Timeline) -> NextCue {
        let next_danmaku = timeline.next_time_after(now_ms);
        let next_expire = self.active.iter().map(|a| a.end_ms()).min();
        NextCue {
            next_ms: match (next_danmaku, next_expire) {
                (None, None) => None,
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (Some(a), Some(b)) => Some(a.min(b)),
            },
            next_danmaku_ms: next_danmaku,
            next_expire_ms: next_expire,
        }
    }

    /// 现在屏上有几条（检查工具 / 测试用）。
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    fn place(&mut self, d: &Danmaku, id: u64, width: f32) -> Option<Planned> {
        match d.mode {
            DanmakuMode::Scroll => self.place_scroll(d, id, width),
            DanmakuMode::Top => {
                let stay = self.fixed_ms();
                place_fixed(
                    d,
                    id,
                    width,
                    &mut self.top_busy,
                    DanmakuMode::Top,
                    stay,
                    self.view_width,
                )
            }
            DanmakuMode::Bottom => {
                let stay = self.fixed_ms();
                place_fixed(
                    d,
                    id,
                    width,
                    &mut self.bottom_busy,
                    DanmakuMode::Bottom,
                    stay,
                    self.view_width,
                )
            }
            DanmakuMode::Other => None,
        }
    }

    fn place_scroll(&mut self, d: &Danmaku, id: u64, width: f32) -> Option<Planned> {
        let tn = d.time_ms;
        let dur = self.scroll_ms();
        if dur <= 0 {
            return None;
        }
        let sn = (self.view_width + width) / dur as f32;
        for lane in 0..self.scroll_lanes.max(0) {
            let prev = self.scroll_last.get(lane as usize).cloned().flatten();
            let fits = match &prev {
                None => true,
                Some(p) => p.end_ms() <= tn || self.fits(p, tn, sn),
            };
            if fits {
                let a = Planned {
                    id,
                    mode: DanmakuMode::Scroll,
                    lane,
                    start_ms: tn,
                    duration_ms: dur,
                    width,
                    view_width: self.view_width,
                };
                self.scroll_last[lane as usize] = Some(a.clone());
                return Some(a);
            }
        }
        // 轨道都占着：丢掉这条（弹幕多少由轨道数限制）
        None
    }

    /// 新弹幕（`tn` 出现、速度 `sn`）接在 `prev` 后面会不会撞上。
    fn fits(&self, prev: &Planned, tn: i64, sn: f32) -> bool {
        let prev_speed = prev.speed();
        // NaN 防御：轨道里那条的时长是 0 时 speed() 会是 inf，比较结果按 Kotlin 的 NaN 语义走
        let tail_entered = prev_speed * (tn - prev.start_ms) as f32 >= prev.width + LANE_GAP_PX;
        let no_catch_up = sn * (prev.end_ms() - tn) as f32 <= self.view_width;
        tail_entered && no_catch_up
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

fn place_fixed(
    d: &Danmaku,
    id: u64,
    width: f32,
    busy: &mut [i64],
    mode: DanmakuMode,
    stay: i64,
    view_width: f32,
) -> Option<Planned> {
    let tn = d.time_ms;
    for lane in 0..busy.len() {
        if busy[lane] <= tn {
            busy[lane] = tn + stay;
            return Some(Planned {
                id,
                mode,
                lane: lane as i32,
                start_ms: tn,
                duration_ms: stay,
                width,
                view_width,
            });
        }
    }
    None
}

// ===========================================================================
// 设置模型（Kotlin `DanmakuSettings` 的换算部分）
// ===========================================================================

/// 显示区域的八档：1/8、1/4 …… 全屏。
pub const AREA_OPTIONS: [f32; 8] = [0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];

/// 屏蔽词最多存多少条。
pub const MAX_RULES: usize = 300;

/// 一条屏蔽词最长多少字（正则太长没意义，也防着手滑粘进一大段）。
pub const MAX_RULE_LENGTH: usize = 200;

/// 弹幕设置（Kotlin `DanmakuSettings`）。
///
/// 字段名和默认值一对一（`@Serializable` 的落盘格式在宿主那边，这一层只管模型 + 换算）。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// 透明度 0.3~1。
    pub opacity: f32,
    /// 字号缩放 0.5~2。
    pub font_scale: f32,
    /// 字重 1~9。
    pub font_weight: i32,
    /// 描边粗细 0~4 dp。
    pub stroke_width: f32,
    pub area: f32,
    pub line_height: f32,
    /// 海量模式：行距压到 3/4（不低于 1.0）。
    pub massive: bool,
    pub scroll_seconds: f32,
    pub fixed_velocity: bool,
    pub fixed_seconds: f32,
    pub static_to_scroll: bool,
    pub allow_scroll: bool,
    pub allow_top: bool,
    pub allow_bottom: bool,
    pub allow_colorful: bool,
    pub weight_level: i32,
    pub merge_duplicates: bool,
    pub merge_window_ms: i32,
    pub merge_threshold: i32,
    pub block_rules: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            opacity: 0.85,
            font_scale: 1.0,
            font_weight: 4,
            stroke_width: 1.5,
            area: 0.5,
            line_height: 1.4,
            massive: false,
            scroll_seconds: 8.0,
            fixed_velocity: false,
            fixed_seconds: 4.0,
            static_to_scroll: false,
            allow_scroll: true,
            allow_top: true,
            allow_bottom: true,
            allow_colorful: true,
            weight_level: 0,
            merge_duplicates: true,
            merge_window_ms: 500,
            merge_threshold: 2,
            block_rules: Vec::new(),
        }
    }
}

impl Settings {
    /// 过滤用的那几项（[`Filter`]）。
    pub fn filter(&self) -> Filter {
        Filter {
            allow_scroll: self.allow_scroll,
            allow_top: self.allow_top,
            allow_bottom: self.allow_bottom,
            allow_colorful: self.allow_colorful,
            weight_level: self.weight_level,
            rules: self.block_rules.clone(),
            static_to_scroll: self.static_to_scroll,
            merge: self.merge_duplicates,
            merge_window_ms: self.merge_window_ms as i64,
            merge_threshold: self.merge_threshold,
        }
    }

    /// 在 `view_width × view_height`（像素，`density` 是每 dp 多少像素）的画面上，排布要用的那几项
    /// 换算成像素。字号随播放器高度走：高度的 1/25，但在 12~22 dp 之间，再乘 [`font_scale`](Self::font_scale)。
    pub fn layout(&self, view_width: f32, view_height: f32, density: f32) -> Layout {
        let font_px = (view_height / 25.0).clamp(12.0 * density, 22.0 * density) * self.font_scale;
        let lines = if self.massive {
            massive_line_height(self.line_height)
        } else {
            self.line_height
        };
        let width_factor = if self.fixed_velocity {
            fixed_velocity_factor(view_width / density)
        } else {
            1.0
        };
        Layout {
            font_px,
            font_weight: self.font_weight,
            lane_height: font_px * lines,
            area: self.area,
            scroll_ms: (self.scroll_seconds * 1000.0 * width_factor) as i64,
            fixed_ms: (self.fixed_seconds * 1000.0) as i64,
            reserve_top: self.allow_top && !self.static_to_scroll,
            reserve_bottom: self.allow_bottom && !self.static_to_scroll,
        }
    }

    /// 把超出范围的值收回范围里（手改了存档、以后改了范围时用）；屏蔽词去掉空白、重复和过长的。
    pub fn normalized(&self) -> Settings {
        let mut out = self.clone();
        out.opacity = clamp_or(self.opacity, 0.3, 1.0, 0.85);
        out.font_scale = clamp_or(self.font_scale, 0.5, 2.0, 1.0);
        out.font_weight = self.font_weight.clamp(1, 9);
        out.stroke_width = clamp_or(self.stroke_width, 0.0, 4.0, 1.5);
        out.area = nearest_area(self.area);
        out.line_height = clamp_or(self.line_height, 0.8, 2.2, 1.4);
        out.scroll_seconds = clamp_or(self.scroll_seconds, 2.0, 15.0, 8.0);
        out.fixed_seconds = clamp_or(self.fixed_seconds, 2.0, 15.0, 4.0);
        out.weight_level = self.weight_level.clamp(0, 10);
        out.merge_window_ms = self.merge_window_ms.clamp(100, 3000);
        out.merge_threshold = self.merge_threshold.clamp(2, 10);
        let mut rules: Vec<String> = Vec::new();
        for rule in &self.block_rules {
            let t = crate::hosted::kotlin_trim(rule);
            if t.is_empty() || t.chars().count() > MAX_RULE_LENGTH {
                continue;
            }
            if !rules.iter().any(|r| r == t) {
                rules.push(t.to_string());
            }
            if rules.len() >= MAX_RULES {
                break;
            }
        }
        out.block_rules = rules;
        out
    }

    /// 恢复默认，但屏蔽词是用户自己攒的，留着。
    pub fn reset(&self) -> Settings {
        Settings {
            block_rules: self.block_rules.clone(),
            ..Settings::default()
        }
    }

    /// 旧版只有一个"弹幕密度"（0~100）：最多用画面高度的 `0.25 + 0.55 × 密度` 排弹幕，并且丢掉权重低于
    /// `round((100 − 密度) / 100 × 6)` 的弹幕。读到旧存档时按这两个公式换算过来（显示区域取最近的一档）。
    pub fn from_legacy_density(density: i32) -> Settings {
        let d = density.clamp(0, 100) as f32;
        Settings {
            area: nearest_area(0.25 + 0.55 * d / 100.0),
            weight_level: round_half_up((100.0 - d) / 100.0 * 6.0) as i32,
            ..Settings::default()
        }
    }
}

/// 海量模式下的行高：压到 3/4，但不低于 1.0（本来就小于 1.0 的不动）。
///
/// **`NaN` 要原样传出去**：Kotlin 是 `minOf(lineHeight, maxOf(1f, lineHeight * 0.75f))`，
/// 而 `maxOf` / `minOf` 在 JVM 上就是 `Math.max` / `Math.min`——只要有一边是 `NaN` 结果就是 `NaN`。
/// Rust 的 `f32::min` / `max` 反过来会**吞掉** `NaN`（返回另一边），对拍当场抓出 `NaN` vs `1.0`。
/// 生产路径上 `NaN` 只可能来自手改坏的存档（`Settings::normalized` 会把它折回 1.4），
/// 但口径必须和原来一字不差。
pub fn massive_line_height(line_height: f32) -> f32 {
    if line_height.is_nan() {
        return f32::NAN;
    }
    line_height.min(1.0f32.max(line_height * 0.75))
}

/// 离 `value` 最近的一档显示区域（`NaN` → 0.5，和 Kotlin 的 `isFinite` 判断一样）。
pub fn nearest_area(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.5;
    }
    let mut best = AREA_OPTIONS[0];
    let mut best_d = (best - value).abs();
    for a in AREA_OPTIONS.iter().skip(1) {
        let d = (a - value).abs();
        if d < best_d {
            best = *a;
            best_d = d;
        }
    }
    best
}

fn clamp_or(value: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(lo, hi)
    } else {
        fallback
    }
}

/// Kotlin 的 `roundToInt()`：`floor(x + 0.5)`。
fn round_half_up(v: f32) -> f64 {
    (v as f64 + 0.5).floor()
}

