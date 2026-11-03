//! 贴吧**详情 / 楼层 / 楼中楼**：`pb/page` 与 `pb/floor` 的响应 → 统一模型。
//! 逐条对照 `source-tieba/TiebaDetailParser.kt`（字段含义照 aiotieba Unlicense 的 `api/get_posts` /
//! `api/get_comments` 和 TiebaLite GPL-3.0 的 `ThreadViewModel` / `subposts`；这里只做映射）。
//!
//! 三个接口的分工（和 Kotlin 一样）：
//! - **详情**：`pb/page` 第一页（正序、只要一楼）——条目、正文、主视频、吧（关注按钮）；
//! - **楼层**：同一个接口按页取，丢掉一楼，每楼带几条楼中楼预览；游标是页码，`asc` / `desc` / `hot`；
//! - **楼中楼**：`pb/floor`，`commentId` 是楼层的 pid，游标也是页码。
//!
//! 两条写进口径里的规矩：
//! 1. **平台错误码不当错误抛**：`Error.error_code` 非 0 时返回 [`PlatformError`]（数据带回去），
//!    归类规则（未登录 / 风控 / 110001 签名）只有 Kotlin 的 `TiebaSource.mapError` 一份；
//!    解析不了（不是 protobuf、缺 data、缺一楼）才是 [`Error::Parse`]。
//! 2. 翻过头时服务端**不报错**，而是把第一页原样再给一遍（`current_page` 却是请求的页码）：`page.total_page`
//!    说了算，直接给空页、没有游标。
//!
//! pb 消息里"两个响应共用"的（`User` / `Agree` / `PbContent` / `Page` / `ThreadInfo` / `SimpleForum` /
//! `VideoInfo`）在 `tieba/mod.rs`（信息流那一层已经在读它们）；这里只加详情 / 楼层自己那几个
//! （`Post` / `SubPostList` / `SubPost` / `PbContent.meme_info` 用的 `MemeInfo`）。

use std::collections::HashMap;

use crate::detail::{
    Block, Comment, CommentPage, FeedDetail, FollowKind, FollowState, FollowTarget, RichText, Span,
    VideoInfo, VideoPart,
};
use crate::json;
use crate::model::{
    DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision, UserRef,
};

use super::pb::{self, PbError};
use super::{
    content, envelope_error, is_blank, kotlin_trim, parse_failure, pb_failure, thread_url, user,
    Agree, Content, ErrorMsg, PageMsg, PlatformError, SimpleForum, ThreadInfo, User, TIEBA,
};
use super::content::MainVideo;

/// 详情页正文摘要的长度（Kotlin `SUMMARY_LEN`），单位是 **UTF-16 码元**（Kotlin `String.take`）。
const SUMMARY_LEN: usize = 120;

/// 解析结果：**要么**有值，**要么**是一个平台错误（`error_code` 非 0 / JSON 错误外壳）。
/// 两者不会同时出现；平台错误怎么归类由 Kotlin 的 `TiebaSource.mapError` 决定。
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed<T> {
    pub value: Option<T>,
    pub error: Option<PlatformError>,
}

impl<T> Parsed<T> {
    pub fn ok(value: T) -> Self {
        Parsed {
            value: Some(value),
            error: None,
        }
    }

    pub fn failed(error: PlatformError) -> Self {
        Parsed {
            value: None,
            error: Some(error),
        }
    }
}

/// 主视频（播放地址 / 封面 / 时长 / 转码后的宽高）。`None` 表示这条帖子没有一楼视频。
///
/// 为什么平台对象要交出它：`recentPages`（详情取过的第一页给播放复用，5 分钟）**在 Rust 里**
/// （见 `tieba/source.rs`），Kotlin 的 `play(partId=main)` 从这里拿地址，不再自己解析一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainVideoOutcome {
    pub video: Option<MainVideo>,
    pub error: Option<PlatformError>,
}

/// `play` 的结果：**要么**有播放信息，**要么**是一个平台错误（`error_code` 非 0）。
///
/// 两者都不出现 = 参数没问题、帖子里就是没有那个视频（`main` 时是一楼没视频）——
/// Kotlin 那边翻成 `ParseException("帖子 … 一楼里没有视频")`。
#[derive(Debug, Clone, PartialEq)]
pub struct PlayOutcome {
    pub info: Option<crate::play::PlayInfo>,
    pub error: Option<PlatformError>,
}

