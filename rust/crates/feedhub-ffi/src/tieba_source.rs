//! **贴吧的平台对象**：推荐流 / 关注流（吧列表 + 每个吧的一页）**和详情 / 楼层 / 楼中楼 / 主视频**
//! 的整个"请求怎么发 + 怎么解析"都在这一个 `uniffi::Object` 后面（实现在 `feedhub_core::tieba::source`）。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读 store、读 Cookie 罐。见 [`crate::host`]。
//! **两个 HTTP 端口**（App 接口 / 网页接口），对应 Kotlin `TiebaSource` 的两个客户端：
//! App 接口不读 Cookie 罐（登录凭据在 protobuf 字段里），网页接口读 Cookie 罐 + 桌面 UA / Referer。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiTiebaSource(appBase, webBase, hostAppHttp, hostWebHttp, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.recommend(cursor, nowMs) }
//! ```
//! **平台错误不在这里归类**：`error_code` / `user_msg` / `error_msg` 原样带回去，由 Kotlin 的
//! `TiebaSource.mapError` 按唯一那一份规则翻成异常（未登录 / 风控 / 110001 签名 / Api）。

use std::sync::{Arc, Mutex};

use feedhub_core::tieba;
use feedhub_core::tieba::following::{FollowingFeed, HostedPager};
use feedhub_core::tieba::forum::ForumStore;
use feedhub_core::tieba::forum::ThreadKey;
use feedhub_core::tieba::source::TiebaHostedSource;

use feedhub_core::tieba::search::SearchSort;
use feedhub_core::tieba::source::CommentSort;

use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::tieba::{
    to_ffi_parsed_comments, to_ffi_parsed_detail, to_ffi_parsed_main_video, to_ffi_parsed_play,
    FfiTiebaForum, FfiTiebaParsedComments, FfiTiebaParsedDetail, FfiTiebaParsedMainVideo,
    FfiTiebaParsedPlay,
};
use crate::{map_error, to_ffi_page, FfiCommentSort, FfiError, FfiPage};

/// 搜索的排序（Kotlin `SearchSort`）。贴吧只有两档：没有"最热"（传进来映射成相关，
/// 但插件能力里没声明，聚合层不会传）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTiebaSearchSort {
    Relevance,
    Latest,
    Hot,
}

impl From<FfiTiebaSearchSort> for SearchSort {
    fn from(s: FfiTiebaSearchSort) -> Self {
        match s {
            FfiTiebaSearchSort::Relevance => SearchSort::Relevance,
            FfiTiebaSearchSort::Latest => SearchSort::Latest,
            FfiTiebaSearchSort::Hot => SearchSort::Hot,
        }
    }
}

/// 贴吧平台 id（和 `TiebaPlugin` 里的一致）。
const PLATFORM: &str = tieba::TIEBA;

/// 一页 + 平台错误（`error` 为 true 时 `page` 是空的，和 Kotlin 一样：先看错误码，再碰数据）。
///
/// 和 `FfiTiebaFeed`（纯函数那条路的记录）的区别：多一个 `error: bool`。原因是平台层还要处理
/// "服务端出错时直接回 JSON 外壳"这条路径——那种壳子里的 `error_code` 可能是 **0**，靠 `error_code != 0`
/// 分不出"有错误"和"没错误"，所以单独一个布尔。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaHostedFeed {
    pub page: FfiPage,
    /// 吧内帖子列表：这是不是这个吧的最后一页（推荐流恒为 false）。
    pub last: bool,
    /// 有平台错误（含 `{...}` 错误外壳）：Kotlin 侧按 `TiebaSource.mapError` 归类后抛出。
    pub error: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 关注的吧 + 登录状态。`error` 为 true 时 `error_code` / `error_msg` 是平台给的错（`forumGuide` 的
/// `error_code` 非 0）；`is_login` 原样带回去，Kotlin 判 `== 0` → `NotLoggedInException`。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaHostedForums {
    pub forums: Vec<FfiTiebaForum>,
    pub is_login: Option<i64>,
    pub error: bool,
    pub error_code: i32,
    pub error_msg: String,
}

