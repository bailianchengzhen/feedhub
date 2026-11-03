//! 知乎 Source 的**平台层**：请求怎么拼（URL / 参数 / 头 / 签名 / 翻页游标 / 没 `d_c0` 时先领一个）、
//! 错误怎么判、响应该交给谁解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-zhihu/ZhihuSource.kt` 的同名方法（`feed` / `ensureDc0` / `sign` / `pageUrl`）：
//! 方法 + URL + 参数顺序 + 请求头顺序 + body 必须和 Kotlin 版**逐字节一样**，
//! `ZhihuRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 和 B站 那份的差别：
//! 1. **没有 `now_sec`，只有 `seed`**：知乎这两条流的 URL 里没有时间戳；需要外部注入的是 `x-zse-96` 的随机 seed
//!    （网页是 `Math.random() * 127`，由 Kotlin 生成后传进来）。
//! 2. **没有 `HostStore`**：知乎没有 WBI key 这类要缓存的设备参数，只读 Cookie 罐。
//! 3. **平台默认头也在这里写**（[`platform_headers`]，和 B站 一样）：宿主客户端的 `DefaultHeadersInterceptor`
//!    只补**缺失**的同名头，所以两边最终顺序一致。为了让 MockWebServer（localhost）下 Kotlin 版也吃同一批默认头，
//!    `ZhihuSource` 的 hostFilter 里加了 `|| it == baseUrl.host`（照 B站 的写法；线上是 `www.zhihu.com`，
//!    那个条件本来就是 true，行为不变）。
//!
//! 当前只搬了两个流；详情 / 评论 / 搜索 / 播放 / 扫码还在 Kotlin，`RustZhihuSource` 逐方法委托过去。

use std::sync::{Arc, Mutex};

use crate::detail::{Comment, CommentPage, FeedDetail};
use crate::host::{CookieHost, HttpHost, HttpRequest, HttpResponse};
use crate::hosted::{header_value, query_encode};
use crate::json::{self, JsonMap};
use crate::model::{BiliError as ZhihuError, Page, UserRef};
use crate::play::PlayInfo;
use crate::zhihu::{self, links, sign};

/// 线上接口根（和 `ZhihuSource.WWW` 一样）。测试里换成 MockWebServer 的地址。
pub const WWW: &str = "https://www.zhihu.com";

/// 网页 fetch 的 `Accept`（Kotlin `get()` 里写死的那一个）。
const ACCEPT_JSON: &str = "application/json, text/plain, */*";

/// 和 `core` 的 `UserAgents.DESKTOP_CHROME` **逐字节一样**。
///
/// 为什么 Rust 里再写一份：平台默认头属于"请求怎么发"，归 Rust；宿主客户端的
/// `DefaultHeadersInterceptor` 留着当兜底（它只补**缺失**的同名头，不会改已有的），
/// 两边漂移会被 `ZhihuRequestEquivalenceTest` 逐字节比对抓住。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const REFERER: &str = "https://www.zhihu.com/";
const ACCEPT_ANY: &str = "*/*";
const ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9";
const X_API_VERSION: &str = "3.0.91";
const X_APP_ZA: &str = "OS=Web";

/// 推荐流第一页的参数，照电脑版首页（Kotlin `RECOMMEND_FIRST`）。
const RECOMMEND_FIRST: &str = "desktop=true&page_number=1&limit=6&action=down&after_id=-1&ad_interval=-10";
/// 关注动态第一页（Kotlin `MOMENTS_FIRST`）。
const MOMENTS_FIRST: &str = "desktop=true&limit=10";

const RECOMMEND_PATH: &str = "/api/v3/feed/topstory/recommend";
const MOMENTS_PATH: &str = "/api/v3/moments";

/// `/udid` 的 body 和 Content-Type（Kotlin `EMPTY_JSON`）。
const EMPTY_JSON: &[u8] = b"{}";
const JSON_CONTENT_TYPE: &str = "application/json;charset=UTF-8";

/// Zhihu++ `EmojiManager` 用的默认表情组（Kotlin `STICKER_GROUP`）。
const STICKER_GROUP: &str = "1114161698310770688";

/// 条目类型 → 详情 / 评论接口里的集合名（Kotlin `COLLECTION`）。
fn collection(kind: &str) -> Option<&'static str> {
    match kind {
        "answer" => Some("answers"),
        "article" => Some("articles"),
        "pin" => Some("pins"),
        "zvideo" => Some("zvideos"),
        "question" => Some("questions"),
        _ => None,
    }
}

