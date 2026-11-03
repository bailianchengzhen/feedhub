//! 抖音：推荐流（`/aweme/v1/web/module/feed/`）的**纯解析**，以及 a_bogus / `x-secsdk-web-signature` 签名。
//!
//! 对应 `source-douyin` 的 [`DouyinParser`] 里**推荐流用得到的那部分**、[`AbogusSigner`]（含 [`Sm3`]）
//! 与 [`SecSdkSigner`]，逐条照 Kotlin 的语义移植（`DouyinDualRunTest` / `DouyinSignDualRunTest`
//! 用同一份输入两边跑、逐字段 / 逐字节比）。
//!
//! 推荐流的**整条请求**也进了 Rust：见 [`source::DouyinHostedSource`]
//! （拼 URL / 参数顺序 / 请求头顺序 / 翻页游标 / a_bogus / 状态码与错误码判定）。它通过
//! `crate::host` 的三个端口把真正的收发交回宿主，所以本模块里**除了 `source` 之外**仍然是这样：
//!
//! 边界（和 B站 / 知乎 / 小红书一样）：**不发请求、不碰 Cookie、不落盘、不取时间、不取随机数**。
//! `abogus_profile.json` 的常量、浏览器环境、`now` / `enterPageTs`、随机数流全都由 Kotlin 当参数传进来。
//!
//! 请求在 [`source::DouyinHostedSource`] 的 `detail` / `comments` / `replies`
//! （`aweme/detail` 是 secsdk 保护路径、楼中楼的 `verifyFp` / `fp` 在 `a_bogus` 之后不参与签名）。
//! **没搬**：播放地址、表情表（表本身归 Kotlin，Rust 只按表切 Span）、扫码登录、`whoami`。
//! 它们继续在 Kotlin（`RustDouyinSource` 逐方法委托）。
//!
//! [`DouyinParser`]: ../../../source-douyin/src/main/kotlin/feedhub/source/douyin/DouyinParser.kt
//! [`AbogusSigner`]: ../../../source-douyin/src/main/kotlin/feedhub/source/douyin/AbogusSigner.kt
//! [`Sm3`]: ../../../source-douyin/src/main/kotlin/feedhub/source/douyin/Sm3.kt
//! [`SecSdkSigner`]: ../../../source-douyin/src/main/kotlin/feedhub/source/douyin/SecSdkSigner.kt

pub mod comment;
pub mod detail;
// 设备环境 + 本地设备身份：生成规则和持久化在 Rust，
// 随机量 / 时钟由宿主注入；`ctx.store` 里已有的值原样沿用（风控红线）。
pub mod device;
// 表情表：
// 抓取 / 解析 / 一周缓存 / 失败节流都在这里，表常驻平台对象，不再每趟过 FFI。
pub mod emotes;
pub mod qr;
pub mod sha256;
// 播放地址（`DouyinVideo.kt` 的移植）：详情里的 `FeedDetail.video` 也走它，别和 `play` 各写一份。
pub mod play;
pub mod sign;
pub mod sm3;
pub mod source;
pub mod text;

use crate::json;
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

use json::JsonMap;

/// 平台 id（和 Kotlin `Platform.define("douyin", "抖音")` 一样，定了就别改）。
pub const DOUYIN: &str = "douyin";

/// Rust 侧的业务错误。FFI 会把它拆成带类型的 [`FfiError`]（见 `feedhub-ffi` 的 `douyin.rs`）。
///
/// 和 Kotlin `DouyinSource.checkTransport` / `check` 抛的那几种一一对应：
/// `RiskControlException`（风控码带过去）/ `SignatureException` / `ApiException` /
/// `NotLoggedInException` / `ParseException`。
///
/// [`FfiError`]: ../../feedhub_ffi/enum.FfiError.html
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DouyinError {
    #[error("未登录或登录已失效")]
    NotLoggedIn { detail: Option<String> },

    /// 风控：验证码、请求被拦截、账号异常。**调用方不许自动重试**。
    #[error("风控：{detail}")]
    RiskControl { code: Option<String>, detail: String },

    #[error("签名被拒：{detail}")]
    Signature { detail: String },

    #[error("接口错误 {code}：{detail}")]
    Api { code: String, detail: String },

    #[error("解析失败：{detail}")]
    Parse { detail: String },

    /// 网络错误（连不上 / 超时 / 连接被重置）。调用方映射回 `java.io.IOException`（现有约定：网络错误不包装）。
    ///
    /// 只有平台对象（[`source::DouyinHostedSource`] 的宿主回调）会产生这个变体；
    /// 上一期那几个纯函数入口碰不到它。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    /// 调用方映射成 `ProxyUnavailableException`（cli 的退出码 6），和普通网络错误分开。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题）。别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },

    /// 内部错误（参数对不上 / 随机数不够 / 算法移植的假设被打破）。别让它变成一次崩溃。
    #[error("内部错误：{detail}")]
    Internal { detail: String },
}

