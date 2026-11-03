//! Reddit Source 的**平台层**：推荐流（`/best.json`）/ 关注流（`/new.json`）的请求怎么拼
//! （URL / 参数顺序 / 请求头顺序 / 翻页游标）、错误怎么判、响应该交给谁解析。**收发一律通过宿主**
//! （[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-reddit/RedditSource.kt` 的同名方法（`feed` / `requireLogin` / `checkTransport` /
//! `blocked` / `headers` / `fetch`）：方法（都是 GET）+ URL + 参数顺序 + 请求头顺序必须和 Kotlin 版
//! **逐字节一样**，`RedditRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//! 搜索**没有 Kotlin 版**（Kotlin 的 `RedditSource` 里没有 `search`），验收门换成
//! `RedditSearchTest` 的**请求形状**。
//!
//! 推荐流 / 关注流和**搜索**走这里；**详情 / 评论 / 楼中楼 /
//! 头像批量取 / 视频一行没动**，还在 Kotlin（`RustRedditSource` 逐方法委托）。
//!
//! 搜索（[`RedditHostedSource::search`]）和推荐流**共用同一段收发和同一个解析**——`/search.json` 的响应
//! 和 `/best.json` 完全同构（`data.children[]` 的 `t3` + `data.after`），所以只多了一个"参数怎么拼"。
//!
//! 登录态判定用宿主的 Cookie 罐（[`CookieHost::find`] 找 `.reddit.com` 的 `reddit_session`）；
//! Reddit 的信息流请求**没有时间戳 / 签名 / 随机数**，所以这里没有 `now_sec` 要注入，
//! 也没有任何设备码（风控红线的"只用 store 里那一份"在这条路上不适用：它一个都不需要）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::detail::{CommentPage, CommentSort, FeedDetail};
use crate::host::{CookieHost, HostError, HttpHost, HttpRequest};
use crate::hosted::header_value;
use crate::json::{self, JsonMap, JsonValue};
use crate::model::{Page, UserRef};
use crate::play::PlayInfo;
use crate::reddit::detail;
use crate::reddit::{markdown, RedditError, WWW};

/// 接口根（和 `RedditSource.WWW` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = WWW;

/// 推荐流：登录后的个人首页 "Best"。
const BEST_PATH: &str = "/best.json";

/// 关注流：登录后首页的 new 排序（订阅版块的新帖）。
const NEW_PATH: &str = "/new.json";

/// 搜索：全站搜帖子。结果和 `/best.json` 同构（`data.children[]` 的 `t3` + `data.after`）。
///
/// 参考出处：redlib `src/search.rs:14-22`（`{路径}.json?{浏览器的 query}&raw_json=1`）、
/// `:64`（`sort` 缺省 `relevance`）、`:93-97`（`type` 原样传给 reddit）、`:170`（子版块搜索是另一个接口）。
/// 官方网页的搜索地址是 `/search?q=…&type=link&sort=relevance&t=all`，`.json` 版就是同一串参数。
const SEARCH_PATH: &str = "/search.json";

/// `type=link`：只搜帖子（`t3`）。
const SEARCH_TYPE: &str = "link";

/// 搜索的排序（Kotlin `SearchSort`）→ Reddit 的 `sort` 参数的**唯一一份**映射。
///
/// 三档都是 Reddit 搜索页自己的选项（`relevance` / `hot` / `new`）；没有 `top` 是因为
/// `SearchSort` 里没有对应档，而 `top` 还要额外的 `t`（时间范围）参数，
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    /// 相关度（`sort=relevance`，Reddit 的缺省）。
    Relevance,
    /// 最新（`sort=new`）。
    Latest,
    /// 热门（`sort=hot`）。
    Hot,
}

impl SearchSort {
    pub fn param(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Latest => "new",
            Self::Hot => "hot",
        }
    }
}

/// 登录 Cookie（`.reddit.com`）。**值绝不打印**。
pub const SESSION_COOKIE: &str = "reddit_session";

/// 一页多少条（Kotlin `PAGE_SIZE`）。
pub const PAGE_SIZE: i64 = 50;

/// 一页评论多少条（Kotlin `COMMENT_LIMIT`）。
pub const COMMENT_LIMIT: i64 = 200;

