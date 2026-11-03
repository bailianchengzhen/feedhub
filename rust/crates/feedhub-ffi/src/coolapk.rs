//! 酷安的 UniFFI 导出：推荐流 / 关注流的**纯函数**（解析 + `X-App-Token` 的 v2 / v3）。
//!
//! - 解析：`coolapk_parse_feed(kind, body, http_code, page, firstItem, lastItem, emotes)`，
//!   HTTP 状态码一起传进来（错误映射在 Rust 里做，和 Kotlin `getJson` 的尾巴一一对应）；
//! - 签名：`coolapk_v2_token(deviceCode, ts)` / `coolapk_v3_token(deviceCode, ts, appCode, blob)`，
//!   时间戳由 Kotlin 取（Rust 不碰系统时间），v3 的密码表也由 Kotlin 传（表归插件所有）。
//!
//! 这里这几个纯函数留着给对拍和边界样本当参照（`CoolapkParse` 的 Kotlin 那一侧）。
//!
//! 错误经 [`FfiError`] 带类型回 Kotlin，适配层再映射回 `NotLoggedInException` / `RiskControlException`
//! （风控码带过去）/ `SignatureException` / `ApiException` / `ParseException`。

use feedhub_core::coolapk::{self, CoolapkError, Cursor, FeedKind};
use feedhub_core::model::{FeedItem, UserRef};

// 流的种类和 B站 平台对象共用一份（UniFFI 里同名类型只能定义一次）。
use crate::bili_source::FfiFeedKind;
use crate::play::{to_ffi_play, FfiPlayInfo};
use crate::{
    to_ffi_comment_page, to_ffi_detail, to_ffi_item_node, to_ffi_page, FfiCommentPage, FfiDetail,
    FfiEmote, FfiError, FfiItemNode, FfiPage, FfiUserRef,
};

const PLATFORM: &str = "coolapk";

pub(crate) fn map_error(e: CoolapkError) -> FfiError {
    let platform = PLATFORM.to_string();
    match e {
        CoolapkError::NotLoggedIn { detail } => FfiError::NotLoggedIn { platform, detail },
        CoolapkError::RiskControl { code, detail } => FfiError::RiskControl {
            platform,
            code,
            detail,
        },
        CoolapkError::Signature { detail } => FfiError::Signature { platform, detail },
        CoolapkError::Api { code, detail } => FfiError::Api {
            platform,
            code,
            detail,
        },
        CoolapkError::Parse { detail } => FfiError::Parse { platform, detail },
        // 下面这三个只会从平台对象（`coolapk_source.rs`）的宿主回调回来；纯函数入口碰不到
        CoolapkError::Network { detail } => FfiError::Network { platform, detail },
        CoolapkError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable { platform, detail },
        CoolapkError::Host { detail } => FfiError::Internal { platform, detail },
        CoolapkError::Internal { detail } => FfiError::Internal { platform, detail },
        // 要人机验证：**带类型过去**，宿主让用户本人验证完再重试一次（不循环、不自动绕）
        CoolapkError::Captcha {
            captcha_id,
            field,
            detail,
        } => FfiError::CaptchaRequired {
            platform,
            captcha_id,
            field,
            detail,
        },
    }
}

fn to_pairs(pairs: Vec<FfiEmote>) -> Vec<(String, String)> {
    pairs.into_iter().map(|e| (e.code, e.url)).collect()
}

/// 解析一页信息流（`/v6/main/indexV8` 或 `/v6/page/dataList` 的响应）。
///
/// `http_code` 是 HTTP 状态码，`page` / `first_item` / `last_item` 是**当前**的游标（Kotlin 的 `FeedCursor`）；
/// 返回值里的 `next` 是下一行游标（`page=N&firstItem=..&lastItem=..`），`None` 表示没有下一页。
#[uniffi::export]
pub fn coolapk_parse_feed(
    kind: FfiFeedKind,
    body: String,
    http_code: i32,
    page: i32,
    first_item: Option<String>,
    last_item: Option<String>,
    emotes: Vec<FfiEmote>,
) -> Result<FfiPage, FfiError> {
    let kind = match kind {
        FfiFeedKind::Recommend => FeedKind::Recommend,
        FfiFeedKind::Following => FeedKind::Following,
    };
    let cursor = Cursor {
        page: page as i64,
        first_item,
        last_item,
    };
    let table = to_pairs(emotes);
    coolapk::parse_feed(kind, &body, http_code, &cursor, &table)
        .map(|p| to_ffi_page(&p))
        .map_err(map_error)
}

/// 解析详情（`/v6/feed/detail` 的响应，`{data: {...}}` 外壳）。
///
/// `resolved_media` 是 `b23.tv` 短链跟完重定向后的地址（没有就传 `None`）；`emotes` 是"正文原文 → 图片地址"。
#[uniffi::export]
pub fn coolapk_parse_detail(
    body: String,
    resolved_media: Option<String>,
    emotes: Vec<FfiEmote>,
) -> Result<FfiDetail, FfiError> {
    let table = to_pairs(emotes);
    let json = feedhub_core::json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail,
    })?;
    coolapk::detail::detail(&json, resolved_media.as_deref(), &table)
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_error)
}

