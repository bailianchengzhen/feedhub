//! **X 的平台对象**：推荐流 / 关注流的**整个请求逻辑**（拼 URL / 参数顺序 / features 的字段顺序 /
//! 请求头顺序 / 翻页游标 / 页数账本 / 状态码与 `errors[]` 判定）+ 解析，都在 [`FfiXSource`] 后面；
//! **`x-client-transaction-id` 的算法**在 [`FfiXTransaction`] 后面（调用时机在宿主的拦截器里）。
//!
//! 宿主要做的只有两件（构造时塞进来）：发请求、读 Cookie 罐；报页数 / 查登录态都在 Rust 里判。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiXSource(base, queryIds, features, fieldToggles, hostHttp, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.feed(FfiFeedKind.RECOMMEND, cursor, staleRetry = true) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象没有内部线程 / 定时器（只有两个 `Mutex` 的状态），
//! 闲置时零唤醒。
//!
//! **queryId 过期是"数据"不是"错误"**：404 / 422 时返回 [`FfiXFeedOutcome::stale_query_id`]，
//! 宿主扫完网页脚本再调 `set_query_ids` + 重打一次（扫脚本走的是宿主那份页面客户端，不在这个对象里）。

use std::sync::Arc;

use feedhub_core::x::source::{XFeedOutcome, XHostedSource};
use feedhub_core::x::txid::XTransaction;
use feedhub_core::x::{FeedKind, XError};

use crate::bili_source::FfiFeedKind;
use crate::host::{CookiesAdapter, FfiHeader, FfiHttpRequest, FfiHttpResponse, HostCookies, HostHttp, HttpAdapter};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_play, to_ffi_user, FfiCommentPage,
    FfiCommentSort, FfiDetail, FfiError, FfiPage, FfiParam, FfiPlayInfo, FfiUserRef,
};

const PLATFORM: &str = "x";

pub(crate) fn map_error(e: XError) -> FfiError {
    let platform = PLATFORM.to_string();
    match e {
        XError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        XError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        XError::Signature { detail } => FfiError::Signature { platform, detail },
        XError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        XError::Parse { detail } => FfiError::Parse { platform, detail },
        XError::Network { detail } => FfiError::Network { platform, detail },
        XError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        XError::Host { detail } => FfiError::Internal { platform, detail },
        XError::Internal { detail } => FfiError::Internal { platform, detail },
    }
}

/// 一个有序的 GraphQL 布尔开关（`features` / `fieldToggles`）。
/// **顺序就是 URL 里的字段顺序**，所以用 `Vec` 而不是 Map。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXFlag {
    pub name: String,
    pub value: bool,
}

/// `feed()` 的结果：要么一页，要么"queryId 可能过期了"（由调用方换 id 再打一次）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXFeedOutcome {
    /// 拿到页了。
    pub page: Option<FfiPage>,
    /// `true` = 服务端回 404 / 422，调用方应当扫一次网页脚本换 queryId 后重试。
    pub stale_query_id: bool,
}

/// X 平台对象（`uniffi::Object`）。
/// [`FfiXSource::with_net`] 的数据参数。
#[derive(uniffi::Record)]
pub struct FfiXSourceConfig {
    pub base: String,
    pub page_origin: String,
    pub txid_enabled: bool,
    pub profile_json: Option<String>,
    pub sign_chain_json: Option<String>,
    pub query_ids: Vec<FfiParam>,
    pub features: Vec<FfiXFlag>,
    pub field_toggles: Vec<FfiXFlag>,
    pub detail_field_toggles: Vec<FfiXFlag>,
    pub api_spec: crate::net_host::FfiHostSpec,
    pub page_spec: crate::net_host::FfiHostSpec,
}

#[derive(uniffi::Object)]
pub struct FfiXSource {
    inner: XHostedSource,
}

