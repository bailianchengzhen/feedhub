//! **中转地址映射**（`app/desktopMain/.../video/VideoRelay.kt` 的 `RelayUrls` 那一半）。
//!
//! ## 它解决什么
//!
//! GStreamer 的 dashdemux2 / hlsdemux2 用它们**自己的** libsoup session 下载分片，不会产生 `souphttpsrc`、
//! 也不会回调 `source-setup`，所以"给清单设代理"对分片没用——配了代理也是分片直连 CDN。所以清单里所有远端
//! 地址都改写成**本机回环**地址（`http://127.0.0.1:<port>/…`），由播放器去这里取；中转再用**这个平台的
//! OkHttp 客户端**去取真正的分片。于是平台代理、fail-closed、Referer / UA 全都跟着 OkHttp 那一份走。
//!
//! ## 这一层管什么、不管什么
//!
//! **管**（纯逻辑，搬进了 Rust）：
//! - 每个远端地址 → 本机地址的**映射表**：同一个远端地址只登记一次（直播的播放列表反复改写时不会把表撑大）；
//! - 本机地址的**形状**：`<base>/s/<id><ext>`（资源）、`<base>/r/<id><ext>`（远端清单）、
//!   `<base>/m/<id><ext>`（我们自己拼的清单）、`<base>/t/<id>?p0=…&p1=…`（分片模板）；
//! - 扩展名（dashdemux / hlsdemux 认扩展名 / 内容类型）、`$Number$` 这类**占位符**的抽取与还原；
//! - `Range` 的解析（`bytes=0-`、`bytes=10-19`、`bytes=-5`、越界）；
//! - "这个请求该干什么"的判定（[`Route`]）：取本机清单 / 取远端清单并改写 / 转发分片 / 还原模板 / 404 / 400。
//!
//! **不管**（宿主）：HTTP 服务器（`com.sun.net.httpserver`）、OkHttp 收发、把响应写回 socket。
//! 宿主收到一个请求就问 [`Relay::route`] 要一个 [`Route`]，照着做就行——**判定全在这一层**。
//!
//! ## 为什么要一个 `id`
//!
//! 本机地址里的 `id` 是 `RelayUrls::resource` 之类的返回值的一部分，**由宿主生成**（Kotlin 用
//! `UUID.randomUUID()`）：Rust 这边不该为了"生成一个随机串"把随机源搬进来（那会让"同一份输入两次
//! 调用的结果一样"这条不成立，测试也没法定）。所以 [`Relay`] 持有一张**双向表**：
//! `远端地址 → id`（去重）和 `id → 条目`（路由用），`id` 由宿主给。

use std::collections::HashMap;

/// 改写清单时用来登记地址的口子（[`Relay`] 实现，测试里换成记账用的假实现）。
///
/// 三个方法都返回**本机中转地址**，播放器只会去连它们。
pub trait RelayUrls {
    /// 登记一个完整的远端地址（单个文件、一个分片、一把密钥），返回本机地址。
    fn resource(&mut self, remote_url: &str) -> String;

    /// 登记一个远端播放列表 / 清单（m3u8，或远端 `.mpd`）：播放器取的时候现取现改写。
    fn playlist(&mut self, remote_url: &str) -> String;

    /// 登记一个远端分片模板（带 `$Number$` / `$Time$` 这类占位符），返回本机模板地址。
    fn template(&mut self, remote_template: &str) -> String;
}

/// 一个登记项（Kotlin `VideoRelay.Entry`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// 中转自己生成、缓存在内存里的清单（我们拼的 MPD，已经改写过）。
    Local {
        body: String,
        content_type: Option<String>,
    },
    /// 远端清单 / 播放列表：每次请求现取现改写（直播的 m3u8 每次都可能是新的）。
    Remote { url: String, dash: bool },
    /// 远端的一个具体资源（分片、单文件、密钥）：`Range` 原样转发。
    Resource { url: String },
    /// 远端的分片模板（`$Number$` 这类占位符）。
    Template {
        remote: String,
        tokens: Vec<String>,
    },
}

impl Entry {
    /// 中转自己给的内容类型（远端资源是 `None` = 用上游的）。
    pub fn content_type(&self) -> Option<&str> {
        match self {
            Entry::Local { content_type, .. } => content_type.as_deref(),
            Entry::Remote { dash, .. } => Some(if *dash { DASH_TYPE } else { HLS_TYPE }),
            _ => None,
        }
    }
}

