//! 插件描述自检过 UniFFI 的那一层。
//!
//! 宿主把 `PlatformPlugin` 里**纯数据**的那部分（id / 名字 / 颜色 / 域名 / 能力条数 / 端 / 默认值 /
//! 登录域 / Chrome 导入规则 / 表情表 / 搜索能力）打包成 [`FfiPluginDescriptor`]，
//! Rust 返回**问题列表**（空 = 通过，文案和 Kotlin 逐字一致）。
//!
//! **"描述和它建出来的 Source 对不对得上"那一半不在 Rust**：那要 `create(ctx)` 造对象、比接口实现，
//! 属于宿主（`PluginContract.check`）。
//!
//! 只在插件模块的离线测试 / 启动自检里调一次，不在界面的热路径上。

use feedhub_core::plugin_contract::{self, ChromeImport, Descriptor, Emote, Emotes, Search};

/// `CookieImportSpec` 里自检用得到的三个字段。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginChromeImport {
    pub domain: String,
    pub suffixes: Vec<String>,
    pub exact_hosts: Vec<String>,
}

/// 表情表里的一条：文字代码 → 地址。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginEmote {
    pub code: String,
    pub url: String,
}

/// 随插件带的表情表（`EmoteTable` 的 pack + codes；`spans` 是函数，不过 FFI）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginEmotes {
    pub pack: String,
    pub codes: Vec<FfiPluginEmote>,
}

/// 搜索能力（`SearchCapability` 里自检要看的三项；枚举用 `SearchKind` / `SearchSort` 的名字）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginSearch {
    pub page_size: i32,
    pub kinds: Vec<String>,
    pub sorts: Vec<String>,
}

/// 一份插件描述的投影（`PlatformPlugin` 的纯数据）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginDescriptor {
    /// `platform.key`。
    pub id: String,
    /// `platform.label`。
    pub label: String,
    /// 品牌色 `0xAARRGGBB`。
    pub color: i64,
    pub hosts: Vec<String>,
    /// `capabilities.feeds.size`。
    pub feed_count: u32,
    /// 支持的端（`PluginTarget` 的 key）。
    pub targets: Vec<String>,
    /// `PluginTarget.entries.size`。
    pub all_targets: u32,
    pub unavailable_note: Option<String>,
    pub proxied: bool,
    pub overseas: bool,
    /// `login.web.cookieDomain`。
    pub cookie_domain: String,
    pub chrome_import: Option<FfiPluginChromeImport>,
    pub emotes: Option<FfiPluginEmotes>,
    pub search: Option<FfiPluginSearch>,
}

fn to_descriptor(d: FfiPluginDescriptor) -> Descriptor {
    Descriptor {
        id: d.id,
        label: d.label,
        color: d.color,
        hosts: d.hosts,
        feed_count: d.feed_count,
        targets: d.targets,
        all_targets: d.all_targets,
        unavailable_note: d.unavailable_note,
        proxied: d.proxied,
        overseas: d.overseas,
        cookie_domain: d.cookie_domain,
        chrome_import: d.chrome_import.map(|c| ChromeImport {
            domain: c.domain,
            suffixes: c.suffixes,
            exact_hosts: c.exact_hosts,
        }),
        emotes: d.emotes.map(|e| Emotes {
            pack: e.pack,
            codes: e
                .codes
                .into_iter()
                .map(|c| Emote {
                    code: c.code,
                    url: c.url,
                })
                .collect(),
        }),
        search: d.search.map(|s| Search {
            page_size: s.page_size,
            kinds: s.kinds,
            sorts: s.sorts,
        }),
    }
}

/// 只看描述本身的那些检查（`PluginContract.checkDescriptor`）：返回的问题列表，空 = 通过。
#[uniffi::export]
pub fn plugin_contract_check_descriptor(descriptor: FfiPluginDescriptor) -> Vec<String> {
    plugin_contract::check_descriptor(&to_descriptor(descriptor))
}

/// `Platform.ID` 的正则口径（`[a-z][a-z0-9_-]{0,31}`），给宿主做同一份判定用。
#[uniffi::export]
pub fn plugin_contract_is_platform_id(id: String) -> bool {
    plugin_contract::is_platform_id(&id)
}

/// 插件描述里 `hosts` 那条正则的口径（整串匹配），给宿主做同一份判定用。
#[uniffi::export]
pub fn plugin_contract_is_host(host: String) -> bool {
    plugin_contract::is_host(&host)
}
