//! 贴吧**内容段 → 富文本**：帖子正文 / 楼层 / 楼中楼的 `PbContent` 列表 → [`RichText`]。
//! 逐条对照 `source-tieba/TiebaContent.kt`（那儿的注释里有类型表出处：aiotieba Unlicense 的
//! `api/get_posts/_classdef.py` 和 TiebaLite GPL-3.0 的 `api/models/protos/Extensions.kt`，
//! 再以真实录到的响应为准）。
//!
//! | type | 含义 | 映射 |
//! |---|---|---|
//! | 0 / 9 / 27 / 35 / 36 / 37 / 40 | 文字、电话、百科词条、贴吧 plus 文字、梗百科 | [`Span::Text`]（内部还有个人表情包记号，见 [`meme`]） |
//! | 1 | 链接（`text` 显示文字，`link` 地址） | [`Span::Link`]，是贴吧帖子链接的带 `target` |
//! | 2 / 11 | 表情（`text` = `image_emoticon25`，`c` = 中文名） | [`Span::Emote`]（官方图片地址），认不出图片地址就退回文字 `[滑稽]` |
//! | 3 / 20 | 图片 / 表情包图片（`meme_info.picUrl`） | [`Block::Image`] |
//! | 4 | @某人（`text` 带 @，`uid`） | [`Span::Mention`] |
//! | 5 | 视频（`src` 封面、`link` mp4、`text` 网页版播放页、`during_time` 秒、`count` 播放次数） | [`Block::Video`]（有 mp4 地址）/ [`Block::Card`]（只有网页） |
//! | 10 | 语音 | 文字 `[语音]` |
//! | 18 | 话题（`text` = `#话题#`，`link` = `mob/q/hotMessage?…`） | [`Span::Link`] |
//! | 34 / 52 | 过期的贴吧 plus、投票 | 丢掉 |
//! | 其他 | 没见过的类型 | 有 `text` 就当文字，否则丢掉 |
//!
//! 换行：`\n` 切段（和 core 的 HTML 转换一致），连续空行不产生空段。
//!
//! 三个和 Kotlin 对齐的口径：
//! 1. 一行的首尾**只 trim 文字段**（图片 / 表情 / 链接不动），整行空白就丢掉；
//! 2. 一楼的**第一个有 mp4 地址的视频**是整条帖子的主视频（[`MainVideo`]），不进正文；
//! 3. 站外链接被贴吧套了一层 `tieba.baidu.com/mo/q/checkurl?url=…` 跳转，剥掉给真实地址。

use crate::detail::{Block, RichText, Span};
use crate::json;
use crate::model::{DetailTarget, UserRef};

use super::meme;
use super::{avatar_url, is_blank, kotlin_trim, trim_end, trim_start, user_url, Content, TIEBA};

/// 官方表情图片：`image_emoticon1` ~ `image_emoticon137` 这类。**不能直接改成 https 的那个域名**：
/// 实测 `static.tieba.baidu.com` 的证书不覆盖这个域名，https 连不上；同一批文件在 `tb1.bdstatic.com` 上正常。
pub(crate) const EMOTE_BASE: &str = "https://tb1.bdstatic.com/tb/editor/images/client/";
const EMOTE_PREFIX: &str = "image_emoticon";

/// 一楼里的主视频（Kotlin `TiebaContent.MainVideo`）：整条帖子的播放器放在页面顶上，正文里不再重复。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainVideo {
    pub url: String,
    pub cover: Option<String>,
    pub duration_sec: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 正文分 P 的 id：整条帖子的主视频（`TiebaVideo.MAIN`）。
pub(crate) const MAIN_PART: &str = "main";
/// 楼层 / 正文里夹着的视频：`partId` 直接带着 mp4 地址，播放时不再请求。
pub(crate) const INLINE_PREFIX: &str = "url:";

/// 一个文字段（普通 `Span::Text`）。
fn text_span(text: impl Into<String>) -> Span {
    Span::Text {
        text: text.into(),
        bold: false,
        italic: false,
    }
}

