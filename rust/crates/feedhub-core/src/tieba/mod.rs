//! 贴吧：推荐流 / 关注流用到的**纯解析**，以及 App 侧签名（`TiebaSign`）。
//!
//! 逐条对照 Kotlin：
//! - `TiebaParser.personalized`（推荐流，`/c/f/excellent/personalized` 的 protobuf 响应）
//! - `TiebaParser.frsPage`（吧内帖子列表 `/c/f/frs/page`，关注流每个吧的一页）
//! - `TiebaParser.thread` / `origin` / `user` / `media` / `plainTextWithEmotes` / `abstractText`
//! - `TiebaParser.followedForums`（关注的吧，`forumGuide` 的 JSON）
//! - `TiebaSign.compute` / `signed`
//!
//! -详情 / 楼层 / 楼中楼：`detail.rs`（`pb/page` / `pb/floor` 的响应 → 统一模型）、
//!   `content.rs`（内容段 → 富文本）、`meme.rs`（个人表情包记号）。
//!
//! **不搬**：搜索、扫码、视频（`play` 只是复用 `detail` 的"主视频"信息，见 `detail::MainVideoOutcome`）。
//! 关注流的合并 / 新鲜度 / 存储（`ForumMerge` / `ForumFreshness` / `ForumStore`）有状态，也留在 Kotlin，
//! 只把它们调用的这两个解析换成 Rust。
//!
//! 为什么用 `serde_json::Value`（和 B站 一样）而不是派生结构体：当前的验收标准是"和 Kotlin 逐字段相等"，
//! 容错语义（数字写成字符串、缺字段、null）复刻起来最稳。

pub mod content;
pub mod detail;
pub mod device;
// 关注流的调度 + 存盘：
// 惰性 k 路归并、哪些吧该重抓、`gz1:` + base64(gzip(JSON)) 的存盘格式。
pub mod forum;
// 关注流的整条链路（Kotlin `TiebaSource.following`）：吧列表 TTL、时间预算、归并、存盘。
pub mod following;
pub mod meme;
pub mod pb;
pub mod play;
pub mod qr;
pub mod search;
pub mod sign;
pub mod source;

use std::collections::HashMap;

use crate::json::{self, JsonMap};
use crate::model::{
    BiliError, DetailTarget, FeedItem, ItemKind, Media, Stats, TimePrecision, UserRef,
};

/// 贴吧的平台 id（`TiebaPlugin` 里 `Platform.define("tieba", …)`）。定了就别改。
pub const TIEBA: &str = "tieba";

/// 贴吧官方表情的「中文名 ↔ 官方文件名（`image_emoticon<n>`）」对照表。
///
/// **谁在用**：搜索结果的摘要里只有文字（`content` 是纯文本，`[滑稽]` 这种代码原样在里面），
/// 要把它画成表情图就需要「名字 → 文件名」；另外 `content` 里真出现
/// `<img src="…image_emoticon25.png">` 时反过来查名字当文字代码。
///
/// **表只覆盖真见过的表情**（17 个）：数据全部来自真实录到的 protobuf 响应里表情段的
/// `text` + `c` 两个字段，由 `tools/tieba-emoticons/gen.py` 生成，**不要手改**。
/// 表里没有的名字**原样保留**（不猜：`[受虐滑稽]` 这种是用户手打的文字，不是贴吧表情）。
///
/// 和 Kotlin 的 `TiebaEmoticons` 是同一份表（顺序也一样：`used()` 的输出顺序按插入序）。
pub const EMOTICONS: [(&str, &str); 17] = [
    ("呵呵", "image_emoticon1"),
    ("怒", "image_emoticon6"),
    ("汗", "image_emoticon8"),
    ("泪", "image_emoticon9"),
    ("黑线", "image_emoticon10"),
    ("不高兴", "image_emoticon12"),
    ("阴险", "image_emoticon16"),
    ("笑眼", "image_emoticon22"),
    ("太开心", "image_emoticon24"),
    ("滑稽", "image_emoticon25"),
    ("乖", "image_emoticon28"),
    ("小乖", "image_emoticon66"),
    ("捂嘴笑", "image_emoticon67"),
    ("你懂的", "image_emoticon68"),
    ("what", "image_emoticon69"),
    ("微微一笑", "image_emoticon91"),
    ("欢呼", "image_emoticon95"),
];

/// [`EMOTICONS`] 的一份借用视图（插入序，和 Kotlin 的 `LinkedHashMap` 一样）。
pub fn memoised_emoticons() -> &'static [(&'static str, &'static str)] {
    &EMOTICONS
}

/// 这一层的错误类型：复用 `model::BiliError`（它其实是中立的 `SourceException` 分类，只是名字带 Bili）。
/// 不改名、不改字段（B站 那一期定的），这里只起个别名让贴吧代码读起来顺。
pub type Error = BiliError;

