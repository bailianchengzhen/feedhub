//! 贴吧搜索的**平台层**：两种搜索的请求怎么拼、翻页游标（页码）、状态码与 `{no|error_code}` 的判定、
//! 结果怎么解析。**收发仍然通过宿主**（[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 两个接口（照 `TiebaSearch.kt` / `TiebaSource.globalSearch` / `inForumSearch`）：
//!
//! | Rust | Kotlin | 接口 |
//! |---|---|---|
//! | [`TiebaHostedSource::global_search`] | `globalSearch` | `GET https://tieba.baidu.com/mo/q/search/thread`（**PC salt** 签名） |
//! | [`TiebaHostedSource::in_forum_search`] | `inForumSearch` | `POST https://tiebac.baidu.com/c/s/searchpost`（App salt 签名 + App 头） |
//!
//! `SearchRequest.community` 是空 → 全吧搜索；给了吧名 → 吧内搜索（`kw` = 吧名）。
//!
//! 口径（都是 Kotlin 那边定的，别"优化"）：
//!
//! - **排序映射两套**：全吧搜索 `st`（0 最早 / 2 最相关 / 5 最新）、吧内搜索 `sm`（0 全部 / 1 时间倒序 /
//!   2 相关性）。贴吧**没有"最热"**，`HOT` 两套都映射成"相关"，但能力里没声明，聚合层不会传；
//! - **平台错误码不在这里归类**：`no` / `error_code` 非 0 时当数据带回去（`Parsed::failed`），
//!   归类的唯一一份规则仍是 Kotlin 的 `TiebaSource.mapError`（风控关键词 / 未登录 / 110001 签名）；
//! - **吧内搜索的 `is_floor=1` 是楼层回复**：没有标题、tid 和主题帖重复，解析时**丢掉**（只收主题帖）；
//! - **全吧搜索的 `content` 是纯文本**：贴吧表情被服务端整个拿掉了，
//!   图 / 视频封面在 `post_list[].media[]` 里；`<img>` 那条路留着只是"防形状变回去"；
//! - **`has_more` 是字符串 `"1"`**（`bool()` 认数字字符串，两边的 `JsonExt` 都这样）；
//! - **签名排序是 UTF-16 码元序**（Kotlin 的 `String.compareTo`），不是 Rust 的 UTF-8 字节序——
//!   `tieba::sign` 已经按 Kotlin 的来（搜索词是用户输入，可能带 emoji）。

use crate::host::HttpRequest;
use crate::html::clean_search_text;
use crate::json::{self, JsonMap};
use crate::model::BiliError as TiebaErr;
use crate::tieba::content::emote_url_of;
use crate::tieba::source::{
    form_body, form_headers, is_baidu_host, url_host, TiebaHostedSource, DESKTOP_UA, V12,
};
use crate::tieba::{self, detail, memoised_emoticons, PlatformError, TIEBA};

/// 全吧搜索的路径（网页接口，`tieba.baidu.com`）。
pub const PATH_GLOBAL: &str = "/mo/q/search/thread";
/// 吧内搜索的路径（App 接口，`tiebac.baidu.com`）。
pub const PATH_IN_FORUM: &str = "/c/s/searchpost";

/// 网页接口的 `Referer`（aiotieba 的 `f/search/res`）。
const GLOBAL_REFERER: &str = "https://tieba.baidu.com/f/search/res";

/// 全吧搜索的 `rn`（= `TiebaSource.FRS_PAGE_SIZE`，`SearchCapability.pageSize` 也是它）。
pub const PAGE_SIZE: i32 = 30;

/// 搜索排序（Kotlin `SearchSort`）。贴吧只有两档（没有"最热"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Relevance,
    Latest,
    Hot,
}

impl SearchSort {
    /// 全吧搜索的 `st`（aiotieba `SearchType`）：0 最早 / 2 最相关 / 5 最新。
    pub fn st(self) -> i32 {
        match self {
            SearchSort::Relevance => 2,
            SearchSort::Latest => 5,
            // 贴吧没有"最热"；聚合层不会发（能力里没声明）
            SearchSort::Hot => 2,
        }
    }