#[uniffi::export]
impl FfiXSource {
    /// [`base`] 是接口根（线上 `https://x.com`，测试指向 MockWebServer）；
    /// [`query_ids`] 是 operation 名 → queryId（Kotlin 的 `XClientProfile` + 扫脚本的缓存，读好后传进来）；
    /// [`features`] / [`field_toggles`] 是**有序**的布尔开关（顺序就是 URL 里的字段顺序）；
    /// [`detail_field_toggles`] 是**推文详情**那一份。
    ///
    /// [`page_origin`] / [`page_http`] 是**取页面**（`whoami` 的 `x.com/home`、txid 链那几跳）用的站点根与
    /// 客户端：和 API 那份不是一回事（页面客户端只有浏览器头 + 页面那一档限速）。
    #[uniffi::constructor]
    pub fn new(
        base: String,
        page_origin: String,
        query_ids: Vec<FfiParam>,
        features: Vec<FfiXFlag>,
        field_toggles: Vec<FfiXFlag>,
        detail_field_toggles: Vec<FfiXFlag>,
        http: Arc<dyn HostHttp>,
        page_http: Arc<dyn HostHttp>,
        cookies: Arc<dyn HostCookies>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: XHostedSource::new(
                base,
                page_origin,
                Arc::new(HttpAdapter(http)),
                Arc::new(HttpAdapter(page_http)),
                Arc::new(CookiesAdapter(cookies)),
                query_ids.into_iter().map(|p| (p.name, p.value)).collect(),
                features.into_iter().map(|f| (f.name, f.value)).collect(),
                field_toggles.into_iter().map(|f| (f.name, f.value)).collect(),
                detail_field_toggles
                    .into_iter()
                    .map(|f| (f.name, f.value))
                    .collect(),
            ),
        })
    }

    /// **Rust 联网层的构造**：两个宿主 HTTP 端口换成两份 [`FfiHostSpec`]。
    ///
    /// 两份不能混（`whoami` 走页面客户端，API 请求走接口客户端）：
    /// - `api_spec`：浏览器头 + **登录头**（`after_limit`）+ **2500ms 限速** + **txid**（`after_limit`，
    ///   睡完了再算）+ 响应头记账（`after_headers`）；
    /// - `page_spec`：页面头 + **1000ms 限速**，**没有** auth / txid。
    ///
    /// `auth` 是**每次请求现算**的闭包（`ct0` 可能刚变）；`txid` 是 `(方法, 路径) -> 头值`。
    /// `on_headers` 把响应头交回宿主记账（`x-rate-limit-*` 的冷却还在 Kotlin）。
    ///
    /// 数据参数收成一个 [`FfiXSourceConfig`] 记录：原来 15 个参数，按值传的
    /// `RustBuffer` 有几个要落到栈上传，安卓上崩过一次"第 12 个参数的指针是 0x1"（JNA + libffi 搬栈参数）。
    /// 现在只剩 5 个参数，全在寄存器里。
    #[uniffi::constructor]
    pub fn with_net(
        config: FfiXSourceConfig,
        stack: Arc<crate::net_host::FfiNetStack>,
        auth: Option<Arc<dyn crate::net_host::HostAuthHeaders>>,
        txid: Option<Arc<dyn crate::net_host::HostTxid>>,
        on_headers: Option<Arc<dyn crate::net_host::HostHeaders>>,
    ) -> Result<Arc<Self>, crate::net::FfiNetError> {
        let FfiXSourceConfig {
            base,
            page_origin,
            txid_enabled,
            profile_json,
            sign_chain_json,
            query_ids,
            features,
            field_toggles,
            detail_field_toggles,
            api_spec,
            page_spec,
        } = config;
        let http = stack.build_host_with_hooks(api_spec, auth, txid, on_headers)?;
        let page_http = stack.build_host_with_hooks(page_spec, None, None, None)?;
        Ok(Arc::new(Self {
            inner: XHostedSource::new(
                base,
                page_origin,
                http,
                page_http,
                stack.cookies_host(),
                query_ids.into_iter().map(|p| (p.name, p.value)).collect(),
                features.into_iter().map(|f| (f.name, f.value)).collect(),
                field_toggles.into_iter().map(|f| (f.name, f.value)).collect(),
                detail_field_toggles
                    .into_iter()
                    .map(|f| (f.name, f.value))
                    .collect(),
            )
            .with_profile_json(profile_json.as_deref())
            .with_sign_chain_json(sign_chain_json.as_deref())
            .with_auto_scan_query_id(true)
            .with_txid_enabled(txid_enabled),
        }))
    }

    /// 一页。`cursor == None` 是下拉刷新（第一页）；[`stale_retry`] 为 true 时 404 / 422 会回
    /// `stale_query_id = true` 而不是报错（调用方换 id 后再用 false 调一次）。
    pub fn feed(
        &self,
        kind: FfiFeedKind,
        cursor: Option<String>,
        stale_retry: bool,
    ) -> Result<FfiXFeedOutcome, FfiError> {
        let kind = match kind {
            FfiFeedKind::Recommend => FeedKind::Recommend,
            FfiFeedKind::Following => FeedKind::Following,
        };
        match self.inner.feed(kind, cursor.as_deref(), stale_retry) {
            Ok(XFeedOutcome::Page(page)) => Ok(FfiXFeedOutcome {
                page: Some(to_ffi_page(&page)),
                stale_query_id: false,
            }),
            Ok(XFeedOutcome::StaleQueryId) => Ok(FfiXFeedOutcome {
                page: None,
                stale_query_id: true,
            }),
            Err(e) => Err(map_error(e)),
        }
    }

    /// 登录态 / 账号：`GET <page_origin>/home` 的 HTML 里抠 `user_id` / `screen_name`
    ///
    /// 没登录时**一个请求都不发**；其余情况**任何失败都回 `None`**（Kotlin 是 `runCatching{}` 吞掉：
    /// 代理不可用 / 连不上 / 非 2xx 都算"拿不到"，`UserRef` 的约定就是不抛）。
    pub fn whoami(&self) -> Option<FfiUserRef> {
        self.inner.whoami().as_ref().map(to_ffi_user)
    }

    /// 详情（`TweetDetail`，`focalTweetId`）：焦点推文 + 正文（"整条就是视频"时带分 P）。
    /// 没登录时**一个请求都不发**。
    pub fn detail(&self, id: String) -> Result<FfiDetail, FfiError> {
        self.inner
            .detail(&id)
            .map(|detail| to_ffi_detail(&detail))
            .map_err(map_error)
    }

    /// 一级评论（同一条 `TweetDetail`）。游标是 `bottom` 那条（`None` = 第一页）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        sort: FfiCommentSort,
    ) -> Result<FfiCommentPage, FfiError> {
        let sort = match sort {
            FfiCommentSort::Default => feedhub_core::detail::CommentSort::Default,
            FfiCommentSort::Hot => feedhub_core::detail::CommentSort::Hot,
            FfiCommentSort::Newest => feedhub_core::detail::CommentSort::Newest,
        };
        self.inner
            .comments(&id, cursor.as_deref(), sort)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 楼中楼：**当前返回空页**（和 Kotlin 一样，X 把自回复串随评论一起给）。
    pub fn replies(&self, id: String, comment_id: String) -> Result<FfiCommentPage, FfiError> {
        self.inner
            .replies(&id, &comment_id)
            .map(|page| to_ffi_comment_page(&page))
            .map_err(map_error)
    }

    /// 换一份 queryId（宿主扫完网页脚本后调它）。空值会被忽略。
    /// **播放地址**（`partId = media:<下标>`）：每次都重新拉一次 `TweetDetail`（地址带时效签名），
    /// 再按码率降序把那个视频的 mp4 variant 全放进 `progressive`（一个 mp4 都没有就退回第一个有 url 的，
    /// 一般是直播的 HLS）。
    ///
    /// `partId` 写法不对在**发请求之前**就报错；没登录一条 GraphQL 都不发。
    pub fn play(&self, id: String, part_id: String) -> Result<FfiPlayInfo, FfiError> {
        self.inner
            .play(&id, &part_id)
            .map(|info| to_ffi_play(&info))
            .map_err(map_error)
    }

    pub fn set_query_ids(&self, query_ids: Vec<FfiParam>) {
        self.inner
            .set_query_ids(query_ids.into_iter().map(|p| (p.name, p.value)).collect());
    }

    /// 扫脚本刷新 queryId。
    pub fn refresh_query_id(&self, kind: FfiFeedKind) -> Option<String> {
        let op = match kind {
            FfiFeedKind::Recommend => FeedKind::Recommend.operation(),
            FfiFeedKind::Following => FeedKind::Following.operation(),
        };
        self.inner.refresh_query_id(op)
    }

    /// raw 请求：先 ensure_txid()，相对路径拼 base，记录响应头。
    pub fn raw(&self, request: FfiHttpRequest) -> Result<FfiHttpResponse, FfiError> {
        let headers = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        let body = if request.body.is_empty() {
            None
        } else {
            Some(request.body)
        };
        let response = self
            .inner
            .raw(request.method, request.url, headers, body)
            .map_err(map_error)?;
        Ok(FfiHttpResponse {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| FfiHeader { name, value })
                .collect(),
            body: response.body,
            final_url: response.final_url,
        })
    }

    /// 上一次 txid 生成失败的原因（没失败为 None）。
    pub fn last_txid_error(&self) -> Option<String> {
        self.inner.last_txid_error()
    }

    /// 生成 txid：仅对 `/i/api/graphql/` 生效。
    pub fn generate_txid(
        &self,
        method: String,
        path: String,
        now_ms: i64,
        random_byte: u8,
    ) -> Option<String> {
        self.inner.generate_txid(&method, &path, now_ms, random_byte)
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }
}

