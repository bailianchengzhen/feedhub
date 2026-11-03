//! 贴吧 Source 的**平台层**：推荐流 / 关注流（吧列表 + 每个吧的一页）的请求怎么拼
//! （URL / 查询参数 / 请求头**顺序** / multipart 请求体**逐字节** / 翻页页码）、状态码与错误外壳的判定、
//! 以及响应的解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-tieba/TiebaSource.kt` 的同名方法：
//!
//! | Rust | Kotlin |
//! |---|---|
//! | [`TiebaHostedSource::recommend`] | `recommend` + `TiebaRequests.personalizedBody` + `postProto` |
//! | [`TiebaHostedSource::frs_page`] | `FrsPager.fetch` + `frsPageBytes` + `TiebaRequests.frsPageBody` |
//! | [`TiebaHostedSource::forum_guide`] | `fetchFollowedForums` + `TiebaRequests`（表单） |
//! | [`personalized_body`] / [`frs_page_body`] | `TiebaRequests.personalizedBody` / `frsPageBody`（**protobuf 编码器重写**） |
//! | [`api_headers`] / [`form_headers`] / [`web_headers`] | `TiebaRequests.headers` / `formHeaders` / 网页客户端那几条 |
//! | [`TiebaHostedSource::detail`] | `detail` + `firstPage`（`pb/page` 第一页、正序、只要一楼、不带楼中楼） |
//! | [`TiebaHostedSource::comments`] | `comments`（`pb/page` 按页；`asc` / `desc` / `hot`；每楼带 4 条楼中楼预览） |
//! | [`TiebaHostedSource::replies`] | `replies`（`pb/floor`，`commentId` 是楼层的 pid，版本 [V22]） |
//! | [`TiebaHostedSource::main_video`] | `play(partId=main)` 里"取 / 复用第一页"的那一半（`recentPages`） |
//! | [`pb_page_body`] / [`pb_floor_body`] | `TiebaRequests.pbPageBody` / `pbFloorBody`（同样逐字节照抄写序） |
//!
//! URL / 头顺序 /
//! multipart 请求体 / 页码 / 状态码与错误外壳判定都在这里，平台错误码仍只带回去不归类。
//! **`play` 的其余部分留在 Kotlin**：`partId=url:<mp4>` 直接用楼层里的地址，不请求；
//! `partId=main` 的地址从 [`TiebaHostedSource::main_video`] 拿（详情取过的第一页 5 分钟内复用，
//! 就是 Kotlin 那个 `recentPages`），`PlayInfo` / 清晰度映射仍是 `TiebaVideo.play`。
//!
//! 有几件事**故意留在 Kotlin**：
//! 关注流的**调度**（`ForumMerger` / `ForumFreshness` / `ForumStore`）。它们有状态、读写 `ctx.store`、
//! 缓存格式（gzip + kotlinx JSON）和 Kotlin 版共用；当前只把"每个请求怎么发 + 怎么解析"搬过来
//! （和小红书关注流的处理一样）。
//!
//! # 为什么 protobuf 要自己**编码**
//!
//! 贴吧 App 接口的请求体是 protobuf，Kotlin 那边是 Square Wire 生成的 `encode()`。要"body 逐字节相同"，
//! 光字段值对不够：**字段的写出顺序**是 Wire 按 `.proto` 里的**声明顺序**来的（不是 tag 升序！例如
//! `AppPosInfo` 是 6→2→1→7→3），而且 proto3 的 `optional` 字段**有值就写**（哪怕是 `""` / `0`），
//! 非 `optional` 的字段等于默认值就**不写**。这两条在这里逐字段照抄，见 [`Pb`] 和
//! [`common_request`] / [`personalized_data`] / [`frs_page_data`]。
//! `TiebaWireEquivalenceTest` 拿两边真实发出去的**原始字节**逐字节比，守的就是这一条。

use std::sync::{Arc, Mutex};

use crate::detail::{self as tieba_target, CommentPage, FeedDetail};
use crate::host::{CookieHost, HttpHost, HttpRequest, StoreHost};
use crate::hosted::civil_from_days;
use crate::json::{self, JsonMap};
use crate::model::{BiliError, UserRef};
use crate::tieba::device::{self, Device};
use crate::tieba::{self, detail, PlatformError};

/// App 接口主机（和 `TiebaSource.APP` 一样）。测试里换成 MockWebServer / 裸 socket 的地址。
pub const APP: &str = "https://tiebac.baidu.com";
/// 网页接口主机。
pub const WEB: &str = "https://tieba.baidu.com";
/// Cookie 罐里贴吧的"身份 URL"（Kotlin `COOKIE_URL`，登录凭据按 `tieba.baidu.com` 取）。
const COOKIE_URL: &str = "https://tieba.baidu.com/";

/// 推荐流报的版本号（TiebaLite 的 12.52.1.0）。
pub const V12: &str = "12.52.1.0";
/// 吧内帖子列表报的版本号（aiotieba `get_threads` 的 `LEGACY_VERSION`）：报 V12 时服务端只给前十几条详情，
/// 报这个才整页都是详情。
pub const V12_FRS: &str = "12.64.1.1";

pub const CMD_PERSONALIZED: i32 = 309_264;
pub const CMD_FRS_PAGE: i32 = 301_001;
pub const CMD_PB_PAGE: i32 = 302_001;
pub const CMD_PB_FLOOR: i32 = 302_002;
pub const PATH_PERSONALIZED: &str = "/c/f/excellent/personalized";
pub const PATH_FRS_PAGE: &str = "/c/f/frs/page";
pub const PATH_FORUM_GUIDE: &str = "/c/f/forum/forumGuide";
pub const PATH_PB_PAGE: &str = "/c/f/pb/page";
pub const PATH_PB_FLOOR: &str = "/c/f/pb/floor";

/// 关注 / 取关的 App 表单接口（Kotlin `TiebaRequests.PATH_FORUM_LIKE` / `PATH_FORUM_UNLIKE`）。
pub const PATH_FORUM_LIKE: &str = "/c/c/forum/like";
pub const PATH_FORUM_UNLIKE: &str = "/c/c/forum/unfavolike";

/// 楼中楼 `pb/floor` 报的版本号（TiebaLite 的 `TIEBA_V22`）。
pub const V22: &str = "22.10.1.0";

/// `pb/page` 的排序 `r`（aiotieba `get_posts` 的 `sort`）：0 楼层正序，1 倒序，2 热门。
pub const PB_SORT_ASC: i32 = 0;
pub const PB_SORT_DESC: i32 = 1;
pub const PB_SORT_HOT: i32 = 2;

/// 详情第一页的楼数（只要一楼，多拿也无妨；最少 2，aiotieba `rn if rn > 1 else 2`）。
pub const DETAIL_RN: i32 = 2;
/// 楼层每页楼数、每楼带几条楼中楼预览（TiebaLite `floor_rn = 4`）。
pub const COMMENTS_RN: i32 = 30;
pub const PREVIEW_REPLIES: i32 = 4;

/// 详情取过的第一页给播放复用多久（和小红书 / Kotlin 的 `PB_REUSE_MS` 一样，5 分钟）。
pub const PB_REUSE_MS: i64 = 5 * 60_000;
/// 记住最近几条帖子（够覆盖"连着点开几条"）。
pub const RECENT_PAGES: usize = 8;

