//! 小黑盒**帖子详情**的解析：逐条对照 `source-heybox/HeyboxParser.kt` 的 `detail` 和
//! `HeyboxBody.kt` 的 `body` / `bodyWithVideos` / `images` / `gameIds` / `games` / `html`（那几个钩子）。
//!
//! 试点时 `body = None` 让 Kotlin 用 jsoup 补，
//! 现在 HTML → 富文本走共用的 [`crate::html`]（按 jsoup / `HtmlRichText` 的口径写的），
//! 所以 `FeedDetail.body` 永远是 Rust 解析的完整正文，不再有"回退"。
//!
//! 三块要分开看：
//! 1. **正文的三种形态**（`link.text`）：图文段的 JSON 数组、资讯 / 文章的真 HTML、视频帖的纯文本；
//! 2. **数组里的 `img` 段和 HTML 里的 `<img data-original>` 是同一批图**：按 [`crate::html::image_key`]
//!    配对——显示用段里的缩略图，`original` 放 HTML 里的原图，配上对的 `img` 段不再重复输出；
//! 3. **游戏卡片**只给 id，名字 / 价格要另取 `/game/get_game_infos/`（[`BodyEnv::games`]），
//!    取不到就降级成"游戏 {id}"（Steam 链接只对旧的小 appid 给）。

use crate::detail::{Block, FeedDetail, RichText, Span, VideoInfo, VideoPart};
use crate::html::{self, Element, HookBlock, Hooks, Options};
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, Media, Stats, TimePrecision};

use super::text::{self, VideoRef, VideoSeq};
use super::{community_of, keep_animation, kind_of, link_url, plain, summary_of, user, HeyboxError, HEYBOX};

/// 视频帖自己的视频：`partId = main`（和 Kotlin `HeyboxVideo.MAIN` 一样）。
pub const VIDEO_MAIN: &str = "main";

/// 小黑盒 HTML 正文里的相对地址按网页根补全（Kotlin `HeyboxBody.WEB`）。
pub const WEB_BASE: &str = "https://www.xiaoheihe.cn/";

/// 游戏卡片的 Steam 地址前缀（`HeyboxBody.steamUrl`）。
const STEAM_APP: &str = "https://store.steampowered.com/app/";

/// `/game/get_game_infos/` 的一张卡片（Kotlin `GameInfo`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInfo {
    pub id: String,
    pub name: String,
    pub cover: Option<String>,
    pub subtitle: Option<String>,
    pub url: Option<String>,
}

/// 正文解析要用到的外部数据：表情表、游戏卡片。缺了就降级成文字 / 只有 id 的卡片。
#[derive(Debug, Clone, Default)]
pub struct BodyEnv {
    pub emotes: Vec<(String, String)>,
    pub games: Vec<GameInfo>,
}

impl BodyEnv {
    fn game(&self, id: &str) -> Option<&GameInfo> {
        self.games.iter().find(|g| g.id == id)
    }
}

