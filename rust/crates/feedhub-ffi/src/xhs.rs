//! 小红书的 UniFFI 边界：推荐流 / 关注流三个接口的解析、请求签名（`X-s` / `X-S-Common` / `x-rap-param` 那一套），
//! **全是纯函数**。
//!
//! 形状和 `lib.rs` 里的 B站 那批一致：一次调用换一整页 / 一次签名；HTTP 状态码由 Kotlin 传进来（小红书要先判
//! HTTP 461 / 471 / 406 / 416，再看业务码），Rust 不存状态、不发请求。
//!
//! **时间和随机数都是参数**：
//! - `now_ms` / `now_sec` 由 Kotlin 给（`X-t`、x3 里的时间戳、dsllt、xray traceid、笔记 id 推时间的上下界都用它）；
//! - 随机量（x3 的 version、两个 traceid、`x-rap-param` 的掩码 / 会话密钥 / 密钥 / 盐 / 补位 / 加密耗时、
//!   `xy-direction`）也由 Kotlin 从自己的 `Random` 取好；签名器的会话计数（`seq` / `signCount` / `dsllt` / `xraySeq`）
//!   跟着 `FfiXhsSignState` 传进传出。
//! - 因为补位长度要等 gzip 之后才知道（Kotlin 版也是），所以分两步：先 `xhs_rap_pad_len` 问要取几个补位字符，
//!   再 `xhs_sign` 一次签完。
//!
//! 共用类型（`FfiItem` / `FfiError` / `FfiEmote`）直接用 `lib.rs` 里的，**没有加字段**；小红书自己的几页结果
//! （关注列表 / 未看数 / 置顶标记 / 标题拼串）放在这个文件的 record 里。
//!
//! 它们返回的是**通用模型**
//! `FfiDetail` / `FfiCommentPage`（和 B站 / 抖音 / TikTok 同一份，见 `lib.rs` 的 `to_ffi_detail` /
//! `to_ffi_comment_page`），不在这里另造 record。

use crate::play::to_ffi_play;
use crate::{
    map_error, to_ffi_comment_page, to_ffi_detail, to_ffi_page, FfiCommentPage, FfiDetail, FfiEmote,
    FfiError, FfiItem, FfiPlayInfo,
};
use feedhub_core::model::Page;
use feedhub_core::xhs;
use feedhub_core::xhs::sign as xsign;

const PLATFORM: &str = "xhs";

fn internal(detail: impl Into<String>) -> FfiError {
    FfiError::Internal {
        platform: PLATFORM.to_string(),
        detail: detail.into(),
    }
}

fn table(emotes: Vec<FfiEmote>) -> Vec<(String, String)> {
    emotes.into_iter().map(|e| (e.code, e.url)).collect()
}

fn profile(text: &str) -> Result<xsign::Profile, FfiError> {
    xsign::Profile::parse(text).map_err(internal)
}

// ===========================================================================
// 记录
// ===========================================================================

/// 关注列表里的一个人（`followings/self` 的 `user_list[]`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsAuthor {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub xsec_token: Option<String>,
}

/// `followings/self` 的一页。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsFollowings {
    pub users: Vec<FfiXhsAuthor>,
    /// 下一页要带的游标（`None` 表示没有）。
    pub cursor: Option<String>,
    /// 最后一页之前就可能是 true，翻页时要同时看是不是空页（Kotlin `XhsFollowFeed` 负责）。
    pub has_more: bool,
    pub total: Option<i32>,
}

/// `followings/extra` 的一项：用户 id → 未看笔记数。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsUnread {
    pub user_id: String,
    pub count: i32,
}

/// 推荐流的一页：条目 + 游标 + 这一页的标题拼串（Kotlin 用它决定要不要去取表情表，保持原来的行为）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsHomefeed {
    pub nodes: Vec<FfiItem>,
    pub roots: Vec<u32>,
    pub next: Option<String>,
    pub titles: String,
}

/// 搜索结果的一页：条目 + 游标 + 这一页的标题拼串（Kotlin 拿它决定要不要去取表情表，和推荐流一样）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsSearch {
    pub nodes: Vec<FfiItem>,
    pub roots: Vec<u32>,
    pub next: Option<String>,
    pub titles: String,
}