    /// 吧内搜索的 `sm`：0 全部 / 1 时间倒序 / 2 相关性。
    pub fn sm(self) -> i32 {
        match self {
            SearchSort::Relevance => 2,
            SearchSort::Latest => 1,
            SearchSort::Hot => 2,
        }
    }
}

/// 全吧搜索的参数（签名前，顺序照 Kotlin `TiebaSearch.globalParams`）。
pub fn global_params(text: &str, sort: SearchSort, pn: i32, rn: i32) -> Vec<(String, String)> {
    vec![
        ("word".to_string(), text.to_string()),
        ("pn".to_string(), pn.to_string()),
        ("rn".to_string(), rn.to_string()),
        ("st".to_string(), sort.st().to_string()),
        ("tt".to_string(), "1".to_string()),
        ("subapp_type".to_string(), "pc".to_string()),
        ("_client_type".to_string(), "20".to_string()),
    ]
}

/// 吧内搜索的表单（签名前，顺序照 Kotlin `TiebaSearch.forumForm`）。
///
/// `only_thread=0`：服务端会把楼层回复也列出来（解析时按 `is_floor` 丢掉）。
/// `BDUSS` 只在登录时出现，位置在最后。
pub fn forum_form(
    device: &crate::tieba::device::Device,
    bduss: Option<&str>,
    community: &str,
    text: &str,
    sort: SearchSort,
    pn: i32,
    rn: i32,
) -> Vec<(String, String)> {
    let mut form = vec![
        ("_client_id".to_string(), device.client_id.clone()),
        ("_client_type".to_string(), "2".to_string()),
        ("_client_version".to_string(), V12.to_string()),
        ("kw".to_string(), community.to_string()),
        ("only_thread".to_string(), "0".to_string()),
        ("pn".to_string(), pn.to_string()),
        ("rn".to_string(), rn.to_string()),
        ("sm".to_string(), sort.sm().to_string()),
        ("word".to_string(), text.to_string()),
    ];
    if let Some(bduss) = bduss {
        form.push(("BDUSS".to_string(), bduss.to_string()));
    }
    form
}

