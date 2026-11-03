//! 酷安 Source 的**平台层**：推荐流 / 关注流的请求怎么拼（URL / 参数顺序 / 请求头顺序 / 翻页游标 /
//! `X-App-Token` 的计算与按秒缓存）、错误怎么判、响应该交给谁解析。**收发一律通过宿主**
//! （[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-coolapk/CoolapkSource.kt` 的同名方法（`feed` / `FeedCursor.parse` / `Identity` /
//! `request` / `CoolapkDevice`）：方法 + URL + 参数顺序 + 请求头顺序 + body 必须和 Kotlin 版**逐字节一样**，
//! `CoolapkRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 外加 `b23.tv` 短链跟一次重定向（用的是**另一个**
//!   宿主 HTTP 端口：B站 的代理设置和 Cookie 规则和酷安不一样）；
//! - [`CoolapkHostedSource::comments`] / [`CoolapkHostedSource::replies`]：`api.coolapk.com/v6/feed/replyList`，
//!   走**评论那套独立身份 16.2.0 / 2604201 / API 16 + v3 token**；
//! - **人机验证的识别**（不是验证本身）：第一页会回 `err_request_captcha_v2`，这里翻成
//!   [`CoolapkError::Captcha`]（带上 `captcha_id` / `field` / 服务端原话），宿主拿到之后让用户本人验证、
//!   带着答案把**同一个请求重试一次**（`captcha` 参数就是这个答案）。**验证码只能用户本人做**，
//!   Rust 不解析、不绕过、不循环，也不换版本号 / 身份参数。
//!
//! **收尾又搬了三样**，`RustCoolapkSource` 因此一个方法都不再委托 Kotlin：
//! - [`CoolapkHostedSource::play`]（`/v6/feed/detail` + 一次 `Range: bytes=0-0` 探针）：探针走**第三个**
//!   宿主 HTTP 端口（Kotlin 的 `mediaHttp`：不带 Cookie、不加设备头、不占 600ms 限速），
//!   403 / 401 翻成 [`CoolapkError::Signature`]（酷安托管的视频要官方 App 才有的 URL 签名，我们算不出来）；
//! - [`CoolapkHostedSource::search`]（`/v6/search`）：信息流那套身份 + 游标 `page|lastItem`；
//! - [`CoolapkHostedSource::whoami`]（`/v6/account/checkLoginInfo`）：**没有 `uid` Cookie 就 `None`、0 个请求**；
//!   服务端回"登录信息有误"（`NotLoggedIn`）也折成 `None`（Kotlin 就是这么写的，那是正常结果不是错误）。
//!
//! **没搬**：扫码、`raw`（开发者工具）。
//!
//! 设备（`X-App-Device` / UA / `X-Sdk-Int`）和 `installTime` 都**只从宿主 store 里读那一份**
//! （键 `coolapk.device` / `coolapk.install_time`，由 Kotlin 的 `CoolapkDevice.loadOrCreate` 生成），
//! Rust 不生成设备、不取随机数、不读系统时钟。

use std::sync::{Arc, Mutex};

use crate::coolapk::search::SearchSort;
use crate::coolapk::{self, token, CoolapkError, Cursor, FeedKind};
use crate::detail::{CommentPage, CommentSort, FeedDetail};
use crate::host::{CookieHost, HostError, HttpHost, HttpRequest, HttpResponse, StoreHost};
use crate::hosted::is_kotlin_blank;
use crate::json;
use crate::model::{Page, UserRef};
use crate::play::PlayInfo;

/// 信息流 / 关注流的接口根（和 `CoolapkSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://api2.coolapk.com";

/// 评论 / 楼中楼接口的根（Kotlin `CoolapkSource.COMMENTS_API`，**不是** api2）。
pub const COMMENTS_API: &str = "https://api.coolapk.com";

/// 取 Cookie 用的"身份 URL"（和 Kotlin 的 `COOKIE_URL` 一样）。
const COOKIE_URL: &str = "https://api2.coolapk.com/";

/// 评论那边跟重定向用的 UA（`UserAgents.DESKTOP_CHROME`，和 Kotlin 逐字一样）。
const DESKTOP_CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 服务端没下发 `captchaId` 时的兜底（coolapk-desktop 的默认酷安 id，和 Kotlin 一样）。
const CAPTCHA_DEFAULT_ID: &str = "414e5c9b866a03db03f860a1a9101672";

/// `err_request_captcha` / `err_request_captcha_v2`（Kotlin `CAPTCHA_STATUS`）。
const CAPTCHA_STATUS: &str = "err_request_captcha";