/// `user_posted` 的一页：[`sticky`] 和 [`roots`] 一一对应（置顶笔记排在最前，时间还是它自己的）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsPosted {
    pub nodes: Vec<FfiItem>,
    pub roots: Vec<u32>,
    pub sticky: Vec<bool>,
    pub has_more: bool,
    pub titles: String,
}

/// 一个要签名的请求（字段和 Kotlin `XhsSigner.Request` 一一对应）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsSignRequest {
    /// POST 是路径，GET 是路径 + `?` + 查询串（和实际发出去的逐字节一致）。
    pub api: String,
    pub body: Option<String>,
    pub a1: String,
    pub loadts_ms: i64,
    pub web_build: String,
    pub b1: String,
    pub user_id: Option<String>,
    pub rap: bool,
    pub xy: bool,
}

/// 签名器的会话计数（Kotlin 存着，每次签名后拿新的）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiXhsSignState {
    pub seq: i32,
    pub sign_count: i32,
    pub dsllt: i64,
    pub xray_seq: i32,
}

/// Kotlin `Random` 取到的值（Rust 的签名函数里没有随机源）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsSignRandom {
    pub version: i32,
    pub b3_trace_id: String,
    pub xray_tail: String,
    pub xy_random: i32,
    pub rap_mask: i32,
    pub rap_session_key: String,
    pub rap_key: String,
    pub rap_salt: String,
    pub rap_pad: String,
    pub rap_enc_time_ms: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsHeader {
    pub name: String,
    pub value: String,
}

/// 签好的请求头（**保持插入顺序**，名字小写）和新状态。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsSigned {
    pub headers: Vec<FfiXhsHeader>,
    pub state: FfiXhsSignState,
}

// ===========================================================================
// 解析
// ===========================================================================

/// 推荐流（`POST /api/sns/web/v1/homefeed` 的响应）。
///
/// `http_code` 是这次响应的状态码：小红书要先看 461 / 471 / 406 / 416 / 401 再进业务码（和 Kotlin
/// `XhsApi.checkResponse` 一一对应）。`note_index` 是这一页开始时的 `note_index`（游标 `cursor_score|note_index` 的
/// 后一段），Rust 顺便算好下一页的游标。
#[uniffi::export]
pub fn xhs_parse_homefeed(
    body: String,
    http_code: i32,
    note_index: i32,
    xsec_source: String,
    now_sec: i64,
    emotes: Vec<FfiEmote>,
) -> Result<FfiXhsHomefeed, FfiError> {
    let page = xhs::parse_homefeed(&body, http_code, note_index, &xsec_source, now_sec, &table(emotes))
        .map_err(|e| map_error(PLATFORM, e))?;
    let ffi = to_ffi_page(&page.page);
    Ok(FfiXhsHomefeed {
        nodes: ffi.nodes,
        roots: ffi.roots,
        next: ffi.next,
        titles: page.titles,
    })
}

/// 关注列表（`GET /api/sns/web/v1/user/followings/self` 的响应）。
#[uniffi::export]
pub fn xhs_parse_followings(body: String, http_code: i32) -> Result<FfiXhsFollowings, FfiError> {
    let page = xhs::parse_followings(&body, http_code).map_err(|e| map_error(PLATFORM, e))?;
    Ok(FfiXhsFollowings {
        users: page
            .users
            .into_iter()
            .map(|u| FfiXhsAuthor {
                id: u.id,
                name: u.name,
                avatar: u.avatar,
                xsec_token: u.xsec_token,
            })
            .collect(),
        cursor: page.cursor,
        has_more: page.has_more,
        total: page.total,
    })
}

/// 谁有新笔记（`GET /api/sns/web/v1/user/followings/extra` 的响应）：用户 id → 未看数。
#[uniffi::export]
pub fn xhs_parse_unread(body: String, http_code: i32) -> Result<Vec<FfiXhsUnread>, FfiError> {
    let list = xhs::parse_unread(&body, http_code).map_err(|e| map_error(PLATFORM, e))?;
    Ok(list
        .into_iter()
        .map(|(user_id, count)| FfiXhsUnread { user_id, count })
        .collect())
}

