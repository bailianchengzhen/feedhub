//! **酷安的平台对象**：整个"推荐流 / 关注流 / 详情 / 评论 / 楼中楼"的请求逻辑（拼 URL / 参数顺序 /
//! 请求头顺序、翻页游标、`X-App-Token` 的计算与按秒缓存、状态码与错误码判定、人机验证的识别）+ 解析，
//! 都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有四件（构造时塞进来）：发请求（**两个端口**）、读写 store、读 Cookie 罐。见 [`crate::host`]。
//!
//! **三个 HTTP 端口**（和 Kotlin `CoolapkSource` 里那三个客户端一样，不能合成一个）：
//! - `http`：酷安的接口客户端（设备默认头 + 600ms 限速 + 代理 + Cookie 罐）——
//!   推荐 / 关注 / **详情** / **评论**（评论走 `api.coolapk.com`，客户端是同一个）/ 搜索 / 账号都走它；
//! - `links`：`b23.tv` 短链跟重定向用的（Kotlin 用的是**B站 的客户端**：不跟重定向、不带 Cookie、
//!   按 B站 的代理设置），只在"详情里是 B站 短链"时才用一次；
//! - `media`：播放地址的 `Range: bytes=0-0` 探针（Kotlin `CoolapkSource.mediaHttp`：`ctx.client(platform)`
//!   + `CookieJar.NO_COOKIES`，没有设备默认头、没有 600ms 限速）——只有 [`FfiCoolapkSource::play`] 用。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiCoolapkSource(base, commentsBase, emotes, hostHttp, hostLinks, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.comments(id, cursor, sort, nowSec, captcha) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象**无状态**（除了两条"同一秒的 token 复用"的锁），
//! 可以跨线程用（`Send + Sync`），但没有内部线程 / 定时器。
//!
//! **人机验证**：`comments` / `replies` 第一页会回 `err_request_captcha_v2`，这里**不弹验证、也不重试**，
//! 而是带类型返回 [`FfiError::CaptchaRequired`]（`captcha_id` / `field` / 服务端原话）。宿主让用户本人
//! 完成验证之后，把答案放进 [`FfiCaptchaAnswer`] 再调一次**同一个方法**（`field` 为空时答案已经写进
//! `validate` cookie，传 `field=""` 的答案也一样只是重发请求）。**不循环、不自动绕。**
//!
//! 表情表在**构造时**给一次（483 对，表归插件所有）：解析也在 Rust 里做，每页都搬一遍不值。

use std::sync::Arc;

use feedhub_core::coolapk::search::SearchSort;
use feedhub_core::coolapk::source::{CoolapkHostedSource, COMMENTS_API};
use feedhub_core::coolapk::FeedKind;
use feedhub_core::detail::CommentSort;
use feedhub_core::model::UserRef;

use crate::bili_source::FfiFeedKind;
use crate::coolapk::map_error;
use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::play::{to_ffi_play, FfiPlayInfo};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, FfiCommentPage, FfiCommentSort, FfiDetail,
    FfiEmote, FfiError, FfiPage, FfiUserRef,
};

/// 搜索排序（酷安 `/v6/search` 的 `sort` 参数：`none` / `dateline` / `hot`）。
///
/// **酷安自己的枚举**，和别家的排序不是一套（`FfiHeyboxSearchSort` 只有相关 / 最新、`FfiRedditSearchSort`
/// 有三档但取值不同）。三档在 `CoolapkPlugin.capabilities.search.sorts` 里都声明了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCoolapkSearchSort {
    /// 综合（`sort=none`）。
    Relevance,
    /// 最新（`sort=dateline`）。
    Latest,
    /// 最热（`sort=hot`）。
    Hot,
}

impl From<FfiCoolapkSearchSort> for SearchSort {
    fn from(s: FfiCoolapkSearchSort) -> Self {
        match s {
            FfiCoolapkSearchSort::Relevance => SearchSort::Relevance,
            FfiCoolapkSearchSort::Latest => SearchSort::Latest,
            FfiCoolapkSearchSort::Hot => SearchSort::Hot,
        }
    }
}

/// 用户完成人机验证之后的答案。
///
/// `field` 为空 = 答案已经写进 `.coolapk.com` 的 `validate` cookie（**宿主负责写**，和 Kotlin 那条路一样）；
/// 非空 = 服务端要求把这个查询参数带上，Rust 会**追加在参数表最后**（`addQueryParameter` 的位置）。
///
/// `proof` 是短时凭据，**宿主不许打印它**（和 Cookie / token 一个待遇）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCaptchaAnswer {
    pub field: String,
    pub proof: String,
}