/// `installTime` 参数在 `ctx.store` 里的键（Kotlin `CoolapkSource.INSTALL_TIME_KEY`）。
pub const INSTALL_TIME_KEY: &str = "coolapk.install_time";

/// 假设备在 `ctx.store` 里的键（Kotlin `CoolapkDevice.STORE_KEY`）。
pub const DEVICE_KEY: &str = "coolapk.device";

/// 信息流 / 关注流的身份：**13.4.1 / 2312121 / API 13 + v2 token**（和 `AppVersion` 逐字一样）。
///
/// 为什么 Rust 里再写一份：`User-Agent` / `X-App-Version` / `X-App-Code` / `X-Api-Version` /
/// `X-App-Supported` 都属于"请求怎么发"，归 Rust；两份漂移会被 `CoolapkRequestEquivalenceTest` 抓住。
/// 评论那套 16.2.0 / 2604201 / API 16 是**另一套身份**，当前没搬（评论还在 Kotlin）。
pub const VERSION_NAME: &str = "13.4.1";
pub const VERSION_CODE: &str = "2312121";
pub const API_VERSION: &str = "13";

/// 评论接口的身份：**16.2.0 / 2604201 / API 16 + v3 token**（和 Kotlin `COMMENT_VERSION_*` 逐字一样）。
///
/// 这三样和 `X-App-Token` 必须对得上（v3 的 `appCode` 就是 `2604201`），所以**别单独改其中一个**；
/// 风控红线也不许"换个版本号再试"。
pub const COMMENT_VERSION_NAME: &str = "16.2.0";
pub const COMMENT_VERSION_CODE: &str = "2604201";
pub const COMMENT_API: &str = "16";

/// 一套身份（UA / `X-App-Version` / `X-App-Code` / `X-Api-Version` / `X-App-Supported` 从它取）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub name: &'static str,
    pub code: &'static str,
    pub api: &'static str,
}

/// 信息流 / 详情 / 关注流的身份（13.4.1 / API 13）。
pub const VERSION: Version = Version {
    name: VERSION_NAME,
    code: VERSION_CODE,
    api: API_VERSION,
};

/// 评论 / 楼中楼的身份（16.2.0 / API 16）。
pub const COMMENT_VERSION: Version = Version {
    name: COMMENT_VERSION_NAME,
    code: COMMENT_VERSION_CODE,
    api: COMMENT_API,
};

/// 官方 App 的包名（`X-App-Id`，也是设备信息里最后一段的来源）。
const APP_ID: &str = "com.coolapk.market";

/// 关注流的固定参数（c001apk-flutter `getDataList`：关注页 url / title / subTitle）。
const FOLLOW_URL: &str = "V9_HOME_TAB_FOLLOW";
const FOLLOW_TITLE: &str = "全部关注";

/// 一台假设备（Kotlin `CoolapkDevice` 的 `@Serializable` 形状）。
///
/// **只从 store 读**（`coolapk.device` 的那段 JSON），Rust 不生成、不修改：设备码是平台认人的东西，
/// 风控红线要求"只用 `ctx.store` 里那一份"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub szlm_id: String,
    pub mac: String,
    pub manufacturer: String,
    pub brand: String,
    pub model: String,
    pub build_id: String,
    pub android_version: String,
    pub sdk_int: i32,
}

impl Device {
    /// 从 store 里那段 JSON 还原。字段名和 Kotlin `@Serializable data class CoolapkDevice` 一样；
    /// 少一个字段就报错（Kotlin 的序列化器也会），不猜默认值。
    pub fn from_store_json(text: &str) -> Result<Self, CoolapkError> {
        let map = json::parse_object(text).map_err(|detail| CoolapkError::Internal {
            detail: format!("store 里的 {DEVICE_KEY} 不是 JSON 对象：{detail}"),
        })?;
        let field = |name: &str| {
            json::string(&map, name).ok_or_else(|| CoolapkError::Internal {
                detail: format!("store 里的 {DEVICE_KEY} 缺少 {name}"),
            })
        };
        let sdk_int = json::int(&map, "sdkInt").ok_or_else(|| CoolapkError::Internal {
            detail: format!("store 里的 {DEVICE_KEY} 缺少 sdkInt"),
        })?;
        Ok(Self {
            szlm_id: field("szlmId")?,
            mac: field("mac")?,
            manufacturer: field("manufacturer")?,
            brand: field("brand")?,
            model: field("model")?,
            build_id: field("buildId")?,
            android_version: field("androidVersion")?,
            sdk_int,
        })
    }

