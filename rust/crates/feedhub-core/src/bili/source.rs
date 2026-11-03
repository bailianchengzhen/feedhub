//! B站 Source 的**平台层**：请求怎么拼（URL / 参数 / 头 / 翻页游标 / WBI key 的获取与缓存）、
//! 错误怎么判、响应该交给谁解析。**收发一律通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-bilibili/BilibiliSource.kt` 的同名方法（推荐流 / 关注流 / `ensureBuvid` / `mixinKey`）：
//! 方法 + URL + 参数顺序 + 请求头顺序 + body 必须和 Kotlin 版**逐字节一样**，
//! `BiliRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 搜索 / 扫码 / 关注管理还在 Kotlin，`RustBiliSource` 逐方法委托过去。

use std::sync::{Arc, Mutex};

use crate::account::FollowPage;
use crate::bili::{self, comment, danmaku, detail, ids, sign};
use crate::detail::{CommentPage, FeedDetail, FollowKind, FollowTarget};
use crate::hosted::okhttp_form_encode;
use crate::play::{Danmaku, PlayInfo};
use crate::host::{CookieHost, CookieSpec, HostError, HttpHost, HttpRequest, HttpResponse, StoreHost};
use crate::hosted::{civil_from_days, json_string};
// 保持 `bili::source::query_encode` 这个老路径可用（`BiliRequestEquivalenceTest` 的注释里提到过它）。
pub use crate::hosted::query_encode;
use crate::json::{self, JsonMap};
use crate::model::{BiliError, Page, UserRef};

/// 线上接口（和 `BilibiliSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://api.bilibili.com";

/// Cookie 罐里 B站 的"身份 URL"（和 Kotlin 的 `WWW` 一样：按 www.bilibili.com 取 Cookie）。
const WWW_URL: &str = "https://www.bilibili.com/";

/// WBI 混淆密钥在 `ctx.store` 里的键。**格式和 Kotlin 那份一样**（`<北京时间日期>|<key>`），
/// 这样 Rust 写进去、Kotlin 读得出来（详情 / 评论这些还没搬的方法仍在 Kotlin 里签名）。
const KEY_WBI: &str = "bili.wbi_mixin_key";

/// 外壳 `code` 是 -403（签名被拒，见 `check_envelope_object`）。B站 的外壳 `code` 总在第一个键，
/// 只看开头，不为这个把整份 JSON 解一遍（`data` 里的同名键不算）。
fn signature_rejected(body: &str) -> bool {
    let head: String = body.chars().take(48).filter(|c| !c.is_whitespace()).collect();
    head.starts_with("{\"code\":-403,") || head.starts_with("{\"code\":-403}")
}

/// 动态接口的 features，照 bilby `BiliConstants.DYN_FEATURES`。
const DYN_FEATURES: &str = "itemOpusStyle,listOnlyfans,onlyfansQaCard";

/// 和 `core` 的 `UserAgents.DESKTOP_CHROME` **逐字节一样**。
///
/// 为什么 Rust 里再写一份：平台默认头属于"请求怎么发"，归 Rust；宿主客户端的
/// `DefaultHeadersInterceptor` 留着当兜底（它只补**缺失**的同名头，不会改已有的），
/// 两边漂移会被 `BiliRequestEquivalenceTest` 逐字节比对抓住。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

pub const REFERER: &str = "https://www.bilibili.com/";
pub const ORIGIN: &str = "https://www.bilibili.com";

/// 推荐流 / 关注流（和 `FeedKind` 的子集一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

/// 评论排序（Kotlin `CommentSort`）。**类型在 `crate::detail`**；
/// B站 只有热门 / 最新两档：`Default` 和 `Hot` 都发 `mode=3`（热门）。
pub use crate::detail::CommentSort;

/// `dyn:` 前缀和 BilibiliSource 的 `BiliIds.DYN` 一致。
const DYN_PREFIX: &str = "dyn:";

/// 评论区的 `type` / `oid` 缓存上限（Kotlin `TARGET_CACHE_SIZE`）。
const TARGET_CACHE_SIZE: usize = 256;

/// 楼中楼每页条数（Kotlin `SUB_REPLY_PAGE_SIZE`）。
const SUB_REPLY_PAGE_SIZE: i32 = 20;