/// HTTP 403 / 412 / 429 是风控信号：立刻停，不重试、不跳到下一个吧接着发（项目红线）。
/// 原来这几个状态码落进"非 2xx → `Api`"，关注流调度会把它当"这个吧读不了"继续请求别的吧。
pub(crate) fn risk_status(status: u16) -> Option<BiliError> {
    matches!(status, 403 | 412 | 429).then(|| BiliError::RiskControl {
        code: Some(format!("HTTP {status}")),
        detail: "请求被限制，先别请求了".to_string(),
    })
}

/// protobuf 解码失败怎么翻成错误：和 Kotlin 的 `TiebaParser.decode` 一模一样（带响应字节数）。
pub(crate) fn pb_failure(what: &str, bytes_len: usize, e: pb::PbError) -> Error {
    let _ = e;
    Error::Parse {
        detail: format!("{what} 响应不是合法的 protobuf（{bytes_len} 字节）"),
    }
}

pub(crate) fn parse_failure(detail: impl Into<String>) -> Error {
    Error::Parse {
        detail: detail.into(),
    }
}

// ===========================================================================
// 对外结果
// ===========================================================================

/// 平台错误（响应里那层 `Error`）：`error_code` 非 0。**不在这里归类**——归类的规则
/// （哪些算未登录、哪些算风控、110001 是签名不对）在 Kotlin 的 `TiebaSource.mapError` 里，
/// 那边还有搜索 / 关注管理等别的调用方，抄一份到 Rust 只会漂移。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformError {
    pub code: i32,
    pub user_msg: String,
    pub error_msg: String,
}

/// 一页解析结果。推荐流给 `next`，吧内列表给 `last`；`error` 非空时 `items` 是空的
/// （和 Kotlin 一样：先看错误码，再碰数据）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    pub items: Vec<FeedItem>,
    pub next: Option<String>,
    pub last: bool,
    pub error: Option<PlatformError>,
}

/// 关注的一个吧（`forumGuide` 的 `like_forum[]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedForum {
    pub id: i64,
    pub name: String,
    pub day_threads: i32,
    pub avatar: Option<String>,
    pub member_count: Option<i64>,
    pub thread_count: Option<i64>,
}

// ===========================================================================
// 入口
// ===========================================================================

/// 推荐流：`PersonalizedResponse` → 一页。`pn` 是这一页的页码（从 1 开始），下一页永远是 `pn + 1`。
pub fn parse_personalized(bytes: &[u8], pn: i32) -> Result<Feed, Error> {
    let res = decode_personalized_response(bytes)?;
    if let Some(err) = envelope_error(&res.error) {
        return Ok(Feed {
            items: Vec::new(),
            next: None,
            last: false,
            error: Some(err),
        });
    }
    let data = res
        .data
        .ok_or_else(|| parse_failure("个性推荐响应缺少 data"))?;
    let items: Vec<FeedItem> = data
        .thread_list
        .iter()
        .filter_map(|t| thread(t, None, &HashMap::new()))
        .collect();
    Ok(Feed {
        items,
        next: Some((pn + 1).to_string()),
        last: false,
        error: None,
    })
}

/// 吧内帖子列表的一页 → 关注流每个吧的缓冲。
///
/// - 置顶帖（`isTop` / `isMemberTop`）丢掉：它们永远排在第一页最前面，和发帖时间无关；
/// - 夹在列表里的直播推广卡片（`ala_info`）、已删除、没有 tid / 发帖时间的，丢掉；
/// - 其余按发帖时间降序（同秒按 tid 降序）、按 id 去重；
/// - 作者在响应级的 `user_list` 里（`thread_list[].author` 是空的），按 `authorId` 查；吧名用调用方给的
///   （响应里的 `forumName` 字段是空的）；
/// - `last`：服务端 `page.has_more == 0`，或者这一页什么帖子（含置顶和推广卡）都没有。
pub fn parse_frs_page(bytes: &[u8], forum_name: &str) -> Result<Feed, Error> {
    let res = decode_frs_response(bytes)?;
    if let Some(err) = envelope_error(&res.error) {
        return Ok(Feed {
            items: Vec::new(),
            next: None,
            last: false,
            error: Some(err),
        });
    }
    let data = res
        .data
        .ok_or_else(|| parse_failure("吧内帖子响应缺少 data"))?;
    // `associateBy { it.id }`：同 id 后面的覆盖前面的
    let users: HashMap<i64, User> = data
        .user_list
        .iter()
        .filter(|u| u.id != 0)
        .map(|u| (u.id, u.clone()))
        .collect();

    let mut items: Vec<FeedItem> = Vec::new();
    for t in &data.thread_list {
        if t.is_top != 0 || t.is_member_top != 0 {
            continue;
        }
        let Some(item) = thread(t, Some(forum_name), &users) else {
            continue;
        };
        if item.published_at.is_none() || item.id.parse::<i64>().is_err() {
            continue;
        }
        if items.iter().any(|x| x.id == item.id) {
            continue;
        }
        items.push(item);
    }
    // compareByDescending { publishedAt }.thenByDescending { id.toLong() }（Kotlin 的排序是稳定的，这里也是）
    items.sort_by(|a, b| {
        let at = a.published_at.unwrap_or(i64::MIN);
        let bt = b.published_at.unwrap_or(i64::MIN);
        bt.cmp(&at).then_with(|| {
            let ai = a.id.parse::<i64>().unwrap_or(0);
            let bi = b.id.parse::<i64>().unwrap_or(0);
            bi.cmp(&ai)
        })
    });
    let last = data.thread_list.is_empty() || data.page.as_ref().map(|p| p.has_more == 0) == Some(true);
    Ok(Feed {
        items,
        next: None,
        last,
        error: None,
    })
}