/// 一页推荐流 + 这一页所有卡片的 `desc` 拼串。
///
/// 拼串是给调用方判断"要不要去取表情表、取回来挑哪几个用"的（Kotlin 的 `DouyinParser.feedTexts`）：
/// 表情表归插件所有，Rust 不复制表、也不发请求。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedPage {
    pub page: Page,
    pub texts: String,
}

// ===========================================================================
// 错误映射（Kotlin `DouyinSource.checkTransport` / `check` 的移植）
// ===========================================================================

/// HTTP 响应里我们看的两条风控信号（值不打印、也不参与比较，只判有没有）。
///
/// Kotlin 里是 `HttpResult.headers` 里找 `X-Vc-Bdturing-Parameters` / `X-Tt-Verify-Passport-Decision`；
/// 名字只在 Kotlin 侧出现，这里收"有没有"，免得把值带过来。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportSignals {
    /// 有 `X-Vc-Bdturing-Parameters`（要求过人机验证）。
    pub bdturing: bool,
    /// 有 `X-Tt-Verify-Passport-Decision`（要求二次身份验证）。
    pub passport_decision: bool,
}

/// 传输层风控（`checkTransport` 的逐行移植）。识别到就停，不重试。
pub fn check_transport(body: &str, http_code: i32, signals: TransportSignals) -> Result<(), DouyinError> {
    if signals.bdturing {
        return Err(DouyinError::RiskControl {
            code: Some("bdturing".to_string()),
            detail: "服务端要求过人机验证".to_string(),
        });
    }
    if signals.passport_decision {
        return Err(DouyinError::RiskControl {
            code: Some("passport-decision".to_string()),
            detail: "服务端要求二次身份验证".to_string(),
        });
    }
    if http_code == 403 {
        return Err(if body.contains("ArgusSecurityPlugin") {
            DouyinError::Signature {
                detail: "网关 Argus 拒绝（缺 x-tt-argus / uifid）".to_string(),
            }
        } else if body.contains("Sign Invalid") {
            DouyinError::Signature {
                detail: "a_bogus / x-secsdk-web-signature 被拒".to_string(),
            }
        } else if body.contains("Signature Not Found") {
            DouyinError::Signature {
                detail: "网关没收到签名".to_string(),
            }
        } else {
            // 不是签名被拒的 403 当风控（项目红线：403 / 412 / 429 立刻停）
            DouyinError::RiskControl {
                code: Some("HTTP 403".to_string()),
                detail: "请求被拒绝，先别请求了".to_string(),
            }
        });
    }
    if http_code == 412 || http_code == 429 {
        return Err(DouyinError::RiskControl {
            code: Some(format!("HTTP {http_code}")),
            detail: "请求被限制，先别请求了".to_string(),
        });
    }
    if !(200..=299).contains(&http_code) {
        return Err(DouyinError::Api {
            code: format!("HTTP {http_code}"),
            detail: json::take_chars(body, 120),
        });
    }
    // HTTP 200 + 空 body：签名 / 指纹 / WAF 拒绝
    if body.is_empty() {
        return Err(DouyinError::RiskControl {
            code: Some("empty-body".to_string()),
            detail: "服务端返回空 body（签名 / 指纹 / WAF 拒绝）".to_string(),
        });
    }
    // acrawler 挑战页
    if body.contains("__ac_nonce") || body.contains("$jsvmprt") {
        return Err(DouyinError::RiskControl {
            code: Some("acrawler".to_string()),
            detail: "拿到 JS 挑战页，不是接口响应".to_string(),
        });
    }
    Ok(())
}

/// 业务码（`check` 的逐行移植）。
pub fn check_code(json: &JsonMap) -> Result<(), DouyinError> {
    let code = json::long(json, "status_code").ok_or_else(|| DouyinError::Parse {
        detail: "响应缺少 status_code".to_string(),
    })?;
    let msg = json::text(json, "status_msg")
        .or_else(|| json::text(json, "message"))
        .unwrap_or_default();
    match code {
        0 => Ok(()),
        // 8 = 未登录；被拒的写操作也会让整个会话作废，之后读接口马上返回 8
        8 => Err(DouyinError::NotLoggedIn { detail: None }),
        5 => Err(DouyinError::Api {
            code: "5".to_string(),
            detail: format!("这个接口只接受 POST{msg}"),
        }),
        2096 => Err(DouyinError::Api {
            code: "2096".to_string(),
            detail: "对方设置了隐私，列表不可见".to_string(),
        }),
        200010001 => Err(DouyinError::Api {
            code: "200010001".to_string(),
            detail: "rpc failed（多半缺 x-secsdk-csrf-token）".to_string(),
        }),
        _ => Err(DouyinError::Api {
            code: code.to_string(),
            detail: msg,
        }),
    }
}

