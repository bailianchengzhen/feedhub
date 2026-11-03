//! **B站 的平台对象**：整个"推荐流 / 关注流"的请求逻辑（拼 URL / 参数 / 头、翻页游标、
//! WBI key 的获取与缓存、错误判定）+ 解析，都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读写 store、读写 Cookie 罐。见 [`crate::host`]。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiBiliSource(base, hostHttp, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.feed(FfiFeedKind.RECOMMEND, cursor, nowSec) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象**无状态**（除了两个"只取一次"的锁），
//! 可以跨线程用（`Send + Sync`），但没有内部线程 / 定时器。

use std::sync::Arc;

use feedhub_core::bili::search::{SearchKind, SearchSort};
use feedhub_core::bili::source::{BiliHostedSource, CommentSort, FeedKind};

use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::play::{to_ffi_danmaku, to_ffi_play, FfiDanmaku, FfiPlayInfo};
use crate::{
    map_error, to_ffi_comment_page, to_ffi_detail, to_ffi_page, FfiCommentPage, FfiCommentSort,
    FfiDetail, FfiError, FfiPage,
};

/// 搜索的类型（Kotlin `SearchKind`）。`User` / `Topic` 也能传进来，但**发请求之前**就会报
/// `ParseException`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiBiliSearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

impl From<FfiBiliSearchKind> for SearchKind {
    fn from(k: FfiBiliSearchKind) -> Self {
        match k {
            FfiBiliSearchKind::All => SearchKind::All,
            FfiBiliSearchKind::Video => SearchKind::Video,
            FfiBiliSearchKind::Post => SearchKind::Post,
            FfiBiliSearchKind::User => SearchKind::User,
            FfiBiliSearchKind::Topic => SearchKind::Topic,
        }
    }
}

/// 搜索的排序（Kotlin `SearchSort`）。B站 三档都有。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiBiliSearchSort {
    Relevance,
    Latest,
    Hot,
}

impl From<FfiBiliSearchSort> for SearchSort {
    fn from(s: FfiBiliSearchSort) -> Self {
        match s {
            FfiBiliSearchSort::Relevance => SearchSort::Relevance,
            FfiBiliSearchSort::Latest => SearchSort::Latest,
            FfiBiliSearchSort::Hot => SearchSort::Hot,
        }
    }
}

/// 只搬了两条流的 kind（别的 kind 由 Kotlin 的 `BilibiliSource` 处理，不走这里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedKind {
    Recommend,
    Following,
}

impl From<FfiFeedKind> for FeedKind {
    fn from(k: FfiFeedKind) -> Self {
        match k {
            FfiFeedKind::Recommend => FeedKind::Recommend,
            FfiFeedKind::Following => FeedKind::Following,
        }
    }
}

/// B站 平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiBiliSource {
    inner: BiliHostedSource,
}

#[uniffi::export]
impl FfiBiliSource {
    /// [`base`] 是接口根（线上是 `https://api.bilibili.com`，测试指向 MockWebServer）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: BiliHostedSource::new(
                base,
                Arc::new(HttpAdapter(http)),
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
            ),
        })
    }

    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，**一次 FFI 回调都不走**。
    /// `store` 仍然过 FFI（键值存储的"值"是平台自己写的，没有搬的必要）。
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let host = stack.build_host_public(spec)?;
        Ok(Arc::new(Self {
            inner: BiliHostedSource::new(
                base,
                host,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
        }))
    }

    /// 一页。[`now_sec`] 是秒级时间戳（WBI 的 `wts`、Cookie 过期、"今天"的 WBI key 都用它）——
    /// 由调用方传进来，测试才能注入固定值。
    pub fn feed(
        &self,
        kind: FfiFeedKind,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .feed(kind.into(), cursor.as_deref(), now_sec)
            .map(|page| to_ffi_page(&page))
            .map_err(|e| map_error("bili", e))
    }

    /// 搜一页（`/x/web-interface/wbi/search/type`，WBI 签名）。
    ///
    /// [`text`] 是用户输入（原样进 `keyword`）；[`kind`] 只有内容类会发请求（`User` / `Topic` 在发请求之前
    /// 就报错）；[`cursor`] 是页码（`None` = 第 1 页）；[`page_size`] 是请求口径（插件描述的 30）；
    /// [`now_sec`] 是 WBI 的 `wts` 和"今天"。
    pub fn search(
        &self,
        text: String,
        kind: FfiBiliSearchKind,
        sort: FfiBiliSearchSort,
        cursor: Option<String>,
        page_size: i32,
        now_sec: i64,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .search(&text, kind.into(), sort.into(), cursor.as_deref(), page_size, now_sec)
            .map(|page| to_ffi_page(&page))
            .map_err(|e| map_error("bili", e))
    }

    /// 详情（视频 / 动态 / 图文 / 专栏）。[`id`] 是 `DetailTarget.Native.id`（BV 号 / `dyn:<id_str>` / 纯数字）。
    ///
    /// [`now_sec`] 和 [`feed`] 一样：WBI 的 `wts` 和"今天"的 WBI key 都用它，测试注入固定值。
    pub fn detail(&self, id: String, now_sec: i64) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id, now_sec)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(|e| map_error("bili", e))
    }

    /// 一级评论。游标是上一页给的 `next_offset`（`None` = 第一页）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        now_sec: i64,
    ) -> Result<FfiCommentPage, FfiError> {
        let sort = match sort {
            FfiCommentSort::Default => CommentSort::Default,
            FfiCommentSort::Hot => CommentSort::Hot,
            FfiCommentSort::Newest => CommentSort::Newest,
        };
        self.inner
            .comments(&id, cursor.as_deref(), sort, now_sec)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("bili", e))
    }

    /// 楼中楼。游标是下一页的页码（`None` = 第 1 页），每页 20 条。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies(&id, &comment_id, cursor.as_deref(), now_sec)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("bili", e))
    }

    /// 播放地址（`/x/player/wbi/playurl`，WBI 签名）。[`part_id`] 是分 P 的 cid。
    ///
    /// [`quality`] 是 `Quality.id`，`None` 表示让平台给"能给的最高"（B站 是 `qn=127`）。
    /// 返回的 [`FfiPlayInfo::headers`] 是**有序**的拉流请求头（Referer / UA），播放器必须原样带上。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
        now_sec: i64,
    ) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id, quality, now_sec)
            .map(|play| to_ffi_play(&play))
            .map_err(|e| map_error("bili", e))
    }

    /// 分段弹幕（`/x/v2/dm/wbi/web/seg.so`，WBI 签名，响应是裸 protobuf）。
    ///
    /// [`segment`] 从 1 开始、每段 6 分钟（详情里的 `danmakuSegmentMs`）；超出视频长度的段是**空列表**。
    pub fn danmaku(
        &self,
        id: String,
        part_id: String,
        segment: i32,
        now_sec: i64,
    ) -> Result<Vec<FfiDanmaku>, FfiError> {
        self.inner
            .danmaku(&id, &part_id, segment, now_sec)
            .map(|items| to_ffi_danmaku(&items))
            .map_err(|e| map_error("bili", e))
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}