impl TiebaHostedSource {
    /// 全吧搜索（网页 GET，PC salt 签名）。`Referer` 照 aiotieba 的 `f/search/res`。
    ///
    /// 平台错误码（`no` 非 0）当数据带回去（[`detail::Parsed::failed`]），归类仍走 Kotlin 的 `mapError`。
    pub fn global_search(
        &self,
        text: &str,
        sort: SearchSort,
        cursor: Option<&str>,
    ) -> Result<detail::Parsed<crate::model::Page>, TiebaErr> {
        let pn = page_of(cursor);
        let params = global_params(text, sort, pn, PAGE_SIZE);
        // PC salt 签名，参数按 key 排序后追加 `sign`
        let signed = tieba::sign::signed(&params, tieba::sign::PC_SALT);
        // query 的编码口径是 **OkHttp `addQueryParameter`**（Kotlin 侧是 `.toHttpUrl().newBuilder()
        // .addQueryParameter(k, v)`），不是 `URLEncoder`：空格是 `%20`、`~` 也编。
        let query = signed
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    crate::hosted::query_encode(k),
                    crate::hosted::query_encode(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{}{PATH_GLOBAL}?{query}", self.web_base());
        // 头顺序照 Kotlin：`Request.Builder().url(url).header("Referer", …)` 之后，**没有** `Subapp-Type`
        // （那是 `forumGuide` 才写的），然后 `DefaultHeadersInterceptor` 只补缺失的同名头——
        // `Referer` 已有所以跳过，只追加 `User-Agent`（百度域名才补）。GET 没有 body，所以也没有 `Content-Type`。
        // 最终：`Referer` → `User-Agent`。
        let mut headers = vec![("Referer".to_string(), GLOBAL_REFERER.to_string())];
        if is_baidu_host(&url_host(&url)) {
            headers.push(("User-Agent".to_string(), DESKTOP_UA.to_string()));
        }
        let response = self.http_web.clone().execute(HttpRequest {
            method: "GET".to_string(),
            url,
            headers,
            body: Vec::new(),
        })?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(TiebaErr::Api {
                code: format!("HTTP {}", response.status),
                detail: "全吧搜索".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|detail| TiebaErr::Parse { detail })?;
        let code = json::long(&root, "no").ok_or_else(|| TiebaErr::Parse {
            detail: "全吧搜索缺少 no".to_string(),
        })?;
        if code != 0 {
            return Ok(detail::Parsed::failed(PlatformError {
                code: code as i32,
                user_msg: String::new(),
                error_msg: json::text(&root, "error").unwrap_or_default(),
            }));
        }
        parse_global(&root)
    }

    /// 吧内搜索（App 表单 POST，App salt 签名 + App 头）。
    pub fn in_forum_search(
        &self,
        community: &str,
        text: &str,
        sort: SearchSort,
        cursor: Option<&str>,
        now_ms: i64,
    ) -> Result<detail::Parsed<crate::model::Page>, TiebaErr> {
        let pn = page_of(cursor);
        let device = self.device()?;
        let credentials = self.credentials();
        let form = forum_form(
            &device,
            credentials.as_ref().map(|c| c.bduss.as_str()),
            community,
            text,
            sort,
            pn,
            PAGE_SIZE,
        );
        let signed = tieba::sign::signed(&form, tieba::sign::APP_SALT);
        let pairs: Vec<(&str, &str)> = signed.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let url = format!("{}{PATH_IN_FORUM}", self.app_base());
        // 头顺序照 Kotlin：`form_headers` 那些（UA / Charset / client_type / client_logid / Cookie / cuid…）
        // 之后是 **`Content-Type`**——Kotlin 那边它来自 `FormBody` 的 media type，由 OkHttp 的
        // `BridgeInterceptor` 补在应用拦截器**之后**（= 末尾），所以这里显式写在最后一格
        // （`Hosts.kt` 给 Rust 的 body 没有 media type，Bridge 不会替我们加）。
        let mut headers = form_headers(&device, now_ms);
        headers.push((
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ));
        let response = self.http_app.clone().execute(HttpRequest {
            method: "POST".to_string(),
            url,
            headers,
            body: form_body(&pairs),
        })?;
        if let Some(e) = crate::tieba::risk_status(response.status) {
            return Err(e);
        }
        if !(200..=299).contains(&response.status) {
            return Err(TiebaErr::Api {
                code: format!("HTTP {}", response.status),
                detail: "吧内搜索".to_string(),
            });
        }
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let root = json::parse_object(&text).map_err(|detail| TiebaErr::Parse { detail })?;
        let code = json::long(&root, "error_code").ok_or_else(|| TiebaErr::Parse {
            detail: "吧内搜索缺少 error_code".to_string(),
        })?;
        if code != 0 {
            return Ok(detail::Parsed::failed(PlatformError {
                code: code as i32,
                user_msg: String::new(),
                error_msg: json::text(&root, "error_msg").unwrap_or_default(),
            }));
        }
        parse_in_forum(&root, pn)
    }
}

/// 游标 → 页码（Kotlin `cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1`）。
fn page_of(cursor: Option<&str>) -> i32 {
    cursor
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| v.max(1))
        .unwrap_or(1)
}

/// 全吧搜索响应 → 一页。
///
/// 平台错误码（`no`）不在这里判——调用方先看 `no`，非 0 就当数据带回去。
pub fn parse_global_page(root: &JsonMap) -> Result<crate::model::Page, TiebaErr> {
    Ok(parse_global(root)?.value.unwrap_or(crate::model::Page {
        items: Vec::new(),
        next: None,
    }))
}

/// 吧内搜索响应 → 一页（**纯函数**，同上）。
pub fn parse_in_forum_page(root: &JsonMap, pn: i32) -> Result<crate::model::Page, TiebaErr> {
    Ok(parse_in_forum(root, pn)?.value.unwrap_or(crate::model::Page {
        items: Vec::new(),
        next: None,
    }))
}

/// 全吧搜索响应 → `Feed`（`detail::Parsed` 的 `Ok` 那一支）。
fn parse_global(root: &JsonMap) -> Result<detail::Parsed<crate::model::Page>, TiebaErr> {
    let data = json::obj(root, "data").ok_or_else(|| TiebaErr::Parse {
        detail: "全吧搜索缺少 data".to_string(),
    })?;
    let posts = json::arr(data, "post_list").ok_or_else(|| TiebaErr::Parse {
        detail: "全吧搜索缺少 data.post_list".to_string(),
    })?;
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in posts {
        let Some(o) = el.as_object() else { continue };
        if let Some(item) = global_item(o) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }
    }
    let has_more = json::bool(data, "has_more") == Some(true);
    let current = json::int(data, "current_page").unwrap_or(1);
    let next = if has_more && !items.is_empty() {
        Some((current + 1).to_string())
    } else {
        None
    };
    Ok(detail::Parsed::ok(crate::model::Page { items, next }))
}