/// 一个作者的主页第一页（`GET /api/sns/web/v1/user_posted` 的响应）。
///
/// `author_*` 是关注列表里的这个人：笔记项里的 `user` 缺失时用它。笔记的 `xsec_source` 固定是 `pc_user`。
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn xhs_parse_posted(
    body: String,
    http_code: i32,
    author_id: String,
    author_name: String,
    author_avatar: Option<String>,
    author_xsec_token: Option<String>,
    now_sec: i64,
    emotes: Vec<FfiEmote>,
) -> Result<FfiXhsPosted, FfiError> {
    let author = xhs::FollowedAuthor {
        id: author_id,
        name: author_name,
        avatar: author_avatar,
        xsec_token: author_xsec_token,
    };
    let page = xhs::parse_posted(&body, http_code, &author, now_sec, &table(emotes))
        .map_err(|e| map_error(PLATFORM, e))?;
    let sticky = page.notes.iter().map(|(_, s)| *s).collect();
    let items = page.notes.into_iter().map(|(item, _)| item).collect();
    let ffi = to_ffi_page(&Page { items, next: None });
    Ok(FfiXhsPosted {
        nodes: ffi.nodes,
        roots: ffi.roots,
        sticky,
        has_more: page.has_more,
        titles: page.titles,
    })
}

/// 搜索的一页（`POST /api/sns/web/v1/search/notes` 的响应）。
///
/// `page` 是这一页的页码、`search_id` 是当前搜索的 id：解析要用它们算下一页的游标（`<search_id>|<page+1>`）。
#[uniffi::export]
pub fn xhs_parse_search(
    body: String,
    http_code: i32,
    page: i32,
    search_id: String,
    now_sec: i64,
    emotes: Vec<FfiEmote>,
) -> Result<FfiXhsSearch, FfiError> {
    let page = xhs::search::parse_search(&body, http_code, page, &search_id, now_sec, &table(emotes))
        .map_err(|e| map_error(PLATFORM, e))?;
    let ffi = to_ffi_page(&page.page);
    Ok(FfiXhsSearch {
        nodes: ffi.nodes,
        roots: ffi.roots,
        next: ffi.next,
        titles: page.titles,
    })
}

// ===========================================================================
// 签名
// ===========================================================================

/// 这一次 `x-rap-param` 要取几个补位字符（`(16 - gzip 长度 % 16) % 16`）。
///
/// Kotlin 版里补位长度是 gzip 之后才知道的、补位字符又紧挨着盐从同一个 `Random` 里取，所以调用方要先问长度、
/// 再照那个数取字符。纯计算：不占状态、不写东西（重复算一遍 payload + gzip）。
#[uniffi::export]
pub fn xhs_rap_pad_len(
    request: FfiXhsSignRequest,
    profile_json: String,
    now_ms: i64,
    rap_mask: i32,
    session_key: String,
) -> Result<u32, FfiError> {
    let p = profile(&profile_json)?;
    let n = xsign::rap_pad_len(&p, &req_of(request), now_ms, rap_mask, &session_key).map_err(internal)?;
    Ok(n as u32)
}

/// 签一个请求，返回要加的请求头（保持顺序）和新状态。
#[uniffi::export]
pub fn xhs_sign(
    request: FfiXhsSignRequest,
    profile_json: String,
    now_ms: i64,
    rand: FfiXhsSignRandom,
    state: FfiXhsSignState,
) -> Result<FfiXhsSigned, FfiError> {
    let p = profile(&profile_json)?;
    let random = xsign::SignRandom {
        version: rand.version,
        b3_trace_id: rand.b3_trace_id,
        xray_tail: rand.xray_tail,
        xy_random: rand.xy_random,
        rap_mask: rand.rap_mask,
        rap_session_key: rand.rap_session_key,
        rap_key: rand.rap_key,
        rap_salt: rand.rap_salt,
        rap_pad: rand.rap_pad,
        rap_enc_time_ms: rand.rap_enc_time_ms,
    };
    let (headers, next) = xsign::sign(&p, &req_of(request), now_ms, &random, provide_state(state))
        .map_err(internal)?;
    Ok(FfiXhsSigned {
        headers: headers
            .into_iter()
            .map(|(name, value)| FfiXhsHeader { name, value })
            .collect(),
        state: FfiXhsSignState {
            seq: next.seq,
            sign_count: next.sign_count,
            dsllt: next.dsllt,
            xray_seq: next.xray_seq,
        },
    })
}