// ===========================================================================
// 入口
// ===========================================================================

/// 帖子详情。[`tid`] 是条目的 id（和信息流的 `FeedItem.id` 一样），[`logged_in`] 决定
/// "是否已关注这个吧"能不能报（匿名请求服务端恒给 `is_like = 0`）。
pub fn detail(bytes: &[u8], tid: &str, logged_in: bool) -> Result<Parsed<FeedDetail>, super::Error> {
    let res = decode_page(bytes)?;
    if let Some(error) = page_error(&res) {
        return Ok(Parsed::failed(error));
    }
    detail_of(&res, tid, logged_in).map(Parsed::ok)
}

/// 一页楼层（一楼是正文，这里丢掉）。[`requested_page`] 是请求的页码（游标）。
pub fn comments(
    bytes: &[u8],
    requested_page: i32,
) -> Result<Parsed<CommentPage>, super::Error> {
    let res = decode_page(bytes)?;
    if let Some(error) = page_error(&res) {
        return Ok(Parsed::failed(error));
    }
    comments_of(&res, requested_page).map(Parsed::ok)
}

/// 一页楼中楼。[`requested_page`] 是请求的页码（游标）。
pub fn replies(
    bytes: &[u8],
    requested_page: i32,
) -> Result<Parsed<CommentPage>, super::Error> {
    let res = decode_floor(bytes)?;
    if let Some(error) = floor_error(&res) {
        return Ok(Parsed::failed(error));
    }
    replies_of(&res, requested_page).map(Parsed::ok)
}

// ===========================================================================
// `pb/page` 的映射
// ===========================================================================

/// 帖子详情：条目用 `thread`（标题 / 计数 / 发帖时间），正文用一楼的内容段，作者按 `author_id` 在
/// `user_list` 里查（`thread.author` 里也可能是同一份，按 Kotlin 的顺序退）。
pub(crate) fn detail_of(
    res: &PageResponse,
    tid: &str,
    logged_in: bool,
) -> Result<FeedDetail, super::Error> {
    let data = res
        .data
        .as_ref()
        .ok_or_else(|| parse_failure("帖子响应缺少 data"))?;
    let users = users_map(&data.user_list);
    let thread = data.thread.as_ref();
    let first = data
        .first_floor_post
        .as_ref()
        .or_else(|| data.post_list.iter().find(|p| p.floor == 1))
        .ok_or_else(|| parse_failure(format!("帖子 {tid} 的第一页里没有一楼")))?;
    let forum = data.forum.as_ref();
    let main_video = page_main_video(res);
    // 一楼第一个视频是整条帖子的主视频（FeedDetail.video），不放进正文
    let body = content::rich_text(&first.content, content::main_video(&first.content).is_some());
    let author_id = thread
        .filter(|t| t.author_id != 0)
        .map(|t| t.author_id)
        .unwrap_or(first.author_id);
    let author = users
        .get(&author_id)
        .or_else(|| thread.and_then(|t| t.author.as_ref()))
        .or_else(|| first.author.as_ref())
        .and_then(user);
    let forum_name = forum
        .and_then(|f| non_blank(&f.name))
        .or_else(|| thread.and_then(|t| non_blank(&t.forum_name)));
    let url = thread_url(tid);
    let title = thread
        .map(|t| kotlin_trim(&t.title))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            let t = kotlin_trim(&first.title);
            (!t.is_empty()).then(|| t.to_string())
        });
    let summary = summary_of(&body);
    // 摘要是从正文截出来的，里面的 [滑稽] 对应的图就是正文里的 Span.Emote（和信息流里同一个地址）
    let body_emotes = emotes_of(&body);
    let emotes = emotes_used(&body_emotes, title.as_deref(), summary.as_deref());
    let item = FeedItem {
        platform: TIEBA.to_string(),
        id: tid.to_string(),
        kind: ItemKind::Post,
        title,
        summary,
        language: None,
        author,
        published_at: thread
            .map(|t| t.create_time)
            .filter(|v| *v > 0)
            .map(|v| v as i64)
            .or_else(|| (first.time > 0).then_some(first.time as i64)),
        published_at_precision: TimePrecision::Second,
        media: media(&first.content),
        stats: Stats {
            // thread.view_num 在 pb/page 里是 0，浏览数在 data.thread_freq_num（和信息流里的"浏览"一致）
            views: thread
                .and_then(|t| (t.view_num > 0).then_some(t.view_num as i64))
                .or_else(|| (data.thread_freq_num > 0).then_some(data.thread_freq_num)),
            likes: thread.map(|t| {
                t.agree
                    .as_ref()
                    .map(|a| a.agree_num)
                    .unwrap_or(t.agree_num as i64)
            }),
            comments: thread.map(|t| t.reply_num as i64),
            shares: thread.and_then(|t| (t.share_num > 0).then_some(t.share_num)),
            ..Stats::default()
        },
        reason: None,
        community: forum_name,
        community_icon: None,
        repost: None,
        emotes,
        adult: false,
        url: Some(url),
        detail: DetailTarget::Native {
            id: tid.to_string(),
            platform: None,
        },
    };
    Ok(FeedDetail {
        item,
        body,
        video: main_video.map(|v| VideoInfo {
            parts: vec![VideoPart {
                id: content::MAIN_PART.to_string(),
                title: None,
                duration_sec: v.duration_sec,
                width: v.width,
                height: v.height,
            }],
            danmaku_segment_ms: None,
        }),
        follow: follow_state(forum, logged_in),
        related: Vec::new(),
    })
}

