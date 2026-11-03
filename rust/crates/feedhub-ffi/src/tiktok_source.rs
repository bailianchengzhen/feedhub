//! **TikTok 的平台对象**：推荐流的整个"请求怎么发"（拼 URL / 参数顺序 / 请求头顺序 / `X-Dynosaur` +
//! `X-Gnarly` 签名 / 翻页游标 / 首页设备参数的首次获取 / 状态码与风控信号判定）+ 解析，都在这一个
//! `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读写 store、读 Cookie 罐。见 [`crate::host`]。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiTiktokSource(base, hostHttp, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.feed(cursor, nowMs, key, key2) }
//! val detail = withContext(Dispatchers.IO) { src.detail(id) }                      // 视频页 HTML
//! val comments = withContext(Dispatchers.IO) { src.comments(id, cursor, nowMs, key, key2) }
//! val info = withContext(Dispatchers.IO) { src.play(id, "main", quality) }        // 视频页 hydration
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象里唯一的可变状态是"首页设备数据只取一次"的标记，
//! 可以跨线程用（`Send + Sync`），但没有内部线程 / 定时器。
//!
//! **时间戳与两组签名密钥都由调用方注入**（`now_ms` / `key` / `key2`）：Rust 不读时钟、不取随机数，
//! 测试才能把两次运行的请求逐字节比。`msToken` / `s_v_web_id` 只从 Cookie 罐 / store 里读真值。
//!
//! 另外导出一个**纯函数** [`tiktok_sign`]：和 Kotlin `TiktokSign.sign` 同一套入参 / 出参，
//! 给签名对拍门（定值 + 200 组固定种子随机参数）用，也方便排障时解开一段签名比对。

use std::sync::Arc;

use feedhub_core::tiktok::source::TiktokHostedSource;
use feedhub_core::tiktok::{sign, TiktokError};

use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::play::to_ffi_play;
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_user, FfiCommentPage, FfiDetail,
    FfiError, FfiPage, FfiPlayInfo, FfiUserRef,
};

/// TikTok 平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiTiktokSource {
    inner: TiktokHostedSource,
}

#[uniffi::export]
impl FfiTiktokSource {
    /// [`base`] 是接口根（线上 `https://www.tiktok.com`，测试指向 MockWebServer）。
    ///
    /// 设备标识 / `verifyFp` / `msToken` **不从参数来**：Rust 直接读宿主 store 与 Cookie 罐里那几份
    /// （`tiktok.*` / `multi_sids` / `s_v_web_id` / `msToken`），所以"不伪造身份"是结构上的保证。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: TiktokHostedSource::new(
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
            inner: TiktokHostedSource::new(
                base,
                host,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
        }))
    }

    /// 一页推荐流。[`now_ms`] 是毫秒时间戳，[`key`] / [`key2`] 是两组各 12 个 32 位签名密钥——
    /// 由调用方注入（生产是 `System.currentTimeMillis()` 与 `TiktokRandom`，测试注入固定值）。
    pub fn feed(
        &self,
        cursor: Option<String>,
        now_ms: i64,
        key: Vec<u32>,
        key2: Vec<u32>,
    ) -> Result<FfiPage, FfiError> {
        self.inner
            .feed(cursor.as_deref(), now_ms, &key, &key2)
            .map(|page| to_ffi_page(&page))
            .map_err(map_error)
    }

    /// 登录态 / 账号：`GET /` 的首页 hydration。
    ///
    /// 没登录（`sessionid` / `sid_tt` / `multi_sids` 都没有）时**一个请求都不发**、直接回 `None`；
    /// 有登录就**无条件**打一次首页（Kotlin 的 `whoami` 也不走"取过就跳过"），顺手把设备数据写回 store；
    /// 页面里没有账号就 `None`（不编一个账号）。风控 / 状态码照 `feed` 那一套判定。
    pub fn whoami(&self) -> Result<Option<FfiUserRef>, FfiError> {
        self.inner
            .whoami()
            .map(|user| user.as_ref().map(to_ffi_user))
            .map_err(map_error)
    }

    /// 详情：视频页 HTML 的 hydration（`/@<作者>/video/<id>`，**不签名**）。
    /// id 里没有作者时**在发请求之前**报 `no_author`。
    pub fn detail(&self, id: String) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_error)
    }

    /// 一级评论：`GET /api/comment/list/`。游标就是响应里的 `cursor`（`None` = 第一页）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        now_ms: i64,
        key: Vec<u32>,
        key2: Vec<u32>,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .comments(&id, cursor.as_deref(), now_ms, &key, &key2)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 楼中楼：`GET /api/comment/list/reply/`。id 里没有作者时报 `no_referer`、一个请求都不发。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_ms: i64,
        key: Vec<u32>,
        key2: Vec<u32>,
    ) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies(&id, &comment_id, cursor.as_deref(), now_ms, &key, &key2)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 播放地址：**每次都重新取一次视频页**（`itemStruct.video.playAddr` / `bitrateInfo` 里的地址带
    /// 时效签名），不复用详情那份（和 Kotlin `TiktokSource.play` 一样）。
    ///
    /// 不签名、也不吃时间戳 / 密钥（视频页 HTML 不走 `SignInterceptor`），所以参数只有 id / partId /
    /// 清晰度。id 里没有作者时报 `no_author`、**一个请求都不发**。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
    ) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id, quality)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}

