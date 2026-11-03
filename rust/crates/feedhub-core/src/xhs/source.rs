//! 小红书 Source 的**平台层**：推荐流（homefeed）和关注流三个接口
//! （`followings/self` / `followings/extra` / `user_posted`）的"请求怎么拼、怎么签、错误怎么判"。
//!
//! 收发一律通过宿主的 [`HttpHost`]（OkHttp，用 `XhsApi.http` 那一个客户端：限速 / 默认头兜底 / Cookie 罐 /
//! 代理 / fail-closed 全在它上面），这里不碰 socket，`.so` 里也没有网络栈。
//!
//! 逐条对照 `source-xiaohongshu/.../XhsSource.kt`（推荐流）、`XhsFollowFeed.kt`（关注流三个接口）、
//! `XhsApi.kt`（URL / 头 / 错误）：方法 + 完整 URL（参数顺序）+ 请求头 + body 必须和 Kotlin 版**逐字节一样**，
//! `XhsRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! **时间和随机数都是注入的**：`now_ms` 由调用方给，[`SignRandom`] 由 Kotlin 按
//! `XhsSigner.sign` 的取用顺序取好；Rust 里没有随机源、不读时钟。签名器的会话计数（`seq` / `signCount` /
//! `dsllt` / `xraySeq`）**住在这个对象里**（一个 Source 一份），构造时由 Kotlin 传初值——这样推荐 / 关注 /
//! 表情表 / 详情 / 评论 / 扫码全都用同一份计数。
//!
//! **当前搬了五条请求**：homefeed / followings/self / followings/extra / user_posted / search（搜索笔记）。
//! 关注流的调度（谁有新笔记、缓存、TTL）、表情表的抓取和搜索结果的表情贴回留在 Kotlin，Rust 的粒度是"一个请求 / 一页"。
//!
//! 这三条是**用户正等着的请求**，所以请求头里多一个内部伪请求头
//! [`hosted::PRIORITY_HEADER`]（宿主发出去之前删掉、转成 OkHttp 的优先级 tag）；另外三条流照旧是后台。
//! 表情表仍然由 Kotlin 取（详情 / 评论**不挡首屏**：手上没有表就按原文显示，后台补取），
//! Rust 只是"按已有那份表切 Span"，所以解析要多传一份 `emotes` 进来。

use std::sync::{Arc, Mutex};

use crate::detail::{CommentPage, FeedDetail};
use crate::host::{CookieHost, HttpHost, HttpRequest, StoreHost};
use crate::hosted;
use crate::json;
use crate::model::{BiliError, UserRef};
use crate::play::PlayInfo;
use crate::xhs::emotes::{self, EmoteTable};
use crate::xhs::follow::Authors;
use crate::xhs::{self, codec, detail, play, sign};

/// 线上接口根（和 `XhsApi.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://edith.xiaohongshu.com";

/// Cookie 罐里小红书"身份 URL"（和 Kotlin 的 `XhsApi.WEB_URL` 一样：按 www.xiaohongshu.com 取 Cookie）。
pub const WEB_URL: &str = "https://www.xiaohongshu.com/";

/// 当前登录账号的 user_id（`whoami` 取到后存的；账号半没搬，键和 Kotlin 那份一样）。
pub const KEY_USER_ID: &str = "xhs.user_id";

/// b1（`X-S-Common` 的 x8）：一次生成、之后一直用，键和格式和 Kotlin 那份一样。
pub const KEY_B1: &str = "xhs.b1";

const HOMEFEED: &str = "/api/sns/web/v1/homefeed";
const FOLLOWINGS_SELF: &str = "/api/sns/web/v1/user/followings/self";
const FOLLOWINGS_EXTRA: &str = "/api/sns/web/v1/user/followings/extra";
const USER_POSTED: &str = "/api/sns/web/v1/user_posted";

/// 笔记详情（`POST`，带 `x-rap-param` + `xy-direction`）。
const NOTE_FEED: &str = "/api/sns/web/v1/feed";

/// 一级评论（`GET`，**不**带 `x-rap-param` / `xy-direction`）。
const COMMENT_PAGE: &str = "/api/sns/web/v2/comment/page";

/// 楼中楼（`GET`，同上）。
const SUB_COMMENT_PAGE: &str = "/api/sns/web/v2/comment/sub/page";

/// `XhsSource.play` 复用"详情刚取过的笔记"的时限（Kotlin `NOTE_REUSE_MS`）。播放地址有效期约 3 天，
/// 5 分钟内的那份够新；点开详情马上点播放就**只有一个** `POST feed`（小红书风控最看详情次数）。
pub const NOTE_REUSE_MS: i64 = 5 * 60_000;

/// 记住最近几条笔记。
const RECENT_NOTES: usize = 8;

/// `user_posted` 的 `image_formats`（网页的值）。
const IMAGE_FORMATS: &str = "jpg,webp,avif";

/// `user_posted` 的 `num`（网页的值，一页 30 条，另外还有置顶的）。
const POSTED_NUM: i32 = 30;

/// POST 的 Content-Type（`XhsApi.JSON_TYPE`，逐字节一样）。
const JSON_TYPE: &str = "application/json;charset=UTF-8";

/// "我是谁"的路径（`XhsSource.whoami`）。
const ME: &str = "/api/sns/web/v2/user/me";

/// 小红书的平台对象：三个宿主端口 + 一份签名计数。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustXhsHosted`）建一次、跟着 Source 活；没有后台线程、没有定时器，
/// 所以闲置时零唤醒，也不需要 `Drop` 里收尾。
pub struct XhsHostedSource {
    base: String,
    profile: sign::Profile,
    /// 构造时那份 profile 的原文（扫码平台对象要和这里**同一份**，不再 serialise 一遍）。
    profile_text: String,
    http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    /// 签名器的会话计数：所有签名（包括 Kotlin 那边详情 / 评论用的 `sign`）都走这一份。
    state: crate::xhs::qr::SharedSign,
    /// 最近取过的笔记（**访问序 LRU**，键是笔记 id）：`play` 复用详情刚拿到的原始 body，
    /// 点开视频再播放不再多发一个 `POST feed`。口径照 Kotlin `XhsSource.recentNotes`：
    /// 最多 [`RECENT_NOTES`] 条、[`NOTE_REUSE_MS`] 内有效，每次读都把它挪到队尾。
    notes: Mutex<Vec<RecentNote>>,
    /// 表情表：内存里那份 + 两次补取之间的节流状态（Kotlin `emoteCache` / `emoteRetryAtMs` /
    /// `emoteFetching` 三件套）。表本身住在 Rust 里，详情 / 评论不再每次从 Kotlin 传。
    emotes: Mutex<EmoteState>,
    /// 关注流的**作者缓冲**（Kotlin `XhsFollowFeed.cache`）：一份进程内状态，读一次之后一直改它，
    /// 落盘在"一次刷新的 finally"里。`None` = 这个进程还没读过。
    follow_state: Mutex<Option<crate::xhs::follow::Authors>>,
    /// 关注流的**串行门**（Kotlin `XhsFollowFeed.lock` 那个 `Mutex`）：关注流可能被首页预取和用户
    /// 下拉同时触发，一次只许跑一个页面（否则两份状态互相覆盖、缓存被写坏）。
    follow_lock: Mutex<()>,
}