/// 中转自己给的 MPD 的类型。
pub const DASH_TYPE: &str = "application/dash+xml";

/// 中转自己给的 m3u8 的类型。
pub const HLS_TYPE: &str = "application/vnd.apple.mpegurl";

/// 宿主收到一个中转请求之后该干什么（[`Relay::route`] 的返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// 回 404（没有这个 id）。
    NotFound,
    /// 回 400（模板的占位符对不上）。
    BadTemplate,
    /// 回这段内存里的正文（我们拼的清单 / 改写好的清单）。
    Body {
        body: String,
        content_type: Option<String>,
    },
    /// 去远端取这份清单，然后用 [`crate::player::manifest::rewrite_dash`] /
    /// [`crate::player::manifest::rewrite_hls`] 改写（`dash == true` 用前者）。
    RemoteManifest { url: String, dash: bool },
    /// 把一个远端资源（分片 / 单文件 / 密钥）按原样转发（带上播放器给的 `Range` / `If-Range`）。
    Upstream { url: String },
}

/// 远端地址的扩展名（最多 5 个字母数字），拿去给本机地址用：dashdemux / hlsdemux 认扩展名 / 内容类型。
pub fn ext_of(url: &str) -> String {
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or("");
    match name.rfind('.') {
        None => String::new(),
        Some(i) => {
            let ext = &name[i + 1..];
            if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                format!(".{}", ext.to_ascii_lowercase())
            } else {
                String::new()
            }
        }
    }
}

/// [url] 的主机是不是本机回环（`127.0.0.0/8`、`::1`、`localhost`）；解析不出来的地址当不是。
///
/// 为什么要它：中转出来的清单全在 `127.0.0.1` 上，而 GStreamer 的 `souphttpsrc` 初始化时会把 env
/// `http_proxy` 记成显式代理、**不忽略** localhost，所以取本机清单那一路必须显式"不代理"。
pub fn is_loopback_url(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h == "localhost" || h == "::1" || h == "0:0:0:0:0:0:0:1" {
        return true;
    }
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4
        && parts[0] == "127"
        && parts.iter().all(|p| p.parse::<u16>().map(|v| v <= 255).unwrap_or(false))
}

/// `scheme://host[:port][/path]` 里的 host（小写化之前的样子；`[]` 保留）。
fn host_of(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // userinfo@host
    let authority = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6：[::1]:port
        let close = stripped.find(']')?;
        return Some(&authority[..close + 2]);
    }
    let host = match authority.rfind(':') {
        Some(i) => &authority[..i],
        None => authority,
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// 远端地址是不是一份 DASH 清单。
pub fn is_dash_url(url: &str) -> bool {
    url.split('?')
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".mpd")
}

