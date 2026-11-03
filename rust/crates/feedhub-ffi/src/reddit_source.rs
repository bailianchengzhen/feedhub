//! **Reddit 的平台对象**：整个"推荐流 / 关注流"的请求逻辑（拼 URL / 参数顺序 / 请求头顺序、翻页游标、
//! 登录态判定、状态码与风控信号判定）+ 解析，都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有两件（构造时塞进来）：发请求、读 Cookie 罐。见 [`crate::host`]。
//! **没有 store**：Reddit 这条路上没有设备码 / 签名 / 时间戳要缓存，信息流请求也不带时间参数
//! （这是它和 B站（WBI key）、酷安（设备 + `X-App-Token`）最大的不同）。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiRedditSource(base, hostHttp, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.feed(FfiFeedKind.RECOMMEND, cursor) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象**完全无状态**（没有锁、没有缓存、没有内部线程 / 定时器）。

use std::sync::Arc;

use feedhub_core::detail::CommentSort;
use feedhub_core::reddit::source::{FeedKind, RedditHostedSource, SearchSort};
use feedhub_core::reddit::RedditError;

// 流的种类和 B站 平台对象共用一份（UniFFI 里同名类型只能定义一次）。
use crate::bili_source::FfiFeedKind;
use crate::host::{CookiesAdapter, HostCookies, HostHttp, HttpAdapter};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_play, to_ffi_user, FfiCommentPage,
    FfiCommentSort, FfiDetail, FfiError, FfiPage, FfiParam, FfiPlayInfo, FfiUserRef,
};

const PLATFORM: &str = "reddit";

/// 头像表（`author_fullname` → 地址）过边界的样子：`FfiParam` 的 `name` / `value`。
fn avatars_of(pairs: &[FfiParam]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|p| (p.name.clone(), p.value.clone()))
        .collect()
}

/// 纯函数：`/comments/<id>.json` 里那个帖子（`[0].data.children[0].data`）→ 详情。
///
/// 和 [`FfiRedditSource::detail`] 的区别只是"响应从哪来"：这里由调用方把 body 递进来，
/// 不发请求。给**对拍**（`RedditDetailDualRunTest`）和**测量**（`RedditDetailRustBenchTest`）用。
#[uniffi::export]
pub fn reddit_parse_detail(
    post: String,
    avatars: Vec<FfiParam>,
) -> Result<FfiDetail, FfiError> {
    let map = feedhub_core::json::parse_object(&post)
        .map_err(|detail| FfiError::Parse { platform: PLATFORM.to_string(), detail })?;
    feedhub_core::reddit::detail::parse_detail(&map, &avatars_of(&avatars))
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_error)
}

/// 纯函数：一个 Listing（`[1]`，评论区）→ 一页一级评论。
#[uniffi::export]
pub fn reddit_parse_comments(
    listing: String,
    avatars: Vec<FfiParam>,
) -> Result<FfiCommentPage, FfiError> {
    let map = feedhub_core::json::parse_object(&listing)
        .map_err(|detail| FfiError::Parse { platform: PLATFORM.to_string(), detail })?;
    Ok(to_ffi_comment_page(
        &feedhub_core::reddit::detail::parse_comments(&map, &avatars_of(&avatars)),
    ))
}

/// 纯函数：`/comments/<postId>/_/<commentId>.json` 的响应（数组 / 单 Listing 都认）→ 铺平的一层楼中楼。
#[uniffi::export]
pub fn reddit_parse_comment_thread(
    json: String,
    post_id: String,
    comment_id: String,
    avatars: Vec<FfiParam>,
) -> Result<FfiCommentPage, FfiError> {
    let value = feedhub_core::reddit::detail::parse_element(&json).map_err(map_error)?;
    Ok(to_ffi_comment_page(
        &feedhub_core::reddit::detail::parse_comment_thread(
            &value,
            &post_id,
            &comment_id,
            &avatars_of(&avatars),
        ),
    ))
}

/// 纯函数：`/api/morechildren` 的响应 → 一页评论。
#[uniffi::export]
pub fn reddit_parse_more_children(
    json: String,
    avatars: Vec<FfiParam>,
) -> Result<FfiCommentPage, FfiError> {
    let map = feedhub_core::json::parse_object(&json)
        .map_err(|detail| FfiError::Parse { platform: PLATFORM.to_string(), detail })?;
    Ok(to_ffi_comment_page(
        &feedhub_core::reddit::detail::parse_more_children(&map, &avatars_of(&avatars)),
    ))
}

/// 搜索的排序。
///
/// 放在这个文件里而不是 `lib.rs`：只有 Reddit 用得到（别的家还没搬搜索），
/// 合并时不会和同时在做搜索的小黑盒 / 小红书撞。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRedditSearchSort {
    Relevance,
    Latest,
    Hot,
}

impl FfiRedditSearchSort {
    fn to_core(self) -> SearchSort {
        match self {
            Self::Relevance => SearchSort::Relevance,
            Self::Latest => SearchSort::Latest,
            Self::Hot => SearchSort::Hot,
        }
    }
}

