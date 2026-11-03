//! **YouTube 的平台对象**：TV `browse`（推荐 / 订阅）、搜索、详情、评论 / 楼中楼、播放取流、
//! `whoami`、设备码登录状态机与 token 刷新编排、`raw` 调试请求，全部在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：Rust 联网层 `FfiNetStack`、读写 `HostStore`、可选的
//! QuickJS `HostScript`（解 n/sig）。登录 token 在 `HostStore` 内闭环存取与单飞刷新，不经 FFI 往返暴露。
//!
//! 另外导出三个**纯函数**（[`youtube_parse_detail`] / [`youtube_parse_comments`] /
//! [`youtube_parse_comments_token`]）：给对拍和排障用，生产路径不经过它们。

use std::sync::Arc;

use feedhub_core::bili::source::CommentSort;
use feedhub_core::detail::FeedDetail;
use feedhub_core::json;
use feedhub_core::youtube::account::{LoginUrls, PollOutcome, StartedDeviceCode};
use feedhub_core::youtube::search::SearchKind;
use feedhub_core::youtube::source::{FeedKind, YoutubeHostedSource};
use feedhub_core::youtube::{detail, YoutubeError};

// 流的种类和 B站 平台对象共用一份（UniFFI 里同名类型只能定义一次）。
use crate::bili_source::FfiFeedKind;
use crate::host::{
    FfiHeader, FfiHttpRequest, FfiHttpResponse, HostScript, HostStore, ScriptAdapter, StoreAdapter,
};
use crate::play::{to_ffi_play, FfiPlayInfo};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_user, FfiCommentPage, FfiCommentSort,
    FfiDetail, FfiError, FfiPage, FfiUserRef,
};

const PLATFORM: &str = "yt";

/// 搜索的类型（Kotlin `SearchKind`）。只有 `Video` 会多带 `params=EgIQAQ==`（别的都不带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiYoutubeSearchKind {
    All,
    Video,
    Post,
    User,
    Topic,
}

impl From<FfiYoutubeSearchKind> for SearchKind {
    fn from(k: FfiYoutubeSearchKind) -> Self {
        match k {
            FfiYoutubeSearchKind::All => SearchKind::All,
            FfiYoutubeSearchKind::Video => SearchKind::Video,
            FfiYoutubeSearchKind::Post => SearchKind::Post,
            FfiYoutubeSearchKind::User => SearchKind::User,
            FfiYoutubeSearchKind::Topic => SearchKind::Topic,
        }
    }
}

/// 搜索的排序（Kotlin `SearchSort`）。**能力里只声明了 `Relevance`**（排序 / 时间筛选的 `params`
/// 没有出处），所以这里只有一个变体——传别的进不来，比"静默退化成相关"安全。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiYoutubeSearchSort {
    Relevance,
}

/// `YoutubeError` → `FfiError`（带类型过边界，Kotlin 侧再映射回现有异常）。
///
/// `Host` 算"宿主 / 内部的意外"，到 Kotlin 是 `SourceException`（不变成崩溃）；
/// `Network` / `ProxyUnavailable` 必须分开带回去（代理不可用是 fail-closed，cli 的退出码 6 靠它）。
/// `AgeRestricted` 在详情路径上由 `detail` 转成 18+ 隐藏详情，播放路径映射成
/// `Api { code: "AgeRestricted" }`，别让它变成风控或崩溃。
pub(crate) fn map_error(e: YoutubeError) -> FfiError {
    let platform = PLATFORM.to_string();
    match e {
        YoutubeError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        YoutubeError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code: Some(code),
            detail,
        },
        YoutubeError::AgeRestricted { detail } => FfiError::Api {
            platform,
            code: "AgeRestricted".to_string(),
            detail,
        },
        YoutubeError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        YoutubeError::Parse { detail } => FfiError::Parse { platform, detail },
        YoutubeError::Network { detail } => FfiError::Network { platform, detail },
        YoutubeError::ProxyUnavailable { detail } => {
            FfiError::ProxyUnavailable { platform, detail }
        }
        YoutubeError::Host { detail } => FfiError::Internal { platform, detail },
    }
}

fn sort_of(sort: FfiCommentSort) -> CommentSort {
    match sort {
        FfiCommentSort::Default => CommentSort::Default,
        FfiCommentSort::Hot => CommentSort::Hot,
        FfiCommentSort::Newest => CommentSort::Newest,
    }
}

/// 设备码要用的四个地址（Kotlin `LoginUrls`）。测试换成 MockWebServer 时走这里。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiYoutubeLoginUrls {
    pub tv_device_code: String,
    pub tv_token: String,
    pub google_device_code: String,
    pub google_token: String,
}