/// 吧内搜索响应 → `Feed`。
fn parse_in_forum(root: &JsonMap, pn: i32) -> Result<detail::Parsed<crate::model::Page>, TiebaErr> {
    let posts = json::arr(root, "post_list").ok_or_else(|| TiebaErr::Parse {
        detail: "吧内搜索缺少 post_list".to_string(),
    })?;
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in posts {
        let Some(o) = el.as_object() else { continue };
        if let Some(item) = forum_item(o) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }
    }
    let has_more = json::obj(root, "page")
        .map(|p| json::bool(p, "has_more") == Some(true))
        .unwrap_or(false);
    let next = if has_more && !items.is_empty() {
        Some((pn + 1).to_string())
    } else {
        None
    };
    Ok(detail::Parsed::ok(crate::model::Page { items, next }))
}

/// 一条全吧搜索结果。
fn global_item(o: &JsonMap) -> Option<crate::model::FeedItem> {
    use crate::model::{DetailTarget, FeedItem, ItemKind, UserRef};
    let tid = json::text(o, "tid")?;
    let title = json::text(o, "title")
        .map(|t| clean_search_text(&t))
        .filter(|t| !t.is_empty());
    let content = json::text(o, "content").map(|c| plain(&c)).filter(|c| !c.text.is_empty());
    let user = json::obj(o, "user");
    let author_name = user
        .and_then(|u| json::text(u, "show_nickname").filter(|n| !n.is_empty()))
        .or_else(|| user.and_then(|u| json::text(u, "user_name").filter(|n| !n.is_empty())))
        .unwrap_or_else(|| "匿名用户".to_string());
    let forum = json::text(o, "forum_name").filter(|f| !f.is_empty());
    let (summary, emotes) = match content {
        Some(c) => (Some(c.text), c.emotes),
        None => (None, Vec::new()),
    };
    Some(FeedItem {
        platform: TIEBA.to_string(),
        id: tid.clone(),
        kind: ItemKind::Post,
        title,
        summary,
        language: None,
        author: Some(UserRef {
            platform: TIEBA.to_string(),
            id: user.and_then(|u| json::string(u, "user_id")).unwrap_or_default(),
            name: author_name,
            avatar: None,
            url: None,
        }),
        published_at: json::long(o, "create_time").filter(|t| *t > 0),
        published_at_precision: crate::model::TimePrecision::Second,
        media: cover_of(o),
        stats: crate::model::Stats {
            comments: json::long(o, "post_num"),
            ..Default::default()
        },
        reason: None,
        community: forum,
        community_icon: None,
        repost: None,
        emotes,
        adult: false,
        url: Some(tieba::thread_url(&tid)),
        detail: DetailTarget::Native {
            id: tid,
            platform: None,
        },
    })
}