/// 表情表的运行时状态（`XhsSource` 里那三个字段）。
#[derive(Debug, Default)]
struct EmoteState {
    /// 内存里那份（Kotlin `emoteCache`：拿到就存，**不再看 TTL**；过期判断在读 `ctx.store` 那一步）。
    table: Option<EmoteTable>,
    /// 下一次允许再试的时间（毫秒，Kotlin `emoteRetryAtMs`）。
    retry_at_ms: i64,
    /// 有没有一个在飞的请求（Kotlin `emoteFetching` 的 `AtomicBoolean`）。
    fetching: bool,
}

/// 一条"最近取过的笔记"：笔记 id、取的时刻（毫秒）、原始响应 body。
#[derive(Debug, Clone)]
struct RecentNote {
    note_id: String,
    at_ms: i64,
    body: String,
}

/// 一个拼好的请求（签名 / 发出去都用它，避免"两边各拼一遍"）。
#[derive(Debug, Clone)]
struct Built {
    method: &'static str,
    path: &'static str,
    /// **有序**查询参数（签名和 URL 用同一份）。
    query: Vec<(String, String)>,
    /// POST 的紧凑 JSON；GET 为 `None`。
    body: Option<String>,
    /// 要不要 `x-rap-param`。
    rap: bool,
    /// 要不要 `xy-direction`。
    xy: bool,
}