/// 一页楼层。一楼丢掉（含倒序时出现在最后一页的一楼）；服务端 `page.has_more` 为 1 时游标是
/// `current_page + 1`；请求的页码超过总页数时返回空页。
pub(crate) fn comments_of(
    res: &PageResponse,
    requested_page: i32,
) -> Result<CommentPage, super::Error> {
    let data = res
        .data
        .as_ref()
        .ok_or_else(|| parse_failure("帖子响应缺少 data"))?;
    let total_pages = data.page.as_ref().map(|p| p.total_page).unwrap_or(0);
    if total_pages > 0 && requested_page > total_pages {
        return Ok(CommentPage {
            items: Vec::new(),
            next: None,
        });
    }
    let users = users_map(&data.user_list);
    let mut items: Vec<Comment> = Vec::new();
    for post in &data.post_list {
        if post.floor == 1 || post.id == 0 {
            continue;
        }
        let comment = post_to_comment(post, &users);
        if items.iter().any(|c| c.id == comment.id) {
            continue;
        }
        items.push(comment);
    }
    let page = data.page.as_ref();
    let more = page.map(|p| p.has_more != 0).unwrap_or(false) && !data.post_list.is_empty();
    let next = more.then(|| {
        let current = page
            .map(|p| p.current_page)
            .filter(|p| *p > 0)
            .unwrap_or(requested_page);
        (current + 1).to_string()
    });
    Ok(CommentPage { items, next })
}

/// 一页楼中楼。
pub(crate) fn replies_of(
    res: &FloorResponse,
    requested_page: i32,
) -> Result<CommentPage, super::Error> {
    let data = res
        .data
        .as_ref()
        .ok_or_else(|| parse_failure("楼中楼响应缺少 data"))?;
    let mut items: Vec<Comment> = Vec::new();
    for sub in &data.subpost_list {
        let comment = sub_post_to_comment(sub, &HashMap::new());
        if items.iter().any(|c| c.id == comment.id) {
            continue;
        }
        items.push(comment);
    }
    let page = data.page.as_ref();
    let more = page.map(|p| p.has_more != 0).unwrap_or(false) && !data.subpost_list.is_empty();
    let next = more.then(|| {
        let current = page
            .map(|p| p.current_page)
            .filter(|p| *p > 0)
            .unwrap_or(requested_page);
        (current + 1).to_string()
    });
    Ok(CommentPage { items, next })
}

