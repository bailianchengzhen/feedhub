//! TikTok Source 的**平台层**：推荐流的请求怎么拼（URL / 参数顺序 / 请求头顺序 / 签名 / 翻页游标 /
//! 首次设备参数的获取）、错误怎么判、响应该交给谁解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），
//! 这里不碰 socket。
//!
//! 逐条对照 `source-tiktok/TiktokSource.kt` 的同名方法（`recommend` / `ensureDevice` / `fetchHomeScope` /
//! `getJson` / `getDocument` / `checkTransport` / `commonParams` / `urlOf` / `SignInterceptor`）和
//! `TiktokDevice.kt`：方法 + URL + 参数顺序 + 请求头顺序 + body 必须和 Kotlin 版**逐字节一样**，
//! `TiktokRequestEquivalenceTest` 用 MockWebServer 录两边对比守着。
//!
//! 评论 / 楼中楼走 `comment/list*` 并带同一套 `X-Dynosaur` 签名。
//! 播放 / `raw` 还在 Kotlin（`RustTiktokSource` 逐方法委托）。
//!
//! 几条边界（和 B站 / 酷安 同一套规矩）：
//! - **时间戳、随机密钥都从外面注入**（`now_ms` / `key` / `key2`）：Rust 不读时钟、不取随机数，
//!   测试才能把两次运行的请求逐字节比。
//! - **设备码只读宿主 store / Cookie 罐里那一份**（`tiktok.device_id` / `multi_sids`），拿不到就
//!   **不往 query 里塞**（不编设备号）。
//! - `msToken` / `s_v_web_id` **只从 Cookie 罐 / store 里读真值，绝不伪造**：伪造的 msToken 会让
//!   `item_list` 返回空 body + `tt_orcas_res: 1`。`verifyFp` 的**生成**留在 Kotlin 的
//!   `TiktokDevice`（和酷安设备同一条规矩），Rust 只读；两份都没有就直接报错、不发请求。
//! - 签名参数（`X-Dynosaur` / `msToken` / `X-Bogus` / `X-Gnarly`）**直接拼进 URL**，不再靠 Kotlin 的
//!   `SignInterceptor` 补（那条拦截器只认 `SignTag`，Rust 发出去的请求没有那个 tag，不会被补第二遍）。

use std::sync::{Arc, Mutex};

use crate::detail::{CommentPage, FeedDetail};
use crate::play::PlayInfo;
use crate::host::{CookieHost, HttpHost, HttpRequest, HttpResponse, StoreHost};
use crate::hosted::header_value;
use crate::json::{self, JsonMap};
use crate::model::{Page, UserRef};
use crate::tiktok::sign;
use crate::tiktok::TiktokError;

/// 接口根（和 `TiktokSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://www.tiktok.com";

/// 站根：推荐流请求头的 Referer，也是首页请求的 Referer。
pub const ROOT: &str = "https://www.tiktok.com/";

/// 取 Cookie 用的"身份 URL"（和 Kotlin 的 `WWW` 一样）。
const COOKIE_URL: &str = "https://www.tiktok.com/";

const RECOMMEND_PATH: &str = "/api/recommend/item_list/";

/// 一级评论路径。
pub const COMMENT_LIST_PATH: &str = "/api/comment/list/";

/// 楼中楼路径。
pub const COMMENT_REPLY_PATH: &str = "/api/comment/list/reply/";

/// 评论接口的默认 referer。
pub const DEFAULT_VIDEO_REF: &str = "https://www.tiktok.com/@tiktok?lang=zh-Hans";

/// 抓包的 `count=20`（一级评论一页）。
pub const COMMENT_PAGE_SIZE: usize = 20;

/// 楼中楼一页 3 条（TiktokApis 的 `get_comment_replies`）。
pub const REPLY_PAGE_SIZE: usize = 3;

/// 抓包的 `count=6`（TiktokApis / catbus 的默认值，浏览器一次就请求这么多）。
pub const PAGE_SIZE: usize = 6;

/// `ctx.store` 里的键（和 Kotlin `TiktokDevice` 一字不差，两边共用同一份缓存）。
pub const KEY_DEVICE_ID: &str = "tiktok.device_id";
pub const KEY_ODIN_ID: &str = "tiktok.odin_id";
pub const KEY_REGION: &str = "tiktok.region";
pub const KEY_WEB_ID_TIME: &str = "tiktok.web_id_last_time";
pub const KEY_VERIFY_FP: &str = "tiktok.verify_fp";
pub const KEY_MS_TOKEN: &str = "tiktok.ms_token";
pub const KEY_CLIENT_AB: &str = "tiktok.client_ab_versions";

/// 抓包那台浏览器：Chrome 153 / Windows（TiktokApis 的 `TiktokAuth.UA`）。
pub const CHROME_VERSION: &str = "153.0.0.0";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";