/// 一条吧内搜索结果。`is_floor=1` 是楼层回复：**丢掉**（没有标题、tid 和主题帖重复）。
fn forum_item(o: &JsonMap) -> Option<crate::model::FeedItem> {
    use crate::model::{DetailTarget, FeedItem, ItemKind, UserRef};
    if json::text(o, "is_floor").as_deref() == Some("1") {
        return None;
    }
    let tid = json::text(o, "tid")?;
    let title = json::text(o, "title")
        .map(|t| clean_search_text(&t))
        .filter(|t| !t.is_empty());
    let content = json::text(o, "content").map(|c| plain(&c)).filter(|c| !c.text.is_empty());
    let author = json::obj(o, "author");
    let name = author
        .and_then(|a| json::text(a, "name_show").filter(|n| !n.is_empty()))
        .or_else(|| author.and_then(|a| json::text(a, "name").filter(|n| !n.is_empty())))
        .unwrap_or_else(|| "匿名用户".to_string());
    let forum = json::text(o, "fname").filter(|f| !f.is_empty());
    let (summary, emotes) = match content {
        Some(c) => (Some(c.text), c.emotes),
        None => (None, Vec::new()),
    };
    Some(FeedItem {
        platform: TIEBA.to_string(),
        id: tid.clone(),
        kind: ItemKind::Post,
        title,
        summary,
        language: None,
        author: Some(UserRef {
            platform: TIEBA.to_string(),
            id: author.and_then(|a| json::string(a, "id")).unwrap_or_default(),
            name,
            avatar: None,
            url: None,
        }),
        published_at: json::long(o, "time").or_else(|| json::long(o, "create_time")).filter(|t| *t > 0),
        published_at_precision: crate::model::TimePrecision::Second,
        media: cover_of(o),
        stats: crate::model::Stats {
            comments: json::long(o, "post_num"),
            likes: json::long(o, "like_num"),
            ..Default::default()
        },
        reason: None,
        community: forum,
        community_icon: None,
        repost: None,
        emotes,
        adult: false,
        url: Some(tieba::thread_url(&tid)),
        detail: DetailTarget::Native {
            id: tid,
            platform: None,
        },
    })
}

/// 摘要的纯文本 + 里面真的出现的表情代码（`[滑稽]` → 官方表情图地址）。
///
/// 帖子里的图片 / 视频在 `post_list[].media[]` 里，贴吧表情被服务端整个拿掉了。所以正常情况下这里就是
/// [`clean_search_text`] + 认出文字里**已知**的表情代码；认不得的方括号文字（`[受虐滑稽]` 这种用户手打的）
/// 原样留着，不猜。
///
/// `<img>` 这条路留着是**防形状变回去**（老版本 / 别的接口确实给过带 `<img>` 的 HTML）：
/// `src` 是 `…/image_emoticon<n>.png` 的换成对应的文字代码（认识那个 `<n>` 时），其余图片照旧丢掉。
pub struct Plain {
    pub text: String,
    pub emotes: Vec<(String, String)>,
}

pub fn plain(html: &str) -> Plain {
    let text = if html.to_ascii_lowercase().contains("<img") {
        clean_search_text(&emoticon_codes(html))
    } else {
        clean_search_text(html)
    };
    let emotes = emoticons_used(&text);
    Plain { text, emotes }
}

/// 把 HTML 里官方表情的 `<img>` 换成 `[名字]`（认不出是哪个表情的仍然丢掉，交给 [`clean_search_text`] 剥标签）。
fn emoticon_codes(html: &str) -> String {
    // `<img[^>]*>`：忽略大小写、非贪婪（`[^>]*` 本身就是到第一个 `>` 为止）
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' && i + 3 < bytes.len() && html[i..].to_ascii_lowercase().starts_with("<img") {
            if let Some(rel) = html[i..].find('>') {
                let tag = &html[i..i + rel + 1];
                if let Some(name) = emote_name_of_tag(tag) {
                    out.push_str(&format!("[{name}]"));
                }
                i += rel + 1;
                continue;
            }
        }
        let len = if bytes[i] >= 0x80 {
            crate::html::utf8_len(bytes[i])
        } else {
            1
        };
        out.push_str(&html[i..(i + len).min(html.len())]);
        i += len;
    }
    out
}