/// 响应 → JSON 对象（带传输层 / 业务码判定），和 Kotlin `getJson(...checkCode = true)` 的尾巴一致。
pub fn decode_response(
    body: &str,
    http_code: i32,
    signals: TransportSignals,
    check_business_code: bool,
) -> Result<JsonMap, DouyinError> {
    check_transport(body, http_code, signals)?;
    let json = json::parse_object(body).map_err(|detail| DouyinError::Parse { detail })?;
    if check_business_code {
        check_code(&json)?;
    }
    Ok(json)
}

/// **已经解析过一次的**响应体再解析一遍（`play` 复用 [`source::DouyinHostedSource::detail`] 那份缓存）。
///
/// 传输层 / 业务码判定**不重做**：缓存里的那一份在第一次进来时已经过过 [`decode_response`]
/// （风控信号、业务码 8、空 body 全都查过），再判一次只会因为"这次没有响应头"而误伤。
pub fn decode_body(body: &str) -> Result<JsonMap, DouyinError> {
    json::parse_object(body).map_err(|detail| DouyinError::Parse { detail })
}

/// 推荐流一页（`module/feed`）。`http_code` / 风控信号一起进来，错误映射也在这一层做，
/// 这样对拍连错误语义都能一起比。
pub fn parse_feed(
    body: &str,
    http_code: i32,
    signals: TransportSignals,
    refresh_index: i64,
    emotes: &[(String, String)],
) -> Result<FeedPage, DouyinError> {
    let json = decode_response(body, http_code, signals, true)?;
    // 顺序和 Kotlin `recommend` 一样：先算 desc 拼串（可能触发取表情表），再映射条目
    let texts = feed_texts(&json);
    let page = feed(&json, refresh_index, emotes)?;
    Ok(FeedPage { page, texts })
}

/// 一份**不带外壳**的 aweme 对象（详情 fixture 里的 `aweme_detail`，测试 / 边界样本用）。
pub fn parse_item_json(body: &str, emotes: &[(String, String)]) -> Result<Option<FeedItem>, DouyinError> {
    let json = json::parse_object(body).map_err(|detail| DouyinError::Parse { detail })?;
    Ok(item(&json, emotes))
}

// ===========================================================================
// 推荐流（`/aweme/v1/web/module/feed/`）
// ===========================================================================

/// `module/feed` 的一页。
///
/// 游标就是 `refresh_index`（这个接口没有 cursor，靠 `refresh_index` 递增 + `filterGids` /
/// `presented_ids` 携带已看过的 id；翻页行为**没有被验证过**，和 Kotlin 一样保守实现）。
pub fn feed(
    json: &JsonMap,
    refresh_index: i64,
    emotes: &[(String, String)],
) -> Result<Page, DouyinError> {
    let cards = json::arr(json, "cards").ok_or_else(|| DouyinError::Parse {
        detail: "推荐流缺少 cards".to_string(),
    })?;
    let items = cards
        .iter()
        .filter_map(|card| card.as_object())
        .filter_map(aweme_of)
        .filter_map(|a| item(&a, emotes))
        .collect::<Vec<_>>();
    // has_more 缺失时按"还有下一页"处理：推荐流通常没有尽头
    let has_more = json::bool(json, "has_more") != Some(false);
    let next = if has_more {
        Some((refresh_index + 1).to_string())
    } else {
        None
    };
    Ok(Page { items, next })
}

