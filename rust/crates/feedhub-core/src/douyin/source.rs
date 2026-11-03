//! 抖音 Source 的**平台层**：推荐流（"精选" `/aweme/v1/web/module/feed/`）的整条请求——
//! 拼 URL / 参数顺序 / `version_code` 那几套常量 / 请求头顺序 / 翻页游标 / a_bogus /
//! 空 body 与业务码 8 与两条风控头的判定——再加解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），
//! 这里不碰 socket。
//!
//! 逐条对照 `source-douyin/DouyinSource.kt` 的同名方法（`recommend` / `unsignedUrl` / `runtimeParams` /
//! `identityParams` / `headers` / `checkTransport` / `check`）和 `DouyinDevice.kt` 的那组环境值：
//! 方法 + URL + 参数顺序 + 请求头顺序 + body 必须和 Kotlin 版**逐字节一样**
//! （`DouyinRequestEquivalenceTest` 用 MockWebServer 录两次对比守着）。
//!
//! 播放 / 表情表 / 扫码 / `whoami` 还在 Kotlin（`RustDouyinSource` 逐方法委托）。
//!
//! 设备（UA / 平台串 / 屏幕 / 核数 / 内存 / `install_time` / `verify_fp`）由 Kotlin 的 `DouyinDevice`
//! 读好传进来，**Rust 不生成设备、不伪造身份、不取随机数、不读系统时钟**：
//! - `install_time` / `verify_fp` 是 `DouyinDevice` 从 `ctx.store`（`douyin.install_time` /
//!   `douyin.verify_fp`）取的那一份，缺了它会生成再传过来；Rust 只是用，不会另造一份。
//! - `webid` / `msToken` / `uifid` 这些"服务端签发 / 页面上才有"的标识，Rust 自己按 Kotlin 的顺序
//!   从 `ctx.store`（`douyin.webid` / `douyin.ms_token`）和 Cookie 罐里读；**没有就不塞**（不编值）。
//! - 时间（`now_ms`）和 a_bogus 的随机数流由调用方给：随机数走上一期那条"先问个数、再取"
//!   （`douyin_abogus_random_count`），所以两边的随机流完全对齐。

use std::sync::{Arc, Mutex};

use crate::detail::{CommentPage, FeedDetail};
use crate::json::{self, JsonMap};
use crate::play::PlayInfo;
use crate::douyin::emotes::{self, EmoteTable};
use crate::douyin::{self, sign, DouyinError};
use crate::host::{CookieHost, HostError, HttpHost, HttpRequest, HttpResponse, RandomHost, StoreHost};
use crate::hosted::has_header;
use crate::model::{Page, UserRef};

/// 接口根（和 Kotlin `DOUYIN_API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://www.douyin.com";

/// Cookie 罐的"身份 URL"（Kotlin `DouyinSource.WWW`）：登录态 / `uifid` / `s_v_web_id` 都按它查。
const WWW: &str = "https://www.douyin.com/";

/// 推荐流路径。
pub const MODULE_FEED_PATH: &str = "/aweme/v1/web/module/feed/";

/// 详情路径（**在 secsdk 的 webSign 保护表里**：除了 a_bogus 还要 `timestamp` + `x-secsdk-web-signature`）。
pub const AWEME_DETAIL_PATH: &str = "/aweme/v1/web/aweme/detail/";

/// 一级评论路径（不在 secsdk 表里，只要 a_bogus）。
pub const COMMENT_LIST_PATH: &str = "/aweme/v1/web/comment/list/";

/// 楼中楼路径（同样不在 secsdk 表里；`verifyFp` / `fp` 要放在 `a_bogus` 之后）。
pub const COMMENT_REPLY_PATH: &str = "/aweme/v1/web/comment/list/reply/";

/// 默认 Referer（Kotlin `headers()` 里的那一条）。
const REFERER: &str = "https://www.douyin.com/";

/// `module_id=3003101` 是"精选"。
const MODULE_ID: &str = "3003101";

/// 站点 aid（www / live / login 都是 6383）。
const AID: &str = "6383";

/// 一页 20 条（Kotlin `PAGE_SIZE`）。
pub const PAGE_SIZE: i32 = 20;

/// 推荐流这套公共参数用的前端版本号（真实页面每个前端模块自带一套，`aweme/detail` 是 190500）。
pub const VERSION_CODE: &str = "170400";
pub const VERSION_NAME: &str = "17.4.0";
pub const UPDATE_VERSION_CODE: &str = "170400";

/// `aweme/detail` 的前端模块自带另一套版本号。
pub const DETAIL_VERSION_CODE: &str = "190500";
pub const DETAIL_VERSION_NAME: &str = "19.5.0";

/// 一级评论一页 20 条（Kotlin `COMMENT_PAGE_SIZE`）。
pub const COMMENT_PAGE_SIZE: i32 = 20;

/// 楼中楼一页 5 条（cv-cat 翻页用 5，第一页它用 3，这里统一 5）。
pub const REPLY_PAGE_SIZE: i32 = 5;

/// `whoami`：`/aweme/v1/web/user/profile/self/`（Kotlin `PROFILE_SELF_PATH`）。
const PROFILE_SELF_PATH: &str = "/aweme/v1/web/user/profile/self/";

/// 判定登录态的 Cookie（和 Kotlin `LOGIN_COOKIES` 一样：cv-cat 以 `sessionid` 判定）。
const LOGIN_COOKIES: [&str; 3] = ["sessionid", "sessionid_ss", "sid_tt"];

/// `webid` 在 `ctx.store` 里的键（Kotlin 里写死在 `identityParams` 的 `storeOrCookie` 参数里）。
pub const WEBID_KEY: &str = "douyin.webid";

/// `msToken` 在 `ctx.store` 里的键。
pub const MS_TOKEN_KEY: &str = "douyin.ms_token";

