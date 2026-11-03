//! 插件描述的**自检**。
//!
//! 只查"描述自己合不合理"：id / 显示名 / 颜色 / 域名 / 能力 / 端 / 默认值 / 登录域 / Chrome 导入域 /
//! 表情表 / 搜索能力。**不建 Source、不发请求**——"描述和它建出来的 Source 对不对得上"那一半要构造对象，
//! 留在 Kotlin（`PluginContract.check`），这里只收描述这份数据。
//!
//! **文案逐字照抄 Kotlin**：每个插件模块的离线测试拿到的就是这个字符串列表（空 = 通过），
//! 改一个字都是行为变更。判定里的两个正则也是手写的（仓库离线，没有正则 crate）：
//!
//! - [`PLATFORM_ID`]：`[a-z][a-z0-9_-]{0,31}`（Kotlin `Platform.ID`）；
//! - 域名：`[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+`（见 [`is_host`]）。
//!
//! Kotlin 的 `String.isBlank()` 用 [`crate::hosted::is_kotlin_blank`]（`Char.isWhitespace()`
//! 的真口径，含 NBSP 类三个字符），`Long.toString(16)` 用反过来写的 [`long_to_string_radix`]
//! （负数是 `-` + 绝对值，不是补码）。

/// Kotlin `Platform.ID`：`[a-z][a-z0-9_-]{0,31}`（整串匹配）。
pub fn is_platform_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !(bytes[0].is_ascii_lowercase()) {
        return false;
    }
    bytes[1..].iter().all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-'
    })
}

/// 插件描述的 hosts 那条正则：`[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+`。
///
/// 也就是"至少两段、点分、每段首尾是 [a-z0-9]、中间可以带 `-`、整串匹配"：
/// `bilibili.com` / `www.bilibili.com` / `api.x-y.co` 过；`.bilibili.com` / `bilibili` / `a..b` / `A.com` 不过。
pub fn is_host(host: &str) -> bool {
    if host.is_empty() || !host.is_ascii() {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| is_host_label(label))
}

/// 域名的一段：`[a-z0-9]([a-z0-9-]*[a-z0-9])?`（单字符合法）。
fn is_host_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    match bytes.first() {
        None => return false,
        Some(c) if !c.is_ascii_lowercase() && !c.is_ascii_digit() => return false,
        _ => {}
    }
    match bytes.last() {
        Some(c) if !c.is_ascii_lowercase() && !c.is_ascii_digit() => return false,
        _ => {}
    }
    bytes
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `java.lang.Long.toString(value, radix)`：负数是 `-` + 绝对值，字母小写。
pub fn long_to_string_radix(value: i64, radix: u32) -> String {
    let radix = radix.clamp(2, 36) as u64;
    if value == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let negative = value < 0;
    let mut n = if negative {
        (value as i128).unsigned_abs() as u64
    } else {
        value as u64
    };
    let mut buf = Vec::with_capacity(16);
    while n > 0 {
        buf.push(DIGITS[(n % radix) as usize]);
        n /= radix;
    }
    if negative {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

// ===========================================================================
// 描述（Kotlin `PlatformPlugin` 的纯数据那部分）
// ===========================================================================

/// `CookieImportSpec`（Chrome 导入的收 Cookie 规则）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChromeImport {
    #[allow(dead_code)]
    pub domain: String,
    pub suffixes: Vec<String>,
    pub exact_hosts: Vec<String>,
}

impl ChromeImport {
    /// `CookieImportSpec.matches(host)`：先看 `exactHosts`，再看 `suffixes` 本身或它的子域。
    pub fn matches(&self, host: &str) -> bool {
        if self.exact_hosts.iter().any(|h| h == host) {
            return true;
        }
        let d = host.trim_start_matches('.');
        self.suffixes
            .iter()
            .any(|s| d == s || d.ends_with(&format!(".{s}")))
    }
}

/// 表情表里的一条（`code` → 地址）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emote {
    pub code: String,
    pub url: String,
}

/// 随插件带的表情表（`EmoteTable` 的 pack + codes；`spans` 是函数，不过 FFI）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Emotes {
    pub pack: String,
    pub codes: Vec<Emote>,
}

/// 搜索能力（`SearchCapability` 里能被自检用到的四项）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Search {
    pub page_size: i32,
    /// `SearchKind` 的名字（`ALL` / `VIDEO` / `POST` / `USER` / `TOPIC`）。
    pub kinds: Vec<String>,
    /// `SearchSort` 的名字（`RELEVANCE` / `LATEST` / `HOT`）。
    pub sorts: Vec<String>,
}