/// `DetailTarget.Native.id` 解析出来的两种内容。
#[derive(Debug, Clone, PartialEq, Eq)]
enum BiliId {
    Video(String),
    Dynamic(String),
}

/// 和 Kotlin `BiliIds.parse` 一一对应：BV 号 / `dyn:<digits>` / 纯数字（动态）。
fn parse_id(id: &str) -> Result<BiliId, BiliError> {
    if ids::is_bvid(id) {
        return Ok(BiliId::Video(id.to_string()));
    }
    if let Some(rest) = id.strip_prefix(DYN_PREFIX) {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return Ok(BiliId::Dynamic(rest.to_string()));
        }
    }
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(BiliId::Dynamic(id.to_string()));
    }
    Err(BiliError::Api {
        code: "-400".to_string(),
        detail: format!("不认识的内容 id：{id}"),
    })
}

/// `&[("k","v")]` → 平台层的参数表（参数顺序就是请求里的顺序，**别用 Map 重排**）。
fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// B站 的平台对象：持有三个宿主端口，**自己的状态只有三个锁**（WBI key / buvid 的"只取一次"、
/// 动态评论区的 `type` / `oid` 缓存）。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustBiliSource`）建一次、跟着 Source 活；Source 被 App 缓存到进程结束。
/// 没有后台线程、没有定时器、没有连接池——所以"闲置时零唤醒"，也不需要在 `Drop` 里收尾。
pub struct BiliHostedSource {
    base: String,
    /// 宿主 HTTP 端口。同模块的其他文件（`bili/search.rs`）也要用它发请求，所以是 `pub(crate)`。
    pub(crate) http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    /// WBI key：同一时刻只让一个线程去取（和 Kotlin 的 `wbiMutex` 一样）。
    wbi: Mutex<()>,
    /// `buvid3`：只让一个线程去领（和 Kotlin 的 `buvidMutex` 一样）。
    buvid: Mutex<()>,
    /// 评论区的 `type` / `oid`：动态的 oid 不是 id 本身，要先查一次详情。语义照 Kotlin 那个
    /// `LinkedHashMap(64, 0.75f, true)`（**访问序**）：最近用过的在最后，超过 256 条丢最旧的。
    targets: Mutex<Vec<(String, detail::CommentTarget)>>,
}

