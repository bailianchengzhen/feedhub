//! feedhub-core 的 UniFFI 边界。
//!
//! 设计：
//! - **两种东西**：① 纯函数（解析 / 签名 / AV-BV），② **平台对象**（`FfiBiliSource`，见 `bili_source.rs`）——
//!   它管"请求怎么发"，真正的收发通过 `host.rs` 里那三个 `with_foreign` trait 回调宿主（OkHttp / store / Cookie 罐）。
//!   `.so` 里**没有网络栈**（没有 reqwest / rustls / tokio / hyper），所以它不可能自己发请求。
//! - **一次调用换一整页**：JVM ↔ Rust 每次调用都要过一次 JNA，按字段逐个取值会把开销放大几十倍。
//! - **错误带类型过去**：`FfiError` 是 `uniffi::Error` 的 enum，Kotlin 拿到 `sealed class FfiException`，
//!   适配层再映射回现有的 `NotLoggedInException` / `RiskControlException` / `ApiException` / `ParseException`。
//!   不要在这里拼一句人话就完事——"平台返回了错误码"和"JSON 坏了"必须分得开。
//!
//! 转发（`repost`）用**下标**而不是递归 record：UniFFI 0.29 不支持 `Box<T>` 递归 record
//! （试过，`Box<Node>: Lift` 没实现）。所以一页是"节点数组 + 根下标"，Kotlin 侧按索引还原。

mod bili_source;
mod brain;
// **Cookie 罐 / 限速 / 冷却 / Chrome Cookie 导入**：
// `FfiCookieEntry` / `FfiCookieImport*` / `FfiRate*`（`RateLimitQueue` / `CrossProcess*` / `Cooldown*`）。
mod cookies;
// 信息流 / 搜索 / 账号 / 关注管理：FfiFeed* / FfiSearch* / FfiAcct* 三类对象 + 纯函数。
mod feed;
mod host;
// 本地向量的分词器：FfiTokTokenizer + FfiTokBatch。
mod tokenizer;
// 本地资料库：FfiLibrary + FfiLib* record + 合并回调。
mod library;
// 本地向量：调度 / 画像 / 行为信号 / 封面向量 / 行为导出。
// 前缀：FfiEmbed* / FfiTaste* / FfiCover*；两个状态机对象 + 一个库操作回调 FfiEmbedStore。
mod embed;
// 播放器的决策层：选流 / 解码账本 / 弹幕 / 清单改写 / 中转 / 播放状态机。
// `FfiDanmaku`（一条弹幕）复用 `play.rs` 那份。
mod player;
// 播放 / 弹幕的通用 record（`FfiPlay*` / `FfiDanmaku*` + core → FFI 的唯一一份转换）。
// 后面的平台**直接用它**，别再各造一套。
// 展示层的纯函数：`FfiPresent*` + `FfiPresentBatch`（一屏一批）。
// 逻辑在 `feedhub-core/src/present/`；时区 / 时钟由宿主传进来。
mod present;
mod play;
// 卡片 / 九宫格 / 详情占位的**缩略图地址改写**：前缀 `FfiThumb*`。
// 界面和封面向量都走这一份（规则在 `feedhub-core/src/thumb.rs`）。
mod thumb;
mod tieba_source;
mod xhs_source;

use feedhub_core::bili;
use feedhub_core::json;

use crate::play::{to_ffi_danmaku, to_ffi_play, FfiDanmaku, FfiPlayInfo};
use feedhub_core::model::{BiliError, DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision};

// 各平台各一个文件（并行开发时互不改同一个文件）：这里只加 `mod`，不写实现。
mod library_sync;
// 登录互传：打包 / 解包 / 一次性小服务 / 局域网取与推。
mod login_sync;
mod tieba;
mod heybox;
mod heybox_source;
mod coolapk;
mod coolapk_source;
mod douyin;
mod douyin_source;
// 按平台指定代理的判定：FfiProxy* + proxy_resolve 等纯函数。
mod proxy;
// 内置代理核心：FfiProxyCore 平台对象 + 取订阅的宿主端口 +
// 二进制 / 配置 / 解析的纯函数。
// 这边是 `FfiProxyCore` / `FfiProxyState` / …）。
mod proxy_core;
// 联网层：FfiNetClient / FfiNetRequest / FfiNetResponse。
// **还没接进产品路径**（HostHttp / OkHttp 一行不动）；也让 `feedhub-net`
// 被编进 `.so`（量体积、验交叉编译）。
mod net;
// **`HttpHost` 的 Rust 实现**：平台对象不过 FFI，直接用 `feedhub-net` 发请求。
// 顺序（`onRequest` 计数 → 代理守卫 → 默认头 → 限速 → txid → 连接）和现在的 OkHttp 拦截器链逐条对齐。
mod net_host;
mod relay_server;
// 设置的读 / 迁移 / 归一化 / 默认值：FfiPluginSpec + settings_normalize。
mod settings;
// 桌面对外接口的协议常量 + 取图几何：FfiApiErrorCode + api_segments 等纯函数。
mod appapi;
mod reddit_source;
mod tiktok_source;
mod youtube_source;
mod x_source;