/// 一页推荐流里所有卡片的 `desc`（判断要不要去取表情表、取回来挑哪几个用）。
pub fn feed_texts(json: &JsonMap) -> String {
    json::arr(json, "cards")
        .map(|cards| {
            cards
                .iter()
                .filter_map(|card| card.as_object())
                .filter_map(aweme_of)
                .filter_map(|a| json::text(&a, "desc"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .join("\n")
}

/// 从一张卡片里挑出 aweme 对象：字符串或对象、几种常见键名、以及 card 本身就是 aweme。
///
/// `cards[].aweme` 是一段 **JSON 字符串**（真实录制确认过），二次解析出来的对象没法借用 card，
/// 所以返回自持有的一份（Kotlin 那边是直接拿到解析后的 `JsonObject`）。逐字段语义和 Kotlin `awemeOf` 一样。
pub fn aweme_of(card: &JsonMap) -> Option<JsonMap> {
    for key in AWEME_KEYS {
        let raw = match card.get(*key) {
            Some(v) => v,
            None => continue,
        };
        if let Some(obj) = raw.as_object() {
            return Some(obj.clone());
        }
        let text = match json::as_string(raw) {
            Some(t) => t,
            None => continue,
        };
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text) {
            return Some(map);
        }
    }
    if json::text(card, "aweme_id").is_some() {
        Some(card.clone())
    } else {
        None
    }
}

/// 推荐流里的 `tab/feed` 那种平铺结构（`aweme_list`）。**没人验证过 `tab/feed` 可用**，留着当兜底。
pub fn aweme_list(json: &JsonMap, emotes: &[(String, String)]) -> Vec<FeedItem> {
    json::arr(json, "aweme_list")
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|o| item(o, emotes))
                .collect()
        })
        .unwrap_or_default()
}

// ===========================================================================
// 一条 aweme → FeedItem
// ===========================================================================

/// 一条抖音内容。广告（`is_ads`）和没有 id 的卡片丢掉。
pub fn item(o: &JsonMap, emotes: &[(String, String)]) -> Option<FeedItem> {
    if json::bool(o, "is_ads") == Some(true) {
        return None;
    }
    let id = json::text(o, "aweme_id")?;
    let desc = json::text(o, "desc");

    let images: Vec<Media> = json::arr(o, "images")
        .map(|list| list.iter().filter_map(|v| v.as_object()).filter_map(image).collect())
        .unwrap_or_default();
    let video = json::obj(o, "video").and_then(video_media);
    let is_video = video.is_some() && images.is_empty();

    let url = if is_video { video_url(&id) } else { note_url(&id) };
    let stats = json::obj(o, "statistics");

    let kind = if is_video {
        ItemKind::Video
    } else if !images.is_empty() {
        ItemKind::Post
    } else if json::int(o, "aweme_type") == Some(0) {
        // aweme_type：0 是视频、68 是图集（其余取值未确认），所以以媒体为准、类型号兜底
        ItemKind::Video
    } else {
        ItemKind::Post
    };

    let mut media = Vec::with_capacity(images.len() + 1);
    if let Some(v) = video {
        media.push(v);
    }
    media.extend(images);

    let stat = |key: &str| stats.and_then(|s| json::long(s, key));

    Some(FeedItem {
        platform: DOUYIN.to_string(),
        id: id.clone(),
        kind,
        // 抖音没有标题字段，`desc` 是文案，所以放 summary，title 留 None（不编字段）
        title: None,
        summary: desc.clone(),
        language: None,
        author: json::obj(o, "author").and_then(author),
        published_at: epoch_seconds(json::long(o, "create_time")),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            views: stat("play_count").filter(|v| *v > 0),
            likes: stat("digg_count"),
            comments: stat("comment_count"),
            shares: stat("share_count"),
            danmaku: None,
            favorites: stat("collect_count"),
            coins: None,
        },
        reason: None,
        // 抖音的"圈子"就是话题（`text_extra` / `cha_list`），没有就 None
        community: first_hashtag(o),
        community_icon: None,
        repost: None,
        emotes: emotes_used(emotes, desc.as_deref()),
        adult: false,
        url: Some(url),
        // 详情 / 评论 / 播放都做了，所以在 App 里原生打开；id 就是 aweme_id
        detail: DetailTarget::Native {
            id,
            platform: None,
        },
    })
}

/// 作品的网页地址（`DouyinParser.videoUrl`）。
pub fn video_url(aweme_id: &str) -> String {
    format!("https://www.douyin.com/video/{aweme_id}")
}

/// 图文的网页地址（`DouyinParser.noteUrl`）。
pub fn note_url(aweme_id: &str) -> String {
    format!("https://www.douyin.com/note/{aweme_id}")
}

/// 用户主页地址（`DouyinParser.userUrl`）。
pub fn user_url(sec_uid: &str) -> String {
    format!("https://www.douyin.com/user/{sec_uid}")
}

fn image(o: &JsonMap) -> Option<Media> {
    let url = first_primitive(o, "url_list")?;
    Some(Media::Image {
        url: json::to_https(&url),
        width: json::int(o, "width"),
        height: json::int(o, "height"),
    })
}