impl BiliHostedSource {
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
            wbi: Mutex::new(()),
            buvid: Mutex::new(()),
            targets: Mutex::new(Vec::new()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 一页。`now_sec` 是**秒级时间戳**（WBI 的 `wts`、Cookie 过期时间、key 的"今天"都用它）——
    /// 由调用方传进来而不是这里读时钟，测试才能注入固定值把两次运行的请求逐字节比。
    pub fn feed(
        &self,
        kind: FeedKind,
        cursor: Option<&str>,
        now_sec: i64,
    ) -> Result<Page, BiliError> {
        match kind {
            FeedKind::Recommend => self.recommend(cursor, now_sec),
            FeedKind::Following => self.following(cursor, now_sec),
        }
    }

    /// `/x/web-interface/wbi/index/top/feed/rcmd`（WBI 签名，未登录也能用）。
    /// 参数照网页首页和 PiliPlus `lib/http/video.dart` 的 rcmdVideoList。
    fn recommend(&self, cursor: Option<&str>, now_sec: i64) -> Result<Page, BiliError> {
        self.ensure_buvid(now_sec)?;
        // Kotlin：`cursor?.value?.toIntOrNull() ?: 1`
        let fresh_idx = cursor.and_then(|c| c.parse::<i32>().ok()).unwrap_or(1);
        let idx = fresh_idx.to_string();
        let params = vec![
            ("web_location".to_string(), "1430650".to_string()),
            ("feed_version".to_string(), "V8".to_string()),
            ("homepage_ver".to_string(), "1".to_string()),
            ("fresh_type".to_string(), "4".to_string()),
            ("ps".to_string(), "12".to_string()),
            ("fresh_idx".to_string(), idx.clone()),
            ("fresh_idx_1h".to_string(), idx.clone()),
            ("brush".to_string(), idx.clone()),
        ];
        let body = self.get_body("/x/web-interface/wbi/index/top/feed/rcmd", &params, true, now_sec)?;
        bili::parse_recommend(&body, fresh_idx as i64)
    }

    /// `/x/polymer/web-dynamic/v1/feed/all`（要登录，不签名）。
    fn following(&self, cursor: Option<&str>, now_sec: i64) -> Result<Page, BiliError> {
        self.ensure_buvid(now_sec)?;
        let mut params = vec![
            ("type".to_string(), "all".to_string()),
            ("timezone_offset".to_string(), "-480".to_string()),
            ("platform".to_string(), "web".to_string()),
            ("features".to_string(), DYN_FEATURES.to_string()),
        ];
        if let Some(c) = cursor {
            params.push(("offset".to_string(), c.to_string()));
        }
        let body = self.get_body("/x/polymer/web-dynamic/v1/feed/all", &params, false, now_sec)?;
        bili::parse_dynamics(&body)
    }

    // 详情 / 评论

    /// 详情：视频走 `/x/web-interface/wbi/view/detail`，动态 / 图文 / 专栏走 `v1/detail` +（需要时）`opus/detail`。
    /// `id` 是 [`crate::model::DetailTarget::Native`] 的 id（BV 号 / `dyn:<id_str>` / 纯数字）。
    pub fn detail(&self, id: &str, now_sec: i64) -> Result<FeedDetail, BiliError> {
        self.ensure_buvid(now_sec)?;
        match parse_id(id)? {
            BiliId::Video(bvid) => {
                let json = self.get_json(
                    "/x/web-interface/wbi/view/detail",
                    &pairs(&[("bvid", &bvid)]),
                    true,
                    true,
                    now_sec,
                )?;
                detail::video_detail(&json, self.logged_in())
                    .map_err(|detail| BiliError::Parse { detail })
            }
            BiliId::Dynamic(dyn_id) => self.dynamic_detail(&dyn_id, now_sec),
        }
    }

    /// 动态详情。`v1/detail` 给条目、计数、转发的原动态和评论区位置；图文 / 专栏的全文另从 `opus/detail` 取。
    /// `v1/detail` 没登录会 -352，这里翻译成"需要登录"（和 Kotlin 一样）。
    fn dynamic_detail(&self, dyn_id: &str, now_sec: i64) -> Result<FeedDetail, BiliError> {
        let json = self.dynamic_json(dyn_id, now_sec)?;
        if let Some(target) = detail::comment_target(&json) {
            self.cache_target(&ids::dynamic_native_id(dyn_id), target);
        }
        let opus = if detail::is_opus(&json) {
            let params = pairs(&[
                ("id", dyn_id),
                ("timezone_offset", "-480"),
                ("features", "htmlNewStyle"),
            ]);
            match self.get_json(
                "/x/polymer/web-dynamic/v1/opus/detail",
                &params,
                false,
                true,
                now_sec,
            ) {
                Ok(opus) => Some(opus),
                // 全文拿不到就用动态自带的摘要（Kotlin 只吞 ApiException，风控照抛）
                Err(BiliError::Api { .. }) => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        detail::dynamic_detail(&json, opus.as_ref()).map_err(|detail| BiliError::Parse { detail })
    }

    /// `v1/detail`：要登录，不签名。没登录时的 -352 翻译成 `NotLoggedIn`。
    fn dynamic_json(&self, dyn_id: &str, now_sec: i64) -> Result<JsonMap, BiliError> {
        let params = pairs(&[
            ("id", dyn_id),
            ("timezone_offset", "-480"),
            ("features", DYN_FEATURES),
        ]);
        match self.get_json("/x/polymer/web-dynamic/v1/detail", &params, false, true, now_sec) {
            Ok(json) => Ok(json),
            Err(BiliError::RiskControl {
                code: Some(code), ..
            }) if code == "-352" && !self.logged_in() => Err(BiliError::NotLoggedIn {
                detail: Some("动态详情需要登录".to_string()),
            }),
            Err(e) => Err(e),
        }
    }

    /// 一级评论。`mode` 3 热门 / 2 最新（`Default` 和 `Hot` 都是热门），翻页用
    /// `pagination_str={"offset":"…"}`，游标就是上一页给的 `next_offset`。参数照网页（`web_location=1315875`）。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_sec: i64,
    ) -> Result<CommentPage, BiliError> {
        self.ensure_buvid(now_sec)?;
        let target = self.comment_target(id, now_sec)?;
        let pagination = format!("{{\"offset\":{}}}", json_string(cursor.unwrap_or("")));
        let params = vec![
            ("oid".to_string(), target.oid),
            ("type".to_string(), target.comment_type.to_string()),
            (
                "mode".to_string(),
                if sort == CommentSort::Newest { "2" } else { "3" }.to_string(),
            ),
            ("pagination_str".to_string(), pagination),
            ("plat".to_string(), "1".to_string()),
            ("web_location".to_string(), "1315875".to_string()),
        ];
        let json = self.get_json("/x/v2/reply/wbi/main", &params, true, true, now_sec)?;
        comment::main_list(&json).map_err(|detail| BiliError::Parse { detail })
    }

    /// 楼中楼：页码翻页（游标是下一页的页码），每页 20 条。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_sec: i64,
    ) -> Result<CommentPage, BiliError> {
        self.ensure_buvid(now_sec)?;
        let target = self.comment_target(id, now_sec)?;
        // Kotlin：`cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1`
        let pn = cursor
            .and_then(|c| c.parse::<i32>().ok())
            .map(|n| n.max(1))
            .unwrap_or(1);
        let params = vec![
            ("oid".to_string(), target.oid),
            ("type".to_string(), target.comment_type.to_string()),
            ("root".to_string(), comment_id.to_string()),
            ("pn".to_string(), pn.to_string()),
            ("ps".to_string(), SUB_REPLY_PAGE_SIZE.to_string()),
        ];
        let json = self.get_json("/x/v2/reply/reply", &params, false, true, now_sec)?;
        comment::sub_replies(&json, pn, SUB_REPLY_PAGE_SIZE)
            .map_err(|detail| BiliError::Parse { detail })
    }

    // 播放 / 弹幕

    /// 播放地址：`/x/player/wbi/playurl`（**WBI 签名**）。
    ///
    /// `fnval=4048` + `fourk=1` 要全部 DASH 变体（各清晰度 × AVC / HEVC / AV1，另有杜比 / 无损音轨）；
    /// [`quality`] 没给时用 127（8K，即"能给的最高"），B站 只会给当前账号有权限的那些。
    /// 拉流要带 [`bili::play::play_headers`] 的那两个头（Referer + UA），否则 CDN 403。
    ///
    /// `id` 必须是 BV 号（视频才有播放地址），`part_id` 是分 P 的 cid（[`crate::detail::VideoPart::id`]）。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        quality: Option<i32>,
        now_sec: i64,
    ) -> Result<PlayInfo, BiliError> {
        self.ensure_buvid(now_sec)?;
        let bvid = self.video_id(id)?;
        let params = vec![
            ("bvid".to_string(), bvid),
            ("cid".to_string(), part_id.to_string()),
            ("qn".to_string(), quality.unwrap_or(127).to_string()),
            ("fnval".to_string(), "4048".to_string()),
            ("fnver".to_string(), "0".to_string()),
            ("fourk".to_string(), "1".to_string()),
        ];
        let json = self.get_json("/x/player/wbi/playurl", &params, true, true, now_sec)?;
        bili::play::parse(&json, bili::play::play_headers())
    }