uniffi::setup_scaffolding!();


// ===========================================================================
// 模型（和 core/Model.kt 一一对应）
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiItemKind {
    Video,
    Article,
    Answer,
    Question,
    Post,
    Moment,
    Live,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTimePrecision {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUserRef {
    pub platform: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiMedia {
    Image {
        url: String,
        width: Option<i32>,
        height: Option<i32>,
    },
    Video {
        cover: Option<String>,
        duration_sec: Option<i32>,
        width: Option<i32>,
        height: Option<i32>,
    },
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiStats {
    pub views: Option<i64>,
    pub likes: Option<i64>,
    pub comments: Option<i64>,
    pub shares: Option<i64>,
    pub danmaku: Option<i64>,
    pub favorites: Option<i64>,
    pub coins: Option<i64>,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiDetailTarget {
    Web { url: String },
    Native { id: String, platform: Option<String> },
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiEmote {
    pub code: String,
    pub url: String,
}

/// 一条内容。[`repost`] 是**节点数组里的下标**（见模块说明），不是嵌套 record。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiItem {
    pub platform: String,
    pub id: String,
    /// 跨平台唯一键，`platform:id`。在 Rust 里算好。
    pub key: String,
    pub kind: FfiItemKind,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub language: Option<String>,
    pub author: Option<FfiUserRef>,
    pub published_at: Option<i64>,
    pub published_at_precision: FfiTimePrecision,
    pub media: Vec<FfiMedia>,
    pub stats: FfiStats,
    pub reason: Option<String>,
    pub community: Option<String>,
    pub community_icon: Option<String>,
    /// 转发的原内容：`nodes` 里的下标。
    pub repost: Option<u32>,
    pub emotes: Vec<FfiEmote>,
    pub adult: bool,
    pub url: Option<String>,
    pub detail: FfiDetailTarget,
}

/// 一页：`nodes` 是全部条目（含转发的原内容），`roots` 是这一页真正要显示的条目下标。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPage {
    pub nodes: Vec<FfiItem>,
    pub roots: Vec<u32>,
    pub next: Option<String>,
}

/// 单条（用于测试 / 详情里的 `data.item`）：`root` 是 `nodes` 里的下标。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiItemNode {
    pub nodes: Vec<FfiItem>,
    pub root: Option<u32>,
}

// ===========================================================================
// 详情 / 评论的通用 record
// ===========================================================================
//
// 对应 `core/NativeDetail.kt`：`FfiDetail*` 是详情（正文 / 分 P / 关注 / 相关推荐），
// `FfiComment*` 是评论（一级 + 楼中楼预览）。字段顺序和 Kotlin 一一对应。
//
// **评论树和"相关推荐"都用"节点数组 + 下标"**：UniFFI 0.29 不支持递归 record
// （`Box<T>: Lift` 没实现），所以：
// - `FfiCommentPage.nodes` + `roots`：这一页的一级评论下标；楼中楼预览在 `FfiComment.preview_replies`（也是下标）；
// - `FfiDetail.nodes` + `item`（详情条目）+ `related`（相关推荐的下标，**顺序即显示顺序**）。
// 一个下标可以只出现一次（同一个对象共享），Kotlin 侧按树/数组还原。

/// 评论排序（和 `CommentSort` 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCommentSort {
    Default,
    Hot,
    Newest,
}

/// 行内片段（Kotlin `Span`）。名字带 `FfiDetail` 前缀，避免和别家的试点类型撞车。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiDetailSpan {
    Text {
        text: String,
        bold: bool,
        italic: bool,
    },
    Link {
        text: String,
        url: String,
        target: Option<FfiDetailTarget>,
    },
    Mention {
        text: String,
        user: FfiUserRef,
    },
    Emote {
        text: String,
        url: String,
    },
}

/// 正文里的一种块（Kotlin `Block`）。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiDetailBlock {
    Paragraph {
        spans: Vec<FfiDetailSpan>,
    },
    Heading {
        spans: Vec<FfiDetailSpan>,
    },
    Quote {
        spans: Vec<FfiDetailSpan>,
    },
    Code {
        text: String,
        language: Option<String>,
    },
    Image {
        url: String,
        width: Option<i32>,
        height: Option<i32>,
        original: Option<String>,
        caption: Option<String>,
    },
    Card {
        title: String,
        subtitle: Option<String>,
        cover: Option<String>,
        url: Option<String>,
        target: Option<FfiDetailTarget>,
    },
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

/// 平台无关的富文本（Kotlin `RichText`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailBody {
    pub blocks: Vec<FfiDetailBlock>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailVideoPart {
    pub id: String,
    pub title: Option<String>,
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailVideoInfo {
    pub parts: Vec<FfiDetailVideoPart>,
    /// 弹幕每段多长（毫秒）。
    pub danmaku_segment_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFollowKind {
    User,
    Forum,
}

/// 可关注的对象（Kotlin `FollowTarget`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFollowTarget {
    pub platform: String,
    pub kind: FfiFollowKind,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

/// 详情页"关注"按钮（Kotlin `FollowState`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailFollow {
    pub target: FfiFollowTarget,
    /// 当前是否已关注；没登录或接口没给就是 `None`。
    pub following: Option<bool>,
}

/// 详情（Kotlin `FeedDetail`），"节点数组 + 下标"的写法见本段开头的说明。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetail {
    pub nodes: Vec<FfiItem>,
    /// 详情条目在 `nodes` 里的下标（拿不到就是 `None`）。
    pub item: Option<u32>,
    pub body: FfiDetailBody,
    pub video: Option<FfiDetailVideoInfo>,
    pub follow: Option<FfiDetailFollow>,
    /// 相关推荐（`nodes` 里的下标，顺序即显示顺序）。
    pub related: Vec<u32>,
}

/// 一条评论（Kotlin `Comment`）。楼中楼预览是 `FfiCommentPage.nodes` 里的下标。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiComment {
    pub platform: String,
    pub id: String,
    pub author: Option<FfiUserRef>,
    pub content: FfiDetailBody,
    pub published_at: Option<i64>,
    pub published_at_precision: FfiTimePrecision,
    pub likes: Option<i64>,
    pub reply_count: Option<i64>,
    pub reply_to: Option<FfiUserRef>,
    pub ip_location: Option<String>,
    pub floor: Option<i32>,
    pub pinned: bool,
    /// 楼中楼预览：`nodes` 里的下标（顺序即显示顺序）。
    pub preview_replies: Vec<u32>,
    pub language: Option<String>,
}