/// 详情接口的 `include`（Kotlin `INCLUDE`）：只要解析用得上的字段。
fn include_of(kind: &str) -> &'static str {
    match kind {
        "answer" => "content,excerpt,voteup_count,comment_count,favlists_count,visited_count,created_time,updated_time,ip_info,question.topics",
        "article" => "content,topics,excerpt,voteup_count,comment_count,favlists_count,visited_count,ip_info",
        "pin" => "topics",
        "question" => "read_count,visit_count,answer_count,voteup_count,comment_count,follower_count,detail,excerpt,author,topics",
        _ => "",
    }
}

/// 评论排序（和 `CommentSort` 一一对应）。知乎只有两档：`NEWEST` 是 `ts`，别的都是 `score`（热门）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSort {
    Default,
    Hot,
    Newest,
}

impl CommentSort {
    /// 回答列表的排序：知乎只有"默认"和"按时间"（`updated`）。
    fn answers_order(self) -> &'static str {
        if self == CommentSort::Newest {
            "updated"
        } else {
            "default"
        }
    }

    fn order_by(self) -> &'static str {
        if self == CommentSort::Newest {
            "ts"
        } else {
            "score"
        }
    }
}

/// 推荐流 / 关注动态（和 `FeedKind` 的子集一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

/// 知乎的平台对象：持有宿主端口和表情表，**自己的状态只有一个锁**（`d_c0` 的"只领一次"）。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustZhihuHosted`）建一次、跟着 Source 活；Source 被 App 缓存到进程结束。
/// 没有后台线程、没有定时器、没有连接池——所以"闲置时零唤醒"。
pub struct ZhihuHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    /// lens（视频地址接口）的根。线上是 [`super::play::LENS`]；测试里换成 MockWebServer 的地址
    /// （Kotlin 那边是 `ZhihuSource(lensBase = …)` 的构造参数）。
    lens_base: String,
    /// **播放地址（lens）那一条**用的宿主端口：Kotlin 那边是另一个客户端
    /// （`ctx.client(platform).cookieJar(NO_COOKIES)` + UA + 600ms 限速）。
    ///
    /// 不给就是 `None`（老调用方 / 测试）：构造时传进来就多一条"匿名取流"的路，`play` 才会发请求。
    /// 为什么不复用 `http`：那条挂着 Cookie 罐，匿名接口不该收到登录凭据。
    lens_http: Option<Arc<dyn HttpHost>>,
    cookies: Arc<dyn CookieHost>,
    /// 占位符（`[感谢]`）→ 图片地址。表的所有权在插件（`ZhihuSticker.urls`），Rust 不复制那套命名。
    emotes: Vec<(String, String)>,
    /// `d_c0`：只让一个线程去领（和 Kotlin 的 `dc0Mutex` 一样）。
    dc0: Mutex<()>,
    /// 表情表（占位符 → 图片地址，图片是 data URI）：**第一次真要换表情时才取一次**，
    /// 取不到（接口错误）就记住一张空表，不再重试（Kotlin `ZhihuSource.stickers` 的语义）。
    stickers: Mutex<Option<Vec<(String, String)>>>,
    /// 点开过的回答：回答 id → (问题 id, 这条回答的"楼")。[`qa_detail`](Self::qa_detail) 记下，
    /// [`qa_answers`](Self::qa_answers) 第一页拿它置顶、不用再取一次回答。最近 [`ANSWER_MEMO`] 条。
    answers: Mutex<Vec<(String, String, Comment)>>,
    /// 信息流卡片走问题版面（见 [`Self::set_question_cards`]）。
    question_cards: std::sync::atomic::AtomicBool,
}

/// 记几条点开过的回答（只是省一次请求：没记着就再取一次回答）。
const ANSWER_MEMO: usize = 32;

/// 回答列表一页几条（网页是 3～5；每条是整篇回答，一页不宜多）。
const ANSWERS_PAGE: usize = 5;

/// 回答列表要的字段（`feeds` 接口；网页那串的子集）。
const ANSWERS_INCLUDE: &str = "data[*].is_normal,admin_closed_comment,comment_count,can_comment,content,voteup_count,created_time,updated_time,question,excerpt,is_sticky";

