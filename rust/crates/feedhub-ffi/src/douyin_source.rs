//! **抖音的平台对象**：推荐流（"精选" `module/feed`）的整条请求逻辑（拼 URL / 参数顺序 /
//! `version_code` 那几套常量 / 请求头顺序 / 翻页游标 / a_bogus / 空 body 与业务码 8 与两条风控头的判定）
//! + 解析，都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读写 store、读 Cookie 罐。见 [`crate::host`]。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiDouyinSource(base, device, profile, env, enterPageTs, hostHttp, hostStore, hostCookies)
//! val count = douyin_abogus_random_count(env, nowMs)          // 先问这次要几个随机数
//! val rand = DoubleArray(count) { abogusRandom.nextDouble() }  // 再按 Kotlin 版的顺序取
//! val page = withContext(Dispatchers.IO) { src.feed(cursor, nowMs, rand.toList()) }
//! val again = douyin_parse_feed(page.body, 200, false, false, page.refreshIndex, emotes)  // 带表情表的第二遍
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象**无状态**（没有缓存、没有计时器），可以跨线程用
//! （`Send + Sync`），但没有内部线程。`feed()` 返回时一切都结束了。
//!
//! 设备（`FfiDouyinDevice`）和 a_bogus 的常量 / 环境（`FfiDouyinAbogusProfile` / `FfiDouyinAbogusEnv`）
//! 都是**构造时给一次**：它们同时要进 query、请求头和签名，一份就够，漂不了。

use std::sync::Arc;

use feedhub_core::douyin::source::{Device, DouyinHostedSource};

use crate::douyin::{env_of, map_error, profile_of, FfiDouyinAbogusEnv, FfiDouyinAbogusProfile};
use crate::host::{CookiesAdapter, HostCookies, HostHttp, HostStore, HttpAdapter, StoreAdapter};
use crate::play::to_ffi_play;
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, FfiCommentPage, FfiDetail, FfiEmote, FfiError,
    FfiPage, FfiPlayInfo,
};

/// 抖音 web 端的"设备环境"（Kotlin `DouyinDevice` 的那几个值）。
///
/// `install_time` / `verify_fp` 是 `DouyinDevice` 从 `ctx.store`（`douyin.install_time` /
/// `douyin.verify_fp`）取的那一份。风控红线：设备身份只认 Kotlin 那一套，Rust 不新造。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinDevice {
    pub user_agent: String,
    pub platform: String,
    pub ua_platform: String,
    pub os_name: String,
    pub os_version: String,
    pub libra_divert: String,
    pub browser_major: String,
    pub screen_width: i32,
    pub screen_height: i32,
    pub cpu_core_num: i32,
    pub device_memory: i32,
    /// `install_time` 参数（Kotlin `DouyinDevice.installTime`）。
    pub install_time: i64,
    /// `verifyFp` 的兜底值（Cookie 里没有 `s_v_web_id` 时用）。
    pub verify_fp: String,
}

impl From<FfiDouyinDevice> for Device {
    fn from(d: FfiDouyinDevice) -> Self {
        Device {
            user_agent: d.user_agent,
            platform: d.platform,
            ua_platform: d.ua_platform,
            os_name: d.os_name,
            os_version: d.os_version,
            libra_divert: d.libra_divert,
            browser_major: d.browser_major,
            screen_width: d.screen_width,
            screen_height: d.screen_height,
            cpu_core_num: d.cpu_core_num,
            device_memory: d.device_memory,
            install_time: d.install_time,
            verify_fp: d.verify_fp,
        }
    }
}

/// 一页推荐流 + `desc` 拼串 + 原始响应体 + 这一页的 `refresh_index`。
///
/// `body` 是给第二遍解析用的（调用方看到 `texts` 里有表情记号才取表情表、再映射一次——
/// **不能为了表情表多打一个接口请求**，请求条数要和 Kotlin 版完全一样）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinHostedFeed {
    pub page: FfiPage,
    pub texts: String,
    pub body: String,
    pub refresh_index: i32,
}

