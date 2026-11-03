//! **知乎的平台对象**：推荐流 / 关注动态的整个"请求怎么发"（拼 URL / 参数 / 头、`x-zse-96` 签名、
//! 没 `d_c0` 时先 `POST /udid`、翻页游标、状态码 / `Location` / 错误码判定）+ 解析，都在这一个
//! `uniffi::Object` 后面。
//!
//! 宿主要做的只有两件（构造时塞进来）：发请求、读 Cookie 罐。见 [`crate::host`]。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiZhihuSource(base, hostHttp, hostCookies, emotes)
//! val page = withContext(Dispatchers.IO) { src.feed(FfiZhihuFeedKind.RECOMMEND, cursor, seed) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象**无状态**（除了一个"`d_c0` 只领一次"的锁），
//! 可以跨线程用（`Send + Sync`），但没有内部线程 / 定时器。
//!
//! 这里没有 `HostStore`：知乎没有 WBI key 那种要缓存的设备参数（Cookie 罐里就是全部）。

use std::sync::Arc;

use feedhub_core::zhihu::search::{SearchKind, SearchSort};
use feedhub_core::zhihu::source::{CommentSort, FeedKind, ZhihuHostedSource};

use crate::host::{CookiesAdapter, HostCookies, HostHttp, HttpAdapter};
use crate::zhihu::seed_byte;
use crate::{map_error, to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_play, FfiCommentPage, FfiCommentSort, FfiDetail, FfiEmote, FfiError, FfiPage, FfiPlayInfo};

/// 搜索的类型（Kotlin `SearchKind`）。知乎都收（内容类），`User` / `Topic` 由聚合层保证不会传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiZhihuSearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

impl From<FfiZhihuSearchKind> for SearchKind {
    fn from(k: FfiZhihuSearchKind) -> Self {
        match k {
            FfiZhihuSearchKind::All => SearchKind::All,
            FfiZhihuSearchKind::Video => SearchKind::Video,
            FfiZhihuSearchKind::Post => SearchKind::Post,
            FfiZhihuSearchKind::User => SearchKind::User,
            FfiZhihuSearchKind::Topic => SearchKind::Topic,
        }
    }
}

/// 搜索的排序（Kotlin `SearchSort`）。**能力里只声明了 `Relevance`**（`sort=upvoted_count` 在线被忽略），
/// 但映射照 Kotlin 抄（`Latest` → `created_time`，`Hot` → `upvoted_count`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiZhihuSearchSort {
    Relevance,
    Latest,
    Hot,
}

impl From<FfiZhihuSearchSort> for SearchSort {
    fn from(s: FfiZhihuSearchSort) -> Self {
        match s {
            FfiZhihuSearchSort::Relevance => SearchSort::Relevance,
            FfiZhihuSearchSort::Latest => SearchSort::Latest,
            FfiZhihuSearchSort::Hot => SearchSort::Hot,
        }
    }
}

/// 只搬了两个流的 kind（别的 kind 由 Kotlin 的 `ZhihuSource` 处理，不走这里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiZhihuFeedKind {
    Recommend,
    Following,
}

impl From<FfiZhihuFeedKind> for FeedKind {
    fn from(k: FfiZhihuFeedKind) -> Self {
        match k {
            FfiZhihuFeedKind::Recommend => FeedKind::Recommend,
            FfiZhihuFeedKind::Following => FeedKind::Following,
        }
    }
}

/// 知乎平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiZhihuSource {
    inner: ZhihuHostedSource,
}

