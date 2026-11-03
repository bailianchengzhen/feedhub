//! **小黑盒的平台对象**：整个"推荐流一页 + 详情 + 评论 + 楼中楼"的请求逻辑（拼 URL / 参数顺序 /
//! 公共参数 / `hkey` 签名 / 请求头顺序 / 翻页游标 / 表情表与游戏卡片的取用与缓存 / 状态码与错误码判定）
//! + 解析，都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读写 store、读 Cookie 罐。见 [`crate::host`]。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiHeyboxSource(base, hostHttp, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.feed(cursor, nowSec, nonce) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象除了一把"表情表内存缓存"的锁之外**无状态**，
//! 可以跨线程用（`Send + Sync`），但没有内部线程 / 定时器。
//!
//! **为什么没有 `kind` 参数**：小黑盒只有推荐流这一条（插件描述 `capabilities.feeds = {RECOMMEND}`），
//! Kotlin 门面在调进来之前就按能力拒掉别的流（`UnsupportedFeedException`，和 Kotlin 版 `feed()` 的
//! `when` 一字不差）。B站 / 酷安 有两条流，所以它们的平台对象带 `FfiFeedKind`。
//!
//! 时间戳（`_time` 和表情表的"今天"）与 nonce 都由调用方传进来：Rust 不读系统时钟、不取随机数。

use std::sync::Arc;

use feedhub_core::heybox::search::SearchSort;
use feedhub_core::heybox::source::{CommentSort, HeyboxHostedSource};

use crate::heybox::map_heybox_error;
use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_play, FfiCommentPage, FfiCommentSort,
    FfiDetail, FfiEmote, FfiError, FfiPage, FfiPlayInfo,
};

/// 搜索排序（小黑盒 `filter_list` 里的 `sort_filter`）。
///
/// 只有两档：**相关**（默认，不传 `sort_filter`）和**最新**。
/// `award_num` / `comment_num` 当前不做。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiHeyboxSearchSort {
    Relevance,
    Latest,
}

impl From<FfiHeyboxSearchSort> for SearchSort {
    fn from(s: FfiHeyboxSearchSort) -> Self {
        match s {
            FfiHeyboxSearchSort::Relevance => SearchSort::Relevance,
            FfiHeyboxSearchSort::Latest => SearchSort::Latest,
        }
    }
}

/// 小黑盒平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiHeyboxSource {
    inner: HeyboxHostedSource,
}

#[uniffi::export]
impl FfiHeyboxSource {
    /// 接上宿主的 nonce 来源：一次调用里第二个起的请求各问宿主要一个新的 nonce。
    pub fn set_nonce_source(&self, source: Arc<dyn crate::host::HostNonce>) {
        self.inner.set_nonce_source(Arc::new(crate::host::NonceAdapter(source)));
    }