impl From<FfiYoutubeLoginUrls> for LoginUrls {
    fn from(u: FfiYoutubeLoginUrls) -> Self {
        LoginUrls {
            tv_device_code: u.tv_device_code,
            tv_token: u.tv_token,
            google_device_code: u.google_device_code,
            google_token: u.google_token,
        }
    }
}

/// 取一次设备码的结果（Kotlin `DeviceCodeLogin`；抓到的 TV client 凭据已在 Rust 内直接写回 store）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiYoutubeDeviceCode {
    /// **机密**：轮询时回传，不打印。
    pub device_code: String,
    /// 给用户看的短码（`ABCD-1234`），可以显示。
    pub user_code: String,
    pub verification_url: String,
    pub poll_interval_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub hint: String,
}

impl From<StartedDeviceCode> for FfiYoutubeDeviceCode {
    fn from(s: StartedDeviceCode) -> Self {
        Self {
            device_code: s.device_code,
            user_code: s.user_code,
            verification_url: s.verification_url,
            poll_interval_ms: s.poll_interval_ms,
            expires_at_ms: s.expires_at_ms,
            hint: s.hint,
        }
    }
}

/// 一次轮询的结果（Kotlin `DeviceCodeLoginState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiYoutubePollKind {
    Waiting,
    Expired,
    Denied,
    Succeeded,
    Failed,
}

/// 轮询状态（换到的 token 已在 Rust 内直接写进 store，不再跨 FFI 传回 JVM）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiYoutubePoll {
    pub kind: FfiYoutubePollKind,
    /// 只有 `Failed` 时有（一句给用户看的话，**不含凭据**）。
    pub message: Option<String>,
}

/// YouTube 平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiYoutubeSource {
    inner: YoutubeHostedSource,
    /// n/sig 的 JS 求解在宿主（QuickJS 在 Kotlin 那边）。`None` = 宿主没提供（只有测试会这样）。
    script: Option<Arc<dyn HostScript>>,
}

