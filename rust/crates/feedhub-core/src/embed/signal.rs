//! **行为信号**：把库里已有的六种行为折成带符号、带时间衰减的信号。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/BehaviorSignal.kt` 的 Rust 版。
//! 规则一条没改（权重 / 半衰期 / 阈值 / 去重口径都在下面这张表里），只把"读库"留在宿主
//! （[`Snapshot`] 是一份死数据，谁给它不归这里管）。
//!
//! | 信号 | 来源 | 基础权重 | 半衰期 |
//! |---|---|---|---|
//! | [`SignalKind::Favorite`] 收藏 | `favorite.added_at` | **+1.00** | 30 天 |
//! | [`SignalKind::Visit`] 点开 | `history.visited_at` | +0.35 | 21 天 |
//! | [`SignalKind::WatchDone`] 看完 | 进度 ≥90% 且时长 ≥60 s | +0.65 | 21 天 |
//! | [`SignalKind::DwellLong`] 看得久 | `surface=detail` 且停留 ≥60 s | +0.45 | 21 天 |
//! | [`SignalKind::QuickBack`] 点开就退 | `surface=detail` 且 1 s ≤ 停留 < 5 s | **−0.25** | 14 天 |
//! | [`SignalKind::Skip`] 划过没点 | 首页 / 关注曝光之后没打开过 | −0.08 | 7 天 |
//! | [`SignalKind::BlockedAuthor`] 屏蔽作者 | `blocked_user.blocked_at` | **−1.00** | 120 天 |
//! | [`SignalKind::BlockedWord`] 命中屏蔽词 | 设置里的 `blockWords` | −0.50 | **不衰减** |
//!
//! 时间衰减 `0.5 ^ (age / 半衰期)`：见 [`signal_decay`]。输出按 `(at, key, kind)` 稳定排序，
//! 字符串比大小走 UTF-16 码元（Java `String.compareTo`，见 [`super::text::cmp_utf16`]）。

use std::collections::{HashMap, HashSet};

use crate::brain::filter::{self, MixText, Rules};
use crate::hosted::is_blank;

use super::item::EmbedItem;
use super::text::cmp_utf16;

/// 一条信号 = 「对哪条内容、什么动作、什么时候、有多强」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    Favorite,
    Visit,
    WatchDone,
    DwellLong,
    QuickBack,
    Skip,
    BlockedAuthor,
    BlockedWord,
}

impl SignalKind {
    /// 全部信号（枚举顺序 = 导出 / 测试里的 `ordinal`，**别改顺序**）。
    pub const ALL: [SignalKind; 8] = [
        SignalKind::Favorite,
        SignalKind::Visit,
        SignalKind::WatchDone,
        SignalKind::DwellLong,
        SignalKind::QuickBack,
        SignalKind::Skip,
        SignalKind::BlockedAuthor,
        SignalKind::BlockedWord,
    ];

