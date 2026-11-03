//! **首页大脑**的 UniFFI 边界（`feedhub-core` 的 `brain` 模块）。
//!
//! 全是纯函数，**一次调用进一整批、出一整批**，返回的是**输入里的下标**（`Vec<u32>`）而不是整条记录：
//! Kotlin 侧拿下标去自己的列表里取，既不复制 `FeedItem`，也不会因为 UniFFI 的 record 映射丢字段。
//!
//! 为什么还要分成 [`FfiMixItem`]（排序用：key / 平台 / 发布时间）和 [`FfiMixText`]（过滤用：18+ / 标题 /
//! 摘要 / 社区 / 转发）两种入参：排序一次搬 100 条的标题 / 摘要完全是浪费（长文本过 JNA 要复制字符串），
//! 而过滤根本不看 key / 平台 / 时间。分开之后两边每次只搬自己用得到的字段。
//!
//! 时间 / 种子 / 相似度都由 Kotlin 注入，Rust 不读时钟、不取随机数（和这一层其它模块的规矩一致）。

use feedhub_core::brain::{
    self, filter::Blocked, MixItem, MixText, OrderKind, Reason, Rules,
};

// ===========================================================================
// 过边界的 record / enum
// ===========================================================================

/// 排序 / 去重 / 搜索轮转用的一条。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixItem {
    /// 跨平台唯一键（`FeedItem.key`）。
    pub key: String,
    /// 平台 id（`bilibili` / `zhihu` / …）。
    pub platform: String,
    /// 发布时间（Unix 秒）；`None` = 没有发布时间（按时间排时排最后）。
    pub published_at: Option<i64>,
}

/// 过滤用的一条（18+ / 屏蔽词 / 外出模式）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixText {
    /// 平台标的 18+（`FeedItem.adult`）。
    pub adult: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// 社区（贴吧吧名、Reddit `r/…`、知乎问题标题、酷安话题、抖音话题……）。
    pub community: Option<String>,
    /// 转发那条自己的标题 / 摘要 / 社区。
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    pub repost_community: Option<String>,
}

/// 这次过滤用的规则（和设置里的四项一一对应，每批现读）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixRules {
    pub block_words: Vec<String>,
    pub outing_mode: bool,
    pub outing_keywords: Vec<String>,
    pub outing_communities: Vec<String>,
}

/// 命中的是哪一类规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMixReason {
    BlockWord,
    OutingKeyword,
    OutingCommunity,
}

/// 命中的那条规则（原文，只给日志 / 调试用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixHit {
    pub reason: FfiMixReason,
    /// 用户写的那条原文（`None` 只在"平台标的 18+"时出现，见 `FfiMixBlocked`）。
    pub rule: Option<String>,
}

/// 一条为什么不能显示：18+ 或者命中名单。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiMixBlocked {
    Adult,
    Rule { hit: FfiMixHit },
}

/// 一批过完的结果：保留下来的下标（输入顺序）+ 藏掉几条。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixVisible {
    pub kept: Vec<u32>,
    pub hidden: u32,
}

/// 每平台封顶的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixCapped {
    /// 留下的下标（输入顺序）。
    pub kept: Vec<u32>,
    /// 和 `kept` 一一对应：这条在它自己平台里的序号（0 起）。
    pub ranks: Vec<u32>,
    /// 被砍掉的下标（输入顺序）。
    pub dropped: Vec<u32>,
}

/// 封顶 + 平台序号 + 排序，一次算完的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMixPrepared {
    /// 上屏顺序（输入里的下标）。
    pub order: Vec<u32>,
    /// 和 `order` 一一对应：这条在它自己平台里的序号。
    pub ranks: Vec<u32>,
    /// 被每个平台的上限砍掉的下标。
    pub dropped: Vec<u32>,
}

/// 批内顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMixOrder {
    /// 关注页：永远按发布时间倒序。
    Following,
    /// 首页设置成「按时间」。
    Time,
    /// 首页默认：各平台穿插。
    Interleave,
}

// ===========================================================================
// 转换
// ===========================================================================

fn to_item(item: FfiMixItem) -> MixItem {
    MixItem {
        key: item.key,
        platform: item.platform,
        published_at: item.published_at,
    }
}

fn to_text(text: FfiMixText) -> MixText {
    MixText {
        adult: text.adult,
        title: text.title,
        summary: text.summary,
        community: text.community,
        repost_title: text.repost_title,
        repost_summary: text.repost_summary,
        repost_community: text.repost_community,
    }
}

fn to_rules(rules: FfiMixRules) -> Rules {
    Rules {
        block_words: rules.block_words,
        outing_mode: rules.outing_mode,
        outing_keywords: rules.outing_keywords,
        outing_communities: rules.outing_communities,
    }
}

fn to_kind(kind: FfiMixOrder) -> OrderKind {
    match kind {
        FfiMixOrder::Following => OrderKind::Following,
        FfiMixOrder::Time => OrderKind::Time,
        FfiMixOrder::Interleave => OrderKind::Interleave,
    }
}

fn to_reason(reason: Reason) -> FfiMixReason {
    match reason {
        Reason::BlockWord => FfiMixReason::BlockWord,
        Reason::OutingKeyword => FfiMixReason::OutingKeyword,
        Reason::OutingCommunity => FfiMixReason::OutingCommunity,
    }
}

// ===========================================================================
// 导出
// ===========================================================================

/// 批内顺序（穿插 / 按时间）：返回输入里的下标。
#[uniffi::export]
pub fn brain_order(items: Vec<FfiMixItem>, kind: FfiMixOrder, seed: i64) -> Vec<u32> {
    let items: Vec<MixItem> = items.into_iter().map(to_item).collect();
    brain::order(&items, to_kind(kind), seed)
}