/// 一页评论：`nodes` 是全部评论（含楼中楼预览），`roots` 是这一页要显示的一级评论下标。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCommentPage {
    pub nodes: Vec<FfiComment>,
    pub roots: Vec<u32>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiParam {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWbiKeys {
    pub img_key: String,
    pub sub_key: String,
}

// ===========================================================================
// 错误
// ===========================================================================

/// 对应 `SourceException` 的子类。**错误变体的字段不要叫 `message`**：UniFFI 生成的 Kotlin 是
/// `sealed class FfiException : Exception()`，变体里的 `val message` 会和 `Exception.message` 撞车，
/// Kotlin 2.3 直接编译不过。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("未登录或登录已失效")]
    NotLoggedIn {
        platform: String,
        detail: Option<String>,
    },

    /// 风控：验证码、请求被拦截、账号异常。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl {
        platform: String,
        code: Option<String>,
        detail: String,
    },

    #[error("签名被拒：{detail}")]
    Signature { platform: String, detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api {
        platform: String,
        code: String,
        detail: String,
    },

    #[error("解析失败：{detail}")]
    Parse { platform: String, detail: String },

    /// 网络错误（连不上 / 超时 / 连接被重置）。调用方映射回 `java.io.IOException`（现有约定：网络错误不包装）。
    #[error("网络错误：{detail}")]
    Network { platform: String, detail: String },

    /// fail-closed：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    /// 调用方映射成 `ProxyUnavailableException`（cli 的退出码 6）。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { platform: String, detail: String },

    /// 封装层兜住的内部错误（panic / 编码问题 / 宿主回调里的意外），别让它变成一次崩溃。
    #[error("内部错误：{detail}")]
    Internal { platform: String, detail: String },

    /// **要人机验证**（目前只有酷安评论 / 楼中楼会这样）：`err_request_captcha_v2`。
    ///
    /// 宿主（`RustCoolapkSource`）接到之后照 Kotlin `CoolapkSource.getJson` 的流程办：
    /// 用 [`Self::captcha_id`] / [`Self::field`] / [`Self::detail`] 组一个 `CaptchaChallenge`，
    /// 让**用户本人**完成一次验证（`ctx.captcha`），把答案（`validate` cookie 或查询参数）带上
    /// **重试同一个请求一次**；再要验证就报风控错。**不循环、不自动绕、不改版本号 / 身份参数。**
    #[error("需要人机验证：{detail}")]
    CaptchaRequired {
        platform: String,
        /// 服务端下发的验证码 id（酷安是网易易盾的 NEC captchaId；没下发时是酷安网页那版默认 id）。
        captcha_id: String,
        /// 空 = 答案写进 `validate` cookie；非空 = 作为这个查询参数回传。
        field: String,
        /// 服务端原话，界面可以直接显示。
        detail: String,
    },
}

pub(crate) fn map_error(platform: &str, e: BiliError) -> FfiError {
    let platform = platform.to_string();
    match e {
        BiliError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        BiliError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        BiliError::Signature { detail } => FfiError::Signature { platform, detail },
        BiliError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        BiliError::Parse { detail } => FfiError::Parse { platform, detail },
        BiliError::Network { detail } => FfiError::Network { platform, detail },
        BiliError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        BiliError::Host { detail } => FfiError::Internal { platform, detail },
    }
}