/// **纯函数**：解析一条 itemStruct（详情 fixture 里的作品对象）。
#[uniffi::export]
pub fn tiktok_parse_detail(item_body: String, id: String) -> Result<FfiDetail, FfiError> {
    feedhub_core::tiktok::detail::parse_body(&item_body, &id)
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_error)
}

/// **纯函数**：从视频页 HTML 抠 hydration + 取 itemStruct + 解析。
#[uniffi::export]
pub fn tiktok_parse_video_page(html: String, id: String) -> Result<FfiDetail, FfiError> {
    feedhub_core::tiktok::detail::parse_video_page(&html, &id)
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_error)
}

/// **纯函数**：解析一页评论（`comment/list` 与 `comment/list/reply` 同一个结构）。
#[uniffi::export]
pub fn tiktok_parse_comments(body: String) -> Result<FfiCommentPage, FfiError> {
    feedhub_core::tiktok::detail::comments_body(&body)
        .map(|page| to_ffi_comment_page(&page))
        .map_err(map_error)
}

/// **纯函数**：解析一页推荐流（`item_list` 的响应）。给对拍（Kotlin `TiktokParser.feed`
/// vs Rust）用，生产路径由 [`FfiTiktokSource::feed`] 直接用。
#[uniffi::export]
pub fn tiktok_parse_feed(
    body: String,
    fallback_cursor: Option<String>,
) -> Result<FfiPage, FfiError> {
    feedhub_core::tiktok::parse_feed(&body, fallback_cursor.as_deref())
        .map(|page| to_ffi_page(&page))
        .map_err(map_error)
}

/// 一次签名结果（和 Kotlin `TiktokSign.Signed.params` 的四个参数一一对应）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTiktokSigned {
    /// 要原样发送的查询串（业务参数 → Dynosaur → msToken → Bogus → Gnarly）。
    pub query: String,
    pub dynosaur: String,
    pub ms_token: String,
    /// 固定是 `"1"`。
    pub bogus: String,
    pub gnarly: String,
}

/// **纯函数**：给一个已经编码好的业务查询串签名（和 Kotlin `TiktokSign.sign` 同一套入参）。
///
/// 时间戳 / nonce / 两组密钥全部由调用方给，所以同一组输入两次调用得到**逐字节相同**的签名。
/// 生产路径由 [`FfiTiktokSource::feed`] 直接用，这个导出只给对拍门和排障。
#[allow(clippy::too_many_arguments)]
#[uniffi::export]
pub fn tiktok_sign(
    query: String,
    user_agent: String,
    ms_token: String,
    body: Vec<u8>,
    timestamp: i64,
    nonce: i64,
    nonce2: i64,
    sequence: i32,
    key: Vec<u32>,
    key2: Vec<u32>,
) -> Result<FfiTiktokSigned, FfiError> {
    let nonce = word32(nonce)?;
    let nonce2 = word32(nonce2)?;
    if sequence < 0 {
        return Err(internal(format!("sequence 不能是负数：{sequence}")));
    }
    check_keys(&key, &key2)?;
    let signed = sign::sign(
        &query,
        &user_agent,
        &ms_token,
        &body,
        timestamp,
        nonce,
        nonce2,
        sequence as u32,
        &key,
        &key2,
    );
    Ok(FfiTiktokSigned {
        query: signed.query,
        dynosaur: signed.dynosaur,
        ms_token: signed.ms_token,
        bogus: sign::BOGUS_VALUE.to_string(),
        gnarly: signed.gnarly,
    })
}

fn word32(value: i64) -> Result<u32, FfiError> {
    if (0..=sign::MASK32 as i64).contains(&value) {
        Ok(value as u32)
    } else {
        Err(internal(format!("32 位无符号值越界：{value}")))
    }
}

fn check_keys(key: &[u32], key2: &[u32]) -> Result<(), FfiError> {
    if key.len() != sign::KEY_WORDS || key2.len() != sign::KEY_WORDS {
        return Err(internal(format!(
            "签名要 {} 个密钥字（收到 {} / {}）",
            sign::KEY_WORDS,
            key.len(),
            key2.len()
        )));
    }
    Ok(())
}

fn internal(detail: String) -> FfiError {
    FfiError::Internal {
        platform: feedhub_core::tiktok::PLATFORM.to_string(),
        detail,
    }
}

fn map_error(e: TiktokError) -> FfiError {
    let platform = feedhub_core::tiktok::PLATFORM.to_string();
    match e {
        TiktokError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        TiktokError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        TiktokError::Signature { detail } => FfiError::Signature { platform, detail },
        TiktokError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        TiktokError::Parse { detail } => FfiError::Parse { platform, detail },
        TiktokError::Network { detail } => FfiError::Network { platform, detail },
        TiktokError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        TiktokError::Internal { detail } => FfiError::Internal { platform, detail },
    }
}