/// 纯函数：详情响应（`{"data":{"threaded_conversation_with_injections_v2":…}}`）→ 详情。
///
/// 和 [`FfiXSource::detail`] 的区别只是"响应从哪来"：这里由调用方把 body 递进来，不发请求。
/// 给对拍（`XDetailDualRunTest`）和测量（`XDetailRustBenchTest`）用。
#[uniffi::export]
pub fn x_parse_tweet_detail(body: String, focal_id: String) -> Result<FfiDetail, FfiError> {
    let root = parse_body(&body)?;
    let page = feedhub_core::x::parse_tweet_detail(&root, Some(&focal_id));
    match feedhub_core::x::detail_of(&page) {
        Some(detail) => Ok(to_ffi_detail(&detail)),
        None => Err(FfiError::Parse {
            platform: PLATFORM.to_string(),
            detail: format!("详情里没有推文 {focal_id}（可能已删 / 不可见 / 被广告过滤）"),
        }),
    }
}

/// 纯函数：详情响应 → 一级评论（焦点之后的推文 + `conversationthread-` 模块）。
#[uniffi::export]
pub fn x_parse_tweet_comments(
    body: String,
    focal_id: String,
) -> Result<FfiCommentPage, FfiError> {
    let root = parse_body(&body)?;
    let page = feedhub_core::x::parse_tweet_detail(&root, Some(&focal_id));
    Ok(to_ffi_comment_page(&feedhub_core::detail::CommentPage {
        items: page.replies,
        next: page.next,
    }))
}