/// `/api/morechildren` 一次最多认 100 个 id（Kotlin `MAX_MORE_CHILDREN`）。
pub const MAX_MORE_CHILDREN: usize = 100;

/// `/api/user_data_by_account_ids.json` 一次最多认 100 个账号 id（Kotlin `MAX_AVATAR_IDS`）。
pub const MAX_AVATAR_IDS: usize = 100;

/// `sr_detail=1`：让每条帖子带上版块图标（不多发请求）。
const SR_DETAIL: &str = "1";

/// 电脑版 Chrome 的 UA（`UserAgents.DESKTOP_CHROME`，逐字一样）。
///
/// 为什么 Rust 里再写一份：`User-Agent` / `Accept` / `Accept-Language` / `Referer` 都属于
/// "请求怎么发"，归 Rust；宿主的 `DefaultHeadersInterceptor` 留着当兜底（它只补**缺失的**同名头，
/// 所以两边最终落到线上的头顺序一致）。两份漂移会被 `RedditRequestEquivalenceTest` 抓住。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 哪一条流（对应 Kotlin `FeedKind` 里 Reddit 支持的两个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

impl FeedKind {
    /// `FeedKind.label` 的中文名（未登录的报错里要用，和 Kotlin 一字不差）。
    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Recommend => "推荐",
            FeedKind::Following => "关注",
        }
    }
}

/// Reddit 的平台对象：持有两个宿主端口（发请求、读 Cookie 罐）。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustRedditHosted`）建一次、跟着 Source 活；Source 被 App 缓存到进程结束。
/// **没有内部状态**（没有锁、没有缓存、没有后台线程 / 定时器 / 连接池），闲置时零唤醒。
/// 这也和 Kotlin 版一样：`RedditSource` 的 `avatarCache` 只服务评论 / 详情。
pub struct RedditHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    cookies: Arc<dyn CookieHost>,
    /// 作者头像的**内存**缓存：`t2_…` → 头像地址；值是 `None` 表示"问过、没有"
    /// （Kotlin `RedditSource.avatarCache`，`Collections.synchronizedMap(HashMap())`）。
    ///
    /// 只服务详情 / 评论：每页评论 / 每次详情**最多多发 1 个请求**，
    /// 同一个账号只问一次，失败的也记下来（"不重试"）。不落盘、进程结束就没了。
    avatar_cache: Mutex<HashMap<String, Option<String>>>,
}