/// 一楼里的主视频：内容段里的第一个视频优先（地址 / 封面 / 时长来自内容段），
/// 宽高优先用 `thread.video_info` 里**转码后真正播放**的那一份；没有内容段才退到 `video_info`。
pub(crate) fn page_main_video(res: &PageResponse) -> Option<MainVideo> {
    let data = res.data.as_ref()?;
    let first = data
        .first_floor_post
        .as_ref()
        .or_else(|| data.post_list.iter().find(|p| p.floor == 1));
    let info = data.thread.as_ref().and_then(|t| t.video_info.as_ref());
    if let Some(from_content) = first.and_then(|f| content::main_video(&f.content)) {
        let width = info
            .and_then(|v| (v.video_width > 0).then_some(v.video_width as i32))
            .or(from_content.width);
        let height = info
            .and_then(|v| (v.video_height > 0).then_some(v.video_height as i32))
            .or(from_content.height);
        return Some(MainVideo {
            url: from_content.url,
            cover: from_content.cover,
            duration_sec: from_content.duration_sec,
            width,
            height,
        });
    }
    let info = info?;
    let url = non_blank(&info.video_url).map(|u| json::to_https(&u))?;
    Some(MainVideo {
        url,
        cover: non_blank(&info.thumbnail_url).map(|u| json::to_https(&u)),
        duration_sec: (info.video_duration > 0).then_some(info.video_duration as i32),
        width: (info.video_width > 0).then_some(info.video_width as i32),
        height: (info.video_height > 0).then_some(info.video_height as i32),
    })
}

/// 一页楼层 → 一条评论。作者优先 `user_list`（按 `author_id`）、再退到楼里自带的 `author`。
fn post_to_comment(post: &Post, users: &HashMap<i64, User>) -> Comment {
    let author = users
        .get(&post.author_id)
        .or_else(|| post.author.as_ref());
    Comment {
        platform: TIEBA.to_string(),
        id: post.id.to_string(),
        author: author.and_then(comment_user),
        content: content::rich_text(&post.content, false),
        published_at: (post.time > 0).then_some(post.time as i64),
        published_at_precision: TimePrecision::Second,
        likes: post.agree.as_ref().map(|a| a.agree_num),
        reply_count: Some(post.sub_post_number as i64),
        reply_to: None,
        ip_location: author.and_then(|u| non_blank(&u.ip_address)),
        floor: (post.floor > 0).then_some(post.floor as i32),
        pinned: false,
        preview_replies: post
            .sub_post_list
            .as_ref()
            .map(|list| {
                list.sub_post_list
                    .iter()
                    .map(|sub| sub_post_to_comment(sub, users))
                    .collect()
            })
            .unwrap_or_default(),
        language: None,
    }
}

/// 一条楼中楼。开头的"回复 xxx :"拆出来放 `reply_to`，正文不再带这个前缀。
fn sub_post_to_comment(sub: &SubPostList, users: &HashMap<i64, User>) -> Comment {
    let author = sub
        .author
        .as_ref()
        .filter(|a| a.id != 0)
        .or_else(|| users.get(&sub.author_id));
    let (reply_to, content) = content::split_reply_prefix(&sub.content);
    Comment {
        platform: TIEBA.to_string(),
        id: sub.id.to_string(),
        author: author.and_then(comment_user),
        content: content::rich_text(&content, false),
        published_at: (sub.time > 0).then_some(sub.time as i64),
        published_at_precision: TimePrecision::Second,
        likes: sub.agree.as_ref().map(|a| a.agree_num),
        reply_count: None,
        reply_to,
        ip_location: author.and_then(|u| non_blank(&u.ip_address)),
        floor: None,
        pinned: false,
        preview_replies: Vec::new(),
        language: None,
    }
}

/// 作者。有的账号没有昵称（响应里只有 id 和头像），名字退成"贴吧用户"，免得丢掉头像和 id。
fn comment_user(u: &User) -> Option<UserRef> {
    if let Some(user) = user(u) {
        return Some(user);
    }
    if u.id == 0 {
        return None;
    }
    let portrait = portrait_of(&u.portrait);
    Some(UserRef {
        platform: TIEBA.to_string(),
        id: u.id.to_string(),
        name: "贴吧用户".to_string(),
        avatar: portrait.as_deref().map(super::avatar_url),
        url: portrait.as_deref().map(super::user_url),
    })
}