const SEC_CH_UA: &str = "\"Google Chrome\";v=\"153\", \"Not_A Brand\";v=\"8\", \"Chromium\";v=\"153\"";
const UA_PLATFORM: &str = "\"Windows\"";
const ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,en;q=0.8,zh-TW;q=0.7,ja;q=0.6";
const BROWSER_LANGUAGE: &str = "zh-CN";
const BROWSER_NAME: &str = "Mozilla";
const BROWSER_PLATFORM: &str = "Win32";
const SCREEN_WIDTH: &str = "2560";
const SCREEN_HEIGHT: &str = "1440";
const CPU_CORE_NUMBER: &str = "20";
const TZ_NAME: &str = "Asia/Shanghai";

/// 文档（页面）请求的 Accept（照 TiktokApis 的 `HeaderBuilder` DOC）。
const DOCUMENT_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,\
image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";

/// 登录态判定用的 Cookie。
const LOGIN_COOKIES: [&str; 3] = ["sessionid", "sid_tt", "multi_sids"];

/// Chrome 153 抓包里的 `clientABVersions`（TiktokApis `TiktokAuth.CLIENT_AB_VERSIONS` 原值）。
/// 这是数据不是算法：随版本变，`ctx.store` 的 `tiktok.client_ab_versions` 能覆盖。
pub const DEFAULT_CLIENT_AB_VERSIONS: &str = "70508271,72437276,73720540,75360573,75657507,75843653,75878361,\
76074824,76122410,76179551,76403724,76499369,76626930,76651230,\
76669176,76713103,76740102,76792575,76805936,76813520,76815102,\
76826749,76839930,76840792,76887350,76889673,76907405,76933422,\
76945954,76950455,77005875,77011497,77014490,77056281,77064650,\
77071444,77093404,77120364,77141446,77150562,77155134,70138197,\
70156809,70405643,71057832,71200802,71381811,71803300,72360691,\
72408100,72854054,72892778,73171280,73208420,73989921,74276218,\
74844724,75330961";

/// 一次请求要用的"环境"值（Kotlin `TiktokDevice` 的只读视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub user_agent: String,
    /// `browser_version`：UA 去掉 `Mozilla/` 之后的那一段（cv-cat 就是这么切）。
    pub browser_version: String,
    pub device_id: Option<String>,
    pub odin_id: Option<String>,
    pub region: String,
    pub web_id_last_time: i64,
    pub verify_fp: String,
    pub ms_token: String,
    pub client_ab_versions: String,
}

/// TikTok 的平台对象：持有三个宿主端口，**自己的状态只有一个"首页设备数据只取一次"的标记**。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustTiktokHosted`）建一次、跟着 Source 活；Source 被 App
/// 缓存到进程结束。没有后台线程、没有定时器、没有连接池——闲置时零唤醒。
pub struct TiktokHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    /// 首页（`GET /`）取过设备数据没有（Kotlin `TiktokSource.bootstrapped` 的 `@Volatile`）。
    bootstrapped: Mutex<bool>,
}