impl RedditHostedSource {
    pub fn new(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            cookies,
            avatar_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 一页。游标是不透明字符串（`data.after`），原样带回去。
    pub fn feed(&self, kind: FeedKind, cursor: Option<&str>) -> Result<Page, RedditError> {
        let path = match kind {
            FeedKind::Recommend => BEST_PATH,
            FeedKind::Following => NEW_PATH,
        };
        if !self.logged_in() {
            return Err(RedditError::NotLoggedIn {
                detail: Some(format!(
                    "Reddit 的{}流要登录：先在“我的”里导入 Reddit Cookie（未登录的 .json 会被 403）",
                    kind.label()
                )),
            });
        }
        // 参数顺序照 Kotlin：raw_json、limit、sr_detail，最后才是 after
        let mut params: Vec<(String, String)> = vec![
            ("raw_json".to_string(), "1".to_string()),
            ("limit".to_string(), PAGE_SIZE.to_string()),
            ("sr_detail".to_string(), SR_DETAIL.to_string()),
        ];
        push_cursor(&mut params, cursor);
        self.listing(path, params)
    }

    /// 搜索的一页。游标同样是 `data.after`。
    ///
    /// 参数顺序（**测试逐字节守着**，改之前先想清楚）：`q`、`sort`、`type`、`raw_json`、`limit`、
    /// `sr_detail`，`after` 永远在最后（和推荐流一条规矩）。
    ///
    /// **登录态**：和推荐流一样，没有 `.reddit_session` 就一个请求都不发——未登录的 `.json` 会被 403，
    /// 那是**风控信号**，不该由一次"用户没登录"去触发（`needsLogin = true` 也靠这一点）。
    ///
    /// **不收** `include_over_18=on`：18+ 的过滤交给 Reddit 按账号自己的设置做，App 那边还有
    /// `FeedItem.adult` 的"整条隐藏"兜底（`over_18` 的映射在 [`crate::reddit::post`] 里，
    /// 服务端回了 `over_18: true` 的条目照样标成 `adult`）。
    pub fn search(
        &self,
        text: &str,
        sort: SearchSort,
        cursor: Option<&str>,
    ) -> Result<Page, RedditError> {
        if !self.logged_in() {
            return Err(RedditError::NotLoggedIn {
                detail: Some(
                    "Reddit 的搜索要登录：先在“我的”里导入 Reddit Cookie（未登录的 .json 会被 403）"
                        .to_string(),
                ),
            });
        }
        let mut params: Vec<(String, String)> = vec![
            ("q".to_string(), text.to_string()),
            ("sort".to_string(), sort.param().to_string()),
            ("type".to_string(), SEARCH_TYPE.to_string()),
            ("raw_json".to_string(), "1".to_string()),
            ("limit".to_string(), PAGE_SIZE.to_string()),
            ("sr_detail".to_string(), SR_DETAIL.to_string()),
        ];
        push_cursor(&mut params, cursor);
        self.listing(SEARCH_PATH, params)
    }

    /// 两个接口共用的那一段：拼 URL → 宿主发 → 判定 → 解析成 [`Page`]。
    ///
    /// 推荐流 / 关注流 / 搜索的响应**完全同构**（`{kind:"Listing", data:{children[], after}}`），
    /// 所以解析只走一次 [`crate::reddit::parse_listing`]（广告 / 置顶 / 18+ / 跨帖 / 相册的规则都在那里）。
    fn listing(&self, path: &str, params: Vec<(String, String)>) -> Result<Page, RedditError> {
        let url = self.url_for(path, &params);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: request_headers(),
                body: Vec::new(),
            })
            .map_err(RedditError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        check_transport(response.status, &response.headers, &body)?;
        let map = json::parse_object(&body).map_err(|detail| RedditError::Parse { detail })?;
        crate::reddit::parse_listing(&map)
    }

    /// 登录态：`.reddit.com` 下有没有 `reddit_session`（**只看有没有，不读值**）。
    fn logged_in(&self) -> bool {
        self.cookies.find(WWW, SESSION_COOKIE).is_some()
    }