/// 楼层排序（和 Kotlin 的 `CommentSort` 一一对应，映射在 [`TiebaHostedSource::comments`] 里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSort {
    /// 楼层正序（`r = 0`，**不写进请求体**：proto3 的默认值不序列化）。
    Default,
    Hot,
    Newest,
}

/// multipart 的分隔符（TiebaLite `ProtobufRequest.BOUNDARY`）。
pub const BOUNDARY: &str = "--------7da3d81520810*";
const MULTIPART_TYPE: &str = "multipart/form-data; boundary=--------7da3d81520810*";

/// `frs/page` 每页位置数（`rn`），aiotieba 默认也是 30。
pub const FRS_PAGE_SIZE: i32 = 30;
/// `rn_need`（服务端至少给这么多）：Kotlin 是 `pageSize + 5`。
pub const FRS_PAGE_NEED: i32 = FRS_PAGE_SIZE + 5;
/// `frs/page` 的 `sort_type`：1 = 按发帖时间（aiotieba `ThreadSortType.CREATE`）。
pub const SORT_BY_CREATE: i32 = 1;

/// 网页接口的默认头（和 `core.UserAgents.DESKTOP_CHROME` 一字不差）。
///
/// 为什么 Rust 里也写一份：Kotlin 那边是 `DefaultHeadersInterceptor` 补的，它带 `hostFilter`
/// 。**请求头的顺序**因此和
/// "谁来补"有关：拦截器补的会落在已有头后面。这里照同样的规则写，位置也放在 `Subapp-Type` 之后、
/// `Content-Type` 之前（= 拦截器在 Kotlin 版里插入的位置），两边才逐字节相同；漂移会被
/// `TiebaWireEquivalenceTest` 抓住。
pub const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
pub const WEB_REFERER: &str = "https://tieba.baidu.com/";

/// App 的 UA：TiebaLite 是 WebView 默认 UA + ` tieba/版本`。
pub fn user_agent(version: &str) -> String {
    format!(
        "Mozilla/5.0 (Linux; Android 14; {} Build/UKQ1.230804.001; wv) AppleWebKit/537.36 (KHTML, like Gecko) \
         Version/4.0 Chrome/130.0.6723.103 Mobile Safari/537.36 tieba/{version}",
        device::MODEL
    )
}

// ===========================================================================
// 设备 + 登录凭据（都只读宿主）
// ===========================================================================

/// 登录凭据。只在内存里用，**绝不打印**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub bduss: String,
    pub stoken: Option<String>,
}

/// 贴吧的平台对象：持有宿主端口 + 一个设备标识缓存。
///
/// **两个 HTTP 端口**（和 Kotlin 的两个客户端一一对应，不能合成一个）：
/// - `http_app`：App 接口（`tiebac.baidu.com`）。Kotlin 那边是 `CookieJar.NO_COOKIES` + 没有默认头，
///   登录凭据放在 protobuf 字段里；
/// - `http_web`：网页接口（`tieba.baidu.com`）。Kotlin 那边是只读 Cookie 罐 + 桌面 UA / Referer，
///   登录凭据靠 Cookie。
///
/// 对象自己的状态只有一个设备标识缓存（`ctx.store` 里那三个键的派生值，装一次就不变）和一把锁；
/// 没有后台线程、没有定时器、没有连接池，所以闲置时零唤醒。
pub struct TiebaHostedSource {
    app_base: String,
    web_base: String,
    // 两个 HTTP 端口对同模块的 `tieba/search.rs` 也要可见（搜索走的是同一对客户端：
    // `tiebac.baidu.com` 的 App 表单 / `tieba.baidu.com` 的网页 GET）。
    pub(crate) http_app: Arc<dyn HttpHost>,
    pub(crate) http_web: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    device: Mutex<Option<Device>>,
    /// 详情取过的第一页（只留播放要的主视频）——Kotlin 的 `recentPages`：访问序 LRU、[RECENT_PAGES] 条、
    /// [PB_REUSE_MS] 内有效。`play(partId=main)` 点开刚看过的帖子时不再发一次 `pb/page`。
    recent: Mutex<Vec<RecentPage>>,
}

/// [`TiebaHostedSource::recent`] 的一项：哪条帖子、什么时候取的、一楼的主视频（没有就是 `None`）。
#[derive(Debug, Clone)]
struct RecentPage {
    tid: i64,
    at_ms: i64,
    video: Option<tieba::content::MainVideo>,
}