fn to_ffi_feed(feed: tieba::Feed) -> FfiTiebaHostedFeed {
    let page = feedhub_core::model::Page {
        items: feed.items,
        next: feed.next,
    };
    let (error, code, user_msg, error_msg) = match feed.error {
        Some(e) => (true, e.code, e.user_msg, e.error_msg),
        None => (false, 0, String::new(), String::new()),
    };
    FfiTiebaHostedFeed {
        page: to_ffi_page(&page),
        last: feed.last,
        error,
        error_code: code,
        user_msg,
        error_msg,
    }
}

/// 关注流一页（`nodes` / `roots` 和其余平台一样是"平坦数组 + 根下标"）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaFollowingPage {
    pub page: crate::FfiPage,
}

/// 关注流里一个吧的缓冲摘要（排障 / 测试用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaForumState {
    pub id: i64,
    pub name: String,
    pub items: u32,
    pub tail_pos: i32,
    pub exhausted: bool,
    pub head_fetched_at: i64,
    pub closed_until: i64,
}

/// 贴吧平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiTiebaSource {
    inner: TiebaHostedSource,
    /// 关注流的进程内状态（各吧缓冲）。
    following: Mutex<FollowingFeed>,
}

#[uniffi::export]
impl FfiTiebaSource {
    /// [`app_base`] / [`web_base`] 是接口根（线上是 `https://tiebac.baidu.com` / `https://tieba.baidu.com`，
    /// 测试指向 MockWebServer）。两个 HTTP 回调必须是 Kotlin 那边 `TiebaSource` 自己那两个客户端。
    #[uniffi::constructor]
    pub fn new(
        app_base: String,
        web_base: String,
        http_app: Arc<dyn HostHttp>,
        http_web: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: TiebaHostedSource::new(
                app_base,
                web_base,
                Arc::new(HttpAdapter(http_app)),
                Arc::new(HttpAdapter(http_web)),
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
            ),
            following: Mutex::new(FollowingFeed::new()),
        })
    }

    /// **Rust 联网层的构造**：两个宿主 HTTP 端口换成两份 [`FfiHostSpec`]。
    ///
    /// 两份的区别（见 `TiebaSource`）：`app` **不带 Cookie、没有默认头**（App 接口自己带签名）；
    /// `web` **带 Cookie**、桌面 UA + Referer（网页接口按登录态回内容）。
    #[uniffi::constructor]
    pub fn with_net(
        app_base: String,
        web_base: String,
        stack: Arc<crate::net_host::FfiNetStack>,
        app_spec: crate::net_host::FfiHostSpec,
        web_spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let http_app = stack.build_media_spec_public(app_spec)?;
        let http_web = stack.build_host_public(web_spec)?;
        Ok(Arc::new(Self {
            inner: TiebaHostedSource::new(
                app_base,
                web_base,
                http_app,
                http_web,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
            following: Mutex::new(FollowingFeed::new()),
        }))
    }

    /// 推荐流一页。`now_ms` 是毫秒时间戳（请求体里的 `_timestamp` / `event_day`、`client_logid` 都用它）——
    /// 由调用方传进来，测试才能注入固定值把两次运行的请求逐字节比。游标是页码，下一页永远是 `pn + 1`。
    pub fn recommend(
        &self,
        cursor: Option<String>,
        now_ms: i64,
    ) -> Result<FfiTiebaHostedFeed, FfiError> {
        self.inner
            .recommend(cursor.as_deref(), now_ms)
            .map(to_ffi_feed)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 关注流里某个吧的一页（`/c/f/frs/page`，匿名请求）。`page` 从 1 起。
    pub fn frs_page(
        &self,
        forum_name: String,
        page: i32,
        now_ms: i64,
    ) -> Result<FfiTiebaHostedFeed, FfiError> {
        self.inner
            .frs_page(&forum_name, page, now_ms)
            .map(to_ffi_feed)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 关注的吧（网页接口的表单 POST）。
    pub fn forum_guide(&self) -> Result<FfiTiebaHostedForums, FfiError> {
        self.inner
            .forum_guide()
            .map(|forums| {
                let (error, code, error_msg) = match forums.error {
                    Some(e) => (true, e.code, e.error_msg),
                    None => (false, 0, String::new()),
                };
                FfiTiebaHostedForums {
                    forums: forums
                        .forums
                        .into_iter()
                        .map(|f| FfiTiebaForum {
                            id: f.id,
                            name: f.name,
                            day_threads: f.day_threads,
                            avatar: f.avatar,
                            member_count: f.member_count,
                            thread_count: f.thread_count,
                        })
                        .collect(),
                    is_login: forums.is_login,
                    error,
                    error_code: code,
                    error_msg,
                }
            })
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 搜一页。两种接口由 [`community`] 决定（和 Kotlin `TiebaSource.search` 同一行）：
    ///
    /// - **全吧搜索**（`community` 是空）：网页 GET `/mo/q/search/thread`，**PC salt** 签名；
    /// - **吧内搜索**（给了吧名）：App 表单 POST `/c/s/searchpost`，App salt 签名 + App 头。
    ///
    /// 平台错误码（`no` / `error_code` 非 0）当数据带回去（`error` 为 true 时 `page` 是空的），
    /// 归类仍走 Kotlin 的 `TiebaSource.mapError`（风控关键词 / 未登录 / 110001 签名）。
    /// [`cursor`] 是页码（`None` = 第 1 页）；[`now_ms`] 只给吧内搜索的 `client_logid` 用。
    pub fn search(
        &self,
        text: String,
        sort: FfiTiebaSearchSort,
        community: Option<String>,
        cursor: Option<String>,
        now_ms: i64,
    ) -> Result<FfiTiebaHostedFeed, FfiError> {
        let outcome = match community.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            None => self.inner.global_search(&text, sort.into(), cursor.as_deref()),
            Some(community) => {
                self.inner
                    .in_forum_search(community, &text, sort.into(), cursor.as_deref(), now_ms)
            }
        };
        let parsed = outcome.map_err(|e| map_error(PLATFORM, e))?;
        Ok(match parsed.error {
            Some(error) => FfiTiebaHostedFeed {
                page: to_ffi_page(&feedhub_core::model::Page {
                    items: Vec::new(),
                    next: None,
                }),
                last: false,
                error: true,
                error_code: error.code,
                user_msg: error.user_msg,
                error_msg: error.error_msg,
            },
            None => {
                let page = parsed.value.unwrap_or(feedhub_core::model::Page {
                    items: Vec::new(),
                    next: None,
                });
                FfiTiebaHostedFeed {
                    page: to_ffi_page(&page),
                    last: false,
                    error: false,
                    error_code: 0,
                    user_msg: String::new(),
                    error_msg: String::new(),
                }
            }
        })
    }

    /// 帖子详情：**整套请求 + 解析都在 Rust**（`pb/page` 第一页、正序、只要一楼、带登录凭据）。
    ///
    /// 平台错误码当数据带回去（`error` 为 true 时 `detail` 是空的），归类仍走 Kotlin 的
    /// `TiebaSource.mapError`。`now_ms` 是请求体里的 `_timestamp` / `event_day` / `client_logid`。
    pub fn detail(&self, id: String, now_ms: i64) -> Result<FfiTiebaParsedDetail, FfiError> {
        self.inner
            .detail(&id, now_ms)
            .map(to_ffi_parsed_detail)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 一级评论（楼层）：游标是页码，`sort` 映射成 `pb/page` 的 `r`（0 / 1 / 2）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
        now_ms: i64,
    ) -> Result<FfiTiebaParsedComments, FfiError> {
        let sort = match sort {
            FfiCommentSort::Default => CommentSort::Default,
            FfiCommentSort::Hot => CommentSort::Hot,
            FfiCommentSort::Newest => CommentSort::Newest,
        };
        self.inner
            .comments(&id, cursor.as_deref(), sort, now_ms)
            .map(to_ffi_parsed_comments)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 某一楼的楼中楼（`pb/floor`）：`comment_id` 是楼层的 pid，游标也是页码。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_ms: i64,
    ) -> Result<FfiTiebaParsedComments, FfiError> {
        self.inner
            .replies(&id, &comment_id, cursor.as_deref(), now_ms)
            .map(to_ffi_parsed_comments)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 播放地址（Kotlin `TiebaSource.play`）：
    /// - `part_id = "main"`：一楼的第一个视频（`pb/page` 第一页；详情取过的 5 分钟内复用，
    ///   没有才现取）；
    /// - `part_id = "url:<mp4>"`：**地址就在参数里**，一个请求都不发。
    ///
    /// 参数写法不对时在**发请求之前**就报错（`FfiError.Parse`）。平台错误原样带回去
    /// （`error == true`），由 Kotlin 的 `TiebaSource.mapError` 归类；两者都没有 = 帖子里没有那个视频。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        now_ms: i64,
    ) -> Result<FfiTiebaParsedPlay, FfiError> {
        self.inner
            .play(&id, &part_id, now_ms)
            .map(to_ffi_parsed_play)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 帖子的主视频（**只取地址**，不带 `PlayInfo`）：详情取过的第一页 5 分钟内复用，没有才现取。
    /// 没有主视频时 `video` 是空的、`error` 是 false（Kotlin 那边翻成"一楼里没有视频"）。
    ///
    pub fn main_video(
        &self,
        id: String,
        now_ms: i64,
    ) -> Result<FfiTiebaParsedMainVideo, FfiError> {
        self.inner
            .main_video(&id, now_ms)
            .map(to_ffi_parsed_main_video)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// **关注流一页**（Kotlin `TiebaSource.following`）：吧列表 TTL → 哪些吧该重抓 → 惰性 k 路归并
    /// → 真发 `frs/page` → 存盘，全在 Rust。
    ///
    /// `cursor` 是上一页最后一条的 `"<秒>:<tid>"`（[`FfiTiebaFollowingPage::next`]）；`None` = 刷新
    /// （会联网），非空 = 只在缓存里继续往下排。
    pub fn following_page(
        &self,
        cursor: Option<String>,
        now_ms: i64,
    ) -> Result<FfiTiebaFollowingPage, FfiError> {
        // 游标不对 → 解析错误（和 Kotlin `ThreadKey.decode(...) ?: throw ParseException` 一样）
        let before = match cursor.as_deref().filter(|c| !c.is_empty()) {
            Some(text) => Some(ThreadKey::decode(text).ok_or_else(|| FfiError::Parse {
                platform: PLATFORM.to_string(),
                detail: "关注流游标无效".to_string(),
            })?),
            None => None,
        };
        let store = ForumStore::new(self.inner.store_handle());
        let pager = HostedPager::new(&self.inner, now_ms);
        let mut feed = self.following.lock().unwrap_or_else(|e| e.into_inner());
        let page = feed
            .page(&store, &pager, before, now_ms)
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(FfiTiebaFollowingPage {
            page: to_ffi_page(&feedhub_core::model::Page {
                items: page.items,
                next: page.next,
            }),
        })
    }

    /// 关注流手上那份缓冲（排障 / 测试用：每个吧一条摘要，不发请求、不读盘）。
    pub fn following_forums(&self) -> Vec<FfiTiebaForumState> {
        let feed = self.following.lock().unwrap_or_else(|e| e.into_inner());
        match feed.states() {
            None => Vec::new(),
            Some(states) => {
                let mut ids: Vec<i64> = states.keys().copied().collect();
                ids.sort_unstable();
                ids.into_iter()
                    .filter_map(|id| states.get(&id))
                    .map(|f| FfiTiebaForumState {
                        id: f.id,
                        name: f.name.clone(),
                        items: f.items.len() as u32,
                        tail_pos: f.tail_pos,
                        exhausted: f.exhausted,
                        head_fetched_at: f.head_fetched_at,
                        closed_until: f.closed_until,
                    })
                    .collect()
            }
        }
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn app_base(&self) -> String {
        self.inner.app_base().to_string()
    }

    /// 网页接口根（排障 / 测试断言用）。
    pub fn web_base(&self) -> String {
        self.inner.web_base().to_string()
    }
}

// ===========================================================================
// 账号：我是谁 / 关注·取关 / 扫码登录
// ===========================================================================

#[uniffi::export]
impl FfiTiebaSource {
    /// 「我是谁」：网页接口 `/mo/q/newmoindex?need_user=1`。没登录返回 `None`（不抛、不发请求）。
    pub fn whoami(&self) -> Result<Option<crate::FfiUserRef>, FfiError> {
        self.inner
            .whoami()
            .map(|user| user.as_ref().map(crate::to_ffi_user_ref))
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 关注（`follow = true`）/ 取关一个吧。**唯一的写操作**，只在用户点按钮时调用，不重试。
    /// [`forum_name`] 由调用方给（关注的吧列表里的吧名比 target 里的可靠）。
    pub fn set_following(
        &self,
        target: crate::FfiFollowTarget,
        follow: bool,
        forum_name: Option<String>,
        now_ms: i64,
    ) -> Result<(), FfiError> {
        self.inner
            .set_following(
                &crate::account::from_ffi_follow_target(&target),
                follow,
                forum_name.as_deref(),
                now_ms,
            )
            .map_err(|e| map_error(PLATFORM, e))
    }
}

/// 贴吧（百度通行证）的网页扫码登录。
///
/// 为什么是另一个对象：扫码要**临时 Cookie 罐**的宿主能力（`HostQrLogin`），而那个端口只有扫码用得上。
/// 二维码图片的解码也走宿主（zxing 只有 Java 版），见 `HostQrLogin.decode_qr_image`。
#[derive(uniffi::Object)]
pub struct FfiTiebaQr {
    inner: feedhub_core::tieba::qr::TiebaQrSource,
}

#[uniffi::export]
impl FfiTiebaQr {
    /// [`unicast_timeout_ms`] 是长轮询的读超时（Kotlin `unicastReadTimeoutS × 1000`）。
    #[uniffi::constructor]
    pub fn new(
        passport: String,
        app_base: String,
        web_base: String,
        unicast_timeout_ms: i64,
        qr: Arc<dyn crate::host::HostQrLogin>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: feedhub_core::tieba::qr::TiebaQrSource::new(
                passport,
                app_base,
                web_base,
                unicast_timeout_ms,
                Arc::new(crate::host::QrAdapter(qr)),
            ),
        })
    }

    /// 第一步：要一张二维码。[`gid`] 由调用方生成（Kotlin `UUID.randomUUID().toString().uppercase()`）。
    pub fn start(&self, now_ms: i64, gid: String) -> Result<crate::account::FfiQrLogin, FfiError> {
        self.inner
            .start(now_ms, &gid)
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 第二步：问一次状态（长轮询；`gid` 从 `extra` 里带回来）。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
        gid: String,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let login = crate::account::from_ffi_qr_login(&login);
        self.inner
            .poll(&login, now_ms, &gid)
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// passport 的根（排障 / 测试断言用）。
    pub fn passport(&self) -> String {
        self.inner.passport().to_string()
    }
}

/// 一页搜索结果的 `FfiTiebaHostedFeed`（没有平台错误的那一支）。纯函数入口用。
pub(crate) fn search_feed(page: &feedhub_core::model::Page) -> FfiTiebaHostedFeed {
    FfiTiebaHostedFeed {
        page: to_ffi_page(page),
        last: false,
        error: false,
        error_code: 0,
        user_msg: String::new(),
        error_msg: String::new(),
    }
}

/// 平台错误的那一支（`page` 是空页）。纯函数入口用。
pub(crate) fn failed_search_feed(error: feedhub_core::tieba::PlatformError) -> FfiTiebaHostedFeed {
    FfiTiebaHostedFeed {
        page: to_ffi_page(&feedhub_core::model::Page {
            items: Vec::new(),
            next: None,
        }),
        last: false,
        error: true,
        error_code: error.code,
        user_msg: error.user_msg,
        error_msg: error.error_msg,
    }
}