/// 非空白才有值（Kotlin `ifBlank { null }`）。
fn non_blank(s: &str) -> Option<String> {
    if is_blank(s) {
        None
    } else {
        Some(s.to_string())
    }
}

/// 表情段（type 2）的官方图片地址；`text` 不是 `image_emoticon<n>` 这种官方表情就是 `None`。
pub(crate) fn emote_url(c: &Content) -> Option<String> {
    emote_url_of(&c.text)
}

/// 表情段 / HTML `<img>` 里的 `image_emoticon<n>`（不带数字就是第 1 个）→ 官方图片地址；别的返回 `None`。
///
/// Kotlin `EMOTE_ID = Regex("image_emoticon\\d*")` + `matches`（整串匹配）。
pub(crate) fn emote_url_of(code: &str) -> Option<String> {
    let rest = code.strip_prefix(EMOTE_PREFIX)?;
    if !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = if code == EMOTE_PREFIX {
        "image_emoticon1"
    } else {
        code
    };
    Some(format!("{EMOTE_BASE}{id}.png"))
}

/// 正常显示用 960 宽的大图（动图也是），没有再退到小图、原图。个人表情包（type 20）的地址在 `meme_info.picUrl` 里。
pub(crate) fn image_url(c: &Content) -> Option<String> {
    meme_image_url(c).or_else(|| {
        [&c.big_cdn_src, &c.cdn_src, &c.src, &c.origin_src]
            .into_iter()
            .find(|s| !is_blank(s))
            .map(|s| json::to_https(s))
    })
}

/// 个人表情包图片（type 20 的 `meme_info.picUrl`）；不是这种段、或者响应里没给就是 `None`。
fn meme_image_url(c: &Content) -> Option<String> {
    c.meme_info
        .as_ref()
        .and_then(|m| non_blank(&m.pic_url))
        .map(|u| json::to_https(&u))
}

/// 同一段内容里的人表情包图片：`picId` → 图片地址（缩略图优先）。拿不到图片地址的（`meme_info` 空的段）不进表。
/// 用 `Vec` 而不是 `HashMap`：要保留插入顺序，而且 `putIfAbsent`（同 id 只留第一个）。
pub(crate) fn meme_urls(list: &[Content]) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for c in list {
        if c.type_ != 20 {
            continue;
        }
        let Some(info) = c.meme_info.as_ref() else {
            continue;
        };
        if info.pic_id == 0 || is_blank(&info.pic_url) {
            continue;
        }
        let url = if is_blank(&info.thumbnail) {
            json::to_https(&info.pic_url)
        } else {
            json::to_https(&info.thumbnail)
        };
        if !out.iter().any(|(id, _)| *id == info.pic_id) {
            out.push((info.pic_id, url));
        }
    }
    out
}

/// 一段文字 → [`Span`]：把里面的个人表情包记号换成图（[memes] 里有这张图）或 [`meme::LABEL`]。
/// 没有记号时就是一个 [`Span::Text`]（和原来一模一样）。
pub(crate) fn meme_spans(text: &str, memes: &[(i64, String)]) -> Vec<Span> {
    let hits = meme::scan(text);
    if hits.is_empty() {
        return vec![text_span(text)];
    }
    let mut out: Vec<Span> = Vec::with_capacity(hits.len() * 2 + 1);
    let mut last = 0usize;
    for hit in &hits {
        if hit.start > last {
            out.push(text_span(&text[last..hit.start]));
        }
        match memes.iter().find(|(id, _)| *id == hit.reference.pic_id) {
            Some((_, url)) => out.push(Span::Emote {
                text: meme::LABEL.to_string(),
                url: url.clone(),
            }),
            None => out.push(text_span(meme::LABEL)),
        }
        last = hit.end;
    }
    if last < text.len() {
        out.push(text_span(&text[last..]));
    }
    out
}