/// `b1`（`X-S-Common` 的 x8）：一次生成、之后一直用（网页存在 localStorage 里），由 Kotlin 存进 `ctx.store`。
#[uniffi::export]
pub fn xhs_new_b1(profile_json: String, now_ms: i64, loadts_ms: i64) -> Result<String, FfiError> {
    let p = profile(&profile_json)?;
    Ok(xsign::b1::encode(&p, now_ms, loadts_ms))
}

/// 把 `b1` 解开成 JSON（自检 / 测试用；生产不调）。
#[uniffi::export]
pub fn xhs_decode_b1(b1: String) -> Result<String, FfiError> {
    xsign::b1::decode(&b1).map_err(internal)
}

// ===========================================================================
// 详情 / 评论 / 楼中楼
// ===========================================================================

/// 笔记详情（`POST /api/sns/web/v1/feed` 的响应）。
///
/// [`id`] 是卡片 `detail` 里的那个 `<笔记 id>@<xsec_source>@<xsec_token>`；[`emotes`] 是 Kotlin
/// **手上已经有**的表情表（没有就给空表，Rust 不为表发请求、也不在这里取表）。
#[uniffi::export]
pub fn xhs_parse_detail(
    body: String,
    http_code: i32,
    id: String,
    now_sec: i64,
    emotes: Vec<FfiEmote>,
) -> Result<FfiDetail, FfiError> {
    let reference = xhs::parse_native_id(&id);
    let detail = xhs::detail::parse_detail(&body, http_code, &reference, &table(emotes), now_sec)
        .map_err(|e| map_error(PLATFORM, e))?;
    Ok(to_ffi_detail(&detail))
}

/// 一页一级评论（`GET comment/page` 的响应）。
#[uniffi::export]
pub fn xhs_parse_comments(
    body: String,
    http_code: i32,
    emotes: Vec<FfiEmote>,
) -> Result<FfiCommentPage, FfiError> {
    let page = xhs::detail::parse_comments(&body, http_code, &table(emotes)).map_err(|e| map_error(PLATFORM, e))?;
    Ok(to_ffi_comment_page(&page))
}

/// 一页楼中楼（`GET comment/sub/page` 的响应）。
#[uniffi::export]
pub fn xhs_parse_replies(
    body: String,
    http_code: i32,
    emotes: Vec<FfiEmote>,
) -> Result<FfiCommentPage, FfiError> {
    let page = xhs::detail::parse_replies(&body, http_code, &table(emotes)).map_err(|e| map_error(PLATFORM, e))?;
    Ok(to_ffi_comment_page(&page))
}

// ===========================================================================
// 播放
// ===========================================================================

/// 表情表：`GET /api/im/redmoji/detail` 的响应 → 表。
///
/// [`fetched_at_sec`](FfiXhsEmotes::fetched_at_sec) 就是传进来的 [`now_sec`]（Kotlin
/// `XhsEmoteTable.fromDetail` 也是这么算的）；[`stored`](FfiXhsEmotes::stored) 是写进 `ctx.store`
/// 的那串（`{"v":1,"at":…,"map":{…}}`），两边逐字节一样才算"老数据照读"没问题。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsEmotes {
    pub emotes: Vec<FfiEmote>,
    pub fetched_at_sec: i64,
    pub stored: String,
}

#[uniffi::export]
pub fn xhs_parse_emotes(body: String, http_code: i32, now_sec: i64) -> Result<FfiXhsEmotes, FfiError> {
    let root = xhs::check_http(&body, http_code).map_err(|e| map_error(PLATFORM, e))?;
    let table = xhs::emotes::EmoteTable::from_detail(&root, now_sec);
    Ok(FfiXhsEmotes {
        emotes: table
            .map
            .iter()
            .map(|(code, url)| FfiEmote {
                code: code.clone(),
                url: url.clone(),
            })
            .collect(),
        fetched_at_sec: table.fetched_at_sec,
        stored: table.to_stored(),
    })
}