    /// 设置页 / 导出里给人看的名字。
    pub fn label(self) -> &'static str {
        match self {
            SignalKind::Favorite => "收藏",
            SignalKind::Visit => "点开",
            SignalKind::WatchDone => "看完",
            SignalKind::DwellLong => "看得久",
            SignalKind::QuickBack => "点开就退",
            SignalKind::Skip => "划过没点",
            SignalKind::BlockedAuthor => "屏蔽作者",
            SignalKind::BlockedWord => "命中屏蔽词",
        }
    }

    /// 基础权重（带符号；正 = 喜欢，负 = 不喜欢）。
    pub fn base_weight(self) -> f32 {
        match self {
            SignalKind::Favorite => 1.00,
            SignalKind::Visit => 0.35,
            SignalKind::WatchDone => 0.65,
            SignalKind::DwellLong => 0.45,
            SignalKind::QuickBack => -0.25,
            SignalKind::Skip => -0.08,
            SignalKind::BlockedAuthor => -1.00,
            SignalKind::BlockedWord => -0.50,
        }
    }

    /// 多少天衰减一半（[`SIGNAL_NO_DECAY`] = 不衰减）。
    pub fn half_life_days(self) -> f64 {
        match self {
            SignalKind::Favorite => 30.0,
            SignalKind::Visit => 21.0,
            SignalKind::WatchDone => 21.0,
            SignalKind::DwellLong => 21.0,
            SignalKind::QuickBack => 14.0,
            SignalKind::Skip => 7.0,
            SignalKind::BlockedAuthor => 120.0,
            SignalKind::BlockedWord => SIGNAL_NO_DECAY,
        }
    }

    pub fn is_positive(self) -> bool {
        self.base_weight() > 0.0
    }

    /// 导出的 `kind` 字段（小写枚举名，和 Kotlin `sig.kind.name.lowercase()` 一致）。
    pub fn key(self) -> &'static str {
        match self {
            SignalKind::Favorite => "favorite",
            SignalKind::Visit => "visit",
            SignalKind::WatchDone => "watch_done",
            SignalKind::DwellLong => "dwell_long",
            SignalKind::QuickBack => "quick_back",
            SignalKind::Skip => "skip",
            SignalKind::BlockedAuthor => "blocked_author",
            SignalKind::BlockedWord => "blocked_word",
        }
    }

    /// 按 [`SignalKind::key`] 反查（读导出 / 测试用）。
    pub fn from_key(key: &str) -> Option<SignalKind> {
        SignalKind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Kotlin 枚举名（`FAVORITE` / `WATCH_DONE` / …）。**FFI / 计数 / 对拍夹具共用这一份**。
    pub fn name(self) -> &'static str {
        match self {
            SignalKind::Favorite => "FAVORITE",
            SignalKind::Visit => "VISIT",
            SignalKind::WatchDone => "WATCH_DONE",
            SignalKind::DwellLong => "DWELL_LONG",
            SignalKind::QuickBack => "QUICK_BACK",
            SignalKind::Skip => "SKIP",
            SignalKind::BlockedAuthor => "BLOCKED_AUTHOR",
            SignalKind::BlockedWord => "BLOCKED_WORD",
        }
    }

    /// 按 [`SignalKind::name`] 反查（大小写敏感：Kotlin 枚举名就是这个口径）。
    pub fn from_name(name: &str) -> Option<SignalKind> {
        SignalKind::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// [`SignalKind::half_life_days`] 用这个值表示**不衰减**。
pub const SIGNAL_NO_DECAY: f64 = 0.0;

/// 详情页停留多久算"看得久"（毫秒）。
pub const DWELL_LONG_MS: i64 = 60_000;
/// 详情页停留少于多久算"点开就退"（毫秒）。
pub const QUICK_BACK_MS: i64 = 5_000;
/// 详情曝光本身的门槛：少于 1 秒的不结算，出现了也当脏数据。
pub const DETAIL_MIN_DWELL_MS: i64 = 1_000;
/// 视频多长以上才判"看完"（毫秒）。
pub const WATCH_DONE_MIN_DURATION_MS: i64 = 60_000;
/// 视频看到多少比例算"看完"。
///
/// **必须等于 Kotlin 的 `0.9f`**（`WATCH_DONE_RATIO: Float`）：Kotlin 那边 `ratio`（Double）减这个 Float 常量时
/// 会先把它提升成 Double，值是 `0.899999976158142`。写成 `0.9`（Double 字面量）会差 1～2 ULP，对拍当场抓出来。
pub const WATCH_DONE_RATIO: f64 = 0.9f32 as f64;

const DAY_MS: f64 = 86_400_000.0;

/// 时间衰减：`0.5 ^ (age / 半衰期)`。
///
/// `half_life_days <= 0` = 不衰减；`age <= 0`（时钟倒退 / 未来的信号）= 1。
pub fn signal_decay(age_ms: i64, half_life_days: f64) -> f32 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    if age_ms <= 0 {
        return 1.0;
    }
    (0.5f64.powf(age_ms as f64 / (half_life_days * DAY_MS))) as f32
}

/// 一条行为信号。
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorSignal {
    pub key: String,
    pub kind: SignalKind,
    /// 动作发生的时刻（收藏时间 / 打开时间 / 屏蔽时间 / 曝光结束时间）。
    pub at: i64,
    /// 同类信号内部的强弱（0～1）。
    pub strength: f32,
    pub platform: Option<String>,
}

impl BehaviorSignal {
    /// 到 `now` 这一刻的权重：基础权重 × 强度 × 时间衰减。
    pub fn effective_weight(&self, now: i64) -> f32 {
        let strength = self.strength.clamp(0.0, 1.0);
        self.kind.base_weight() * strength * signal_decay(now - self.at, self.kind.half_life_days())
    }
}