/// 抖音 web 端的"设备环境"（Kotlin `DouyinDevice` 的那几个值）。
///
/// **不是 Rust 造的**：这些值由 Kotlin 的 `DouyinDevice` 提供。
/// 为什么放到构造时给一次：它们同时要进 query、请求头和 a_bogus 的窗口指纹串，**必须成套**，
/// 而且 Rust 一份、Kotlin 一份迟早会漂。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// 和这次抓包的 UA 逐字节一致（`sdkMinor` 就是拿它解出来的）。
    pub user_agent: String,
    /// 平台串（`Linux x86_64`）：窗口指纹串最后一段、`browser_platform` 都用它。
    pub platform: String,
    /// `sec-ch-ua-platform` 的写法（带引号，`"Linux"`）。
    pub ua_platform: String,
    pub os_name: String,
    pub os_version: String,
    /// `pc_libra_divert`。
    pub libra_divert: String,
    /// Chrome 主版本（`155`），`browser_version` / `engine_version` / `sec-ch-ua` 都用它。
    pub browser_major: String,
    pub screen_width: i32,
    pub screen_height: i32,
    pub cpu_core_num: i32,
    pub device_memory: i32,
    /// `install_time` 参数：Kotlin `DouyinDevice.installTime`（秒；`ctx.store` 的 `douyin.install_time`）。
    pub install_time: i64,
    /// `verifyFp` 的兜底值：`ctx.store` 的 `douyin.verify_fp`（Cookie 里有 `s_v_web_id` 时用 Cookie 的）。
    pub verify_fp: String,
}

/// 一页推荐流 + 这一页所有卡片的 `desc` 拼串 + **原始响应体** + 这一页的 `refresh_index`。
///
/// 为什么要带 body：Kotlin 那边的流程是"先解析一遍拿 `desc` 拼串 → 有表情记号才去取表情表 → 再映射一遍"。
/// 第二遍必须用**同一份响应**（不能再发一次请求，请求条数要和以前完全一样），所以把 body 交回调用方，
/// 它用纯函数 `douyin_parse_feed` 带表情表再映射一次。`texts` 是给调用方判断"要不要取表情表"的。
#[derive(Debug, Clone, PartialEq)]
pub struct HostedFeed {
    pub page: Page,
    pub texts: String,
    pub body: String,
    pub refresh_index: i32,
}

/// 详情一页 + `desc`（判断要不要取表情表）+ **原始响应体**（第二遍解析用；`play` 复用那份缓存也要它）。
#[derive(Debug, Clone, PartialEq)]
pub struct HostedDetail {
    pub detail: FeedDetail,
    pub texts: String,
    pub body: String,
}

/// 一页评论 + 这一页所有评论正文的拼串 + **原始响应体**（第二遍解析用）。
#[derive(Debug, Clone, PartialEq)]
pub struct HostedComments {
    pub page: CommentPage,
    pub texts: String,
    pub body: String,
}

/// 抖音的平台对象：持有三个宿主端口 + 设备环境 + a_bogus 的常量 / 浏览器环境。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustDouyinHosted`）建一次、跟着 Source 活；Source 被 App 缓存到进程结束。
/// **没有后台线程、没有定时器、没有连接池**——闲置时零唤醒。
pub struct DouyinHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    device: Device,
    profile: sign::Profile,
    env: sign::Env,
    /// 进入页面（SDK 加载）的时间，只参与 a_bogus 的 `U[28]`。Kotlin 在建 Source 时取一次。
    enter_page_ts: i64,
    /// 最近取过的作品（`aweme_id` → 取的时刻 + **原始响应体**）：点开一条视频是详情先取 `aweme/detail`、
    /// 播放紧接着要同一条作品的地址，[`RECENT_AWEMES`] 内不再打一次 secsdk 保护路径。
    /// Kotlin 是 `LinkedHashMap`（访问序、最多 8 条），这里是 `Vec` + 手写的 LRU。
    /// 存**原始 body**（不存解析后的对象）：详情的第二遍解析（带表情表）按原样文本走，和 Kotlin 一样。
    recent: Mutex<Vec<(String, i64, String)>>,
    /// 表情表：内存里那份 + 两次补取之间的节流状态（Kotlin `emoteCache` / `emoteRetryAtMs`）。
    ///
    /// 不再把 `desc` / 正文拼串交回 Kotlin、等它取表、再拿整份 body 映射第二遍。
    emotes: Mutex<EmoteState>,
    /// a_bogus 的随机数来源（宿主给）。`None` = 用调用方传进来的那串（测试口径）。
    /// 用 `RwLock` 而不是构造时给一次：FFI 对象的构造顺序是"先建 Source、再装宿主能力"。
    ///
    /// 为什么要它：表情表搬进 Rust 之后，"这一趟要不要多签一个 `emoji/list`"只有当 Rust 解析完响应
    /// 才知道。硬让调用方预抽两份会**改掉随机流**（少发一次就多抽了一份，后面的请求全错位），
    /// 所以改成"签的那一刻问宿主抽"——抽取的个数和顺序天然和 Kotlin 的 `AbogusSigner` 逐位一致。
    random: std::sync::RwLock<Option<Arc<dyn RandomHost>>>,
}

/// 表情表的运行时状态（Kotlin `DouyinSource` 里那两个字段）。
#[derive(Debug, Default)]
struct EmoteState {
    /// 内存里那份（Kotlin `emoteCache`：拿到就存，**不再看 TTL**；过期判断在读 `ctx.store` 那一步）。
    table: Option<emotes::EmoteTable>,
    /// 下一次允许再试的时间（毫秒，Kotlin `emoteRetryAtMs`）。
    retry_at_ms: i64,
    /// 有没有一个在飞的请求（`xhs/emotes.rs` 同一条口径：表是锦上添花，不该重复占风控名额）。
    fetching: bool,
}

/// 最近作品的缓存条数上限（Kotlin `RECENT_AWEMES`）。
pub const RECENT_AWEMES: usize = 8;