/// 把 `ctx.store` 里那串表情表读回来（纯函数；**保持表里的顺序**，坏数据返回空表）。
///
/// 读不出来（版本不对 / JSON 坏了）和 Kotlin `XhsEmoteTable.fromStored` 一样返回 `None`。
#[uniffi::export]
pub fn xhs_read_emotes(stored: String) -> Option<Vec<FfiEmote>> {
    xhs::emotes::EmoteTable::from_stored(&stored).map(|t| {
        t.map
            .into_iter()
            .map(|(code, url)| FfiEmote { code, url })
            .collect()
    })
}

/// 详情响应 → 播放信息。
///
/// [`body`] 是 `POST /api/sns/web/v1/feed` 的响应：先照 [`xhs::check_http`] 判状态码 + 业务码，
/// 再取 `note_card`，然后交给 `xhs::play::play`。一条视频流都没有是 `Api("no_stream")`。
///
/// 平台对象那条路（[`crate::xhs_source::FfiXhsSource::play`]）还多管"复用详情刚取过的那份"，
/// 这个纯函数**只解析**、不发请求、不看缓存。
#[uniffi::export]
pub fn xhs_parse_play(body: String, http_code: i32) -> Result<FfiPlayInfo, FfiError> {
    let root = xhs::check_http(&body, http_code).map_err(|e| map_error(PLATFORM, e))?;
    let card = xhs::detail::note_card(&root).map_err(|e| map_error(PLATFORM, e))?;
    let play = xhs::play::play(&card).map_err(|e| map_error(PLATFORM, e))?;
    Ok(to_ffi_play(&play))
}

// ===========================================================================
// 关注流的存盘 / 纯逻辑
// ===========================================================================
//
// 这几条是**给对拍**用的纯函数：Kotlin 写一份、Rust 读一份，再反过来；外加
// "按这套逻辑排一遍谁该重拉 / 合并出哪一页"。生产路径不调它们（那一路在
// `crate::xhs_source::FfiXhsSource::follow_page` 里，调度和存盘都在平台对象里）。

/// 关注流作者缓冲的**明文 JSON**（`gz1:` 解开之后那段）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsAuthors {
    pub extra_at: i64,
    /// 明文 JSON（`{"v":1,"extraAt":…,"states":[…]}`）。Rust 写出来的这串要能被 Kotlin 读回来。
    pub json: String,
    /// 每个作者一条摘要（**按插入顺序**）。
    pub states: Vec<crate::xhs_source::FfiXhsFollowAuthor>,
}

// 作者摘要那个 record（`FfiXhsFollowAuthor`）定义在 `xhs_source.rs`（平台对象那边）：
// UniFFI 的 record 名字是全局唯一的，同一份形状不能再写一遍。纯函数这条路径直接复用它。

/// **读一段作者缓冲的明文 JSON**（`{"v":1,"extraAt":…,"states":[…]}`）→ 摘要列表。
///
/// 版本不对 / 坏 JSON / 缺 `states` 都回 `None`（和 Kotlin `XhsFollowStore.loadAuthors` 一致）。
#[uniffi::export]
pub fn xhs_follow_parse_authors(json: String) -> Option<FfiXhsAuthors> {
    let authors = feedhub_core::xhs::follow::parse_authors_json(&json)?;
    Some(authors_of(&authors))
}

/// **写一份作者缓冲**：Kotlin 传一份明文 JSON 进来（它自己写的），Rust 读进来再写出去，
/// 比对"读进来再写出去逐字节一样"。
///
/// 为什么要过一趟 Kotlin：Rust 写的串要能被 Kotlin 读（`XhsFollowStore.loadAuthors`），
/// Rust 读 Kotlin 写的串也要一样——两头都在 Kotlin 测试里断言。
#[uniffi::export]
pub fn xhs_follow_round_trip(json: String) -> Option<FfiXhsAuthors> {
    let authors = feedhub_core::xhs::follow::parse_authors_json(&json)?;
    Some(authors_of(&authors))
}