/// `user_list`：同 id 后面的覆盖前面的（Kotlin `associateBy`）。
fn users_map(list: &[User]) -> HashMap<i64, User> {
    list.iter()
        .filter(|u| u.id != 0)
        .map(|u| (u.id, u.clone()))
        .collect()
}

/// 一楼里的图片和视频（收藏 / 历史列表里的缩略图用，和信息流里的 `media` 一样）。
fn media(list: &[Content]) -> Vec<Media> {
    let mut out: Vec<Media> = Vec::new();
    for c in list {
        match c.type_ {
            // Kotlin 这里**不过滤**宽高 > 0（`toIntOrNull()` 是什么就给什么）
            3 | 20 => {
                if let Some(url) = content::image_url(c) {
                    out.push(Media::Image {
                        url,
                        width: before_comma(&c.bsize).parse::<i32>().ok(),
                        height: after_comma(&c.bsize).parse::<i32>().ok(),
                    });
                }
            }
            // 视频插在列表最前面；封面取 `src`（不是 mp4 地址），时长 `during_time`
            5 => out.insert(
                0,
                Media::Video {
                    cover: non_blank(&c.src).map(|s| json::to_https(&s)),
                    duration_sec: (c.during_time > 0).then_some(c.during_time as i32),
                    width: None,
                    height: None,
                },
            ),
            _ => {}
        }
    }
    out
}

/// `bsize.substringBefore(',')`。
fn before_comma(bsize: &str) -> &str {
    bsize.split_once(',').map(|(a, _)| a).unwrap_or(bsize)
}

/// `bsize.substringAfter(',', "")`。
fn after_comma(bsize: &str) -> &str {
    bsize.split_once(',').map(|(_, b)| b).unwrap_or("")
}

/// 正文里的文字（不把图片 / 视频降级成占位符），再按 Java 正则的 `\s+` 压成一个空格、trim、截到
/// [`SUMMARY_LEN`]。
fn summary_of(body: &RichText) -> Option<String> {
    let text = body
        .blocks
        .iter()
        .filter(|b| !matches!(b, Block::Image { .. } | Block::Video { .. }))
        .map(Block::plain_text)
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed = collapse_jvm_spaces(&text);
    let trimmed = kotlin_trim(&collapsed);
    if trimmed.is_empty() {
        return None;
    }
    Some(take_utf16(trimmed, SUMMARY_LEN))
}

/// Java 正则 `\s+`（ASCII 空白）→ 一个空格。
fn collapse_jvm_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for ch in s.chars() {
        if matches!(ch, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r') {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    out
}

/// Kotlin `String.take(n)`：按 **UTF-16 码元**取前 n 个。Rust 的 `String` 放不下孤立代理项，
/// 所以在会切到代理对中间时**停在这个字符之前**（真实的贴吧正文里不会出现这种情况）。
fn take_utf16(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let len = ch.len_utf16();
        if used + len > n {
            break;
        }
        out.push(ch);
        used += len;
    }
    out
}

/// 正文里（段落 / 小标题 / 引用）的表情：文字代码 → 图片地址，`putIfAbsent` 且保留出现顺序。
fn emotes_of(body: &RichText) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for block in &body.blocks {
        let spans: &[Span] = match block {
            Block::Paragraph { spans } | Block::Heading { spans } | Block::Quote { spans } => spans,
            _ => continue,
        };
        for span in spans {
            if let Span::Emote { text, url } = span {
                if !out.iter().any(|(code, _)| code == text) {
                    out.push((text.clone(), url.clone()));
                }
            }
        }
    }
    out
}

/// 对应 `Emotes.used`：只留文字里真的出现过的代码，顺序就是表的插入顺序。
fn emotes_used(
    table: &[(String, String)],
    title: Option<&str>,
    summary: Option<&str>,
) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let texts: Vec<&str> = [title, summary]
        .into_iter()
        .flatten()
        .filter(|t| !t.is_empty())
        .collect();
    if texts.is_empty() {
        return Vec::new();
    }
    table
        .iter()
        .filter(|(code, url)| {
            !code.is_empty() && !url.is_empty() && texts.iter().any(|t| t.contains(code))
        })
        .cloned()
        .collect()
}

