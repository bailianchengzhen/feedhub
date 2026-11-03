//! 贴吧的 UniFFI 边界：**全是纯函数**（不建 HTTP 客户端、不存状态、不碰 Cookie / store）。
//!
//! 和 B站 那一份（`lib.rs`）的分工一样：一次调用换一整页；错误带类型过去。
//! 唯一的不同是**平台错误（`Error.error_code` 非 0）不在这里归类**——贴吧的归类规则
//! （哪些算未登录、哪些算风控、110001 是签名不对）在 Kotlin 的 `TiebaSource.mapError`，
//! 那边还有搜索 / 关注管理等别的调用方。所以这里把 `error_code` / `user_msg` / `error_msg`
//! 原样带回去（`FfiTiebaFeed` 的字段），由 Kotlin 那边按同一份规则翻成异常。

use feedhub_core::detail::{CommentPage, FeedDetail};
use feedhub_core::tieba;
use feedhub_core::tieba::detail::Parsed;

use crate::{
    map_error, to_ffi_comment_page, to_ffi_detail, to_ffi_page, to_ffi_play, FfiCommentPage,
    FfiDetail, FfiError, FfiPage, FfiParam, FfiPlayInfo,
};

/// 贴吧平台 id（和 `TiebaPlugin` 里的一致）。
const PLATFORM: &str = tieba::TIEBA;

/// 一页 + 平台错误码。
///
/// `error_code` 非 0 时 `page` 是空的（和 Kotlin 一样：先看错误码，再碰数据）。
/// `user_msg` / `error_msg` 两个都带回来，是因为 Kotlin 那边取的是"`user_msg` 空白就用 `error_msg`"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaFeed {
    pub page: FfiPage,
    /// 吧内帖子列表：这是不是这个吧的最后一页（推荐流恒为 false）。
    pub last: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 关注的一个吧（`forumGuide` 的 `like_forum[]`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaForum {
    pub id: i64,
    pub name: String,
    pub day_threads: i32,
    pub avatar: Option<String>,
    pub member_count: Option<i64>,
    pub thread_count: Option<i64>,
}

fn to_ffi_feed(feed: tieba::Feed) -> FfiTiebaFeed {
    let page = feedhub_core::model::Page {
        items: feed.items,
        next: feed.next,
    };
    let (error_code, user_msg, error_msg) = match feed.error {
        Some(e) => (e.code, e.user_msg, e.error_msg),
        None => (0, String::new(), String::new()),
    };
    FfiTiebaFeed {
        page: to_ffi_page(&page),
        last: feed.last,
        error_code,
        user_msg,
        error_msg,
    }
}

/// 推荐流（`/c/f/excellent/personalized` 的 protobuf 响应）。`pn` 是这一页的页码，下一页永远是 `pn + 1`。
#[uniffi::export]
pub fn tieba_parse_personalized(bytes: Vec<u8>, pn: i32) -> Result<FfiTiebaFeed, FfiError> {
    tieba::parse_personalized(&bytes, pn)
        .map(to_ffi_feed)
        .map_err(|e| map_error(PLATFORM, e))
}

/// 吧内帖子列表（`/c/f/frs/page` 的 protobuf 响应）→ 关注流里某个吧的一页。
#[uniffi::export]
pub fn tieba_parse_frs_page(bytes: Vec<u8>, forum_name: String) -> Result<FfiTiebaFeed, FfiError> {
    tieba::parse_frs_page(&bytes, &forum_name)
        .map(to_ffi_feed)
        .map_err(|e| map_error(PLATFORM, e))
}

/// 关注的吧（`POST /c/f/forum/forumGuide` 的 JSON 响应）。
#[uniffi::export]
pub fn tieba_followed_forums(body: String) -> Result<Vec<FfiTiebaForum>, FfiError> {
    tieba::followed_forums(&body)
        .map(|forums| {
            forums
                .into_iter()
                .map(|f| FfiTiebaForum {
                    id: f.id,
                    name: f.name,
                    day_threads: f.day_threads,
                    avatar: f.avatar,
                    member_count: f.member_count,
                    thread_count: f.thread_count,
                })
                .collect()
        })
        .map_err(|e| map_error(PLATFORM, e))
}

/// App 签名：`md5(排序后的 "k=v" 拼接 + salt)`，小写十六进制。
#[uniffi::export]
pub fn tieba_sign(params: Vec<FfiParam>, salt: String) -> String {
    let input: Vec<(String, String)> = params
        .into_iter()
        .map(|p| (p.name, p.value))
        .collect();
    tieba::sign::compute(&input, &salt)
}

/// 去掉 `sign`、排好序、末尾补上 `sign`：返回**保持顺序**的参数表（调用方按顺序拼请求，请求字节才不变）。
#[uniffi::export]
pub fn tieba_signed(params: Vec<FfiParam>, salt: String) -> Vec<FfiParam> {
    let input: Vec<(String, String)> = params
        .into_iter()
        .map(|p| (p.name, p.value))
        .collect();
    tieba::sign::signed(&input, &salt)
        .into_iter()
        .map(|(name, value)| FfiParam { name, value })
        .collect()
}