fn video_media(v: &JsonMap) -> Option<Media> {
    let cover = json::obj(v, "cover")
        .and_then(url_list)
        .or_else(|| json::obj(v, "origin_cover").and_then(url_list));
    let duration_ms = json::long(v, "duration");
    let has_play_addr = json::obj(v, "play_addr")
        .and_then(|p| json::arr(p, "url_list"))
        .is_some_and(|l| !l.is_empty());
    if cover.is_none() && duration_ms.is_none() && !has_play_addr {
        return None;
    }
    Some(Media::Video {
        cover: cover.map(|c| json::to_https(&c)),
        duration_sec: duration_ms.filter(|d| *d > 0).map(|d| (d / 1000) as i32),
        width: json::int(v, "width"),
        height: json::int(v, "height"),
    })
}

fn url_list(o: &JsonMap) -> Option<String> {
    first_primitive(o, "url_list")
}

/// `url_list` 里第一个 JSON 原始值（字符串 / 数字 / 布尔）取文本。
///
/// **照抄 Kotlin 的一个怪癖**：Kotlin 是 `(it as? JsonPrimitive)?.content`，而 `JsonNull` 也是
/// `JsonPrimitive`，`content` 是字符串 `"null"`——所以真遇到 `url_list: [null, ...]` 时 Kotlin 会
/// 取到 `"null"` 这个地址。这里同样取 `"null"`（而不是跳过），为的是两边**任何输入都逐字段相等**；
/// 线上样本里 `url_list` 没有 null（真出现的话两边都是同一个坏地址，不是这次迁移引入的）。
fn first_primitive(o: &JsonMap, key: &str) -> Option<String> {
    json::arr(o, key)?.iter().find_map(|v| {
        if v.is_null() {
            Some("null".to_string())
        } else {
            json::as_string(v)
        }
    })
}

fn author(a: &JsonMap) -> Option<UserRef> {
    let sec_uid = json::text(a, "sec_uid");
    let uid = json::text(a, "uid");
    let id = sec_uid.clone().or(uid)?;
    Some(UserRef {
        platform: DOUYIN.to_string(),
        name: json::text(a, "nickname").unwrap_or_else(|| id.clone()),
        id,
        avatar: json::obj(a, "avatar_thumb")
            .and_then(url_list)
            .or_else(|| json::obj(a, "avatar_larger").and_then(url_list))
            .map(|u| json::to_https(&u)),
        url: sec_uid.map(|s| user_url(&s)),
    })
}

fn first_hashtag(o: &JsonMap) -> Option<String> {
    if let Some(list) = json::arr(o, "text_extra") {
        for e in list.iter().filter_map(|v| v.as_object()) {
            if let Some(name) = json::text(e, "hashtag_name") {
                return Some(name);
            }
        }
    }
    if let Some(list) = json::arr(o, "cha_list") {
        for e in list.iter().filter_map(|v| v.as_object()) {
            if let Some(name) = json::text(e, "cha_name") {
                return Some(name);
            }
        }
    }
    None
}

/// `create_time` 按秒；只有数值大到像毫秒（> 10^12）才除 1000。
fn epoch_seconds(value: Option<i64>) -> Option<i64> {
    let v = value.filter(|v| *v > 0)?;
    Some(if v > 1_000_000_000_000 { v / 1000 } else { v })
}

/// `Emotes.used`：按**表的顺序**挑出真的出现在文本里的代码（空代码 / 空地址丢掉）。
pub fn emotes_used(table: &[(String, String)], text: Option<&str>) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let present: Vec<&str> = text.into_iter().filter(|t| !t.is_empty()).collect();
    if present.is_empty() {
        return Vec::new();
    }
    table
        .iter()
        .filter(|(code, url)| {
            !code.is_empty() && !url.is_empty() && present.iter().any(|t| t.contains(code.as_str()))
        })
        .cloned()
        .collect()
}

/// 已登录账号（`/aweme/v1/web/user/profile/self/`）。`whoami` 当前没搬，这里只为对拍留着。
pub fn self_user(json: &JsonMap) -> Option<UserRef> {
    let user = json::obj(json, "user")?;
    let sec_uid = json::text(user, "sec_uid");
    let uid = json::text(user, "uid");
    let id = sec_uid.clone().or(uid)?;
    Some(UserRef {
        platform: DOUYIN.to_string(),
        name: json::text(user, "nickname").unwrap_or_else(|| id.clone()),
        id,
        avatar: json::obj(user, "avatar_thumb")
            .and_then(url_list)
            .map(|u| json::to_https(&u)),
        url: sec_uid.map(|s| user_url(&s)),
    })
}

/// `module/feed` 的 card 里 aweme 可能挂的键名。
const AWEME_KEYS: &[&str] = &["aweme", "aweme_info", "aweme_detail", "awemeInfo", "item"];