/// 吧 → 关注按钮对象。[`logged_in`] 为 false 时 `following` 一律 `None`（服务端恒给 `is_like = 0`）。
fn follow_state(forum: Option<&SimpleForum>, logged_in: bool) -> Option<FollowState> {
    let forum = forum?;
    if forum.id <= 0 || is_blank(&forum.name) {
        return None;
    }
    let kind = [&forum.first_class, &forum.second_class]
        .into_iter()
        .filter(|s| !is_blank(s))
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    Some(FollowState {
        target: FollowTarget {
            platform: TIEBA.to_string(),
            kind: FollowKind::Forum,
            id: forum.id.to_string(),
            name: forum.name.clone(),
            avatar: non_blank(&forum.avatar).map(|a| json::to_https(&a)),
            description: (!kind.is_empty()).then_some(kind),
            url: Some(forum_url(&forum.name)),
        },
        following: logged_in.then_some(forum.is_like == 1),
    })
}

/// 吧的网页地址（Kotlin `TiebaDetailParser.forumUrl`：`URLEncoder.encode(name, "UTF-8")`）。
pub fn forum_url(name: &str) -> String {
    format!(
        "https://tieba.baidu.com/f?kw={}",
        super::source::form_encode(name)
    )
}

/// 非空白才有值（Kotlin `ifBlank { null }`）。
fn non_blank(s: &str) -> Option<String> {
    if is_blank(s) {
        None
    } else {
        Some(s.to_string())
    }
}

/// `portrait` 去掉 `?t=…` 之后（空白算没有）。
fn portrait_of(raw: &str) -> Option<String> {
    let before = raw.split_once('?').map(|(a, _)| a).unwrap_or(raw);
    non_blank(before)
}

// ===========================================================================
// pb 消息（详情 / 楼层自己那几个）
// ===========================================================================

/// 一条楼层（`Post`）。只读我们用到的字段。
#[derive(Debug, Clone, Default)]
pub(crate) struct Post {
    pub id: u64,
    pub title: String,
    pub floor: i32,
    pub time: u32,
    pub content: Vec<Content>,
    pub sub_post_number: i32,
    pub sub_post_list: Option<SubPost>,
    pub author_id: i64,
    pub author: Option<User>,
    pub agree: Option<Agree>,
}

/// 一条楼层带的楼中楼预览（`SubPost`）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SubPost {
    pub sub_post_list: Vec<SubPostList>,
}

/// 楼中楼的一条（`SubPostList`，预览和 `pb/floor` 里是同一种消息）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SubPostList {
    pub id: u64,
    pub content: Vec<Content>,
    pub time: u32,
    pub author_id: i64,
    pub author: Option<User>,
    pub agree: Option<Agree>,
}