impl FfiCaptchaAnswer {
    fn as_pair(&self) -> (&str, &str) {
        (self.field.as_str(), self.proof.as_str())
    }
}

/// 酷安平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiCoolapkSource {
    inner: CoolapkHostedSource,
}

#[uniffi::export]
impl FfiCoolapkSource {
    /// [`base`] 是信息流 / 关注流 / 详情的接口根（线上 `https://api2.coolapk.com`，测试指向 MockWebServer）；
    /// [`comments_base`] 是评论 / 楼中楼的根（线上 `https://api.coolapk.com`，**另一台**）；
    /// [`emotes`] 是"正文原文 → 图片地址"（`CoolapkPlugin.emotes.codes`）；
    /// [`v3_blob`] 是 v3 token 的 930 字节密码表（`AppToken.v3BlobBytes()`，表归插件所有）；
    /// 三个 `HostHttp` 端口分别是 **接口 / 短链跟随 / 播放探针**（见模块说明，别合成一个）。
    ///
    /// 设备（`X-App-Device` / UA / `X-Sdk-Int`）和 `installTime` **不从参数来**：Rust 直接读宿主 store 里
    /// 那两份（`coolapk.device` / `coolapk.install_time`），所以"只用 `ctx.store` 里那一份"是结构上保证的。
    // 9 个参数：三个宿主 HTTP 端口 + store + cookies + 表情表 + v3 密码表 + 两个接口根。
    // 拆成两个 record 反而会把"身份 / 表归谁所有"这件事搞糊，所以留着，只在这里说明一下。
    #[allow(clippy::too_many_arguments)]
    #[uniffi::constructor]
    pub fn new(
        base: String,
        comments_base: String,
        emotes: Vec<FfiEmote>,
        v3_blob: Vec<u8>,
        http: Arc<dyn HostHttp>,
        links: Arc<dyn HostHttp>,
        media: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        let table = emotes.into_iter().map(|e| (e.code, e.url)).collect();
        Arc::new(Self {
            inner: CoolapkHostedSource::new(
                base,
                comments_base,
                table,
                v3_blob,
                Arc::new(HttpAdapter(http)),
                Arc::new(HttpAdapter(links)),
                Arc::new(HttpAdapter(media)),
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
            ),
        })
    }

    /// **Rust 联网层的构造**：三个宿主 HTTP 端口换成三份 [`FfiHostSpec`]。
    ///
    /// 三份不能混（`media` / `links` 的语义见 `source-coolapk/CoolapkSource.kt`）：
    /// - `http`：设备头 + 600ms 限速 + **带 Cookie**；
    /// - `media`：**不带 Cookie、不占限速**（拉媒体文件 / `Range: bytes=0-0` 探针）；
    /// - `links`：b23.tv 短链，**用 B站 的代理档**、不带 Cookie、**不跟重定向**。
    #[allow(clippy::too_many_arguments)]
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        comments_base: String,
        emotes: Vec<FfiEmote>,
        v3_blob: Vec<u8>,
        stack: Arc<crate::net_host::FfiNetStack>,
        http_spec: crate::net_host::FfiHostSpec,
        media_spec: crate::net_host::FfiHostSpec,
        links_spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let table = emotes.into_iter().map(|e| (e.code, e.url)).collect();
        let http = stack.build_host_public(http_spec)?;
        let media = stack.build_media_spec_public(media_spec)?;
        let links = stack.build_media_spec_public(links_spec)?;
        Ok(Arc::new(Self {
            inner: CoolapkHostedSource::new(
                base,
                comments_base,
                table,
                v3_blob,
                http,
                links,
                media,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
        }))
    }