#[uniffi::export]
impl FfiZhihuSource {
    /// [`base`] 是接口根（线上是 `https://www.zhihu.com`，测试指向 MockWebServer）。
    /// [`emotes`] 是表情表（占位符 → 图片地址），所有权在插件（`ZhihuSticker.urls`），Rust 只按表挑用到的。
    ///
    /// [`lens_http`] 是**播放地址（lens）那一条**的宿主端口：Kotlin 那边是另一个客户端
    /// （`ctx.client(platform).cookieJar(NO_COOKIES)` + UA + 600ms 限速）。不给就传 `None`，
    /// 那时 `play` 直接报内部错误、一个请求都不发（比"悄悄带上 Cookie"好）。
    /// [`lens_base`] 是 lens 的根（线上 `https://lens.zhihu.com`，测试指向 MockWebServer）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        http: Arc<dyn HostHttp>,
        cookies: Arc<dyn HostCookies>,
        emotes: Vec<FfiEmote>,
        lens_http: Option<Arc<dyn HostHttp>>,
        lens_base: Option<String>,
    ) -> Arc<Self> {
        let emotes: Vec<(String, String)> =
            emotes.into_iter().map(|e| (e.code, e.url)).collect();
        let inner = match lens_http {
            Some(lens) => ZhihuHostedSource::with_lens(
                base,
                Arc::new(HttpAdapter(http)),
                Arc::new(HttpAdapter(lens)),
                Arc::new(CookiesAdapter(cookies)),
                emotes,
            ),
            None => ZhihuHostedSource::new(
                base,
                Arc::new(HttpAdapter(http)),
                Arc::new(CookiesAdapter(cookies)),
                emotes,
            ),
        };
        let inner = match lens_base {
            Some(lens_base) => inner.with_lens_base(lens_base),
            None => inner,
        };
        Arc::new(Self { inner })
    }

    /// **Rust 联网层的构造**。
    ///
    /// `lens` 那份是**匿名端口**（不带 Cookie、不签名，知乎的播放地址走它）——给 `None` 就和原来
    /// 不给 `lens_http` 一样（`play` 报内部错误、一个请求都不发）。
    #[allow(clippy::too_many_arguments)]
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        emotes: Vec<FfiEmote>,
        lens_spec: Option<crate::net_host::FfiHostSpec>,
        lens_base: Option<String>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let emotes: Vec<(String, String)> = emotes.into_iter().map(|e| (e.code, e.url)).collect();
        let http = stack.build_host_public(spec)?;
        let inner = match lens_spec {
            Some(spec) => ZhihuHostedSource::with_lens(
                base,
                http,
                stack.build_media_spec_public(spec)?,
                stack.cookies_host(),
                emotes,
            ),
            None => ZhihuHostedSource::new(base, http, stack.cookies_host(), emotes),
        };
        let inner = match lens_base {
            Some(lens_base) => inner.with_lens_base(lens_base),
            None => inner,
        };
        Ok(Arc::new(Self { inner }))
    }

    /// 一页。[`seed`] 是 `x-zse-96` 的明文首字节（0..=255），由调用方生成（网页是 `Math.random() * 127`）——
    /// 测试里注入固定值，两次运行的请求才能逐字节比。
    pub fn feed(
        &self,
        kind: FfiZhihuFeedKind,
        cursor: Option<String>,
        seed: i32,
    ) -> Result<FfiPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .feed(kind.into(), cursor.as_deref(), seed)
            .map(|page| to_ffi_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 搜一页（`/api/v4/search_v3`，`x-zse-96` 签名）。
    ///
    /// [`cursor`] 是上一页 `paging.next` 的 query（`None` = 第一页）；[`seed`] 是签名的随机首字节；
    /// [`page_size`] 是请求口径（插件描述的 20）。只收 `type=search_result` 的卡片（用户 / 话题 / 热词丢掉），
    /// 标题 / 摘要里的 `<em>` 高亮去掉。
    pub fn search(
        &self,
        text: String,
        kind: FfiZhihuSearchKind,
        sort: FfiZhihuSearchSort,
        cursor: Option<String>,
        page_size: i32,
        seed: i32,
    ) -> Result<FfiPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .search(&text, kind.into(), sort.into(), cursor.as_deref(), page_size, seed)
            .map(|page| to_ffi_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 详情：`GET /api/v4/{answer,article,pin,zvideo,question}/{id}`。`id` 是 `answer:123` 那种带类型的 id。
    ///
    /// [`seed`] 和 [`feed`](Self::feed) 一样是 `x-zse-96` 的随机首字节——**每个请求都要一个新的**
    /// （Kotlin 版每次 `sign()` 都调一次 `seed()`），所以由调用方每次传。
    pub fn detail(&self, id: String, seed: i32) -> Result<FfiDetail, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .detail(&id, seed)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 一级评论。`order_by`：`DEFAULT` / `HOT` 是 `score`（热门），`NEWEST` 是 `ts`；
    /// 翻页用游标（上一页 `paging.next` 的 query）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        seed: i32,
    ) -> Result<FfiCommentPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .comments(&id, cursor.as_deref(), sort.into(), seed)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 楼中楼：`comment_id` 是一级评论的 id。
    pub fn replies(
        &self,
        comment_id: String,
        cursor: Option<String>,
        seed: i32,
    ) -> Result<FfiCommentPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .replies(&comment_id, cursor.as_deref(), seed)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 信息流卡片的**问题版面**：回答只显示它的问题（没有答主、摘要是问题描述、计数是回答数）。
    pub fn set_question_cards(&self, on: bool) {
        self.inner.set_question_cards(on);
    }

    /// 详情的**问题版面**：回答 / 问题两种 id 显示问题（标题、描述、提问者、计数），
    /// 条目身份不变；别的类型同 [`detail`](Self::detail)。点回答进来是两个请求（回答 + 问题）。
    pub fn qa_detail(&self, id: String, seed: i32) -> Result<FfiDetail, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .qa_detail(&id, seed)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 评论区放**回答**（`/api/v4/questions/{qid}/feeds`）：推过来的那条第一页置顶；别的类型同
    /// [`comments`](Self::comments)。
    pub fn qa_answers(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        seed: i32,
    ) -> Result<FfiCommentPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .qa_answers(&id, cursor.as_deref(), sort.into(), seed)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 楼中楼：`answer:<id>` 那一"楼"是这条回答的评论（摊平）；别的同 [`replies`](Self::replies)。
    pub fn qa_replies(
        &self,
        comment_id: String,
        cursor: Option<String>,
        seed: i32,
    ) -> Result<FfiCommentPage, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .qa_replies(&comment_id, cursor.as_deref(), seed)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(|e| map_error("zhihu", e))
    }

    /// **播放地址**（`GET {lens}/api/v4/videos/{video_id}`）：匿名、不带 Cookie、不签名。
    ///
    /// [`part_id`] 是 lens 视频 id（zvideo 的 `video.video_id`，或正文视频盒子的 `data-lens-id`）。
    /// 写法不对（空 / 不是数字）在**发请求之前**就抛 `bad_id`；`403` / `429` 是风控，`404` 是
    /// "视频不存在或已删除"，其余非 2xx 带 body 前 200 个字符（全照 Kotlin）。
    pub fn play(&self, part_id: String) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&part_id)
            .map(|info| to_ffi_play(&info))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}