    /// 设备信息串（官方 App 的格式）：`数字联盟 id; ; ; mac; 厂商; 品牌; 型号; 版本号; null`。
    pub fn info(&self) -> String {
        format!(
            "{}; ; ; {}; {}; {}; {}; {}; null",
            self.szlm_id, self.mac, self.manufacturer, self.brand, self.model, self.build_id
        )
    }

    /// `X-App-Device`：设备信息串 base64 → 整体反转 → 去掉 `=` 和换行（照 `CoolapkDevice.encodeDeviceCode`）。
    pub fn code(&self) -> String {
        let encoded = token::base64(self.info().as_bytes(), true);
        encoded
            .chars()
            .rev()
            .filter(|c| *c != '=' && *c != '\r' && *c != '\n')
            .collect()
    }

    /// 官方 App 的 UA（`CoolapkDevice.userAgentFor`，逐字一样）。
    pub fn user_agent_for(&self, version_name: &str, version_code: &str) -> String {
        format!(
            "Dalvik/2.1.0 (Linux; U; Android {}; {} Build/{}) (#Build; {}; {}; {}; {}) \
             +CoolMarket/{}-{}-universal",
            self.android_version,
            self.model,
            self.build_id,
            self.brand,
            self.model,
            self.build_id,
            self.android_version,
            version_name,
            version_code
        )
    }
}

/// 酷安的平台对象：持有三个宿主端口 + 表情表，**自己的状态只有一个 token 缓存锁**。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustCoolapkHosted`）建一次、跟着 Source 活；Source 被 App 缓存到进程结束。
/// 没有后台线程、没有定时器、没有连接池——闲置时零唤醒。
pub struct CoolapkHostedSource {
    base: String,
    /// 评论 / 楼中楼那条接口的根（`api.coolapk.com`，和 `base` 不是同一台）。
    comments_base: String,
    http: Arc<dyn HttpHost>,
    /// **站外短链**（`b23.tv`）跟重定向用的 HTTP 端口：Kotlin 那边用的是**B站 的客户端**
    /// （`ctx.client(Platform("bili"))` + 不跟重定向 + 不带 Cookie），所以这里必须是另一个端口。
    links: Arc<dyn HttpHost>,
    /// **播放地址探针**用的 HTTP 端口（Kotlin `CoolapkSource.mediaHttp`：`ctx.client(platform)` +
    /// `CookieJar.NO_COOKIES`，没有设备默认头、没有 600ms 限速）。拉流 / 探针都不带 Cookie 是有意的：
    /// 视频在 `video-qc.coolapk.com` 上，`.coolapk.com` 那套登录 Cookie 不该跟着去 CDN。
    media: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    /// 表情表（"正文里的原文 → 图片地址"）。**所有权在插件**，构造时传一次（483 对），不每页搬。
    emotes: Vec<(String, String)>,
    /// v3 token 的 930 字节密码表（插件资源 `auth_blob.b64` 解码后）。**所有权在插件**，同样构造时传一次。
    v3_blob: Vec<u8>,
    /// `X-App-Token` 按**秒**缓存（和 Kotlin `Identity.tokenFor` 的 `@Synchronized` 缓存一样）：
    /// 同一秒里的多个请求复用同一个 token，不重复跑 bcrypt（v2 是 cost 10，一次约 100ms）。
    token: Mutex<Option<(i64, String)>>,
    /// 评论身份的 v3 token 缓存（**和 v2 分开**：token 不一样，跑的是不同的 bcrypt 输入）。
    token_v3: Mutex<Option<(i64, String)>>,
}