/// 详情 + `desc`（判断要不要取表情表）+ **原始响应体**（第二遍解析；`play` 复用那份缓存也要它）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinHostedDetail {
    pub detail: FfiDetail,
    pub texts: String,
    pub body: String,
}

/// 一页评论 + 这一页所有评论正文的拼串 + **原始响应体**（第二遍解析用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDouyinHostedComments {
    pub page: FfiCommentPage,
    pub texts: String,
    pub body: String,
}

/// 抖音平台对象（`uniffi::Object`）。
///
/// 所以详情 / 评论里出现 `[赞]` 时 Rust 会**在同一个调用里**多签一次 `emoji/list`。
/// 随机数因此改成"签的那一刻问宿主抽"（[`Self::set_random`]）：调用方预抽几个会和 Kotlin 的随机流
/// 错位（多抽就整条流往后挪）。
#[derive(uniffi::Object)]
pub struct FfiDouyinSource {
    inner: DouyinHostedSource,
}

#[uniffi::export]
impl FfiDouyinSource {
    /// [`base`] 是接口根（线上 `https://www.douyin.com`，测试指向 MockWebServer）。
    ///
    /// `device` / `profile` / `env` 都是 Kotlin 读好传进来的那一份（见各自的文档）；
    /// `enter_page_ts` 是进入页面的时间，只参与 a_bogus 的 `U[28]`（Kotlin 建 Source 时取一次）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        device: FfiDouyinDevice,
        profile: FfiDouyinAbogusProfile,
        env: FfiDouyinAbogusEnv,
        enter_page_ts: i64,
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: DouyinHostedSource::new(
                base,
                device.into(),
                profile_of(profile),
                env_of(env),
                enter_page_ts,
                Arc::new(HttpAdapter(http)),
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
            ),
        })
    }

    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，一次 FFI 回调都不走。
    /// 随机源（a_bogus）仍然是宿主的 `HostRandom`，见 `set_random`。
    #[allow(clippy::too_many_arguments)]
    #[uniffi::constructor]
    pub fn with_net(
        base: String,
        device: FfiDouyinDevice,
        profile: FfiDouyinAbogusProfile,
        env: FfiDouyinAbogusEnv,
        enter_page_ts: i64,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let http = stack.build_host_public(spec)?;
        Ok(Arc::new(Self {
            inner: DouyinHostedSource::new(
                base,
                device.into(),
                profile_of(profile),
                env_of(env),
                enter_page_ts,
                http,
                Arc::new(StoreAdapter(store)),
                stack.cookies_host(),
            ),
        }))
    }

    /// 推荐流一页。[`now_ms`] 是毫秒级时间戳（a_bogus 用），[`rand`] 是按
    /// `douyin_abogus_random_count(env, now_ms)` 从 Kotlin 的随机源里取好的那串——
    /// **都由调用方传进来**，Rust 不读系统时钟、不取随机数，所以两次运行可以逐字节比。
    pub fn feed(
        &self,
        cursor: Option<String>,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<FfiDouyinHostedFeed, FfiError> {
        self.inner
            .feed(cursor.as_deref(), now_ms, &rand)
            .map(|feed| FfiDouyinHostedFeed {
                page: to_ffi_page(&feed.page),
                texts: feed.texts,
                body: feed.body,
                refresh_index: feed.refresh_index,
            })
            .map_err(map_error)
    }

    /// 详情：`GET /aweme/v1/web/aweme/detail/`（**secsdk 保护路径**）。
    ///
    /// [`now_ms`] / [`rand`] 和 [`Self::feed`] 一样由调用方注入（"先问个数、再取"）。
    /// 第一遍解析不带表情表：调用方看 `texts` 里有记号才取表、再用纯函数 `douyin_parse_detail`
    /// 拿**同一份 `body`** 映射第二遍（**不发第二个请求**）。
    pub fn detail(
        &self,
        id: String,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<FfiDouyinHostedDetail, FfiError> {
        self.inner
            .detail(&id, now_ms, &rand)
            .map(|hosted| FfiDouyinHostedDetail {
                detail: to_ffi_detail(&hosted.detail),
                texts: hosted.texts,
                body: hosted.body,
            })
            .map_err(map_error)
    }

    /// 一级评论：`GET /aweme/v1/web/comment/list/`。游标就是响应里的 `cursor`（`None` = 第一页）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<FfiDouyinHostedComments, FfiError> {
        self.inner
            .comments(&id, cursor.as_deref(), now_ms, &rand)
            .map(|hosted| FfiDouyinHostedComments {
                page: to_ffi_comment_page(&hosted.page),
                texts: hosted.texts,
                body: hosted.body,
            })
            .map_err(map_error)
    }

    /// 楼中楼：`GET /aweme/v1/web/comment/list/reply/`（`verifyFp` / `fp` 放在 `a_bogus` 之后）。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<FfiDouyinHostedComments, FfiError> {
        self.inner
            .replies(&id, &comment_id, cursor.as_deref(), now_ms, &rand)
            .map(|hosted| FfiDouyinHostedComments {
                page: to_ffi_comment_page(&hosted.page),
                texts: hosted.texts,
                body: hosted.body,
            })
            .map_err(map_error)
    }

    /// 播放地址：`aweme/detail` 里的 `video.bit_rate[].play_addr`（`DouyinVideo.play` 的移植）。
    ///
    /// [`now_ms`] / [`rand`] 和 [`Self::feed`] 一样由调用方注入（a_bogus 要签 query）。
    /// **60 秒内 [`Self::detail`] 取过同一条作品就不再发请求**（`recentAwemes` 搬进了 Rust：
    /// 详情 → 播放仍然只有**一个** `aweme/detail` 请求）。超过 60 秒才重新取一次——地址带时效参数。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id, quality, now_ms, &rand)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}