/// `pb/page` 的 data（`PbPageResponseData`）：只读详情要的那几个字段。
#[derive(Debug, Clone, Default)]
pub(crate) struct PageData {
    pub forum: Option<SimpleForum>,
    pub page: Option<PageMsg>,
    pub post_list: Vec<Post>,
    pub thread: Option<ThreadInfo>,
    pub user_list: Vec<User>,
    pub thread_freq_num: i64,
    pub first_floor_post: Option<Post>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PageResponse {
    pub error: Option<ErrorMsg>,
    pub data: Option<PageData>,
}

/// `pb/floor` 的 data（`PbFloorResponseData`）。
#[derive(Debug, Clone, Default)]
pub(crate) struct FloorData {
    pub page: Option<PageMsg>,
    pub post: Option<Post>,
    pub subpost_list: Vec<SubPostList>,
    pub thread: Option<ThreadInfo>,
    pub forum: Option<SimpleForum>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FloorResponse {
    pub error: Option<ErrorMsg>,
    pub data: Option<FloorData>,
}

// ---- 解码 ------------------------------------------------------------------

/// `pb/page` 的响应字节。解码失败 → `ParseException`（文案带响应字节数，和 Kotlin 一样）。
pub(crate) fn decode_page(bytes: &[u8]) -> Result<PageResponse, super::Error> {
    let mut res = PageResponse::default();
    pb::walk(bytes, |field, value| {
        match field {
            1 => res.error = Some(super::decode_error(pb::require_bytes(value)?)?),
            2 => res.data = Some(decode_page_data(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })
    .map_err(|e| pb_failure("帖子", bytes.len(), e))?;
    Ok(res)
}

/// `pb/floor` 的响应字节。
pub(crate) fn decode_floor(bytes: &[u8]) -> Result<FloorResponse, super::Error> {
    let mut res = FloorResponse::default();
    pb::walk(bytes, |field, value| {
        match field {
            1 => res.error = Some(super::decode_error(pb::require_bytes(value)?)?),
            2 => res.data = Some(decode_floor_data(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })
    .map_err(|e| pb_failure("楼中楼", bytes.len(), e))?;
    Ok(res)
}

/// 平台错误（`error_code` 非 0 才算）。
pub(crate) fn page_error(res: &PageResponse) -> Option<PlatformError> {
    envelope_error(&res.error)
}

pub(crate) fn floor_error(res: &FloorResponse) -> Option<PlatformError> {
    envelope_error(&res.error)
}

fn decode_page_data(buf: &[u8]) -> Result<PageData, PbError> {
    let mut data = PageData::default();
    pb::walk(buf, |field, value| {
        match field {
            2 => data.forum = Some(super::decode_simple_forum(pb::require_bytes(value)?)?),
            3 => data.page = Some(super::decode_page(pb::require_bytes(value)?)?),
            6 => data.post_list.push(decode_post(pb::require_bytes(value)?)?),
            8 => data.thread = Some(super::decode_thread(pb::require_bytes(value)?)?),
            13 => data.user_list.push(super::decode_user(pb::require_bytes(value)?)?),
            37 => data.thread_freq_num = pb::as_i64(value),
            38 => data.first_floor_post = Some(decode_post(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(data)
}

fn decode_floor_data(buf: &[u8]) -> Result<FloorData, PbError> {
    let mut data = FloorData::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => data.page = Some(super::decode_page(pb::require_bytes(value)?)?),
            3 => data.post = Some(decode_post(pb::require_bytes(value)?)?),
            4 => data
                .subpost_list
                .push(decode_sub_post_list(pb::require_bytes(value)?)?),
            5 => data.thread = Some(super::decode_thread(pb::require_bytes(value)?)?),
            6 => data.forum = Some(super::decode_simple_forum(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(data)
}

fn decode_post(buf: &[u8]) -> Result<Post, PbError> {
    let mut post = Post::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => post.id = pb::as_u64(value),
            2 => post.title = pb::as_string(value),
            3 => post.floor = pb::as_u32(value) as i32,
            4 => post.time = pb::as_u32(value),
            5 => post.content.push(super::decode_content(pb::require_bytes(value)?)?),
            13 => post.sub_post_number = pb::as_u32(value) as i32,
            15 => post.sub_post_list = Some(decode_sub_post(pb::require_bytes(value)?)?),
            19 => post.author_id = pb::as_i64(value),
            23 => post.author = Some(super::decode_user(pb::require_bytes(value)?)?),
            37 => post.agree = Some(super::decode_agree(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(post)
}

fn decode_sub_post(buf: &[u8]) -> Result<SubPost, PbError> {
    let mut sub = SubPost::default();
    pb::walk(buf, |field, value| {
        if field == 2 {
            sub.sub_post_list
                .push(decode_sub_post_list(pb::require_bytes(value)?)?);
        }
        Ok(())
    })?;
    Ok(sub)
}

fn decode_sub_post_list(buf: &[u8]) -> Result<SubPostList, PbError> {
    let mut sub = SubPostList::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => sub.id = pb::as_u64(value),
            2 => sub.content.push(super::decode_content(pb::require_bytes(value)?)?),
            3 => sub.time = pb::as_u32(value),
            4 => sub.author_id = pb::as_i64(value),
            7 => sub.author = Some(super::decode_user(pb::require_bytes(value)?)?),
            9 => sub.agree = Some(super::decode_agree(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(sub)
}