impl CoolapkHostedSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base: impl Into<String>,
        comments_base: impl Into<String>,
        emotes: Vec<(String, String)>,
        v3_blob: Vec<u8>,
        http: Arc<dyn HttpHost>,
        links: Arc<dyn HttpHost>,
        media: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            base: base.into(),
            comments_base: comments_base.into(),
            http,
            links,
            media,
            store,
            cookies,
            emotes,
            v3_blob,
            token: Mutex::new(None),
            token_v3: Mutex::new(None),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn comments_base(&self) -> &str {
        &self.comments_base
    }

    /// 一页。`now_sec` 是**秒级时间戳**（`X-App-Token` 只取决于设备码 + 秒），由调用方注入——
    /// Rust 不读系统时钟，测试才能把两次运行的请求逐字节比。
    pub fn feed(
        &self,
        kind: FeedKind,
        cursor: Option<&str>,
        now_sec: i64,
    ) -> Result<Page, CoolapkError> {
        let device = self.device()?;
        let c = parse_cursor(cursor);
        let (path, params) = match kind {
            // 参数照 c001apk-flutter getHomeFeed：page、firstLaunch、installTime、firstItem、lastItem、ids
            FeedKind::Recommend => {
                let mut params = vec![
                    ("page".to_string(), c.page.to_string()),
                    (
                        "firstLaunch".to_string(),
                        if c.page == 1 { "1" } else { "0" }.to_string(),
                    ),
                    ("installTime".to_string(), self.install_time()?),
                ];
                params.extend(item_params(&c));
                params.push(("ids".to_string(), String::new()));
                ("/v6/main/indexV8", params)
            }
            // 参数照 c001apk-flutter getDataList（关注页 url=V9_HOME_TAB_FOLLOW，title=全部关注）
            FeedKind::Following => {
                if self
                    .cookies
                    .find(COOKIE_URL, "uid")
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
                {
                    return Err(CoolapkError::NotLoggedIn { detail: None });
                }
                let mut params = vec![
                    ("url".to_string(), FOLLOW_URL.to_string()),
                    ("title".to_string(), FOLLOW_TITLE.to_string()),
                    ("subTitle".to_string(), String::new()),
                ];
                params.extend(item_params(&c));
                params.push(("page".to_string(), c.page.to_string()));
                ("/v6/page/dataList", params)
            }
        };
        let url = url_for(&self.base, path, &params);
        let headers = request_headers(&device, &self.token_v2(&device, now_sec)?, VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        // 错误映射也在 `parse_feed` 里（HTTP 状态码一起传）：和 Kotlin `decodeResponse` 一字不差
        coolapk::parse_feed(kind, &body, status, &c, &self.emotes)
    }

    // 详情

    /// 详情：`GET {base}/v6/feed/detail?id=…`（信息流那套身份）。
    ///
    /// 站外视频是 `b23.tv` 短链时要**再跟一次重定向**才知道是不是 B站 的哪个视频——那一次请求走
    /// [`Self::links`]（Kotlin 用 B站 的客户端：不跟重定向、不带 Cookie、B站 的代理设置），
    /// 失败了（连不上 / fail-closed）就当没解析出来（和 Kotlin `catch (IOException) → null` 一样）。
    pub fn detail(&self, id: &str, now_sec: i64) -> Result<FeedDetail, CoolapkError> {
        let device = self.device()?;
        let params = vec![("id".to_string(), id.to_string())];
        let url = url_for(&self.base, "/v6/feed/detail", &params);
        let headers = request_headers(&device, &self.token_v2(&device, now_sec)?, VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        let json = coolapk::decode_json(&body, status)?;
        let short = json::obj(&json, "data")
            .and_then(|d| json::text(d, "media_url"))
            .filter(|u| crate::coolapk::video::is_short_link(u));
        let resolved = match short {
            Some(url) => self.expand_short_link(&url)?,
            None => None,
        };
        coolapk::detail::detail(&json, resolved.as_deref(), &self.emotes)
    }

    /// `b23.tv` 短链跟一次重定向（不自动跟随，只读 `Location`）。
    fn expand_short_link(&self, url: &str) -> Result<Option<String>, CoolapkError> {
        let headers = vec![("User-Agent".to_string(), DESKTOP_CHROME.to_string())];
        let request = HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers,
            body: Vec::new(),
        };
        match self.links.execute(request) {
            Ok(response) => Ok(crate::hosted::header_value(&response.headers, "location")
                .map(|value| value.to_string())),
            // Kotlin `catch (_: java.io.IOException) { null }`：连不上 / fail-closed 都当"没解析出来"
            Err(HostError::Network { .. }) | Err(HostError::ProxyUnavailable { .. }) => Ok(None),
            Err(e) => Err(CoolapkError::from(e)),
        }
    }

    // 评论 / 楼中楼

    /// 一级评论：`GET {comments_base}/v6/feed/replyList` + **16.2.0 / API 16 的身份**。
    ///
    /// 排序：`Default` → `lastupdate_desc`（最近回复）、`Hot` → `popular`、`Newest` → `dateline_desc`。
    /// `captcha` 是"用户已经验证过一次、这次要把答案带上"的 `(field, proof)`：
    /// field 为空表示答案已经写进 `validate` cookie（宿主做的），非空就作为查询参数追加在最后。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_sec: i64,
        captcha: Option<(&str, &str)>,
    ) -> Result<CommentPage, CoolapkError> {
        let c = parse_cursor(cursor);
        let list_type = match sort {
            CommentSort::Default => "lastupdate_desc",
            CommentSort::Hot => "popular",
            CommentSort::Newest => "dateline_desc",
        };
        let mut params = vec![
            ("id".to_string(), id.to_string()),
            ("listType".to_string(), list_type.to_string()),
            ("page".to_string(), c.page.to_string()),
        ];
        params.extend(item_params(&c));
        params.extend([
            ("discussMode".to_string(), "1".to_string()),
            ("feedType".to_string(), "feed".to_string()),
            ("blockStatus".to_string(), "0".to_string()),
            ("fromFeedAuthor".to_string(), "0".to_string()),
        ]);
        self.reply_list(params, &c, now_sec, captcha)
    }

    /// 楼中楼：同一个接口、`id` 换成一级评论的 id、`feedType=feed_reply`（**仍然走 api + 新身份**）。
    pub fn replies(
        &self,
        comment_id: &str,
        cursor: Option<&str>,
        now_sec: i64,
        captcha: Option<(&str, &str)>,
    ) -> Result<CommentPage, CoolapkError> {
        let c = parse_cursor(cursor);
        let mut params = vec![
            ("id".to_string(), comment_id.to_string()),
            ("listType".to_string(), String::new()),
            ("page".to_string(), c.page.to_string()),
        ];
        params.extend(item_params(&c));
        params.extend([
            ("discussMode".to_string(), "0".to_string()),
            ("feedType".to_string(), "feed_reply".to_string()),
            ("blockStatus".to_string(), "0".to_string()),
            ("fromFeedAuthor".to_string(), "0".to_string()),
        ]);
        self.reply_list(params, &c, now_sec, captcha)
    }

    fn reply_list(
        &self,
        mut params: Vec<(String, String)>,
        cursor: &Cursor,
        now_sec: i64,
        captcha: Option<(&str, &str)>,
    ) -> Result<CommentPage, CoolapkError> {
        let device = self.device()?;
        if let Some((field, proof)) = captcha {
            // Kotlin：`url.newBuilder().addQueryParameter(challenge.field, proof)` —— **追加在最后**
            if !field.is_empty() {
                params.push((field.to_string(), proof.to_string()));
            }
        }
        let url = url_for(&self.comments_base, "/v6/feed/replyList", &params);
        let headers = request_headers(&device, &self.token_v3(&device, now_sec)?, COMMENT_VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        let parsed = json::parse_object(&body).ok();
        if let Some(json) = &parsed {
            // 要人机验证：**不在这里弹、也不重试**，交给宿主（用户本人验证完再带 captcha 参数来一次）
            if is_captcha(json) {
                return Err(captcha_error(json));
            }
        }
        let json = coolapk::decode_parsed(&body, status, parsed.as_ref())?;
        coolapk::comment::reply_list(&json, cursor, &self.emotes)
    }

    // 搜索 / 播放 / 账号

    /// 搜一页（`GET {base}/v6/search`，信息流那套 13.4.1 / API 13 身份）。
    ///
    /// 游标是 `page|lastItem`（**和列表的 `page=N&firstItem=…` 不是一个格式**，编解码在
    /// [`crate::coolapk::search`]）。撞上 `err_request_captcha*` 时**不弹验证、不重试**：
    /// 判定落在 [`crate::coolapk::check_status`] 里，结果是一条风控错。
    pub fn search(
        &self,
        text: &str,
        sort: SearchSort,
        cursor: Option<&str>,
        now_sec: i64,
    ) -> Result<Page, CoolapkError> {
        let device = self.device()?;
        let (page, last_item) = coolapk::search::decode_cursor(cursor);
        let params = coolapk::search::params(text, sort, page, last_item.as_deref());
        let url = url_for(&self.base, "/v6/search", &params);
        let headers = request_headers(&device, &self.token_v2(&device, now_sec)?, VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        coolapk::search::parse_response(&body, status, page, &self.emotes)
    }

    /// 播放（`CoolapkSource.play`）：只有酷安自己托管的视频动态（`partId == "main"`）有。
    ///
    /// 两步：① `GET /v6/feed/detail?id=…` 取地址（信息流身份）；② 用**另一个宿主端口**
    /// （[`Self::media`]，不带 Cookie / 不加设备头 / 不占限速）发一次 `Range: bytes=0-0` 探一下——
    /// 酷安托管的地址有 URL 签名防盗链，签名只有官方 App 才会算，探针被 401 / 403 拒就报
    /// [`CoolapkError::Signature`]（说清楚原因），**不返回一个注定播不了的地址**让播放器报"播放失败"。
    ///
    /// `quality` 不参与：酷安一条视频只有一档（整段 mp4），参数留着只是为了和接口签名对齐。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        _quality: Option<i32>,
        now_sec: i64,
    ) -> Result<PlayInfo, CoolapkError> {
        if part_id != coolapk::video::MAIN {
            return Err(CoolapkError::Api {
                code: "bad_part".to_string(),
                detail: format!("不认识的视频 {part_id}"),
            });
        }
        let device = self.device()?;
        let params = vec![("id".to_string(), id.to_string())];
        let url = url_for(&self.base, "/v6/feed/detail", &params);
        let headers = request_headers(&device, &self.token_v2(&device, now_sec)?, VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        let (hosted, info) = coolapk::video::play_from_detail(&body, status)?;

        // Range 探针：`Range` 在最前面，后面是播放器要带的两个头（Referer 在 User-Agent 前面）
        let mut probe_headers = vec![("Range".to_string(), "bytes=0-0".to_string())];
        probe_headers.extend(info.headers.iter().cloned());
        let (code, probe_body) = self.probe(&hosted.url, probe_headers)?;
        if code == 401 || code == 403 {
            return Err(CoolapkError::Signature {
                detail: format!(
                    "酷安托管的视频需要官方 App 才会算的 URL 签名（HTTP {code} {}），暂时放不了",
                    json::take_chars(&probe_body, 40)
                ),
            });
        }
        if !(200..=299).contains(&code) {
            return Err(CoolapkError::Api {
                code: format!("HTTP {code}"),
                detail: "视频地址不可用".to_string(),
            });
        }
        Ok(info)
    }

    /// 拿播放地址做一次 Range 探针（走 [`Self::media`] 端口），返回 `(状态码, body)`。
    fn probe(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<(i32, String), CoolapkError> {
        let response: HttpResponse = self
            .media
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: url.to_string(),
                headers,
                body: Vec::new(),
            })
            .map_err(CoolapkError::from)?;
        Ok((
            response.status as i32,
            String::from_utf8_lossy(&response.body).into_owned(),
        ))
    }

    /// 当前登录的用户（`GET {base}/v6/account/checkLoginInfo`）。
    ///
    /// **没有 `uid` Cookie 就直接 `None`、一个请求都不发**（Kotlin 第一行就是这个判断）；
    /// 有 Cookie 但服务端说"登录信息有误"也折成 `None`（[`crate::coolapk::whoami_from`]）；
    /// 别的错误（风控 / 签名 / 接口码）照常抛。Cookie 只查"有没有"，**值不进日志、不进返回值**。
    pub fn whoami(&self, now_sec: i64) -> Result<Option<UserRef>, CoolapkError> {
        if self
            .cookies
            .find(COOKIE_URL, "uid")
            .map(|v| is_kotlin_blank(&v))
            .unwrap_or(true)
        {
            return Ok(None);
        }
        let device = self.device()?;
        let url = url_for(&self.base, "/v6/account/checkLoginInfo", &[]);
        let headers = request_headers(&device, &self.token_v2(&device, now_sec)?, VERSION);
        let (status, body) = self.send(&self.http, url, headers)?;
        coolapk::whoami_from(&body, status)
    }

    /// 一次 GET：返回 `(状态码, body)`。**失败分类原样带回去**（网络 / fail-closed / 宿主意外）。
    fn send(
        &self,
        host: &Arc<dyn HttpHost>,
        url: String,
        headers: Vec<(String, String)>,
    ) -> Result<(i32, String), CoolapkError> {
        let response = host
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers,
                body: Vec::new(),
            })
            .map_err(CoolapkError::from)?;
        Ok((
            response.status as i32,
            String::from_utf8_lossy(&response.body).into_owned(),
        ))
    }

    /// store 里那台假设备（Kotlin 的 `CoolapkDevice.loadOrCreate` met 它，Rust 只读）。
    fn device(&self) -> Result<Device, CoolapkError> {
        let text = self.store.get(DEVICE_KEY).ok_or_else(|| CoolapkError::Internal {
            detail: format!(
                "store 里没有 {DEVICE_KEY}（设备由 Kotlin 的 CoolapkDevice.loadOrCreate 生成，Rust 不新造）"
            ),
        })?;
        Device::from_store_json(&text)
    }

    /// `installTime`：推荐流的参数。同样只读 store（Kotlin 在构造 Source 时生成一次）。
    fn install_time(&self) -> Result<String, CoolapkError> {
        self.store.get(INSTALL_TIME_KEY).ok_or_else(|| CoolapkError::Internal {
            detail: format!("store 里没有 {INSTALL_TIME_KEY}（installTime 由 Kotlin 生成一次，Rust 不新造）"),
        })
    }

    /// 信息流 / 详情身份的 `X-App-Token`（v2）：同一秒内复用（和 Kotlin `Identity.tokenFor` 一样）。
    fn token_v2(&self, device: &Device, now_sec: i64) -> Result<String, CoolapkError> {
        let mut cache = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((ts, token)) = cache.as_ref() {
            if *ts == now_sec {
                return Ok(token.clone());
            }
        }
        let token =
            token::v2(&device.code(), now_sec).map_err(|detail| CoolapkError::Internal { detail })?;
        *cache = Some((now_sec, token.clone()));
        Ok(token)
    }

    /// 评论身份的 `X-App-Token`（v3，`appCode` 必须等于 `X-App-Code`）：**自己那条缓存**。
    fn token_v3(&self, device: &Device, now_sec: i64) -> Result<String, CoolapkError> {
        let mut cache = self.token_v3.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((ts, token)) = cache.as_ref() {
            if *ts == now_sec {
                return Ok(token.clone());
            }
        }
        let app_code = COMMENT_VERSION_CODE
            .parse::<i64>()
            .map_err(|detail| CoolapkError::Internal {
                detail: format!("评论身份的 X-App-Code 不是数字：{detail}"),
            })?;
        let token = token::v3(&device.code(), now_sec, app_code, &self.v3_blob)
            .map_err(|detail| CoolapkError::Internal { detail })?;
        *cache = Some((now_sec, token.clone()));
        Ok(token)
    }
}