impl ZhihuHostedSource {
    pub fn new(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        cookies: Arc<dyn CookieHost>,
        emotes: Vec<(String, String)>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            lens_base: super::play::LENS.to_string(),
            lens_http: None,
            cookies,
            emotes,
            dc0: Mutex::new(()),
            stickers: Mutex::new(None),
            answers: Mutex::new(Vec::new()),
            question_cards: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 同 [`new`](Self::new)，另外给一条**匿名**的 HTTP 端口（播放地址走它，不带 Cookie）。
    pub fn with_lens(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        lens_http: Arc<dyn HttpHost>,
        cookies: Arc<dyn CookieHost>,
        emotes: Vec<(String, String)>,
    ) -> Self {
        let mut source = Self::new(base, http, cookies, emotes);
        source.lens_http = Some(lens_http);
        source
    }

    /// 换掉 lens 的根（测试指向 MockWebServer；生产用默认的 [`super::play::LENS`]）。
    pub fn with_lens_base(mut self, lens_base: impl Into<String>) -> Self {
        self.lens_base = lens_base.into();
        self
    }

    /// lens 的根（排障 / 测试断言用）。
    pub fn lens_base(&self) -> &str {
        &self.lens_base
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 插件带的表情表（占位符 → 资源地址）。搜索的卡片标题 / 摘要也要贴表情，所以同模块的
    /// `zhihu/search.rs` 用它；**正文**的表情表是另一份（[`Self::body_emotes`]，接口给的 data URI）。
    pub(crate) fn platform_emotes(&self) -> Vec<(String, String)> {
        self.emotes.clone()
    }

    /// 取 Cookie 用的地址：和 Kotlin 的 `baseUrl` 一样是 `base + "/"`。
    fn base_url(&self) -> String {
        format!("{}/", self.base)
    }

    /// 一页。`seed` 是 `x-zse-96` 的明文首字节（0..=255），由调用方注入——
    /// 测试里固定它，两次运行的请求才能逐字节比。
    pub fn feed(&self, kind: FeedKind, cursor: Option<&str>, seed: u8) -> Result<Page, ZhihuError> {
        // 两个流都要登录。没登录时实测：推荐流 401（code 101），领了 d_c0 再签名请求则是 403 / 40352 人机验证。
        // 所以没有 z_c0 就不发请求，免得白白触发风控（Kotlin 版同一行）。
        if self.cookies.find(&self.base_url(), "z_c0").is_none() {
            return Err(ZhihuError::NotLoggedIn { detail: None });
        }
        let (path, first) = match kind {
            FeedKind::Recommend => (RECOMMEND_PATH, RECOMMEND_FIRST),
            FeedKind::Following => (MOMENTS_PATH, MOMENTS_FIRST),
        };
        let (url, path_and_query) = self.page_url(path, first, cursor);
        self.ensure_dc0()?;
        let response = self.get(&url, &path_and_query, seed)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let location = header(&response, "location");
        let code = response.status as i32;
        let mut page = match kind {
            FeedKind::Recommend => {
                zhihu::parse_recommend(&body, code, location.as_deref(), &self.emotes)
            }
            FeedKind::Following => {
                zhihu::parse_moments(&body, code, location.as_deref(), &self.emotes)
            }
        }?;
        if self.question_cards.load(std::sync::atomic::Ordering::Relaxed) {
            zhihu::as_question_cards(&body, &mut page.items, &self.emotes);
        }
        Ok(page)
    }

    /// 信息流卡片走问题版面（[`zhihu::as_question_cards`]）。生产上宿主建好就打开；对拍参照测试不开。
    pub fn set_question_cards(&self, on: bool) {
        self.question_cards.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// 第一页用固定参数；之后用上一页 `paging.next` 的 query（游标里只存 query，路径和域名固定，
    /// 免得游标被改成别的地址把 Cookie 带出去）。返回 `(完整 URL, 签名原文里的 path?query)`。
    fn page_url(&self, path: &str, first: &str, cursor: Option<&str>) -> (String, String) {
        let query = canonical_query(cursor.unwrap_or(first));
        let path_and_query = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        };
        (
            format!("{}{path_and_query}", self.base),
            path_and_query,
        )
    }

    /// 带签名的 GET：`Accept` 先写，再按 Kotlin `sign()` 的顺序补 `x-zse-93` / `x-zse-96` / `x-requested-with`。
    /// 没有 `d_c0` 就原样发出（由服务端决定收不收），和 Kotlin 一样。
    ///
    /// **同模块的 `zhihu/search.rs` 也走它**（搜索和两条流是同一套签名 / 头顺序 / 默认头）。
    pub(crate) fn get(
        &self,
        url: &str,
        path_and_query: &str,
        seed: u8,
    ) -> Result<HttpResponse, ZhihuError> {
        let headers = api_headers(url, path_and_query, self.cookies.find(url, "d_c0").as_deref(), seed);
        self.execute(HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers,
            body: Vec::new(),
        })
    }

    // 账号：我是谁

    /// `whoami`：没有 `z_c0` 就是没登录（一个请求都不发）；`GET /api/v4/me` 401 也是 `None`。
    /// `ZhihuParser.me(checked(res))`：先按错误映射，`NotLoggedIn` 折成 `None`。
    pub fn whoami(&self, seed: u8) -> Result<Option<UserRef>, ZhihuError> {
        let base_url = self.base_url();
        if self.cookies.find(&base_url, "z_c0").is_none() {
            return Ok(None);
        }
        let path = "/api/v4/me";
        let url = format!("{}{path}", self.base);
        let response = self.get(&url, path, seed)?;
        if response.status == 401 {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let location = header(&response, "location");
        match zhihu::check_http(response.status as i32, &body, location.as_deref()) {
            Ok(json) => Ok(zhihu::user(&json)),
            Err(ZhihuError::NotLoggedIn { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 扫码登录要用的身份 URL（`https://www.zhihu.com/`）。
    pub fn base_url_text(&self) -> String {
        self.base_url()
    }

    /// `/udid` 的请求（第一次访问时领 `d_c0`）。扫码流程里也用同一份（临时罐没有 `d_c0` 时）。
    pub fn udid_request(&self) -> HttpRequest {
        let base_url = self.base_url();
        let mut headers = vec![
            ("Origin".to_string(), self.base.clone()),
            ("x-requested-with".to_string(), "fetch".to_string()),
        ];
        if let Some(xsrf) = self.cookies.find(&base_url, "_xsrf") {
            headers.push(("x-xsrftoken".to_string(), xsrf));
        }
        headers.extend(platform_headers(true));
        headers.push(("Content-Type".to_string(), JSON_CONTENT_TYPE.to_string()));
        HttpRequest {
            method: "POST".to_string(),
            url: format!("{}/udid", self.base),
            headers,
            body: EMPTY_JSON.to_vec(),
        }
    }

    /// 没有 `d_c0` 时 `POST /udid` 领一个（网页首次访问也是这么做的），服务端用 Set-Cookie 种进 Cookie 罐
    /// （那一步在宿主的 OkHttp 里做，这里不用 `CookieHost::put`）。
    /// 没领到就算了（后面的请求不签名）；但风控之类的错误照常抛（Kotlin `ensureDc0` 同一行）。
    pub(crate) fn ensure_dc0(&self) -> Result<(), ZhihuError> {
        let _guard = self.dc0.lock().unwrap_or_else(|e| e.into_inner());
        let base_url = self.base_url();
        if self.cookies.find(&base_url, "d_c0").is_some() {
            return Ok(());
        }
        let mut headers = vec![
            ("Origin".to_string(), self.base.clone()),
            ("x-requested-with".to_string(), "fetch".to_string()),
        ];
        if let Some(xsrf) = self.cookies.find(&base_url, "_xsrf") {
            headers.push(("x-xsrftoken".to_string(), xsrf));
        }
        headers.extend(platform_headers(true));
        // 媒体类型在这里写成头：宿主接口只有 body 字节，没有 content-type 字段。
        // 放在所有请求级头之后，和 Kotlin 那边 BridgeInterceptor 补的位置一样（逐字节等价门守着）。
        headers.push(("Content-Type".to_string(), JSON_CONTENT_TYPE.to_string()));
        let response = self.execute(HttpRequest {
            method: "POST".to_string(),
            url: format!("{}/udid", self.base),
            headers,
            body: EMPTY_JSON.to_vec(),
        })?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        // 这里只要"有没有错误"，不要求 body 是 JSON（`/udid` 的 200 经常是空 body）。
        match zhihu::check_error(response.status as i32, &body, None) {
            None => Ok(()),
            Some(ZhihuError::NotLoggedIn { .. }) => Ok(()),
            Some(e) => Err(e),
        }
    }

    // 详情 / 评论 / 楼中楼

    /// 详情：`GET /api/v4/{answer,article,pin,zvideo,question}/{id}`，`include` 照 Kotlin 那张表。
    /// 想法正文里的表情要表情表（第一次用时取一次，取不到就留文字）。
    pub fn detail(&self, id: &str, seed: u8) -> Result<FeedDetail, ZhihuError> {
        let (kind, number) = links::split_id(id).ok_or_else(|| bad_id(id))?;
        let json = self.object_json(&kind, &number, id, seed)?;
        let emotes = if kind == "pin" && zhihu::comment::has_emote_candidates(&json) {
            self.body_emotes(seed)?
        } else {
            Vec::new()
        };
        zhihu::detail::detail(&kind, &json, &emotes, &self.emotes)
            .map_err(|detail| ZhihuError::Parse { detail })
    }

    /// `GET /api/v4/{集合}/{id}?include=…`（详情那张 include 表）。
    fn object_json(&self, kind: &str, number: &str, id: &str, seed: u8) -> Result<JsonMap, ZhihuError> {
        let collection = collection(kind).ok_or_else(|| bad_id(id))?;
        let path = format!("/api/v4/{collection}/{number}");
        let include = include_of(kind);
        let path_and_query = if include.is_empty() {
            path.clone()
        } else {
            // Kotlin 用 OkHttp 的 `addQueryParameter`（口径见 `hosted::query_encode`）
            format!("{path}?include={}", query_encode(include))
        };
        let url = format!("{}{path_and_query}", self.base);
        self.get_json(&url, &path_and_query, seed)
    }

    // 问题 + 回答楼层

    /// 详情的**问题版面**：回答 / 问题两种 id 走这里，别的类型就是 [`detail`](Self::detail)。
    ///
    /// 点回答进来是**两个请求**：先取这条回答（拿问题 id、记下置顶用的那一楼），再取问题
    /// （标题 / 描述 / 提问者 / 计数）；条目的身份还是这条回答（[`zhihu::qa::question_view`]）。
    /// 任何一个请求出错（风控 / 未登录 / 签名）都原样抛出，不接着发第二个。
    pub fn qa_detail(&self, id: &str, seed: u8) -> Result<FeedDetail, ZhihuError> {
        let (kind, number) = links::split_id(id).ok_or_else(|| bad_id(id))?;
        match kind.as_str() {
            "answer" => {
                let answer = self.object_json("answer", &number, id, seed)?;
                let (qid, _) = self.remember_answer(&number, &answer)?;
                let item = zhihu::detail::detail("answer", &answer, &[], &self.emotes)
                    .map_err(|detail| ZhihuError::Parse { detail })?
                    .item;
                let question = self.object_json("question", &qid, id, next_seed(seed))?;
                zhihu::qa::question_view(&question, &self.emotes, Some(&item))
                    .map_err(|detail| ZhihuError::Parse { detail })
            }
            "question" => {
                let question = self.object_json("question", &number, id, seed)?;
                zhihu::qa::question_view(&question, &self.emotes, None)
                    .map_err(|detail| ZhihuError::Parse { detail })
            }
            _ => self.detail(id, seed),
        }
    }

    /// 评论区的**回答列表**：`GET /api/v4/questions/{qid}/feeds`（回答 / 问题两种 id；别的类型就是
    /// [`comments`](Self::comments)）。第一页把推过来的那条回答置顶，后面各页里再出现就去掉。
    /// 排序：默认 / 热门是 `default`，最新是 `updated`；翻页用 `paging.next` 的 query（里面已带排序）。
    pub fn qa_answers(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        seed: u8,
    ) -> Result<CommentPage, ZhihuError> {
        let (kind, number) = links::split_id(id).ok_or_else(|| bad_id(id))?;
        let (qid, pinned, seed) = match kind.as_str() {
            "answer" => {
                let (qid, floor, used) = self.answer_context(&number, id, seed)?;
                (qid, Some(floor), if used { next_seed(seed) } else { seed })
            }
            "question" => (number, None, seed),
            _ => return self.comments(id, cursor, sort, seed),
        };
        let path = format!("/api/v4/questions/{qid}/feeds");
        let first = format!(
            "include={}&limit={ANSWERS_PAGE}&offset=0&order={}&platform=desktop",
            query_encode(ANSWERS_INCLUDE),
            sort.answers_order()
        );
        let (url, path_and_query) = self.page_url(&path, &first, cursor);
        let json = self.get_json(&url, &path_and_query, seed)?;
        let mut page = zhihu::qa::answers_page(&json, pinned.as_ref().map(|c| c.id.as_str()))
            .map_err(|detail| ZhihuError::Parse { detail })?;
        if cursor.is_none() {
            if let Some(p) = pinned {
                page.items.insert(0, p);
            }
        }
        Ok(page)
    }

    /// 楼中楼面板：楼 id 是 `answer:<id>`（一条回答）时是**这条回答的评论**
    /// （`root_comment`，热门排序，一级评论和它的子评论预览摊平成一列）；别的就是 [`replies`](Self::replies)。
    pub fn qa_replies(
        &self,
        comment_id: &str,
        cursor: Option<&str>,
        seed: u8,
    ) -> Result<CommentPage, ZhihuError> {
        let Some(answer) = comment_id.strip_prefix("answer:") else {
            return self.replies(comment_id, cursor, seed);
        };
        if answer.is_empty() || !answer.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad_id(comment_id));
        }
        let path = format!("/api/v4/comment_v5/answers/{answer}/root_comment");
        let first = format!("order_by={}", CommentSort::Default.order_by());
        let (url, path_and_query) = self.page_url(&path, &first, cursor);
        let json = self.get_json(&url, &path_and_query, seed)?;
        let emotes = self.emotes_for(&json, seed)?;
        zhihu::comment::comments(&json, &emotes)
            .map(zhihu::qa::flatten)
            .map_err(|detail| ZhihuError::Parse { detail })
    }

    /// 记下一条回答（问题 id + 置顶用的楼），返回问题 id 和那一楼。
    fn remember_answer(&self, number: &str, answer: &JsonMap) -> Result<(String, Comment), ZhihuError> {
        let qid = json::obj(answer, "question")
            .and_then(|q| json::string(q, "id"))
            .ok_or_else(|| ZhihuError::Parse { detail: format!("回答缺少所属问题（id={number}）") })?;
        let mut floor = zhihu::qa::answer_floor(answer)
            .ok_or_else(|| ZhihuError::Parse { detail: format!("回答缺少 id（id={number}）") })?;
        floor.pinned = true;
        let mut memo = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        memo.retain(|(a, _, _)| a != number);
        memo.push((number.to_string(), qid.clone(), floor.clone()));
        let excess = memo.len().saturating_sub(ANSWER_MEMO);
        memo.drain(..excess);
        Ok((qid, floor))
    }

    /// 问题 id + 置顶那一楼：记着就直接用，没记着（详情走了缓存、进程重启过）就取一次回答。
    /// 第三个值是"这次发没发请求"（发了的话下一个请求换一个签名随机字节）。
    fn answer_context(&self, number: &str, id: &str, seed: u8) -> Result<(String, Comment, bool), ZhihuError> {
        let known = {
            let memo = self.answers.lock().unwrap_or_else(|e| e.into_inner());
            memo.iter().find(|(a, _, _)| a == number).map(|(_, q, f)| (q.clone(), f.clone()))
        };
        if let Some((qid, floor)) = known {
            return Ok((qid, floor, false));
        }
        let answer = self.object_json("answer", number, id, seed)?;
        let (qid, floor) = self.remember_answer(number, &answer)?;
        Ok((qid, floor, true))
    }

    /// 一级评论：`GET /api/v4/comment_v5/{answers,articles,…}/{id}/root_comment`。
    /// `order_by`：`DEFAULT` / `HOT` 是 `score`（热门），`NEWEST` 是 `ts`；翻页用游标（`paging.next` 的 query，里面已带排序）。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        seed: u8,
    ) -> Result<CommentPage, ZhihuError> {
        let (kind, number) = links::split_id(id).ok_or_else(|| bad_id(id))?;
        let collection = collection(&kind).ok_or_else(|| bad_id(id))?;
        let path = format!("/api/v4/comment_v5/{collection}/{number}/root_comment");
        let first = format!("order_by={}", sort.order_by());
        let (url, path_and_query) = self.page_url(&path, &first, cursor);
        let json = self.get_json(&url, &path_and_query, seed)?;
        let emotes = self.emotes_for(&json, seed)?;
        zhihu::comment::comments(&json, &emotes).map_err(|detail| ZhihuError::Parse { detail })
    }

    /// 楼中楼：`GET /api/v4/comment_v5/comment/{commentId}/child_comment`。
    /// `commentId` 是一级评论的 id（它所在的内容用不上，留着只为和别的平台签名一致）。
    pub fn replies(
        &self,
        comment_id: &str,
        cursor: Option<&str>,
        seed: u8,
    ) -> Result<CommentPage, ZhihuError> {
        if comment_id.is_empty() || !comment_id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad_id(comment_id));
        }
        let path = format!("/api/v4/comment_v5/comment/{comment_id}/child_comment");
        let (url, path_and_query) = self.page_url(&path, "", cursor);
        let json = self.get_json(&url, &path_and_query, seed)?;
        let emotes = self.emotes_for(&json, seed)?;
        zhihu::comment::comments(&json, &emotes).map_err(|detail| ZhihuError::Parse { detail })
    }

    /// **播放地址**（Kotlin `ZhihuSource.play`）：`GET {lens}/api/v4/videos/{video_id}`，匿名、
    /// 不带 Cookie、不带签名；地址带签名（约一小时过期），所以每次播放都现取。
    ///
    /// [`part_id`] 是 lend 视频 id（zvideo 的 `video.video_id`，或正文视频盒子的 `data-lens-id`）。
    /// 写法不对（空 / 不是数字）在**发请求之前**就抛 `bad_id`（和 Kotlin 的 `badId` 一字不差）。
    ///
    /// 状态码：`403` / `429` → 风控（"视频地址接口拒绝了请求"），`404` → `Api("HTTP 404")`
    /// （"视频不存在或已删除"），其余非 2xx → `Api("HTTP <码>")` 带 body 前 200 个字符（照 Kotlin）。
    pub fn play(&self, part_id: &str) -> Result<PlayInfo, ZhihuError> {
        super::play::check_part(part_id)?;
        let Some(http) = self.lens_http.clone() else {
            return Err(ZhihuError::Host {
                detail: "这个平台对象没有匿名取流端口（`ZhihuHostedSource::with_lens`）".to_string(),
            });
        };
        let url = super::play::url_at(&self.lens_base, part_id);
        let response = http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: super::play::headers(),
                body: Vec::new(),
            })
            .map_err(ZhihuError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        match response.status {
            200..=299 => {}
            403 | 429 => {
                return Err(ZhihuError::RiskControl {
                    code: Some(format!("HTTP {}", response.status)),
                    detail: "视频地址接口拒绝了请求".to_string(),
                })
            }
            404 => {
                return Err(ZhihuError::Api {
                    code: "HTTP 404".to_string(),
                    detail: "视频不存在或已删除".to_string(),
                })
            }
            code => {
                return Err(ZhihuError::Api {
                    code: format!("HTTP {code}"),
                    detail: json::take_chars(&body, 200),
                })
            }
        }
        let json = json::parse_object(&body).map_err(|detail| ZhihuError::Parse { detail })?;
        super::play::play(&json)
    }