impl TiebaHostedSource {
    pub fn new(
        app_base: impl Into<String>,
        web_base: impl Into<String>,
        http_app: Arc<dyn HttpHost>,
        http_web: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            app_base: app_base.into(),
            web_base: web_base.into(),
            http_app,
            http_web,
            store,
            cookies,
            device: Mutex::new(None),
            recent: Mutex::new(Vec::new()),
        }
    }

    pub fn app_base(&self) -> &str {
        &self.app_base
    }

    /// 宿主那个 store 的句柄（关注流的存盘由 [`crate::tieba::following`] 自己拿它读写）。
    pub fn store_handle(&self) -> Arc<dyn StoreHost> {
        self.store.clone()
    }

    pub fn web_base(&self) -> &str {
        &self.web_base
    }

    /// 推荐流一页。`now_ms` 由调用方注入（请求体里的 `_timestamp` / `event_day`、`client_logid` 都用它），
    /// 测试才能把两次运行的请求逐字节比。游标是页码，下一页永远是 `pn + 1`。
    pub fn recommend(&self, cursor: Option<&str>, now_ms: i64) -> Result<tieba::Feed, BiliError> {
        let pn = cursor
            .and_then(|c| c.parse::<i32>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(1);
        let device = self.device()?;
        let cred = self.credentials();
        let body = personalized_body(&device, cred.as_ref(), pn, now_ms);
        let ua = user_agent(V12);
        let outcome = self.post_proto(
            &self.http_app,
            &self.app_base,
            PATH_PERSONALIZED,
            CMD_PERSONALIZED,
            &body,
            &ua,
            &device,
            now_ms,
            &[],
        )?;
        match outcome {
            ProtoOutcome::Bytes(bytes) => tieba::parse_personalized(&bytes, pn),
            // 服务端出错时偶尔直接回 JSON 而不是 protobuf：错误码当数据带回 Kotlin（归类规则只有一份）
            ProtoOutcome::PlatformError(error) => Ok(tieba::Feed {
                items: Vec::new(),
                next: None,
                last: false,
                error: Some(error),
            }),
        }
    }

    /// 关注流里某个吧的一页（`/c/f/frs/page`，**匿名**请求：吧里的帖子是公开的，`cred` 传 null，
    /// 和 Kotlin `frsPageBytes` 一样）。
    pub fn frs_page(
        &self,
        forum_name: &str,
        page: i32,
        now_ms: i64,
    ) -> Result<tieba::Feed, BiliError> {
        let device = self.device()?;
        let body = frs_page_body(&device, forum_name, page, now_ms);
        let ua = user_agent(V12_FRS);
        let forum_header = form_encode(forum_name);
        let outcome = self.post_proto(
            &self.http_app,
            &self.app_base,
            PATH_FRS_PAGE,
            CMD_FRS_PAGE,
            &body,
            &ua,
            &device,
            now_ms,
            &[("forum_name", forum_header.as_str())],
        )?;
        match outcome {
            ProtoOutcome::Bytes(bytes) => tieba::parse_frs_page(&bytes, forum_name),
            ProtoOutcome::PlatformError(error) => Ok(tieba::Feed {
                items: Vec::new(),
                next: None,
                last: false,
                error: Some(error),
            }),
        }
    }

    /// 关注的吧（网页接口 `POST /c/f/forum/forumGuide`，表单 `sort_type=3&call_from=3`，带 Cookie）。
    ///
    /// `error_code` 非 0 时**不抛**，当数据带回去（Kotlin 的 `TiebaSource.mapError` 是唯一归类处）；
    /// `is_login` 原样带回去（Kotlin 判 `== 0` → `NotLoggedInException`）。
    pub fn forum_guide(&self) -> Result<HostedForums, BiliError> {
        let url = format!("{}{}", self.web_base, PATH_FORUM_GUIDE);
        let body = form_body(&[("sort_type", "3"), ("call_from", "3")]);
        let response = self
            .http_web
            .execute(HttpRequest {
                method: "POST".to_string(),
                url: url.clone(),
                headers: web_headers(&url),
                body,
            })
            .map_err(BiliError::from)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: "forumGuide".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|_| BiliError::Parse {
            detail: "forumGuide 响应不是 JSON".to_string(),
        })?;
        let code = json::long(&root, "error_code").unwrap_or(0);
        let is_login = json::long(&root, "is_login");
        if code != 0 {
            return Ok(HostedForums {
                forums: Vec::new(),
                is_login,
                error: Some(PlatformError {
                    code: code as i32,
                    user_msg: String::new(),
                    error_msg: json::text(&root, "error_msg").unwrap_or_default(),
                }),
            });
        }
        Ok(HostedForums {
            forums: tieba::followed_forums_object(&root)?,
            is_login,
            error: None,
        })
    }

    // 详情 / 楼层 / 楼中楼

    /// 帖子详情（`pb/page` 第一页、正序、[DETAIL_RN] 楼、不带楼中楼）。
    ///
    /// `id` 就是 tid（和信息流的 `FeedItem.id` 一样）。平台错误码当数据带回去（归类只有 Kotlin 的 `mapError`），
    /// 解析不了才是错误。没有错误时把一楼的主视频记进 [`recent`](Self::recent)（给 `play(partId=main)` 复用）。
    pub fn detail(
        &self,
        id: &str,
        now_ms: i64,
    ) -> Result<detail::Parsed<FeedDetail>, BiliError> {
        let tid = parse_tid(id)?;
        let logged_in = self.credentials().is_some();
        match self.post_page(tid, 1, DETAIL_RN, PB_SORT_ASC, 0, now_ms)? {
            ProtoOutcome::PlatformError(error) => Ok(detail::Parsed::failed(error)),
            ProtoOutcome::Bytes(bytes) => {
                let res = detail::decode_page(&bytes)?;
                if let Some(error) = detail::page_error(&res) {
                    return Ok(detail::Parsed::failed(error));
                }
                let value = detail::detail_of(&res, id, logged_in)?;
                self.remember(tid, now_ms, detail::page_main_video(&res));
                Ok(detail::Parsed::ok(value))
            }
        }
    }

    /// 一页楼层（二楼起）。游标是页码；`Default` 正序、`Newest` 倒序、`Hot` 热门。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_ms: i64,
    ) -> Result<detail::Parsed<CommentPage>, BiliError> {
        let tid = parse_tid(id)?;
        let pn = page_of(cursor)?;
        let r = match sort {
            CommentSort::Default => PB_SORT_ASC,
            CommentSort::Newest => PB_SORT_DESC,
            CommentSort::Hot => PB_SORT_HOT,
        };
        match self.post_page(tid, pn, COMMENTS_RN, r, PREVIEW_REPLIES, now_ms)? {
            ProtoOutcome::PlatformError(error) => Ok(detail::Parsed::failed(error)),
            ProtoOutcome::Bytes(bytes) => {
                let res = detail::decode_page(&bytes)?;
                if let Some(error) = detail::page_error(&res) {
                    return Ok(detail::Parsed::failed(error));
                }
                detail::comments_of(&res, pn).map(detail::Parsed::ok)
            }
        }
    }

    /// 某一楼的楼中楼（`pb/floor`）。`comment_id` 是楼层的 pid，游标是页码。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
    ) -> Result<detail::Parsed<CommentPage>, BiliError> {
        let tid = parse_tid(id)?;
        let pid = comment_id
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| tieba::parse_failure(format!("楼层 id 不对：{comment_id}")))?;
        let pn = page_of(cursor)?;
        let device = self.device()?;
        let cred = self.credentials();
        let body = pb_floor_body(&device, cred.as_ref(), tid, pid, pn, now_ms);
        let ua = user_agent(V22);
        let outcome = self.post_proto(
            &self.http_app,
            &self.app_base,
            PATH_PB_FLOOR,
            CMD_PB_FLOOR,
            &body,
            &ua,
            &device,
            now_ms,
            &[],
        )?;
        match outcome {
            ProtoOutcome::PlatformError(error) => Ok(detail::Parsed::failed(error)),
            ProtoOutcome::Bytes(bytes) => {
                let res = detail::decode_floor(&bytes)?;
                if let Some(error) = detail::floor_error(&res) {
                    return Ok(detail::Parsed::failed(error));
                }
                detail::replies_of(&res, pn).map(detail::Parsed::ok)
            }
        }
    }

    /// 帖子的主视频（`play(partId=main)` 的地址）：[PB_REUSE_MS] 内 [`detail`](Self::detail) 取过就直接用，
    /// 没有才现取一次第一页。没有主视频时 `video = None`（Kotlin 那边翻成"一楼里没有视频"）。
    pub fn main_video(
        &self,
        id: &str,
        now_ms: i64,
    ) -> Result<detail::MainVideoOutcome, BiliError> {
        let tid = parse_tid(id)?;
        if let Some(video) = self.recent_video(tid, now_ms) {
            return Ok(detail::MainVideoOutcome { video, error: None });
        }
        match self.post_page(tid, 1, DETAIL_RN, PB_SORT_ASC, 0, now_ms)? {
            ProtoOutcome::PlatformError(error) => Ok(detail::MainVideoOutcome {
                video: None,
                error: Some(error),
            }),
            ProtoOutcome::Bytes(bytes) => {
                let res = detail::decode_page(&bytes)?;
                if let Some(error) = detail::page_error(&res) {
                    return Ok(detail::MainVideoOutcome {
                        video: None,
                        error: Some(error),
                    });
                }
                let video = detail::page_main_video(&res);
                self.remember(tid, now_ms, video.clone());
                Ok(detail::MainVideoOutcome { video, error: None })
            }
        }
    }

    // 账号：我是谁

    /// `whoami`：网页接口 `GET /mo/q/newmoindex?need_user=1`（带 Cookie，只读）。
    /// 没有 `BDUSS` 就**一个请求都不发**；`no` 非 0 时按 Kotlin `isNotLoggedIn` / `mapError` 归类。
    pub fn whoami(&self) -> Result<Option<UserRef>, BiliError> {
        if self.credentials().is_none() {
            return Ok(None);
        }
        let url = format!("{}/mo/q/newmoindex?need_user=1", self.web_base);
        let response = self
            .http_web
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: url.clone(),
                headers: web_get_headers(&url),
                body: Vec::new(),
            })
            .map_err(BiliError::from)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: "newmoindex".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        // 不把 body 带进异常信息：这个接口返回的是账号资料
        let root = json::parse_object(&text).map_err(|_| BiliError::Parse {
            detail: "newmoindex 响应不是 JSON".to_string(),
        })?;
        let code = json::long(&root, "no").ok_or_else(|| BiliError::Parse {
            detail: "newmoindex 缺少 no".to_string(),
        })?;
        if code != 0 {
            let msg = json::text(&root, "error").unwrap_or_default();
            if is_not_logged_in(&msg) {
                return Ok(None);
            }
            return Err(classify_error(code as i32, &msg));
        }
        Ok(json::obj(&root, "data").and_then(self_user))
    }

    /// `SET / GET /dc/common/tbs`：`{"tbs":"…","is_login":1}`。
    pub fn fetch_tbs(&self) -> Result<String, BiliError> {
        let url = format!("{}/dc/common/tbs", self.web_base);
        let response = self
            .http_web
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: url.clone(),
                headers: web_get_headers(&url),
                body: Vec::new(),
            })
            .map_err(BiliError::from)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: "dc/common/tbs".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|_| BiliError::Parse {
            detail: "dc/common/tbs 响应不是 JSON".to_string(),
        })?;
        if json::long(&root, "is_login") == Some(0) {
            return Err(BiliError::NotLoggedIn { detail: None });
        }
        json::text(&root, "tbs").ok_or_else(|| BiliError::Parse {
            detail: "dc/common/tbs 缺少 tbs".to_string(),
        })
    }

    /// 关注（`POST /c/c/forum/like`）/ 取关（`/c/c/forum/unfavolike`）一个吧。
    /// **写操作，只在用户点按钮时调用，不重试**（宿主那边 `writer` 客户端还把"连接失败换连接重发"关了）。
    ///
    /// `forum_name` 由调用方给（关注列表里的吧名比 target 里的可靠）；写请求只发一次。
    /// 已经关注 / 未关注的报错按 Kotlin 的 `alreadyInState` 当成功。
    pub fn set_following(
        &self,
        target: &tieba_target::FollowTarget,
        follow: bool,
        forum_name: Option<&str>,
        now_ms: i64,
    ) -> Result<(), BiliError> {
        if target.platform != "tieba" || target.kind != tieba_target::FollowKind::Forum {
            return Err(BiliError::Api {
                code: "-".to_string(),
                detail: "贴吧只能关注 / 取关吧".to_string(),
            });
        }
        let fid: i64 = target.id.parse().ok().filter(|id| *id > 0).ok_or_else(|| {
            BiliError::Parse {
                detail: format!("吧 id 不对：{}", target.id),
            }
        })?;
        let cred = self.credentials().ok_or(BiliError::NotLoggedIn { detail: None })?;
        let name = forum_name
            .or(Some(target.name.as_str()))
            .filter(|n| !tieba::is_blank(n))
            .ok_or_else(|| BiliError::Parse {
                detail: "缺少吧名".to_string(),
            })?
            .to_string();
        let tbs = self.fetch_tbs()?;
        let device = self.device()?;
        let form = tieba::sign::signed(
            &[
                ("BDUSS".to_string(), cred.bduss.clone()),
                ("_client_id".to_string(), device.client_id.clone()),
                ("_client_type".to_string(), "2".to_string()),
                ("_client_version".to_string(), V12.to_string()),
                ("fid".to_string(), fid.to_string()),
                ("kw".to_string(), name),
                ("tbs".to_string(), tbs),
            ],
            tieba::sign::APP_SALT,
        );
        let path = if follow {
            PATH_FORUM_LIKE
        } else {
            PATH_FORUM_UNLIKE
        };
        let body = form_body(&form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>());
        // 表单的 Content-Type 在 Kotlin 那边由 `FormBody` 经 BridgeInterceptor 补在**最后**，
        // 宿主接口只有 body 字节，所以这里自己写在表单头之后（位置一样）。
        let mut headers = form_headers(&device, now_ms);
        headers.push((
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ));
        let response = self
            .http_app
            .execute(HttpRequest {
                method: "POST".to_string(),
                url: format!("{}{path}", self.app_base),
                headers,
                body,
            })
            .map_err(BiliError::from)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: path.to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|_| BiliError::Parse {
            detail: format!("{path} 响应不是 JSON"),
        })?;
        if let Some((code, msg)) = write_failure(&root) {
            if !already_in_state(&msg, follow) {
                return Err(classify_error(code, &msg));
            }
        }
        Ok(())
    }

    /// 播放地址（Kotlin `TiebaSource.play`）：
    /// - `partId = main`：一楼的第一个视频，`pb/page` 第一页（[PB_REUSE_MS] 内 [`detail`](Self::detail) 取过
    ///   就直接用那份）；一楼没有视频时 `video = None`，Kotlin 那边翻成"一楼里没有视频"；
    /// - `partId = url:<mp4>`：**地址就在参数里**，一个请求都不发（楼层里的视频就是这样）。
    ///
    /// 参数不对（`main2` / `url:` / 不是 http 的地址）在**发请求之前**就报错。
    /// 平台错误（`error_code` 非 0）原样带回去，由 Kotlin 的 [`TiebaSource.mapError`] 归类。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        now_ms: i64,
    ) -> Result<detail::PlayOutcome, BiliError> {
        super::play::check_part(part_id)?;
        if part_id != super::play::MAIN {
            // `url:<mp4>`：地址就在参数里，不再请求
            let info = super::play::play_inline(part_id)?;
            return Ok(detail::PlayOutcome {
                info: Some(info),
                error: None,
            });
        }
        let outcome = self.main_video(id, now_ms)?;
        if let Some(error) = outcome.error {
            return Ok(detail::PlayOutcome {
                info: None,
                error: Some(error),
            });
        }
        match outcome.video {
            Some(video) => Ok(detail::PlayOutcome {
                info: Some(super::play::play(&video)),
                error: None,
            }),
            // 一楼没有视频：`info = None`、没有平台错误（Kotlin 那边报 ParseException）
            None => Ok(detail::PlayOutcome {
                info: None,
                error: None,
            }),
        }
    }

    /// `pb/page` 的一次请求（详情 / 楼层共用）。
    fn post_page(
        &self,
        tid: i64,
        pn: i32,
        rn: i32,
        sort: i32,
        floor_rn: i32,
        now_ms: i64,
    ) -> Result<ProtoOutcome, BiliError> {
        let device = self.device()?;
        let cred = self.credentials();
        let body = pb_page_body(&device, cred.as_ref(), tid, pn, rn, sort, floor_rn, now_ms);
        let ua = user_agent(V12);
        self.post_proto(
            &self.http_app,
            &self.app_base,
            PATH_PB_PAGE,
            CMD_PB_PAGE,
            &body,
            &ua,
            &device,
            now_ms,
            &[],
        )
    }

    fn remember(&self, tid: i64, now_ms: i64, video: Option<tieba::content::MainVideo>) {
        let mut cache = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = cache.iter().position(|r| r.tid == tid) {
            cache.remove(index);
        }
        cache.push(RecentPage {
            tid,
            at_ms: now_ms,
            video,
        });
        while cache.len() > RECENT_PAGES {
            cache.remove(0);
        }
    }

    /// 最近取过的帖子第一页里的一楼视频（访问序，和 Kotlin 那个 `LinkedHashMap(accessOrder = true)` 一样：
    /// 读过就挪到最后，过期的不删、只是不返回——下次 `detail` 会覆盖它）。
    fn recent_video(&self, tid: i64, now_ms: i64) -> Option<Option<tieba::content::MainVideo>> {
        let mut cache = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let index = cache.iter().position(|r| r.tid == tid)?;
        let entry = cache.remove(index);
        let fresh = now_ms - entry.at_ms <= PB_REUSE_MS;
        let video = entry.video.clone();
        cache.push(entry);
        fresh.then_some(video)
    }

    pub(crate) fn credentials(&self) -> Option<Credentials> {
        let bduss = self
            .cookies
            .find(COOKIE_URL, "BDUSS")
            .filter(|v| !tieba::is_blank(v))?;
        Some(Credentials {
            bduss,
            stoken: self
                .cookies
                .find(COOKIE_URL, "STOKEN")
                .filter(|v| !tieba::is_blank(v)),
        })
    }

    /// 设备标识：三个原件由宿主写进 `ctx.store`，这里只读 + 派生（不取随机数、不造身份）。
    pub(crate) fn device(&self) -> Result<Device, BiliError> {
        let mut guard = self.device.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(device) = guard.as_ref() {
            return Ok(device.clone());
        }
        let read = |key: &str| {
            self.store
                .get(key)
                .filter(|v| !tieba::is_blank(v))
                .ok_or_else(|| BiliError::Host {
                    detail: format!("设备标识缺失（ctx.store 里没有 {key}）"),
                })
        };
        let device = Device::new(
            read(device::KEY_ANDROID_ID)?,
            read(device::KEY_UUID)?,
            read(device::KEY_CLIENT_ID)?,
        );
        *guard = Some(device.clone());
        Ok(device)
    }

    fn post_proto(
        &self,
        http: &Arc<dyn HttpHost>,
        base: &str,
        path: &str,
        cmd: i32,
        body: &[u8],
        ua: &str,
        device: &Device,
        now_ms: i64,
        extra_headers: &[(&str, &str)],
    ) -> Result<ProtoOutcome, BiliError> {
        let url = format!("{base}{path}?cmd={cmd}");
        let mut headers = api_headers(device, ua, now_ms);
        for (name, value) in extra_headers {
            headers.push(((*name).to_string(), (*value).to_string()));
        }
        headers.push(("Content-Type".to_string(), MULTIPART_TYPE.to_string()));
        let response = http
            .execute(HttpRequest {
                method: "POST".to_string(),
                url,
                headers,
                body: body.to_vec(),
            })
            .map_err(BiliError::from)?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: path.to_string(),
            });
        }
        if response.body.is_empty() {
            return Err(BiliError::Parse {
                detail: format!("{path} 返回空响应"),
            });
        }
        // 出错时服务端偶尔直接回 JSON 而不是 protobuf（Kotlin `postProto` 的那三行）
        if response.body[0] == b'{' {
            let text = String::from_utf8_lossy(&response.body).into_owned();
            let root = json::parse_object(&text).map_err(|detail| BiliError::Parse { detail })?;
            let code = json::long(&root, "error_code")
                .or_else(|| json::long(&root, "errno"))
                .or_else(|| json::long(&root, "no"))
                .map(|c| c as i32)
                .unwrap_or(-1);
            let message = json::text(&root, "error_msg")
                .or_else(|| json::text(&root, "errmsg"))
                .or_else(|| json::text(&root, "error"))
                .unwrap_or_default();
            return Ok(ProtoOutcome::PlatformError(PlatformError {
                code,
                user_msg: String::new(),
                error_msg: message,
            }));
        }
        Ok(ProtoOutcome::Bytes(response.body))
    }
}