#[uniffi::export]
impl FfiYoutubeSource {
    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，一次 FFI 回调都不走。
    /// n/sig 的 JS 求解仍然是宿主的 `HostScript`（quickjs 只有宿主有）。
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        login_urls: Option<FfiYoutubeLoginUrls>,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
        script: Option<Arc<dyn HostScript>>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let urls = login_urls.map(LoginUrls::from).unwrap_or_default();
        let http = stack.build_host_public(spec)?;
        Ok(Arc::new(Self {
            inner: YoutubeHostedSource::with_login_urls(
                base,
                http,
                Arc::new(StoreAdapter(store)),
                urls,
            ),
            script,
        }))
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }

    /// 搜一页（`POST /youtubei/v1/search`，**WEB 客户端**，一律匿名）。
    pub fn search(
        &self,
        text: String,
        kind: FfiYoutubeSearchKind,
        sort: FfiYoutubeSearchSort,
        cursor: Option<String>,
        now_sec: i64,
    ) -> Result<FfiPage, FfiError> {
        let _ = sort;
        self.inner
            .search(&text, kind.into(), cursor.as_deref(), now_sec)
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    pub fn feed_session(
        &self,
        kind: FfiFeedKind,
        cursor: Option<String>,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<FfiPage, FfiError> {
        let kind = match kind {
            FfiFeedKind::Recommend => FeedKind::Recommend,
            FfiFeedKind::Following => FeedKind::Following,
        };
        self.inner
            .feed_session(kind, cursor.as_deref(), now_sec, now_ms)
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    pub fn whoami_session(&self, now_ms: i64) -> Result<Option<FfiUserRef>, FfiError> {
        self.inner
            .whoami_session(now_ms)
            .map(|user| user.as_ref().map(to_ffi_user))
            .map_err(map_error)
    }

    pub fn detail_session(
        &self,
        id: String,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail_session(&id, now_sec, now_ms)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_error)
    }

    pub fn comments_session(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .comments_session(&id, cursor.as_deref(), sort_of(sort), now_sec, now_ms)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    pub fn replies_session(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_sec: i64,
        now_ms: i64,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies_session(&id, &comment_id, cursor.as_deref(), now_sec, now_ms)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    pub fn play_session(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
        now_ms: i64,
    ) -> Result<FfiPlayInfo, FfiError> {
        let script = self
            .script
            .as_ref()
            .map(|host| ScriptAdapter(host.clone()));
        let script_ref: Option<&dyn feedhub_core::host::ScriptHost> =
            script.as_ref().map(|s| s as &dyn feedhub_core::host::ScriptHost);
        self.inner
            .play_session(&id, &part_id, quality, now_ms, script_ref)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    pub fn start_device_code_session(&self, now_ms: i64) -> Result<FfiYoutubeDeviceCode, FfiError> {
        self.inner
            .start_device_code_session(now_ms)
            .map(FfiYoutubeDeviceCode::from)
            .map_err(map_error)
    }

    pub fn poll_device_code_session(
        &self,
        device_code: String,
        now_ms: i64,
    ) -> Result<FfiYoutubePoll, FfiError> {
        let outcome = self
            .inner
            .poll_device_code_session(&device_code, now_ms)
            .map_err(map_error)?;
        Ok(match outcome {
            // `SlowDown` 会话层已经记下退避、换成了 `Waiting`，这里只是让匹配完整
            PollOutcome::Waiting | PollOutcome::SlowDown => FfiYoutubePoll {
                kind: FfiYoutubePollKind::Waiting,
                message: None,
            },
            PollOutcome::Expired => FfiYoutubePoll {
                kind: FfiYoutubePollKind::Expired,
                message: None,
            },
            PollOutcome::Denied => FfiYoutubePoll {
                kind: FfiYoutubePollKind::Denied,
                message: None,
            },
            PollOutcome::Succeeded(_) => FfiYoutubePoll {
                kind: FfiYoutubePollKind::Succeeded,
                message: None,
            },
            PollOutcome::Failed(message) => FfiYoutubePoll {
                kind: FfiYoutubePollKind::Failed,
                message: Some(message),
            },
        })
    }

    pub fn logout_session(&self) {
        self.inner.logout_session();
    }

    pub fn keep_session_fresh(&self, now_ms: i64) {
        self.inner.keep_session_fresh(now_ms);
    }

    pub fn refresh_session_in_store(&self, now_ms: i64) -> Result<bool, FfiError> {
        self.inner.refresh_in_store(now_ms).map_err(map_error)
    }

    pub fn has_session(&self, now_ms: i64) -> bool {
        self.inner.has_session(now_ms)
    }

    /// 调试 `raw` 请求：共享平台 `HttpHost`，相对路径拼 `base`，仅对 YouTube/`base` 域名补默认头。
    pub fn raw(&self, request: FfiHttpRequest) -> Result<FfiHttpResponse, FfiError> {
        let headers = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        let body = if request.body.is_empty() {
            None
        } else {
            Some(request.body)
        };
        let response = self
            .inner
            .raw(request.method, request.url, headers, body)
            .map_err(map_error)?;
        Ok(FfiHttpResponse {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| FfiHeader { name, value })
                .collect(),
            body: response.body,
            final_url: response.final_url,
        })
    }
}

// ===========================================================================
// 纯函数
// ===========================================================================

/// **纯函数**：详情解析（`player` body + 可选的 `next {videoId}` body → `FeedDetail`）。
///
/// 组合的就是生产路径上的那几步（`video` → `video_meta` → `merge_meta` → `item_of` / `body_of` /
/// `video_info`），年龄限制同样转成隐藏详情。给 `YoutubeDetailDualRunTest` 用。
#[uniffi::export]
pub fn youtube_parse_detail(
    player_body: String,
    next_body: Option<String>,
    now_sec: i64,
) -> Result<FfiDetail, FfiError> {
    let player = json::parse_object(&player_body).map_err(|detail| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail,
    })?;
    if detail::is_age_restricted(&player) {
        let video = detail::video(&player);
        return Ok(to_ffi_detail(&detail::hidden_detail(&video.video_id)));
    }
    let video = detail::video(&player);
    let meta = next_body.and_then(|body| {
        json::parse_object(&body)
            .ok()
            .and_then(|map| detail::video_meta(&map, now_sec))
    });
    let video = detail::merge_meta(video, meta.as_ref());
    Ok(to_ffi_detail(&FeedDetail {
        item: detail::item_of(&video, meta.as_ref()),
        body: detail::body_of(&video),
        video: Some(detail::video_info(&video)),
        follow: None,
        related: Vec::new(),
    }))
}

/// **纯函数**：一页评论 / 楼中楼（`next {continuation}` 的 body）。[`parent_id`] 是楼中楼要跳过的父评论。
#[uniffi::export]
pub fn youtube_parse_comments(
    body: String,
    now_sec: i64,
    parent_id: Option<String>,
) -> Result<FfiCommentPage, FfiError> {
    let map = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail,
    })?;
    let (page, _) = detail::comments(&map, now_sec, parent_id.as_deref());
    Ok(to_ffi_comment_page(&page))
}

/// **纯函数**：`next {videoId}` 的响应里评论面板的续页 token（按排序挑，拿不到就是 `None`）。
#[uniffi::export]
pub fn youtube_parse_comments_token(next_body: String, sort: FfiCommentSort) -> Option<String> {
    let map = json::parse_object(&next_body).ok()?;
    detail::comments_token(&map, sort_of(sort))
}