/// 一份插件描述里"自检要看"的那部分（`PlatformPlugin` 的纯数据投影）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Descriptor {
    /// 平台 id（`platform.key`）。
    pub id: String,
    /// 显示名（`platform.label`）。
    pub label: String,
    /// 品牌色 `0xAARRGGBB`。
    pub color: i64,
    pub hosts: Vec<String>,
    /// 有几条流（`capabilities.feeds.size`）。
    pub feed_count: u32,
    /// 支持哪些端（`PluginTarget` 的 key：`desktop` / `android`）。
    pub targets: Vec<String>,
    /// `PluginTarget.entries.size`（用来判"有没有端跑不了"）。
    pub all_targets: u32,
    pub unavailable_note: Option<String>,
    pub proxied: bool,
    pub overseas: bool,
    /// `login.web.cookieDomain`。
    pub cookie_domain: String,
    pub chrome_import: Option<ChromeImport>,
    pub emotes: Option<Emotes>,
    pub search: Option<Search>,
}

/// 只看描述本身的那些检查（Kotlin `PluginContract.checkDescriptor`），**顺序和文案逐字照抄**。
pub fn check_descriptor(d: &Descriptor) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !is_platform_id(&d.id) {
        out.push(format!("id 不合法：{}", d.id));
    }
    if d.label == d.id {
        out.push(format!(
            "显示名没登记：用 Platform.define(\"{}\", \"显示名\") 定义平台标识",
            d.id
        ));
    }
    if (d.color as u64 >> 24) != 0xFF {
        out.push(format!(
            "颜色要写成不透明的 0xFFRRGGBB：{}",
            long_to_string_radix(d.color, 16)
        ));
    }
    if d.hosts.is_empty() {
        out.push("hosts 是空的：至少要有接口的域名".to_string());
    }
    for host in &d.hosts {
        if !is_host(host) {
            out.push(format!(
                "hosts 里的 {host} 不合法（小写、不带点开头、不带协议和路径）"
            ));
        }
    }
    // hosts 有重复：Kotlin 是 `toSet().size != size`
    let mut seen: Vec<&str> = Vec::with_capacity(d.hosts.len());
    let mut duplicated = false;
    for host in &d.hosts {
        if seen.iter().any(|h| *h == host.as_str()) {
            duplicated = true;
            break;
        }
        seen.push(host.as_str());
    }
    if duplicated {
        out.push("hosts 有重复".to_string());
    }
    if d.feed_count == 0 {
        out.push("一个流都没有（feeds 是空的）".to_string());
    }
    if d.targets.is_empty() {
        out.push("targets 是空的：哪一端都跑不了".to_string());
    }
    let note_blank = match d.unavailable_note.as_deref() {
        None => true,
        Some(text) => crate::hosted::is_kotlin_blank(text),
    };
    if (d.targets.len() as u32) < d.all_targets && note_blank {
        out.push("有跑不了的端，但没写 unavailableNote（灰显时给用户看的原因）".to_string());
    }
    if d.proxied && !d.overseas {
        out.push("默认走代理的应该是境外平台（overseas = true）".to_string());
    }
    if !d.cookie_domain.starts_with('.') {
        out.push(format!(
            "login.web.cookieDomain 要以点开头（\".example.com\"）：{}",
            d.cookie_domain
        ));
    }
    if let Some(spec) = &d.chrome_import {
        let domain = d.cookie_domain.trim_start_matches('.');
        if !spec.matches(&format!(".{domain}")) && !spec.matches(domain) {
            out.push(format!(
                "chromeImport 收不到 login 的 Cookie 域 {}",
                d.cookie_domain
            ));
        }
    }
    if let Some(emotes) = &d.emotes {
        if emotes.codes.is_empty() {
            out.push("emotes 是空表".to_string());
        }
        for emote in &emotes.codes {
            if !emote
                .url
                .starts_with(&format!("feedhub-emoji:{}/", emotes.pack))
            {
                out.push(format!(
                    "表情 {} 的地址不在包 {} 里：{}",
                    emote.code, emotes.pack, emote.url
                ));
            }
        }
    }
    // 搜索能力：声明了就得是能用的，不是空壳
    if let Some(search) = &d.search {
        if search.page_size <= 0 {
            out.push(format!("搜索的 pageSize 要大于 0：{}", search.page_size));
        }
        if search.kinds.is_empty() {
            out.push("搜索的 kinds 是空的（至少要有 ALL）".to_string());
        }
        if !search.kinds.iter().any(|k| k == "ALL") {
            out.push("搜索的 kinds 必须包含 ALL（目前只做内容类结果）".to_string());
        }
        if !search.sorts.iter().any(|s| s == "RELEVANCE") {
            out.push("搜索的 sorts 必须包含 RELEVANCE".to_string());
        }
        if search.kinds.iter().any(|k| k == "USER" || k == "TOPIC") {
            out.push("暂不支持搜用户 / 话题 / 吧：FeedItem 没有对应模型".to_string());
        }
    }
    out
}