/// `play` 复用详情那次的时限（Kotlin `AWEME_REUSE_MS`）：60 秒。
pub const AWEME_REUSE_MS: i64 = 60_000;

impl DouyinHostedSource {
    pub fn new(
        base: impl Into<String>,
        device: Device,
        profile: sign::Profile,
        env: sign::Env,
        enter_page_ts: i64,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            store,
            cookies,
            device,
            profile,
            env,
            enter_page_ts,
            recent: Mutex::new(Vec::new()),
            emotes: Mutex::new(EmoteState::default()),
            random: std::sync::RwLock::new(None),
        }
    }

    /// 装上 a_bogus 的随机数来源（生产路径：宿主从它自己的 `Random` 里按签名顺序抽）。
    ///
    /// 不给也能用（`rand` 参数那条路），但那条路只在"一次调用只签一个请求"时才对得上——
    /// 详情 / 评论里出现表情记号会多签一次 `emoji/list`，所以生产**必须**装上。
    pub fn set_random(&self, random: Arc<dyn RandomHost>) {
        *self.random.write().unwrap_or_else(|e| e.into_inner()) = Some(random);
    }

    /// 一次签名的随机数：有宿主就问宿主抽（流和 Kotlin 逐位对齐）；没有就用调用方给的那串
    /// （测试口径：所有签名共用同一串，`vec![0.5; N]` 这类固定值下等价）。
    fn draw_rand(&self, rand: &[f64], count: usize) -> Vec<f64> {
        if let Some(host) = self.random.read().unwrap_or_else(|e| e.into_inner()).clone() {
            return host.draw(count);
        }
        // 不够就原样交回去：`sign` 自己会报"随机数不够"（测试守的那条，不在这里悄悄补 0）
        rand.to_vec()
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 一页推荐流。
    ///
    /// `now_ms` 是**毫秒级**时间戳（a_bogus 用它、`douyin_abogus_random_count` 也用它），由调用方注入——
    /// Rust 不读系统时钟，测试才能把两次运行的请求逐字节比。`rand` 是按
    /// `sign::random_count(env, now_ms)` 取好的随机数流（少了会报 `Internal`，不会偷偷补）。
    pub fn feed(
        &self,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<HostedFeed, DouyinError> {
        // 没登录就直接失败，**一个请求都不发**。
        if !self.logged_in() {
            return Err(DouyinError::NotLoggedIn {
                detail: Some("抖音推荐流要登录：先在“我的”里导入抖音 Cookie".to_string()),
            });
        }
        let refresh_index = parse_refresh_index(cursor);
        let params = self.feed_params(refresh_index);
        let raw_query = query_of(&params);
        // Kotlin 是 `(base + path + "?" + query).toHttpUrl()`，签名签的是 OkHttp **规范化之后**的 query。
        // 这一步必须一样，否则"签的和发的"差一个字节（`'` → `%27`），请求等价门会立刻抓到。
        let query = okhttp_canonical_query(&raw_query);
        let count = sign::random_count(&self.env, now_ms);
        let values = self.draw_rand(rand, count);
        let bogus = sign::sign(
            &self.profile,
            &self.env,
            &query,
            now_ms,
            self.enter_page_ts,
            &values,
        )?;
        // `module/feed` 不在 secsdk 的 webSign 保护表里，所以只要 a_bogus（`secsdk_is_protected` 有断言守着）。
        let signed_query = format!("{query}&a_bogus={bogus}");
        let url = format!(
            "{}{}?{}",
            self.base.trim_end_matches('/'),
            MODULE_FEED_PATH,
            signed_query
        );
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: self.request_headers(None),
                body: Vec::new(),
            })
            .map_err(DouyinError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        // 第一遍解析**不带表情表**（和 Kotlin `parse.feed(res, refreshIndex, emptyMap())` 一样）：
        // 看到 `texts` 里有记号才去取表情表，再拿**同一份 body** 映射第二遍（不多发一个请求）。
        let first = douyin::parse_feed(
            &body,
            response.status as i32,
            signals(&response),
            refresh_index as i64,
            &[],
        )?;
        let table = self.emotes_for(&first.texts, now_ms, rand)?;
        let page = if table.is_empty() {
            first.page
        } else {
            douyin::parse_feed(
                &body,
                response.status as i32,
                signals(&response),
                refresh_index as i64,
                &table,
            )?
            .page
        };
        Ok(HostedFeed {
            page,
            texts: first.texts,
            body,
            refresh_index,
        })
    }

    /// 详情：`GET /aweme/v1/web/aweme/detail/?aweme_id=…`。
    ///
    /// 参数照 cv-cat 的 `get_work_info`（`request_source=600`、`origin_type=video_page`），
    /// **这个接口的 `version_code` 是 190500 / 19.5.0**（和 `module/feed` 的 170400 不同 ——
    /// 真实页面每个前端模块自带版本号），而且它在 secsdk 保护表里，所以 [`Self::send`] 会再补
    /// `timestamp` + `x-secsdk-web-signature`。
    ///
    /// 第一遍解析**不带表情表**（和 Kotlin 一样：请求条数不能因为表情表多一条）；调用方拿 `texts`
    /// 判断要不要取表情表，要就带表用纯函数再映射一遍。
    pub fn detail(&self, id: &str, now_ms: i64, rand: &[f64]) -> Result<HostedDetail, DouyinError> {
        let (json, body) = self.fetch_aweme(id, now_ms, rand)?;
        // 正文（`desc`）里有 `[赞]` 这样的记号才去取表情表（和卡片 / 评论同一张表，缓存一周）；
        // 取不到就保持文字。表在 Rust 里，所以这里直接解析两遍，不再把 body 交回宿主。
        let texts = douyin::detail::texts(&json);
        let table = self.emotes_for(&texts, now_ms, rand)?;
        let detail = douyin::detail::parse(&json, id, &table)?;
        Ok(HostedDetail { detail, texts, body })
    }

    /// 播放地址（`DouyinVideo.play` 的移植）：`aweme/detail` 里的 `video.bit_rate[].play_addr`。
    ///
    /// [`AWEME_REUSE_MS`]（60 秒）内 [`detail`](Self::detail) 取过同一条作品就直接用那份响应，
    /// **不再打一次 secsdk 保护路径**（点开视频时详情刚取过，省掉一个请求和 1～1.5 秒限速）；
    /// 没有才现取一次。地址里有 `l` / `ft` / `sign` 这类时效参数，旧的会过期。
    ///
    /// 各档清晰度、AVC / HEVC 都列出来（[`crate::douyin::play`]），由前端按设置和硬解能力挑。
    /// 抖音是音画一体的 mp4，所以走 [`PlayInfo::progressive`]，没有 DASH 轨、也没有弹幕。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        quality: Option<i32>,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<PlayInfo, DouyinError> {
        let json = match self.recent_aweme(id, now_ms) {
            Some(body) => douyin::decode_body(&body)?,
            None => self.fetch_aweme(id, now_ms, rand)?.0,
        };
        let aweme = json::obj(&json, "aweme_detail").ok_or_else(|| {
            douyin::detail::not_found(&json, id)
        })?;
        let headers = self.stream_headers();
        douyin::play::play(aweme, part_id, quality, headers)
    }

    /// UA 用和接口一致的那套。
    /// **没有 Cookie**（`PlayInfo` 会进日志 / 本机中转，Cookie 不是它该带的东西）。
    pub fn stream_headers(&self) -> Vec<(String, String)> {
        vec![
            pair("Referer", REFERER),
            pair("User-Agent", self.device.user_agent.clone()),
        ]
    }

    /// `aweme/detail` 的响应（解析后的对象 + **原始 body**）；取到有 `aweme_detail` 的就记进
    /// [`Self::recent`]（和 Kotlin `fetchAweme` 的 `.also` 条件一样）。详情和播放共用这一份。
    fn fetch_aweme(
        &self,
        id: &str,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<(JsonMap, String), DouyinError> {
        let get = Get {
            path: AWEME_DETAIL_PATH.to_string(),
            params: vec![
                pair("aweme_id", id),
                pair("request_source", "600"),
                pair("origin_type", "video_page"),
            ],
            referer: Some(douyin::video_url(id)),
            ids: Ids::default(),
            after_sign: Vec::new(),
            version_code: DETAIL_VERSION_CODE,
            version_name: DETAIL_VERSION_NAME,
            round_trip_time: "50",
        };
        let (response, body) = self.send(&get, now_ms, rand)?;
        let json = douyin::decode_response(&body, response.status as i32, signals(&response), true)?;
        // 只在 `aweme_detail` 是对象时才记（和 Kotlin `fetchAweme` 的 `.also` 条件一样）
        if json::obj(&json, "aweme_detail").is_some() {
            self.remember_aweme(id, now_ms, body.clone());
        }
        Ok((json, body))
    }

    /// 把一份 `aweme/detail` 响应记进 [`Self::recent`]（访问序、最多 [`RECENT_AWEMES`] 条）。
    fn remember_aweme(&self, id: &str, now_ms: i64, body: String) {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|(key, _, _)| key != id);
        recent.push((id.to_string(), now_ms, body));
        while recent.len() > RECENT_AWEMES {
            recent.remove(0);
        }
    }

    /// [`AWEME_REUSE_MS`] 内取过的作品；顺带把它挪到"最近用过"的位置（Kotlin 那个 `LinkedHashMap`
    /// 是访问序）。过期的不删（失败路径也可能要它），只按时间判。
    fn recent_aweme(&self, id: &str, now_ms: i64) -> Option<String> {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let index = recent.iter().position(|(key, _, _)| key == id)?;
        if now_ms - recent[index].1 > AWEME_REUSE_MS {
            return None;
        }
        let entry = recent.remove(index);
        let body = entry.2.clone();
        recent.push(entry);
        Some(body)
    }

    /// 一级评论：`GET /aweme/v1/web/comment/list/`。
    ///
    /// 参数照 cv-cat 的 `get_work_out_comment`：`whale_cut_token` / `rcFT` 是**空值字段**
    /// （浏览器确实发，别丢），`cut_version=1`；Referer 必须是作品页。游标就是响应里的 `cursor`，
    /// `has_more` 为假结束。评论不在 secsdk 表里，只要 a_bogus。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<HostedComments, DouyinError> {
        let get = Get {
            path: COMMENT_LIST_PATH.to_string(),
            params: vec![
                pair("aweme_id", id),
                pair("cursor", cursor.unwrap_or("0")),
                pair("count", COMMENT_PAGE_SIZE.to_string()),
                pair("item_type", "0"),
                pair("whale_cut_token", ""),
                pair("cut_version", "1"),
                pair("rcFT", ""),
            ],
            referer: Some(douyin::video_url(id)),
            ids: Ids::default(),
            after_sign: Vec::new(),
            version_code: VERSION_CODE,
            version_name: VERSION_NAME,
            round_trip_time: "0",
        };
        let (response, body) = self.send(&get, now_ms, rand)?;
        let json = douyin::decode_response(&body, response.status as i32, signals(&response), true)?;
        let texts = douyin::comment::texts(&json);
        // 这一页的评论正文里有表情记号才去取表，然后用**同一份解析树**（不再重解析 body）映射一遍
        let table = self.emotes_for(&texts, now_ms, rand)?;
        let page = douyin::comment::page(&json, &table);
        Ok(HostedComments { page, texts, body })
    }

    /// 楼中楼：`GET /aweme/v1/web/comment/list/reply/`。
    ///
    /// 我们照做：
    /// ① 公共参数组用同一套（`round_trip_time=0`）；② **不带 `uifid`**；③ `verifyFp` / `fp` 放在
    /// `a_bogus` **之后**、不参与签名（[`Ids`] 关掉 + `after_sign`）。三者错一个就会被判 bdturing。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<HostedComments, DouyinError> {
        let fp = self.verify_fp();
        let get = Get {
            path: COMMENT_REPLY_PATH.to_string(),
            params: vec![
                pair("item_id", id),
                pair("comment_id", comment_id),
                pair("cut_version", "1"),
                pair("cursor", cursor.unwrap_or("0")),
                pair("count", REPLY_PAGE_SIZE.to_string()),
                pair("item_type", "0"),
            ],
            referer: Some(douyin::video_url(id)),
            ids: Ids {
                uifid: false,
                verify_fp: false,
                ..Ids::default()
            },
            after_sign: vec![pair("verifyFp", fp.clone()), pair("fp", fp)],
            version_code: VERSION_CODE,
            version_name: VERSION_NAME,
            round_trip_time: "0",
        };
        let (response, body) = self.send(&get, now_ms, rand)?;
        let json = douyin::decode_response(&body, response.status as i32, signals(&response), true)?;
        let texts = douyin::comment::texts(&json);
        let table = self.emotes_for(&texts, now_ms, rand)?;
        let page = douyin::comment::page(&json, &table);
        Ok(HostedComments { page, texts, body })
    }

    // 表情表

    /// 手上有的表 → `Vec<(代码, 地址)>`（没有就是空表）。
    pub fn emote_map(&self, now_ms: i64) -> Vec<(String, String)> {
        self.cached_emote_table(now_ms).map(|t| t.map).unwrap_or_default()
    }

    /// 手上已经有的那份表（内存 → `ctx.store` 里 [`emotes::TTL_SEC`] 内那份）。**不发请求。**
    ///
    /// 和 Kotlin 一样：内存里那份**不再看 TTL**（拿到就一直用，直到进程重启 / store 里那份过期），
    /// store 里那份按取到时间算一周。
    pub fn cached_emote_table(&self, now_ms: i64) -> Option<EmoteTable> {
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(table) = &state.table {
            return Some(table.clone());
        }
        let stored = self
            .store
            .get(emotes::KEY)
            .and_then(|t| EmoteTable::from_stored(&t))?;
        if now_ms / 1000 - stored.fetched_at_sec >= emotes::TTL_SEC {
            return None;
        }
        state.table = Some(stored.clone());
        Some(stored)
    }

    /// 下一趟 [`Self::ensure_emote_table`] 会不会真的发请求（纯查询：不取表、不改状态）。
    ///
    /// 给宿主门面用：**不发请求的那一趟不能从 `Random` 里抽随机量**（签名的取用顺序要和 Kotlin
    /// 参照实现逐位对齐），所以先问这一句再决定要不要取。
    pub fn emote_fetch_needed(&self, now_ms: i64) -> bool {
        if self.cached_emote_table(now_ms).is_some() {
            return false;
        }
        let state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        !(state.fetching || now_ms < state.retry_at_ms)
    }

    /// 确保表可用：**可能要发一次请求**（`GET /aweme/v1/web/emoji/list?channel=channel_pc_web`）。
    ///
    /// 口径照 Kotlin `DouyinSource.emotesFor` / `emoteTable`：
    /// - 内存有就直接用；store 里那份没过期也用；
    /// - [`emotes::RETRY_MS`]（10 分钟）内失败过就不再试，先用旧的那份（可能空）；
    /// - 同一时间**只许一个在飞**（后来的人拿旧表、不重复发：风控名额很贵）；
    /// - 取到非空的表就写回 `ctx.store`（键 / 格式和 Kotlin 一样）；
    /// - **风控照常往外抛**（Kotlin 也是 rethrow）；别的错误（接口码 / 解析 / 网络 / 代理不可用）
    ///   都吞成"用旧表 + 10 分钟内不再试"（Kotlin 的 `catch (SourceException)` / `catch (IOException)`）。
    ///
    /// `status_code != 0` 是**空表**（Kotlin `checkEmoji`：不当成错误），空表也进内存缓存。
    pub fn ensure_emote_table(&self, now_ms: i64, rand: &[f64]) -> Result<EmoteTable, DouyinError> {
        if let Some(table) = self.cached_emote_table(now_ms) {
            return Ok(table);
        }
        // store 里过期的那份：失败时拿它顶着（Kotlin 的 `stored`）
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
                if matches!(e, DouyinError::RiskControl { .. }) {
                    return Err(e);
                }
                state.retry_at_ms = now_ms + emotes::RETRY_MS;
                Ok(stored.unwrap_or_default())
            }
        }
    }

    /// 一段文字里**真有**表情记号才去取表，返回文字里真出现过的那几项（`Emotes.used` 的口径）。
    ///
    /// 和 Kotlin `emotesFor(text)` 一样：没有记号就一个请求都不加。
    pub fn emotes_for(
        &self,
        text: &str,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<Vec<(String, String)>, DouyinError> {
        if text.is_empty() || !emotes::has_emote_token(text) {
            return Ok(Vec::new());
        }
        let table = self.ensure_emote_table(now_ms, rand)?;
        Ok(table.used(&[Some(text)]))
    }

    /// 一次真的取表请求。`channel=channel_pc_web`；**不做业务码判定**（`checkCode = false`：
    /// `status_code != 0` 就是空表，Kotlin `checkEmoji` 的口径）。
    fn fetch_emote_table(&self, now_ms: i64, rand: &[f64]) -> Result<EmoteTable, DouyinError> {
        let get = Get {
            path: emotes::PATH.to_string(),
            params: vec![pair("channel", "channel_pc_web")],
            referer: None,
            ids: Ids::default(),
            after_sign: Vec::new(),
            version_code: VERSION_CODE,
            version_name: VERSION_NAME,
            round_trip_time: "0",
        };
        let (response, body) = self.send(&get, now_ms, rand)?;
        // 传输层风控照判（Kotlin `getJson` 也是先 `checkTransport`），业务码**不判**
        let json = douyin::decode_response(&body, response.status as i32, signals(&response), false)?;
        if json::long(&json, "status_code") != Some(0) {
            // 结构不认 / 业务码不是 0：空表（Kotlin `checkEmoji` 返回 null → EMPTY）
            return Ok(EmoteTable {
                map: Vec::new(),
                fetched_at_sec: now_ms / 1000,
            });
        }
        Ok(EmoteTable::from_list(&json, now_ms / 1000))
    }

    /// 只发请求、不做任何判定（Kotlin `fetch` 的移植）：**URL / 参数顺序 / 签名 / 头顺序全在这里**。
    ///
    /// 顺序照 Kotlin `unsignedUrl`：接口自己的参数 → 公共运行环境组 → `webid` → `uifid` →
    /// `verifyFp` → `fp` → `msToken`；`a_bogus`（以及受保护路径的 `timestamp` + `x-secsdk-web-signature`、
    /// [Get::after_sign] 那几个）由 [`Self::signed_url`] 追加。
    fn send(
        &self,
        get: &Get,
        now_ms: i64,
        rand: &[f64],
    ) -> Result<(HttpResponse, String), DouyinError> {
        let url = self.signed_url(get, now_ms, rand)?;
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: self.request_headers(get.referer.as_deref()),
                body: Vec::new(),
            })
            .map_err(DouyinError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        Ok((response, body))
    }

    /// 未签名 URL → 签名后的 URL（Kotlin `signedUrl` 的移植）：
    /// 先 a_bogus，受保护路径再补 `timestamp` + `x-secsdk-web-signature`，最后是 `after_sign`
    /// 那几个**不参与签名**的参数。
    fn signed_url(&self, get: &Get, now_ms: i64, rand: &[f64]) -> Result<String, DouyinError> {
        let raw_query = query_of(&self.get_params(get));
        // Kotlin 是 `(base + path + "?" + query).toHttpUrl()`，签名签的是 OkHttp **规范化之后**的 query。
        let query = okhttp_canonical_query(&raw_query);
        let count = sign::random_count(&self.env, now_ms);
        let values = self.draw_rand(rand, count);
        let bogus = sign::sign(
            &self.profile,
            &self.env,
            &query,
            now_ms,
            self.enter_page_ts,
            &values,
        )?;
        let mut signed = if query.is_empty() {
            format!("a_bogus={bogus}")
        } else {
            format!("{query}&a_bogus={bogus}")
        };
        if sign::secsdk_is_protected(&get.path, "GET") {
            let secsdk = sign::secsdk_sign(&signed, self.uifid().as_deref(), now_ms / 1000);
            signed = format!(
                "{}&{}={}",
                secsdk.query,
                sign::SECSDK_HEADER,
                secsdk.signature
            );
        }
        for (key, value) in &get.after_sign {
            signed.push_str(&format!("&{key}={}", form_encode(value)));
        }
        Ok(format!(
            "{}{}?{}",
            self.base.trim_end_matches('/'),
            get.path,
            signed
        ))
    }

    /// 一次请求的全部参数（接口自己的 → 公共运行环境组 → 身份值，同名的不重复塞）。
    fn get_params(&self, get: &Get) -> Vec<(String, String)> {
        let mut params = get.params.clone();
        // Kotlin 是 `all.putAll(runtimeParams)`：**同名键覆盖值、位置留在第一次出现的地方**
        // （`LinkedHashMap` 的语义）。`emoji/list` 的 `channel` 两边都有，不去重就会发两次。
        for (k, v) in self.runtime_params(get.version_code, get.version_name, get.round_trip_time) {
            match params.iter_mut().find(|(name, _)| *name == k) {
                Some(slot) => slot.1 = v,
                None => params.push((k, v)),
            }
        }
        // Kotlin `identityParams(ids).forEach { (k, v) -> if (k !in all) all[k] = v }`
        for (k, v) in self.identity_params(get.ids) {
            if !params.iter().any(|(name, _)| *name == k) {
                params.push((k, v));
            }
        }
        params
    }

    /// 有 `sessionid` 系列 Cookie 才算登录。
    fn logged_in(&self) -> bool {
        LOGIN_COOKIES
            .iter()
            .any(|name| self.cookies.find(WWW, name).is_some())
    }

    /// 一页的全部参数，**顺序照 Kotlin `unsignedUrl`**：
    /// 接口自己的参数 → 公共运行环境组 → `webid` → `uifid` → `verifyFp` → `fp` → `msToken`。
    fn feed_params(&self, refresh_index: i32) -> Vec<(String, String)> {
        let mut params = vec![
            pair("module_id", MODULE_ID),
            pair("count", PAGE_SIZE.to_string()),
            pair("filterGids", ""),
            pair("presented_ids", ""),
            pair("refresh_index", refresh_index.to_string()),
            pair("refer_id", ""),
            pair("refer_type", "10"),
            pair("awemePcRecRawData", r#"{"is_client":false}"#),
            pair("Seo-Flag", "0"),
            pair("install_time", self.device.install_time.to_string()),
        ];
        params.extend(self.runtime_params(VERSION_CODE, VERSION_NAME, "0"));
        // Kotlin `identityParams(ids).forEach { (k, v) -> if (k !in all) all[k] = v }`：
        // 只有真的拿到的才塞，**不编值**。
        for (k, v) in self.identity_params(Ids::default()) {
            if !params.iter().any(|(name, _)| *name == k) {
                params.push((k, v));
            }
        }
        params
    }

    ///
    /// `version_code` / `version_name` / `round_trip_time` 每家前端模块自己一套
    /// （`module/feed` 是 170400 / 0，`aweme/detail` 是 190500 / 50）。
    fn runtime_params(
        &self,
        version_code: &str,
        version_name: &str,
        round_trip_time: &str,
    ) -> Vec<(String, String)> {
        runtime_params(&self.device, version_code, version_name, round_trip_time)
    }

    /// 三个"服务端签发 / 页面上才拿得到"的标识（加 `verifyFp` / `fp`）：本机没有就**不塞**。
    ///
    /// - `uifid`：Cookie `UIFID` / `UIFID_TEMP`；
    /// - `webid`：`ctx.store` 的 `douyin.webid` 或 Cookie `webid`；
    /// - `msToken`：`ctx.store` 的 `douyin.ms_token` 或 Cookie `msToken`；
    /// - `verifyFp` / `fp`：优先 Cookie 里的 `s_v_web_id`，没有才用 `DouyinDevice.verifyFp` 生成并存下来的那个。
    fn identity_params(&self, ids: Ids) -> Vec<(String, String)> {
        let mut params = Vec::new();
        if ids.webid {
            if let Some(v) = self.store_or_cookie(WEBID_KEY, "webid") {
                params.push(pair("webid", v));
            }
        }
        if ids.uifid {
            if let Some(v) = self.uifid() {
                params.push(pair("uifid", v));
            }
        }
        if ids.verify_fp {
            let fp = self.verify_fp();
            params.push(pair("verifyFp", fp.clone()));
            params.push(pair("fp", fp));
        }
        if ids.ms_token {
            if let Some(v) = self.store_or_cookie(MS_TOKEN_KEY, "msToken") {
                params.push(pair("msToken", v));
            }
        }
        params
    }

    /// 请求头，**顺序照 Kotlin 版最终落到线上的顺序**：`fetch()` 先放接口自己的 Referer（评论 / 详情是
    /// 作品页）、再补 `uifid`（`withDefaultHeaders()`），最后 `DefaultHeadersInterceptor` 按 `headers()`
    /// 的插入顺序补剩下的（已有的同名头跳过：所以显式 Referer 时默认的站根 Referer 不会再来一遍）。
    /// 这里一次写全，宿主拦截器因此一个都不会再补（它只补缺失的同名头）。
    fn request_headers(&self, referer: Option<&str>) -> Vec<(String, String)> {
        let d = &self.device;
        let mut headers = Vec::with_capacity(14);
        if let Some(referer) = referer {
            headers.push(pair("Referer", referer));
        }
        if let Some(uifid) = self.uifid() {
            headers.push(pair("uifid", uifid));
        }
        headers.push(pair("User-Agent", d.user_agent.clone()));
        headers.push(pair("Accept", "application/json,text/plain,*/*"));
        headers.push(pair(
            "Accept-Language",
            "zh-CN,zh;q=0.9,en;q=0.8,zh-TW;q=0.7,ja;q=0.6",
        ));
        headers.push(pair(
            "sec-ch-ua",
            format!(
                "\"Not=A?Brand\";v=\"99\", \"Google Chrome\";v=\"{}\", \"Chromium\";v=\"{}\"",
                d.browser_major, d.browser_major
            ),
        ));
        headers.push(pair("sec-ch-ua-mobile", "?0"));
        headers.push(pair("sec-ch-ua-platform", d.ua_platform.clone()));
        headers.push(pair("priority", "u=1, i"));
        headers.push(pair("sec-fetch-dest", "empty"));
        headers.push(pair("sec-fetch-mode", "cors"));
        headers.push(pair("sec-fetch-site", "same-origin"));
        if referer.is_none() {
            headers.push(pair("Referer", REFERER));
        }
        // 称网关只校验这个头是否存在；**没有实测**
        headers.push(pair("x-tt-argus", "1"));
        headers
    }

    fn verify_fp(&self) -> String {
        self.cookies
            .find(WWW, "s_v_web_id")
            .filter(|v| !is_blank(v))
            .unwrap_or_else(|| self.device.verify_fp.clone())
    }

    fn store_or_cookie(&self, store_key: &str, cookie_name: &str) -> Option<String> {
        self.store
            .get(store_key)
            .filter(|v| !is_blank(v))
            .or_else(|| {
                self.cookies
                    .find(WWW, cookie_name)
                    .filter(|v| !is_blank(v))
            })
    }

    fn uifid(&self) -> Option<String> {
        self.cookies
            .find(WWW, "UIFID")
            .filter(|v| !is_blank(v))
            .or_else(|| self.cookies.find(WWW, "UIFID_TEMP").filter(|v| !is_blank(v)))
    }

    // 账号：我是谁

    /// `whoami`：`/aweme/v1/web/user/profile/self/`（`checkCode = false`）。
    /// 没有 `sessionid` 系列 Cookie 就**一个请求都不发**（未登录时这个接口回业务码 8）。
    pub fn whoami(&self, now_ms: i64, rand: &[f64]) -> Result<Option<UserRef>, DouyinError> {
        if !self.logged_in() {
            return Ok(None);
        }
        let get = Get {
            path: PROFILE_SELF_PATH.to_string(),
            params: Vec::new(),
            referer: None,
            ids: Ids::default(),
            after_sign: Vec::new(),
            version_code: VERSION_CODE,
            version_name: VERSION_NAME,
            round_trip_time: "0",
        };
        let (response, body) = self.send(&get, now_ms, rand)?;
        Self::parse_whoami(&body, response.status as i32, signals(&response))
    }

    /// `whoami` 的解析部分（扫码登录验证时用同一份）。
    /// `status_code` 0 → 账号；8 → 未登录（`None`）；其他 → 统一业务码映射后再取。
    pub fn parse_whoami(
        body: &str,
        http_code: i32,
        signals: douyin::TransportSignals,
    ) -> Result<Option<UserRef>, DouyinError> {
        let json = douyin::decode_response(body, http_code, signals, false)?;
        match json::long(&json, "status_code") {
            Some(0) => Ok(douyin::self_user(&json)),
            Some(8) => Ok(None),
            _ => {
                douyin::check_code(&json)?;
                Ok(douyin::self_user(&json))
            }
        }
    }
}