    /// `GET /api/me.json?raw_json=1`：最便宜的登录态检查。
    ///
    /// - 没有 `reddit_session` → `Ok(None)`，**一个请求都不发**；
    /// - 401 → `None`；403 **且 body 是 JSON**（未登录时 Reddit 回 `{"message":"Forbidden","error":403}`）
    ///   → `None`；403 但是 HTML 拦截页 / 429 / 限流额度用尽 → 风控错误（`check_transport`，不重试）；
    /// - 成功 → [`crate::reddit::parse_me`]（没有 `name` 就是 `None`）。
    ///
    /// 异常消息和 Kotlin 一样**不带 Cookie 值**。
    pub fn whoami(&self) -> Result<Option<UserRef>, RedditError> {
        if !self.logged_in() {
            return Ok(None);
        }
        let params = vec![("raw_json".to_string(), "1".to_string())];
        let url = self.url_for("/api/me.json", &params);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: request_headers(),
                body: Vec::new(),
            })
            .map_err(RedditError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        if response.status == 401 {
            return Ok(None);
        }
        // 未登录的 403 是 JSON；真正的拦截页是 HTML（Cloudflare 那一类）→ 继续交给 check_transport
        if response.status == 403 && body.trim_start_matches(markdown::is_java_ws).starts_with('{') {
            return Ok(None);
        }
        check_transport(response.status, &response.headers, &body)?;
        let map = json::parse_object(&body).map_err(|detail| RedditError::Parse { detail })?;
        Ok(crate::reddit::parse_me(&map))
    }

    // 详情 / 评论 / 楼中楼

    /// 详情：`GET /comments/<id>.json?raw_json=1&limit=1&sort=confidence`，取 `[0]` 的帖子。
    ///
    /// 正文作者头像走同一个批量接口（先按这一份响应收 `author_fullname`，再**最多一个**请求取头像）。
    pub fn detail(&self, id: &str) -> Result<FeedDetail, RedditError> {
        let thread = self.thread(id, 1, CommentSort::Default)?;
        // Kotlin 是先 `postOf(thread)`（没有帖子就抛）再取头像：顺序一样，失败时一个头像请求都不发
        let post = detail::post_of_thread(&thread)?.clone();
        let ids = detail::account_ids(&JsonValue::Array(thread));
        let avatars = self.avatars(&ids);
        detail::parse_detail(&post, &avatars)
    }

    /// 一级评论：`/comments/<id>.json` 的第二个 Listing；顶层 `more` 节点成为下一页游标
    /// （`more:<id1>,<id2>,…`），翻页时走 `/api/morechildren`。
    ///
    /// 每页**多发 1 个**请求取作者头像（这一页所有 `author_fullname` 合成一次，最多 100 个）。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
    ) -> Result<CommentPage, RedditError> {
        if let Some(c) = cursor.filter(|c| c.starts_with(detail::MORE_PREFIX)) {
            return self.more_page(id, &detail::more_ids(c), sort);
        }
        let thread = JsonValue::Array(self.thread(id, COMMENT_LIMIT, sort)?);
        // Kotlin: 第一个"children 里出现非 t3"的 Listing 就是评论区
        let listing = thread
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|v| v.as_object())
            .find(|l| {
                json::obj(l, "data")
                    .and_then(|d| json::arr(d, "children"))
                    .map(|children| {
                        children.iter().any(|child| {
                            child
                                .as_object()
                                .and_then(|o| json::text(o, "kind"))
                                .as_deref()
                                != Some("t3")
                        })
                    })
                    .unwrap_or(false)
            });
        let Some(listing) = listing else {
            return Ok(CommentPage::default());
        };
        let ids = detail::account_ids(&thread);
        let avatars = self.avatars(&ids);
        Ok(detail::parse_comments(listing, &avatars))
    }

    /// 楼中楼：`GET /comments/<postId>/_/<commentId>.json`（评论的永久链接，`_` 是占位 slug）。
    ///
    /// [comment_id] 以 `more:` 开头时直接走 `/api/morechildren`（顶层 / 楼中楼 `more` 游标就是这种）。
    /// **游标参数和 Kotlin 一样不用**：这个接口只回第一层，往下翻要靠 `more:` 那条游标。
    pub fn replies(&self, id: &str, comment_id: &str) -> Result<CommentPage, RedditError> {
        if comment_id.starts_with(detail::MORE_PREFIX) {
            return self.more_page(id, &detail::more_ids(comment_id), CommentSort::Default);
        }
        let params = vec![
            ("raw_json".to_string(), "1".to_string()),
            ("limit".to_string(), COMMENT_LIMIT.to_string()),
            (
                "sort".to_string(),
                sort_param(CommentSort::Default).to_string(),
            ),
        ];
        check_id(id)?;
        check_id(comment_id)?;
        let body = self.get(&format!("/comments/{id}/_/{comment_id}.json"), &params)?;
        let element = detail::parse_element(&body)?;
        let ids = detail::account_ids(&element);
        let avatars = self.avatars(&ids);
        Ok(detail::parse_comment_thread(
            &element, id, comment_id, &avatars,
        ))
    }

    /// **播放地址**（Kotlin `RedditSource.play`）：整条帖子一个视频（`partId = main`）。
    ///
    /// 三步，和 Kotlin 一模一样：
    /// 1. `/comments/<id>.json?limit=1` 取帖子，从 `secure_media` / `media` / `preview` 里挑 `reddit_video`；
    /// 2. 按 [`super::play::dash_url_of`] 拼清单地址，GET 一次 MPD（`Accept: */*`）；
    ///    **失败就吞掉**（HTTP 非 2xx / 空 body / 结构不对），回退链继续走 HLS；
    /// 3. 有 DASH 就用 DASH，否则 `hls_url` → `fallback_url` → 报 `no_video`。
    ///
    /// [`part_id`] 不是 `main` 时在**发请求之前**就报 `Api("part")`（和 Kotlin 一样）。
    /// 清单那一个请求**不走 `check_transport`**：它是不是 HTML 挑战页不影响"取不到就回退"这条链
    /// （Kotlin 的 `fetchText` 也只判 2xx + 非空白），所以这里照抄、不额外触发风控异常。
    pub fn play(&self, id: &str, part_id: &str) -> Result<PlayInfo, RedditError> {
        super::play::check_part(part_id)?;
        let thread = self.thread(id, 1, CommentSort::Default)?;
        let post = detail::post_of_thread(&thread)?.clone();
        let video = super::play::video_of(&post).ok_or_else(|| RedditError::Api {
            code: "no_video".to_string(),
            detail: "这条帖子没有 reddit_video（不是 Reddit 托管的视频，放不了）".to_string(),
        })?;
        let mpd = super::play::dash_url_of(&video).and_then(|url| {
            let response = self
                .http
                .execute(HttpRequest {
                    method: "GET".to_string(),
                    url: url.clone(),
                    headers: super::play::manifest_headers(),
                    body: Vec::new(),
                })
                .ok()?;
            let body = String::from_utf8_lossy(&response.body).into_owned();
            if !(200..=299).contains(&response.status) || markdown::is_blank(&body) {
                return None;
            }
            super::play::decode_mpd(&body, &url)
        });
        super::play::play(&video, mpd.as_ref())
    }

    /// `/comments/<id>.json`：`[0]` 是帖子、`[1]` 是评论。不是数组就抛 `Parse`（文案照 Kotlin）。
    fn thread(
        &self,
        id: &str,
        limit: i64,
        sort: CommentSort,
    ) -> Result<Vec<JsonValue>, RedditError> {
        let params = vec![
            ("raw_json".to_string(), "1".to_string()),
            ("limit".to_string(), limit.to_string()),
            ("sort".to_string(), sort_param(sort).to_string()),
        ];
        check_id(id)?;
        let body = self.get(&format!("/comments/{id}.json"), &params)?;
        match serde_json::from_str::<JsonValue>(&body) {
            Ok(JsonValue::Array(items)) => Ok(items),
            Ok(_) => Err(RedditError::Parse {
                detail: format!(
                    "详情不是 [帖子, 评论] 两个 Listing 的数组：{}",
                    json::take_chars(&body, 120)
                ),
            }),
            Err(_) => Err(RedditError::Parse {
                detail: format!("响应不是合法 JSON：{}", json::take_chars(&body, 120)),
            }),
        }
    }

    /// 展开一个 `more` 节点：一次最多认 100 个 id，剩下的做成下一页游标。
    fn more_page(
        &self,
        post_id: &str,
        ids: &[String],
        sort: CommentSort,
    ) -> Result<CommentPage, RedditError> {
        let chunk: Vec<String> = ids.iter().take(MAX_MORE_CHILDREN).cloned().collect();
        let rest: Vec<String> = ids.iter().skip(MAX_MORE_CHILDREN).cloned().collect();
        let json = self.more_children(post_id, &chunk, sort)?;
        let value = JsonValue::Object(json.clone());
        let avatars = self.avatars(&detail::account_ids(&value));
        let page = detail::parse_more_children(&json, &avatars);
        Ok(CommentPage {
            items: page.items,
            next: if rest.is_empty() {
                None
            } else {
                Some(detail::more_cursor(&rest))
            },
        })
    }

    /// `/api/morechildren`（GET，`api_type=json`）。**ids 为空时一个请求都不发**（Kotlin 的做法）。
    fn more_children(
        &self,
        post_id: &str,
        ids: &[String],
        sort: CommentSort,
    ) -> Result<JsonMap, RedditError> {
        if ids.is_empty() {
            return Ok(JsonMap::new());
        }
        check_id(post_id)?;
        let params = vec![
            ("api_type".to_string(), "json".to_string()),
            ("link_id".to_string(), format!("t3_{post_id}")),
            (
                "children".to_string(),
                ids.iter()
                    .take(MAX_MORE_CHILDREN)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("limit_children".to_string(), "false".to_string()),
            ("sort".to_string(), sort_param(sort).to_string()),
            ("raw_json".to_string(), "1".to_string()),
        ];
        let body = self.get("/api/morechildren", &params)?;
        json::parse_object(&body).map_err(|detail| RedditError::Parse { detail })
    }

    /// 作者头像表：`author_fullname` → 地址。
    ///
    /// - 只问**还没问过**的（缓存里连"问过、没有"都记着），一次最多 [`MAX_AVATAR_IDS`] 个；
    /// - 请求失败 / 结构不对就整批记成"没有"（Kotlin `catch (_: Exception)`：**不影响评论显示、不重试**）；
    /// - 返回的是"这次 + 缓存里"的并集，按传入顺序过滤（`RedditParser` 按 `author_fullname` 查）。
    fn avatars(&self, ids: &[String]) -> HashMap<String, String> {
        // Kotlin: `ids.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.distinct()`
        let mut all: Vec<String> = Vec::new();
        for id in ids {
            let trimmed = markdown::trim_java(id);
            if !trimmed.is_empty() && !all.iter().any(|s| s == trimmed) {
                all.push(trimmed.to_string());
            }
        }
        let mut wanted: Vec<String> = Vec::new();
        {
            let cache = self.avatar_cache.lock().unwrap();
            for id in &all {
                if wanted.len() >= MAX_AVATAR_IDS {
                    break;
                }
                if !cache.contains_key(id) {
                    wanted.push(id.clone());
                }
            }
        }
        if !wanted.is_empty() {
            let params = vec![
                ("raw_json".to_string(), "1".to_string()),
                ("ids".to_string(), wanted.join(",")),
            ];
            // 头像取不到不影响评论：宿主错误 / 非 2xx / 坏 JSON 都当成"没有"
            let json = self
                .get("/api/user_data_by_account_ids.json", &params)
                .ok()
                .and_then(|body| json::parse_object(&body).ok());
            let mut cache = self.avatar_cache.lock().unwrap();
            for id in &wanted {
                let avatar = json.as_ref().and_then(|j| {
                    json::obj(j, id)
                        .and_then(|o| json::text(o, "profile_img"))
                        .map(|u| json::to_https(&u.replace("&amp;", "&")))
                        .filter(|u| !markdown::is_blank(u))
                });
                cache.insert(id.clone(), avatar);
            }
        }
        let cache = self.avatar_cache.lock().unwrap();
        let mut out = HashMap::new();
        for id in all {
            if let Some(Some(url)) = cache.get(&id) {
                out.insert(id, url.clone());
            }
        }
        out
    }

    /// 拼 URL → 宿主发 → 判定 → 返回 body（详情 / 评论这一层共用的那几步）。
    fn get(&self, path: &str, params: &[(String, String)]) -> Result<String, RedditError> {
        let url = self.url_for(path, params);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: request_headers(),
                body: Vec::new(),
            })
            .map_err(RedditError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        check_transport(response.status, &response.headers, &body)?;
        Ok(body)
    }

    fn url_for(&self, path: &str, params: &[(String, String)]) -> String {
        let mut url = format!("{}{}", self.base.trim_end_matches('/'), path);
        if !params.is_empty() {
            url.push('?');
            let query = params
                .iter()
                .map(|(k, v)| format!("{}={}", query_encode(k), query_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.push_str(&query);
        }
        url
    }
}

/// 评论排序 → Reddit 的 `sort` 参数（Kotlin `RedditSource.sortOf`）。
fn sort_param(sort: CommentSort) -> &'static str {
    match sort {
        CommentSort::Default => "confidence",
        CommentSort::Hot => "top",
        CommentSort::Newest => "new",
    }
}