/// `<img … src="…/image_emoticon25.png" …>` → 贴吧表情的中文名（认不出就是 `None`）。
fn emote_name_of_tag(tag: &str) -> Option<&'static str> {
    let src = attr_value(tag, "src")?;
    let file = src.rsplit('/').next()?;
    let stem = file.split('.').next()?;
    emote_name_of_file(stem)
}

/// `src=["']([^"']+)["']`（Kotlin `TiebaSearch.SRC`，忽略大小写、**没有单词边界**：`data-src=` 也算，
/// 因为 Kotlin 那条正则就是直接 `find` 的）。`=` 前后不许有空白（正则里也没有）。
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut from = 0usize;
    while let Some(at) = lower[from..].find(&needle) {
        let i = from + at + needle.len();
        let bytes = lower.as_bytes();
        let quote = *bytes.get(i)?;
        if quote != b'"' && quote != b'\'' {
            from = i;
            continue;
        }
        let end = tag[i + 1..].find(quote as char)? + i + 1;
        return Some(&tag[i + 1..end]);
    }
    None
}

/// 卡片封面。两条路（照 Kotlin `TiebaSearch.coverOf`）：
///
/// 1. `content` 里第一张不是表情的图（老形状，HTML）；
/// 帖子自己的图 / 视频封面全在
///    `post_list[].media[]`（`{type:pic, water_pic, big_pic, small_pic}` / `{type:flash, vpic}`）。
fn cover_of(o: &JsonMap) -> Vec<crate::model::Media> {
    if let Some(url) = first_content_image(json::text(o, "content").as_deref().unwrap_or("")) {
        return vec![crate::model::Media::Image {
            url,
            width: None,
            height: None,
        }];
    }
    for el in json::arr(o, "media").map(|v| v.as_slice()).unwrap_or_default() {
        let Some(picture) = el.as_object() else { continue };
        for key in ["water_pic", "big_pic", "small_pic", "vpic"] {
            if let Some(value) = json::text(picture, key).filter(|v| !v.is_empty()) {
                return vec![crate::model::Media::Image {
                    url: json::to_https(&value),
                    width: None,
                    height: None,
                }];
            }
        }
    }
    Vec::new()
}

/// `content` 里第一张 `src`（不是官方表情那张）。
fn first_content_image(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' && html[i..].to_ascii_lowercase().starts_with("<img") {
            if let Some(rel) = html[i..].find('>') {
                let tag = &html[i..i + rel + 1];
                if let Some(src) = attr_value(tag, "src") {
                    let url = json::to_https(src);
                    if !url.contains("tb1.bdstatic.com") {
                        return Some(url);
                    }
                }
                i += rel + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// 文字里真的出现过的表情代码 → 图片地址。
fn emoticons_used(text: &str) -> Vec<(String, String)> {
    if text.is_empty() || !text.contains('[') {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (name, file) in memoised_emoticons() {
        let code = format!("[{name}]");
        if text.contains(&code) {
            if let Some(url) = emote_url_of(file) {
                out.push((code, url));
            }
        }
    }
    out
}

/// `image_emoticon<n>` → 中文名（`TiebaEmoticons.nameOf`）；认不出是 `None`。
pub fn emote_name_of_file(file_id: &str) -> Option<&'static str> {
    let normalized = if file_id == "image_emoticon" {
        "image_emoticon1"
    } else {
        file_id
    };
    memoised_emoticons()
        .iter()
        .find(|(_, file)| *file == normalized)
        .map(|(name, _)| *name)
}