/// 拼 URL：`{base}{path}?k=v&…`（query 编码是 OkHttp 口径，见 [`query_encode`]）。
fn url_for(base: &str, path: &str, params: &[(String, String)]) -> String {
    let mut url = format!("{base}{path}");
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

/// `err_request_captcha*`：这个响应是"先去做人机验证"（Kotlin `isCaptcha`）。
pub fn is_captcha(json: &json::JsonMap) -> bool {
    json::string(json, "messageStatus")
        .map(|s| s.starts_with(CAPTCHA_STATUS))
        .unwrap_or(false)
}

/// 要人机验证的那个错误：`captchaId` / `captchaField` 从 `messageExtra`（字符串形式的 JSON）里取。
pub fn captcha_error(json: &json::JsonMap) -> CoolapkError {
    let (captcha_id, field) = captcha_challenge(json);
    CoolapkError::Captcha {
        captcha_id,
        field,
        detail: json::text(json, "message") .unwrap_or_else(|| "当前访问需要验证码".to_string()),
    }
}

/// `CoolapkSource.captchaChallenge`：`(captchaId, field)`。
///
/// - `messageExtra` 可能是**对象**，也可能是一个**字符串**（里面是 JSON）；两者都认；
/// - `captchaId` 不合形状（`^[A-Za-z0-9]{16,64}$`）就用酷安网页那版默认 id；
/// - `captchaField` 为空表示答案走 `validate` cookie；给了但形状不对（`^_[A-Za-z0-9_]{1,63}$`）
///   就退回 `_v2_post_token`。
pub fn captcha_challenge(json: &json::JsonMap) -> (String, String) {
    let extra_owned: Option<json::JsonMap> =
        match json.get("messageExtra").or_else(|| json.get("extra")) {
            Some(serde_json::Value::String(text)) => json::parse_object(text).ok(),
            _ => None,
        };
    let extra = json::obj(json, "messageExtra")
        .or_else(|| json::obj(json, "extra"))
        .or(extra_owned.as_ref());
    let captcha_id = extra
        .and_then(|e| json::string(e, "captchaId"))
        .filter(|s| is_captcha_id(s))
        .unwrap_or_else(|| CAPTCHA_DEFAULT_ID.to_string());
    let field = match extra.and_then(|e| json::string(e, "captchaField")) {
        None => String::new(),
        Some(f) if f.is_empty() => String::new(),
        Some(f) if is_captcha_field(&f) => f,
        Some(_) => "_v2_post_token".to_string(),
    };
    (captcha_id, field)
}

/// `^[A-Za-z0-9]{16,64}$`
fn is_captcha_id(s: &str) -> bool {
    (16..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `^_[A-Za-z0-9_]{1,63}$`
fn is_captcha_field(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('_') else {
        return false;
    };
    (1..=63).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl From<HostError> for CoolapkError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => CoolapkError::Network { detail },
            HostError::ProxyUnavailable { detail } => CoolapkError::ProxyUnavailable { detail },
            HostError::Other { detail } => CoolapkError::Host { detail },
        }
    }
}

/// 游标：`page=N&firstItem=…&lastItem=…`，也接受直接写一个页码（cli 的 `--cursor 3`）。
/// 逐行照 `FeedCursor.parse`：`=` 前面的键（下标 0 的丢掉）、重复键后出现的赢、页码至少 1、
/// 空的 `firstItem` / `lastItem` 当没有。
pub fn parse_cursor(value: Option<&str>) -> Cursor {
    let Some(value) = value else {
        return Cursor {
            page: 1,
            first_item: None,
            last_item: None,
        };
    };
    let mut page: Option<i64> = None;
    let mut first_item: Option<String> = None;
    let mut last_item: Option<String> = None;
    for part in value.split('&') {
        // Kotlin：`val i = it.indexOf('='); if (i <= 0) null else …`
        let Some(i) = part.find('=') else { continue };
        if i == 0 {
            continue;
        }
        let key = &part[..i];
        let val = &part[i + 1..];
        match key {
            "page" => page = parse_page(val),
            "firstItem" => first_item = non_empty(val),
            "lastItem" => last_item = non_empty(val),
            _ => {}
        }
    }
    let page = page
        .or_else(|| parse_page(value))
        .unwrap_or(1)
        .max(1);
    Cursor {
        page,
        first_item,
        last_item,
    }
}

/// Kotlin `String.toIntOrNull()`：只认可选正负号 + 十进制数字，溢出 / 空白一律 `None`。
fn parse_page(text: &str) -> Option<i64> {
    text.parse::<i64>().ok()
}

fn non_empty(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// 当前游标里的 `firstItem` / `lastItem`（Kotlin `FeedCursor.itemParams`），顺序固定。
fn item_params(cursor: &Cursor) -> Vec<(String, String)> {
    let mut params = Vec::new();
    if let Some(first) = &cursor.first_item {
        params.push(("firstItem".to_string(), first.clone()));
    }
    if let Some(last) = &cursor.last_item {
        params.push(("lastItem".to_string(), last.clone()));
    }
    params
}

/// 请求头，**顺序照 Kotlin 版最终落到线上的顺序**：
/// `request()` 先写 `X-App-Token` + 身份头（UA / X-App-Version / X-App-Code / X-Api-Version /
/// X-App-Supported），`DefaultHeadersInterceptor` 再按 `device.headers()` 的顺序补**缺失的**那 8 个
/// （X-Requested-With / X-Sdk-Int / X-Sdk-Locale / X-App-Id / X-App-Device / X-Dark-Mode /
/// X-App-Channel / X-App-Mode）。这里一次写全，宿主拦截器因此一个都不会再补。
fn request_headers(device: &Device, token: &str, version: Version) -> Vec<(String, String)> {
    vec![
        ("X-App-Token".to_string(), token.to_string()),
        (
            "User-Agent".to_string(),
            device.user_agent_for(version.name, version.code),
        ),
        ("X-App-Version".to_string(), version.name.to_string()),
        ("X-App-Code".to_string(), version.code.to_string()),
        ("X-Api-Version".to_string(), version.api.to_string()),
        ("X-App-Supported".to_string(), version.code.to_string()),
        ("X-Requested-With".to_string(), "XMLHttpRequest".to_string()),
        ("X-Sdk-Int".to_string(), device.sdk_int.to_string()),
        ("X-Sdk-Locale".to_string(), "zh-CN".to_string()),
        ("X-App-Id".to_string(), APP_ID.to_string()),
        ("X-App-Device".to_string(), device.code()),
        ("X-Dark-Mode".to_string(), "0".to_string()),
        ("X-App-Channel".to_string(), "coolapk".to_string()),
        ("X-App-Mode".to_string(), "universal".to_string()),
    ]
}

// `query_encode` 从 `crate::hosted` 来（口径和注释都在那里）；`pub use` 保留老路径。
pub use crate::hosted::query_encode;
