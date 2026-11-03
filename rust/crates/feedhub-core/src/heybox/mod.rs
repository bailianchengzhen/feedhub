//! 小黑盒：推荐流（唯一的流）的解析，以及网页接口的 hkey 签名。
//!
//! 逐条对照 `source-heybox/HeyboxParser.kt` 的 `feeds` / `link` 和 `HeyboxBody.keepAnimation` /
//! `Emotes.used`；签名对照 `HkeySigner.kt`（见 [`sign`]）。
//!
//! 正文里的 HTML 走共用的
//! [`crate::html`]；播放 / 扫码 / whoami 仍是 Kotlin。
//!
//! 两个实现细节值得说明：
//! - **不用 `regex` crate**：`rust/.cargo` 的离线缓存里没有它，加依赖要联网改 `Cargo.lock`，
//!   而当前的规矩是"`.so` 不引入新东西、构建要能离线跑"。所以 `HeyboxParser` 里那几条正则
//!   （表情代码、去 HTML 标签、`/format/jpg`）都是手写扫描器，语义按 Java 正则逐条对着写，
//!   对拍（`HeyboxDualRunTest`）守着两边一致。
//! - `isNotBlank` / `\s` 这些 Kotlin 语义按 Java 的定义来（`\s` = `[ \t\n\x0B\f\r]`，**不含** NBSP），
//!   不用 Rust 的 Unicode 空白，否则边界样本会和 Kotlin 分叉。

pub mod comment;
pub mod detail;
pub mod play;
pub mod qr;
pub mod search;
pub mod sign;
pub mod source;
pub mod text;

use crate::host::HostError;
use crate::json::{self, JsonMap};
use crate::model::{DetailTarget, FeedItem, ItemKind, Media, Page, Stats, TimePrecision, UserRef};

/// 小黑盒的平台 id（和 `HeyboxPlugin` 里的 `Platform.define("heybox", …)` 一致）。
pub const HEYBOX: &str = "heybox";

/// 正文 / 评论里的游戏卡片 id 前缀（`Block.Card` 的 url）。
const STEAM_APP: &str = "https://store.steampowered.com/app/";

/// 文章的 `link_tag`：11 是 xiaoheihe-cli 发文章用的分类，1 是老的网页文章。
const ARTICLE_TAGS: [i32; 2] = [1, 11];

/// 广告位的常见标记（真实响应里没见过，按 Kotlin 的兜底口径抄）。
const AD_FLAGS: [&str; 3] = ["is_ad", "is_advert", "ad"];

/// Rust 侧的业务错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeyboxError {
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

    /// 宿主发请求时失败（连不上 / 超时 / 连接被重置）。上层映射回 `java.io.IOException`。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// fail-closed：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题），别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Host { detail: String },
}

impl HeyboxError {
    fn parse(detail: impl Into<String>) -> Self {
        HeyboxError::Parse {
            detail: detail.into(),
        }
    }
}

/// 宿主回调的错误**原样分类**带回来：`Network` / `ProxyUnavailable` 是两个不同的 Kotlin 异常
/// （后者是 fail-closed，cli 靠它分退出码 6），不能合并。
impl From<HostError> for HeyboxError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Network { detail } => HeyboxError::Network { detail },
            HostError::ProxyUnavailable { detail } => HeyboxError::ProxyUnavailable { detail },
            HostError::Other { detail } => HeyboxError::Host { detail },
        }
    }
}

pub fn link_url(link_id: &str) -> String {
    format!("https://www.xiaoheihe.cn/app/bbs/link/{link_id}")
}

/// 推荐流：`/bbs/app/feeds` 的响应。
///
/// `offset` 是这一页请求时的 offset（网页按已加载的条数往后翻，第二页 offset=10），
/// 下一页是 `offset + 本页条数`——**按原始条数算**，被跳过的卡片（广告 / 没 id / 全空）也占位。
/// 推荐流没有尽头；某页一条都没有就当到头了。
pub fn parse_feeds(
    body: &str,
    offset: i32,
    emotes: &[(String, String)],
) -> Result<Page, HeyboxError> {
    let root = json::parse_object(body).map_err(HeyboxError::parse)?;
    parse_feeds_object(&root, offset, emotes)
}

