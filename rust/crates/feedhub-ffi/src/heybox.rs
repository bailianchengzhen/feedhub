//! 小黑盒的 UniFFI 边界（`feedhub-core` 的 `heybox` 模块）。
//!
//! 和 `lib.rs` 里的 B站 导出一样：**全是纯函数**，一次调用换一整页；表情表这种"外部数据"
//! 由 Kotlin 当参数传进来（`Vec<FfiEmote>`，顺序即 Kotlin `LinkedHashMap` 的顺序），Rust 不存状态。
//!
//! 共用的 `FfiItem` / `FfiPage` / `FfiError` / `FfiParam` / `FfiEmote` 全部复用 `lib.rs` 里现成的，
//! 这个文件**没有改任何共用类型的字段**。heybox 自己的错误类型在 `feedhub-core` 里单独一份
//! （`heybox::HeyboxError`），这里映射成同一套 `FfiError`，Kotlin 侧看到的异常和 B站 一致。

use crate::{to_ffi_comment_page, to_ffi_detail, to_ffi_page, FfiCommentPage, FfiDetail, FfiEmote,
    FfiError, FfiPage, FfiParam};
use feedhub_core::heybox::detail::BodyEnv;
use feedhub_core::heybox::{self, HeyboxError};

pub(crate) fn map_heybox_error(e: HeyboxError) -> FfiError {
    let platform = heybox::HEYBOX;
    match e {
        HeyboxError::NotLoggedIn { detail } => FfiError::NotLoggedIn {
            platform: platform.to_string(),
            detail,
        },
        HeyboxError::RiskControl { code, detail } => FfiError::RiskControl {
            platform: platform.to_string(),
            code,
            detail,
        },
        HeyboxError::Signature { detail } => FfiError::Signature {
            platform: platform.to_string(),
            detail,
        },
        HeyboxError::Api { code, detail } => FfiError::Api {
            platform: platform.to_string(),
            code,
            detail,
        },
        HeyboxError::Parse { detail } => FfiError::Parse {
            platform: platform.to_string(),
            detail,
        },
        // 下面这三个只会从平台对象（`heybox_source.rs`）的宿主回调回来；纯函数入口碰不到
        HeyboxError::Network { detail } => FfiError::Network {
            platform: platform.to_string(),
            detail,
        },
        HeyboxError::ProxyUnavailable { detail } => FfiError::ProxyUnavailable {
            platform: platform.to_string(),
            detail,
        },
        HeyboxError::Host { detail } => FfiError::Internal {
            platform: platform.to_string(),
            detail,
        },
    }
}

/// 解析小黑盒推荐流（`/bbs/app/feeds` 的响应）。
///
/// [`offset`] 是这一页请求时的 offset；下一页的游标是 `offset + 本页原始条数`
/// （被跳过的广告 / 空卡片也占位），一条都没有时 `next = None`。
/// [`emotes`] 是表情表（`/bbs/app/api/emojis/list` 整理出来的 `cube_喜欢 → 图片地址`），
/// 空的就表示这次没有表情表（卡片上的表情降级成文字）。
#[uniffi::export]
pub fn heybox_parse_feeds(
    body: String,
    offset: i32,
    emotes: Vec<FfiEmote>,
) -> Result<FfiPage, FfiError> {
    let table: Vec<(String, String)> = emotes.into_iter().map(|e| (e.code, e.url)).collect();
    heybox::parse_feeds(&body, offset, &table)
        .map(|page| to_ffi_page(&page))
        .map_err(map_heybox_error)
}

/// 小黑盒的路径签名（`hkey`）。纯函数：时间戳和 nonce 都由 Kotlin 给（nonce 的随机生成留在 Kotlin）。
#[uniffi::export]
pub fn heybox_hkey(path: String, timestamp: i64, nonce: String) -> String {
    heybox::sign::hkey(&path, timestamp, &nonce)
}