/// `post_proto` 的两种"成功"：拿到 protobuf 字节，或者拿到一个平台错误（JSON 外壳）。
enum ProtoOutcome {
    Bytes(Vec<u8>),
    PlatformError(PlatformError),
}

/// 关注的吧 + 登录状态（`error` 非空时不抛，交给 Kotlin 按 `mapError` 归类）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostedForums {
    pub forums: Vec<tieba::FollowedForum>,
    pub is_login: Option<i64>,
    pub error: Option<PlatformError>,
}

// ===========================================================================
// 请求头（顺序就是线上的顺序）
// ===========================================================================

/// App 接口的请求头，顺序照 `TiebaRequests.headers`（`Headers.Builder` 的插入顺序）。
pub fn api_headers(device: &Device, user_agent: &str, log_id: i64) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), user_agent.to_string()),
        ("Charset".to_string(), "UTF-8".to_string()),
        ("client_type".to_string(), "2".to_string()),
        ("client_logid".to_string(), log_id.to_string()),
        (
            "Cookie".to_string(),
            format!(
                "ka=open; CUID={}; TBBRAND={}",
                device.cuid_galaxy2,
                device::MODEL
            ),
        ),
        ("cuid".to_string(), device.cuid_galaxy2.clone()),
        ("cuid_galaxy2".to_string(), device.cuid_galaxy2.clone()),
        ("cuid_gid".to_string(), String::new()),
        ("c3_aid".to_string(), device.c3_aid.clone()),
        ("x_bd_data_type".to_string(), "protobuf".to_string()),
    ]
}