    /// 分段弹幕：`/x/v2/dm/wbi/web/seg.so`（**WBI 签名**，响应是裸 protobuf）。
    ///
    /// 每段 6 分钟、[`segment`] 从 1 开始；超出视频长度的段 B站 回 **304 空响应**，这里按空列表处理。
    /// 出错时（风控等）B站 回的是 **JSON**，所以响应首字节是 `{` 时按 `{code}` 外壳判一次、返回空列表
    /// （`code` 非 0 照常抛）。
    pub fn danmaku(
        &self,
        id: &str,
        part_id: &str,
        segment: i32,
        now_sec: i64,
    ) -> Result<Vec<Danmaku>, BiliError> {
        self.ensure_buvid(now_sec)?;
        let bvid = self.video_id(id)?;
        let aid = self.aid_of(&bvid)?;
        let params = vec![
            ("type".to_string(), "1".to_string()),
            ("oid".to_string(), part_id.to_string()),
            ("pid".to_string(), aid.to_string()),
            ("segment_index".to_string(), segment.to_string()),
        ];
        let url = self.signed_url("/x/v2/dm/wbi/web/seg.so", &params, now_sec)?;
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: default_headers(),
                body: Vec::new(),
            })
            .map_err(BiliError::from)?;
        // 超出视频长度的段：304（不是错误，空列表）
        if response.status == 304 {
            return Ok(Vec::new());
        }
        if matches!(response.status, 403 | 412 | 429) {
            return Err(BiliError::RiskControl {
                code: Some(format!("HTTP {}", response.status)),
                detail: "请求被拦截".to_string(),
            });
        }
        if !(200..=299).contains(&response.status) {
            // Kotlin 这里不贴响应内容（和 `check_status` 不同），照抄
            return Err(BiliError::Api {
                code: format!("HTTP {}", response.status),
                detail: "弹幕请求失败".to_string(),
            });
        }
        // 风控 / 出错时 B站 回 JSON 而不是 protobuf
        if response.body.first() == Some(&b'{') {
            let body = String::from_utf8_lossy(&response.body);
            bili::check_envelope(&body)?;
            return Ok(Vec::new());
        }
        danmaku::segment(&response.body)
    }

    /// 播放 / 弹幕只对视频有意义（Kotlin `bvidOf`）：不是 BV 号就是 `-400`。
    fn video_id(&self, id: &str) -> Result<String, BiliError> {
        if ids::is_bvid(id) {
            Ok(id.to_string())
        } else {
            Err(BiliError::Api {
                code: "-400".to_string(),
                detail: format!("不是视频 id：{id}"),
            })
        }
    }

    /// BV 号 → aid（Kotlin `aidOf`）：换算不出来就是 `-400`。
    fn aid_of(&self, bvid: &str) -> Result<i64, BiliError> {
        ids::to_aid(bvid).ok_or_else(|| BiliError::Api {
            code: "-400".to_string(),
            detail: format!("无效的 BV 号：{bvid}"),
        })
    }

    /// 评论区的 `type` / `oid`。视频的 oid 是 aid（BV 号直接算），动态要先查一次详情。
    fn comment_target(&self, id: &str, now_sec: i64) -> Result<detail::CommentTarget, BiliError> {
        match parse_id(id)? {
            BiliId::Video(bvid) => {
                let aid = ids::to_aid(&bvid).ok_or_else(|| BiliError::Api {
                    code: "-400".to_string(),
                    detail: format!("无效的 BV 号：{bvid}"),
                })?;
                Ok(detail::CommentTarget {
                    comment_type: 1,
                    oid: aid.to_string(),
                })
            }
            BiliId::Dynamic(dyn_id) => {
                let key = ids::dynamic_native_id(&dyn_id);
                if let Some(target) = self.cached_target(&key) {
                    return Ok(target);
                }
                let json = self.dynamic_json(&dyn_id, now_sec)?;
                let target = detail::comment_target(&json).ok_or_else(|| BiliError::Parse {
                    detail: format!("动态 {dyn_id} 的详情里没有评论区信息"),
                })?;
                self.cache_target(&key, target.clone());
                Ok(target)
            }
        }
    }

    fn cached_target(&self, key: &str) -> Option<detail::CommentTarget> {
        let mut targets = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        let index = targets.iter().position(|(k, _)| k == key)?;
        let entry = targets.remove(index);
        let target = entry.1.clone();
        targets.push(entry);
        Some(target)
    }

    fn cache_target(&self, key: &str, target: detail::CommentTarget) {
        let mut targets = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = targets.iter().position(|(k, _)| k == key) {
            targets.remove(index);
        }
        targets.push((key.to_string(), target));
        while targets.len() > TARGET_CACHE_SIZE {
            targets.remove(0);
        }
    }

    fn logged_in(&self) -> bool {
        self.cookies.find(WWW_URL, "SESSDATA").is_some()
    }

    /// 没有 `buvid3` 时很多接口会 -352。从浏览器导入的 Cookie 里一般都有；没有就从 spi 接口领一个，种进 Cookie 罐。
    pub(crate) fn ensure_buvid(&self, now_sec: i64) -> Result<(), BiliError> {
        let _guard = self.buvid.lock().unwrap_or_else(|e| e.into_inner());
        if self.cookies.find(WWW_URL, "buvid3").is_some() {
            return Ok(());
        }
        let json = self.get_json("/x/frontend/finger/spi", &[], false, true, now_sec)?;
        let data = json::path(&json, &["data"]).ok_or_else(|| BiliError::Parse {
            detail: "spi 缺少 data".to_string(),
        })?;
        let expires = now_sec.saturating_mul(1000) + 365 * 24 * 3600 * 1000;
        let mut cookies = Vec::new();
        for (field, name) in [("b_3", "buvid3"), ("b_4", "buvid4")] {
            if let Some(value) = json::string(data, field) {
                cookies.push(CookieSpec {
                    name: name.to_string(),
                    value,
                    domain: "bilibili.com".to_string(),
                    path: "/".to_string(),
                    expires_at_ms: expires,
                });
            }
        }
        self.cookies.put(WWW_URL, cookies);
        Ok(())
    }

    /// WBI 混淆密钥，按**北京时间**每天从 nav 取一次，缓存在宿主 store 里（键和格式与 Kotlin 版一致）。
    fn mixin_key(&self, now_sec: i64) -> Result<String, BiliError> {
        let _guard = self.wbi.lock().unwrap_or_else(|e| e.into_inner());
        let today = beijing_date(now_sec);
        if let Some(cached) = self.store.get(KEY_WBI) {
            let mut parts = cached.split('|');
            if let (Some(day), Some(key), None) = (parts.next(), parts.next(), parts.next()) {
                if day == today {
                    return Ok(key.to_string());
                }
            }
        }
        // nav 未登录时 code=-101，但 wbi_img 照样给，所以不检查 code
        let nav = self.get_json("/x/web-interface/nav", &[], false, false, now_sec)?;
        let (img, sub) = bili::wbi_keys_from_nav_object(&nav).ok_or_else(|| BiliError::Parse {
            detail: "nav 里没有 wbi_img".to_string(),
        })?;
        let key = sign::mixin_key(&img, &sub);
        self.store.put(KEY_WBI, Some(&format!("{today}|{key}")));
        Ok(key)
    }

    /// 要签名的那条：**先签参数、再拼 URL**（Kotlin 是从拼好的 URL 里读回参数再签，两者等价：
    /// 读回来的就是同一批参数、同一个顺序；已在 URL 里的 `wts` / `w_rid` 本来就没有）。
    pub(crate) fn signed_url(
        &self,
        path: &str,
        params: &[(String, String)],
        now_sec: i64,
    ) -> Result<String, BiliError> {
        let key = self.mixin_key(now_sec)?;
        let signed = sign::sign(params, &key, now_sec);
        Ok(self.url_for(path, &signed))
    }

    pub(crate) fn url_for(&self, path: &str, params: &[(String, String)]) -> String {
        let mut url = format!("{}{}", self.base, path);
        if !params.is_empty() {
            url.push('?');
            let q = params
                .iter()
                .map(|(k, v)| format!("{}={}", query_encode(k), query_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.push_str(&q);
        }
        url
    }

    pub(crate) fn get_body(
        &self,
        path: &str,
        params: &[(String, String)],
        wbi: bool,
        now_sec: i64,
    ) -> Result<String, BiliError> {
        let url = if wbi {
            self.signed_url(path, params, now_sec)?
        } else {
            self.url_for(path, params)
        };
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: default_headers(),
                body: Vec::new(),
            })
            .map_err(BiliError::from)?;
        check_status(&response)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        if wbi && signature_rejected(&body) {
            // 只作废缓存的混淆密钥、**不当场重试**，
            // 错误照常往上抛；下一次用户刷新时自然重取 nav。
            self.store.put(KEY_WBI, None);
        }
        Ok(body)
    }

    fn get_json(
        &self,
        path: &str,
        params: &[(String, String)],
        wbi: bool,
        check_code: bool,
        now_sec: i64,
    ) -> Result<JsonMap, BiliError> {
        let body = self.get_body(path, params, wbi, now_sec)?;
        let json = json::parse_object(&body).map_err(|detail| BiliError::Parse { detail })?;
        if check_code {
            bili::check_envelope_object(&json)?;
            Ok(json)
        } else {
            Ok(json)
        }
    }

    // 账号：我是谁 / 关注列表 / 关注·取关

    /// `whoami`：`/x/web-interface/nav`（不签名、不需要 `buvid3`）。
    /// 外壳 `code` 0 → 账号；-101 → 未登录（`None`，不抛）；其他 → 统一错误映射。
    pub fn whoami(&self, now_sec: i64) -> Result<Option<UserRef>, BiliError> {
        let body = self.get_body("/x/web-interface/nav", &[], false, now_sec)?;
        Self::parse_whoami(&body)
    }

    /// `whoami` 的解析部分（扫码登录验证时用同一份，见 [`crate::bili::qr`]）。
    pub fn parse_whoami(body: &str) -> Result<Option<UserRef>, BiliError> {
        let json = json::parse_object(body).map_err(|detail| BiliError::Parse { detail })?;
        match json::long(&json, "code") {
            Some(0) => Ok(bili::account::nav_user(&json)),
            Some(-101) => Ok(None),
            // Kotlin：`check(json).let { BiliParser.nav(it) }`——先按错误码翻译（缺 code 也是解析错误）
            _ => {
                bili::check_envelope_object(&json)?;
                Ok(bili::account::nav_user(&json))
            }
        }
    }

    /// `/x/relation/followings`：当前账号关注的 UP 主，按最近关注倒序，每页 50 个，游标是页码。
    pub fn followings(&self, cursor: Option<&str>, now_sec: i64) -> Result<FollowPage, BiliError> {
        self.ensure_buvid(now_sec)?;
        // Kotlin：`cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1`
        let pn = cursor
            .and_then(|c| c.parse::<i32>().ok())
            .map(|n| n.max(1))
            .unwrap_or(1);
        let vmid = self.self_mid(now_sec)?;
        let params = vec![
            ("vmid".to_string(), vmid),
            ("pn".to_string(), pn.to_string()),
            ("ps".to_string(), FOLLOWINGS_PAGE_SIZE.to_string()),
            ("order".to_string(), "desc".to_string()),
            ("order_type".to_string(), String::new()),
        ];
        let json = self.get_json("/x/relation/followings", &params, false, true, now_sec)?;
        bili::account::followings_page(&json, pn, FOLLOWINGS_PAGE_SIZE)
    }

    /// 自己的 mid：Cookie 里的 `DedeUserID`，没有就问 `nav`（Kotlin `selfMid`）。
    fn self_mid(&self, now_sec: i64) -> Result<String, BiliError> {
        if let Some(mid) = self.cookies.find(WWW_URL, "DedeUserID") {
            if !mid.is_empty() && mid.chars().all(|c| c.is_ascii_digit()) {
                return Ok(mid);
            }
        }
        match self.whoami(now_sec)? {
            Some(user) => Ok(user.id),
            None => Err(BiliError::NotLoggedIn { detail: None }),
        }
    }

    /// 关注（`act=1`）/ 取关（`act=2`）。这是唯一的写操作：只在用户点按钮时调用，**不重试**。
    ///
    /// 请求照网页（bilby `RelationRepository.modify`、PiliPlus `video.dart` 的 `relationMod`）：
    /// `statistics` 和 `x-bili-device-req-json` 放 query，业务字段放表单，Referer / Origin 指向 UP 主的空间页。
    /// 关注时服务端回 22014（已经关注了）按成功处理。
    pub fn set_following(
        &self,
        target: &FollowTarget,
        follow: bool,
        now_sec: i64,
    ) -> Result<(), BiliError> {
        if target.platform != "bili"
            || target.kind != FollowKind::User
            || target.id.is_empty()
            || !target.id.chars().all(|c| c.is_ascii_digit())
        {
            return Err(BiliError::Api {
                code: "-400".to_string(),
                detail: format!(
                    "B站只能关注 UP 主（kind=USER，id 是 mid）：{} {}",
                    match target.kind {
                        FollowKind::User => "USER",
                        FollowKind::Forum => "FORUM",
                    },
                    target.id
                ),
            });
        }
        let csrf = self
            .cookies
            .find(WWW_URL, "bili_jct")
            .filter(|v| !v.is_empty())
            .ok_or(BiliError::NotLoggedIn { detail: None })?;
        self.ensure_buvid(now_sec)?;
        let url = self.url_for(
            "/x/relation/modify",
            &pairs(&[
                ("statistics", "{\"appId\":100,\"platform\":5}"),
                (
                    "x-bili-device-req-json",
                    "{\"platform\":\"web\",\"device\":\"pc\",\"spmid\":\"333.1387\"}",
                ),
            ]),
        );
        // FormBody 的字段顺序就是 body 里的顺序（OkHttp 的 `FormBody.Builder.add` 口径，见 `okhttp_form_encode`）
        let extend = format!(
            "{{\"entity\":\"user\",\"entity_id\":{},\"fp\":\"{}\"}}",
            target.id, USER_AGENT
        );
        let form = [
            ("fid", target.id.clone()),
            ("act", if follow { "1" } else { "2" }.to_string()),
            ("re_src", RE_SRC_SPACE.to_string()),
            ("gaia_source", "web_main".to_string()),
            ("spmid", "333.1387".to_string()),
            ("extend_content", extend),
            ("csrf", csrf),
        ];
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", okhttp_form_encode(k), okhttp_form_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        // 请求级头：Referer / Origin（Kotlin 显式设的），默认头只补缺的 User-Agent，最后是表单的 Content-Type
        // （宿主接口只有 body 字节，Content-Type 只能自己写；位置和 OkHttp `BridgeInterceptor` 补的一样）。
        let mut headers = vec![
            (
                "Referer".to_string(),
                format!("https://space.bilibili.com/{}/dynamic", target.id),
            ),
            ("Origin".to_string(), "https://space.bilibili.com".to_string()),
        ];
        headers.extend(default_headers_missing(&headers));
        headers.push((
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ));
        let response = self
            .http
            .execute(HttpRequest {
                method: "POST".to_string(),
                url,
                headers,
                body: body.into_bytes(),
            })
            .map_err(BiliError::from)?;
        check_status(&response)?;
        let json = json::parse_object(&String::from_utf8_lossy(&response.body))
            .map_err(|detail| BiliError::Parse { detail })?;
        // 关注时服务端回 22014（已经关注了）按成功处理
        if follow && json::long(&json, "code") == Some(CODE_ALREADY_FOLLOWING) {
            return Ok(());
        }
        bili::check_envelope_object(&json)
    }
}