// ===========================================================================
// core → FFI
// ===========================================================================

fn to_ffi_kind(kind: ItemKind) -> FfiItemKind {
    match kind {
        ItemKind::Video => FfiItemKind::Video,
        ItemKind::Article => FfiItemKind::Article,
        ItemKind::Answer => FfiItemKind::Answer,
        ItemKind::Question => FfiItemKind::Question,
        ItemKind::Post => FfiItemKind::Post,
        ItemKind::Moment => FfiItemKind::Moment,
        ItemKind::Live => FfiItemKind::Live,
        ItemKind::Other => FfiItemKind::Other,
    }
}

fn to_ffi_precision(p: TimePrecision) -> FfiTimePrecision {
    match p {
        TimePrecision::Second => FfiTimePrecision::Second,
        TimePrecision::Minute => FfiTimePrecision::Minute,
        TimePrecision::Hour => FfiTimePrecision::Hour,
        TimePrecision::Day => FfiTimePrecision::Day,
        TimePrecision::Month => FfiTimePrecision::Month,
        TimePrecision::Year => FfiTimePrecision::Year,
    }
}

fn to_ffi_stats(stats: &Stats) -> FfiStats {
    FfiStats {
        views: stats.views,
        likes: stats.likes,
        comments: stats.comments,
        shares: stats.shares,
        danmaku: stats.danmaku,
        favorites: stats.favorites,
        coins: stats.coins,
    }
}

fn to_ffi_media(media: &Media) -> FfiMedia {
    match media {
        Media::Image {
            url,
            width,
            height,
        } => FfiMedia::Image {
            url: url.clone(),
            width: *width,
            height: *height,
        },
        Media::Video {
            cover,
            duration_sec,
            width,
            height,
        } => FfiMedia::Video {
            cover: cover.clone(),
            duration_sec: *duration_sec,
            width: *width,
            height: *height,
        },
    }
}

pub(crate) fn push_item(item: &FeedItem, nodes: &mut Vec<FfiItem>) -> u32 {
    // 先把转发内容塞进数组（后序），拿到下标，再塞自己。
    let repost = item.repost.as_deref().map(|r| push_item(r, nodes));
    let idx = nodes.len() as u32;
    nodes.push(FfiItem {
        platform: item.platform.clone(),
        id: item.id.clone(),
        key: item.key(),
        kind: to_ffi_kind(item.kind),
        title: item.title.clone(),
        summary: item.summary.clone(),
        language: item.language.clone(),
        author: item.author.as_ref().map(|a| FfiUserRef {
            platform: a.platform.clone(),
            id: a.id.clone(),
            name: a.name.clone(),
            avatar: a.avatar.clone(),
            url: a.url.clone(),
        }),
        published_at: item.published_at,
        published_at_precision: to_ffi_precision(item.published_at_precision),
        media: item.media.iter().map(to_ffi_media).collect(),
        stats: to_ffi_stats(&item.stats),
        reason: item.reason.clone(),
        community: item.community.clone(),
        community_icon: item.community_icon.clone(),
        repost,
        emotes: item
            .emotes
            .iter()
            .map(|(code, url)| FfiEmote {
                code: code.clone(),
                url: url.clone(),
            })
            .collect(),
        adult: item.adult,
        url: item.url.clone(),
        detail: match &item.detail {
            DetailTarget::Web { url } => FfiDetailTarget::Web { url: url.clone() },
            DetailTarget::Native { id, platform } => FfiDetailTarget::Native {
                id: id.clone(),
                platform: platform.clone(),
            },
        },
    });
    idx
}

pub(crate) fn to_ffi_page(page: &feedhub_core::model::Page) -> FfiPage {
    let mut nodes = Vec::new();
    let roots = page
        .items
        .iter()
        .map(|it| push_item(it, &mut nodes))
        .collect();
    FfiPage {
        nodes,
        roots,
        next: page.next.clone(),
    }
}

pub(crate) fn to_ffi_item_node(item: Option<&FeedItem>) -> FfiItemNode {
    match item {
        Some(item) => {
            let mut nodes = Vec::new();
            let root = push_item(item, &mut nodes);
            FfiItemNode {
                nodes,
                root: Some(root),
            }
        }
        None => FfiItemNode {
            nodes: Vec::new(),
            root: None,
        },
    }
}

// ===========================================================================
// 详情 / 评论：core → FFI
// ===========================================================================

pub(crate) fn to_ffi_user_ref(user: &feedhub_core::model::UserRef) -> FfiUserRef {
    to_ffi_user(user)
}

pub(crate) fn to_ffi_user(user: &feedhub_core::model::UserRef) -> FfiUserRef {
    FfiUserRef {
        platform: user.platform.clone(),
        id: user.id.clone(),
        name: user.name.clone(),
        avatar: user.avatar.clone(),
        url: user.url.clone(),
    }
}