/// 关注的吧：`forumGuide` 响应。缺 id（<= 0）或吧名的条目丢掉，同 id 只留第一个。
pub fn followed_forums(json_body: &str) -> Result<Vec<FollowedForum>, Error> {
    let root: JsonMap = json::parse_object(json_body).map_err(parse_failure)?;
    followed_forums_object(&root)
}

/// 同上，但入参是**已经解析好**的 JSON（平台层只解析一遍，不再重新 parse 一次文本）。
pub fn followed_forums_object(root: &JsonMap) -> Result<Vec<FollowedForum>, Error> {
    let list = json::arr(root, "like_forum")
        .ok_or_else(|| parse_failure("forumGuide 响应缺少 like_forum"))?;
    let mut out: Vec<FollowedForum> = Vec::new();
    for entry in list {
        let Some(o) = entry.as_object() else { continue };
        let Some(id) = json::long(o, "forum_id").filter(|id| *id > 0) else {
            continue;
        };
        let Some(name) = json::text(o, "forum_name") else {
            continue;
        };
        if out.iter().any(|f| f.id == id) {
            continue;
        }
        out.push(FollowedForum {
            id,
            name,
            day_threads: json::int(o, "day_thread_num").unwrap_or(0),
            avatar: json::text(o, "avatar").map(|a| json::to_https(&a)),
            member_count: json::long(o, "member_count").filter(|n| *n >= 0),
            thread_count: json::long(o, "thread_num").filter(|n| *n >= 0),
        });
    }
    Ok(out)
}

/// 响应的 `Error` 里 `error_code` 非 0 才算错误（`error` 消息本身缺席就是没问题）。
pub(crate) fn envelope_error(error: &Option<ErrorMsg>) -> Option<PlatformError> {
    let e = error.as_ref()?;
    if e.code == 0 {
        return None;
    }
    Some(PlatformError {
        code: e.code,
        user_msg: e.user_msg.clone(),
        error_msg: e.error_msg.clone(),
    })
}

// ===========================================================================
// 帖子 → FeedItem（对照 `TiebaParser.thread` / `origin` / `user` / `media`）
// ===========================================================================

pub fn thread_url(tid: &str) -> String {
    format!("https://tieba.baidu.com/p/{tid}")
}

pub fn user_url(portrait: &str) -> String {
    format!("https://tieba.baidu.com/home/main?id={portrait}")
}

pub fn avatar_url(portrait: &str) -> String {
    format!("https://himg.bdimg.com/sys/portrait/item/{portrait}")
}

fn thread(t: &ThreadInfo, forum_name: Option<&str>, users: &HashMap<i64, User>) -> Option<FeedItem> {
    let id = if t.id != 0 {
        t.id
    } else if t.thread_id != 0 {
        t.thread_id
    } else {
        return None;
    };
    if t.ala_info.is_some() || t.twzhibo_info.is_some() || t.is_deleted != 0 {
        return None;
    }
    let tid = id.to_string();
    let url = thread_url(&tid);

    let rich = {
        let primary = plain_text_with_emotes(&t.rich_abstract);
        if primary.text.is_some() {
            primary
        } else if let Some(text) = abstract_text(&t.abstract_) {
            PlainText {
                text: Some(text),
                emotes: Vec::new(),
            }
        } else {
            plain_text_with_emotes(&t.first_post_content)
        }
    };

    let title = if t.is_no_title == 0 {
        let trimmed = kotlin_trim(&t.title);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    };

    let author = t
        .author
        .as_ref()
        .and_then(user)
        .or_else(|| users.get(&t.author_id).and_then(user));

    let community = {
        let first = if !is_blank(&t.forum_name) {
            t.forum_name.clone()
        } else {
            t.forum_info
                .as_ref()
                .map(|f| f.name.clone())
                .unwrap_or_default()
        };
        let second = if !is_blank(&first) {
            first
        } else {
            forum_name.unwrap_or("").to_string()
        };
        if is_blank(&second) {
            None
        } else {
            Some(second)
        }
    };

    Some(FeedItem {
        platform: TIEBA.to_string(),
        id: tid.clone(),
        kind: ItemKind::Post,
        title,
        summary: rich.text,
        language: None,
        author,
        published_at: (t.create_time > 0).then_some(t.create_time as i64),
        published_at_precision: TimePrecision::Second,
        media: media(&t.media, t.video_info.as_ref()),
        stats: Stats {
            views: (t.view_num > 0).then_some(t.view_num as i64),
            // `t.agree?.agreeNum ?: t.agreeNum.toLong()`：缺 Agree 消息时是 0，不是 null
            likes: Some(
                t.agree
                    .as_ref()
                    .map(|a| a.agree_num)
                    .unwrap_or(t.agree_num as i64),
            ),
            comments: Some(t.reply_num as i64),
            shares: (t.share_num > 0).then_some(t.share_num),
            ..Stats::default()
        },
        reason: None,
        community,
        community_icon: None,
        repost: t
            .origin_thread_info
            .as_ref()
            .filter(|_| t.is_share_thread != 0)
            .and_then(origin)
            .map(Box::new),
        emotes: rich.emotes,
        adult: false,
        url: Some(url),
        detail: DetailTarget::Native {
            id: tid,
            platform: None,
        },
    })
}