impl From<FfiCommentSort> for CommentSort {
    fn from(sort: FfiCommentSort) -> Self {
        match sort {
            FfiCommentSort::Default => CommentSort::Default,
            FfiCommentSort::Hot => CommentSort::Hot,
            FfiCommentSort::Newest => CommentSort::Newest,
        }
    }
}

// ===========================================================================
// 账号：我是谁 + 扫码登录
// ===========================================================================

#[uniffi::export]
impl FfiZhihuSource {
    /// 「我是谁」：没有 `z_c0` 时**一个请求都不发**（直接 `None`）；401 / `need_login` 也是 `None`。
    pub fn whoami(&self, seed: i32) -> Result<Option<crate::FfiUserRef>, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .whoami(seed)
            .map(|user| user.as_ref().map(crate::to_ffi_user_ref))
            .map_err(|e| map_error("zhihu", e))
    }
}

/// 知乎的网页扫码登录（`/signin` → `/udid` → `login/qrcode` → `scan_info`）。
///
/// 为什么是另一个对象：扫码要**临时 Cookie 罐**的宿主能力（`HostQrLogin`），而那个端口只有扫码用得上；
/// 分开放之后 `FfiZhihuSource` 的构造函数一个字都不用改。
#[derive(uniffi::Object)]
pub struct FfiZhihuQr {
    inner: feedhub_core::zhihu::qr::ZhihuQrSource,
}

#[uniffi::export]
impl FfiZhihuQr {
    /// [`cookie_domain`] 是 `base` 的 top private domain（线上 `zhihu.com`；测试里是 MockWebServer 的 host），
    /// 由 Kotlin 按 `HttpUrl.topPrivateDomain()` 算好传进来——Rust 不背公共后缀表。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        cookie_domain: String,
        qr: Arc<dyn crate::host::HostQrLogin>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: feedhub_core::zhihu::qr::ZhihuQrSource::new(
                base,
                cookie_domain,
                Arc::new(crate::host::QrAdapter(qr)),
            ),
        })
    }

    /// 第一步：要一张二维码（`now_ms` 定有效期，`seed` 是 `x-zse-96` 的明文首字节）。
    pub fn start(&self, now_ms: i64, seed: i32) -> Result<crate::account::FfiQrLogin, FfiError> {
        let seed = seed_byte(seed)?;
        self.inner
            .start(now_ms, seed)
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 第二步：问一次状态。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
        seed: i32,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let seed = seed_byte(seed)?;
        let login = crate::account::from_ffi_qr_login(&login);
        self.inner
            .poll(&login, now_ms, seed)
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(|e| map_error("zhihu", e))
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}