impl TiktokHostedSource {
    pub fn new(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            store,
            cookies,
            bootstrapped: Mutex::new(false),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 一页推荐流。[`now_ms`] 是毫秒时间戳（`timestamp` / `nonce` / `WebIdLastTime` 的兜底都用它），
    /// [`key`] / [`key2`] 是两组各 12 个 32 位签名密钥——**全部由调用方注入**，Rust 不取随机数 / 不读时钟。
    ///
    /// 顺序照 Kotlin `recommend()`：先判登录（没登录**一个请求都不发**），再补设备数据，再请求。
    pub fn feed(
        &self,
        cursor: Option<&str>,
        now_ms: i64,
        key: &[u32],
        key2: &[u32],
    ) -> Result<Page, TiktokError> {
        if !self.logged_in() {
            return Err(TiktokError::NotLoggedIn {
                detail: Some("TikTok 推荐流要登录：先在“我的”里导入 TikTok Cookie".to_string()),
            });
        }
        if key.len() != sign::KEY_WORDS || key2.len() != sign::KEY_WORDS {
            return Err(TiktokError::Internal {
                detail: format!(
                    "签名要 {} 个密钥字（收到 {} / {}）",
                    sign::KEY_WORDS,
                    key.len(),
                    key2.len()
                ),
            });
        }
        self.ensure_device()?;
        let device = self.device(now_ms)?;

        let params = recommend_params(&device, now_ms, cursor);
        let query = sign::rfc3986_query(&params);
        let timestamp = now_ms / 1000;
        let nonce = sign::clock_nonce(now_ms);
        let signed = sign::sign(
            &query,
            &device.user_agent,
            &device.ms_token,
            &[],
            timestamp,
            nonce,
            !nonce,
            sign::CALL_SEQUENCE_START,
            key,
            key2,
        );
        let url = format!("{}{}?{}", self.base.trim_end_matches('/'), RECOMMEND_PATH, signed.query);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: api_headers(&device.user_agent, ROOT),
                body: Vec::new(),
            })
            .map_err(TiktokError::from)?;
        let body = body_text(&response);
        check_transport(&response, &body)?;
        let object = json::parse_object(&body).map_err(|detail| TiktokError::Parse { detail })?;
        crate::tiktok::parse_feed_object(&object, Some(&now_ms.to_string()))
    }

    /// 详情：视频页 HTML 的 `webapp.video-detail.itemInfo.itemStruct`（`/@<作者>/video/<id>`）。
    ///
    /// **不签名**（HTML 那条路不经过 `SignInterceptor`），也不能没有作者：id 里没有 `<作者>/` 时
    /// **在发请求之前**就报 `no_author`（和 Kotlin `requireUniqueId` 一样）。
    pub fn detail(&self, id: &str) -> Result<FeedDetail, TiktokError> {
        check_id(id)?;
        let unique_id = crate::tiktok::unique_id_of(id).ok_or_else(|| TiktokError::Api {
            code: "no_author".to_string(),
            detail: "TikTok 的详情页地址必须带作者（`<作者>/<itemId>`），这个 id 里没有：请从信息流里打开这条内容"
                .to_string(),
        })?;
        // 和 Kotlin `detail()` 的顺序一样：先判 id、再补设备数据、最后请求
        self.ensure_device()?;
        let item = self.fetch_video_item(id, &unique_id)?;
        crate::tiktok::detail::parse(&item, id)
    }

    /// 播放地址：**每次都重新取一次视频页**（`itemStruct.video.playAddr` / `bitrateInfo` 里的地址带
    /// 时效签名，会过期），不复用打开详情时那份（和抖音 / 小黑盒 / 小红书不一样——Kotlin 就是这么做的）。
    ///
    /// 拉流要带的头只有 `Referer` + 浏览器 UA：3MH 记的是"只带 UA 和 Range"也能拿到 206（它用的是 embed
    /// 接口的直链），抖音那边则是"不带 Referer 403"。**这里不留 Cookie 头**：`PlayInfo` 会进日志 /
    /// 本机中转，Cookie 不是它该带的东西。
    ///
    /// id 里没有作者时报 `no_author`、**一个请求都不发**（和 Kotlin `requireUniqueId` 一样）。
    pub fn play(&self, id: &str, part_id: &str, quality: Option<i32>) -> Result<PlayInfo, TiktokError> {
        check_id(id)?;
        let unique_id = crate::tiktok::unique_id_of(id).ok_or_else(|| TiktokError::Api {
            code: "no_author".to_string(),
            detail: "TikTok 的详情页地址必须带作者（`<作者>/<itemId>`），这个 id 里没有：请从信息流里打开这条内容"
                .to_string(),
        })?;
        // 顺序照 Kotlin `play()`：先判 id、再补设备数据、再请求
        self.ensure_device()?;
        let item = self.fetch_video_item(id, &unique_id)?;
        let headers = self.stream_headers(&unique_id, &crate::tiktok::item_id_of(id));
        crate::tiktok::play::play(&item, part_id, quality, headers)
    }

    /// 拉流要带的头：视频页 Referer + 与接口一致的浏览器 UA（Kotlin `streamHeaders`）。
    pub fn stream_headers(&self, unique_id: &str, item_id: &str) -> Vec<(String, String)> {
        vec![
            pair("Referer", video_page_url(unique_id, item_id)),
            pair("User-Agent", USER_AGENT),
        ]
    }

    /// 视频页的 `itemStruct`（`/@<作者>/video/<id>` 的 hydration；详情 / 播放共用）。
    ///
    /// Kotlin `fetchVideoItem`：抠 hydration → 取 `itemInfo.itemStruct` → 拿不到就把状态码带出来。
    fn fetch_video_item(&self, id: &str, unique_id: &str) -> Result<JsonMap, TiktokError> {
        let path = format!("/@{unique_id}/video/{}", crate::tiktok::item_id_of(id));
        let url = format!("{}{}", self.base.trim_end_matches('/'), path);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: url.clone(),
                headers: document_headers(USER_AGENT, &url),
                body: Vec::new(),
            })
            .map_err(TiktokError::from)?;
        let body = body_text(&response);
        check_transport(&response, &body)?;
        self.item_from_page(&body, id)
    }

    /// 视频页 HTML → `itemStruct`（hydration 缺失 / 作品看不到就报错，文案和 Kotlin 一样）。
    fn item_from_page(&self, body: &str, id: &str) -> Result<JsonMap, TiktokError> {
        let scope = hydration(body).ok_or_else(|| TiktokError::Parse {
            detail: "视频页里没有 __UNIVERSAL_DATA_FOR_REHYDRATION__（可能是验证页 / 登录墙）"
                .to_string(),
        })?;
        crate::tiktok::detail::video_detail(&scope).ok_or_else(|| {
            let (status, message) = crate::tiktok::detail::video_detail_status(&scope);
            TiktokError::Api {
                code: status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "no_item".to_string()),
                detail: crate::tiktok::detail::not_found_message(
                    status,
                    message.as_deref(),
                    id,
                ),
            }
        })
    }

    /// 一级评论：`GET /api/comment/list/`（参数照 TiktokApis 的 `get_comments`：
    /// `app_language=ja-JP`、`fromWeb=1`、`enter_from=tiktok_web`、`history_len=3`）。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        key: &[u32],
        key2: &[u32],
    ) -> Result<CommentPage, TiktokError> {
        check_id(id)?;
        self.ensure_device()?;
        let device = self.device(now_ms)?;
        let item_id = crate::tiktok::item_id_of(id);
        let page_url = crate::tiktok::unique_id_of(id)
            .map(|unique_id| video_page_url(&unique_id, &item_id))
            .unwrap_or_else(|| DEFAULT_VIDEO_REF.to_string());
        let mut params = common_params(
            &device,
            &page_url,
            &page_url,
            Some("video"),
            3,
            "ja-JP",
            true,
        );
        params.extend([
            pair("aweme_id", item_id),
            pair("count", COMMENT_PAGE_SIZE.to_string()),
            pair("current_region", device.region.clone()),
            pair("cursor", cursor.unwrap_or("0")),
            pair("enter_from", "tiktok_web"),
            pair("fromWeb", "1"),
            pair("is_non_personalized", "false"),
        ]);
        self.signed_api(COMMENT_LIST_PATH, &device, &page_url, params, now_ms, key, key2)
            .and_then(|body| {
                let object =
                    json::parse_object(&body).map_err(|detail| TiktokError::Parse { detail })?;
                crate::tiktok::detail::comments(&object)
            })
    }

    /// 楼中楼：`GET /api/comment/list/reply/`（`history_len=6`、`count=3`）。
    ///
    /// query 里的 `referer` 与 `root_referer` **必须是完整的视频页 URL**（两个参考实现都在本地校验），
    /// 所以只能从信息流里带作者的入口进来；没有作者就报 `no_referer`、一个请求都不发。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        key: &[u32],
        key2: &[u32],
    ) -> Result<CommentPage, TiktokError> {
        check_id(id)?;
        let unique_id = crate::tiktok::unique_id_of(id).ok_or_else(|| TiktokError::Api {
            code: "no_referer".to_string(),
            detail: "楼中楼要完整的视频页地址：请从信息流里打开这条内容（详情 id 形如 `<作者>/<itemId>`）"
                .to_string(),
        })?;
        self.ensure_device()?;
        let device = self.device(now_ms)?;
        let item_id = crate::tiktok::item_id_of(id);
        let page_url = video_page_url(&unique_id, &item_id);
        let mut params = common_params(
            &device,
            &page_url,
            &page_url,
            Some("video"),
            6,
            "zh-Hans",
            true,
        );
        params.extend([
            pair("comment_id", comment_id),
            pair("count", REPLY_PAGE_SIZE.to_string()),
            pair("cursor", cursor.unwrap_or("0")),
            pair("item_id", item_id),
        ]);
        self.signed_api(COMMENT_REPLY_PATH, &device, &page_url, params, now_ms, key, key2)
            .and_then(|body| {
                let object =
                    json::parse_object(&body).map_err(|detail| TiktokError::Parse { detail })?;
                crate::tiktok::detail::comments(&object)
            })
    }

    /// 签名 + 发一个 GET 接口请求，返回响应体（评论 / 楼中楼共用）。
    fn signed_api(
        &self,
        path: &str,
        device: &Device,
        referer: &str,
        mut params: Vec<(String, String)>,
        now_ms: i64,
        key: &[u32],
        key2: &[u32],
    ) -> Result<String, TiktokError> {
        if key.len() != sign::KEY_WORDS || key2.len() != sign::KEY_WORDS {
            return Err(TiktokError::Internal {
                detail: format!(
                    "签名要 {} 个密钥字（收到 {} / {}）",
                    sign::KEY_WORDS,
                    key.len(),
                    key2.len()
                ),
            });
        }
        // 业务参数按 **ASCII 键名升序**
        params.sort_by(|a, b| a.0.cmp(&b.0));
        let query = sign::rfc3986_query(&params);
        let timestamp = now_ms / 1000;
        let nonce = sign::clock_nonce(now_ms);
        let signed = sign::sign(
            &query,
            &device.user_agent,
            &device.ms_token,
            &[],
            timestamp,
            nonce,
            !nonce,
            sign::CALL_SEQUENCE_START,
            key,
            key2,
        );
        let url = format!(
            "{}{}?{}",
            self.base.trim_end_matches('/'),
            path,
            signed.query
        );
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: api_headers(&device.user_agent, referer),
                body: Vec::new(),
            })
            .map_err(TiktokError::from)?;
        let body = body_text(&response);
        check_transport(&response, &body)?;
        Ok(body)
    }

    /// 有没有登录：`sessionid` / `sid_tt` / `multi_sids` 任一即可。
    fn logged_in(&self) -> bool {
        LOGIN_COOKIES
            .iter()
            .any(|name| self.cookies.find(COOKIE_URL, name).is_some())
    }

    /// `GET /` 的首页 hydration → 当前登录的账号。
    ///
    /// - 没登录（`sessionid` / `sid_tt` / `multi_sids` 都没有）→ `Ok(None)`，**一个请求都不发**；
    /// - 有登录就**无条件**打一次首页（Kotlin 的 `whoami` 不走 `ensureDevice` 的"取过就跳过"，它每次都问）；
    /// - 首页顺手把设备数据（wid / odinId / region / webIdCreatedTime）和响应头 `x-ms-token` 写回 store
    ///   （和 `ensure_device` 同一条），`bootstrapped` 在**请求之前**置 true（首页失败也不会重问）；
    /// - 页面里没有 hydration → `Ok(None)`；hydration 里找不到账号 → `Ok(None)`
    ///   （宁可显示"登录已失效"，也不编一个账号）；
    /// - 风控 / 状态码照 `check_transport`（**不重试**）。
    pub fn whoami(&self) -> Result<Option<UserRef>, TiktokError> {
        if !self.logged_in() {
            return Ok(None);
        }
        // Kotlin `fetchHomeScope` 一进来就置 true（请求失败也算"问过首页"）
        *self.bootstrapped.lock().unwrap_or_else(|e| e.into_inner()) = true;
        let url = format!("{}/", self.base.trim_end_matches('/'));
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: document_headers(USER_AGENT, ROOT),
                body: Vec::new(),
            })
            .map_err(TiktokError::from)?;
        let body = body_text(&response);
        check_transport(&response, &body)?;
        // 服务端在响应里下发的新 msToken（和 `ensure_device` 一样写回 store）
        if let Some(token) = response_header(&response, "x-ms-token") {
            if !token.trim().is_empty() {
                self.store.put(KEY_MS_TOKEN, Some(&token));
            }
        }
        let Some(scope) = hydration(&body) else {
            return Ok(None);
        };
        self.remember_device(json::obj(&scope, "webapp.app-context"));
        Ok(crate::tiktok::self_user(&scope))
    }

    fn stored(&self, key: &str) -> Option<String> {
        self.store.get(key).filter(|v| !v.trim().is_empty())
    }

    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies.find(COOKIE_URL, name)
    }

    /// 设备标识。拿不到就是 `None`（不编设备号）。
    fn device_id(&self) -> Option<String> {
        self.stored(KEY_DEVICE_ID).or_else(|| self.multi_sids())
    }

    /// `multi_sids` 的第一段（`<device_id>%3A<…>` 或 `:` 分隔）。TiktokApis 的 `_sync_ids_from_cookies`。
    fn multi_sids(&self) -> Option<String> {
        let raw = self.cookie("multi_sids").filter(|v| !v.trim().is_empty())?;
        let replaced = raw.replace("%3A", ":").replace("%3a", ":");
        let first = replaced.split(':').next().unwrap_or("").trim().to_string();
        let len = first.chars().count();
        if (16..=20).contains(&len) && first.chars().all(|c| c.is_ascii_digit()) {
            Some(first)
        } else {
            None
        }
    }

    /// 一次请求要用的环境值（Kotlin `TiktokDevice` 的 getter 们）。
    fn device(&self, now_ms: i64) -> Result<Device, TiktokError> {
        let device_id = self.device_id();
        let odin_id = self
            .stored(KEY_ODIN_ID)
            .or_else(|| self.multi_sids())
            .or_else(|| device_id.clone());
        let region = self.region();
        let web_id_last_time = self.web_id_last_time(now_ms);
        let verify_fp = self.verify_fp()?;
        let ms_token = self
            .stored(KEY_MS_TOKEN)
            .or_else(|| self.cookie("msToken").filter(|v| !v.trim().is_empty()))
            .unwrap_or_default();
        let client_ab_versions = self
            .stored(KEY_CLIENT_AB)
            .unwrap_or_else(|| DEFAULT_CLIENT_AB_VERSIONS.to_string());
        Ok(Device {
            user_agent: USER_AGENT.to_string(),
            browser_version: USER_AGENT
                .split_once("Mozilla/")
                .map(|(_, tail)| tail.to_string())
                .unwrap_or_else(|| USER_AGENT.to_string()),
            device_id,
            odin_id,
            region,
            web_id_last_time,
            verify_fp,
            ms_token,
            client_ab_versions,
        })
    }

    /// 地区：store → `store-country-code` Cookie（大写，两位）→ `SG`。
    fn region(&self) -> String {
        if let Some(stored) = self.stored(KEY_REGION) {
            return stored;
        }
        if let Some(raw) = self.cookie("store-country-code") {
            let upper = raw.trim().to_uppercase();
            if upper.chars().count() == 2 {
                return upper;
            }
        }
        "SG".to_string()
    }

    /// `WebIdLastTime`：store 里的 `webIdCreatedTime`，没有就记下首次用到的那一刻。
    fn web_id_last_time(&self, now_ms: i64) -> i64 {
        if let Some(stored) = self.stored(KEY_WEB_ID_TIME) {
            if let Ok(value) = stored.parse::<i64>() {
                return value;
            }
        }
        let value = now_ms / 1000;
        self.store.put(KEY_WEB_ID_TIME, Some(&value.to_string()));
        value
    }

    /// `s_v_web_id`（= query 里的 `verifyFp`）：**只从 Cookie 罐 / store 里读真值，绝不伪造**。
    ///
    /// 生成留在 Kotlin 的 `TiktokDevice`（`RustTiktokSource` 在第一次 `feed` 之前让它落到 store）；
    /// Rust 这里两处都没有就报错、**一个请求都不发**——和酷安"设备由 Kotlin 生成、Rust 只读"同一条规矩。
    fn verify_fp(&self) -> Result<String, TiktokError> {
        if let Some(cookie) = self.cookie("s_v_web_id").filter(|v| !v.trim().is_empty()) {
            return Ok(cookie);
        }
        if let Some(stored) = self.stored(KEY_VERIFY_FP) {
            return Ok(stored);
        }
        Err(TiktokError::Internal {
            detail: format!(
                "Cookie 罐里没有 s_v_web_id，store 里也没有 {KEY_VERIFY_FP}（由 Kotlin 的 TiktokDevice 生成，Rust 不新造）"
            ),
        })
    }

    /// 缺 `device_id` 时先补一次首页（`GET /`）：取 `webapp.app-context` 的 wid / odinId / region /
    /// webIdCreatedTime，以及响应头 `x-ms-token`（服务端下发的新 msToken，照 Kotlin 写回）。
    fn ensure_device(&self) -> Result<(), TiktokError> {
        if self.device_id().is_some() || *self.bootstrapped.lock().unwrap_or_else(|e| e.into_inner()) {
            return Ok(());
        }
        // Kotlin 在请求**之前**就把 bootstrapped 置 true（首页失败也不会反复重试）。
        *self.bootstrapped.lock().unwrap_or_else(|e| e.into_inner()) = true;
        let url = format!("{}/", self.base.trim_end_matches('/'));
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: document_headers(USER_AGENT, ROOT),
                body: Vec::new(),
            })
            .map_err(TiktokError::from)?;
        let body = body_text(&response);
        check_transport(&response, &body)?;
        // 服务端在响应里下发的新 msToken（3MH：`set-cookie` 与 `x-ms-token` 两条路；Cookie 罐那条归宿主）
        if let Some(token) = response_header(&response, "x-ms-token") {
            if !token.trim().is_empty() {
                self.store.put(KEY_MS_TOKEN, Some(&token));
            }
        }
        let Some(scope) = hydration(&body) else {
            return Ok(());
        };
        let context = json::obj(&scope, "webapp.app-context");
        self.remember_device(context);
        Ok(())
    }

    fn remember_device(&self, context: Option<&JsonMap>) {
        let Some(context) = context else { return };
        let wid = json::text(context, "wid");
        let odin = json::text(context, "odinId");
        let region = json::text(context, "region").or_else(|| json::text(context, "userRegion"));
        let created = json::long(context, "webIdCreatedTime");
        if let Some(wid) = wid {
            self.store.put(KEY_DEVICE_ID, Some(&wid));
        }
        if let Some(odin) = odin {
            self.store.put(KEY_ODIN_ID, Some(&odin));
        }
        if let Some(region) = region {
            self.store.put(KEY_REGION, Some(&region.to_uppercase()));
        }
        if let Some(created) = created.filter(|v| *v > 0) {
            self.store.put(KEY_WEB_ID_TIME, Some(&created.to_string()));
        }
    }
}