fn to_ffi_target(t: &DetailTarget) -> FfiDetailTarget {
    match t {
        DetailTarget::Web { url } => FfiDetailTarget::Web { url: url.clone() },
        DetailTarget::Native { id, platform } => FfiDetailTarget::Native {
            id: id.clone(),
            platform: platform.clone(),
        },
    }
}

pub(crate) fn to_ffi_detail_span(span: &feedhub_core::detail::Span) -> FfiDetailSpan {
    use feedhub_core::detail::Span;
    match span {
        Span::Text {
            text,
            bold,
            italic,
        } => FfiDetailSpan::Text {
            text: text.clone(),
            bold: *bold,
            italic: *italic,
        },
        Span::Link { text, url, target } => FfiDetailSpan::Link {
            text: text.clone(),
            url: url.clone(),
            target: target.as_ref().map(to_ffi_target),
        },
        Span::Mention { text, user } => FfiDetailSpan::Mention {
            text: text.clone(),
            user: to_ffi_user(user),
        },
        Span::Emote { text, url } => FfiDetailSpan::Emote {
            text: text.clone(),
            url: url.clone(),
        },
    }
}

pub(crate) fn to_ffi_detail_block(block: &feedhub_core::detail::Block) -> FfiDetailBlock {
    use feedhub_core::detail::Block;
    match block {
        Block::Paragraph { spans } => FfiDetailBlock::Paragraph {
            spans: spans.iter().map(to_ffi_detail_span).collect(),
        },
        Block::Heading { spans } => FfiDetailBlock::Heading {
            spans: spans.iter().map(to_ffi_detail_span).collect(),
        },
        Block::Quote { spans } => FfiDetailBlock::Quote {
            spans: spans.iter().map(to_ffi_detail_span).collect(),
        },
        Block::Code { text, language } => FfiDetailBlock::Code {
            text: text.clone(),
            language: language.clone(),
        },
        Block::Image {
            url,
            width,
            height,
            original,
            caption,
        } => FfiDetailBlock::Image {
            url: url.clone(),
            width: *width,
            height: *height,
            original: original.clone(),
            caption: caption.clone(),
        },
        Block::Card {
            title,
            subtitle,
            cover,
            url,
            target,
        } => FfiDetailBlock::Card {
            title: title.clone(),
            subtitle: subtitle.clone(),
            cover: cover.clone(),
            url: url.clone(),
            target: target.as_ref().map(to_ffi_target),
        },
        Block::Video {
            id,
            part_id,
            cover,
            duration_sec,
            width,
            height,
            title,
            url,
        } => FfiDetailBlock::Video {
            id: id.clone(),
            part_id: part_id.clone(),
            cover: cover.clone(),
            duration_sec: *duration_sec,
            width: *width,
            height: *height,
            title: title.clone(),
            url: url.clone(),
        },
        Block::Divider => FfiDetailBlock::Divider,
    }
}

pub(crate) fn to_ffi_detail_body(body: &feedhub_core::detail::RichText) -> FfiDetailBody {
    FfiDetailBody {
        blocks: body.blocks.iter().map(to_ffi_detail_block).collect(),
    }
}

fn to_ffi_follow(state: &feedhub_core::detail::FollowState) -> FfiDetailFollow {
    use feedhub_core::detail::{FollowKind, FollowTarget};
    let FollowTarget {
        platform,
        kind,
        id,
        name,
        avatar,
        description,
        url,
    } = &state.target;
    FfiDetailFollow {
        target: FfiFollowTarget {
            platform: platform.clone(),
            kind: match kind {
                FollowKind::User => FfiFollowKind::User,
                FollowKind::Forum => FfiFollowKind::Forum,
            },
            id: id.clone(),
            name: name.clone(),
            avatar: avatar.clone(),
            description: description.clone(),
            url: url.clone(),
        },
        following: state.following,
    }
}

/// 详情 → `FfiDetail`。详情条目和相关推荐都进同一个 `nodes` 数组（转发内容由 [`push_item`] 递归塞进去）。
pub(crate) fn to_ffi_detail(detail: &feedhub_core::detail::FeedDetail) -> FfiDetail {
    let mut nodes: Vec<FfiItem> = Vec::new();
    let item = Some(push_item(&detail.item, &mut nodes));
    let related = detail
        .related
        .iter()
        .map(|it| push_item(it, &mut nodes))
        .collect();
    FfiDetail {
        nodes,
        item,
        body: to_ffi_detail_body(&detail.body),
        video: detail.video.as_ref().map(|v| FfiDetailVideoInfo {
            parts: v
                .parts
                .iter()
                .map(|p| FfiDetailVideoPart {
                    id: p.id.clone(),
                    title: p.title.clone(),
                    duration_sec: p.duration_sec,
                    width: p.width,
                    height: p.height,
                })
                .collect(),
            danmaku_segment_ms: v.danmaku_segment_ms,
        }),
        follow: detail.follow.as_ref().map(to_ffi_follow),
        related,
    }
}