/// 一段文字塞进当前行：`\n` 切段（每段先 flush 上一段），`\r` 换成空格。
fn push_text(line: &mut Vec<Span>, blocks: &mut Vec<Block>, s: &str, memes: &[(i64, String)]) {
    let replaced = s.replace('\r', " ");
    let parts: Vec<&str> = replaced.split('\n').collect();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            flush(line, blocks);
        }
        if !part.is_empty() {
            line.extend(meme_spans(part, memes));
        }
    }
}

/// 把当前行收成一个段落：首尾只 trim 文字段，空文字段丢掉，整行都空了就什么也不加。
fn flush(line: &mut Vec<Span>, blocks: &mut Vec<Block>) {
    if line.is_empty() {
        return;
    }
    let mut spans = std::mem::take(line);
    if let Span::Text { text, bold, italic } = spans[0].clone() {
        spans[0] = Span::Text {
            text: trim_start(&text).to_string(),
            bold,
            italic,
        };
    }
    let last = spans.len() - 1;
    if let Span::Text { text, bold, italic } = spans[last].clone() {
        spans[last] = Span::Text {
            text: trim_end(&text).to_string(),
            bold,
            italic,
        };
    }
    spans.retain(|s| !matches!(s, Span::Text { text, .. } if text.is_empty()));
    if !spans.is_empty() {
        blocks.push(Block::Paragraph { spans });
    }
}

/// 内容段列表 → 富文本。
///
/// [`skip_main_video`]：跳过第一个有 mp4 地址的视频段（一楼的主视频，前端把播放器放在页面顶上，
/// 再在正文里出现一次就是重复）。
pub(crate) fn rich_text(list: &[Content], skip_main_video: bool) -> RichText {
    let memes = meme_urls(list);
    let mut blocks: Vec<Block> = Vec::new();
    let mut line: Vec<Span> = Vec::new();
    let mut skip_video = skip_main_video;

    for c in list {
        match c.type_ {
            0 | 9 | 27 | 35 | 36 | 37 | 40 => push_text(&mut line, &mut blocks, &c.text, &memes),
            1 => {
                let raw = if is_blank(&c.link) {
                    c.text.clone()
                } else {
                    c.link.clone()
                };
                let url = unwrap_redirect(&raw);
                if is_blank(&url) {
                    push_text(&mut line, &mut blocks, &c.text, &memes);
                } else {
                    let text = if is_blank(&c.text) {
                        url.clone()
                    } else {
                        c.text.clone()
                    };
                    line.push(Span::Link {
                        text,
                        target: thread_target(&url),
                        url: json::to_https(&url),
                    });
                }
            }
            2 | 11 => {
                if let Some(span) = emote(c) {
                    line.push(span);
                }
            }
            3 | 20 => {
                if let Some(block) = image(c) {
                    flush(&mut line, &mut blocks);
                    blocks.push(block);
                }
            }
            4 => match mention(c) {
                Some(span) => line.push(span),
                None => push_text(&mut line, &mut blocks, &c.text, &memes),
            },
            5 => {
                if skip_video && !is_blank(&c.link) {
                    skip_video = false;
                } else if let Some(block) = video(c) {
                    flush(&mut line, &mut blocks);
                    blocks.push(block);
                }
            }
            10 => push_text(&mut line, &mut blocks, "[语音]", &memes),
            18 => {
                if is_blank(&c.link) {
                    push_text(&mut line, &mut blocks, &c.text, &memes);
                } else {
                    line.push(Span::Link {
                        text: c.text.clone(),
                        url: json::to_https(&c.link),
                        target: None,
                    });
                }
            }
            34 | 52 => {}
            _ => {
                if !is_blank(&c.text) {
                    push_text(&mut line, &mut blocks, &c.text, &memes);
                }
            }
        }
    }
    flush(&mut line, &mut blocks);
    RichText { blocks }
}