/// `RedditError` → `FfiError`（带类型过边界，Kotlin 侧再映射回现有异常）。
///
/// `Host` / `Internal` 都算"宿主 / 内部的意外"，到 Kotlin 是 `SourceException`（不变成崩溃）；
/// `Network` / `ProxyUnavailable` 必须分开带回去（代理不可用是 fail-closed，cli 的退出码 6 靠它）。
pub(crate) fn map_error(e: RedditError) -> FfiError {
    let platform = PLATFORM.to_string();
    match e {
        RedditError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        RedditError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        RedditError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        RedditError::Parse { detail } => FfiError::Parse { platform, detail },
        RedditError::Network { detail } => FfiError::Network { platform, detail },
        RedditError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        RedditError::Host { detail } => FfiError::Internal { platform, detail },
    }
}

/// Reddit 平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiRedditSource {
    inner: RedditHostedSource,
}

#[uniffi::export]
impl FfiRedditSource {
    /// [`base`] 是接口根（线上 `https://www.reddit.com`，测试指向 MockWebServer）。
    /// 登录态只看宿主 Cookie 罐里的 `reddit_session`（构造时不检查，等真的 `feed` 时再看）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        http: Arc<dyn HostHttp>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: RedditHostedSource::new(
                base,
                Arc::new(HttpAdapter(http)),
                Arc::new(CookiesAdapter(cookies)),
            ),
        })
    }

    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，**一次 FFI 回调都不走**。
    ///
    /// 和 [`new`](Self::new) 的区别只有"`HttpHost` 是谁"：那边是 Kotlin 的实现（`RustHttpHost` → OkHttp），
    /// 这边是 [`crate::net_host::NetHttpHost`]（Rust 内部）。**没有开关**——产品路径用这一个；
    /// `new` 那一个只留给"宿主自己的传输层"（现在只剩测试参照）。
    ///
    /// 为什么不能借用 `new`（把 `HttpHost` 传成 `RustNetHttpHost` 那样的 Kotlin 包装）：那样每个请求
    /// 仍然要过一次 UniFFI 回调，而回调里抛的任何 Kotlin 异常（`ProxyUnavailableException` 也算）
    /// 都会被 UniFFI 包成 `UnexpectedUniFFICallbackError`，fail-closed 那一档的异常类型就丢了
    /// （`source-reddit` 的 fail-closed 门当场抓出来的）。
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let host = stack.build_host_public(spec)?;
        Ok(Arc::new(Self {
            inner: RedditHostedSource::new(base, host.clone(), stack.cookies_host()),
        }))
    }

    /// 登录态 / 账号：`GET /api/me.json?raw_json=1`。
    ///
    /// 没登录（没有 `reddit_session`）时**一个请求都不发**、直接回 `None`；401、以及 403 +
    /// JSON body（Reddit 未登录时的那个 `{"message":"Forbidden","error":403}`）也回 `None`；
    /// 403 的 HTML 拦截页 / 429 / 限流用尽 → `FfiError`（和 Kotlin 的 `whoami` 同一条路，不重试）。
    /// 返回的 `FfiUserRef` 就是 `UserRef` 的同一份形状（`FeedItem.author` 用的那个 record），
    pub fn whoami(&self) -> Result<Option<FfiUserRef>, FfiError> {
        self.inner
            .whoami()
            .map(|user| user.as_ref().map(to_ffi_user))
            .map_err(map_error)
    }

    /// 一页。游标是 `data.after`（不透明字符串），没有就是第一页。
    pub fn feed(&self, kind: FfiFeedKind, cursor: Option<String>) -> Result<FfiPage, FfiError> {
        let kind = match kind {
            FfiFeedKind::Recommend => FeedKind::Recommend,
            FfiFeedKind::Following => FeedKind::Following,
        };
        self.inner
            .feed(kind, cursor.as_deref())
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    /// 搜索的一页（`GET /search.json`）。游标同样是 `data.after`；没登录时**一个请求都不发**。
    ///
    pub fn search(
        &self,
        text: String,
        sort: FfiRedditSearchSort,
        cursor: Option<String>,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .search(&text, sort.to_core(), cursor.as_deref())
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    /// 详情（`/comments/<id>.json`，`limit=1`）：帖子 + 正文（markdown → 富文本）+ 视频分 P。
    ///
    /// 正文作者头像走 `/api/user_data_by_account_ids.json`（**最多一个**请求，账号在平台对象里缓存）。
    pub fn detail(&self, id: String) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_error)
    }

    /// 一级评论。游标是 `more:<id1>,<id2>,…`（`None` = 第一页）；翻页走 `/api/morechildren`。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
    ) -> Result<FfiCommentPage, FfiError> {
        let sort = match sort {
            FfiCommentSort::Default => CommentSort::Default,
            FfiCommentSort::Hot => CommentSort::Hot,
            FfiCommentSort::Newest => CommentSort::Newest,
        };
        self.inner
            .comments(&id, cursor.as_deref(), sort)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 楼中楼：`/comments/<postId>/_/<commentId>.json`；[`comment_id`] 以 `more:` 开头时走
    /// `/api/morechildren`（和 Kotlin 一样，这个接口没有"下一页"参数）。
    pub fn replies(&self, id: String, comment_id: String) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies(&id, &comment_id)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// **播放地址**（`partId = main`）：`/comments/<id>.json?limit=1` 取帖子 → 清单（`DASHPlaylist.mpd`）
    /// → 有 DASH 用 DASH，否则 `hls_url` → `fallback_url` → `no_video`。
    ///
    /// 清单那一个请求失败（HTTP 非 2xx / 结构不对）**不算错误**，回退链继续走（和 Kotlin 一样）。
    /// `partId` 不是 `main` 时在**发请求之前**就报错。
    pub fn play(&self, id: String, part_id: String) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}