/// 解析一页评论 / 楼中楼（`/v6/feed/replyList` 的响应）。
///
/// `page` / `first_item` / `last_item` 是**当前**游标（Kotlin 的 `FeedCursor`），返回值里的 `next` 是
/// 下一页游标。评论里的人机验证**不在这里**：那是请求层（`FfiCoolapkSource.comments`）的事。
#[uniffi::export]
pub fn coolapk_parse_replies(
    body: String,
    page: i32,
    first_item: Option<String>,
    last_item: Option<String>,
    emotes: Vec<FfiEmote>,
) -> Result<FfiCommentPage, FfiError> {
    let table = to_pairs(emotes);
    let cursor = Cursor {
        page: page as i64,
        first_item,
        last_item,
    };
    let json = feedhub_core::json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: PLATFORM.to_string(),
        detail,
    })?;
    coolapk::comment::reply_list(&json, &cursor, &table)
        .map(|page| to_ffi_comment_page(&page))
        .map_err(map_error)
}

/// 解析一份**不带外壳**的 `feed` 实体（测试的边界样本）。
#[uniffi::export]
pub fn coolapk_parse_item(body: String, emotes: Vec<FfiEmote>) -> Result<FfiItemNode, FfiError> {
    let table = to_pairs(emotes);
    let item: Option<FeedItem> = coolapk::parse_item_json(&body, &table).map_err(map_error)?;
    Ok(to_ffi_item_node(item.as_ref()))
}

/// 解析一页搜索结果（`/v6/search` 的响应，`{data:[实体]}` 外壳）。
///
/// `page` 是**当前**页（下一页游标要用它 +1，格式 `page|lastItem`）；`http_code` 一起传进来，
/// 错误映射（含 `err_request_captcha*` → 风控）在 Rust 里做，和 Kotlin `getJson` 的尾巴一字不差。
#[uniffi::export]
pub fn coolapk_parse_search(
    body: String,
    http_code: i32,
    page: i32,
    emotes: Vec<FfiEmote>,
) -> Result<FfiPage, FfiError> {
    coolapk::search::parse_response(&body, http_code, page as i64, &to_pairs(emotes))
        .map(|p| to_ffi_page(&p))
        .map_err(map_error)
}

/// 解析 `checkLoginInfo` 的响应（`whoami` 的纯函数入口）。
///
/// `NotLoggedIn` 折成 `None`（服务端对未登录用户回"登录信息有误"，那是正常结果）；
/// 别的错误照常抛。**Cookie 判断不在这里**——那是平台对象的事（没有 `uid` 时一个请求都不发）。
#[uniffi::export]
pub fn coolapk_parse_account(body: String, http_code: i32) -> Result<Option<FfiUserRef>, FfiError> {
    coolapk::whoami_from(&body, http_code)
        .map(|user| user.as_ref().map(to_ffi_user_ref))
        .map_err(map_error)
}

/// 解析播放（`/v6/feed/detail` 的响应 → 播放通用模型）。
///
/// **不含 Range 探针**（那是请求层的事，在 `FfiCoolapkSource.play`）：这里只管"详情响应 →
/// 托管视频 → `FfiPlayInfo`"。没有托管视频是 `no_video`（文案照 Kotlin）。
#[uniffi::export]
pub fn coolapk_parse_play(body: String, http_code: i32) -> Result<FfiPlayInfo, FfiError> {
    coolapk::video::play_from_detail(&body, http_code)
        .map(|(_, info)| to_ffi_play(&info))
        .map_err(map_error)
}

/// `UserRef` → `FfiUserRef`（`lib.rs` 里那份是私有的，这里留一份给本平台的 `whoami` 用）。
fn to_ffi_user_ref(user: &UserRef) -> FfiUserRef {
    FfiUserRef {
        platform: user.platform.clone(),
        id: user.id.clone(),
        name: user.name.clone(),
        avatar: user.avatar.clone(),
        url: user.url.clone(),
    }
}

/// v2 token（信息流 / 关注流 / 详情的身份，13.4.1 / API 13）。
#[uniffi::export]
pub fn coolapk_v2_token(device_code: String, timestamp_sec: i64) -> Result<String, FfiError> {
    coolapk::token::v2(&device_code, timestamp_sec).map_err(|detail| FfiError::Internal {
        platform: PLATFORM.to_string(),
        detail,
    })
}

/// v3 token（评论身份，16.2.0 / API 16）。`app_code` 必须和请求头 `X-App-Code` 一致；
/// `blob` 是 `auth_blob.b64` 解码后的 930 字节。
#[uniffi::export]
pub fn coolapk_v3_token(
    device_code: String,
    timestamp_sec: i64,
    app_code: i64,
    blob: Vec<u8>,
) -> Result<String, FfiError> {
    coolapk::token::v3(&device_code, timestamp_sec, app_code, &blob).map_err(|detail| {
        FfiError::Internal {
            platform: PLATFORM.to_string(),
            detail,
        }
    })
}

/// 小写十六进制 MD5（固定值测试用；`AppToken.md5` 的对照）。
#[uniffi::export]
pub fn coolapk_md5_hex(text: String) -> String {
    coolapk::token::md5_hex(&text)
}

/// 标准 base64、去掉 `=`（`AppToken.base64` 的对照）。
#[uniffi::export]
pub fn coolapk_base64(text: String) -> String {
    coolapk::token::base64(text.as_bytes(), false)
}