    /// 一页。[`now_sec`] 是秒级时间戳（`X-App-Token` 只取决于设备码 + 秒）——由调用方传进来，
    /// 测试才能注入固定值把两次运行的请求逐字节比。
    pub fn feed(
        &self,
        kind: FfiFeedKind,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<FfiPage, FfiError> {
        let kind = match kind {
            FfiFeedKind::Recommend => FeedKind::Recommend,
            FfiFeedKind::Following => FeedKind::Following,
        };
        self.inner
            .feed(kind, cursor.as_deref(), now_sec)
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    /// 详情：`GET /v6/feed/detail?id=…`（信息流那套 13.4.1 / API 13 的身份）。
    ///
    /// 站外视频是 `b23.tv` 短链时会用**另一个宿主端口**跟一次重定向（B站 的客户端）。
    pub fn detail(&self, id: String, now_sec: i64) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id, now_sec)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_error)
    }

    /// 一级评论：`GET {comments_base}/v6/feed/replyList` + **16.2.0 / API 16 的身份**。
    ///
    /// `captcha` 是"用户刚验证过、这次把答案带上"的那一份（第一次调用传 `None`）；要验证时返回
    /// [`FfiError::CaptchaRequired`]。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        now_sec: i64,
        captcha: Option<FfiCaptchaAnswer>,
    ) -> Result<FfiCommentPage, FfiError> {
        let sort = to_sort(sort);
        let answer = captcha.as_ref().map(FfiCaptchaAnswer::as_pair);
        self.inner
            .comments(&id, cursor.as_deref(), sort, now_sec, answer)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 楼中楼：同一个接口、`id` 换成一级评论的 id、`feedType=feed_reply`。
    pub fn replies(
        &self,
        comment_id: String,
        cursor: Option<String>,
        now_sec: i64,
        captcha: Option<FfiCaptchaAnswer>,
    ) -> Result<FfiCommentPage, FfiError> {
        let answer = captcha.as_ref().map(FfiCaptchaAnswer::as_pair);
        self.inner
            .replies(&comment_id, cursor.as_deref(), now_sec, answer)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 搜一页（`GET {base}/v6/search`，信息流那套 13.4.1 / API 13 / v2 token）。
    ///
    /// [`cursor`] 是 `page|lastItem`（上一页最后一条内容的 id）；`None` = 第一页。
    /// 撞上 `err_request_captcha*` 时**不弹验证、不重试**，直接回风控错。
    pub fn search(
        &self,
        text: String,
        sort: FfiCoolapkSearchSort,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .search(&text, sort.into(), cursor.as_deref(), now_sec)
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    /// 播放地址：`GET /v6/feed/detail?id=…` 取地址，再用 `media` 端口发一次 `Range: bytes=0-0` 探一下。
    ///
    /// `part_id` 只认 `"main"`（别的报 `bad_part`）；`quality` 不参与（酷安一条视频只有一档整段 mp4）。
    /// 防盗链拒绝（401 / 403）时回 [`FfiError::Signature`]（说清楚"签名算不出来"），**不返回注定播不了的地址**。
    ///
    /// 返回的是**播放通用模型**（`FfiPlayInfo`），和 B站 / 后面几家用同一套 record。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
        now_sec: i64,
    ) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id, quality, now_sec)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    /// 当前登录的用户（`GET {base}/v6/account/checkLoginInfo`）。
    ///
    /// **没有 `uid` Cookie 就回 `None`、一个请求都不发**；服务端说"登录信息有误"也回 `None`
    /// （Kotlin 就是这么写的）。Cookie 只查"有没有"，值不过边界。
    pub fn whoami(&self, now_sec: i64) -> Result<Option<FfiUserRef>, FfiError> {
        self.inner
            .whoami(now_sec)
            .map(|user| user.as_ref().map(to_ffi_user_ref))
            .map_err(map_error)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }

    /// 评论接口的根（排障 / 测试断言用）。
    pub fn comments_base(&self) -> String {
        self.inner.comments_base().to_string()
    }

    /// 评论接口的线上根（常量，给宿主的默认值用）。
    pub fn default_comments_base(&self) -> String {
        COMMENTS_API.to_string()
    }
}

/// `UserRef` → `FfiUserRef`（`lib.rs` 里那份是私有的；这一份只给 `whoami` 用，**不复制类型**）。
fn to_ffi_user_ref(user: &UserRef) -> FfiUserRef {
    FfiUserRef {
        platform: user.platform.clone(),
        id: user.id.clone(),
        name: user.name.clone(),
        avatar: user.avatar.clone(),
        url: user.url.clone(),
    }
}

fn to_sort(sort: FfiCommentSort) -> CommentSort {
    match sort {
        FfiCommentSort::Default => CommentSort::Default,
        FfiCommentSort::Hot => CommentSort::Hot,
        FfiCommentSort::Newest => CommentSort::Newest,
    }
}