/// 一条评论 → 下标。楼中楼预览**先**递归塞进数组（后序），再塞自己。
fn push_comment(comment: &feedhub_core::detail::Comment, nodes: &mut Vec<FfiComment>) -> u32 {
    let preview_replies = comment
        .preview_replies
        .iter()
        .map(|r| push_comment(r, nodes))
        .collect();
    let idx = nodes.len() as u32;
    nodes.push(FfiComment {
        platform: comment.platform.clone(),
        id: comment.id.clone(),
        author: comment.author.as_ref().map(to_ffi_user),
        content: to_ffi_detail_body(&comment.content),
        published_at: comment.published_at,
        published_at_precision: to_ffi_precision(comment.published_at_precision),
        likes: comment.likes,
        reply_count: comment.reply_count,
        reply_to: comment.reply_to.as_ref().map(to_ffi_user),
        ip_location: comment.ip_location.clone(),
        floor: comment.floor,
        pinned: comment.pinned,
        preview_replies,
        language: comment.language.clone(),
    });
    idx
}

pub(crate) fn to_ffi_comment_page(page: &feedhub_core::detail::CommentPage) -> FfiCommentPage {
    let mut nodes: Vec<FfiComment> = Vec::new();
    let roots = page.items.iter().map(|c| push_comment(c, &mut nodes)).collect();
    FfiCommentPage {
        nodes,
        roots,
        next: page.next.clone(),
    }
}

// ===========================================================================
// 导出的纯函数
// ===========================================================================

/// 这个 `.so` 的版本号（Kotlin 侧用来确认加载到的是同一份构建）。
#[uniffi::export]
pub fn core_version() -> String {
    feedhub_core::VERSION.to_string()
}

/// 解析 B站推荐流（`/x/web-interface/wbi/index/top/feed/rcmd` 的响应）。
/// `fresh_idx` 是这一页的序号；下一页永远是 `fresh_idx + 1`（推荐流没有尽头），和 Kotlin 侧一致。
#[uniffi::export]
pub fn bili_parse_recommend(body: String, fresh_idx: i64) -> Result<FfiPage, FfiError> {
    bili::parse_recommend(&body, fresh_idx)
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("bili", e))
}

/// 解析 B站搜索（`/x/web-interface/wbi/search/type` 的响应，**已经过外层 `{code}` 外壳检查**）。
///
/// 纯函数入口：对拍 / 排障用（生产路径走 `FfiBiliSource::search`，请求和解析都在那边）。
/// [`page`] 是这一页的页码（游标），决定 `numPages > page` 时给不给下一页。
#[uniffi::export]
pub fn bili_parse_search(body: String, page: i32) -> Result<FfiPage, FfiError> {
    let root = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "bili".to_string(),
        detail,
    })?;
    bili::check_envelope_object(&root).map_err(|e| map_error("bili", e))?;
    bili::search::parse_search(&root, page)
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("bili", e))
}

/// 解析贴吧搜索（全吧 / 吧内的两种响应都由 `community` 决定不了形状，所以分开两个函数）。
///
/// 纯函数入口：对拍 / 排障用（生产路径走 `FfiTiebaSource::search`）。
/// 平台错误码当数据带回去（`error`），归类仍走 Kotlin 的 `TiebaSource.mapError`。
#[uniffi::export]
pub fn tieba_parse_search_global(
    body: String,
) -> Result<crate::tieba_source::FfiTiebaHostedFeed, FfiError> {
    let root = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "tieba".to_string(),
        detail,
    })?;
    let code = json::long(&root, "no").ok_or_else(|| FfiError::Parse {
        platform: "tieba".to_string(),
        detail: "全吧搜索缺少 no".to_string(),
    })?;
    if code != 0 {
        return Ok(crate::tieba_source::failed_search_feed(feedhub_core::tieba::PlatformError {
            code: code as i32,
            user_msg: String::new(),
            error_msg: json::text(&root, "error").unwrap_or_default(),
        }));
    }
    feedhub_core::tieba::search::parse_global_page(&root)
        .map(|page| crate::tieba_source::search_feed(&page))
        .map_err(|e| map_error("tieba", e))
}

/// 解析贴吧吧内搜索（`POST /c/s/searchpost` 的响应）。`pn` 是这一页的页码（决定下一页游标）。
#[uniffi::export]
pub fn tieba_parse_search_forum(
    body: String,
    pn: i32,
) -> Result<crate::tieba_source::FfiTiebaHostedFeed, FfiError> {
    let root = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "tieba".to_string(),
        detail,
    })?;
    let code = json::long(&root, "error_code").ok_or_else(|| FfiError::Parse {
        platform: "tieba".to_string(),
        detail: "吧内搜索缺少 error_code".to_string(),
    })?;
    if code != 0 {
        return Ok(crate::tieba_source::failed_search_feed(feedhub_core::tieba::PlatformError {
            code: code as i32,
            user_msg: String::new(),
            error_msg: json::text(&root, "error_msg").unwrap_or_default(),
        }));
    }
    feedhub_core::tieba::search::parse_in_forum_page(&root, pn)
        .map(|page| crate::tieba_source::search_feed(&page))
        .map_err(|e| map_error("tieba", e))
}