/// 曝光发生在哪个面（口径同 Kotlin `ExposureSurface.fromKey`：认不出的当 `Home`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Home,
    Following,
    Detail,
    Favorites,
    History,
    Related,
}

impl Surface {
    pub fn from_key(key: &str) -> Surface {
        match key {
            "following" => Surface::Following,
            "detail" => Surface::Detail,
            "favorites" => Surface::Favorites,
            "history" => Surface::History,
            "related" => Surface::Related,
            _ => Surface::Home,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Surface::Home => "home",
            Surface::Following => "following",
            Surface::Detail => "detail",
            Surface::Favorites => "favorites",
            Surface::History => "history",
            Surface::Related => "related",
        }
    }
}

/// 一条曝光（`Library.exposureRows` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureRow {
    pub key: String,
    pub platform: String,
    pub surface: Surface,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

/// 收藏里的一条（只要算信号用得到的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteRow {
    pub item: EmbedItem,
    pub added_at: i64,
}

/// 历史里的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub item: EmbedItem,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// 曝光过的卡片（`seen_item` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenRow {
    pub item: EmbedItem,
    pub first_seen_at: i64,
}

/// 屏蔽的作者（[`BlockedAuthorRow::author_key`] = `platform:id`，Kotlin `UserRef.blockKey()`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedAuthorRow {
    pub author_key: String,
    pub blocked_at: i64,
}

/// 从库里读出来的**一份死数据**：算信号要的全部原始行为。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub favorites: Vec<FavoriteRow>,
    pub history: Vec<HistoryRow>,
    pub exposures: Vec<ExposureRow>,
    pub seen: Vec<SeenRow>,
    pub blocked_authors: Vec<BlockedAuthorRow>,
    /// 只放"屏蔽词"（一直生效的那份）；外出模式的关键词是临时状态，**不算口味**。
    pub block_words: Vec<String>,
}