// ===========================================================================
// 详情 / 楼层 / 楼中楼
// ===========================================================================

/// 详情 + 平台错误。`error` 为 true 时 `detail` 是空的（和 [`FfiTiebaFeed`] 同一个约定：
/// **平台错误码的归类只在 Kotlin**，这里原样带回去）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaParsedDetail {
    pub detail: Option<FfiDetail>,
    pub error: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 一页评论（一级评论 / 楼中楼）+ 平台错误。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaParsedComments {
    pub page: Option<FfiCommentPage>,
    pub error: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 一楼的主视频（`play(partId=main)` 的地址）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaMainVideo {
    pub url: String,
    pub cover: Option<String>,
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 主视频 + 平台错误。`error` 为 false 而 `video` 是空的时候表示"这条帖子一楼没有视频"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaParsedMainVideo {
    pub video: Option<FfiTiebaMainVideo>,
    pub error: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 平台错误 → （有没有错、错误码、两个文案）。文案怎么取（`user_msg` 空白就用 `error_msg`）归 Kotlin。
fn error_flags(error: Option<tieba::PlatformError>) -> (bool, i32, String, String) {
    match error {
        Some(e) => (true, e.code, e.user_msg, e.error_msg),
        None => (false, 0, String::new(), String::new()),
    }
}

pub(crate) fn to_ffi_parsed_detail(parsed: Parsed<FeedDetail>) -> FfiTiebaParsedDetail {
    let (error, error_code, user_msg, error_msg) = error_flags(parsed.error);
    FfiTiebaParsedDetail {
        detail: parsed.value.as_ref().map(to_ffi_detail),
        error,
        error_code,
        user_msg,
        error_msg,
    }
}

pub(crate) fn to_ffi_parsed_comments(parsed: Parsed<CommentPage>) -> FfiTiebaParsedComments {
    let (error, error_code, user_msg, error_msg) = error_flags(parsed.error);
    FfiTiebaParsedComments {
        page: parsed.value.as_ref().map(to_ffi_comment_page),
        error,
        error_code,
        user_msg,
        error_msg,
    }
}

/// `play` 的结果：**要么**有播放信息，**要么**是平台错误；两者都没有 = 帖子里没有那个视频
/// （`main` 时是一楼没视频），Kotlin 那边翻成 `ParseException`。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaParsedPlay {
    pub info: Option<FfiPlayInfo>,
    pub error: bool,
    pub error_code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

pub(crate) fn to_ffi_parsed_play(
    outcome: feedhub_core::tieba::detail::PlayOutcome,
) -> FfiTiebaParsedPlay {
    let (error, error_code, user_msg, error_msg) = error_flags(outcome.error);
    FfiTiebaParsedPlay {
        info: outcome.info.as_ref().map(to_ffi_play),
        error,
        error_code,
        user_msg,
        error_msg,
    }
}

pub(crate) fn to_ffi_parsed_main_video(
    outcome: feedhub_core::tieba::detail::MainVideoOutcome,
) -> FfiTiebaParsedMainVideo {
    let (error, error_code, user_msg, error_msg) = error_flags(outcome.error);
    FfiTiebaParsedMainVideo {
        video: outcome.video.map(|v| FfiTiebaMainVideo {
            url: v.url,
            cover: v.cover,
            duration_sec: v.duration_sec,
            width: v.width,
            height: v.height,
        }),
        error,
        error_code,
        user_msg,
        error_msg,
    }
}

/// 解析帖子详情（`pb/page` 第一页的 protobuf 响应）。[`logged_in`] 决定"是否已关注这个吧"能不能报。
#[uniffi::export]
pub fn tieba_parse_detail(
    bytes: Vec<u8>,
    tid: String,
    logged_in: bool,
) -> Result<FfiTiebaParsedDetail, FfiError> {
    tieba::detail::detail(&bytes, &tid, logged_in)
        .map(to_ffi_parsed_detail)
        .map_err(|e| map_error(PLATFORM, e))
}

/// 解析一页楼层（`pb/page` 的响应）。[`requested_page`] 是请求的页码（游标）。
#[uniffi::export]
pub fn tieba_parse_comments(
    bytes: Vec<u8>,
    requested_page: i32,
) -> Result<FfiTiebaParsedComments, FfiError> {
    tieba::detail::comments(&bytes, requested_page)
        .map(to_ffi_parsed_comments)
        .map_err(|e| map_error(PLATFORM, e))
}

/// 解析一页楼中楼（`pb/floor` 的响应）。
#[uniffi::export]
pub fn tieba_parse_replies(
    bytes: Vec<u8>,
    requested_page: i32,
) -> Result<FfiTiebaParsedComments, FfiError> {
    tieba::detail::replies(&bytes, requested_page)
        .map(to_ffi_parsed_comments)
        .map_err(|e| map_error(PLATFORM, e))
}

/// 这个 `.so` 里贴吧这一份的"版本"：就是 crate 版本（Kotlin 侧排障时看一眼）。
#[uniffi::export]
pub fn tieba_version() -> String {
    feedhub_core::VERSION.to_string()
}