/// App 表单接口（关注 / 取关）的请求头：和 [`api_headers`] 一样，只是没有 `x_bd_data_type`。
pub fn form_headers(device: &Device, log_id: i64) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), format!("bdtb for Android {V12}")),
        ("Charset".to_string(), "UTF-8".to_string()),
        ("client_type".to_string(), "2".to_string()),
        ("client_logid".to_string(), log_id.to_string()),
        (
            "Cookie".to_string(),
            format!(
                "ka=open; CUID={}; TBBRAND={}",
                device.cuid_galaxy2,
                device::MODEL
            ),
        ),
        ("cuid".to_string(), device.cuid_galaxy2.clone()),
        ("cuid_galaxy2".to_string(), device.cuid_galaxy2.clone()),
        ("cuid_gid".to_string(), String::new()),
        ("c3_aid".to_string(), device.c3_aid.clone()),
    ]
}

/// 网页接口的请求头：`Subapp-Type: hybrid` + （百度域名才有的）UA / Referer + Content-Type。
///
/// UA / Referer 的条件和 Kotlin `DefaultHeadersInterceptor(hostFilter = 百度域名)` 一致，位置也照着
/// （拦截器在 `Subapp-Type` 之后插入），这样两边发出去的头**顺序**一样。
pub fn web_headers(url: &str) -> Vec<(String, String)> {
    let mut headers = vec![("Subapp-Type".to_string(), "hybrid".to_string())];
    if is_baidu_host(&url_host(url)) {
        headers.push(("User-Agent".to_string(), DESKTOP_UA.to_string()));
        headers.push(("Referer".to_string(), WEB_REFERER.to_string()));
    }
    headers.push((
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    ));
    headers
}

/// 网页 GET 的请求头：Kotlin `web` 客户端的 `DefaultHeadersInterceptor`（百度域名才有的 UA / Referer）。
///
/// 和 [`web_headers`] 的区别：那是**表单 POST**（多 `Subapp-Type` 和 `Content-Type`）。GET 没有 body，
/// 拦截器只补 `User-Agent` / `Referer` 两条（顺序照 map 插入顺序）。
pub fn web_get_headers(url: &str) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if is_baidu_host(&url_host(url)) {
        headers.push(("User-Agent".to_string(), DESKTOP_UA.to_string()));
        headers.push(("Referer".to_string(), WEB_REFERER.to_string()));
    }
    headers
}