/// 帖子 / 评论 id 只认 base36（`[0-9A-Za-z]`，Reddit 实际是小写）：它们会**原样拼进路径**，
/// 带 `/` `.` `?` `%` 的一律 `bad_id`、一个请求都不发。`more:` 游标里的 id 走 query、另有编码，不经过这里。
fn check_id(id: &str) -> Result<(), RedditError> {
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Ok(());
    }
    Err(RedditError::Api {
        code: "bad_id".to_string(),
        detail: format!("不认识的 Reddit id：{}（应该是字母数字，例如 1abc2d）", json::take_chars(id, 40)),
    })
}

impl From<HostError> for RedditError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => RedditError::Network { detail },
            HostError::ProxyUnavailable { detail } => RedditError::ProxyUnavailable { detail },
            HostError::Other { detail } => RedditError::Host { detail },
        }
    }
}

/// 游标（`data.after`）永远追加在参数最后；**空白游标当没有**（Kotlin `takeIf { it.isNotBlank() }`）。
fn push_cursor(params: &mut Vec<(String, String)>, cursor: Option<&str>) {
    if let Some(after) = cursor.filter(|c| !markdown::is_blank(c)) {
        params.push(("after".to_string(), after.to_string()));
    }
}

/// 请求头，**顺序照 Kotlin 版最终落到线上的顺序**。
///
/// Kotlin 的 `headers()` 是 `linkedMapOf(User-Agent, Accept, Accept-Language, Referer)`，由
/// `DefaultHeadersInterceptor` 按这个顺序补上；OkHttp 之后才追加 Host / Connection /
/// Accept-Encoding / Cookie。这里一次写全，宿主拦截器因此一个都不会再补。
fn request_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        (
            "Accept".to_string(),
            "application/json, text/plain, */*".to_string(),
        ),
        (
            "Accept-Language".to_string(),
            "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7".to_string(),
        ),
        ("Referer".to_string(), format!("{WWW}/")),
    ]
}