/// 推荐流的 query（Kotlin `commonParams(...) + mapOf(...)`，最后按 **ASCII 键名升序**排）。
///
/// 抓包 URL 里就是这个顺序，而顺序是签名输入的一部分（`X-Dynosaur` 的 `0x2E` 覆盖整串）。
pub fn recommend_params(device: &Device, now_ms: i64, cursor: Option<&str>) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: &str| params.push((k.to_string(), v.to_string()));
    push("WebIdLastTime", &device.web_id_last_time.to_string());
    push("aid", "1988");
    push("app_language", "zh-Hans");
    push("app_name", "tiktok_web");
    push("browser_language", BROWSER_LANGUAGE);
    push("browser_name", BROWSER_NAME);
    push("browser_online", "true");
    push("browser_platform", BROWSER_PLATFORM);
    push("browser_version", &device.browser_version);
    push("channel", "tiktok_web");
    push("cookie_enabled", "true");
    push("data_collection_enabled", "true");
    if let Some(id) = &device.device_id {
        push("device_id", id);
    }
    push("device_platform", "web_pc");
    push("focus_state", "true");
    push("from_page", "fyp");
    push("history_len", "2");
    push("is_fullscreen", "false");
    push("is_page_visible", "true");
    if let Some(odin) = &device.odin_id {
        push("odinId", odin);
    }
    push("os", "windows");
    push("priority_region", &device.region);
    push("referer", "");
    push("region", &device.region);
    push("root_referer", "");
    push("screen_height", SCREEN_HEIGHT);
    push("screen_width", SCREEN_WIDTH);
    push("tz_name", TZ_NAME);
    push("verifyFp", &device.verify_fp);
    push("webcast_language", "zh-Hans");
    // 推荐流特有（TiktokApis 的 `get_recommend_feed`）
    push("clientABVersions", &device.client_ab_versions);
    push("count", &PAGE_SIZE.to_string());
    push("coverFormat", "2");
    push("cpu_core_number", CPU_CORE_NUMBER);
    push("dark_mode", "false");
    push("day_of_week", "0");
    push("enable_cache", "false");
    push("isNonPersonalized", "false");
    push("is_new_user", "true");
    push("language", "zh-Hans");
    push("pullType", "1");
    push("showAboutThisAd", "true");
    push("showAds", "true");
    push("time_of_day", "5");
    push("video_encoding", "dash");
    push("vv_count", "0");
    push("vv_count_fyp", "0");
    push("watchLiveLastTime", &now_ms.to_string());
    if let Some(cursor) = cursor {
        push("cursor", cursor);
    }
    drop(push);
    params.sort_by(|a, b| a.0.cmp(&b.0));
    params
}