/// `newmoindex` 的 `data` → 账号（Kotlin `TiebaParser.selfUser`）：
/// `id` 是 0 / 没有就给 `None`；名字依次退到 `name_show` / `show_nickname` / `name` / portrait / id。
pub fn self_user(data: &JsonMap) -> Option<UserRef> {
    let id = json::long(data, "id").filter(|id| *id != 0)?.to_string();
    let portrait = json::text(data, "portrait").map(|p| p.split('?').next().unwrap_or("").to_string());
    let name = json::text(data, "name_show")
        .or_else(|| json::text(data, "show_nickname"))
        .or_else(|| json::text(data, "name"))
        .or_else(|| portrait.clone())
        .unwrap_or_else(|| id.clone());
    Some(UserRef {
        platform: "tieba".to_string(),
        id,
        name,
        avatar: portrait.as_ref().map(|p| format!("https://himg.bdimg.com/sys/portrait/item/{p}")),
        url: portrait
            .as_ref()
            .map(|p| format!("https://tieba.baidu.com/home/main?id={p}")),
    })
}

/// 写操作（关注 / 取关）的失败：(code, msg)；`error_code` 和嵌套的 `error.errno` 都认（Kotlin `writeFailure`）。
pub fn write_failure(root: &JsonMap) -> Option<(i32, String)> {
    let code = json::long(root, "error_code").unwrap_or(0);
    let nested = json::obj(root, "error");
    let errno = nested.and_then(|e| json::long(e, "errno")).unwrap_or(0);
    if code == 0 && errno == 0 {
        return None;
    }
    let msg = if code != 0 {
        json::text(root, "error_msg")
    } else {
        nested
            .and_then(|e| json::text(e, "usermsg"))
            .or_else(|| nested.and_then(|e| json::text(e, "errmsg")))
    };
    Some((
        if code != 0 { code } else { errno } as i32,
        msg.unwrap_or_default(),
    ))
}

/// 消息明确说"已经关注 / 没有关注"：关注时说已关注、取关时说没关注，都算已经是目标状态（Kotlin `alreadyInState`）。
pub fn already_in_state(msg: &str, follow: bool) -> bool {
    // Kotlin 用的是两个正则：`已经?(关注|喜欢)` / `(未|没有?|还没)(有)?(关注|喜欢)`
    if follow {
        contains_any(msg, &["已关注", "已经关注", "已喜欢", "已经喜欢"])
    } else {
        contains_any(
            msg,
            &[
                "未关注", "没有关注", "没关注", "还没关注", "未喜欢", "没有喜欢", "没喜欢", "还没喜欢",
            ],
        )
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

/// 贴吧的错误归类（Kotlin `TiebaSource.mapError`）：未登录 / 风控词 / 签名 / 其他。
pub fn classify_error(code: i32, msg: &str) -> BiliError {
    if is_not_logged_in(msg) {
        return BiliError::NotLoggedIn {
            detail: Some(format!("未登录或登录已失效（{code} {msg}）")),
        };
    }
    if RISK_WORDS.iter().any(|w| msg.contains(w)) {
        return BiliError::RiskControl {
            code: Some(code.to_string()),
            detail: msg.to_string(),
        };
    }
    if code == 110001 || msg.to_lowercase().contains("sign") || msg.contains("签名") {
        return BiliError::Signature {
            detail: format!("{code} {msg}"),
        };
    }
    BiliError::Api {
        code: code.to_string(),
        detail: msg.to_string(),
    }
}

/// Kotlin `isNotLoggedIn`：未登录 / 重新登录 / "登录…失效" / 消息里带 bduss。
pub fn is_not_logged_in(msg: &str) -> bool {
    msg.contains("未登录")
        || msg.contains("重新登录")
        || (msg.contains("登录") && msg.contains("失效"))
        || msg.to_lowercase().contains("bduss")
}

/// 风控关键词（Kotlin `RISK_WORDS`）。
const RISK_WORDS: [&str; 6] = ["频繁", "验证", "安全", "风险", "异常", "封禁"];

/// `it == "baidu.com" || it.endsWith(".baidu.com")`。
pub fn is_baidu_host(host: &str) -> bool {
    host == "baidu.com" || host.ends_with(".baidu.com")
}

/// 从完整 URL 里取 host（不引 url crate：这里只需要 `scheme://host[:port]/...` 这一段）。
pub fn url_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    match authority.rfind('@') {
        Some(i) => authority[i + 1..].to_string(),
        None => authority.to_string(),
    }
}

// ===========================================================================
// multipart / 表单
// ===========================================================================

/// 一份 multipart 请求体，**逐字节**照 OkHttp `MultipartBody` 的写法（`TiebaRequests.multipart`）：
/// 先 `stoken` 段（登录时才有），再 `data` 段（文件名 `file`，没有 Content-Type），最后收尾分隔符。
///
/// 每一段的头是 **`Content-Disposition` + `Content-Length`** 两行——`Content-Length` 是 OkHttp
/// `MultipartBody.Part.create` 自己加的（`body.contentLength()`），漏掉它请求体就不一样了
/// （`TiebaWireEquivalenceTest` 一开始就是在这一段相差 30 来个字节）。
pub fn multipart_body(form: &[(&str, &str)], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 512);
    for (name, value) in form {
        part(&mut out, name, None, value.as_bytes());
    }
    part(&mut out, "data", Some("file"), data);
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn part(out: &mut Vec<u8>, name: &str, filename: Option<&str>, body: &[u8]) {
    out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    let disposition = match filename {
        Some(filename) => format!(
            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n"
        ),
        None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
    };
    out.extend_from_slice(disposition.as_bytes());
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
}

/// `application/x-www-form-urlencoded`（**OkHttp `FormBody.Builder` 的口径**，不是 `URLEncoder`）。
///
/// 实测 OkHttp 4.12：只有 `* - . 0-9 A-Z _ a-z` 原样，**空格是 `%20` 不是 `+`**，`~ ! ' ( ) , : ; @ / ?` 全编
/// ——和 [`crate::hosted::query_encode`]（`addQueryParameter`）是同一个集合，所以直接用那个。
/// （`URLEncoder.encode(s, "UTF-8")` 是另一套：空格 `+`、`*` 原样、`~` 不编；贴吧那边只有
/// `forum_url` / `forum_name` 头用它，见 [`form_encode`]。）
pub fn form_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut out = String::new();
    for (i, (name, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&crate::hosted::query_encode(name));
        out.push('=');
        out.push_str(&crate::hosted::query_encode(value));
    }
    out.into_bytes()
}

/// **`java.net.URLEncoder.encode(s, "UTF-8")` 的口径**（不是 OkHttp 表单编码）：字母数字和 `-._*` 原样，
/// **空格是 `+`**，`~` 会被编成 `%7E`。
///
/// 只有两处在用，而且都必须是这个口径（Kotlin 侧写的就是 `URLEncoder`）：
/// - [`crate::tieba::detail::forum_url`]：吧的网页地址 `?kw=…`；
/// - `frs_page` 的 `forum_name` 请求头。
///
/// 表单请求体（[`form_body`]）和搜索的 query 用的是 OkHttp 那一套，**别拿这个**。
pub fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'*');
        match (keep, b) {
            (true, _) => out.push(b as char),
            (false, b' ') => out.push('+'),
            (false, _) => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ===========================================================================
// protobuf 编码（照 Wire 的声明顺序 + proto3 的有值 / 默认值规则）
// ===========================================================================

/// 极小的 protobuf 写入器。字段**按调用顺序**写出（调用顺序 = `.proto` 里的声明顺序 = Wire 的编码顺序）。
#[derive(Default)]
pub struct Pb {
    out: Vec<u8>,
}

impl Pb {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> Vec<u8> {
        self.out
    }

    fn tag(&mut self, field: u32, wire: u32) {
        self.varint(((field << 3) | wire) as u64);
    }

    fn varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                break;
            }
            self.out.push(byte | 0x80);
        }
    }

    /// `int32` / `int64` / `uint32` / `uint64` / `bool`（wire type 0）。负数按 64 位补码（protobuf 的坑）。
    pub fn int(&mut self, field: u32, value: i64) {
        self.tag(field, 0);
        self.varint(value as u64);
    }

    pub fn string(&mut self, field: u32, value: &str) {
        self.bytes(field, value.as_bytes());
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) {
        self.tag(field, 2);
        self.varint(value.len() as u64);
        self.out.extend_from_slice(value);
    }

    /// `double`（wire type 1，小端 8 字节）。
    pub fn double(&mut self, field: u32, value: f64) {
        self.tag(field, 1);
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    /// 嵌套消息（先算好子消息的字节）。
    pub fn message(&mut self, field: u32, value: &[u8]) {
        self.bytes(field, value);
    }
}