/// 传输层的风控信号：识别到就停下，不重试。
///
/// 异常消息里只放 body 开头和响应头的**名字**，不放 Cookie（和 Kotlin `checkTransport` / `blocked` 一样）。
pub fn check_transport(
    status: u16,
    headers: &[(String, String)],
    body: &str,
) -> Result<(), RedditError> {
    let remaining = header_value(headers, "x-ratelimit-remaining");
    if status == 401 {
        return Err(RedditError::NotLoggedIn { detail: None });
    }
    // 403 先看：它是"TLS 指纹 / IP 被拦"的直接证据，消息里要带上 body 和响应头
    if status == 403 {
        return Err(blocked(headers, body));
    }
    if status == 429 {
        return Err(RedditError::RiskControl {
            code: Some("429".to_string()),
            detail: "请求过于频繁（HTTP 429）".to_string(),
        });
    }
    if remaining
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v <= 0.0)
        .unwrap_or(false)
    {
        return Err(RedditError::RiskControl {
            code: Some("rate-limit".to_string()),
            detail: "限流额度用尽（x-ratelimit-remaining=0）".to_string(),
        });
    }
    if !(200..=299).contains(&status) {
        return Err(RedditError::Api {
            code: format!("HTTP {status}"),
            detail: markdown::take_utf16(body, 120).to_string(),
        });
    }
    let head = body.trim_start_matches(markdown::is_java_ws);
    if head.starts_with("<!") || head.starts_with("<html") {
        return Err(RedditError::RiskControl {
            code: Some("html".to_string()),
            detail: "拿到的是 HTML 页面而不是 JSON（风控 / 验证页）".to_string(),
        });
    }
    Ok(())
}