/// 转发帖里的原帖。
fn origin(o: &OriginThreadInfo) -> Option<FeedItem> {
    if is_blank(&o.tid) || o.tid == "0" {
        return None;
    }
    let tid = o.tid.clone();
    let url = thread_url(&tid);
    let rich = {
        let primary = plain_text_with_emotes(&o.content);
        if primary.text.is_some() {
            primary
        } else if let Some(text) = abstract_text(&o.abstract_) {
            PlainText {
                text: Some(text),
                emotes: Vec::new(),
            }
        } else {
            PlainText {
                text: None,
                emotes: Vec::new(),
            }
        }
    };
    let title = {
        let trimmed = kotlin_trim(&o.title);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    Some(FeedItem {
        platform: TIEBA.to_string(),
        id: tid.clone(),
        kind: ItemKind::Post,
        title,
        summary: rich.text,
        language: None,
        author: o.author.as_ref().and_then(user),
        published_at: None,
        published_at_precision: TimePrecision::Second,
        media: media(&o.media, o.video_info.as_ref()),
        stats: Stats {
            likes: o.agree.as_ref().map(|a| a.agree_num),
            comments: Some(o.reply_num as i64),
            ..Stats::default()
        },
        reason: None,
        community: if is_blank(&o.fname) {
            None
        } else {
            Some(o.fname.clone())
        },
        community_icon: None,
        repost: None,
        emotes: rich.emotes,
        adult: false,
        url: Some(url),
        detail: DetailTarget::Native {
            id: tid,
            platform: None,
        },
    })
}

pub(crate) fn user(u: &User) -> Option<UserRef> {
    let base = if !is_blank(&u.portrait) {
        u.portrait.as_str()
    } else {
        u.portraith.as_str()
    };
    let before_query = base.split('?').next().unwrap_or("");
    let trimmed = kotlin_trim(before_query);
    let portrait = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    let id = if u.id != 0 {
        Some(u.id.to_string())
    } else {
        portrait.clone()
    }?;
    let name = if !is_blank(&u.name_show) {
        u.name_show.as_str()
    } else {
        u.name.as_str()
    };
    if is_blank(name) {
        return None;
    }
    Some(UserRef {
        platform: TIEBA.to_string(),
        id,
        name: name.to_string(),
        avatar: portrait.as_deref().map(avatar_url),
        url: portrait.as_deref().map(user_url),
    })
}

/// 图片取中等尺寸（bigPic），没有再退到其他尺寸；视频只取封面和时长（插在列表最前面）。
fn media(list: &[MediaMsg], video: Option<&VideoInfo>) -> Vec<Media> {
    let mut out: Vec<Media> = Vec::new();
    for m in list {
        // `listOf(bigPic, dynamicPic, srcPic, originPic).firstOrNull { it.isNotBlank() }`
        let src = [&m.big_pic, &m.dynamic_pic, &m.src_pic, &m.origin_pic]
            .into_iter()
            .find(|s| !is_blank(s));
        let Some(src) = src else { continue };
        if video.is_some() && m.type_ == MEDIA_VIDEO {
            continue;
        }
        let width = m.width as i32;
        let height = m.height as i32;
        out.push(Media::Image {
            url: json::to_https(src),
            width: (width > 0).then_some(width),
            height: (height > 0).then_some(height),
        });
    }
    if let Some(v) = video {
        let duration = v.video_duration as i32;
        if !is_blank(&v.thumbnail_url) || duration > 0 {
            let cover = if is_blank(&v.thumbnail_url) {
                None
            } else {
                Some(json::to_https(&v.thumbnail_url))
            };
            out.insert(
                0,
                Media::Video {
                    cover,
                    duration_sec: (duration > 0).then_some(duration),
                    // 宽高在信息流里就有：打开详情时直接按竖屏 / 横屏摆，不用等流回来再换界面
                    width: (v.video_width > 0).then_some(v.video_width as i32),
                    height: (v.video_height > 0).then_some(v.video_height as i32),
                },
            );
        }
    }
    out
}

const MEDIA_VIDEO: i32 = 5;

// ===========================================================================
// 富文本 → 纯文本 + 表情（对照 `TiebaParser.plainTextWithEmotes` / `abstractText`）
// ===========================================================================

/// 表情图片的地址前缀。`static.tieba.baidu.com` 的证书不覆盖那个域名，同一批文件在 `tb1.bdstatic.com` 上正常。
const EMOTE_BASE: &str = "https://tb1.bdstatic.com/tb/editor/images/client/";
const EMOTE_PREFIX: &str = "image_emoticon";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlainText {
    pub text: Option<String>,
    pub emotes: Vec<(String, String)>,
}

/// 富文本 → 纯文本：文字、链接文字、@某人、话题原样保留；表情写成 `[名字]`；图片、视频、语音丢掉。
/// `PbContent.type`：0 文字、1 链接、2 表情、3 图片、4 @、5 视频、9 电话号码、10 语音、18 话题、20 表情包、40 文字（新版）。
fn plain_text_with_emotes(list: &[Content]) -> PlainText {
    let mut table: Vec<(String, String)> = Vec::new();
    let mut text = String::new();
    for it in list {
        match it.type_ {
            0 | 1 | 4 | 9 | 18 | 40 => text.push_str(&it.text),
            2 => {
                let name = if !is_blank(&it.c) {
                    it.c.clone()
                } else {
                    it.text.clone()
                };
                if !is_blank(&name) {
                    let code = format!("[{name}]");
                    if let Some(url) = emote_url(it) {
                        // `putIfAbsent`：同一个代码只记第一次的地址
                        if !table.iter().any(|(k, _)| *k == code) {
                            table.push((code.clone(), url));
                        }
                    }
                    text.push_str(&code);
                }
            }
            _ => {}
        }
    }
    let normalized = normalize(&text);
    let emotes = emotes_used(&table, normalized.as_deref());
    PlainText {
        text: normalized,
        emotes,
    }
}

/// 老式摘要（`Abstract`）：0 文字、4 @。
fn abstract_text(list: &[Abstract]) -> Option<String> {
    let text: String = list
        .iter()
        .filter(|a| a.type_ == 0 || a.type_ == 4)
        .map(|a| a.text.as_str())
        .collect();
    normalize(&text)
}

/// 表情段（type 2）的官方图片地址；`text` 不是 `image_emoticon<n>` 这种官方表情就是 `None`。
fn emote_url(c: &Content) -> Option<String> {
    let code = c.text.as_str();
    let rest = code.strip_prefix(EMOTE_PREFIX)?;
    if !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    // `image_emoticon` 不带数字就是第 1 个（TiebaLite `registerEmoticon`）
    let id = if code == EMOTE_PREFIX {
        "image_emoticon1"
    } else {
        code
    };
    Some(format!("{EMOTE_BASE}{id}.png"))
}

/// 对应 `Emotes.used`：只留文字里真的出现过的代码，顺序就是表的插入顺序。
fn emotes_used(table: &[(String, String)], text: Option<&str>) -> Vec<(String, String)> {
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return Vec::new();
    };
    table
        .iter()
        .filter(|(code, url)| !code.is_empty() && !url.is_empty() && text.contains(code.as_str()))
        .cloned()
        .collect()
}