/// [`DouyinHostedSource::runtime_params`] 的自由函数版：扫码登录的身份 / 头也要同一套（`douyin/qr.rs`）。
pub(super) fn runtime_params(
    d: &Device,
    version_code: &str,
    version_name: &str,
    round_trip_time: &str,
) -> Vec<(String, String)> {
        vec![
            pair("device_platform", "webapp"),
            pair("aid", AID),
            pair("channel", "channel_pc_web"),
            pair("update_version_code", UPDATE_VERSION_CODE),
            pair("pc_client_type", "1"),
            pair("pc_libra_divert", d.libra_divert.clone()),
            pair("support_h265", "1"),
            pair("support_dash", "1"),
            pair("cpu_core_num", d.cpu_core_num.to_string()),
            pair("version_code", version_code),
            pair("version_name", version_name),
            pair("cookie_enabled", "true"),
            pair("screen_width", d.screen_width.to_string()),
            pair("screen_height", d.screen_height.to_string()),
            pair("browser_language", "zh-CN"),
            pair("browser_platform", d.platform.clone()),
            pair("browser_name", "Chrome"),
            pair("browser_version", format!("{}.0.0.0", d.browser_major)),
            pair("browser_online", "true"),
            pair("engine_name", "Blink"),
            pair("engine_version", format!("{}.0.0.0", d.browser_major)),
            pair("os_name", d.os_name.clone()),
            pair("os_version", d.os_version.clone()),
            pair("device_memory", d.device_memory.to_string()),
            pair("platform", "PC"),
            pair("downlink", "10"),
            pair("effective_type", "4g"),
            pair("round_trip_time", round_trip_time),
        ]
}