/// `"/bbs//app/feeds"` → `"/bbs/app/feeds/"`（签名用的形式）。
#[uniffi::export]
pub fn heybox_normalize_path(path: String) -> String {
    heybox::sign::normalize_path(&path)
}

/// 要追加到 query 里的三个签名参数（`hkey` / `_time` / `nonce`），**保持这个顺序**
/// ——调用方按顺序拼 query，请求字节才不会变。
#[uniffi::export]
pub fn heybox_signed_params(path: String, timestamp: i64, nonce: String) -> Vec<FfiParam> {
    heybox::sign::signed_params(&path, timestamp, &nonce)
        .into_iter()
        .map(|(name, value)| FfiParam { name, value })
        .collect()
}

// ===========================================================================
// 详情 / 评论 / 楼中楼
// ===========================================================================
//
// 统一用 `lib.rs` 里
// `FfiDetail` / `FfiDetailBody` / `FfiComment` / `FfiCommentPage`（B站 样板那一套，11 家共用）。
// 评论树还是"节点数组 + 下标"（`FfiCommentPage.nodes` + `roots` + `preview_replies`），
// 因为 UniFFI 0.29 不支持递归 record。
//
// 正文里的 HTML 也在 Rust 解析了（`crate::html`），所以 `body` 不再是 `Option`。

/// `/game/get_game_infos/` 的一张游戏卡片（Kotlin `GameInfo`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHeyboxGameInfo {
    pub id: String,
    pub name: String,
    pub cover: Option<String>,
    pub subtitle: Option<String>,
    pub url: Option<String>,
}

fn env(emotes: Vec<FfiEmote>, games: Vec<FfiHeyboxGameInfo>) -> BodyEnv {
    BodyEnv {
        emotes: emotes.into_iter().map(|e| (e.code, e.url)).collect(),
        games: games
            .into_iter()
            .map(|g| feedhub_core::heybox::detail::GameInfo {
                id: g.id,
                name: g.name,
                cover: g.cover,
                subtitle: g.subtitle,
                url: g.url,
            })
            .collect(),
    }
}

/// 解析帖子详情（`/bbs/app/link/tree` 的响应）。
///
/// `emotes` 是表情表，`games` 是 `/game/get_game_infos/` 已经取到的卡片（没有就只显示 id）。
/// 正文（含 HTML 文章 / 资讯）整份由 Rust 解析，包括游戏卡片、图片说明、行内视频。
#[uniffi::export]
pub fn heybox_parse_detail(
    body: String,
    emotes: Vec<FfiEmote>,
    games: Vec<FfiHeyboxGameInfo>,
) -> Result<FfiDetail, FfiError> {
    heybox::detail::parse_detail(&body, &env(emotes, games))
        .map(|detail| to_ffi_detail(&detail))
        .map_err(map_heybox_error)
}

/// 解析一级评论（`page` 是这一页的页码，游标就是下一页的页码）。
#[uniffi::export]
pub fn heybox_parse_comments(
    body: String,
    page: i32,
    emotes: Vec<FfiEmote>,
) -> Result<FfiCommentPage, FfiError> {
    let table: Vec<(String, String)> = emotes.into_iter().map(|e| (e.code, e.url)).collect();
    heybox::comment::parse_comments(&body, page, &table)
        .map(|page| to_ffi_comment_page(&page))
        .map_err(map_heybox_error)
}

/// 解析楼中楼（`from` 是上一页的游标，用来判定"游标没推进"）。
#[uniffi::export]
pub fn heybox_parse_replies(
    body: String,
    root_id: String,
    emotes: Vec<FfiEmote>,
    from: Option<String>,
) -> Result<FfiCommentPage, FfiError> {
    let table: Vec<(String, String)> = emotes.into_iter().map(|e| (e.code, e.url)).collect();
    heybox::comment::parse_replies(&body, &root_id, &table, from.as_deref())
        .map(|page| to_ffi_comment_page(&page))
        .map_err(map_heybox_error)
}