/// 每平台封顶 + 数平台序号（不排序）。
#[uniffi::export]
pub fn brain_cap(items: Vec<FfiMixItem>, max_per_platform: i32) -> FfiMixCapped {
    let items: Vec<MixItem> = items.into_iter().map(to_item).collect();
    let capped = brain::cap(&items, max_per_platform);
    FfiMixCapped {
        kept: capped.kept,
        ranks: capped.ranks,
        dropped: capped.dropped,
    }
}

/// 封顶 + 平台序号 + 排序，一次算完（`BatchMerge.prepare` 里"去重之后"的那一段）。
#[uniffi::export]
pub fn brain_prepare(
    items: Vec<FfiMixItem>,
    max_per_platform: i32,
    kind: FfiMixOrder,
    seed: i64,
) -> FfiMixPrepared {
    let items: Vec<MixItem> = items.into_iter().map(to_item).collect();
    let prepared = brain::prepare(&items, max_per_platform, to_kind(kind), seed);
    FfiMixPrepared {
        order: prepared.order,
        ranks: prepared.ranks,
        dropped: prepared.dropped,
    }
}

/// 打分钩子的合成：位置分数 + 相似度（权重 0 = 原样返回位置分数）。
#[uniffi::export]
pub fn brain_blend(position: f32, similarity: Option<f32>, weight: f32) -> f32 {
    brain::blend(position, similarity, weight)
}

/// 在排好的顺序上叠一层相似度（稳定排序）：返回输入里的下标。
#[uniffi::export]
pub fn brain_blend_order(
    items: Vec<FfiMixItem>,
    similarities: Vec<Option<f32>>,
    weight: f32,
) -> Vec<u32> {
    let items: Vec<MixItem> = items.into_iter().map(to_item).collect();
    brain::blend_order(&items, &similarities, weight)
}

/// 一批里**按名单**能显示的那些，返回下标 + 藏掉几条（`ContentFilter.visible`：不看 18+）。
#[uniffi::export]
pub fn brain_filter_visible(items: Vec<FfiMixText>, rules: FfiMixRules) -> FfiMixVisible {
    let items: Vec<MixText> = items.into_iter().map(to_text).collect();
    let visible = brain::visible(&items, &to_rules(rules));
    FfiMixVisible {
        kept: visible.kept,
        hidden: visible.hidden,
    }
}

/// 一批里**能上屏**的那些：18+（一直生效）或者命中名单都藏掉（信息流 / 搜索的 `kept`）。
#[uniffi::export]
pub fn brain_filter_shown(items: Vec<FfiMixText>, rules: FfiMixRules) -> FfiMixVisible {
    let items: Vec<MixText> = items.into_iter().map(to_text).collect();
    let shown = brain::shown(&items, &to_rules(rules));
    FfiMixVisible {
        kept: shown.kept,
        hidden: shown.hidden,
    }
}

/// 单条按名单判断（不看 18+，`ContentFilter.hidden`）：命中的话是哪条规则。
#[uniffi::export]
pub fn brain_filter_hidden(text: FfiMixText, rules: FfiMixRules) -> Option<FfiMixHit> {
    brain::hidden(&to_text(text), &to_rules(rules)).map(|hit| FfiMixHit {
        reason: to_reason(hit.reason),
        rule: Some(hit.rule),
    })
}

/// 单条判断（详情页直接打开一条、以后别的单条入口用）：18+ 或者命中名单。
#[uniffi::export]
pub fn brain_filter_blocked(text: FfiMixText, rules: FfiMixRules) -> Option<FfiMixBlocked> {
    let text = to_text(text);
    let rules = to_rules(rules);
    match brain::blocked(&text, &rules) {
        None => None,
        Some(Blocked::Adult) => Some(FfiMixBlocked::Adult),
        Some(Blocked::Rule(hit)) => Some(FfiMixBlocked::Rule {
            hit: FfiMixHit {
                reason: to_reason(hit.reason),
                rule: Some(hit.rule),
            },
        }),
    }
}

/// 名单 / 文本的归一化口径（`ContentFilter.normalize`）——给 Kotlin 侧的设置代码和对拍用。
#[uniffi::export]
pub fn brain_normalize(text: String) -> String {
    brain::normalize(&text)
}

/// 跨平台搜索：每家在**一页里**取几条进桶。
#[uniffi::export]
pub fn brain_search_quota(platforms: i32, page_size: i32, target: i32) -> i32 {
    brain::quota(platforms, page_size, target)
}

/// 跨平台搜索：按平台顺序轮转（同一批重复的 key 只留第一条），返回输入里的下标。
#[uniffi::export]
pub fn brain_search_round_robin(platforms: Vec<String>, items: Vec<FfiMixItem>) -> Vec<u32> {
    let items: Vec<MixItem> = items.into_iter().map(to_item).collect();
    brain::round_robin(&platforms, &items)
}

// ===========================================================================
// 名单的归拢
// ===========================================================================

/// 名单收干净（去空白 / 丢空的和过长的 / 按归一化去重 / 最多 200 条），**保留用户写的原文**。
/// 批量：一次调用收一整份名单（设置页 / 读存档），不逐条过 FFI。
#[uniffi::export]
pub fn brain_normalize_rules(words: Vec<String>) -> Vec<String> {
    brain::filter::normalize_rules(&words)
}

/// 一批文本的归一化（`ContentFilter.normalize` 的批量版）：设置页的"加一条"一次要比
/// 输入框 + 已有条目，`remember(text, words)` 里一次调用算完，**不在重组时逐条过 FFI**。
#[uniffi::export]
pub fn brain_normalize_many(texts: Vec<String>) -> Vec<String> {
    texts.iter().map(|t| brain::filter::normalize(t)).collect()
}