/// `CommonRequest`（`CommonRequest.proto`）。
///
/// 逐字段照 `TiebaRequests.commonV12` 的实参：非 `optional` 的字段等于默认值就不写（`tbs` / `sign` /
/// `applist` / `z_id` / `sample_id` / `device_score` 根本没给）；`optional` 的字段**有值就写**，
/// 所以 `cuid_gid` / `oaid` / `start_scheme` / `extra` 是**空串也要写**，`q_type` / `is_teenager` 是
/// **0 也要写**，`BDUSS` / `stoken` 没登录时是 null → 不写。
pub fn common_request(
    device: &Device,
    cred: Option<&Credentials>,
    now_ms: i64,
    version: &str,
) -> Vec<u8> {
    let mut pb = Pb::new();
    // 声明顺序（= Wire 的写顺序）：1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 14, 23, 24, 25, 26, 28, 29,
    // 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 49, 50, 51, 53, 54, 55, 56, 57,
    // 61, 62, 63, 70
    pb.int(1, 2); // _client_type
    pb.string(2, version); // _client_version
    pb.string(3, &device.client_id); // _client_id
    pb.string(5, device::IMEI); // _phone_imei
    pb.string(6, "1020031h"); // from
    pb.string(7, &device.cuid_galaxy2); // cuid
    pb.int(8, now_ms); // _timestamp
    pb.string(9, device::MODEL); // model
    if let Some(bduss) = cred.map(|c| c.bduss.as_str()) {
        pb.string(10, bduss); // BDUSS（optional）
    }
    // 11 tbs：没给
    pb.int(12, 1); // net_type
    // 14 _phone_newimei：没给
    // 23 sign：没给
    pb.string(24, "1.0.3"); // pversion
    pb.string(25, device::OS_VERSION); // _os_version
    pb.string(26, device::BRAND); // brand
    pb.string(28, "3.0.0"); // lego_lib_version
    // 29 applist：没给
    if let Some(stoken) = cred.and_then(|c| c.stoken.as_deref()) {
        pb.string(30, stoken); // stoken（optional）
    }
    // 31 z_id：没给
    pb.string(32, &device.cuid_galaxy2); // cuid_galaxy2
    pb.string(33, ""); // cuid_gid（optional，空串也写）
    pb.string(34, ""); // oaid（optional，空串也写）
    pb.string(35, &device.c3_aid); // c3_aid
    // 36 sample_id：没给
    pb.int(37, device::SCREEN_W as i64); // scr_w
    pb.int(38, device::SCREEN_H as i64); // scr_h
    pb.double(39, device::SCREEN_DIP); // scr_dip
    pb.int(40, 0); // q_type（optional，0 也写）
    pb.int(41, 0); // is_teenager（optional，0 也写）
    pb.string(42, "2.34.0"); // sdk_ver
    pb.string(43, "3340042"); // framework_ver
    pb.string(44, "1038000"); // swan_game_ver
    pb.int(49, device.install_time_ms); // active_timestamp
    pb.int(50, device.install_time_ms); // first_install_time
    pb.int(51, device.install_time_ms); // last_update_time
    pb.string(53, &event_day(now_ms)); // event_day
    pb.string(54, &device::base64(device.android_id.as_bytes())); // android_id
    pb.int(55, 1); // cmode
    pb.string(56, ""); // start_scheme（optional，空串也写）
    pb.int(57, 1); // start_type
    pb.string(61, ""); // extra（optional，空串也写）
    pb.string(62, &user_agent(version)); // user_agent
    pb.int(63, 1); // personalized_rec_switch
    // 70 device_score：没给
    pb.finish()
}

/// `PersonalizedRequestData`（`Personalized.proto`），字段值照 `TiebaRequests.personalizedBody`。
fn personalized_data(device: &Device, cred: Option<&Credentials>, pn: i32, now_ms: i64) -> Vec<u8> {
    let app_pos = {
        let mut pb = Pb::new();
        pb.int(6, 0); // addr_timestamp（optional，0 也写）
        pb.int(2, 1); // ap_connected = true（optional）
        pb.string(1, "02:00:00:00:00:00"); // ap_mac（optional）
        pb.string(7, ""); // asp_shown_info（optional，空串也写）
        pb.string(3, "BD09LL"); // coordinate_type（optional）
        pb.finish()
    };
    let common = common_request(device, cred, now_ms, V12);
    let mut pb = Pb::new();
    // 声明顺序：30, 37, 36, 1, 29, 4, 22, 3, 27, 23, 5, 6, 26, 11, 28, 10, 9, 8, 7, 2
    // 30 / 37 ad_context_list / ad_ext_params：没给
    pb.message(36, &app_pos);
    pb.message(1, &common);
    // 29 invoke_source：没给
    pb.int(4, if pn <= 1 { 1 } else { 2 }); // load_type（第一页 1 = 下拉刷新）
    // 22 need_forumlist / 3 need_tags / 27 new_install：0，不写
    pb.int(23, 1); // new_net_type
    pb.int(5, 11); // page_thread_count
    pb.int(6, pn as i64); // pn
    // 26 pre_ad_thread_count：0，不写
    pb.int(11, 1); // q_type
    // 28 request_times：0，不写
    pb.double(10, device::SCREEN_DIP); // scr_dip
    pb.int(9, device::SCREEN_H as i64); // scr_h
    pb.int(8, device::SCREEN_W as i64); // scr_w
    // 7 sug_count / 2 tag_code：0，不写
    pb.finish()
}

/// 推荐流的完整请求体：`PersonalizedRequest{ data }` 包进 multipart（登录时先一个 `stoken` 段）。
pub fn personalized_body(device: &Device, cred: Option<&Credentials>, pn: i32, now_ms: i64) -> Vec<u8> {
    let mut outer = Pb::new();
    outer.message(1, &personalized_data(device, cred, pn, now_ms));
    let data = outer.finish();
    let form: Vec<(&str, &str)> = match cred.and_then(|c| c.stoken.as_deref()) {
        Some(stoken) => vec![("stoken", stoken)],
        None => Vec::new(),
    };
    multipart_body(&form, &data)
}