#[uniffi::export]
impl FfiDouyinSource {
    /// 装上 a_bogus 的随机数来源（`AbogusRandom` / `Random` 那一份）。
    ///
    /// **生产必须装**：表情表搬进 Rust 之后，一次 `detail` / `comments` 里可能在解析完之后
    /// 多签一个 `emoji/list` 请求，随机流的长度只有当 Rust 才知道——调用方预抽几个就会和 Kotlin
    /// 参照实现错位（少发一次就多抽一份，后面全错）。装上之后抽取的个数与顺序天然一致。
    pub fn set_random(&self, random: Arc<dyn crate::host::HostRandom>) {
        self.inner
            .set_random(Arc::new(crate::host::RandomAdapter(random)));
    }

    /// 手上已经有的表情表（内存 → `ctx.store` 里一周内那份）。**不发请求、不改状态。**
    pub fn cached_emotes(&self, now_ms: i64) -> Vec<FfiEmote> {
        self.inner
            .emote_map(now_ms)
            .into_iter()
            .map(|(code, url)| FfiEmote { code, url })
            .collect()
    }

    /// 下一趟 [`Self::ensure_emotes`] 会不会真的发请求（纯查询：不取表、不改状态、不占随机数）。
    pub fn emote_fetch_needed(&self, now_ms: i64) -> bool {
        self.inner.emote_fetch_needed(now_ms)
    }

    /// 后台把表情表补上（详情 / 评论看到正文里有记号、而表还没到手上时调）：
    /// 同一时间只许一个在飞，失败（不是风控）降级成旧表 + [10 分钟](crate 常量)不再试。
    /// **风控照常抛**（Kotlin `emotesFor` 也不吞）。返回补好之后表里有几项（0 = 没取到）。
    pub fn ensure_emotes(&self, now_ms: i64, rand: Vec<f64>) -> Result<u32, FfiError> {
        let table = self.inner.ensure_emote_table(now_ms, &rand).map_err(map_error)?;
        let used = self
            .inner
            .emote_map(now_ms)
            .len()
            .max(table.map.len());
        Ok(used as u32)
    }