/// 一个表情段（type 2 / 11）。
fn emote(c: &Content) -> Option<Span> {
    let name = non_blank(&c.c);
    match (emote_url(c), &name) {
        (Some(url), _) => Some(Span::Emote {
            text: format!("[{}]", name.as_deref().unwrap_or(c.text.as_str())),
            url,
        }),
        (None, Some(name)) => Some(text_span(format!("[{name}]"))),
        (None, None) => {
            if is_blank(&c.text) {
                None
            } else {
                Some(text_span(c.text.clone()))
            }
        }
    }
}

/// 图片段（type 3 / 20）。
fn image(c: &Content) -> Option<Block> {
    let url = image_url(c)?;
    if let Some(info) = c.meme_info.as_ref() {
        if !is_blank(&info.pic_url) {
            // 个人表情包：宽高在 meme_info 里（`bsize` / `width` 可能是空的）
            let full = json::to_https(&info.pic_url);
            let thumb = if is_blank(&info.thumbnail) {
                None
            } else {
                Some(json::to_https(&info.thumbnail)).filter(|t| *t != full)
            };
            return Some(Block::Image {
                url: thumb.clone().unwrap_or_else(|| full.clone()),
                width: (info.width > 0)
                    .then_some(info.width as i32)
                    .or_else(|| (c.width > 0).then_some(c.width as i32)),
                height: (info.height > 0)
                    .then_some(info.height as i32)
                    .or_else(|| (c.height > 0).then_some(c.height as i32)),
                original: thumb.map(|_| full),
                caption: None,
            });
        }
    }
    let original = non_blank(&c.origin_src)
        .map(|s| json::to_https(&s))
        .filter(|s| *s != url);
    let (w, h) = bsize_size(&c.bsize);
    Some(Block::Image {
        url,
        width: w.or_else(|| (c.width > 0).then_some(c.width as i32)),
        height: h.or_else(|| (c.height > 0).then_some(c.height as i32)),
        original,
        caption: None,
    })
}

/// `bsize`（`"560,509"`）→ 宽高；两样都要有才算，否则都不给（Kotlin `size(bsize)`）。
fn bsize_size(bsize: &str) -> (Option<i32>, Option<i32>) {
    let before = bsize.split_once(',').map(|(a, _)| a).unwrap_or("");
    let after = bsize.split_once(',').map(|(_, b)| b).unwrap_or("");
    let w = kotlin_trim(before).parse::<i32>().ok().filter(|v| *v > 0);
    let h = kotlin_trim(after).parse::<i32>().ok().filter(|v| *v > 0);
    match (w, h) {
        (Some(w), Some(h)) => (Some(w), Some(h)),
        _ => (None, None),
    }
}

/// `@某人` 段（type 4）。`uid` 是 0 或名字是空的就不认（上层退回文字）。
pub(crate) fn mention(c: &Content) -> Option<Span> {
    if c.uid == 0 {
        return None;
    }
    let name = c.text.strip_prefix('@').unwrap_or(&c.text);
    if is_blank(name) {
        return None;
    }
    let portrait = portrait_of(&c.portrait);
    Some(Span::Mention {
        text: c.text.clone(),
        user: UserRef {
            platform: TIEBA.to_string(),
            id: c.uid.to_string(),
            name: name.to_string(),
            avatar: portrait.as_deref().map(avatar_url),
            url: portrait.as_deref().map(user_url),
        },
    })
}

/// `portrait` 去掉 `?t=…` 之后（空白算没有）。
fn portrait_of(raw: &str) -> Option<String> {
    let before = raw.split_once('?').map(|(a, _)| a).unwrap_or(raw);
    non_blank(before)
}

/// 一楼里的主视频（第一个有 mp4 地址的视频段）。
pub(crate) fn main_video(list: &[Content]) -> Option<MainVideo> {
    let c = list.iter().find(|c| c.type_ == 5 && !is_blank(&c.link))?;
    Some(MainVideo {
        url: json::to_https(&c.link),
        cover: non_blank(&c.src).map(|s| json::to_https(&s)),
        duration_sec: (c.during_time > 0).then_some(c.during_time as i32),
        width: (c.width > 0).then_some(c.width as i32),
        height: (c.height > 0).then_some(c.height as i32),
    })
}