/// 解析 YouTube 搜索（`POST /youtubei/v1/search` 的响应，WEB 客户端的形状）。
///
/// 纯函数入口：对拍 / 排障用（生产路径走 `FfiYoutubeSource::search`）。
/// `cursor` 是这一页发出去的那个 `continuation`（用来跳过自己，不原地打转）；`now_sec` 给相对时间换算。
#[uniffi::export]
pub fn youtube_parse_search(
    body: String,
    cursor: Option<String>,
    now_sec: i64,
) -> Result<FfiPage, FfiError> {
    let root = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "yt".to_string(),
        detail,
    })?;
    Ok(to_ffi_page(&feedhub_core::youtube::search::parse_search(
        &root,
        cursor.as_deref(),
        now_sec,
    )))
}

/// 解析知乎搜索（`/api/v4/search_v3` 的响应，**不带外壳**）。
///
/// 纯函数入口：对拍 / 排障用（生产路径走 `FfiZhihuSource::search`）。`emotes` 是**插件带的表情表**
/// （占位符 → 资源地址，和卡片共用）；正文那套 data URI 表情表不经过这条路。
#[uniffi::export]
pub fn zhihu_parse_search(body: String, emotes: Vec<FfiEmote>) -> Result<FfiPage, FfiError> {
    let table: Vec<(String, String)> = emotes.into_iter().map(|e| (e.code, e.url)).collect();
    let root = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "zhihu".to_string(),
        detail,
    })?;
    feedhub_core::zhihu::search::parse_search(&root, &table)
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("zhihu", e))
}

/// 解析 B站关注流（`/x/polymer/web-dynamic/v1/feed/all` 的响应）。
#[uniffi::export]
pub fn bili_parse_dynamics(body: String) -> Result<FfiPage, FfiError> {
    bili::parse_dynamics(&body)
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("bili", e))
}

/// 解析一条动态（`v1/detail` 那种带 `{code}` 外壳、内容在 `data.item` 的响应）。
#[uniffi::export]
pub fn bili_parse_dynamic_item(body: String) -> Result<FfiItemNode, FfiError> {
    bili::parse_dynamic_item(&body)
        .map(|item| to_ffi_item_node(item.as_ref()))
        .map_err(|e| map_error("bili", e))
}

/// 解析一条**不带外壳**的动态对象（测试边界样本用）。
#[uniffi::export]
pub fn bili_parse_dynamic(body: String) -> Result<FfiItemNode, FfiError> {
    bili::parse_dynamic_object_json(&body)
        .map(|item| to_ffi_item_node(item.as_ref()))
        .map_err(|e| map_error("bili", e))
}

/// WBI 的 mixin key（32 个字符）。
#[uniffi::export]
pub fn bili_wbi_mixin_key(img_key: String, sub_key: String) -> String {
    bili::sign::mixin_key(&img_key, &sub_key)
}

/// `https://i0.hdslb.com/bfs/wbi/<key>.png` → `<key>`。
#[uniffi::export]
pub fn bili_wbi_key_from_url(url: String) -> String {
    bili::sign::key_from_url(&url)
}

/// 从 nav 的响应里取 WBI 的两个 key。没有 / 不是 JSON 返回 `None`（未登录也照样有 `wbi_img`）。
#[uniffi::export]
pub fn bili_wbi_keys_from_nav(body: String) -> Option<FfiWbiKeys> {
    bili::wbi_keys_from_nav(&body).map(|(img_key, sub_key)| FfiWbiKeys { img_key, sub_key })
}

/// 给一份参数补上 `wts` / `w_rid`，返回**保持插入顺序**的新参数表（调用方按顺序拼 query，请求字节才不变）。
#[uniffi::export]
pub fn bili_wbi_sign(params: Vec<FfiParam>, mixin_key: String, wts: i64) -> Vec<FfiParam> {
    let input: Vec<(String, String)> = params
        .into_iter()
        .map(|p| (p.name, p.value))
        .collect();
    bili::sign::sign(&input, &mixin_key, wts)
        .into_iter()
        .map(|(name, value)| FfiParam { name, value })
        .collect()
}

#[uniffi::export]
pub fn bili_av_to_bv(aid: i64) -> Option<String> {
    bili::ids::to_bvid(aid)
}

#[uniffi::export]
pub fn bili_bv_to_av(bvid: String) -> Option<i64> {
    bili::ids::to_aid(&bvid)
}

#[uniffi::export]
pub fn bili_is_bvid(value: String) -> bool {
    bili::ids::is_bvid(&value)
}

/// 评论区的 `type` / `oid`（测试 / 排障用：请求里的 `oid` / `type` 就是它）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiBiliCommentTarget {
    pub comment_type: i32,
    pub oid: String,
}