/// `FrsPageRequestData`（`FrsPage/FrsPage.proto`），字段值照 `TiebaRequests.frsPageBody`（匿名：`cred` 传 null）。
fn frs_page_data(device: &Device, forum_name: &str, page: i32, now_ms: i64) -> Vec<u8> {
    let common = common_request(device, None, now_ms, V12_FRS);
    let mut pb = Pb::new();
    // 声明顺序：60, 62, 51, 50, 56, 44, 5, 23, 39, 17, 18, 58, 59, 4, 55, 1, 48, 40, 49, 19, 52, 53,
    // 15, 14, 2, 3, 13, 12, 11, 47, 27, 16, 61, 8, 45
    pb.message(39, &common);
    // 4 is_good（optional，0 也写）
    pb.int(4, 0);
    pb.string(1, forum_name); // kw（原始吧名，不是 id）
    // 49 load_type = 0：不写
    // **第一页线上要传 pn=0**（aiotieba 同样 `0 if pn == 1`）：传 1 时服务端走"首屏"模式，
    // 只给前十几条详情，其余 tid 放进 thread_id_list；传 0 才是整页详情。
    let pn = if page <= 1 { 0 } else { page };
    if pn != 0 {
        pb.int(15, pn as i64);
    }
    // 14 q_type = 0：不写
    pb.int(2, FRS_PAGE_SIZE as i64); // rn
    pb.int(3, FRS_PAGE_NEED as i64); // rn_need
    // 13 scr_dip = 0 / 12 scr_h = 0 / 11 scr_w = 0：不写
    pb.int(47, SORT_BY_CREATE as i64); // sort_type（optional，有值就写）
    pb.finish()
}

/// 吧内帖子列表的完整请求体（multipart：匿名，所以没有 `stoken` 段）。
pub fn frs_page_body(device: &Device, forum_name: &str, page: i32, now_ms: i64) -> Vec<u8> {
    let mut outer = Pb::new();
    outer.message(1, &frs_page_data(device, forum_name, page, now_ms));
    multipart_body(&[], &outer.finish())
}

/// `PbPageRequestData`（`PbPage/PbPageRequestData.proto`），字段值照 `TiebaRequests.pbPageBody`。
///
/// 写序 = **`.proto` 的声明顺序**（4, 6, 8, 9, 13, 14, 15, 16, 17, 18, 25, 74, 75），而且 proto3 的非 `optional`
/// 字段**等于默认值就不写**——所以 `lz = 0` / `r = 0`（正序）/ `floor_rn = 0`（详情不要楼中楼预览）都不出现，
/// 而 `with_floor` / `q_type` / `floor_sort_type` / `source_type` 这些非 0 的一直在。
fn pb_page_data(
    device: &Device,
    cred: Option<&Credentials>,
    tid: i64,
    pn: i32,
    rn: i32,
    sort: i32,
    floor_rn: i32,
    now_ms: i64,
) -> Vec<u8> {
    let common = common_request(device, cred, now_ms, V12);
    let mut pb = Pb::new();
    pb.int(4, tid); // kz
    if sort != 0 {
        pb.int(6, sort as i64); // r（0 = 正序，不写）
    }
    pb.int(8, 1); // with_floor
    if floor_rn != 0 {
        pb.int(9, floor_rn as i64); // floor_rn（详情传 0，不写）
    }
    pb.int(13, rn as i64); // rn
    pb.int(14, device::SCREEN_W as i64); // scr_w
    pb.int(15, device::SCREEN_H as i64); // scr_h
    pb.double(16, device::SCREEN_DIP); // scr_dip
    pb.int(17, 2); // q_type
    pb.int(18, pn as i64); // pn（从 1 开始，1 也写）
    pb.message(25, &common); // common
    pb.int(74, 1); // floor_sort_type（楼中楼按热度，aiotieba 的 comment_sort_by_agree）
    pb.int(75, 2); // source_type
    pb.finish()
}

/// 楼层 / 详情的完整请求体（multipart：登录时先一个 `stoken` 段）。
pub fn pb_page_body(
    device: &Device,
    cred: Option<&Credentials>,
    tid: i64,
    pn: i32,
    rn: i32,
    sort: i32,
    floor_rn: i32,
    now_ms: i64,
) -> Vec<u8> {
    let mut outer = Pb::new();
    outer.message(
        1,
        &pb_page_data(device, cred, tid, pn, rn, sort, floor_rn, now_ms),
    );
    let data = outer.finish();
    let form: Vec<(&str, &str)> = match cred.and_then(|c| c.stoken.as_deref()) {
        Some(stoken) => vec![("stoken", stoken)],
        None => Vec::new(),
    };
    multipart_body(&form, &data)
}

/// `PbFloorRequestData`（`PbFloor/PbFloorRequestData.proto`），字段值照 `TiebaRequests.pbFloorBody`。
///
/// 写序 = 声明顺序（1, 2, 4, 5, 6, 7, 9, 10, 15）。两个 `optional` 字段（`is_comm_reverse` / `ori_ugc_type`）
/// 是 **0 也要写**（proto3 的 optional 有值就序列化）；`spid` / `st_type` / `forum_id` / `ori_ugc_*` 没给就不写。
fn pb_floor_data(
    device: &Device,
    cred: Option<&Credentials>,
    tid: i64,
    pid: i64,
    pn: i32,
    now_ms: i64,
) -> Vec<u8> {
    let common = common_request(device, cred, now_ms, V22);
    let mut pb = Pb::new();
    pb.int(1, tid); // kz
    pb.int(2, pid); // pid（optional，有值就写）
    pb.int(4, pn as i64); // pn
    pb.int(5, device::SCREEN_W as i64); // scr_w
    pb.int(6, device::SCREEN_H as i64); // scr_h
    pb.double(7, device::SCREEN_DIP); // scr_dip
    pb.message(9, &common); // common
    pb.int(10, 0); // is_comm_reverse（optional，0 也写）
    pb.int(15, 0); // ori_ugc_type（optional，0 也写）
    pb.finish()
}

/// 楼中楼的完整请求体（multipart：**没有 stoken 段**，和 TiebaLite `pbFloorFlow` 一样；
/// 但 `CommonRequest` 里带不带 BDUSS 由 `cred` 决定，和 Kotlin 一样）。
pub fn pb_floor_body(
    device: &Device,
    cred: Option<&Credentials>,
    tid: i64,
    pid: i64,
    pn: i32,
    now_ms: i64,
) -> Vec<u8> {
    let mut outer = Pb::new();
    outer.message(1, &pb_floor_data(device, cred, tid, pid, pn, now_ms));
    multipart_body(&[], &outer.finish())
}

/// 帖子 id：必须是正整数（Kotlin `tid(id)` 的 `ParseException`）。
fn parse_tid(id: &str) -> Result<i64, BiliError> {
    id.parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| tieba::parse_failure(format!("帖子 id 不对：{id}")))
}

/// 页码游标：`None` = 第 1 页；给了就得是 >= 1 的整数（Kotlin `pageOf(cursor)`）。
fn page_of(cursor: Option<&str>) -> Result<i32, BiliError> {
    match cursor {
        None => Ok(1),
        Some(value) => value
            .parse::<i32>()
            .ok()
            .filter(|v| *v >= 1)
            .ok_or_else(|| tieba::parse_failure(format!("游标无效：{value}"))),
    }
}

/// `yyyyMdd`（月份不补零），北京时间，和 Kotlin `TiebaRequests.eventDay`（`SimpleDateFormat("yyyyMdd")`
/// + `Asia/Shanghai`）一致。
pub fn event_day(now_ms: i64) -> String {
    let secs = now_ms.div_euclid(1000) + 8 * 3600;
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m}{d:02}")
}

// `civil_from_days` 从 `crate::hosted` 来（和 B站 的"北京时间今天"共用一份）。