/// 一次 `getJson` 调用（Kotlin `DouyinSource.Get` 的移植）：路径、接口自己的参数、Referer、
/// 哪些身份值参与签名、`a_bogus` 之后追加的参数、这套前端模块的版本号。
struct Get {
    path: String,
    params: Vec<(String, String)>,
    /// 这个接口要的 Referer（评论 / 详情是作品页）；`None` 用默认的站根。
    referer: Option<String>,
    /// 哪些"服务端签发 / 页面上才有"的标识进 query、且参与签名（楼中楼要 `uifid=false` / `verifyFp=false`）。
    ids: Ids,
    /// 放在 `a_bogus` **之后**、不参与签名的参数（楼中楼的 `verifyFp` / `fp`）。
    after_sign: Vec<(String, String)>,
    version_code: &'static str,
    version_name: &'static str,
    round_trip_time: &'static str,
}

/// Kotlin `DouyinSource.Ids`：默认四个都参与签名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ids {
    webid: bool,
    uifid: bool,
    verify_fp: bool,
    ms_token: bool,
}

impl Default for Ids {
    fn default() -> Self {
        Self {
            webid: true,
            uifid: true,
            verify_fp: true,
            ms_token: true,
        }
    }
}

/// 响应里的两条风控信号（只看"有没有"：值不打印、也不进 Rust）。
pub(super) fn signals(response: &HttpResponse) -> douyin::TransportSignals {
    douyin::TransportSignals {
        bdturing: has_header(&response.headers, "X-Vc-Bdturing-Parameters"),
        passport_decision: has_header(&response.headers, "X-Tt-Verify-Passport-Decision"),
    }
}