/// 解析 B站 视频详情（`/x/web-interface/wbi/view/detail` 的响应）。
/// `logged_in` 为 false 时 `Card.following` 不可信，`follow.following` 一律是 `None`（和 Kotlin 一样）。
#[uniffi::export]
pub fn bili_parse_video_detail(body: String, logged_in: bool) -> Result<FfiDetail, FfiError> {
    let root = bili::check_envelope(&body).map_err(|e| map_error("bili", e))?;
    bili::detail::video_detail(&root, logged_in)
        .map(|detail| to_ffi_detail(&detail))
        .map_err(|detail| FfiError::Parse {
            platform: "bili".to_string(),
            detail,
        })
}

/// 解析 B站 动态详情（`v1/detail` 的响应 + 可选的 `opus/detail` 响应）。
#[uniffi::export]
pub fn bili_parse_dynamic_detail(
    detail_body: String,
    opus_body: Option<String>,
) -> Result<FfiDetail, FfiError> {
    let root = bili::check_envelope(&detail_body).map_err(|e| map_error("bili", e))?;
    let opus = match opus_body {
        Some(body) => Some(
            json::parse_object(&body).map_err(|detail| FfiError::Parse {
                platform: "bili".to_string(),
                detail,
            })?,
        ),
        None => None,
    };
    bili::detail::dynamic_detail(&root, opus.as_ref())
        .map(|detail| to_ffi_detail(&detail))
        .map_err(|detail| FfiError::Parse {
            platform: "bili".to_string(),
            detail,
        })
}

/// 这条动态的正文在 `opus/detail` 里吗（图文 / 专栏）。
#[uniffi::export]
pub fn bili_is_opus(detail_body: String) -> Result<bool, FfiError> {
    let root = bili::check_envelope(&detail_body).map_err(|e| map_error("bili", e))?;
    Ok(bili::detail::is_opus(&root))
}

/// `v1/detail` 的响应里取评论区位置（没有就 `None`）。
#[uniffi::export]
pub fn bili_comment_target(detail_body: String) -> Result<Option<FfiBiliCommentTarget>, FfiError> {
    let root = bili::check_envelope(&detail_body).map_err(|e| map_error("bili", e))?;
    Ok(bili::detail::comment_target(&root).map(|t| FfiBiliCommentTarget {
        comment_type: t.comment_type,
        oid: t.oid,
    }))
}

/// 解析一级评论（`/x/v2/reply/wbi/main` 的响应）。
#[uniffi::export]
pub fn bili_parse_comments(body: String) -> Result<FfiCommentPage, FfiError> {
    let root = bili::check_envelope(&body).map_err(|e| map_error("bili", e))?;
    bili::comment::main_list(&root)
        .map(|page| to_ffi_comment_page(&page))
        .map_err(|detail| FfiError::Parse {
            platform: "bili".to_string(),
            detail,
        })
}

/// 解析楼中楼（`/x/v2/reply/reply` 的响应）。`pn` 从 1 开始，`ps` 是每页条数。
#[uniffi::export]
pub fn bili_parse_replies(body: String, pn: i32, ps: i32) -> Result<FfiCommentPage, FfiError> {
    let root = bili::check_envelope(&body).map_err(|e| map_error("bili", e))?;
    bili::comment::sub_replies(&root, pn, ps)
        .map(|page| to_ffi_comment_page(&page))
        .map_err(|detail| FfiError::Parse {
            platform: "bili".to_string(),
            detail,
        })
}

/// 解析播放地址（`/x/player/wbi/playurl` 的响应，**已经过 `{code}` 外壳**）。
///
/// 拉流要带 [`FfiPlayInfo::headers`]（Referer / UA），顺序原样用。
#[uniffi::export]
pub fn bili_parse_play(body: String) -> Result<FfiPlayInfo, FfiError> {
    let root = bili::check_envelope(&body).map_err(|e| map_error("bili", e))?;
    bili::play::parse(&root, bili::play::play_headers())
        .map(|play| to_ffi_play(&play))
        .map_err(|e| map_error("bili", e))
}

/// 解析一段弹幕（`/x/v2/dm/wbi/web/seg.so` 的响应，**裸 protobuf**）。
///
/// 空 body 是空列表；不是合法 protobuf 时是 `Parse`（文案和 Kotlin 的 `ParseException` 一样）。
#[uniffi::export]
pub fn bili_parse_danmaku(bytes: Vec<u8>) -> Result<Vec<FfiDanmaku>, FfiError> {
    bili::danmaku::segment(&bytes)
        .map(|items| to_ffi_danmaku(&items))
        .map_err(|e| map_error("bili", e))
}

mod zhihu;
mod zhihu_source;
mod xhs;
// 翻译：FfiTrStore / FfiTrQueue / FfiTrEngine + tr_* 纯函数。
mod translate;

mod account;

mod douyin_device;
mod plugin_contract;
mod qr;
mod tieba_forum;
mod present_paging;
mod present_images;
mod present_links;

mod captcha;

mod perf;