fn parse_body(body: &str) -> Result<feedhub_core::json::JsonMap, FfiError> {
    feedhub_core::json::parse_object(body).map_err(|detail| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail,
    })
}

/// `x-client-transaction-id` 的生成器（**纯计算**：给定页面 HTML + sign 模块正文）。
///
/// 构造时要抠输入（找 meta / 4 个动画帧 / sign 模块里的下标），失败会报 `Internal`；
/// 之后每次请求调 [`generate`](Self::generate)——**时机在宿主的拦截器**（限速之后、真要发出去那一刻）。
#[derive(uniffi::Object)]
pub struct FfiXTransaction {
    inner: XTransaction,
}

#[uniffi::export]
impl FfiXTransaction {
    /// [`page_html`] 是 x.com 的页面，[`sign_file_text`] 是加载链第三跳拿到的 sign 模块。
    /// [`random_keyword`] / [`random_number`] 默认是 `obfiowerehiring` / 3（由调用方传，别在这里写死）。
    #[uniffi::constructor]
    pub fn new(
        page_html: String,
        sign_file_text: String,
        random_keyword: String,
        random_number: u8,
    ) -> Result<Arc<Self>, FfiError> {
        let inner =
            XTransaction::from_sources(&page_html, &sign_file_text, &random_keyword, random_number)
                .map_err(|detail| FfiError::Internal {
                    platform: PLATFORM.to_string(),
                    detail,
                })?;
        Ok(Arc::new(Self { inner }))
    }

    /// 生成一次请求用的 id。[`path`] 是 URL 的 path（不含 query），[`method`] 用大写，
    /// [`now_ms`] / [`random_byte`] 由调用方注入（测试才能逐字节比）。
    pub fn generate(
        &self,
        method: String,
        path: String,
        now_ms: i64,
        random_byte: u8,
    ) -> String {
        self.inner.generate(&method, &path, now_ms, random_byte)
    }

    /// 抠出来的 48 字节 key 的十六进制（**只给测试断言用**，不是页面内容的替代）。
    pub fn key_hex(&self) -> String {
        feedhub_core::x::txid::hex_of(self.inner.key_bytes())
    }