/// 分片模板里的占位符：`$Number$`、`$RepresentationID$`、`$Time$`、`$Number%05d$` 都算。
pub fn template_tokens(template: &str) -> Vec<String> {
    let bytes = template.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            if let Some(end) = template[i + 1..].find('$') {
                let body = &template[i + 1..i + 1 + end];
                if is_template_body(body) {
                    out.push(format!("${body}$"));
                    i += end + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// `$…$` 里面的内容算不算一个占位符（`[A-Za-z0-9_]+` 加可选的 `%0Nd` / `%d` 格式）。
fn is_template_body(body: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    let mut chars = body.chars().peekable();
    let mut name_len = 0;
    while let Some(c) = chars.peek() {
        if c.is_ascii_alphanumeric() || *c == '_' {
            name_len += 1;
            chars.next();
        } else {
            break;
        }
    }
    if name_len == 0 {
        return false;
    }
    let rest: String = chars.collect();
    if rest.is_empty() {
        return true;
    }
    let Some(mut fmt) = rest.strip_prefix('%') else {
        return false;
    };
    // `%0Nd` 或 `%d`：可选的 0 + 位数 + 字母
    while fmt.starts_with('0') {
        fmt = &fmt[1..];
    }
    let digits = fmt.chars().take_while(|c| c.is_ascii_digit()).count();
    let fmt = &fmt[digits..];
    fmt.len() == 1 && fmt.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
}

/// 用播放器填好的占位符值还原出真正的上游地址（`values` 的长度必须和 `tokens` 一样）。
///
/// **按参数名 `p0` / `p1` … 读、不按出现顺序读**：本机地址是
/// `…?p0=$RepresentationID$&p1=$Number%05d$`，但 dashdemux2 填完之后**会重排查询参数**
/// ，按位置读就会把两个值对调，还原出 `chunk-stream00001-0.m4s`
/// 这种不存在的地址 → 上游 404 → "Couldn't download fragments"。参数名是我们自己定的，
/// 按名字取就不受顺序影响（[`query_values`]）。
pub fn resolve_template(remote: &str, tokens: &[String], values: &[String]) -> Option<String> {
    if values.len() != tokens.len() {
        return None;
    }
    let mut out = String::with_capacity(remote.len() + 32);
    let mut rest = remote;
    for value in values {
        let Some(at) = rest.find('$') else {
            return None;
        };
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('$') else {
            return None;
        };
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// 把播放器填好的占位符值取回来：按**参数名** `p0` / `p1` … 读。
///
/// `count == 0` 时不需要查询串（模板里没有占位符）。缺一个就是 `None`（路由会回 400）。
pub fn query_values(raw_query: Option<&str>, count: usize) -> Option<Vec<String>> {
    if count == 0 {
        return Some(Vec::new());
    }
    let parts: Vec<&str> = raw_query?.split('&').collect();
    let mut by_name: HashMap<&str, &str> = HashMap::with_capacity(parts.len());
    for part in parts {
        if let Some(eq) = part.find('=') {
            if eq > 0 {
                by_name.insert(&part[..eq], &part[eq + 1..]);
            }
        }
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(by_name.get(format!("p{i}").as_str())?.to_string());
    }
    Some(out)
}

/// `bytes=a-b` / `bytes=-n` 解析成闭区间；解析不出来按"整段"处理。
///
/// - `bytes=0-` → `(0, total-1)`
/// - `bytes=10-19` → `(10, 19)`
/// - `bytes=-5` → `(total-5, total-1)`（不足 5 就从 0 开始）
/// - `bytes=10-1000` → `(10, total-1)`（尾巴超界夹住）
/// - 起点越界 / 不是一个字节范围 / `total <= 0` → `None`（当整段处理）
pub fn slice_for(range: Option<&str>, total: usize) -> Option<(usize, usize)> {
    let range = range?;
    if total == 0 {
        return None;
    }
    let text = crate::hosted::kotlin_trim(range);
    let body = text.strip_prefix("bytes=")?;
    let dash = body.find('-')?;
    let (a, b) = (&body[..dash], &body[dash + 1..]);
    if a.is_empty() && b.is_empty() {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let n: usize = b.parse().ok()?;
        (total.saturating_sub(n), total - 1)
    } else {
        let start: usize = a.parse().ok()?;
        let end = if b.is_empty() {
            total - 1
        } else {
            let b: usize = b.parse().ok()?;
            b.min(total - 1)
        };
        (start, end)
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

/// 中转的映射表 + 路由（Kotlin `VideoRelay` 的登记 / 路由那一半）。
///
/// `base` 是播放器看到的基地址（`http://127.0.0.1:<port>`，端口由宿主的服务器决定）。
#[derive(Debug, Clone)]
pub struct Relay {
    base: String,
    /// `id → 条目`（路由用）。
    entries: HashMap<String, Entry>,
    /// 远端地址 → id（同一个远端地址只登记一次）。
    ids: HashMap<String, String>,
    /// 登记顺序（调试 / 检查工具用；也是"这张表有多大"的答案）。
    order: Vec<String>,
}

impl Relay {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            entries: HashMap::new(),
            ids: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 登记了多少条。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 登记过的 id（登记顺序）。
    pub fn registered_ids(&self) -> Vec<String> {
        self.order.clone()
    }

    /// 按 id 取条目（测试 / 检查工具用）。
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// 登记一个远端资源，`id` 由宿主生成（同一个远端地址再来就用原来的 id）。
    pub fn register_resource(&mut self, remote_url: &str, id: &str) -> String {
        let key = format!("res:{remote_url}");
        let id = self.upsert(&key, id, Entry::Resource {
            url: remote_url.to_string(),
        });
        format!("{}/s/{id}{}", self.base, ext_of(remote_url))
    }

    /// 登记一个远端清单 / 播放列表（`.mpd` 现取现改写，其余当 m3u8）。
    pub fn register_playlist(&mut self, remote_url: &str, id: &str) -> String {
        let dash = is_dash_url(remote_url);
        let key = format!("man:{remote_url}");
        let id = self.upsert(&key, id, Entry::Remote {
            url: remote_url.to_string(),
            dash,
        });
        let ext = {
            let e = ext_of(remote_url);
            if e.is_empty() {
                if dash {
                    ".mpd".to_string()
                } else {
                    ".m3u8".to_string()
                }
            } else {
                e
            }
        };
        format!("{}/r/{id}{ext}", self.base)
    }

    /// 登记一个远端分片模板，返回带占位符参数名的本机模板地址。
    pub fn register_template(&mut self, remote_template: &str, id: &str) -> String {
        let tokens = template_tokens(remote_template);
        let key = format!("tpl:{remote_template}");
        let id = self.upsert(&key, id, Entry::Template {
            remote: remote_template.to_string(),
            tokens: tokens.clone(),
        });
        let query: Vec<String> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| format!("p{i}={t}"))
            .collect();
        if query.is_empty() {
            format!("{}/t/{id}", self.base)
        } else {
            format!("{}/t/{id}?{}", self.base, query.join("&"))
        }
    }

    /// 放一份我们自己生成、已经改写好的清单（[`crate::player::manifest::dash_manifest`] +
    /// [`crate::player::manifest::rewrite_dash`] 的产物）。
    pub fn register_local(&mut self, body: &str, content_type: Option<&str>, id: &str) -> String {
        self.entries.insert(
            id.to_string(),
            Entry::Local {
                body: body.to_string(),
                content_type: content_type.map(|s| s.to_string()),
            },
        );
        self.order.push(id.to_string());
        format!("{}/m/{id}.mpd", self.base)
    }

    fn upsert(&mut self, key: &str, id: &str, entry: Entry) -> String {
        if let Some(existing) = self.ids.get(key) {
            return existing.clone();
        }
        self.ids.insert(key.to_string(), id.to_string());
        self.entries.insert(id.to_string(), entry);
        self.order.push(id.to_string());
        id.to_string()
    }

    /// 播放器发来一个请求：`path` 是 `requestURI` 的原始路径（`/s/<id>.mp4` 这种），
    /// `raw_query` 是原始查询串（模板的 `p0` / `p1`）。
    pub fn route(&self, path: &str, raw_query: Option<&str>) -> Route {
        let raw = path.trim_start_matches('/');
        let rest = raw.split_once('/').map(|(_, r)| r).unwrap_or("");
        let id = rest.split(&['/', '.'][..]).next().unwrap_or("");
        if id.is_empty() {
            return Route::NotFound;
        }
        let Some(entry) = self.entries.get(id) else {
            return Route::NotFound;
        };
        match entry {
            Entry::Template { remote, tokens } => match query_values(raw_query, tokens.len())
                .and_then(|values| resolve_template(remote, tokens, &values))
            {
                Some(url) => Route::Upstream { url },
                None => Route::BadTemplate,
            },
            Entry::Local { body, content_type } => Route::Body {
                body: body.clone(),
                content_type: content_type.clone(),
            },
            Entry::Remote { url, dash } => Route::RemoteManifest {
                url: url.clone(),
                dash: *dash,
            },
            Entry::Resource { url } => Route::Upstream { url: url.clone() },
        }
    }
}

impl RelayUrls for Relay {
    /// **测试 / 检查工具用**：`id` 由 [`Relay`] 按登记顺序生成（`e0` / `e1` …）。
    ///
    /// 生产路径走 [`Relay::register_resource`]，`id` 是宿主的 UUID——这样"同一份清单改写出同样的本机地址"
    /// 这句话在对拍里成立（Kotlin 那边是随机的，只能比"结构"）。
    fn resource(&mut self, remote_url: &str) -> String {
        let id = format!("e{}", self.order.len());
        self.register_resource(remote_url, &id)
    }

    fn playlist(&mut self, remote_url: &str) -> String {
        let id = format!("e{}", self.order.len());
        self.register_playlist(remote_url, &id)
    }

    fn template(&mut self, remote_template: &str) -> String {
        let id = format!("e{}", self.order.len());
        self.register_template(remote_template, &id)
    }
}

