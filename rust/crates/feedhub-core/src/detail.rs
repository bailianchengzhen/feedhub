//! **详情 / 评论的通用模型**（对应 `core/src/main/kotlin/feedhub/core/NativeDetail.kt` 那一套）。
//!
//! 为什么单独一个模块、而不是塞进 `model.rs`：`model.rs` 只装"信息流"那半边，详情 / 评论是当前新开的
//! 一层，下一轮 10 家都要往里填。字段顺序、可空性、默认值都与 Kotlin 一一对应：
//!
//! | Rust | Kotlin |
//! |---|---|
//! | [`FeedDetail`] | `FeedDetail`（`item` / `body` / `video` / `follow` / `related`） |
//! | [`RichText`] / [`Block`] / [`Span`] | `RichText` / `Block` / `Span`（前端只认这几种块） |
//! | [`Comment`] | `Comment`（一级评论和楼中楼是同一个类型，楼中楼预览在 `preview_replies`） |
//! | [`CommentPage`] | `Page<Comment>`（`items` + 不透明游标 `next`） |
//! | [`VideoInfo`] / [`VideoPart`] | 同名的 Kotlin 类型（播放 / 弹幕用 `VideoPart.id`） |
//! | [`FollowTarget`] / [`FollowState`] / [`FollowKind`] | 同名的 Kotlin 类型（详情页的"关注"按钮） |
//!
//! 约定和 `model.rs` 一样：**拿不到就是 `None`，不要编默认值**；文本一律是纯文本（表情保留文字代码）。
//!
//! 评论树是递归的（楼中楼的楼中楼），Rust 侧用 `Vec<Comment>` 直接嵌（`Box` 都不需要）——
//! **过 FFI 时不能这样**：UniFFI 0.29 不支持递归 record，所以 `feedhub-ffi` 里换成了
//! "节点数组 + 下标"（`FfiCommentPage` / `FfiComment.preview_replies`）。

use crate::model::{DetailTarget, FeedItem, PlatformId, TimePrecision, UserRef};

/// 评论排序（Kotlin `CommentSort`）。**各平台自己映射**到自己的参数，不支持的档按 `Default` 处理。
///
/// 别各写一份）；
/// `bili::source::CommentSort` 仍然是这个名字（`pub use`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSort {
    Default,
    Hot,
    Newest,
}

/// 平台无关的富文本（Kotlin `RichText`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RichText {
    pub blocks: Vec<Block>,
}

impl RichText {
    pub const EMPTY: RichText = RichText { blocks: Vec::new() };

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// 正文里的文字，但**不把图片 / 视频降级成占位符**（Kotlin `RichText.textOnly()`）。
    /// 信息流摘要用它：卡片自己会把封面画出来，摘要里再出现一次 `[图片]` 就是重复。
    pub fn text_only(&self) -> String {
        text_only(&self.blocks)
    }