// ===========================================================================
// 账号：我是谁 / 关注列表 / 关注·取关
// ===========================================================================

#[uniffi::export]
impl FfiBiliSource {
    /// 「我是谁」：没登录返回 `None`（不抛）。
    pub fn whoami(&self, now_sec: i64) -> Result<Option<crate::FfiUserRef>, FfiError> {
        self.inner
            .whoami(now_sec)
            .map(|user| user.as_ref().map(crate::to_ffi_user_ref))
            .map_err(|e| map_error("bili", e))
    }

    /// 关注的 UP 主，一页 50 个（游标是页码）。
    pub fn followings(
        &self,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<crate::account::FfiAccountFollowPage, FfiError> {
        self.inner
            .followings(cursor.as_deref(), now_sec)
            .map(|page| to_ffi_follow_page(&page))
            .map_err(|e| map_error("bili", e))
    }

    /// 关注（`follow = true`）/ 取关。**唯一的写操作**，只在用户点按钮时调用。
    pub fn set_following(
        &self,
        target: FfiFollowTarget,
        follow: bool,
        now_sec: i64,
    ) -> Result<(), FfiError> {
        self.inner
            .set_following(&from_ffi_follow_target(&target), follow, now_sec)
            .map_err(|e| map_error("bili", e))
    }
}

use crate::account::{from_ffi_follow_target, to_ffi_follow_page};
use crate::FfiFollowTarget;

// ===========================================================================
// 扫码登录
// ===========================================================================

/// B站 的网页扫码登录（`passport.bilibili.com/x/passport-login/web/qrcode/{generate,poll}`）。
///
/// 为什么是**另一个对象**（而不是挂在 [`FfiBiliSource`] 上）：扫码要一个**临时 Cookie 罐**的宿主能力
/// （`HostQrLogin`），而那个端口只有扫码用得上。分开放之后，`FfiBiliSource` 的构造函数
/// 一个字都不用改。对象跟着 Source 活，状态只有"平台 token → 临时罐"那张 LRU 8 的表。
#[derive(uniffi::Object)]
pub struct FfiBiliQr {
    inner: feedhub_core::bili::qr::BiliQrSource,
}

#[uniffi::export]
impl FfiBiliQr {
    /// [`base`] 是接口根，[`passport`] 是 passport 的根（线上是两个域名，测试里都指向 MockWebServer）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        passport: String,
        qr: Arc<dyn crate::host::HostQrLogin>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: feedhub_core::bili::qr::BiliQrSource::new(
                base,
                passport,
                Arc::new(crate::host::QrAdapter(qr)),
            ),
        })
    }

    /// 第一步：要一张二维码。[`now_ms`] 是毫秒时间戳（二维码 180 秒有效）。
    pub fn start(&self, now_ms: i64) -> Result<crate::account::FfiQrLogin, FfiError> {
        self.inner
            .start(now_ms)
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(|e| map_error("bili", e))
    }

    /// 第二步：问一次状态。终态之后临时罐会被丢掉。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let login = crate::account::from_ffi_qr_login(&login);
        self.inner
            .poll(&login, now_ms)
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(|e| map_error("bili", e))
    }

    /// Rust 侧记住的接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}