    /// [`base`] 是接口根（线上 `https://api.xiaoheihe.cn`，测试指向 MockWebServer）。
    ///
    /// 表情表的缓存（`ctx.store` 的 `heybox.emotes`）由 Rust 自己读 / 写：**和 Kotlin 那条路共用同一份**，
    /// 键名和值格式一字不差。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: HeyboxHostedSource::new(
                base,
                Arc::new(HttpAdapter(http)),
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
            ),
        })
    }

    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，一次 FFI 回调都不走。
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let host = stack.build_host_public(spec)?;
        Ok(Arc::new(Self {
            inner: HeyboxHostedSource::new(
                base,
                host,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
        }))
    }

    /// 推荐流一页。
    ///
    /// [`cursor`] 就是页游标（`offset` 的十进制写法；`null` / 不是数字都是第 1 页）；
    /// [`now_sec`] 是秒级时间戳（`_time` 和表情表的"今天"都用它）；[`nonce`] 由调用方给
    /// （`SecureRandom` 留在 Kotlin）。测试注入固定值就能把两次运行的请求逐字节比。
    pub fn feed(
        &self,
        cursor: Option<String>,
        now_sec: i64,
        nonce: String,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .feed(cursor.as_deref(), now_sec, &nonce)
            .map(|page| to_ffi_page(&page))
            .map_err(map_heybox_error)
    }

    /// 搜一页（`/bbs/app/api/general/search/v1`）。
    ///
    /// [`sort`] 只有相关 / 最新两档；[`cursor`] 是帖子结果的绝对下标（下一页 = 这一页的 offset +
    /// 这一页的 link 条数）；[`now_sec`] / [`nonce`] 和 [`feed`] 一样由调用方给。
    ///
    /// 标题 / 摘要里有 `[cube_惊讶]` 这样的表情代码时，和推荐流一样会**再取一次** `emojis/list`
    /// （内存 / `ctx.store` 命中就不发）——所以一次搜索可能是 1 个或 2 个请求。
    pub fn search(
        &self,
        text: String,
        sort: FfiHeyboxSearchSort,
        cursor: Option<String>,
        now_sec: i64,
        nonce: String,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .search(&text, sort.into(), cursor.as_deref(), now_sec, &nonce)
            .map(|page| to_ffi_page(&page))
            .map_err(map_heybox_error)
    }

    /// 详情：`link/tree` 第一页（`limit=1&is_first=1`），正文里的游戏卡片自动再取一次
    /// `/game/get_game_infos/`（20 个一批，最多 8 批，失败不影响详情）。
    ///
    /// [`id`] 是 `DetailTarget.Native.id`（帖子 linkid）；[`now_sec`] / [`nonce`] 和 [`feed`] 一样由调用方给。
    pub fn detail(&self, id: String, now_sec: i64, nonce: String) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id, now_sec, &nonce)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_heybox_error)
    }

    /// 一级评论：同一个 `link/tree`，游标是页码（`None` = 第 1 页），每页 20 组。
    ///
    /// [`sort`] 三档和 Kotlin 一一对应：`Default` / `Hot` 都是 `sort_filter=hot`，`Newest` 是 `time_desc`。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        now_sec: i64,
        nonce: String,
    ) -> Result<FfiCommentPage, FfiError> {
        let sort = match sort {
            FfiCommentSort::Default => CommentSort::Default,
            FfiCommentSort::Hot => CommentSort::Hot,
            FfiCommentSort::Newest => CommentSort::Newest,
        };
        self.inner
            .comments(&id, cursor.as_deref(), sort, now_sec, &nonce)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_heybox_error)
    }

    /// 楼中楼：`lastval` 游标（`None` = 第一条），每页 20 条。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_sec: i64,
        nonce: String,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies(&id, &comment_id, cursor.as_deref(), now_sec, &nonce)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_heybox_error)
    }

    /// 播放地址：视频帖自己的视频（`partId=main`）或正文里夹着的 `seg:<n>`。
    ///
    /// 地址取 `link/tree` 第一页里的 `video_url` / 正文视频段；60 秒内 [`detail`](Self::detail) 取过
    /// 同一条帖子就直接用那份响应（**不再发请求**，和 Kotlin 的 `recentLinks` 一样），没有才现取一次。
    /// `partId` 的写法不认识时在**发请求之前**就报错。
    ///
    /// [`now_sec`] / [`nonce`] 和 [`detail`](Self::detail) 一样由调用方给。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        now_sec: i64,
        nonce: String,
    ) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id, now_sec, &nonce)
            .map(|info| to_ffi_play(&info))
            .map_err(map_heybox_error)
    }

    /// 详情之后 60 秒内的同一条帖子原始响应。
    /// 返回的是 `link/tree` 的 JSON 字符串。
    ///
    /// **播放不再需要它**，留着只是为了
    /// `RustHeyboxSourceTest` 能确认两边看到的是同一份 body。
    pub fn recent_link(&self, id: String, now_sec: i64) -> Option<String> {
        self.inner.recent_link(&id, now_sec)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }

    /// 手上已经有的表情表（内存 → `ctx.store` 里 7 天内那份）。**不发请求、不改状态。**
    pub fn cached_emotes(&self, now_sec: i64) -> Vec<FfiEmote> {
        self.inner
            .cached_emote_table(now_sec)
            .unwrap_or_default()
            .into_iter()
            .map(|(code, url)| FfiEmote { code, url })
            .collect()
    }

    /// 下一趟取表会不会真的发请求（纯查询）。
    pub fn emote_fetch_needed(&self, now_sec: i64) -> bool {
        self.inner.emote_fetch_needed(now_sec)
    }

    /// 后台把表情表补上（同一时间只许一个在飞；取不到降级成旧表 / 空表）。返回表里有几项。
    pub fn ensure_emotes(&self, now_sec: i64, nonce: String) -> Result<u32, FfiError> {
        self.inner
            .emote_table(now_sec, &nonce)
            .map(|map| map.len() as u32)
            .map_err(map_heybox_error)
    }
}

// ===========================================================================
// 账号：我是谁
// ===========================================================================

#[uniffi::export]
impl FfiHeyboxSource {
    /// 「我是谁」：`/account/restore_login`。未登录返回 `None`（不抛）。
    pub fn whoami(&self, now_sec: i64, nonce: String) -> Result<Option<crate::FfiUserRef>, FfiError> {
        self.inner
            .whoami(now_sec, &nonce)
            .map(|user| user.as_ref().map(crate::to_ffi_user_ref))
            .map_err(map_heybox_error)
    }
}

// ===========================================================================
// 扫码登录
// ===========================================================================

/// 小黑盒的网页扫码登录（`/account/get_qrcode_url/` + `/account/qr_state/`）。
///
/// 为什么是另一个对象：扫码要**临时 Cookie 罐**的宿主能力（`HostQrLogin`），而那个端口只有扫码用得上；
/// 分开放之后 `FfiHeyboxSource` 的构造函数一个字都不用改。
#[derive(uniffi::Object)]
pub struct FfiHeyboxQr {
    inner: feedhub_core::heybox::qr::HeyboxQrSource,
}

#[uniffi::export]
impl FfiHeyboxQr {
    /// 接上宿主的 nonce 来源：扫码一次轮询里的多个请求各用一个 nonce。
    pub fn set_nonce_source(&self, source: Arc<dyn crate::host::HostNonce>) {
        self.inner.set_nonce_source(Arc::new(crate::host::NonceAdapter(source)));
    }

    #[uniffi::constructor]
    pub fn new(base: String, qr: Arc<dyn crate::host::HostQrLogin>) -> Arc<Self> {
        Arc::new(Self {
            inner: feedhub_core::heybox::qr::HeyboxQrSource::new(
                base,
                Arc::new(crate::host::QrAdapter(qr)),
            ),
        })
    }

    /// 第一步：要一张二维码（`now_ms` 定有效期，`now_sec` / `nonce` 是签名用的）。
    pub fn start(
        &self,
        now_ms: i64,
        now_sec: i64,
        nonce: String,
    ) -> Result<crate::account::FfiQrLogin, FfiError> {
        self.inner
            .start(now_ms, now_sec, &nonce)
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(map_heybox_error)
    }

    /// 第二步：问一次状态。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
        now_sec: i64,
        nonce: String,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let login = crate::account::from_ffi_qr_login(&login);
        self.inner
            .poll(&login, now_ms, now_sec, &nonce)
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(map_heybox_error)
    }

    /// Rust 侧记住的接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}
