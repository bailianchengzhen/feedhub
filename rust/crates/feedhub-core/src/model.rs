//! 统一模型：字段和 `core/src/main/kotlin/feedhub/core/Model.kt` 一一对应。
//!
//! 约定和 Kotlin 侧完全一样：**拿不到就是 `None`，不要编默认值**（没有播放数就别填 0）；
//! 文本一律是纯文本；`emotes` 保留插入顺序（对应 Kotlin 的 `LinkedHashMap`）。

/// 平台标识（Kotlin 的 `Platform` 就是一个字符串 id，所以这里也用字符串，加平台不用改模型）。
pub type PlatformId = String;

/// B 站的平台 id。**定了就别改**（存储键、cli 参数、条目 key 都用它）。
pub const BILI: &str = "bili";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Video,
    Article,
    Answer,
    Question,
    Post,
    Moment,
    Live,
    Other,
}

/// 发布时间精确到哪一档（对应 `FeedItem.publishedAtPrecision`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimePrecision {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRef {
    pub platform: PlatformId,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    /// 个人主页网页地址。
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
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

/// 互动数据。缺失就是 `None`，不是 0。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub views: Option<i64>,
    pub likes: Option<i64>,
    pub comments: Option<i64>,
    pub shares: Option<i64>,
    pub danmaku: Option<i64>,
    pub favorites: Option<i64>,
    pub coins: Option<i64>,
}

/// 点进去去哪（对应 `DetailTarget`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailTarget {
    /// 没有原生详情的内容（直播、番剧等）：前端只能给"在官方 App 打开"。
    Web { url: String },
    /// 原生详情：把 `id` 交给 `NativeDetailSource.detail` / `comments`。
    Native {
        id: String,
        /// 平时 `None`（和当前内容同一个平台）；站外链接指向另一个平台的内容时给出那个平台。
        platform: Option<PlatformId>,
    },
}

/// 统一信息流里的一条。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedItem {
    pub platform: PlatformId,
    /// 平台内唯一的 id（B站视频用 bvid，动态用 id_str）。
    pub id: String,
    pub kind: ItemKind,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub language: Option<String>,
    pub author: Option<UserRef>,
    /// 发布时间，Unix 秒。
    pub published_at: Option<i64>,
    pub published_at_precision: TimePrecision,
    pub media: Vec<Media>,
    pub stats: Stats,
    /// 平台给的推荐理由 / 动作说明，例如"已关注""投稿了视频"。
    pub reason: Option<String>,
    /// 所属圈子（B站动态的话题名）。
    pub community: Option<String>,
    pub community_icon: Option<String>,
    /// 转发 / 引用的原内容。
    pub repost: Option<Box<FeedItem>>,
    /// 卡片上的表情：文字代码（原样，带括号）→ 图片地址，只收标题 / 摘要里真实出现过的。
    /// 用 `Vec` 而不是 `HashMap`：要保留插入顺序（Kotlin 侧是 `LinkedHashMap`）。
    pub emotes: Vec<(String, String)>,
    /// 平台自己把这条标成了 18+ / 敏感内容。
    pub adult: bool,
    pub url: Option<String>,
    pub detail: DetailTarget,
}

impl FeedItem {
    /// 跨平台唯一键。和 Kotlin 的 `FeedItem.key` 一样在 Rust 里算好，Kotlin 不重复算。
    pub fn key(&self) -> String {
        format!("{}:{}", self.platform, self.id)
    }
}

/// 一页结果。[`next`] 是不透明游标，调用方原样传回来；`None` 表示没有下一页。
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub items: Vec<FeedItem>,
    pub next: Option<String>,
}

/// Rust 侧的业务错误。FFI 会把它拆成带类型的错误 enum（见 `feedhub-ffi`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BiliError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：验证码、请求被拦截、账号异常。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl {
        code: Option<String>,
        detail: String,
    },

    #[error("签名被拒：{detail}")]
    Signature { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    /// 响应结构和预期不符 / 不是 JSON。
    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 宿主执行请求时失败了：连不上 / 超时 / 连接被重置。宿主那边是 `java.io.IOException`，
    /// 上层映射回 `IOException`（现有约定：网络错误不包装成 `SourceException`）。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    /// 上层映射成 `ProxyUnavailableException`（cli 的退出码 6），和普通网络错误分开。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常）。别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },
}