fn parse_err(detail: impl Into<String>) -> HeyboxError {
    HeyboxError::Parse {
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 详情：`/bbs/app/link/tree` 的响应。
pub fn parse_detail(body: &str, env: &BodyEnv) -> Result<FeedDetail, HeyboxError> {
    let root = json::parse_object(body).map_err(parse_err)?;
    parse_detail_object(&root, env)
}

/// 同 [`parse_detail`]，但入参是已经解析好的树（平台层要先查 `status` 外壳）。
pub fn parse_detail_object(root: &JsonMap, env: &BodyEnv) -> Result<FeedDetail, HeyboxError> {
    let link = json::obj(root, "result")
        .and_then(|r| json::obj(r, "link"))
        .ok_or_else(|| parse_err("详情缺少 result.link"))?;
    let link_id = json::string(link, "linkid")
        .or_else(|| json::string(link, "link_id"))
        .filter(|s| !super::is_blank(s) && s != "0")
        .ok_or_else(|| parse_err("详情缺少 linkid"))?;
    let title = json::text(link, "title").and_then(|t| plain(&t));
    let summary = summary_of(link, title.as_deref());
    let video = json::bool(link, "has_video") == Some(true);
    let media = if video {
        vec![Media::Video {
            cover: json::text(link, "video_thumb").map(|c| json::to_https(&c)),
            duration_sec: json::int(link, "video_duration").filter(|d| *d > 0),
            width: None,
            height: None,
        }]
    } else {
        images(link)
    };
    let item = FeedItem {
        platform: HEYBOX.to_string(),
        id: link_id.clone(),
        kind: kind_of(link, video),
        title: title.clone(),
        summary: summary.clone(),
        language: None,
        author: json::obj(link, "user").and_then(user),
        published_at: json::long(link, "create_at").filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: json::long(link, "link_award_num"),
            comments: json::long(link, "comment_num"),
            shares: json::long(link, "forward_num"),
            favorites: json::long(link, "favour_count"),
            ..Stats::default()
        },
        reason: None,
        community: community_of(link),
        community_icon: None,
        repost: None,
        emotes: super::card_emotes(link, &env.emotes, title.as_deref(), summary.as_deref()),
        adult: false,
        url: Some(link_url(&link_id)),
        detail: DetailTarget::Native {
            id: link_id.clone(),
            platform: None,
        },
    };
    Ok(FeedDetail {
        item,
        body: body_blocks(link, env),
        video: video_info(link),
        follow: None,
        related: Vec::new(),
    })
}

/// 帖子自己的视频（`has_video=1` + `video_url`）→ `FeedDetail.video`：一个分 P，`id = main`。
fn video_info(link: &JsonMap) -> Option<VideoInfo> {
    text::main_video(link).map(|(_, duration, width, height)| VideoInfo {
        parts: vec![VideoPart {
            id: VIDEO_MAIN.to_string(),
            title: None,
            duration_sec: duration,
            width,
            height,
        }],
        danmaku_segment_ms: None,
    })
}

/// 帖子里的图片（详情条目的 `media`）：图文段数组里的 `img` 段。
fn images(link: &JsonMap) -> Vec<Media> {
    segments(&json::string(link, "text").unwrap_or_default())
        .unwrap_or_default()
        .iter()
        .filter(|s| json::string(s, "type").as_deref() == Some("img"))
        .filter_map(|s| {
            json::text(s, "url").map(|u| Media::Image {
                url: keep_animation(&json::to_https(&u)),
                width: json::int(s, "width"),
                height: json::int(s, "height"),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 正文
// ---------------------------------------------------------------------------

/// 帖子正文（`HeyboxBody.body`）。
///
/// 视频帖自己的视频不在正文里（它是 `FeedDetail.video`）；拿不到地址、只剩封面时在顶上放一张卡片。
fn body_blocks(link: &JsonMap, env: &BodyEnv) -> RichText {
    body_with_videos(link, env).0
}

/// 同 [`body_blocks`]，另外按出现顺序返回正文里夹着的视频的真实地址（第 n 个就是 `seg:n`）。
///
/// Kotlin 是 `HeyboxBody.bodyWithVideos`：`detail` 只要正文（[`body_blocks`] 是它的 `.0`），
/// `play(partId = seg:n)` 要那份地址列表——两边走**同一个**扫描器，`seg:n` 的编号才不会漂。
pub fn body_with_videos(link: &JsonMap, env: &BodyEnv) -> (RichText, Vec<VideoRef>) {
    let mut out: Vec<Block> = Vec::new();
    if text::main_video(link).is_none() {
        if let Some(card) = text::video_card(link) {
            out.push(card);
        }
    }
    let link_id = json::string(link, "linkid")
        .or_else(|| json::string(link, "link_id"))
        .filter(|s| !super::is_blank(s) && s != "0");
    let mut seq = VideoSeq::new(link_id);
    let raw = json::string(link, "text").unwrap_or_default();
    match segments(&raw) {
        None => {
            if text::has_html_tag(&raw) {
                out.extend(html_blocks(
                    &raw,
                    env,
                    &[],
                    &mut Vec::new(),
                    false,
                    &mut seq,
                ));
            } else {
                out.extend(text::plain_blocks(&raw, &env.emotes));
            }
        }
        Some(segs) => out.extend(segment_blocks(&segs, env, &mut seq)),
    }
    (RichText { blocks: out }, seq.refs)
}

/// 正文的段数组：`[{"type":"text",…},{"type":"img",…}]`。不是数组 / 不是 JSON 就当纯文本。
fn segments(raw: &str) -> Option<Vec<JsonMap>> {
    let t = raw.trim();
    if !t.starts_with('[') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(t).ok()?;
    let arr = value.as_array()?;
    Some(arr.iter().filter_map(|v| v.as_object().cloned()).collect())
}

/// 段数组 → 正文块（`HeyboxBody.segmentBlocks`）。
fn segment_blocks(segs: &[JsonMap], env: &BodyEnv, seq: &mut VideoSeq) -> Vec<Block> {
    // 缩略图按路径索引，HTML 里的 data-original 用它配对
    let mut thumbs: Vec<(String, Thumb)> = Vec::new();
    for s in segs {
        if json::string(s, "type").as_deref() != Some("img") {
            continue;
        }
        let Some(url) = json::text(s, "url") else {
            continue;
        };
        let url = json::to_https(&url);
        thumbs.push((
            html::image_key(&url),
            Thumb {
                url,
                width: json::int(s, "width"),
                height: json::int(s, "height"),
            },
        ));
    }
    let mut used: Vec<String> = Vec::new();
    let mut out: Vec<Block> = Vec::new();
    for seg in segs {
        match json::string(seg, "type").as_deref() {
            Some("html") => {
                out.extend(html_blocks(
                    &json::string(seg, "text").unwrap_or_default(),
                    env,
                    &thumbs,
                    &mut used,
                    false,
                    seq,
                ));
            }
            Some("text") => out.extend(text::plain_blocks(
                &json::string(seg, "text").unwrap_or_default(),
                &env.emotes,
            )),
            Some("img") => {
                let Some(url) = json::text(seg, "url") else {
                    continue;
                };
                let url = keep_animation(&json::to_https(&url));
                if !used.contains(&html::image_key(&url)) {
                    out.push(Block::Image {
                        original: original_of(&url),
                        url,
                        width: json::int(seg, "width"),
                        height: json::int(seg, "height"),
                        caption: None,
                    });
                }
            }
            Some("video") => {
                if let Some(block) = video_segment(seg, seq) {
                    out.push(block);
                }
            }
            // 没见过的段类型：有文字就当文字，不丢内容
            _ => {
                if let Some(t) = json::text(seg, "text") {
                    out.extend(text::plain_blocks(&t, &env.emotes));
                }
            }
        }
    }
    out
}

/// 段数组里的 `video` 段（没在真实数据里见过，按 `img` 段的写法猜：url + 封面）。
/// 有地址就是行内播放的 `Block.Video`（`seg:n`），只有封面才是卡片。
fn video_segment(seg: &JsonMap, seq: &mut VideoSeq) -> Option<Block> {
    let url = json::text(seg, "url").map(|u| json::to_https(&u));
    let cover = json::text(seg, "thumb")
        .or_else(|| json::text(seg, "cover"))
        .or_else(|| json::text(seg, "video_thumb"))
        .map(|u| json::to_https(&u));
    if url.is_none() && cover.is_none() {
        return None;
    }
    if let Some(url) = &url {
        if let Some(block) = seq.add(VideoRef {
            url: url.clone(),
            cover: cover.clone(),
            duration_sec: json::int(seg, "duration").filter(|d| *d > 0),
            width: json::int(seg, "width"),
            height: json::int(seg, "height"),
        }) {
            return Some(block);
        }
    }
    Some(Block::Card {
        title: "视频".to_string(),
        subtitle: None,
        cover,
        url,
        target: None,
    })
}

/// 一张缩略图（段数组里的 `img` 段）。
pub(crate) struct Thumb {
    url: String,
    width: Option<i32>,
    height: Option<i32>,
}

/// HTML 段 → 正文块：换表情 span → 建树 / 走钩子 → 并图片说明 → 展开表情代码。
pub(crate) fn html_blocks(
    raw: &str,
    env: &BodyEnv,
    thumbs: &[(String, Thumb)],
    used: &mut Vec<String>,
    newline_is_break: bool,
    seq: &mut VideoSeq,
) -> Vec<Block> {
    let html = text::emoji_spans_to_images(raw, &env.emotes);
    let mut hooks = BodyHooks {
        env,
        thumbs,
        used,
        seq,
    };
    let converted = html::convert(
        &html,
        &Options {
            base_uri: WEB_BASE.to_string(),
            newline_is_break,
        },
        &mut hooks,
    );
    let with_captions = attach_captions(converted.blocks, &converted.captions);
    text::expand_emotes(with_captions, &env.emotes)
}

/// 说明段落（`<h4 class="img-desc">` 标记出来的块）紧跟在图片后面就并进去当 caption（Kotlin `attachCaptions`）。
fn attach_captions(blocks: Vec<Block>, captions: &[usize]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.into_iter().enumerate() {
        let is_caption = captions.contains(&index);
        let attached = if is_caption {
            match out.last_mut() {
                Some(Block::Image {
                    caption: slot @ None,
                    ..
                }) => {
                    if let Block::Paragraph { spans } = &block {
                        *slot = Some(spans.iter().map(|s| s.text()).collect::<String>());
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        } else {
            false
        };
        if !attached {
            out.push(block);
        }
    }
    out
}

/// HTML 转换的小黑盒钩子（Kotlin `HeyboxBody.element` / `link` / `inlineImage` / `image`）。
struct BodyHooks<'a> {
    env: &'a BodyEnv,
    thumbs: &'a [(String, Thumb)],
    used: &'a mut Vec<String>,
    seq: &'a mut VideoSeq,
}

impl<'a> BodyHooks<'a> {
    fn thumb(&self, key: &str) -> Option<&Thumb> {
        self.thumbs.iter().find(|(k, _)| k == key).map(|(_, t)| t)
    }
}

impl<'a> Hooks for BodyHooks<'a> {
    fn element(&mut self, el: &Element) -> Option<Vec<HookBlock>> {
        let tag = el.normal_name();
        // 游戏卡片：<img class="mention-game" data-gameid data-type="game_card">，
        // 或 <a data-link-type="game" data-game-id>
        if (tag == "img" && (el.attr("data-type") == "game_card" || el.has_class("mention-game")))
            || (tag == "a" && (el.attr("data-link-type") == "game" || el.has_attr("data-game-id")))
        {
            let id = if !el.attr("data-gameid").is_empty() {
                el.attr("data-gameid")
            } else {
                el.attr("data-game-id")
            };
            if id.is_empty() {
                return None;
            }
            return Some(vec![HookBlock::plain(game_card(id, self.env))]);
        }
        // 图片说明：<h4 class="img-desc">，等会儿并进上一张图
        if HEADING_TAGS.contains(&tag) && el.has_class("img-desc") {
            let caption = html::kotlin_trim(&el.text()).to_string();
            if caption.is_empty() {
                return Some(Vec::new());
            }
            return Some(vec![HookBlock {
                block: Block::Paragraph {
                    spans: vec![Span::Text {
                        text: caption,
                        bold: false,
                        italic: false,
                    }],
                },
                caption: true,
            }]);
        }
        if tag == "video" || tag == "iframe" {
            let mut src = el.attr("src").to_string();
            if src.is_empty() {
                if let Some(source) = el.select_first("source") {
                    src = source.attr("src").to_string();
                }
            }
            let cover = el.attr("poster").to_string();
            if src.is_empty() && cover.is_empty() {
                return Some(Vec::new());
            }
            // <video> 有地址就是行内播放；<iframe> 是网页（不是视频文件），还是卡片
            if tag == "video" && !src.is_empty() {
                if let Some(block) = self.seq.add(VideoRef {
                    url: json::to_https(&src),
                    cover: if cover.is_empty() {
                        None
                    } else {
                        Some(json::to_https(&cover))
                    },
                    duration_sec: el
                        .attr("data-duration")
                        .parse::<f64>()
                        .ok()
                        .map(|d| d as i32)
                        .filter(|d| *d > 0),
                    width: parse_int(el.attr("width")),
                    height: parse_int(el.attr("height")),
                }) {
                    return Some(vec![HookBlock::plain(block)]);
                }
            }
            return Some(vec![HookBlock::plain(Block::Card {
                title: "视频".to_string(),
                subtitle: None,
                cover: if cover.is_empty() {
                    None
                } else {
                    Some(json::to_https(&cover))
                },
                url: if src.is_empty() {
                    None
                } else {
                    Some(json::to_https(&src))
                },
                target: None,
            })]);
        }
        None
    }

    fn link(&mut self, _url: &str, text: &str, el: &Element) -> Option<Span> {
        let raw = html::kotlin_trim(el.attr("href")).to_string();
        let lower = raw.to_ascii_lowercase();
        if !lower.starts_with("heybox://") && !lower.starts_with("xhh://") {
            return None;
        }
        let encoded = match raw.find("://") {
            Some(i) => &raw[i + 3..],
            None => "",
        };
        let Some(decoded) = text::url_decode(encoded) else {
            return plain_span(text);
        };
        let json = match json::parse_object(&decoded) {
            Ok(json) => json,
            Err(_) => return plain_span(text),
        };
        let link = json::obj(&json, "link");
        if let Some(link_id) = link
            .and_then(|l| json::string(l, "linkid"))
            .filter(|s| !super::is_blank(s) && s != "0")
        {
            let label = if text.is_empty() {
                format!("帖子 {link_id}")
            } else {
                text.to_string()
            };
            return Some(Span::Link {
                text: label,
                url: link_url(&link_id),
                target: Some(DetailTarget::Native {
                    id: link_id,
                    platform: None,
                }),
            });
        }
        // 用户（没有真实样本，按常见字段名猜）
        let user_id = json::obj(&json, "user")
            .and_then(|u| json::string(u, "userid"))
            .or_else(|| json::string(&json, "userid"))
            .filter(|s| !super::is_blank(s) && s != "0");
        if let Some(id) = user_id {
            if !text.is_empty() {
                return Some(Span::Mention {
                    text: text.to_string(),
                    user: text::mention_user(id, text),
                });
            }
        }
        let url = json::string(&json, "url")
            .or_else(|| link.and_then(|l| json::string(l, "url")));
        if let Some(url) = url {
            if url.starts_with("http") && !text.is_empty() {
                return Some(Span::Link {
                    text: text.to_string(),
                    url: json::to_https(&url),
                    target: None,
                });
            }
        }
        plain_span(text)
    }

    fn inline_image(&mut self, el: &Element) -> Option<Span> {
        if el.has_class("hb-emote") {
            return Some(Span::Emote {
                text: el.attr("alt").to_string(),
                url: el.attr("src").to_string(),
            });
        }
        None
    }

    fn image(&mut self, el: &Element) -> Option<Block> {
        let original_raw = el.attr("data-original");
        if html::kotlin_is_blank(original_raw) {
            return None;
        }
        let original = json::to_https(original_raw);
        let key = html::image_key(&original);
        if !self.used.contains(&key) {
            self.used.push(key.clone());
        }
        let thumb = self
            .thumb(&key)
            .map(|t| keep_animation(&json::to_https(&t.url)));
        let url = thumb.clone().unwrap_or_else(|| original.clone());
        let width = parse_int(el.attr("data-width")).or_else(|| self.thumb(&key).and_then(|t| t.width));
        let height =
            parse_int(el.attr("data-height")).or_else(|| self.thumb(&key).and_then(|t| t.height));
        let caption = html::kotlin_trim(el.attr("data-description"));
        Some(Block::Image {
            original: if thumb.is_some() && url != original {
                Some(original)
            } else {
                None
            },
            url,
            width,
            height,
            caption: if caption.is_empty() {
                None
            } else {
                Some(caption.to_string())
            },
        })
    }
}

/// `<h1>`~`<h6>`：和 Kotlin `HEADING_TAGS` 一样（`p` / `div` 也在里面）。
const HEADING_TAGS: [&str; 8] = ["h1", "h2", "h3", "h4", "h5", "h6", "p", "div"];

fn plain_span(text: &str) -> Option<Span> {
    if text.is_empty() {
        None
    } else {
        Some(Span::Text {
            text: text.to_string(),
            bold: false,
            italic: false,
        })
    }
}

/// Kotlin `String.toIntOrNull()`（不 trim、不认空格）。
fn parse_int(raw: &str) -> Option<i32> {
    if raw.is_empty() {
        return None;
    }
    let mut chars = raw.chars().peekable();
    let mut sign = 1i64;
    if let Some(&c) = chars.peek() {
        if c == '+' || c == '-' {
            if c == '-' {
                sign = -1;
            }
            chars.next();
        }
    }
    let digits: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()
        .map(|v| sign * v)
        .and_then(|v| i32::try_from(v).ok())
}

// ---------------------------------------------------------------------------
// 游戏卡片
// ---------------------------------------------------------------------------

/// 帖子里所有游戏卡片的 id（去重，按出现顺序）：Kotlin `HeyboxBody.gameIds`。
pub fn game_ids(link: &JsonMap) -> Vec<String> {
    let raw = json::string(link, "text").unwrap_or_default();
    let sources: Vec<String> = match segments(&raw) {
        Some(segs) => segs
            .iter()
            .filter(|s| json::string(s, "type").as_deref() == Some("html"))
            .filter_map(|s| json::string(s, "text"))
            .collect(),
        None => vec![raw],
    };
    let mut out: Vec<String> = Vec::new();
    for html in sources {
        for id in game_id_matches(&html) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// `data-gameid="(\d+)"`（Kotlin `GAME_ID`）：只认双引号、只认数字。
fn game_id_matches(html: &str) -> Vec<String> {
    let needle = "data-gameid=\"";
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + needle.len() <= b.len() {
        if b[i..i + needle.len()].eq_ignore_ascii_case(needle.as_bytes()) {
            let start = i + needle.len();
            let mut end = start;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
            if end > start && b.get(end) == Some(&b'"') {
                out.push(html[start..end].to_string());
            }
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// `/game/get_game_infos/` 的响应 → 卡片信息（Kotlin `HeyboxBody.games`）。
pub fn games_from_response(root: &JsonMap) -> Vec<GameInfo> {
    let mut out: Vec<GameInfo> = Vec::new();
    let base_infos = json::obj(root, "result")
        .and_then(|r| json::arr(r, "base_infos"))
        .map(|v| v.as_slice())
        .unwrap_or_default();
    for value in base_infos {
        let Some(o) = value.as_object() else {
            continue;
        };
        let Some(id) = json::string(o, "steam_appid").filter(|s| !super::is_blank(s) && s != "0")
        else {
            continue;
        };
        let Some(name) = json::text(o, "name") else {
            continue;
        };
        let info = GameInfo {
            id: id.clone(),
            name,
            cover: json::text(o, "image").map(|u| json::to_https(&u)),
            subtitle: game_subtitle(o),
            url: if json::string(o, "game_type").as_deref() == Some("pc") {
                Some(steam_url(&id))
            } else {
                None
            },
        };
        // Kotlin 是 LinkedHashMap：重复键覆盖值、位置留在第一次出现的地方
        match out.iter_mut().find(|g| g.id == id) {
            Some(slot) => *slot = info,
            None => out.push(info),
        }
    }
    out
}

/// 游戏卡片的副标题：价格 / 折扣 / 史低 / 评分（Kotlin `gameSubtitle`）。
fn game_subtitle(o: &JsonMap) -> Option<String> {
    let price = json::obj(o, "price");
    let mut parts: Vec<String> = Vec::new();
    if json::bool(o, "is_free") == Some(true) {
        parts.push("免费".to_string());
    } else if let Some(price) = price {
        if let Some(current) = json::text(price, "current") {
            parts.push(format!("¥{current}"));
        }
        if let Some(discount) = json::int(price, "discount").filter(|d| *d > 0) {
            parts.push(format!("-{discount}%"));
        }
        if json::bool(price, "is_lowest") == Some(true) {
            parts.push("史低".to_string());
        }
    }
    if let Some(score) = json::text(o, "score") {
        parts.push(format!("评分 {score}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// 游戏卡片：环境里有信息就用它，否则只有 id（旧的小 appid 给 Steam 链接，Kotlin `gameCard`）。
fn game_card(id: &str, env: &BodyEnv) -> Block {
    match env.game(id) {
        Some(g) => Block::Card {
            title: g.name.clone(),
            subtitle: g.subtitle.clone(),
            cover: g.cover.clone(),
            url: g.url.clone(),
            target: None,
        },
        None => Block::Card {
            title: format!("游戏 {id}"),
            subtitle: None,
            cover: None,
            url: id
                .parse::<i64>()
                .ok()
                .filter(|n| *n < 900_000_000)
                .map(|_| steam_url(id)),
            target: None,
        },
    }
}

fn steam_url(id: &str) -> String {
    format!("{STEAM_APP}{id}")
}

/// 缩略图地址带 `?imageMogr2/…` 处理参数，去掉就是原图（Kotlin `originalOf`）。
fn original_of(url: &str) -> Option<String> {
    let base = url.split('?').next().unwrap_or("");
    if url.contains("imageMogr2") && base != url {
        Some(base.to_string())
    } else {
        None
    }
}