/// 同 [`parse_feeds`]，但入参是**已经解析好的树**。
///
/// 平台层（[`source`]）要先查 `status` 外壳、再看要不要取表情表，树已经建好了；
/// 再 `parse` 一遍 body 是白花的开销。
pub fn parse_feeds_object(
    root: &JsonMap,
    offset: i32,
    emotes: &[(String, String)],
) -> Result<Page, HeyboxError> {
    let links = json::path(root, &["result"])
        .and_then(|r| json::arr(r, "links"))
        .ok_or_else(|| HeyboxError::parse("推荐流缺少 result.links"))?;
    let next = if links.is_empty() {
        None
    } else {
        // Kotlin 的 `offset + links.size` 是 Int 加法（溢出会绕），这里照做
        Some((offset.wrapping_add(links.len() as i32)).to_string())
    };
    let items = links
        .iter()
        .filter_map(|v| v.as_object())
        .filter_map(|o| link(o, emotes))
        .collect();
    Ok(Page { items, next })
}

/// 文本里有没有 `[cube_xx]` 这样的表情代码（Kotlin `HeyboxBody.EMOTE_TOKEN`
/// = `\[[A-Za-z0-9]+_[^\[\]\s]{1,30}]`）。决定"这一页要不要去取表情表"。
///
/// 和 `card_emotes` 共用同一个扫描器 [`emote_match`]；`{1,30}` 按**字符**数。
pub fn contains_emote_token(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            if emote_match(b, i).is_some() {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 推荐流这一页所有标题和摘要拼在一起（Kotlin `feedTexts`）：里面有 `[cube_喜欢]` 这样的表情代码
/// 才需要去取表情表。留在 Kotlin 也行，这里给 Rust 侧的测试 / 探针用。
pub fn feed_texts(root: &JsonMap) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(result) = json::obj(root, "result") {
        for v in json::arr(result, "links").map(|v| v.as_slice()).unwrap_or_default() {
            let Some(o) = v.as_object() else { continue };
            let mut lines: Vec<String> = Vec::new();
            if let Some(t) = json::string(o, "title") {
                lines.push(t);
            }
            if let Some(d) = json::string(o, "description") {
                lines.push(d);
            }
            parts.push(lines.join("\n"));
        }
    }
    parts.join("\n")
}

/// 一条帖子。跳过：带广告标记的、没有 linkid 的、标题正文图片全空的。作者缺了照样保留。
///
/// 类型：`has_video=1` 是视频；否则 `link_tag` 27 / 28 是图文和动态，11 / 1 是文章。
pub fn link(o: &JsonMap, emotes: &[(String, String)]) -> Option<FeedItem> {
    if is_ad(o) {
        return None;
    }
    // 信息流里是 linkid；xiaoheihe-cli 读帖子 id 时也兼容 link_id
    let link_id = json::string(o, "linkid")
        .or_else(|| json::string(o, "link_id"))
        .filter(|s| !is_blank(s) && s != "0")?;
    let title = json::text(o, "title").and_then(|t| plain(&t));
    let summary = summary_of(o, title.as_deref());

    let video = json::bool(o, "has_video") == Some(true);
    // 动图的缩略图地址带 format/jpg 会变成静态图，见 keep_animation
    let urls: &[serde_json::Value] = match json::arr(o, "thumbs").filter(|t| !t.is_empty()) {
        Some(thumbs) => thumbs.as_slice(),
        None => json::arr(o, "imgs").map(|v| v.as_slice()).unwrap_or_default(),
    };
    let images: Vec<Media> = urls
        .iter()
        .filter_map(json::as_string)
    .filter(|s| !is_blank(s))
    .map(|u| Media::Image {
        url: keep_animation(&json::to_https(&u)),
        width: None,
        height: None,
    })
    .collect();

    let media = if video {
        let info = json::obj(o, "video_info");
        let cover = json::text(o, "video_thumb")
            .or_else(|| info.and_then(|i| json::text(i, "thumb")))
            .map(|c| json::to_https(&c))
            .or_else(|| {
                images.iter().find_map(|m| match m {
                    Media::Image { url, .. } => Some(url.clone()),
                    _ => None,
                })
            });
        // 时长是 "00:05" 这样的文本
        let duration = json::text(o, "duration")
            .or_else(|| info.and_then(|i| json::text(i, "duration")));
        vec![Media::Video {
            cover,
            duration_sec: json::parse_duration(duration.as_deref()),
            // `video_info` 里带画面宽高：打开详情时直接按竖屏 / 横屏摆，不用等流回来再换界面
            width: info.and_then(|i| json::int(i, "width")).filter(|v| *v > 0),
            height: info.and_then(|i| json::int(i, "height")).filter(|v| *v > 0),
        }]
    } else {
        images
    };
    if title.is_none() && summary.is_none() && media.is_empty() {
        return None;
    }

    let url = link_url(&link_id);
    Some(FeedItem {
        platform: HEYBOX.to_string(),
        id: link_id.clone(),
        kind: kind_of(o, video),
        title: title.clone(),
        summary: summary.clone(),
        language: None,
        author: json::obj(o, "user").and_then(user),
        published_at: json::long(o, "create_at").filter(|t| *t > 0),
        published_at_precision: TimePrecision::Second,
        media,
        stats: Stats {
            likes: json::long(o, "link_award_num"),
            comments: json::long(o, "comment_num"),
            shares: json::long(o, "forward_num"),
            ..Stats::default()
        },
        reason: None,
        community: community_of(o),
        community_icon: None,
        repost: None,
        emotes: card_emotes(o, emotes, title.as_deref(), summary.as_deref()),
        adult: false,
        url: Some(url),
        detail: DetailTarget::Native {
            id: link_id,
            platform: None,
        },
    })
}

fn kind_of(o: &JsonMap, video: bool) -> ItemKind {
    if video {
        ItemKind::Video
    } else if json::int(o, "link_tag").is_some_and(|t| ARTICLE_TAGS.contains(&t)) {
        ItemKind::Article
    } else {
        ItemKind::Post
    }
}

/// 文章的 description 常常以标题开头，去掉这段重复。
fn summary_of(o: &JsonMap, title: Option<&str>) -> Option<String> {
    let text = json::text(o, "description").and_then(|d| plain(&d))?;
    let text = match title {
        Some(t) if text.starts_with(t) => text[t.len()..].trim_start().to_string(),
        _ => text,
    };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn community_of(o: &JsonMap) -> Option<String> {
    json::arr(o, "topics")?
        .iter()
        .filter_map(|v| v.as_object())
        .find_map(|t| json::text(t, "name").map(|n| n.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// 当前账号（`/account/restore_login` 的 `result`，Kotlin `HeyboxParser.account`）。
///
/// 账号 id 有三处：`result.profile.heybox_id` / `result.account_detail.userid` / `result.heybox_id`，
/// 取第一个非空且不是 `"0"` 的（**按这个顺序**）；名字依次退到 `account_detail.username` /
/// `profile.nickname` / `profile.username` / id；头像 `account_detail` 优先。
pub fn account(root: &JsonMap) -> Option<UserRef> {
    let result = json::obj(root, "result")?;
    let profile = json::obj(result, "profile");
    let detail = json::obj(result, "account_detail");
    let id = [
        profile.and_then(|p| json::string(p, "heybox_id")),
        detail.and_then(|d| json::string(d, "userid")),
        json::string(result, "heybox_id"),
    ]
    .into_iter()
    .flatten()
    .map(|v| v.trim().to_string())
    .find(|v| !v.is_empty() && v != "0")?;
    let name = detail
        .and_then(|d| json::text(d, "username"))
        .or_else(|| profile.and_then(|p| json::text(p, "nickname")))
        .or_else(|| profile.and_then(|p| json::text(p, "username")))
        .unwrap_or_else(|| id.clone());
    let avatar = detail
        .and_then(avatar)
        .or_else(|| profile.and_then(avatar));
    Some(UserRef {
        platform: HEYBOX.to_string(),
        id,
        name,
        avatar,
        url: None,
    })
}

fn user(u: &JsonMap) -> Option<UserRef> {
    let id = json::string(u, "userid").filter(|s| !is_blank(s) && s != "0")?;
    let name = json::text(u, "username").unwrap_or_else(|| id.clone());
    Some(UserRef {
        platform: HEYBOX.to_string(),
        id: id.clone(),
        name,
        // 网页个人主页的地址没核实过，先不填 url
        avatar: avatar(u),
        url: None,
    })
}

/// 小黑盒接口里头像字段有正确拼写 avatar 和历史拼写 avartar 两种。
fn avatar(u: &JsonMap) -> Option<String> {
    json::text(u, "avatar")
        .or_else(|| json::text(u, "avartar"))
        .map(|a| json::to_https(&a))
}

/// 广告位的字段没在真实响应里见过，按常见的几个标记兜底。
fn is_ad(o: &JsonMap) -> bool {
    AD_FLAGS.iter().any(|k| json::bool(o, k) == Some(true))
        || matches!(o.get("ad_info"), Some(serde_json::Value::Object(_)))
        || json::text(o, "ad_type").is_some()
}

// ---------------------------------------------------------------------------
// 纯文本 / 图片地址的小工具（对应 Kotlin 的 plain / stripHtml / keepAnimation）
// ---------------------------------------------------------------------------

/// 纯文本（标题、摘要）：去 HTML；表情代码按约定留文字，但去掉表情包前缀（`[cube_感动]` → `[感动]`）。
fn plain(s: &str) -> Option<String> {
    let stripped = strip_html(s);
    let out = replace_emote_prefix(&stripped);
    if is_blank(&out) {
        None
    } else {
        Some(out)
    }
}

/// 去掉 HTML 标签并还原常见实体（`core/Text.kt` 的 `stripHtml`）。
///
/// 这一份只是给本模块的 `plain()` 留名字。
fn strip_html(html: &str) -> String {
    crate::html::strip_html(html)
}

/// Kotlin `Regex("\\[[A-Za-z0-9]+_([^\\[\\]\\s]{1,30})]")` 的替换：`[cube_感动]` → `[感动]`。
fn replace_emote_prefix(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            if let Some((end, name)) = emote_match(b, i) {
                out.push('[');
                out.push_str(&s[name.0..name.1]);
                out.push(']');
                i = end;
                continue;
            }
        }
        let len = if b[i] >= 0x80 { utf8_len(b[i]) } else { 1 };
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}

/// 在 `b[start] == '['` 处试匹配表情代码，返回 `(整段结束位置, 名字的字节区间)`。
/// 语义和 Java 正则一致：字母数字段不吃回退（段里不可能出现 `_`），名字段 `{1,30}` 按**字符**数
/// （不是字节）限长。
fn emote_match(b: &[u8], start: usize) -> Option<(usize, (usize, usize))> {
    let mut j = start + 1;
    while j < b.len() && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == start + 1 || j >= b.len() || b[j] != b'_' {
        return None;
    }
    let name_start = j + 1;
    let mut k = name_start;
    let mut chars = 0;
    while k < b.len() && chars < 30 {
        if !is_emote_name_char(b, k) {
            break;
        }
        k += utf8_len(b[k]).max(1);
        chars += 1;
    }
    if chars == 0 || k >= b.len() || b[k] != b']' {
        return None;
    }
    Some((k + 1, (name_start, k)))
}

/// `[^\[\]\s]` 里"不是空白"的那一半：按 Java 的 `\s`（`[ \t\n\x0B\f\r]`），不含 NBSP。
fn is_emote_name_char(b: &[u8], at: usize) -> bool {
    let c = b[at];
    if c == b'[' || c == b']' {
        return false;
    }
    // ASCII 直接按 Java 的 \s 判；多字节字符（中文 / emoji）都不是 \s
    if c < 0x80 {
        !is_java_space(c)
    } else {
        true
    }
}

/// Java `\s` 的 ASCII 部分：`[ \t\n\x0B\f\r]`。
fn is_java_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Kotlin `Char.isBlank` / `String.isBlank`（Java 的 `Character.isWhitespace`：**不含** NBSP）。
///
/// 实现在 `crate::hosted`（11 家共用一份）；这里 `pub(crate)` 转发，`body.rs` / `source.rs` /
/// 测试里的 `super::is_blank` 一个字都不用改。
pub(crate) use crate::hosted::is_blank;

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// 动图要会动：信息流里的缩略图地址带 `?imageMogr2/…/format/jpg/…`（腾讯云数据万象的格式转换），
/// GIF 经过它会变成 JPEG，只剩静态的第一帧。路径是 `.gif` 时去掉 `format/jpg` 这一段；
/// 只剩空的 `?imageMogr2` 就当成原图。别的图原样返回（和 `HeyboxBody.keepAnimation` 一致）。
pub fn keep_animation(url: &str) -> String {
    let path = url.split('?').next().unwrap_or("");
    if !path.to_ascii_lowercase().ends_with(".gif") {
        return url.to_string();
    }
    let fixed = remove_format_jpg(url);
    if fixed.ends_with("?imageMogr2") {
        fixed[..fixed.len() - "?imageMogr2".len()].to_string()
    } else {
        fixed
    }
}

/// `Regex("/format/jpe?g(?=/|$)")` 的删除版（按 Java 的语义逐个匹配、不重叠）。
fn remove_format_jpg(url: &str) -> String {
    let b = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b[i..].starts_with(b"/format/jp") {
            let mut j = i + "/format/jp".len();
            let ok = if b.get(j) == Some(&b'g') {
                j += 1;
                true
            } else if b.get(j) == Some(&b'e') && b.get(j + 1) == Some(&b'g') {
                j += 2;
                true
            } else {
                false
            };
            if ok && (j == b.len() || b[j] == b'/') {
                i = j;
                continue;
            }
        }
        let len = if b[i] >= 0x80 { utf8_len(b[i]) } else { 1 };
        out.push_str(&url[i..i + len]);
        i += len;
    }
    out
}

// ---------------------------------------------------------------------------
// 表情（`Emotes.used` / `HeyboxParser.cardEmotes`）
// ---------------------------------------------------------------------------

/// `Emotes.used`：挑出 `texts` 里**真的出现过**的表情代码，保持表里的顺序。
pub fn emotes_used(
    table: &[(String, String)],
    texts: &[Option<&str>],
) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let present: Vec<&str> = texts
        .iter()
        .flatten()
        .copied()
        .filter(|t| !t.is_empty())
        .collect();
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

/// 信息流卡片上的表情：原始标题 / 摘要里的 `[cube_喜欢]` 在表情表里有图的，按 [`plain`] 去掉前缀
/// 之后的样子（`[喜欢]`）记成键；只收标题 / 摘要里真有的。同名（不同表情包）取先出现的。
fn card_emotes(
    o: &JsonMap,
    table: &[(String, String)],
    title: Option<&str>,
    summary: Option<&str>,
) -> Vec<(String, String)> {
    if table.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(String, String)> = Vec::new();
    for field in ["title", "description"] {
        let raw = json::string(o, field).unwrap_or_default();
        let b = raw.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'[' {
                if let Some((end, name)) = emote_match(b, i) {
                    let full = &raw[i + 1..end - 1];
                    if let Some(img) = table.iter().find(|(c, _)| c == full).map(|(_, u)| u.clone()) {
                        let code = format!("[{}]", &raw[name.0..name.1]);
                        if !found.iter().any(|(c, _)| *c == code) {
                            found.push((code, img));
                        }
                    }
                    i = end;
                    continue;
                }
            }
            i += utf8_len(b[i]).max(1);
        }
    }
    emotes_used(&found, &[title, summary])
}

/// 游戏卡片的 Steam 地址。
pub fn steam_url(app_id: &str) -> String {
    format!("{STEAM_APP}{app_id}")
}