/// **谁该重拉**：`states` 是一份明文 JSON 的作者缓冲，按 `now_ms` / `limit` 算一遍。
///
/// 返回的 id 顺序就是 Kotlin `XhsFollowFreshness.due` 的顺序（有新笔记的在前 → 没拉过的 → 最久没拉的）。
#[uniffi::export]
pub fn xhs_follow_due(json: String, now_ms: i64, limit: u32) -> Option<Vec<String>> {
    let authors = feedhub_core::xhs::follow::parse_authors_json(&json)?;
    Some(feedhub_core::xhs::follow::due(
        &authors.ordered(),
        now_ms,
        limit as usize,
    ))
}

/// **跨作者归并出一页**（`XhsFollowMerge.nextPage`）：`before` 是游标（`<秒>:<id>`），`None` = 从最新开始。
///
/// 返回（条目 + 游标 + `more`）；游标无效时 `None`（Kotlin 那边抛 `ParseException`）。
#[uniffi::export]
pub fn xhs_follow_next_page(
    json: String,
    before: Option<String>,
    limit: u32,
) -> Option<FfiXhsHomefeed> {
    let authors = feedhub_core::xhs::follow::parse_authors_json(&json)?;
    let before_key = match before.as_deref() {
        Some(text) => Some(feedhub_core::xhs::follow::NoteKey::decode(text)?),
        None => None,
    };
    let merge = feedhub_core::xhs::follow::next_page(
        &authors.ordered(),
        before_key.as_ref(),
        limit as usize,
    );
    let page = Page {
        items: merge.items,
        next: None,
    };
    let ffi = to_ffi_page(&page);
    // `more` 借用 `titles` 这个字段带出去（`FfiXhsHomefeed` 没有多余的槽位，也不想为测试新造 record）：
    // `"more"` = 后面还有，空串 = 到底了。游标由调用方自己从最后一条算（Kotlin 那边也是这么做的）。
    Some(FfiXhsHomefeed {
        nodes: ffi.nodes,
        roots: ffi.roots,
        next: None,
        titles: if merge.more { "more".to_string() } else { String::new() },
    })
}

/// **游标编解码**（`NoteKey.encode` / `NoteKey.decode`）：`None` = 解不出来。
#[uniffi::export]
pub fn xhs_follow_note_key_encode(time: i64, id: String) -> String {
    feedhub_core::xhs::follow::NoteKey::new(time, id).encode()
}

/// [`xhs_follow_note_key_encode`] 的逆运算。
#[uniffi::export]
pub fn xhs_follow_note_key_decode(text: String) -> Option<String> {
    feedhub_core::xhs::follow::NoteKey::decode(&text).map(|k| k.encode())
}

fn authors_of(authors: &feedhub_core::xhs::follow::Authors) -> FfiXhsAuthors {
    FfiXhsAuthors {
        extra_at: authors.extra_at,
        json: feedhub_core::xhs::follow::encode_authors(authors),
        states: authors
            .values()
            .map(|a| crate::xhs_source::FfiXhsFollowAuthor {
                id: a.id.clone(),
                name: a.name.clone(),
                xsec_token: a.xsec_token.clone(),
                items: a.items.len() as u32,
                fetched_at: a.fetched_at,
                unread: a.unread,
                unread_at_fetch: a.unread_at_fetch,
                closed_until: a.closed_until,
            })
            .collect(),
    }
}

fn req_of(r: FfiXhsSignRequest) -> xsign::SignRequest {
    xsign::SignRequest {
        api: r.api,
        body: r.body,
        a1: r.a1,
        loadts_ms: r.loadts_ms,
        web_build: r.web_build,
        b1: r.b1,
        user_id: r.user_id,
        rap: r.rap,
        xy: r.xy,
    }
}

fn provide_state(s: FfiXhsSignState) -> xsign::SignState {
    xsign::SignState {
        seq: s.seq,
        sign_count: s.sign_count,
        dsllt: s.dsllt,
        xray_seq: s.xray_seq,
    }
}