/// 视频页地址（query 的 `referer` / `root_referer` / 请求头 Referer 都用它）。
pub fn video_page_url(unique_id: &str, item_id: &str) -> String {
    format!("https://www.tiktok.com/@{unique_id}/video/{item_id}")
}

/// 评论 / 楼中楼共用的"当前环境"参数（Kotlin `TiktokSource.commonParams`）。
///
/// 顺序在这里**无所谓**：调用方最后统一按 ASCII 键名升序排（签名覆盖的是整串，顺序是输入的一部分）。
/// `from_page` 为 `None` 时**不塞**这个键（和 Kotlin 的 `if (fromPage != null)` 一样）；
/// `user_is_login` 为真才塞 `user_is_login=true`（推荐流不塞）。
fn common_params(
    device: &Device,
    query_referer: &str,
    root_referer: &str,
    from_page: Option<&str>,
    history_len: i32,
    app_language: &str,
    user_is_login: bool,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: String| out.push((k.to_string(), v));
    push("WebIdLastTime", device.web_id_last_time.to_string());
    push("aid", "1988".to_string());
    push("app_language", app_language.to_string());
    push("app_name", "tiktok_web".to_string());
    push("browser_language", BROWSER_LANGUAGE.to_string());
    push("browser_name", BROWSER_NAME.to_string());
    push("browser_online", "true".to_string());
    push("browser_platform", BROWSER_PLATFORM.to_string());
    push("browser_version", device.browser_version.clone());
    push("channel", "tiktok_web".to_string());
    push("cookie_enabled", "true".to_string());
    push("data_collection_enabled", "true".to_string());
    if let Some(device_id) = &device.device_id {
        push("device_id", device_id.clone());
    }
    push("device_platform", "web_pc".to_string());
    push("focus_state", "true".to_string());
    if let Some(from_page) = from_page {
        push("from_page", from_page.to_string());
    }
    push("history_len", history_len.to_string());
    push("is_fullscreen", "false".to_string());
    push("is_page_visible", "true".to_string());
    if let Some(odin_id) = &device.odin_id {
        push("odinId", odin_id.clone());
    }
    push("os", "windows".to_string());
    push("priority_region", device.region.clone());
    push("referer", query_referer.to_string());
    push("region", device.region.clone());
    push("root_referer", root_referer.to_string());
    push("screen_height", SCREEN_HEIGHT.to_string());
    push("screen_width", SCREEN_WIDTH.to_string());
    push("tz_name", TZ_NAME.to_string());
    if user_is_login {
        push("user_is_login", "true".to_string());
    }
    push("verifyFp", device.verify_fp.clone());
    push("webcast_language", "zh-Hans".to_string());
    drop(push);
    out
}