/// 403：带上能判断风控来源的证据（body 开头 + 像风控的响应头**名字**）。
fn blocked(headers: &[(String, String)], body: &str) -> RedditError {
    let mut names: Vec<String> = headers.iter().map(|(name, _)| name.to_lowercase()).collect();
    names.sort();
    names.dedup();
    let interesting: Vec<String> = names
        .into_iter()
        .filter(|n| {
            ["ratelimit", "retry-after", "cf-", "block", "server", "x-", "set-cookie"]
                .iter()
                .any(|needle| n.contains(needle))
        })
        .collect();
    // Kotlin: `body.trim().replace(Regex("\\s+"), " ").take(140)`
    let collapsed = collapse_ws(markdown::trim_java(body));
    RedditError::RiskControl {
        code: Some("blocked".to_string()),
        detail: format!(
            "HTTP 403（blocked）：body 开头 {}；响应头 {}",
            markdown::take_utf16(&collapsed, 140),
            if interesting.is_empty() {
                "（没有特别的）".to_string()
            } else {
                interesting.join(",")
            }
        ),
    }
}

/// Java 正则 `\s+` → 一个空格（只认那 6 个 ASCII 空白，和 `RedditSource.blocked` 一样）。
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if markdown::is_java_regex_ws(c) {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

// `query_encode` 从 `crate::hosted` 来（口径和注释都在那里）；`pub use` 保留老路径。
pub use crate::hosted::query_encode;

// ---------------------------------------------------------------------------
// 播放（rust-play-misc）
// ---------------------------------------------------------------------------