/// 把一份快照折成信号列表（纯函数）。
///
/// 输出按 `(at, key, kind)` 稳定排序——和 Kotlin `compareBy({ it.at }, { it.key }, { it.kind.ordinal })` 一样。
pub fn build_signals(snapshot: &Snapshot) -> Vec<BehaviorSignal> {
    let mut out: Vec<BehaviorSignal> =
        Vec::with_capacity(snapshot.favorites.len() + snapshot.history.len() * 2);

    // 1) 收藏 / 点开 / 看完
    for f in &snapshot.favorites {
        if f.item.adult {
            continue;
        }
        out.push(BehaviorSignal {
            key: f.item.key.clone(),
            kind: SignalKind::Favorite,
            at: f.added_at,
            strength: 1.0,
            platform: Some(f.item.platform.clone()),
        });
    }
    for h in &snapshot.history {
        if h.item.adult {
            continue;
        }
        out.push(BehaviorSignal {
            key: h.item.key.clone(),
            kind: SignalKind::Visit,
            at: h.visited_at,
            strength: 1.0,
            platform: Some(h.item.platform.clone()),
        });
        if let (Some(duration), Some(progress)) = (h.duration_ms, h.progress_ms) {
            if duration >= WATCH_DONE_MIN_DURATION_MS {
                let ratio = ((progress as f64) / (duration as f64)).clamp(0.0, 1.0);
                if ratio >= WATCH_DONE_RATIO {
                    let strength =
                        (0.5 + 0.5 * (ratio - WATCH_DONE_RATIO) / (1.0 - WATCH_DONE_RATIO)) as f32;
                    out.push(BehaviorSignal {
                        key: h.item.key.clone(),
                        kind: SignalKind::WatchDone,
                        at: h.visited_at,
                        strength: strength.clamp(0.0, 1.0),
                        platform: Some(h.item.platform.clone()),
                    });
                }
            }
        }
    }

    // 曝光行里没有 18+ 标记，先从有内容的表里收集一份"这条被标了 18+"
    let mut adult_keys: HashSet<&str> = HashSet::new();
    for f in &snapshot.favorites {
        if f.item.adult {
            adult_keys.insert(f.item.key.as_str());
        }
    }
    for h in &snapshot.history {
        if h.item.adult {
            adult_keys.insert(h.item.key.as_str());
        }
    }
    for s in &snapshot.seen {
        if s.item.adult {
            adult_keys.insert(s.item.key.as_str());
        }
    }

    // 2) 详情停留：同一条内容只判"最久的那次"（插入顺序 = 第一次出现的顺序）
    let mut detail_dwell: Vec<&ExposureRow> = Vec::new();
    {
        let mut at: HashMap<&str, usize> = HashMap::new();
        for e in &snapshot.exposures {
            if e.surface != Surface::Detail {
                continue;
            }
            if adult_keys.contains(e.key.as_str()) {
                continue;
            }
            match at.get(e.key.as_str()) {
                Some(idx) => {
                    if e.dwell_ms > detail_dwell[*idx].dwell_ms {
                        detail_dwell[*idx] = e;
                    }
                }
                None => {
                    at.insert(e.key.as_str(), detail_dwell.len());
                    detail_dwell.push(e);
                }
            }
        }
    }
    for e in detail_dwell {
        if e.dwell_ms >= DWELL_LONG_MS {
            // 1 分钟 → 0.33，3 分钟以上 → 1
            let strength = ((e.dwell_ms as f64) / (DWELL_LONG_MS as f64 * 3.0)) as f32;
            out.push(BehaviorSignal {
                key: e.key.clone(),
                kind: SignalKind::DwellLong,
                at: e.last_seen_at,
                strength: strength.clamp(0.0, 1.0),
                platform: Some(e.platform.clone()),
            });
        } else if e.dwell_ms >= DETAIL_MIN_DWELL_MS && e.dwell_ms < QUICK_BACK_MS {
            out.push(BehaviorSignal {
                key: e.key.clone(),
                kind: SignalKind::QuickBack,
                at: e.last_seen_at,
                strength: 1.0,
                platform: Some(e.platform.clone()),
            });
        }
    }

    // 3) 划过没点：首页 / 关注面的曝光，合并成一条，曝光之后打开过就不算
    let mut click_times: HashMap<&str, i64> = HashMap::with_capacity(snapshot.history.len() * 2);
    for h in &snapshot.history {
        if h.item.adult {
            continue;
        }
        let slot = click_times.entry(h.item.key.as_str()).or_insert(h.visited_at);
        *slot = (*slot).max(h.visited_at);
    }
    let mut skipped: Vec<ExposureRow> = Vec::new();
    {
        let mut at: HashMap<&str, usize> = HashMap::new();
        for e in &snapshot.exposures {
            if e.surface != Surface::Home && e.surface != Surface::Following {
                continue;
            }
            if adult_keys.contains(e.key.as_str()) {
                continue;
            }
            match at.get(e.key.as_str()) {
                Some(idx) => {
                    let old = &skipped[*idx];
                    skipped[*idx] = ExposureRow {
                        key: e.key.clone(),
                        platform: e.platform.clone(),
                        surface: e.surface,
                        exposed_at: old.exposed_at.min(e.exposed_at),
                        last_seen_at: old.last_seen_at.max(e.last_seen_at),
                        dwell_ms: old.dwell_ms.max(e.dwell_ms),
                    };
                }
                None => {
                    at.insert(e.key.as_str(), skipped.len());
                    skipped.push(e.clone());
                }
            }
        }
    }
    for e in &skipped {
        let clicked = click_times
            .get(e.key.as_str())
            .map(|visited| *visited >= e.exposed_at)
            .unwrap_or(false);
        if clicked {
            continue;
        }
        out.push(BehaviorSignal {
            key: e.key.clone(),
            kind: SignalKind::Skip,
            at: e.last_seen_at,
            strength: 1.0,
            platform: Some(e.platform.clone()),
        });
    }

    // 4) 负向的"内容级"信号：把两条规则作用到库里有内容的每张卡片上
    let mut items: Vec<(&EmbedItem, i64)> = Vec::new();
    {
        let mut seen_keys: HashSet<&str> = HashSet::new();
        for s in &snapshot.seen {
            if seen_keys.insert(s.item.key.as_str()) {
                items.push((&s.item, s.first_seen_at));
            }
        }
        for h in &snapshot.history {
            if seen_keys.insert(h.item.key.as_str()) {
                items.push((&h.item, h.visited_at));
            }
        }
        for f in &snapshot.favorites {
            if seen_keys.insert(f.item.key.as_str()) {
                items.push((&f.item, f.added_at));
            }
        }
    }

    let blocked: HashMap<&str, i64> = snapshot
        .blocked_authors
        .iter()
        .map(|b| (b.author_key.as_str(), b.blocked_at))
        .collect();
    if !blocked.is_empty() {
        for (item, item_at) in &items {
            if item.adult {
                continue;
            }
            let author = match item.author_block_key() {
                Some(key) => key,
                None => continue,
            };
            let blocked_at = match blocked.get(author.as_str()) {
                Some(v) => *v,
                None => continue,
            };
            // 实现没给屏蔽时间（0）时退回"这张卡片第一次出现"的时刻
            let at = if blocked_at > 0 { blocked_at } else { *item_at };
            out.push(BehaviorSignal {
                key: item.key.clone(),
                kind: SignalKind::BlockedAuthor,
                at,
                strength: 1.0,
                platform: Some(item.platform.clone()),
            });
        }
    }
    if !snapshot.block_words.is_empty() {
        // 只把屏蔽词给过滤规则（外出模式关着），所以"外出模式关键词"不会被当成口味。
        let rules = Rules {
            block_words: snapshot.block_words.clone(),
            ..Default::default()
        };
        let candidates: Vec<MixText> = items
            .iter()
            .filter(|(item, _)| !item.adult)
            .map(|(item, _)| mix_text(item))
            .collect();
        let visible = filter::visible(&candidates, &rules);
        let visible_keys: HashSet<u32> = visible.kept.iter().copied().collect();
        for (i, entry) in items.iter().filter(|(item, _)| !item.adult).enumerate() {
            if visible_keys.contains(&(i as u32)) {
                continue;
            }
            let (item, at) = entry;
            out.push(BehaviorSignal {
                key: item.key.clone(),
                kind: SignalKind::BlockedWord,
                at: *at,
                strength: 1.0,
                platform: Some(item.platform.clone()),
            });
        }
    }

    out.sort_by(|a, b| {
        a.at.cmp(&b.at)
            .then_with(|| cmp_utf16(&a.key, &b.key))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    out
}

fn mix_text(item: &EmbedItem) -> MixText {
    MixText {
        adult: item.adult,
        title: item.title.clone(),
        summary: item.summary.clone(),
        community: item.community.clone(),
        repost_title: item.repost_title.clone(),
        repost_summary: item.repost_summary.clone(),
        repost_community: None,
    }
}

/// 每种信号各几条（设置页那一行状态用；顺序固定 = 枚举顺序，没出现的算 0）。
pub fn signal_counts(signals: &[BehaviorSignal]) -> Vec<(SignalKind, i32)> {
    let mut counts = [0i32; 8];
    for s in signals {
        counts[s.kind as usize] += 1;
    }
    SignalKind::ALL
        .into_iter()
        .map(|k| (k, counts[k as usize]))
        .collect()
}

/// 一条只带种类的信号。
///
/// 为什么不让宿主自己数：设置页那一行"共 N 条（正 X · 负 Y）"和"每种几条"的**顺序 / 零项 / 正负口径**
/// 都跟着 [`SignalKind`] 走（Rust 是唯一真相），宿主拿一份 `kind` 列表回来自己数就又开始抄规则了。
/// 这里只收"种类"这一列，别的字段不需要（每条信号有几个 String，省下来的都是过边界的钱）。
pub fn count_kinds(kinds: &[SignalKind]) -> Vec<(SignalKind, i32)> {
    let mut counts = [0i32; 8];
    for k in kinds {
        counts[*k as usize] += 1;
    }
    SignalKind::ALL
        .into_iter()
        .map(|k| (k, counts[k as usize]))
        .collect()
}

/// 从"种类名"还原 [`SignalKind`]（`FAVORITE` / `WATCH_DONE` / …，和 Kotlin 枚举名同一个口径）。
pub fn kind_from_name(name: &str) -> Option<SignalKind> {
    SignalKind::from_name(name)
}

/// 只保留"标题 / 摘要 / 转发原文至少有一个不是空白的"的卡片（算信号时的 candidates 过滤用；
/// 现在不额外过滤，留着给将来"没文字的卡片不算负向信号"那条规则）。
pub fn has_any_text(item: &EmbedItem) -> bool {
    !(is_blank_opt(&item.title)
        && is_blank_opt(&item.summary)
        && is_blank_opt(&item.repost_title)
        && is_blank_opt(&item.repost_summary))
}

fn is_blank_opt(value: &Option<String>) -> bool {
    match value {
        None => true,
        Some(s) => is_blank(s),
    }
}