/// `replace(Regex("[ \\t\\u3000]{2,}"), " ").replace(Regex("\\n{3,}"), "\n\n").trim().ifEmpty { null }`
fn normalize(s: &str) -> Option<String> {
    let collapsed = collapse_spaces(s);
    let collapsed = collapse_newlines(&collapsed);
    let trimmed = kotlin_trim(&collapsed);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run: Vec<char> = Vec::new();
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' || ch == '\u{3000}' {
            run.push(ch);
            continue;
        }
        flush_spaces(&mut out, &run);
        run.clear();
        out.push(ch);
    }
    flush_spaces(&mut out, &run);
    out
}

/// 一个空白字符原样留着（究竟是空格、制表符还是全角空格），两个以上压成一个空格。
fn flush_spaces(out: &mut String, run: &[char]) {
    match run.len() {
        0 => {}
        1 => out.push(run[0]),
        _ => out.push(' '),
    }
}

fn collapse_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            run += 1;
            continue;
        }
        match run {
            0 => {}
            1 => out.push('\n'),
            _ => out.push_str("\n\n"),
        }
        run = 0;
        out.push(ch);
    }
    match run {
        0 => {}
        1 => out.push('\n'),
        _ => out.push_str("\n\n"),
    }
    out
}

// ===========================================================================
// 文本工具：和 Kotlin 的 `isBlank()` / `trim()` 同一个口径
// ===========================================================================