impl Built {
    /// 网址里的那串（编码和 Kotlin `XhsApi.encodeQuery` 一样：`encodeURIComponent` + 逗号保留）。
    fn encoded_query(&self) -> String {
        self.query
            .iter()
            .map(|(k, v)| format!("{}={}", xhs::encode_component(k), xhs::encode_component(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    /// 签名用的 `api`：POST 是路径，GET 是路径 + `?` + 查询串（和实际发出去的逐字节一致）。
    fn api(&self) -> String {
        if self.query.is_empty() {
            self.path.to_string()
        } else {
            format!("{}?{}", self.path, self.encoded_query())
        }
    }

    fn url(&self, base: &str) -> String {
        format!("{}{}", base, self.api())
    }

    /// 要不要 `cache-control: no-cache` + `pragma: no-cache`（Kotlin `XhsApi.NO_CACHE_PATHS`：
    /// `user/me` / `config` / `system/config`）——这三条的默认头里多这两行，**顺序在 `priority` 之后**。
    fn no_cache(&self) -> bool {
        matches!(
            self.path,
            "/api/sns/web/v2/user/me" | "/api/sns/web/v1/config" | "/api/sns/web/v1/system/config"
        )
    }

    fn sign_request(
        &self,
        a1: String,
        loadts_ms: i64,
        web_build: String,
        b1: String,
        user_id: Option<String>,
    ) -> sign::SignRequest {
        sign::SignRequest {
            api: self.api(),
            body: self.body.clone(),
            a1,
            loadts_ms,
            web_build,
            b1,
            user_id,
            rap: self.rap,
            xy: self.xy,
        }
    }
}

impl XhsHostedSource {
    /// [`profile_json`] 是 Kotlin 读好的 `sign_profile.json`（或 `ctx.store` 里的覆盖版）再序列化出来的，
    /// Rust 不读文件；[`state`] 是签名计数的初值（Kotlin 按 `XhsSigner` 构造函数的顺序抽好的）。
    pub fn new(
        base: impl Into<String>,
        profile_json: &str,
        state: sign::SignState,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Result<Self, String> {
        Ok(Self {
            base: base.into(),
            profile: sign::Profile::parse(profile_json)?,
            profile_text: profile_json.to_string(),
            http,
            store,
            cookies,
            state: Arc::new(Mutex::new(state)),
            notes: Mutex::new(Vec::new()),
            emotes: Mutex::new(EmoteState::default()),
            follow_state: Mutex::new(None),
            follow_lock: Mutex::new(()),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 签名计数的**那一份 `Arc`**：扫码平台对象（[`crate::xhs::qr::XhsQrSource`]）要拿同一个，
    /// 这样一次会话里的 `seq` / `signCount` / `xraySeq` 才是一条序列（Kotlin 那边也是同一个 `XhsSigner`）。
    pub fn sign_state(&self) -> crate::xhs::qr::SharedSign {
        self.state.clone()
    }

    /// 构造时那份 profile 的原文（扫码对象要用同一份：UA / `webBuild` / 签名版本）。
    pub fn profile_text(&self) -> &str {
        &self.profile_text
    }

    /// `ctx.store` 的那一份（扫码要读写同一个 `xhs.b1` / `xhs.user_id` 键）。
    pub fn store_host(&self) -> Arc<dyn StoreHost> {
        self.store.clone()
    }

    // 「我是谁」

    /// `GET /api/sns/web/v2/user/me`：当前登录账号。
    ///
    /// 口径照 `XhsSource.whoami`，一行不差：
    /// - 没有 `a1` + `web_session` → **一个请求都不发**、直接 `None`（`api.hasLogin()`）；
    /// - 401 / 业务码 `-100` / `-101` → `None`（Kotlin 的 `catch (NotLoggedInException) → null`）；
    /// - 游客态（`data.guest == true`）或没有 `user_id` → `None`（[`crate::xhs::account::me_from`]）；
    /// - 成功时把 `user_id` 写进 `ctx.store` 的 `xhs.user_id`（算 `xy-direction` 用，键和 Kotlin 一样）。
    ///
    /// 别的错误（风控 / 签名 / 解析 / 网络）**照常抛**——Kotlin 也只吞 `NotLoggedInException`。
    pub fn whoami(&self, now_ms: i64, rand: &sign::SignRandom) -> Result<Option<UserRef>, BiliError> {
        if !self.has_login() {
            return Ok(None);
        }
        let built = Built {
            method: "GET",
            path: ME,
            query: Vec::new(),
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = match self.send(&built, now_ms, rand, false) {
            Ok(pair) => pair,
            Err(BiliError::NotLoggedIn { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let root = match xhs::check_http(&body, status as i32) {
            Ok(root) => root,
            Err(BiliError::NotLoggedIn { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let me = crate::xhs::account::me_from(&root);
        if let Some(user) = &me {
            self.store.put(KEY_USER_ID, Some(&user.id));
        }
        Ok(me)
    }

    /// `XhsApi.hasLogin()`：`a1` 和 `web_session` 都在（都按"非空白"算）。
    ///
    /// FFI 也导出它（纯查询，**不发请求**）：`whoami` 在没登录时一个请求都不发、也就不签名，
    /// 调用方要据此**不抽随机量**（随机流从第二个请求起才和 Kotlin 参照实现对齐）。
    pub fn has_login(&self) -> bool {
        let has = |name: &str| {
            self.cookies
                .find(WEB_URL, name)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        };
        has("a1") && has("web_session")
    }

    // -----------------------------------------------------------------------
    // 四条请求
    // -----------------------------------------------------------------------

    /// 推荐流一页：`POST /api/sns/web/v1/homefeed`（`x-rap-param` + `xy-direction`）。
    ///
    /// 游标是 `<cursor_score>|<note_index>`；第一页 `refresh_type:1`、之后 `3`，`note_index` 用上一页累加的条目数。
    /// 解析用的是 [`xhs::parse_homefeed`]（下一页游标也由它算）。
    pub fn homefeed(
        &self,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<xhs::HomefeedPage, BiliError> {
        let built = homefeed_request(cursor);
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        xhs::parse_homefeed(
            &body,
            status as i32,
            cursor_index(cursor),
            xhs::SOURCE_FEED,
            now_ms / 1000,
            &[],
        )
    }

    /// 这一次 `homefeed` 的 `x-rap-param` 要取几个补位字符（纯计算，不占状态、不发请求）。
    pub fn homefeed_pad_len(
        &self,
        cursor: Option<&str>,
        now_ms: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<usize, BiliError> {
        self.pad_len(&homefeed_request(cursor), now_ms, rap_mask, session_key)
    }

    /// 关注列表一页：`GET /api/sns/web/v1/user/followings/self`（不签名 rap / xy）。
    pub fn followings(
        &self,
        self_id: &str,
        cursor: &str,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<xhs::FollowingsPage, BiliError> {
        let built = Built {
            method: "GET",
            path: FOLLOWINGS_SELF,
            query: vec![
                ("user_id".to_string(), self_id.to_string()),
                ("cursor".to_string(), cursor.to_string()),
                ("order".to_string(), String::new()),
                ("tab_type".to_string(), "all".to_string()),
                ("source".to_string(), String::new()),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        xhs::parse_followings(&body, status as i32)
    }

    /// 谁有新笔记：`GET /api/sns/web/v1/user/followings/extra`（一批人一次，`following_user_id_list` 用逗号）。
    pub fn unread(
        &self,
        self_id: &str,
        user_ids: &[String],
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<Vec<(String, i32)>, BiliError> {
        let built = Built {
            method: "GET",
            path: FOLLOWINGS_EXTRA,
            query: vec![
                ("user_id".to_string(), self_id.to_string()),
                ("following_user_id_list".to_string(), user_ids.join(",")),
                ("tab_type".to_string(), "all".to_string()),
                ("source".to_string(), String::new()),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        xhs::parse_unread(&body, status as i32)
    }

    /// 一个作者的主页第一页：`GET /api/sns/web/v1/user_posted`（带 `x-rap-param`，不带 `xy-direction`）。
    ///
    /// `xsec_source=pc_followed`、`xsec_token` 用关注列表项里那个（会过期，所以关注列表缓存一天）。
    pub fn posted(
        &self,
        author: &xhs::FollowedAuthor,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<xhs::PostedPage, BiliError> {
        let built = posted_request(author);
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        xhs::parse_posted(&body, status as i32, author, now_ms / 1000, &[])
    }

    /// 这一次 `user_posted` 的 `x-rap-param` 要取几个补位字符（纯计算）。
    pub fn posted_pad_len(
        &self,
        author: &xhs::FollowedAuthor,
        now_ms: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<usize, BiliError> {
        self.pad_len(&posted_request(author), now_ms, rap_mask, session_key)
    }

    // 详情 / 评论 / 楼中楼

    /// 笔记详情：`POST /api/sns/web/v1/feed`（`x-rap-param` + `xy-direction`）。
    ///
    /// [`id`] 是 [`FeedItem.detail`](crate::model::DetailTarget) 里的那个
    /// `<笔记 id>@<xsec_source>@<xsec_token>`（[`xhs::parse_native_id`] 拆）；**缺 `xsec_token` 就不发请求**，
    /// 直接报"请从信息流重新打开"（和 Kotlin `XhsSource.detail` 一样，不猜、不退回不带 token 的请求）。
    ///
    /// 这三条是**用户正等着的请求**（Kotlin 那边是 `RequestPriority.INTERACTIVE`）：请求头里多一个
    /// [`hosted::PRIORITY_HEADER`]，宿主发出去之前会把它删掉、转成 OkHttp 的优先级 tag。
    ///
    /// 表情表用**Rust 里那份**（[`Self::emote_map`]：内存 → `ctx.store` 里一周内那份），
    /// **不取表、也不为表发请求**——不挡首屏：表不在手上就按原文显示记号，后台补取（下一次点开就有图）。
    pub fn detail(&self, id: &str, now_ms: i64, rand: &sign::SignRandom) -> Result<FeedDetail, BiliError> {
        let reference = xhs::parse_native_id(id);
        let built = note_request(&reference)?;
        let (status, body) = self.send(&built, now_ms, rand, true)?;
        let table = self.emote_map(now_ms);
        detail::parse_detail(&body, status as i32, &reference, &table, now_ms / 1000)
    }

    /// 详情请求的**原始响应**（状态码 + body），解析交给调用方。
    ///
    /// 为什么要分这一层：FFI 那边既要解析出 `FeedDetail`，又要把原始 body 交回 Kotlin 回填
    /// `recentNotes`（视频笔记点开之后 `play` 5 分钟内复用这一份，不再多发一个详情请求）。
    /// 判定（状态码 / 业务码）不在这里做——`parse_detail` 会照 [`xhs::check_http`] 的口径判一遍。
    pub fn detail_body(
        &self,
        id: &str,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<(u16, String), BiliError> {
        let reference = xhs::parse_native_id(id);
        let built = note_request(&reference)?;
        self.send(&built, now_ms, rand, true)
    }

    /// 这一次详情请求的 `x-rap-param` 要取几个补位字符（纯计算；和 [`Self::detail`] 用同一个 builder）。
    ///
    /// 小红书的时间 / 随机数是注入的，补位长度取决于 gzip 之后的长度，所以 Kotlin 要先问长度、
    /// 再从自己的 `Random` 里取补位字符（和推荐流那三条同一个道理）。
    pub fn detail_pad_len(
        &self,
        id: &str,
        now_ms: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<usize, BiliError> {
        self.pad_len(&note_request(&xhs::parse_native_id(id))?, now_ms, rap_mask, session_key)
    }

    // 播放

    /// 播放地址：`partId` 只认 [`play::MAIN`]；先看 [`Self::recent_note`]（详情刚取过的那份），
    /// 没有（或超过 [`NOTE_REUSE_MS`]）才用 **[`INTERACTIVE`](hosted::PRIORITY_INTERACTIVE) 优先级**现取一次详情。
    ///
    /// 口径照 Kotlin `XhsSource.play`：
    /// - `partId` 不是 `main` 时**一个请求都不发**，直接报 `Api("part")`；
    /// - 复用缓存时不发请求，也不看登录态（和 Kotlin 一样：缓存命中就走不到 `api.post`）；
    /// - 缓存没命中时，详情**成功**（状态码 + 业务码都过）才写进缓存，错误响应不缓存；
    /// - 小红书没有弹幕、也没有"按清晰度挑流"的参数：`quality` 收下但**不影响任何字节**
    ///   （Kotlin 版也是收下不用：一次把 EF4~EF7 的流全列出来，播放器自己挑）。
    ///
    /// 返回的是播放通用模型（[`PlayInfo`]），`headers` 里只有一条 `Referer`（和 Kotlin 的 `mapOf` 一样）。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        _quality: Option<i32>,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<PlayInfo, BiliError> {
        if part_id != play::MAIN {
            return Err(BiliError::Api {
                code: "part".to_string(),
                detail: format!("小红书的视频只有一个分段 main，没有 {part_id}"),
            });
        }
        let reference = xhs::parse_native_id(id);
        // 刚打开详情时取过这条笔记就直接用（`recent_note`），不再发一次详情请求。
        // 两边都只解析一次：命中缓存时是"解析存下来的 body"，没命中时 `check_http` 那次就是解析。
        let root = match self.recent_note(&reference.note_id, now_ms) {
            Some(body) => json::parse_object(&body).map_err(|detail| BiliError::Parse { detail })?,
            None => {
                let built = note_request(&reference)?;
                let (status, body) = self.send(&built, now_ms, rand, true)?;
                // 状态码 / 业务码都过才算"取到了"（和 Kotlin `api.post` 先 checkResponse 再缓存一样）
                let root = xhs::check_http(&body, status as i32)?;
                self.remember_note(&reference.note_id, &body, now_ms);
                root
            }
        };
        let card = detail::note_card(&root)?;
        play::play(&card)
    }

    // 表情表

    /// 手上有的表 → `Vec<(代码, 地址)>`（没有就是空表）。详情 / 评论 / 楼中楼解析时直接用它。
    pub fn emote_map(&self, now_ms: i64) -> Vec<(String, String)> {
        self.cached_emote_table(now_ms).map(|t| t.map).unwrap_or_default()
    }

    /// 手上已经有的那份表（内存 → `ctx.store` 里 [`emotes::TTL_SEC`] 内那份）。**不发请求。**
    ///
    /// 和 Kotlin `XhsSource.cachedEmoteTable()` 一样：内存里那份**不再看 TTL**（拿到就一直用，
    /// 直到进程重启 / 过期那份从 store 里读出来），store 里那份按取到时间算一周。
    pub fn cached_emote_table(&self, now_ms: i64) -> Option<EmoteTable> {
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(table) = &state.table {
            return Some(table.clone());
        }
        let stored = self.store.get(emotes::KEY).and_then(|t| EmoteTable::from_stored(&t))?;
        if now_ms / 1000 - stored.fetched_at_sec >= emotes::TTL_SEC {
            return None;
        }
        state.table = Some(stored.clone());
        Some(stored)
    }

    /// 下一趟 [`Self::ensure_emote_table`] 会不会真的发请求（纯查询：不取表、不改状态）。
    ///
    /// 给 Kotlin 门面用：**不发请求的那一趟不能从 `Random` 里抽随机量**（签名的取用顺序要和
    /// Kotlin 参照实现逐位对齐），所以先问这一句再决定要不要取。
    pub fn emote_fetch_needed(&self, now_ms: i64) -> bool {
        if self.cached_emote_table(now_ms).is_some() {
            return false;
        }
        let state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        if state.fetching || now_ms < state.retry_at_ms {
            return false;
        }
        // store 里那份过期了但还在：也算"要发请求"（Kotlin 会先用它顶着，然后去取新的）
        true
    }

    /// 确保表可用：**可能要发一次请求**（`GET /api/im/redmoji/detail`）。
    ///
    /// 口径照 Kotlin `XhsSource.emoteTable()`：
    /// - 内存有就直接用；store 里那份没过期也用；
    /// - [`emotes::RETRY_MS`] 内失败过就不再试，先用旧的那份（可能空）；
    /// - 同一时间**只许一个在飞**的请求，后来的人拿旧表、不重复发（风控名额很贵）；
    /// - 取到非空的表就写回 `ctx.store`（键 / 格式和 Kotlin 一样）；
    /// - **风控照常往外抛**（Kotlin 也是 rethrow），别的错误（签名 / 接口码 / 解析 / 网络 / 代理不可用）
    ///   都吞成"用旧表 + 10 分钟内不再试"（Kotlin 的 `catch (SourceException)` / `catch (IOException)`）。
    pub fn ensure_emote_table(&self, now_ms: i64, rand: &sign::SignRandom) -> Result<EmoteTable, BiliError> {
        if let Some(table) = self.cached_emote_table(now_ms) {
            return Ok(table);
        }
        // store 里过期的那份：失败时拿它顶着（Kotlin 的 `stored`）
        let stored = self.store.get(emotes::KEY).and_then(|t| EmoteTable::from_stored(&t));
        {
            let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
            if state.fetching || now_ms < state.retry_at_ms {
                return Ok(stored.unwrap_or_default());
            }
            state.fetching = true;
        }
        let result = self.fetch_emote_table(now_ms, rand);
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        state.fetching = false;
        match result {
            Ok(table) => {
                if !table.is_empty() {
                    self.store.put(emotes::KEY, Some(&table.to_stored()));
                }
                state.table = Some(table.clone());
                Ok(table)
            }
            Err(e) => {
                if matches!(e, BiliError::RiskControl { .. }) {
                    return Err(e);
                }
                state.retry_at_ms = now_ms + emotes::RETRY_MS;
                Ok(stored.unwrap_or_default())
            }
        }
    }

    /// 记下"某某时刻之前别再试表情表"（Kotlin `warmEmotes` 碰到风控时干的那一下）。
    ///
    /// 为什么单独一个口子：**风控时 [`Self::ensure_emote_table`] 照 Kotlin 原样往外抛、不动重试时间**
    /// （信息流那条路上的 `emotesFor` 就是这么做的：失败了这次刷新就失败）；后台补取那条路
    /// （`warmEmotes`）才把重试往后推 10 分钟。两条路的差别由调用方决定，核心里不猜。
    pub fn defer_emotes(&self, until_ms: i64) {
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        state.retry_at_ms = until_ms;
    }

    /// 信息流 / 搜索 / 关注流用：文字里**真有表情记号**才去取表（可能发一次请求），
    /// 返回文字里真出现过的那几项（`Emotes.used` 的口径）。
    ///
    /// 和 Kotlin `XhsSource.emotesFor(text)` 一样：没有记号就一个请求都不加；
    /// 有记号时表不在手上会**同步**取（刷新本来就在后台跑，多等一个限速槽没关系）。
    /// 风控照常往外抛（不吞）。
    pub fn emotes_for(&self, text: &str, now_ms: i64, rand: &sign::SignRandom) -> Result<Vec<(String, String)>, BiliError> {
        if !detail::has_emote_token(text) {
            return Ok(Vec::new());
        }
        let table = self.ensure_emote_table(now_ms, rand)?;
        Ok(table.used(&[Some(text)]))
    }

    /// 一次真的取表请求（`GET /api/im/redmoji/detail`，不带 rap / xy，后台优先级）。
    fn fetch_emote_table(&self, now_ms: i64, rand: &sign::SignRandom) -> Result<EmoteTable, BiliError> {
        let built = Built {
            method: "GET",
            path: emotes::PATH,
            query: Vec::new(),
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        let root = xhs::check_http(&body, status as i32)?;
        Ok(EmoteTable::from_detail(&root, now_ms / 1000))
    }

    /// 记住一份详情响应（`detail` / `play` 都往里写）。键是**笔记 id**（不是入口 id），
    /// 和 Kotlin `XhsSource.fetchNote` 一样：`recentNotes[ref.noteId] = nowMs() to json`。
    pub fn remember_note(&self, note_id: &str, body: &str, now_ms: i64) {
        let mut notes = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        notes.retain(|n| n.note_id != note_id);
        notes.push(RecentNote {
            note_id: note_id.to_string(),
            at_ms: now_ms,
            body: body.to_string(),
        });
        while notes.len() > RECENT_NOTES {
            notes.remove(0);
        }
    }

    /// 手上的那条笔记（`None` = 没有 / 超时）。读到就挪到队尾（访问序 LRU，和 Kotlin
    /// `LinkedHashMap(accessOrder = true)` 一样）。
    pub fn recent_note(&self, note_id: &str, now_ms: i64) -> Option<String> {
        let mut notes = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        let idx = notes
            .iter()
            .position(|n| n.note_id == note_id && now_ms - n.at_ms <= NOTE_REUSE_MS)?;
        let found = notes.remove(idx);
        let body = found.body.clone();
        notes.push(found);
        Some(body)
    }

    /// 一页一级评论：`GET comment/page?note_id=&cursor=&top_comment_id=&image_formats=&xsec_token=`。
    ///
    /// 游标就是响应的 `data.cursor`（不透明字符串，原样传回）；一次 10 条、每条带 1 条楼中楼预览。
    /// **这家没有排序参数**（Kotlin 的 `comments(..., sort)` 收下但不用），所以这里也不收。
    /// 缺 `xsec_token` 时在**发请求之前**报错。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<CommentPage, BiliError> {
        let reference = xhs::parse_native_id(id);
        let token = require_token(&reference)?;
        let built = Built {
            method: "GET",
            path: COMMENT_PAGE,
            query: vec![
                ("note_id".to_string(), reference.note_id.clone()),
                ("cursor".to_string(), cursor.unwrap_or("").to_string()),
                ("top_comment_id".to_string(), String::new()),
                ("image_formats".to_string(), IMAGE_FORMATS.to_string()),
                ("xsec_token".to_string(), token),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send(&built, now_ms, rand, true)?;
        let table = self.emote_map(now_ms);
        detail::parse_comments(&body, status as i32, &table)
    }

    /// 一页楼中楼：`GET comment/sub/page?note_id=&root_comment_id=&num=10&cursor=&image_formats=&top_comment_id=&xsec_token=`。
    ///
    /// 第一页 `cursor` 留空就是从第一条回复开始（含一级评论里带的那条预览），之后用响应的 `data.cursor`。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<CommentPage, BiliError> {
        let reference = xhs::parse_native_id(id);
        let token = require_token(&reference)?;
        let built = Built {
            method: "GET",
            path: SUB_COMMENT_PAGE,
            query: vec![
                ("note_id".to_string(), reference.note_id.clone()),
                ("root_comment_id".to_string(), comment_id.to_string()),
                ("num".to_string(), "10".to_string()),
                ("cursor".to_string(), cursor.unwrap_or("").to_string()),
                ("image_formats".to_string(), IMAGE_FORMATS.to_string()),
                ("top_comment_id".to_string(), String::new()),
                ("xsec_token".to_string(), token),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send(&built, now_ms, rand, true)?;
        let table = self.emote_map(now_ms);
        detail::parse_replies(&body, status as i32, &table)
    }

    // -----------------------------------------------------------------------
    // 搜索（`POST /api/sns/web/v1/search/notes`）
    // -----------------------------------------------------------------------

    /// 搜索一页：`POST /api/sns/web/v1/search/notes`（v1，edith；带 `x-rap-param`，不带 `xy-direction`）。
    ///
    /// [cursor] 是`<search_id>|<下一页页码>`：`None` = 新一轮搜索，这时用 [search_id] 按
    /// `(now_ms, random_part)` 现造一个 id、从第 1 页开始。同一个 id 在当前的所有页里**复用**
    /// （参考实现都这么做，服务端按它把一次搜索串起来）。
    ///
    /// [random_part] 是 `1..=2147483646` 的随机数，由 Kotlin 给（Rust 不取随机数）；
    /// [`Self::search_pad_len`] 要用**同一对** `(now_ms, random_part)`，否则补位长度和实际 body 对不上。
    pub fn search(
        &self,
        keyword: &str,
        cursor: Option<&str>,
        now_ms: i64,
        random_part: i64,
        rand: &sign::SignRandom,
    ) -> Result<xhs::search::SearchPage, BiliError> {
        let built = search_request(keyword, cursor, now_ms, random_part);
        let (search_id, page) = search_plan(cursor, now_ms, random_part);
        let (status, body) = self.send(&built, now_ms, rand, false)?;
        xhs::search::parse_search(&body, status as i32, page, &search_id, now_ms / 1000, &[])
    }

    /// 这一次搜索的 `x-rap-param` 要取几个补位字符（纯计算，不占状态、不发请求）。
    ///
    /// 和 [`Self::search`] 用同一个 builder：`(now_ms, random_part)` 相同 → 同一个 `search_id` → 同一个 body
    /// （补位长度取决于 gzip 之后的长度，body 变一个字节它就可能变）。
    pub fn search_pad_len(
        &self,
        keyword: &str,
        cursor: Option<&str>,
        now_ms: i64,
        random_part: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<usize, BiliError> {
        self.pad_len(
            &search_request(keyword, cursor, now_ms, random_part),
            now_ms,
            rap_mask,
            session_key,
        )
    }

    // -----------------------------------------------------------------------
    // 给 Kotlin 那边还没搬的请求签名（详情 / 评论 / 扫码 / 表情表）
    // -----------------------------------------------------------------------

    /// 签一个**已经由 Kotlin 拼好**的请求（`XhsApi.SignInterceptor` 调）。
    ///
    /// 和四条自建请求用**同一个计数**，所以一次会话里的 `seq` / `signCount` / `xraySeq` 是一条序列。
    /// 参数（a1 / loadts / webBuild / b1 / user_id）由 Kotlin 从那一次的 Cookie 罐 / store 里读好传进来
    /// （扫码登录用的是临时罐，不能这里读全局那份）。
    pub fn sign_api(
        &self,
        request: &sign::SignRequest,
        now_ms: i64,
        rand: &sign::SignRandom,
    ) -> Result<Vec<(String, String)>, BiliError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (headers, next) =
            sign::sign(&self.profile, request, now_ms, rand, *state).map_err(sign_failed)?;
        *state = next;
        Ok(headers)
    }

    /// 签名计数的当前值（测试断言它真的在走；排障也看这个）。
    pub fn state(&self) -> sign::SignState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    /// `x-rap-param` 的补位长度：拿和真发请求**同一个 builder** 拼出来算一遍（纯计算）。
    fn pad_len(
        &self,
        built: &Built,
        now_ms: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<usize, BiliError> {
        // 补位长度只取决于 api / body / 模板 / 时间 / 掩码 / 会话密钥，这几项 a1 之类填假的没关系。
        let req = built.sign_request(
            String::new(),
            0,
            String::new(),
            String::new(),
            None,
        );
        sign::rap_pad_len(&self.profile, &req, now_ms, rap_mask, session_key).map_err(sign_failed)
    }

    /// 签名 → 拼头 → 回调宿主发出去。返回（状态码, body 文本）。
    ///
    /// [`interactive`] 为 true 时在**最后**多挂一个内部伪请求头 [`hosted::PRIORITY_HEADER`]：
    /// 宿主读到它之后把它**删掉**、转成 OkHttp 的 `RequestPriority` tag（限速器让"用户正等着的请求"插队），
    /// 所以它永远不会出现在线上请求里。别的平台不传这个参数，字节一个不差。
    fn send(
        &self,
        built: &Built,
        now_ms: i64,
        rand: &sign::SignRandom,
        interactive: bool,
    ) -> Result<(u16, String), BiliError> {
        // `XhsApi.fetchRaw`：要签名的请求没有 a1 就直接报未登录，一个字节都不发。
        // Kotlin `XhsApi.fetchRaw` 用 `hasSession()`（`isNullOrBlank`）：全是空白的 a1 也算没有。
        let a1 = match self.cookies.find(WEB_URL, "a1") {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Err(BiliError::NotLoggedIn {
                    detail: Some("没有小红书的 a1 Cookie，先导入登录态".to_string()),
                })
            }
        };
        let loadts = self
            .cookies
            .find(WEB_URL, "loadts")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(now_ms - 10 * 60_000);
        let web_build = self
            .cookies
            .find(WEB_URL, "webBuild")
            // Kotlin 是 `cookie("webBuild")?.takeIf { it.isNotBlank() }`
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| self.profile.web_build.clone());
        let b1 = self.b1(loadts, now_ms);
        // Kotlin `XhsApi.userId` 是 store 里的原值（空串就是空串，`xyDirection` 会当"不知道"处理）。
        let user_id = self.store.get(KEY_USER_ID);

        let request = built.sign_request(a1, loadts, web_build, b1, user_id);
        let headers = self.sign_api(&request, now_ms, rand)?;

        // 头的顺序要照 Kotlin **最终**发出去的那一份：默认头 → 签名头 → Content-Type。
        // Content-Type 在 Kotlin 那边是 `RequestBody` 带出来的，而 BridgeInterceptor 排在应用拦截器**之后**，
        // 所以它落在签名头后面（`XhsRequestEquivalenceTest` 的 `describe()` 逐字节比着）。
        let mut all: Vec<(String, String)> = Vec::with_capacity(headers.len() + 13);
        all.extend(self.default_headers(built.no_cache()));
        all.extend(headers);
        if built.body.is_some() {
            all.push(("Content-Type".to_string(), JSON_TYPE.to_string()));
        }
        if interactive {
            // 伪请求头：宿主发出去之前会删掉（`Hosts.kt`），转成 OkHttp 的 tag。
            all.push((
                hosted::PRIORITY_HEADER.to_string(),
                hosted::PRIORITY_INTERACTIVE.to_string(),
            ));
        }

        let response = self
            .http
            .execute(HttpRequest {
                method: built.method.to_string(),
                url: built.url(&self.base),
                headers: all,
                body: built
                    .body
                    .as_ref()
                    .map(|b| b.clone().into_bytes())
                    .unwrap_or_default(),
            })
            .map_err(BiliError::from)?;
        Ok((
            response.status,
            String::from_utf8_lossy(&response.body).into_owned(),
        ))
    }

    /// 平台默认头，顺序照 Kotlin `XhsApi.defaultHeaders()`（宿主那边只补缺失的同名头，不会重复）。
    ///
    /// [`no_cache`] 是 `XhsApi.NO_CACHE_PATHS` 那一档：`cache-control` / `pragma` 排在 `priority` **之后**。
    fn default_headers(&self, no_cache: bool) -> Vec<(String, String)> {
        let p = &self.profile;
        vec![
            ("User-Agent".to_string(), p.user_agent.clone()),
            (
                "Accept".to_string(),
                "application/json, text/plain, */*".to_string(),
            ),
            ("Accept-Language".to_string(), p.accept_language.clone()),
            ("Origin".to_string(), xhs::WEB.to_string()),
            ("Referer".to_string(), WEB_URL.to_string()),
            ("sec-ch-ua".to_string(), p.sec_ch_ua.clone()),
            ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
            (
                "sec-ch-ua-platform".to_string(),
                format!("\"{}\"", p.platform),
            ),
            ("sec-fetch-dest".to_string(), "empty".to_string()),
            ("sec-fetch-mode".to_string(), "cors".to_string()),
            ("sec-fetch-site".to_string(), "same-site".to_string()),
            ("priority".to_string(), "u=1, i".to_string()),
        ]
        .into_iter()
        .chain(if no_cache {
            vec![
                ("cache-control".to_string(), "no-cache".to_string()),
                ("pragma".to_string(), "no-cache".to_string()),
            ]
        } else {
            Vec::new()
        })
        .collect()
    }

    // -----------------------------------------------------------------------
    // 宿主回调驱动的请求（关注流用：一次刷新要发 N 个请求，随机量 / 时钟在发的那一刻问宿主）
    // -----------------------------------------------------------------------

    /// 关注流的串行门（Kotlin `XhsFollowFeed.lock` 那个 `Mutex`）。返回一个守卫，出了作用域自动放开。
    pub fn follow_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.follow_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **关注流那条路上的"读一份状态"**（内存里那份；没有就从 `ctx.store` 读）。
    ///
    /// 和表情表同一个道理：调度要"读 → 改 → 写"，所以状态住在这个对象里（`Mutex<Option<Authors>>`），
    /// 不每次从 store 反序列化（那里是 gzip + base64 + 手写 JSON）。
    pub fn cached_follow_state(&self) -> Authors {
        if let Some(state) = self.cached_follow_state_opt() {
            return state;
        }
        let loaded = self.follow_store().load_authors();
        let mut slot = self.follow_state.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(loaded.clone());
        loaded
    }

    /// 手上有没有（`None` = 从没读过）。
    pub fn cached_follow_state_opt(&self) -> Option<Authors> {
        let slot = self.follow_state.lock().unwrap_or_else(|e| e.into_inner());
        slot.clone()
    }

    /// 记下内存里那份（不落盘）。`page` 的 `finally` 用它。
    pub fn store_follow_state(&self, state: Authors) {
        let mut slot = self.follow_state.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(state);
    }

    /// 平台对象里的 store 端口（关注流的存盘要用它；`Arc` 克隆一份，不新建连接 / 不占资源）。
    pub fn store_port(&self) -> Arc<dyn crate::host::StoreHost> {
        self.store.clone()
    }

    /// Cookie 罐里某个值（`HostCookies::find` 的薄包装）。
    pub fn cookie(&self, url: &str, name: &str) -> Option<String> {
        self.cookies.find(url, name)
    }

    /// `ctx.store` 里的 `xhs.user_id`（和 Kotlin `XhsApi.userId` 同一个键）。
    pub fn store_get_user_id(&self) -> Option<String> {
        self.store.get(KEY_USER_ID)
    }

    /// `followings/self` 的一页，**随机量 / 时钟由宿主回调给**（关注流用）。
    pub fn followings_driven(
        &self,
        self_id: &str,
        cursor: &str,
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
    ) -> Result<xhs::FollowingsPage, BiliError> {
        let built = Built {
            method: "GET",
            path: FOLLOWINGS_SELF,
            query: vec![
                ("user_id".to_string(), self_id.to_string()),
                ("cursor".to_string(), cursor.to_string()),
                ("order".to_string(), String::new()),
                ("tab_type".to_string(), "all".to_string()),
                ("source".to_string(), String::new()),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send_driven(&built, drive, false)?;
        xhs::parse_followings(&body, status as i32)
    }

    /// `followings/extra` 的一批（随机量 / 时钟由宿主回调给）。
    pub fn unread_driven(
        &self,
        self_id: &str,
        user_ids: &[String],
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
    ) -> Result<Vec<(String, i32)>, BiliError> {
        let built = Built {
            method: "GET",
            path: FOLLOWINGS_EXTRA,
            query: vec![
                ("user_id".to_string(), self_id.to_string()),
                (
                    "following_user_id_list".to_string(),
                    user_ids.join(","),
                ),
                ("tab_type".to_string(), "all".to_string()),
                ("source".to_string(), String::new()),
            ],
            body: None,
            rap: false,
            xy: false,
        };
        let (status, body) = self.send_driven(&built, drive, false)?;
        xhs::parse_unread(&body, status as i32)
    }

    /// 一个作者的主页第一页（**不带表情表**：调用方拿 `titles` 自己判断要不要取表、再贴回条目）。
    pub fn posted_driven_raw(
        &self,
        author: &xhs::FollowedAuthor,
        now_ms: i64,
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
    ) -> Result<xhs::PostedPage, BiliError> {
        let built = posted_request(author);
        let (status, body) = self.send_driven(&built, drive, false)?;
        xhs::parse_posted(&body, status as i32, author, now_ms / 1000, &[])
    }

    /// 信息流 / 关注流用的表情表（**随机量 / 时钟由宿主回调给**）。
    ///
    /// 和 [`Self::emotes_for`] 的区别只有一个：随机量从哪儿来。要发请求时先让宿主抽一份
    /// （`XhsSignRandomDrawer` 的顺序和 Kotlin 逐位一致），不发生请求就一个随机量都不抽。
    pub fn emotes_for_driven(
        &self,
        text: &str,
        now_ms: i64,
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
    ) -> Result<Vec<(String, String)>, BiliError> {
        if !detail::has_emote_token(text) {
            return Ok(Vec::new());
        }
        let table = self.ensure_emote_table_driven(now_ms, drive)?;
        Ok(table.used(&[Some(text)]))
    }

    /// [`Self::ensure_emote_table`] 的宿主回调版（同一套缓存 / 节流 / 降级口径）。
    fn ensure_emote_table_driven(
        &self,
        now_ms: i64,
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
    ) -> Result<EmoteTable, BiliError> {
        if let Some(table) = self.cached_emote_table(now_ms) {
            return Ok(table);
        }
        let stored = self
            .store
            .get(emotes::KEY)
            .and_then(|t| EmoteTable::from_stored(&t));
        {
            let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
            if state.fetching || now_ms < state.retry_at_ms {
                return Ok(stored.unwrap_or_default());
            }
            state.fetching = true;
        }
        let built = Built {
            method: "GET",
            path: emotes::PATH,
            query: Vec::new(),
            body: None,
            rap: false,
            xy: false,
        };
        let result = self
            .send_driven(&built, drive, false)
            .and_then(|(status, body)| {
                let root = xhs::check_http(&body, status as i32)?;
                Ok(EmoteTable::from_detail(&root, now_ms / 1000))
            });
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        state.fetching = false;
        match result {
            Ok(table) => {
                if !table.is_empty() {
                    self.store.put(emotes::KEY, Some(&table.to_stored()));
                }
                state.table = Some(table.clone());
                Ok(table)
            }
            Err(e) => {
                if matches!(e, BiliError::RiskControl { .. }) {
                    return Err(e);
                }
                state.retry_at_ms = now_ms + emotes::RETRY_MS;
                Ok(stored.unwrap_or_default())
            }
        }
    }

    /// [`Self::send`] 的宿主回调版：随机量 / 时钟在**要发出去的这一刻**问宿主。
    ///
    /// 取用顺序和 Kotlin `XhsSigner.sign` 逐位一致：
    /// `version → [xy-direction] → xray 尾巴 → b3 → (rap: 掩码 → 会话密钥 → 密钥 → 盐) → 补位长度 → 补位 → 加密耗时`。
    /// 补位长度是纯计算（gzip 之后才知道），所以宿主抽前半段、Rust 算长度、再抽后半段。
    fn send_driven(
        &self,
        built: &Built,
        drive: &crate::xhs::follow_feed::FollowDrive<'_>,
        interactive: bool,
    ) -> Result<(u16, String), BiliError> {
        let now_ms = drive.clock.now_ms();
        let a1 = match self.cookies.find(WEB_URL, "a1") {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Err(BiliError::NotLoggedIn {
                    detail: Some("没有小红书的 a1 Cookie，先导入登录态".to_string()),
                })
            }
        };
        let loadts = self
            .cookies
            .find(WEB_URL, "loadts")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(now_ms - 10 * 60_000);
        let web_build = self
            .cookies
            .find(WEB_URL, "webBuild")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| self.profile.web_build.clone());
        let b1 = self.b1(loadts, now_ms);
        let user_id = self.store.get(KEY_USER_ID);

        // `xy-direction` 的种子：`req.userId ?: req.a1`（和 `XhsSigner` 一样，空串才抽随机）
        let xy_seed = user_id.clone().unwrap_or_else(|| a1.clone());
        let head = drive
            .random
            .draw_head(built.xy, Some(xy_seed.as_str()), built.rap);
        let mut rand = sign::SignRandom {
            version: head.version,
            b3_trace_id: head.b3_trace_id,
            xray_tail: head.xray_tail,
            xy_random: head.xy_random,
            rap_mask: head.rap_mask,
            rap_session_key: head.rap_session_key,
            rap_key: head.rap_key,
            rap_salt: head.rap_salt,
            rap_pad: String::new(),
            rap_enc_time_ms: 0,
        };
        if built.rap {
            // 补位长度：和真发请求**同一个 builder**（纯计算，不占状态）
            let pad_len = self.pad_len(built, now_ms, rand.rap_mask, &rand.rap_session_key)?;
            let tail = drive.random.draw_tail(pad_len);
            rand.rap_pad = tail.rap_pad;
            rand.rap_enc_time_ms = tail.rap_enc_time_ms;
        }

        let request = built.sign_request(a1, loadts, web_build, b1, user_id);
        let headers = self.sign_api(&request, now_ms, &rand)?;
        let mut all: Vec<(String, String)> = Vec::with_capacity(headers.len() + 13);
        all.extend(self.default_headers(built.no_cache()));
        all.extend(headers);
        if built.body.is_some() {
            all.push(("Content-Type".to_string(), JSON_TYPE.to_string()));
        }
        if interactive {
            all.push((
                hosted::PRIORITY_HEADER.to_string(),
                hosted::PRIORITY_INTERACTIVE.to_string(),
            ));
        }
        let response = self
            .http
            .execute(HttpRequest {
                method: built.method.to_string(),
                url: built.url(&self.base),
                headers: all,
                body: built
                    .body
                    .as_ref()
                    .map(|b| b.clone().into_bytes())
                    .unwrap_or_default(),
            })
            .map_err(BiliError::from)?;
        Ok((
            response.status,
            String::from_utf8_lossy(&response.body).into_owned(),
        ))
    }

    /// b1：`ctx.store` 的 `xhs.b1`（长度 > 200 就直接用），没有就按 profile + 时间生成并写回。
    /// 键和格式和 Kotlin `XhsApi.b1(loadtsMs)` 完全一样，所以还没搬的请求读到的是同一份。
    fn b1(&self, loadts_ms: i64, now_ms: i64) -> String {
        if let Some(cached) = self.store.get(KEY_B1) {
            if cached.chars().count() > 200 {
                return cached;
            }
        }
        let b1 = sign::b1::encode(&self.profile, now_ms, loadts_ms);
        self.store.put(KEY_B1, Some(&b1));
        b1
    }
}

/// 签名 / 补位长度这类本地计算的失败：映射到 `FfiError::Internal`（`BiliError::Host` 就是那个出口）。
fn sign_failed(detail: String) -> BiliError {
    BiliError::Host { detail }
}

/// 推荐流的请求：`POST homefeed`，body 字段和顺序照网页实抓（`XhsSource.homefeedBody`）。
fn homefeed_request(cursor: Option<&str>) -> Built {
    let (score, index) = split_cursor(cursor);
    let refresh_type = if cursor.is_none() { 1 } else { 3 };
    Built {
        method: "POST",
        path: HOMEFEED,
        query: Vec::new(),
        body: Some(homefeed_body(&score, refresh_type, index)),
        rap: true,
        xy: true,
    }
}

/// `user_posted` 的请求：参数顺序照 Kotlin `XhsFollowFeed.fetchDue`（num → cursor → user_id → image_formats
/// → xsec_token → xsec_source），`cursor` 固定空串（关注流只拉每个作者的第一页）。
fn posted_request(author: &xhs::FollowedAuthor) -> Built {
    Built {
        method: "GET",
        path: USER_POSTED,
        query: vec![
            ("num".to_string(), POSTED_NUM.to_string()),
            ("cursor".to_string(), String::new()),
            ("user_id".to_string(), author.id.clone()),
            ("image_formats".to_string(), IMAGE_FORMATS.to_string()),
            (
                "xsec_token".to_string(),
                author.xsec_token.clone().unwrap_or_default(),
            ),
            ("xsec_source".to_string(), xhs::SOURCE_FOLLOWED.to_string()),
        ],
        body: None,
        rap: true,
        xy: false,
    }
}

/// 详情的请求：`POST /api/sns/web/v1/feed`，body 字段和顺序照网页实抓（Kotlin `XhsSource.feedBody`）。
///
/// **入口里没有 `xsec_token` 也照发**：Kotlin 的 `detail` 本来就不校验 token（只有评论 / 楼中楼校验），
/// 空 token 交上去由平台回 `300031`「当前笔记暂时无法浏览」，用户看到的是平台给的原因。
/// 这里要是提前拦，错误类型就从 `Api("300031")` 变成 `Api("xsec_token")`，对拍立刻对不上。
fn note_request(reference: &xhs::NoteRef) -> Result<Built, BiliError> {
    Ok(Built {
        method: "POST",
        path: NOTE_FEED,
        query: Vec::new(),
        body: Some(note_body(
            &reference.note_id,
            reference.xsec_source.as_deref().unwrap_or(xhs::SOURCE_FEED),
            reference.xsec_token.as_deref().unwrap_or(""),
        )),
        rap: true,
        xy: true,
    })
}

/// 评论 / 楼中楼都要这篇笔记的 `xsec_token`：入口里没有就在**发请求之前**报错（和 Kotlin 同一句文案）。
///
/// **只给评论 / 楼中楼用**：详情不校验 token（见 [`note_request`]）。
fn require_token(reference: &xhs::NoteRef) -> Result<String, BiliError> {
    match reference.xsec_token.as_deref() {
        Some(token) if !token.is_empty() => Ok(token.to_string()),
        _ => Err(BiliError::Api {
            code: "xsec_token".to_string(),
            detail: "评论要带这篇笔记的 xsec_token，入口里没有（很久以前收藏的？），请从信息流重新打开".to_string(),
        }),
    }
}

/// `feedBody` 的移植（逐字节：`XhsCodec.jsonString` 的转义在 `codec::json_string`）。
fn note_body(note_id: &str, xsec_source: &str, xsec_token: &str) -> String {
    format!(
        "{{\"source_note_id\":{},\"image_formats\":[\"jpg\",\"webp\",\"avif\"],\
\"extra\":{{\"need_body_topic\":\"1\"}},\"xsec_source\":{},\"xsec_token\":{}}}",
        codec::json_string(note_id),
        codec::json_string(xsec_source),
        codec::json_string(xsec_token),
    )
}

/// 搜索的请求：`POST /api/sns/web/v1/search/notes`（体见 [`xhs::search::search_body`]；
/// `x-rap-param` 要带，`xy-direction` 不带——`XhsApi.XY_PATHS` 里只有 homefeed / feed）。
fn search_request(keyword: &str, cursor: Option<&str>, now_ms: i64, random_part: i64) -> Built {
    let (search_id, page) = search_plan(cursor, now_ms, random_part);
    Built {
        method: "POST",
        path: xhs::search::SEARCH_NOTES,
        query: Vec::new(),
        body: Some(xhs::search::search_body(keyword, page, &search_id)),
        rap: true,
        xy: false,
    }
}

/// 这一页的 `(search_id, page)`：给了游标就照游标（同一轮搜索复用同一个 id），
/// 没给就是新一轮——按 `(now_ms, random_part)` 现造 id、从第 1 页开始。
///
/// 空串游标（cli / 别的入口可能给）当"没有游标"，和 Kotlin `XhsSearchCursor.decode` 的口径一致。
fn search_plan(cursor: Option<&str>, now_ms: i64, random_part: i64) -> (String, i32) {
    match cursor.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => xhs::search::parse_cursor(c),
        None => (xhs::search::search_id(now_ms, random_part), 1),
    }
}

/// `homefeedBody` 的移植（逐字节：`XhsCodec.jsonString` 的转义在 `codec::json_string`）。
fn homefeed_body(cursor_score: &str, refresh_type: i32, note_index: i32) -> String {
    format!(
        "{{\"cursor_score\":{},\"num\":39,\"refresh_type\":{refresh_type},\"note_index\":{note_index},\
\"unread_begin_note_id\":\"\",\"unread_end_note_id\":\"\",\"unread_note_count\":0,\
\"category\":\"homefeed_recommend\",\"search_key\":\"\",\"need_num\":14,\
\"image_formats\":[\"jpg\",\"webp\",\"avif\"],\"need_filter_image\":false}}",
        codec::json_string(cursor_score)
    )
}

/// 游标 `<cursor_score>|<note_index>` 拆两段，口径照 Kotlin：
/// `substringBefore('|')` / `substringAfter('|', "0").toIntOrNull() ?: 0`。
fn split_cursor(cursor: Option<&str>) -> (String, i32) {
    match cursor {
        None => (String::new(), 0),
        Some(c) => {
            let mut parts = c.splitn(2, '|');
            let score = parts.next().unwrap_or_default().to_string();
            let index = parts
                .next()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            (score, index)
        }
    }
}

fn cursor_index(cursor: Option<&str>) -> i32 {
    split_cursor(cursor).1
}