    /// 算出来的动画 key（测试断言用）。
    pub fn animation_key(&self) -> String {
        self.inner.animation_key().to_string()
    }
}

/// 批量状态机对拍入口：
/// 维护一份 `XRateLimiter` + `XPageBudget`，顺序执行 JSON 步表并返回每一步的精确结果。
#[uniffi::export]
pub fn x_dual_run_batch(steps_json: String) -> Result<String, FfiError> {
    use feedhub_core::x::profile::{XClientProfile, XFeatures};
    use feedhub_core::x::rate::{XPageBudget, XRateLimiter, XRatePolicy};
    use feedhub_core::x::sign_chain::{resolve_import, script_names_in, txid_switch, XScriptHints};
    use feedhub_core::x::source::risk_signal_in;
    use serde_json::{json, Value};

    let steps: Vec<Value> = serde_json::from_str(&steps_json).map_err(|e| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail: format!("解析对拍 steps 失败: {e}"),
    })?;

    let rate_limiter = XRateLimiter::new();
    let page_budget = XPageBudget::new(XRatePolicy::MAX_PAGES_PER_REFRESH);
    let mut results = Vec::with_capacity(steps.len());

    for step in steps {
        let op = step.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let out = match op {
            "rate_record" => {
                let headers: Vec<(String, String)> = step
                    .get("headers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|pair| {
                                let p = pair.as_array()?;
                                Some((p.first()?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                rate_limiter.record(&headers);
                match rate_limiter.last() {
                    Some(l) => json!({
                        "exhausted": l.exhausted(),
                        "limit": l.limit,
                        "remaining": l.remaining,
                        "reset": l.reset_at_sec,
                    }),
                    None => Value::Null,
                }
            }
            "rate_check" => match rate_limiter.check(feedhub_core::x::X) {
                Ok(()) => json!({"ok": true}),
                Err(XError::RiskControl { code, detail }) => {
                    json!({"ok": false, "code": code, "detail": detail})
                }
                Err(other) => json!({"ok": false, "code": "other", "detail": other.to_string()}),
            },
            "rate_clear" => {
                rate_limiter.clear();
                json!({"cleared": rate_limiter.last().is_none()})
            }
            "budget_allow" => {
                let key = step.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let first = step.get("first").and_then(|v| v.as_bool()).unwrap_or(false);
                let allow = page_budget.allow_next_page(key, first);
                let pages = page_budget.pages_this_refresh(key);
                json!({"allow": allow, "pages": pages})
            }
            "budget_reset" => {
                page_budget.reset();
                json!({"reset": true})
            }
            "risk_signal" => {
                let body = step.get("body").and_then(|v| v.as_str()).unwrap_or("");
                json!(risk_signal_in(body))
            }
            "txid_switch" => {
                let html = step.get("html").and_then(|v| v.as_str()).unwrap_or("");
                json!(txid_switch(html))
            }
            "resolve_import" => {
                let base = step.get("base").and_then(|v| v.as_str()).unwrap_or("");
                let path = step.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match resolve_import(base, path) {
                    Ok(u) => json!({"ok": true, "url": u}),
                    Err(_) => json!({"ok": false}),
                }
            }
            "script_hints" => {
                let text = step.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let names = script_names_in(text);
                let hints = XScriptHints::of(text, &names);
                json!({"names": names, "hints": hints})
            }
            "profile_features" => {
                let json_str = step.get("json").and_then(|v| v.as_str()).unwrap_or("{}");
                let html = step.get("html").and_then(|v| v.as_str()).unwrap_or("");
                let profile = XClientProfile::parse(json_str).map_err(|detail| FfiError::Parse {
                    platform: PLATFORM.to_string(),
                    detail,
                })?;
                let updated = XFeatures::update_from_html(&profile.features, html);
                let matched = XFeatures::matched_flags(html);
                json!({
                    "features": profile.features,
                    "toggles": profile.field_toggles,
                    "detailToggles": profile.tweet_detail_field_toggles,
                    "updated": updated,
                    "matched": matched,
                })
            }
            other => {
                return Err(FfiError::Internal {
                    platform: PLATFORM.to_string(),
                    detail: format!("未知对拍 op: {other}"),
                })
            }
        };
        results.push(out);
    }

    serde_json::to_string(&results).map_err(|e| FfiError::Internal {
        platform: PLATFORM.to_string(),
        detail: format!("序列化对拍结果失败: {e}"),
    })
}
