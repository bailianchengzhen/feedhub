//! 知乎的 UniFFI 边界：推荐流 / 关注动态的解析、`x-zse-96` 签名，**全是纯函数**。
//!
//! 形状和 `lib.rs` 里的 B站 那批一致：一次调用换一整页 / 一条结果；HTTP 状态码和 `Location`
//! 由 Kotlin 传进来（知乎的错误判断要看它们），Rust 不存状态、不发请求。
//!
//! 表情表也由 Kotlin 传（`ZhihuSticker.urls`，占位符 → 随插件带的资源地址）：整张表的所有权在插件里，
//! Rust 不复制"表情包名 → 资源路径"这套命名。
//!
//! 共用类型（`FfiItem` / `FfiPage` / `FfiError` / `FfiEmote`）直接用 `lib.rs` 里的，**没有加字段**。

use crate::{
    map_error, to_ffi_comment_page, to_ffi_detail, to_ffi_item_node, to_ffi_page, FfiCommentPage, FfiDetail,
    FfiEmote, FfiError, FfiItemNode, FfiPage,
};
use feedhub_core::{json, zhihu};

fn table(emotes: Vec<FfiEmote>) -> Vec<(String, String)> {
    emotes.into_iter().map(|e| (e.code, e.url)).collect()
}

pub(crate) fn seed_byte(seed: i32) -> Result<u8, FfiError> {
    if (0..=255).contains(&seed) {
        Ok(seed as u8)
    } else {
        Err(FfiError::Internal {
            platform: "zhihu".to_string(),
            detail: format!("seed 超出一个字节：{seed}"),
        })
    }
}

/// 解析推荐流（`/api/v3/feed/topstory/recommend` 的响应）。
///
/// `http_code` / `location` 是这次响应的状态码和 `Location` 头：知乎的错误体没有统一外壳，
/// 判断风控 / 未登录要靠它们（和 Kotlin `ZhihuParser.error` 一一对应）。
#[uniffi::export]
pub fn zhihu_parse_recommend(
    body: String,
    http_code: i32,
    location: Option<String>,
    emotes: Vec<FfiEmote>,
) -> Result<FfiPage, FfiError> {
    zhihu::parse_recommend(&body, http_code, location.as_deref(), &table(emotes))
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("zhihu", e))
}

/// 解析关注动态（`/api/v3/moments` 的响应）。
#[uniffi::export]
pub fn zhihu_parse_moments(
    body: String,
    http_code: i32,
    location: Option<String>,
    emotes: Vec<FfiEmote>,
) -> Result<FfiPage, FfiError> {
    zhihu::parse_moments(&body, http_code, location.as_deref(), &table(emotes))
        .map(|page| to_ffi_page(&page))
        .map_err(|e| map_error("zhihu", e))
}

/// 解析一份**不带外壳**的 `target` 对象。
#[uniffi::export]
pub fn zhihu_parse_target(body: String, emotes: Vec<FfiEmote>) -> Result<FfiItemNode, FfiError> {
    zhihu::parse_target_object(&body, &table(emotes))
        .map(|item| to_ffi_item_node(item.as_ref()))
        .map_err(|e| map_error("zhihu", e))
}

/// 完整的 `x-zse-96` 请求头值（`2.0_` + 64 个字符）。
/// `seed` 是明文首字节，由 Kotlin 生成（网页上是 `Math.random() * 127`）——测试里注入固定值才能逐字节比。
#[uniffi::export]
pub fn zhihu_zse96(
    path_and_query: String,
    dc0: String,
    body: Option<String>,
    seed: i32,
) -> Result<String, FfiError> {
    Ok(zhihu::sign::zse96(
        &path_and_query,
        &dc0,
        body.as_deref(),
        seed_byte(seed)?,
    ))
}

/// 不带 `2.0_` 前缀的密文（入参是 32 位 md5 hex）。只给固定值测试用，生产签名走 [`zhihu_zse96`]。
#[uniffi::export]
pub fn zhihu_encrypt(input: String, seed: i32) -> Result<String, FfiError> {
    Ok(zhihu::sign::encrypt(&input, seed_byte(seed)?))
}

/// 小写十六进制 MD5（测试里比对签名原文用）。
#[uniffi::export]
pub fn zhihu_md5_hex(text: String) -> String {
    zhihu::sign::md5_hex(&text)
}

// ===========================================================================
// 详情 / 评论 / 楼中楼
// ===========================================================================

/// 解析详情（`/api/v4/{answers,articles,pins,zvideos,questions}/{id}` 的响应）。
///
/// - `kind` 是 `answer` / `article` / `pin` / `zvideo` / `question`（和 Kotlin 的 `ZhihuLinks.TYPES` 一致）；
/// - `emotes` 是**正文用**的表情表（`/api/v4/sticker-groups/…` 拿到的 data URI，只有想法正文会用到）；
/// - `bundled` 是**条目用**的表情表（`ZhihuSticker.urls`，`FeedItem.emotes` 用，和推荐流同一份）。
///
/// 两个表分开是因为 Kotlin 那边就是两个来源：`ZhihuDetail.pin(json, emotes)` 用取回来的表，
/// 而 `ZhihuParser.target` 里的 `Emotes.used(ZhihuSticker.urls, …)` 用随插件带的表。
#[uniffi::export]
pub fn zhihu_parse_detail(
    kind: String,
    body: String,
    emotes: Vec<FfiEmote>,
    bundled: Vec<FfiEmote>,
) -> Result<FfiDetail, FfiError> {
    let json = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "zhihu".to_string(),
        detail,
    })?;
    zhihu::detail::detail(&kind, &json, &table(emotes), &table(bundled))
        .map(|detail| to_ffi_detail(&detail))
        .map_err(|detail| FfiError::Parse {
            platform: "zhihu".to_string(),
            detail,
        })
}

/// 解析评论列表（`comment_v5/…/root_comment` 或 `comment_v5/comment/{id}/child_comment` 的响应）。
/// `emotes` 是正文用的表情表；不认识的表情代码原样留文字。
#[uniffi::export]
pub fn zhihu_parse_comments(body: String, emotes: Vec<FfiEmote>) -> Result<FfiCommentPage, FfiError> {
    let json = json::parse_object(&body).map_err(|detail| FfiError::Parse {
        platform: "zhihu".to_string(),
        detail,
    })?;
    zhihu::comment::comments(&json, &table(emotes))
        .map(|page| to_ffi_comment_page(&page))
        .map_err(|detail| FfiError::Parse {
            platform: "zhihu".to_string(),
            detail,
        })
}
