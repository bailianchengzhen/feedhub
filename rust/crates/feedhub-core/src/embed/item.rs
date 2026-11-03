//! 向量这一层用的**卡片输入**（比 `model::FeedItem` 小得多）。
//!
//! 为什么不用 `FeedItem` 本身：算向量 / 排封面只用到十来个小字段，而 `FeedItem` 还带着作者头像、统计、
//! 转发整棵子树、表情表、详情目标——过 UniFFI 时那些全都要复制一遍（一次 offer 一批 100 条卡片）。
//! 这个 record 只装"拼文字 + 判要不要算 + 找封面"用得到的字段，Kotlin 侧一行 `toEmbedItem()` 转过来。
//!
//! 和 `model::FeedItem` 之间有一份 `From`：Rust 自己的测试、以及将来"资料库直接喂进来"的路径用它，
//! 免得再造第三份模型。

use crate::model::{FeedItem, ItemKind, Media};

/// 一条卡片在这个模块里的样子。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedItem {
    /// 跨平台唯一键（`FeedItem.key` = `platform:id`）。
    pub key: String,
    /// 平台内唯一的 id（导出用；`key` 里虽然也有，但 id 本身可能带冒号，不拆）。
    pub id: String,
    /// 平台 id（`bili` / `zhihu` / …）。
    pub platform: String,
    /// 内容类型（Kotlin `ItemKind.name`：`VIDEO` / `ARTICLE` / …；导出用）。
    pub kind: String,
    /// 发布时间（Unix 秒；导出用）。
    pub published_at: Option<i64>,
    /// 语言（导出用）。
    pub language: Option<String>,
    /// 作者显示名（导出用；屏蔽作者的判定用 [`EmbedItem::author_id`]）。
    pub author_name: Option<String>,
    /// 平台标了 18+（整条隐藏；向量 / 信号 / 封面都不算它）。
    pub adult: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// 所属圈子（贴吧吧名、知乎问题……）。
    pub community: Option<String>,
    /// 平台给的推荐理由 / 动作说明。
    pub reason: Option<String>,
    /// 作者 id（屏蔽作者的信号要用 `platform:id` 去对）。名字不算，别过边界。
    pub author_id: Option<String>,
    /// 转发那条自己的标题 / 摘要（屏蔽词要扫这两段，「命中屏蔽词」这个信号用）。
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    /// 视频封面的原地址（`Media.Video.cover`）。
    pub video_cover: Option<String>,
    /// 正文里第一张图的地址（`Media.Image.url`，**保持 media 里的顺序**）。
    pub first_image: Option<String>,
}

impl EmbedItem {
    /// 从统一模型转过来。`key` 在 Rust 里算好（和 Kotlin 的 `FeedItem.key` 同一个口径）。
    pub fn from_feed(item: &FeedItem) -> Self {
        let mut video_cover = None;
        let mut first_image = None;
        for media in &item.media {
            match media {
                Media::Video { cover, .. } => {
                    if video_cover.is_none() {
                        video_cover = cover.clone();
                    }
                }
                Media::Image { url, .. } => {
                    if first_image.is_none() {
                        first_image = Some(url.clone());
                    }
                }
            }
        }
        EmbedItem {
            key: item.key(),
            id: item.id.clone(),
            platform: item.platform.clone(),
            kind: kind_name(item.kind).to_string(),
            published_at: item.published_at,
            language: item.language.clone(),
            author_name: item.author.as_ref().map(|a| a.name.clone()),
            adult: item.adult,
            title: item.title.clone(),
            summary: item.summary.clone(),
            community: item.community.clone(),
            reason: item.reason.clone(),
            author_id: item.author.as_ref().map(|a| a.id.clone()),
            repost_title: item.repost.as_ref().and_then(|r| r.title.clone()),
            repost_summary: item.repost.as_ref().and_then(|r| r.summary.clone()),
            video_cover,
            first_image,
        }
    }

    /// 作者的屏蔽键（Kotlin `UserRef.blockKey()` = `platform:id`）。
    pub fn author_block_key(&self) -> Option<String> {
        self.author_id.as_ref().map(|id| format!("{}:{}", self.platform, id))
    }

    /// 这条卡片有没有可算的文字（18+ 不算；四个文字字段全空 / 全空白也不算）。
    pub fn has_text(&self) -> bool {
        if self.adult {
            return false;
        }
        !(blank(&self.title) && blank(&self.summary) && blank(&self.community) && blank(&self.reason))
    }
}

/// `ItemKind` → Kotlin 枚举名（导出里的 `kind` 字段；`sanitizedItem` 用的就是 `item.kind.name`）。
pub fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Video => "VIDEO",
        ItemKind::Article => "ARTICLE",
        ItemKind::Answer => "ANSWER",
        ItemKind::Question => "QUESTION",
        ItemKind::Post => "POST",
        ItemKind::Moment => "MOMENT",
        ItemKind::Live => "LIVE",
        ItemKind::Other => "OTHER",
    }
}

/// Kotlin 的 `String?.isBlank()`：`None` / 空串 / 全是空白（口径见 [`crate::hosted::is_kotlin_blank`]）。
pub fn blank(value: &Option<String>) -> bool {
    match value {
        None => true,
        Some(s) => crate::hosted::is_kotlin_blank(s),
    }
}

/// 从统一模型**批量**转（一次 offer 一批）。
pub fn from_feeds(items: &[FeedItem]) -> Vec<EmbedItem> {
    items.iter().map(EmbedItem::from_feed).collect()
}