/// 正文 / 楼层里夹着的视频 / 只有播放页的卡片。
fn video(c: &Content) -> Option<Block> {
    let cover = non_blank(&c.src).map(|s| json::to_https(&s));
    let mp4 = non_blank(&c.link).map(|s| json::to_https(&s));
    let page = non_blank(&c.text).map(|s| json::to_https(&s));
    if cover.is_none() && mp4.is_none() && page.is_none() {
        return None;
    }
    if let Some(mp4) = mp4 {
        return Some(Block::Video {
            id: thread_id_of(&mp4).unwrap_or_else(|| "0".to_string()),
            part_id: inline_part(&mp4),
            cover,
            duration_sec: (c.during_time > 0).then_some(c.during_time as i32),
            width: (c.width > 0).then_some(c.width as i32),
            height: (c.height > 0).then_some(c.height as i32),
            title: None,
            url: page,
        });
    }
    // 只有网页版播放页（不是视频文件）：播不了，做成卡片复制链接
    let mut sub: Vec<String> = Vec::new();
    if c.during_time > 0 {
        sub.push(format!("{}:{:02}", c.during_time / 60, c.during_time % 60));
    }
    if c.count > 0 {
        sub.push(format!("播放 {} 次", c.count));
    }
    Some(Block::Card {
        title: "视频".to_string(),
        subtitle: (!sub.is_empty()).then(|| sub.join(" · ")),
        cover,
        url: page,
        target: None,
    })
}

/// 楼层 / 正文里夹着的视频的 `partId`：直接带着 mp4 地址。
pub(crate) fn inline_part(mp4: &str) -> String {
    format!("{INLINE_PREFIX}{mp4}")
}

/// 地址里的 `ptid` 就是帖子 tid。
pub(crate) fn thread_id_of(mp4: &str) -> Option<String> {
    let url = HttpUrl::parse(mp4)?;
    url.query_parameter("ptid").filter(|v| !is_blank(v))
}

/// 站内帖子链接 → 原生详情的 id（tid）。
///
/// Kotlin `THREAD_LINK = Regex("^https?://(?:tieba\\.baidu\\.com|tiebac\\.baidu\\.com)/p/(\\\\d+)")`：
/// 前缀锚定、只认这两个域名、`/p/` 后面至少一个数字。
pub(crate) fn thread_target(url: &str) -> Option<DetailTarget> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest
        .strip_prefix("tieba.baidu.com")
        .or_else(|| rest.strip_prefix("tiebac.baidu.com"))?;
    let rest = rest.strip_prefix("/p/")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some(DetailTarget::Native {
        id: digits,
        platform: None,
    })
}

/// 贴吧给站外链接套了一层跳转：`https://tieba.baidu.com/mo/q/checkurl?url=<真实地址>`，
/// 剥掉直接给真实地址。不是这种形式就原样返回。
///
/// Kotlin 用 OkHttp 的 `toHttpUrlOrNull()` + `queryParameter("url")`（query 里的百分号编码要解）；
/// 这里用一个只做这件事的极小 URL 解析（见 [`HttpUrl`]）。
pub(crate) fn unwrap_redirect(link: &str) -> String {
    let Some(url) = HttpUrl::parse(link) else {
        return link.to_string();
    };
    if url.host != "tieba.baidu.com" || url.path != "/mo/q/checkurl" {
        return link.to_string();
    }
    match url.query_parameter("url") {
        Some(value) if !is_blank(&value) => value,
        _ => link.to_string(),
    }
}