    /// 纯文本（`RichText.plainText()`）：块之间用 `\n` 拼，图片 / 视频 / 卡片用占位符。
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(Block::plain_text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `RichText.textOnly()`：过滤图片 / 视频块，其余按 `\n` 连起来（卡片保留 `[标题]`）。
pub fn text_only(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter(|b| !matches!(b, Block::Image { .. } | Block::Video { .. }))
        .map(Block::plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 前端只认这几种块（Kotlin `Block`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// 正文段落（列表项也是它，带 `• ` / `1. ` 前缀）。
    Paragraph { spans: Vec<Span> },
    /// 小标题（h1~h6 都降成它）。
    Heading { spans: Vec<Span> },
    /// 引用块。
    Quote { spans: Vec<Span> },
    Code {
        text: String,
        language: Option<String>,
    },
    /// 图片；`original` 是原图（和 `url` 不同时才给）。
    Image {
        url: String,
        width: Option<i32>,
        height: Option<i32>,
        original: Option<String>,
        caption: Option<String>,
    },
    /// 正文里嵌的卡片（视频 / 链接 / 引用的另一条内容 / 投票）。
    Card {
        title: String,
        subtitle: Option<String>,
        cover: Option<String>,
        url: Option<String>,
        target: Option<DetailTarget>,
    },
    /// 正文里夹着的视频：**不放播放地址**（会过期），点播放时前端用 `id` / `part_id` 现取。
    Video {
        id: String,
        part_id: String,
        cover: Option<String>,
        duration_sec: Option<i32>,
        width: Option<i32>,
        height: Option<i32>,
        title: Option<String>,
        url: Option<String>,
    },
    Divider,
}

impl Block {
    /// 和 Kotlin 的 `RichText.textOf` 逐条对应（调试 / cli / 摘要用）。
    pub fn plain_text(&self) -> String {
        fn join(spans: &[Span]) -> String {
            spans.iter().map(|s| s.text()).collect::<Vec<_>>().join("")
        }
        match self {
            Block::Paragraph { spans } | Block::Heading { spans } => join(spans),
            Block::Quote { spans } => format!("> {}", join(spans)),
            Block::Code { text, .. } => text.clone(),
            Block::Image { .. } => "[图片]".to_string(),
            Block::Card { title, .. } => format!("[{title}]"),
            Block::Video { .. } => "[视频]".to_string(),
            Block::Divider => "----".to_string(),
        }
    }
}

/// 行内片段（Kotlin `Span`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Span {
    Text {
        text: String,
        bold: bool,
        italic: bool,
    },
    /// 链接；站内能原生打开的给 `target`。
    Link {
        text: String,
        url: String,
        target: Option<DetailTarget>,
    },
    Mention {
        text: String,
        user: UserRef,
    },
    /// 平台表情：`text` 是文字代码（`[doge]`），`url` 是图片地址（可以是 `feedhub-emoji:` 那种应用自带图）。
    Emote {
        text: String,
        url: String,
    },
}

impl Span {
    pub fn text(&self) -> &str {
        match self {
            Span::Text { text, .. }
            | Span::Link { text, .. }
            | Span::Mention { text, .. }
            | Span::Emote { text, .. } => text,
        }
    }
}

/// 视频分 P（Kotlin `VideoPart`）。播放和弹幕用 [`VideoPart::id`]。
#[derive(Debug, Clone, PartialEq)]
pub struct VideoPart {
    pub id: String,
    pub title: Option<String>,
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 视频信息（Kotlin `VideoInfo`）：分 P 列表 + 弹幕每段多长（毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub parts: Vec<VideoPart>,
    pub danmaku_segment_ms: Option<i64>,
}

/// 可关注的对象（Kotlin `FollowKind` / `FollowTarget`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowKind {
    User,
    Forum,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FollowTarget {
    pub platform: PlatformId,
    pub kind: FollowKind,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    /// 签名 / 吧简介之类的一行说明。
    pub description: Option<String>,
    pub url: Option<String>,
}

/// 详情页"关注"按钮的状态（Kotlin `FollowState`）。
#[derive(Debug, Clone, PartialEq)]
pub struct FollowState {
    pub target: FollowTarget,
    /// 当前是否已关注；没登录或接口没给就是 `None`。
    pub following: Option<bool>,
}

/// 详情（Kotlin `FeedDetail`）。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedDetail {
    /// 详情接口拿到的完整信息（标题 / 作者 / 计数通常比信息流里的全）。
    pub item: FeedItem,
    /// 正文；视频是简介。
    pub body: RichText,
    /// 视频才有：分 P 列表。
    pub video: Option<VideoInfo>,
    /// 页面上"关注"按钮对应的对象和当前状态。
    pub follow: Option<FollowState>,
    /// 相关推荐（B站视频下面那一列）。
    pub related: Vec<FeedItem>,
}

/// 一条评论（Kotlin `Comment`）。一级评论和楼中楼是同一个类型；
/// 楼中楼的预览在 [`Comment::preview_replies`]（一般是前两条），总数在 [`Comment::reply_count`]。
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub platform: PlatformId,
    pub id: String,
    pub author: Option<UserRef>,
    pub content: RichText,
    pub published_at: Option<i64>,
    /// `published_at` 精确到哪一档（YouTube 的"6 years ago"之类只到年）。
    pub published_at_precision: TimePrecision,
    pub likes: Option<i64>,
    /// 楼中楼总数。
    pub reply_count: Option<i64>,
    /// 回复的是谁（楼中楼里"回复 @xxx"）。
    pub reply_to: Option<UserRef>,
    /// IP 属地（如 `上海`）。
    pub ip_location: Option<String>,
    /// 楼层号（贴吧"第 N 楼"）。
    pub floor: Option<i32>,
    /// 置顶评论。
    pub pinned: bool,
    pub preview_replies: Vec<Comment>,
    /// 平台自己给的语言（BCP 47），没有就 `None`。
    pub language: Option<String>,
}

impl Comment {
    /// 一条最小可用的评论（各平台解析器照着填剩下的字段；楼中楼预览用 [Comment::preview_replies] 递归塞）。
    pub fn new(platform: impl Into<PlatformId>, id: impl Into<String>, content: RichText) -> Self {
        Comment {
            platform: platform.into(),
            id: id.into(),
            author: None,
            content,
            published_at: None,
            published_at_precision: TimePrecision::Second,
            likes: None,
            reply_count: None,
            reply_to: None,
            ip_location: None,
            floor: None,
            pinned: false,
            preview_replies: Vec::new(),
            language: None,
        }
    }
}

/// 一页评论（Kotlin `Page<Comment>`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommentPage {
    pub items: Vec<Comment>,
    /// 不透明游标，调用方原样传回来；`None` 表示没有下一页。
    pub next: Option<String>,
}