/// 关注列表每页 50 个（Kotlin `FOLLOWINGS_PAGE_SIZE`）。
const FOLLOWINGS_PAGE_SIZE: i32 = 50;

/// 关注请求的 `re_src`（Kotlin `RE_SRC_SPACE`）。
const RE_SRC_SPACE: &str = "11";

/// 关注时服务端回"已经关注了"的业务码（Kotlin `CODE_ALREADY_FOLLOWING`）。
const CODE_ALREADY_FOLLOWING: i64 = 22014;

/// 平台默认头，顺序照 Kotlin 的 `DefaultHeadersInterceptor`（User-Agent / Referer / Origin）。
pub(crate) fn default_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Referer".to_string(), REFERER.to_string()),
        ("Origin".to_string(), ORIGIN.to_string()),
    ]
}

/// 平台默认头里**还没有**的那几个（OkHttp 的 `DefaultHeadersInterceptor` 只补缺失的同名头，
/// 而且补在请求级头**后面**）。写请求自带 `Referer` / `Origin` 时必须用它，顺序才和 Kotlin 版一样。
fn default_headers_missing(existing: &[(String, String)]) -> Vec<(String, String)> {
    default_headers()
        .into_iter()
        .filter(|(name, _)| !existing.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
        .collect()
}

/// HTTP 状态判定，**照 Kotlin `getBody` 的三行**抄。
fn check_status(response: &HttpResponse) -> Result<(), BiliError> {
    if matches!(response.status, 403 | 412 | 429) {
        return Err(BiliError::RiskControl {
            code: Some(format!("HTTP {}", response.status)),
            detail: "请求被拦截".to_string(),
        });
    }
    if !(200..=299).contains(&response.status) {
        return Err(BiliError::Api {
            code: format!("HTTP {}", response.status),
            detail: json::take_chars(&String::from_utf8_lossy(&response.body), 200),
        });
    }
    Ok(())
}

impl From<HostError> for BiliError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => BiliError::Network { detail },
            HostError::ProxyUnavailable { detail } => BiliError::ProxyUnavailable { detail },
            HostError::Other { detail } => BiliError::Host { detail },
        }
    }
}

/// 秒级时间戳 → 北京时间（UTC+8）的 `YYYY-MM-DD`，和 Kotlin `LocalDate.now(ZoneId.of("Asia/Shanghai")).toString()` 一致。
pub fn beijing_date(now_sec: i64) -> String {
    let days = (now_sec + 8 * 3600).div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

// `query_encode` 从 `crate::hosted` 导入（口径和注释都在那里，别在这里再写一份）。