/// 楼中楼开头的"回复 xxx :"：实测三个内容段：`回复 `（文字）、人名（文字，带 `uid` 和 `portrait`）、
/// ` :正文`（文字）。认得出来就返回（被回复的人, 去掉前缀后的内容段），认不出来原样返回。
pub(crate) fn split_reply_prefix(list: &[Content]) -> (Option<UserRef>, Vec<Content>) {
    if list.len() < 3 {
        return (None, list.to_vec());
    }
    let (head, who, tail) = (&list[0], &list[1], &list[2]);
    if head.type_ != 0 || head.text != "回复 " {
        return (None, list.to_vec());
    }
    if tail.type_ != 0 || !(tail.text.starts_with(" :") || tail.text.starts_with(':')) {
        return (None, list.to_vec());
    }
    // 被回复的人被屏蔽 / 匿名时人名段是空的（type 4、text 空、uid 0，portrait 是十六进制编码的"贴吧用户_0000000"）：
    // 前缀照样去掉，只是没有 replyTo
    let masked = who.type_ == 4 && who.text.is_empty() && who.uid == 0;
    if !masked && (!(who.type_ == 0 || who.type_ == 4) || who.uid == 0) {
        return (None, list.to_vec());
    }
    let rest = if let Some(r) = tail.text.strip_prefix(" :") {
        r
    } else {
        &tail.text
    };
    let rest = if let Some(r) = rest.strip_prefix(':') {
        r
    } else {
        rest
    };
    let rest = trim_start(rest);
    let mut content: Vec<Content> = Vec::new();
    if !rest.is_empty() {
        let mut tail = tail.clone();
        tail.text = rest.to_string();
        content.push(tail);
    }
    content.extend_from_slice(&list[3..]);
    if masked {
        return (None, content);
    }
    let name = who.text.strip_prefix('@').unwrap_or(&who.text);
    if is_blank(name) {
        return (None, list.to_vec());
    }
    match mention(who) {
        Some(Span::Mention { user, .. }) => {
            let mut user = user;
            user.name = name.to_string();
            (Some(user), content)
        }
        _ => (None, content),
    }
}

// ===========================================================================
// 极小的 URL 解析（只做 checkurl 剥跳转 / 视频地址里取 ptid 这两件事）
// ===========================================================================

/// OkHttp `HttpUrl` 的一个子集：scheme + host（小写）+ path + query，**query 参数能按名字取出来**
/// （百分号解码；和 OkHttp 一样不解 `+`）。
///
/// 解析失败（没有 scheme、不是 http/https、没有 host、百分号转义不合法）就是 `None`——对应 Kotlin 的
/// `toHttpUrlOrNull()` 返回 null。只做这两处要的事，不追求把 OkHttp 的规范化规则全搬过来。
struct HttpUrl {
    host: String,
    path: String,
    query: Option<String>,
}

impl HttpUrl {
    fn parse(raw: &str) -> Option<HttpUrl> {
        let (scheme, rest) = raw.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let host_port = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };
        let host = if let Some(stripped) = host_port.strip_prefix('[') {
            // IPv6 字面量：`[::1]:80`
            let end = stripped.find(']')?;
            &host_port[..end + 2]
        } else {
            host_port.split(':').next().unwrap_or("")
        };
        if host.is_empty() {
            return None;
        }
        let host = host.to_ascii_lowercase();
        let mut tail = &rest[authority_end..];
        if let Some(i) = tail.find('#') {
            tail = &tail[..i];
        }
        let (path, query) = match tail.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (tail, None),
        };
        let path = if path.is_empty() { "/" } else { path };
        if !valid_escapes(path) {
            return None;
        }
        if let Some(q) = query {
            if !valid_escapes(q) {
                return None;
            }
        }
        Some(HttpUrl {
            host,
            path: path.to_string(),
            query: query.map(|q| q.to_string()),
        })
    }

    /// OkHttp `queryParameter(name)`：第一个名字对得上的参数，解码后的值。
    fn query_parameter(&self, name: &str) -> Option<String> {
        let query = self.query.as_deref()?;
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            if percent_decode(key)? == name {
                return percent_decode(value);
            }
        }
        None
    }
}

/// 百分号转义都合法（`%` 后面必须跟两个十六进制数字）。
fn valid_escapes(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}

/// 百分号解码（不把 `+` 当成空格，和 OkHttp 的 query 一致）；非法 UTF-8 按替换字符处理。
fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}