// 空白 / trim 的口径（Java `Character.isWhitespace`）在 `crate::hosted`；`pub(crate)` 转发，
// `tieba::is_blank` / `tieba::kotlin_trim` 这两个老路径照旧可用。
pub(crate) use crate::hosted::{is_blank, kotlin_trim};

/// Kotlin `String.trimStart()`：去掉开头的空白（和 [`kotlin_trim`] 同一个集合）。
pub(crate) fn trim_start(s: &str) -> &str {
    let start = s
        .char_indices()
        .find(|(_, ch)| !crate::hosted::is_java_whitespace(*ch))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[start..]
}

/// Kotlin `String.trimEnd()`：去掉结尾的空白。
pub(crate) fn trim_end(s: &str) -> &str {
    let end = s
        .char_indices()
        .rev()
        .find(|(_, ch)| !crate::hosted::is_java_whitespace(*ch))
        .map(|(i, ch)| i + ch.len_utf8())
        .unwrap_or(0);
    &s[..end]
}

// ===========================================================================
// protobuf 消息（只留我们读的字段，字段号照 `src/main/proto/*.proto`）
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub(crate) struct ErrorMsg {
    pub(crate) code: i32,
    pub(crate) error_msg: String,
    pub(crate) user_msg: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PageMsg {
    pub(crate) current_page: i32,
    pub(crate) total_page: i32,
    pub(crate) has_more: i32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Agree {
    pub(crate) agree_num: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SimpleForum {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) avatar: String,
    /// 当前登录账号是否已关注（匿名请求恒为 0）。
    pub(crate) is_like: i32,
    pub(crate) first_class: String,
    pub(crate) second_class: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct VideoInfo {
    pub(crate) video_url: String,
    pub(crate) video_duration: u32,
    pub(crate) video_width: u32,
    pub(crate) video_height: u32,
    pub(crate) thumbnail_url: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MediaMsg {
    type_: i32,
    big_pic: String,
    src_pic: String,
    origin_pic: String,
    dynamic_pic: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Content {
    pub(crate) type_: i32,
    pub(crate) text: String,
    pub(crate) link: String,
    pub(crate) src: String,
    pub(crate) bsize: String,
    pub(crate) big_cdn_src: String,
    pub(crate) cdn_src: String,
    pub(crate) c: String,
    pub(crate) during_time: u32,
    pub(crate) uid: i64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) origin_src: String,
    pub(crate) meme_info: Option<MemeInfo>,
    pub(crate) count: i32,
    pub(crate) portrait: String,
}

/// 个人表情包图片（`PbContent.meme_info`，type 20）。
#[derive(Debug, Clone, Default)]
pub(crate) struct MemeInfo {
    pub(crate) pic_id: i64,
    pub(crate) pic_url: String,
    pub(crate) thumbnail: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Abstract {
    type_: i32,
    text: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct User {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) name_show: String,
    pub(crate) portrait: String,
    pub(crate) portraith: String,
    pub(crate) ip_address: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OriginThreadInfo {
    title: String,
    media: Vec<MediaMsg>,
    abstract_: Vec<Abstract>,
    fname: String,
    tid: String,
    video_info: Option<VideoInfo>,
    content: Vec<Content>,
    reply_num: i32,
    author: Option<User>,
    agree: Option<Agree>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ThreadInfo {
    pub(crate) id: i64,
    pub(crate) thread_id: i64,
    pub(crate) title: String,
    pub(crate) reply_num: i32,
    pub(crate) view_num: i32,
    pub(crate) is_top: i32,
    pub(crate) author: Option<User>,
    pub(crate) abstract_: Vec<Abstract>,
    pub(crate) media: Vec<MediaMsg>,
    pub(crate) forum_name: String,
    pub(crate) is_member_top: i32,
    pub(crate) is_no_title: i32,
    pub(crate) author_id: i64,
    pub(crate) create_time: i32,
    pub(crate) twzhibo_info: Option<Empty>,
    pub(crate) video_info: Option<VideoInfo>,
    pub(crate) rich_abstract: Vec<Content>,
    pub(crate) ala_info: Option<Empty>,
    pub(crate) agree_num: i32,
    pub(crate) agree: Option<Agree>,
    pub(crate) share_num: i64,
    pub(crate) origin_thread_info: Option<OriginThreadInfo>,
    pub(crate) first_post_content: Vec<Content>,
    pub(crate) is_share_thread: i32,
    pub(crate) forum_info: Option<SimpleForum>,
    pub(crate) is_deleted: i32,
}

/// 只关心"出现过"的消息（`ala_info` / `twzhibo_info`）：解析成空结构，用来判 `!= null`。
#[derive(Debug, Clone, Default)]
pub(crate) struct Empty;

#[derive(Debug, Clone, Default)]
pub(crate) struct PersonalizedData {
    thread_list: Vec<ThreadInfo>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PersonalizedResponse {
    error: Option<ErrorMsg>,
    data: Option<PersonalizedData>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FrsData {
    page: Option<PageMsg>,
    thread_list: Vec<ThreadInfo>,
    user_list: Vec<User>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FrsResponse {
    error: Option<ErrorMsg>,
    data: Option<FrsData>,
}

// ---- 解码 ------------------------------------------------------------------

pub(crate) fn decode_personalized_response(bytes: &[u8]) -> Result<PersonalizedResponse, Error> {
    let mut res = PersonalizedResponse::default();
    pb::walk(bytes, |field, value| {
        match field {
            1 => res.error = Some(decode_error(pb::require_bytes(value)?)?),
            2 => res.data = Some(decode_personalized_data(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })
    .map_err(|e| pb_failure("个性推荐", bytes.len(), e))?;
    Ok(res)
}

pub(crate) fn decode_personalized_data(buf: &[u8]) -> Result<PersonalizedData, pb::PbError> {
    let mut data = PersonalizedData::default();
    pb::walk(buf, |field, value| {
        if field == 2 {
            data.thread_list.push(decode_thread(pb::require_bytes(value)?)?);
        }
        Ok(())
    })?;
    Ok(data)
}

pub(crate) fn decode_frs_response(bytes: &[u8]) -> Result<FrsResponse, Error> {
    let mut res = FrsResponse::default();
    pb::walk(bytes, |field, value| {
        match field {
            1 => res.error = Some(decode_error(pb::require_bytes(value)?)?),
            2 => res.data = Some(decode_frs_data(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })
    .map_err(|e| pb_failure("吧内帖子列表", bytes.len(), e))?;
    Ok(res)
}

pub(crate) fn decode_frs_data(buf: &[u8]) -> Result<FrsData, pb::PbError> {
    let mut data = FrsData::default();
    pb::walk(buf, |field, value| {
        match field {
            4 => data.page = Some(decode_page(pb::require_bytes(value)?)?),
            7 => data.thread_list.push(decode_thread(pb::require_bytes(value)?)?),
            17 => data.user_list.push(decode_user(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(data)
}

pub(crate) fn decode_error(buf: &[u8]) -> Result<ErrorMsg, pb::PbError> {
    let mut m = ErrorMsg::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.code = pb::as_i32(value),
            2 => m.error_msg = pb::as_string(value),
            3 => m.user_msg = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_page(buf: &[u8]) -> Result<PageMsg, pb::PbError> {
    let mut m = PageMsg::default();
    pb::walk(buf, |field, value| {
        match field {
            3 => m.current_page = pb::as_i32(value),
            5 => m.total_page = pb::as_i32(value),
            6 => m.has_more = pb::as_i32(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_agree(buf: &[u8]) -> Result<Agree, pb::PbError> {
    let mut m = Agree::default();
    pb::walk(buf, |field, value| {
        if field == 1 {
            m.agree_num = pb::as_i64(value);
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_simple_forum(buf: &[u8]) -> Result<SimpleForum, pb::PbError> {
    let mut m = SimpleForum::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.id = pb::as_i64(value),
            2 => m.name = pb::as_string(value),
            4 => m.avatar = pb::as_string(value),
            5 => m.is_like = pb::as_i32(value),
            7 => m.first_class = pb::as_string(value),
            8 => m.second_class = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_video_info(buf: &[u8]) -> Result<VideoInfo, pb::PbError> {
    let mut m = VideoInfo::default();
    pb::walk(buf, |field, value| {
        match field {
            2 => m.video_url = pb::as_string(value),
            3 => m.video_duration = pb::as_u32(value),
            4 => m.video_width = pb::as_u32(value),
            5 => m.video_height = pb::as_u32(value),
            6 => m.thumbnail_url = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_media(buf: &[u8]) -> Result<MediaMsg, pb::PbError> {
    let mut m = MediaMsg::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.type_ = pb::as_i32(value),
            3 => m.big_pic = pb::as_string(value),
            8 => m.src_pic = pb::as_string(value),
            10 => m.width = pb::as_u32(value),
            11 => m.height = pb::as_u32(value),
            15 => m.origin_pic = pb::as_string(value),
            18 => m.dynamic_pic = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_content(buf: &[u8]) -> Result<Content, pb::PbError> {
    let mut m = Content::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.type_ = pb::as_i32(value),
            2 => m.text = pb::as_string(value),
            3 => m.link = pb::as_string(value),
            4 => m.src = pb::as_string(value),
            5 => m.bsize = pb::as_string(value),
            8 => m.cdn_src = pb::as_string(value),
            9 => m.big_cdn_src = pb::as_string(value),
            11 => m.c = pb::as_string(value),
            13 => m.during_time = pb::as_u32(value),
            15 => m.uid = pb::as_i64(value),
            18 => m.width = pb::as_u32(value),
            19 => m.height = pb::as_u32(value),
            25 => m.origin_src = pb::as_string(value),
            28 => m.count = pb::as_i32(value),
            33 => m.meme_info = Some(decode_meme_info(pb::require_bytes(value)?)?),
            49 => m.portrait = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_meme_info(buf: &[u8]) -> Result<MemeInfo, pb::PbError> {
    let mut m = MemeInfo::default();
    pb::walk(buf, |field, value| {
        match field {
            2 => m.pic_id = pb::as_i64(value),
            3 => m.pic_url = pb::as_string(value),
            4 => m.thumbnail = pb::as_string(value),
            5 => m.width = pb::as_u32(value),
            6 => m.height = pb::as_u32(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_abstract(buf: &[u8]) -> Result<Abstract, pb::PbError> {
    let mut m = Abstract::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.type_ = pb::as_i32(value),
            2 => m.text = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_user(buf: &[u8]) -> Result<User, pb::PbError> {
    let mut m = User::default();
    pb::walk(buf, |field, value| {
        match field {
            2 => m.id = pb::as_i64(value),
            3 => m.name = pb::as_string(value),
            4 => m.name_show = pb::as_string(value),
            5 => m.portrait = pb::as_string(value),
            27 => m.portraith = pb::as_string(value),
            127 => m.ip_address = pb::as_string(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_empty(buf: &[u8]) -> Result<Empty, pb::PbError> {
    // 只判"出现过"：把嵌套消息完整走一遍，坏消息照样报错（Wire 也是）
    pb::walk(buf, |_, _| Ok(()))?;
    Ok(Empty)
}

pub(crate) fn decode_origin(buf: &[u8]) -> Result<OriginThreadInfo, pb::PbError> {
    let mut m = OriginThreadInfo::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.title = pb::as_string(value),
            2 => m.media.push(decode_media(pb::require_bytes(value)?)?),
            3 => m.abstract_.push(decode_abstract(pb::require_bytes(value)?)?),
            4 => m.fname = pb::as_string(value),
            5 => m.tid = pb::as_string(value),
            13 => m.video_info = Some(decode_video_info(pb::require_bytes(value)?)?),
            14 => m.content.push(decode_content(pb::require_bytes(value)?)?),
            16 => m.reply_num = pb::as_i32(value),
            18 => m.author = Some(decode_user(pb::require_bytes(value)?)?),
            19 => m.agree = Some(decode_agree(pb::require_bytes(value)?)?),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}

pub(crate) fn decode_thread(buf: &[u8]) -> Result<ThreadInfo, pb::PbError> {
    let mut m = ThreadInfo::default();
    pb::walk(buf, |field, value| {
        match field {
            1 => m.id = pb::as_i64(value),
            2 => m.thread_id = pb::as_i64(value),
            3 => m.title = pb::as_string(value),
            4 => m.reply_num = pb::as_i32(value),
            5 => m.view_num = pb::as_i32(value),
            9 => m.is_top = pb::as_i32(value),
            18 => m.author = Some(decode_user(pb::require_bytes(value)?)?),
            21 => m.abstract_.push(decode_abstract(pb::require_bytes(value)?)?),
            22 => m.media.push(decode_media(pb::require_bytes(value)?)?),
            28 => m.forum_name = pb::as_string(value),
            38 => m.is_no_title = pb::as_i32(value),
            45 => m.create_time = pb::as_i32(value),
            54 => m.is_member_top = pb::as_i32(value),
            56 => m.author_id = pb::as_i64(value),
            72 => m.twzhibo_info = Some(decode_empty(pb::require_bytes(value)?)?),
            79 => m.video_info = Some(decode_video_info(pb::require_bytes(value)?)?),
            112 => m.rich_abstract.push(decode_content(pb::require_bytes(value)?)?),
            113 => m.ala_info = Some(decode_empty(pb::require_bytes(value)?)?),
            124 => m.agree_num = pb::as_i32(value),
            126 => m.agree = Some(decode_agree(pb::require_bytes(value)?)?),
            135 => m.share_num = pb::as_i64(value),
            141 => m.origin_thread_info = Some(decode_origin(pb::require_bytes(value)?)?),
            142 => m.first_post_content.push(decode_content(pb::require_bytes(value)?)?),
            143 => m.is_share_thread = pb::as_i32(value),
            155 => m.forum_info = Some(decode_simple_forum(pb::require_bytes(value)?)?),
            181 => m.is_deleted = pb::as_i32(value),
            _ => {}
        }
        Ok(())
    })?;
    Ok(m)
}