fn pair(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// 接口（XHR）头，**顺序照 Kotlin 版最终落到线上的顺序**：
/// `apiHeaders()` 的 6 个，然后 `DefaultHeadersInterceptor` 追加的浏览器身份头（UA / Accept-Language /
/// sec-ch-ua* / sec-ch-ua-platform）。这里一次写全，宿主拦截器因此一个都不会再补。
pub fn api_headers(user_agent: &str, referer: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Accept".to_string(), "*/*".to_string()),
        ("Referer".to_string(), referer.to_string()),
        ("priority".to_string(), "u=1, i".to_string()),
        ("sec-fetch-dest".to_string(), "empty".to_string()),
        ("sec-fetch-mode".to_string(), "cors".to_string()),
        ("sec-fetch-site".to_string(), "same-origin".to_string()),
    ];
    headers.extend(browser_headers(user_agent));
    headers
}

/// 文档（页面）头，顺序照 Kotlin `documentHeaders()` + 拦截器追加的那批。
pub fn document_headers(user_agent: &str, referer: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Accept".to_string(), DOCUMENT_ACCEPT.to_string()),
        ("Cache-Control".to_string(), "no-cache".to_string()),
        ("Pragma".to_string(), "no-cache".to_string()),
        ("Referer".to_string(), referer.to_string()),
        ("priority".to_string(), "u=0, i".to_string()),
        ("sec-fetch-dest".to_string(), "document".to_string()),
        ("sec-fetch-mode".to_string(), "navigate".to_string()),
        ("sec-fetch-site".to_string(), "same-origin".to_string()),
        ("sec-fetch-user".to_string(), "?1".to_string()),
        ("Upgrade-Insecure-Requests".to_string(), "1".to_string()),
    ];
    headers.extend(browser_headers(user_agent));
    headers
}

/// 每条请求都带的浏览器身份头（和签名里的环境是同一套）。
fn browser_headers(user_agent: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), user_agent.to_string()),
        ("Accept-Language".to_string(), ACCEPT_LANGUAGE.to_string()),
        ("sec-ch-ua".to_string(), SEC_CH_UA.to_string()),
        ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
        ("sec-ch-ua-platform".to_string(), UA_PLATFORM.to_string()),
    ]
}

fn body_text(response: &HttpResponse) -> String {
    String::from_utf8_lossy(&response.body).into_owned()
}

fn response_header(response: &HttpResponse, name: &str) -> Option<String> {
    header_value(&response.headers, name).map(|v| v.to_string())
}

/// 传输层的风控信号。识别到就**停下抛异常，不重试、不换参数**。
pub fn check_transport(response: &HttpResponse, body: &str) -> Result<(), TiktokError> {
    // 三层拒绝（签名环境码 / 伪造 msToken / TLS 指纹）的统一表现：HTTP 200 + 空 body + tt_orcas_res: 1
    if response_header(response, "tt_orcas_res").is_some() {
        return Err(TiktokError::RiskControl {
            code: Some("tt_orcas_res".to_string()),
            detail: "Orcas 拒绝了这次请求（签名 / msToken / TLS 指纹三层之一）".to_string(),
        });
    }
    if response.status == 403 || response.status == 429 {
        return Err(TiktokError::RiskControl {
            code: Some(format!("HTTP {}", response.status)),
            detail: "服务端拒绝（403 / 429；也可能签名不对）".to_string(),
        });
    }
    if !(200..=299).contains(&response.status) {
        return Err(TiktokError::Api {
            code: format!("HTTP {}", response.status),
            detail: body.chars().take(120).collect(),
        });
    }
    if response.body.is_empty() {
        return Err(TiktokError::RiskControl {
            code: Some("empty-body".to_string()),
            detail: "HTTP 200 + 空 body（签名 / msToken / TLS 指纹拒绝的统一表现）".to_string(),
        });
    }
    if body.contains("__ac_nonce") || body.contains("$jsvmprt") {
        return Err(TiktokError::RiskControl {
            code: Some("acrawler".to_string()),
            detail: "拿到 JS 挑战页，不是接口响应".to_string(),
        });
    }
    if body.contains("<title>Security Check</title>") {
        return Err(TiktokError::RiskControl {
            code: Some("security-check".to_string()),
            detail: "拿到 Security Check 页（人机验证）".to_string(),
        });
    }
    Ok(())
}

/// 页面里 `__UNIVERSAL_DATA_FOR_REHYDRATION__` 的 `__DEFAULT_SCOPE__`（Kotlin `TiktokParser.hydration`）。
pub fn hydration(html: &str) -> Option<JsonMap> {
    const MARKER: &str = "id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\"";
    let start = html.find(MARKER)?;
    let open = html[start..].find('>').map(|i| start + i)?;
    let end = html[open..].find("</script>").map(|i| open + i)?;
    let raw = html[open + 1..end].trim();
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    object
        .get("__DEFAULT_SCOPE__")
        .and_then(|v| v.as_object())
        .cloned()
}

/// 详情 id（`<作者>/<itemId>` 或裸 `<itemId>`）的形状：作者只认 `[A-Za-z0-9._]`、itemId 只认数字。
/// 两段都会**原样拼进视频页路径**（`/@<作者>/video/<itemId>`），所以不合法就 `bad_id`、**连补设备数据的
/// 首页请求都不发**。
fn check_id(id: &str) -> Result<(), TiktokError> {
    let item_id = crate::tiktok::item_id_of(id);
    let item_ok = !item_id.is_empty() && item_id.bytes().all(|b| b.is_ascii_digit());
    let author_ok = crate::tiktok::unique_id_of(id).map_or(true, |u| {
        u.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
    });
    if item_ok && author_ok {
        return Ok(());
    }
    Err(TiktokError::Api {
        code: "bad_id".to_string(),
        detail: format!(
            "不认识的 TikTok id：{}（应该形如 `<作者>/<数字 itemId>`）",
            json::take_chars(id, 60)
        ),
    })
}