    /// `ensureDc0()` + 带签名的 GET + 错误 / 解析（Kotlin 的 `getJson`）。
    fn get_json(&self, url: &str, path_and_query: &str, seed: u8) -> Result<JsonMap, ZhihuError> {
        self.ensure_dc0()?;
        let response = self.get(url, path_and_query, seed)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        let location = header(&response, "location");
        zhihu::check_http(response.status as i32, &body, location.as_deref())
    }

    /// 只有评论 / 想法文字里真有 `[xxx]` 的时候才去取表情表（Kotlin `emotesFor`）。
    pub(crate) fn emotes_for(&self, json: &JsonMap, seed: u8) -> Result<Vec<(String, String)>, ZhihuError> {
        if zhihu::comment::has_emote_candidates(json) {
            self.body_emotes(seed)
        } else {
            Ok(Vec::new())
        }
    }

    /// 表情对照表，第一次用时取一次；**接口错误**就当空表（表情保留文字），风控 / 未登录 / 解析错误照抛
    /// （和 Kotlin 只 `catch (e: ApiException)` 一样）。
    pub(crate) fn body_emotes(&self, seed: u8) -> Result<Vec<(String, String)>, ZhihuError> {
        if let Some(cached) = self.stickers.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return Ok(cached.clone());
        }
        let path = format!("/api/v4/sticker-groups/{STICKER_GROUP}");
        let url = format!("{}{path}", self.base);
        let table = match self.get_json(&url, &path, seed) {
            Ok(json) => zhihu::comment::stickers(&json),
            Err(ZhihuError::Api { .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        *self.stickers.lock().unwrap_or_else(|e| e.into_inner()) = Some(table.clone());
        Ok(table)
    }

    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, ZhihuError> {
        self.http.execute(request).map_err(ZhihuError::from)
    }
}

/// `ZhihuSource.badId`：不认识的内容 id。
/// 同一次调用里第二个请求的签名随机字节（`x-zse-96` 每个请求要一个新的，范围和 `seed` 一样是 0..127）。
fn next_seed(seed: u8) -> u8 {
    ((seed as u16 * 31 + 17) % 127) as u8
}

fn bad_id(id: &str) -> ZhihuError {
    ZhihuError::Api {
        code: "bad_id".to_string(),
        detail: format!(
            "不认识的内容 id：{id}（应该像 answer:123，类型有 {}）",
            links::TYPES.join("/")
        ),
    }
}

/// 带签名的 GET 的请求头（Kotlin `get()` + `sign()` 的那个顺序）：
/// `Accept` 先写，再按 `sign()` 的顺序补 `x-zse-93` / `x-zse-96` / `x-requested-with`，最后是平台默认头。
///
/// 为什么拆成自由函数：扫码登录验证时用 `/api/v4/me` 同一个请求（只是换成临时罐的 `d_c0`），
/// 头必须和这条路一模一样（`ZhihuRequestEquivalenceTest` 盯着）。
pub(super) fn api_headers(
    url: &str,
    path_and_query: &str,
    dc0: Option<&str>,
    seed: u8,
) -> Vec<(String, String)> {
    let mut headers = vec![("Accept".to_string(), ACCEPT_JSON.to_string())];
    if let Some(dc0) = dc0 {
        headers.push(("x-zse-93".to_string(), sign::ZSE93.to_string()));
        headers.push((
            "x-zse-96".to_string(),
            sign::zse96(path_and_query, dc0, None, seed),
        ));
        headers.push(("x-requested-with".to_string(), "fetch".to_string()));
    }
    // 请求里已经有 Accept 了，默认头里的 `Accept: */*` 不重复加（拦截器也只补缺失的）
    headers.extend(platform_headers(false));
    let _ = url;
    headers
}

/// 平台默认头，**顺序照 Kotlin `DefaultHeadersInterceptor` 的 map 插入顺序**：
/// User-Agent / Referer / Accept / Accept-Language / x-api-version / x-app-za。
///
/// [`include_accept`]：GET 自己已经有 `Accept: application/json, text/plain, */*` 了，
/// 默认头里的 `Accept: */*` 不能重复加（Kotlin 的拦截器同样会跳过已有同名头）。
/// `/udid` 的 POST 没有 Accept，所以整组都加。
pub(super) fn platform_headers(include_accept: bool) -> Vec<(String, String)> {
    let all = [
        ("User-Agent", USER_AGENT),
        ("Referer", REFERER),
        ("Accept", ACCEPT_ANY),
        ("Accept-Language", ACCEPT_LANGUAGE),
        ("x-api-version", X_API_VERSION),
        ("x-app-za", X_APP_ZA),
    ];
    all.iter()
        .filter(|(name, _)| include_accept || *name != "Accept")
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// 响应头里找 `Location`（名字大小写不敏感，和 Kotlin 的 `equals(..., true)` 一样）。
/// 查找在 `crate::hosted::header_value`，这里只把 `&str` 复制成 `String`。
fn header(response: &HttpResponse, name: &str) -> Option<String> {
    header_value(&response.headers, name).map(|v| v.to_string())
}

// `canonical_query` 从 `crate::hosted::okhttp_canonical_query` 来（OkHttp 解析 URL 的口径，
// 注释在那里）。这里 `pub use` 保留 `zhihu::source::canonical_query` 这个老路径。
pub use crate::hosted::okhttp_canonical_query as canonical_query;