impl From<HostError> for DouyinError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => DouyinError::Network { detail },
            HostError::ProxyUnavailable { detail } => DouyinError::ProxyUnavailable { detail },
            HostError::Other { detail } => DouyinError::Host { detail },
        }
    }
}

/// 游标 → `refresh_index`：逐行照 Kotlin `cursor?.value?.toIntOrNull() ?: 1`。
///
/// 注意 `toIntOrNull()` **不 trim**、必须是完整的十进制（可带一个正负号）、必须落在 `Int` 里。
pub fn parse_refresh_index(cursor: Option<&str>) -> i32 {
    cursor.and_then(kotlin_to_int_or_null).unwrap_or(1)
}

/// Kotlin `String.toIntOrNull()` 的语义（十进制、可带 `+`/`-`、整体匹配、32 位内）。
fn kotlin_to_int_or_null(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'+' => (false, &bytes[1..]),
        b'-' => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // 用 i64 累加，最后再判范围（Kotlin 的 parseInt 是逐位溢出判定的，结果一样：超界就 None）
    let mut value: i64 = 0;
    for b in digits {
        value = value.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
        if value > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    let signed = if negative { -value } else { value };
    if signed < i64::from(i32::MIN) || signed > i64::from(i32::MAX) {
        return None;
    }
    Some(signed as i32)
}

/// 参数表 → query 串：**键原样、值按 [form_encode]**（和 Kotlin `"$k=${formEncode(v)}"` 一样）。
pub(super) fn query_of(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 页面拼参数用的编码：`encodeURIComponent` **再把空格写成 `+`**。和 Kotlin `DouyinSource.formEncode` 逐字节一致
pub(super) fn form_encode(value: &str) -> String {
    sign::encode_component(value).replace("%20", "+")
}

// `okhttp_canonical_query` / `has_header` 从 `crate::hosted` 来（口径和注释都在那里）；
// `pub use` 保留 `douyin::source::okhttp_canonical_query` 这个老路径。
pub use crate::hosted::okhttp_canonical_query;
pub(super) fn pair(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Kotlin `String.isNotBlank()`：全是空白字符也算"没有"。
///
/// **没并进 `crate::hosted::is_blank`**：这里用的是 Rust 的 `trim()`（含 NBSP / U+0085），
/// Java 的 `Character.isWhitespace` 不含它们。只在"值全是 NBSP"这种输入上看得出来，
/// 这轮是纯重构、不动行为（见 `crate::hosted` 的文件头）。
pub(super) fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