    /// 一段文字里真出现过的表情代码（`Emotes.used` 的口径：按表的顺序、空的跳过）。
    /// **可能发一次请求**（表不在手上时）。风控照常抛；别的错误降级成旧表 / 空表。
    pub fn emotes_for(
        &self,
        text: String,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<Vec<FfiEmote>, FfiError> {
        self.inner
            .emotes_for(&text, now_ms, &rand)
            .map(|list| list.into_iter().map(|(code, url)| FfiEmote { code, url }).collect())
            .map_err(map_error)
    }
}

// ===========================================================================
// 账号：我是谁 + 扫码登录
// ===========================================================================

#[uniffi::export]
impl FfiDouyinSource {
    /// 「我是谁」：`/aweme/v1/web/user/profile/self/`。
    /// 没有 `sessionid` 系列 Cookie 时**一个请求都不发**（直接 `None`）。
    pub fn whoami(
        &self,
        now_ms: i64,
        rand: Vec<f64>,
    ) -> Result<Option<crate::FfiUserRef>, FfiError> {
        self.inner
            .whoami(now_ms, &rand)
            .map(|user| user.as_ref().map(crate::to_ffi_user_ref))
            .map_err(map_error)
    }
}

/// 抖音的网页扫码登录（`login.douyin.com` 的 passport 接口）。
///
/// 为什么是另一个对象：扫码要**临时 Cookie 罐**的宿主能力（`HostQrLogin`），而那个端口只有扫码用得上；
/// 分开放之后 `FfiDouyinSource` 的构造函数一个字都不用改。
#[derive(uniffi::Object)]
pub struct FfiDouyinQr {
    inner: feedhub_core::douyin::qr::DouyinQrSource,
}

#[uniffi::export]
impl FfiDouyinQr {
    /// `device` / `profile` / `env` 与 [`FfiDouyinSource::new`] 同一份（a_bogus 和设备身份不能漂）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        passport: String,
        device: FfiDouyinDevice,
        profile: FfiDouyinAbogusProfile,
        env: FfiDouyinAbogusEnv,
        enter_page_ts: i64,
        window_width: i32,
        window_height: i32,
        // 这个宿主端口用不上了；
        // **参数留着**是因为 Kotlin 门面还在按位置传（拆掉要同步改 `RustDouyinHosted`）。
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
        qr: Arc<dyn crate::host::HostQrLogin>,
    ) -> Arc<Self> {
        let _ = &http;
        Arc::new(Self {
            inner: feedhub_core::douyin::qr::DouyinQrSource::new(
                base,
                passport,
                device.into(),
                profile_of(profile),
                env_of(env),
                enter_page_ts,
                window_width,
                window_height,
                Arc::new(StoreAdapter(store)),
                Arc::new(CookiesAdapter(cookies)),
                Arc::new(crate::host::QrAdapter(qr)),
            ),
        })
    }

    /// 第一步：要一张二维码。[`trace_id`] 是 `biz_trace_id`（Kotlin `newTraceId()`：UUID 去横线取前 8 位），
    /// [`rand`] 是按"先问个数、再取"拿到的那串 a_bogus 随机数。
    pub fn start(
        &self,
        now_ms: i64,
        trace_id: String,
        rand: Vec<f64>,
    ) -> Result<crate::account::FfiQrLogin, FfiError> {
        self.inner
            .start(now_ms, &trace_id, &rand)
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(map_error)
    }

    /// 第二步：问一次状态（可能要两次签名：passport 一跳 + 验证 whoami 一跳）。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
        trace_id: String,
        rand: Vec<f64>,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let login = crate::account::from_ffi_qr_login(&login);
        self.inner
            .poll(&login, now_ms, &trace_id, &rand)
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(map_error)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}
