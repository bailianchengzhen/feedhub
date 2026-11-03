//! **首页大脑**：信息流"怎么排、留哪些"的纯函数。
//!
//! 这一块原来在 Kotlin：`app/.../feed/FeedMix.kt`（穿插 / 按时间 / 打分钩子 / 每平台封顶）、
//! `app/.../feed/FeedState.kt` 的 `BatchMerge.prepare`（去重之后的封顶 + 平台序号 + 排序）、
//! `core/.../ContentFilter.kt` / `AdultFilter.kt`（18+ / 屏蔽词 / 外出模式）、
//! `core/.../search/SearchMerge.kt`（跨平台搜索的配额 + 轮转）。
//! Rust 是**唯一实现**，Kotlin 版只剩测试里的参照
//! （`core/src/testFixtures/.../legacy/`）；**没有开关**。
//!
//! 边界和别的平台层一样：**一次调用进一整批、出一整批**，不收单条（`FeedState` 里逐条调的那些
//! 入口在 Kotlin 门面里合成一批再过来）。这些函数**不读时钟、不取随机数、不碰 IO**：种子 / 相似度
//! 都是调用方给的，所以同一批两次调用结果一样。
//!
//! 产品规则：
//! - 首页每批**各平台穿插**，平台内保持平台自己的推荐顺序；关注页**永远按时间**倒序；
//! - **每平台一批最多 N 条**（只截断、不重排，默认 15，0 = 不限）；
//! - 个性化（本地向量）**只排序、不删内容**，接在打分钩子上，权重现在是 0；
//! - 平台标成 18+ 的**整条隐藏**；屏蔽词 / 外出模式命中同样整条隐藏、不打码（第一层）。
//!
//! 文件：
//! - [`order`]：每平台封顶、批内顺序（穿插 / 按时间）、打分钩子的合成与稳定排序；
//! - [`filter`]：18+ / 屏蔽词 / 外出模式，以及名单的归一化口径；
//! - [`search`]：跨平台搜索的配额与轮转；
//! - [`rng`]：Kotlin `Random(seed)` 的逐位复刻（穿插同分时打乱平台先后的种子）。

pub mod filter;
pub mod order;
pub mod rng;
pub mod search;

pub use filter::{
    blocked, hidden, normalize, shown, visible, Blocked, Hit, MixText, Reason, Rules, Visible,
};
pub use order::{blend, blend_order, cap, order, order_refs, prepare, Capped, OrderKind, Prepared};
pub use search::{quota, round_robin};

/// 排序 / 去重 / 搜索轮转要的一条：只要 `key`、平台、发布时间。
///
/// 别的字段（标题 / 摘要 / 18+ / 社区）在 [`MixText`] 里，过滤那条路才用到——分开是为了不让排序
/// 把几十个字的长文本也搬过一次 FFI（那正是这一层想省的开销）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixItem {
    /// 跨平台唯一键，`platform:id`（和 `FeedItem.key` 一样）。
    pub key: String,
    /// 平台 id（`bilibili` / `zhihu` / …），顺序就是批内第一次出现的顺序。
    pub platform: String,
    /// 发布时间（Unix 秒）；`None` = 没有发布时间（按时间排时**排在最后**）。
    pub published_at: Option<i64>,
}
