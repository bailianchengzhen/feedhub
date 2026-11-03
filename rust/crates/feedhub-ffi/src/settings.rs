//! **设置的归一化与运行时规则**：
//! - `settings_normalize` / `settings_default`；
//! - `settings_requested_platforms` / `settings_proxy_choice_for` / `settings_with_proxy_choice` /
//!   `settings_proxy_for` / `settings_direct_fallback_reason` / `FfiDirectFallbackTracker`。

use feedhub_core::settings as core;
use crate::proxy::{FfiProxyChoice, FfiProxyMode, FfiProxyType};

/// 一个平台插件里设置用得上的那几项（Kotlin `PlatformPlugin` 的 `platform` / `defaults` / `overseas`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPluginSpec {
    /// 平台 id（`bili` / `yt` / …）。
    pub id: String,
    /// `defaults.feedOn`。
    pub feed_on: bool,
    /// `defaults.proxied`。
    pub proxied: bool,
    /// `defaults.autoTranslate`。
    pub auto_translate: bool,
    /// `overseas`（**不能直连**的红线靠它）。
    pub overseas: bool,
}

impl From<FfiPluginSpec> for core::PluginSpec {
    fn from(p: FfiPluginSpec) -> Self {
        core::PluginSpec {
            id: p.id,
            feed_on: p.feed_on,
            proxied: p.proxied,
            auto_translate: p.auto_translate,
            overseas: p.overseas,
        }
    }
}

fn plugins_of(list: Vec<FfiPluginSpec>) -> Vec<core::PluginSpec> {
    list.into_iter().map(Into::into).collect()
}

/// 把一份存档 JSON 归一化（`decodeSettings` 的规则，见 `feedhub-core/src/settings.rs`）。
///
/// 读不出来（不是 JSON 对象）返回 `None` —— 调用方退回"全套默认"。
#[uniffi::export]
pub fn settings_normalize(text: String, plugins: Vec<FfiPluginSpec>) -> Option<String> {
    core::normalize(&text, &plugins_of(plugins))
}

/// 全套默认值的 JSON（Kotlin `defaultSettings(platforms)`）。
#[uniffi::export]
pub fn settings_default(plugins: Vec<FfiPluginSpec>) -> String {
    core::defaults(&plugins_of(plugins))
}

/// 设置改动多久没有新的改动才写回存储（毫秒）。
#[uniffi::export]
pub fn settings_save_debounce_ms() -> i64 {
    core::SAVE_DEBOUNCE_MS
}


/// 内容类型（用于按内容过滤请求平台）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSettingsContentType {
    Mixed,
    Video,
    Text,
}

impl From<FfiSettingsContentType> for core::ContentType {
    fn from(c: FfiSettingsContentType) -> Self {
        match c {
            FfiSettingsContentType::Mixed => core::ContentType::Mixed,
            FfiSettingsContentType::Video => core::ContentType::Video,
            FfiSettingsContentType::Text => core::ContentType::Text,
        }
    }
}

/// 平台请求能力描述。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPlatformRequestSpec {
    pub id: String,
    pub playable_video: bool,
    pub video_only_feeds: Vec<crate::bili_source::FfiFeedKind>,
}

impl From<FfiPlatformRequestSpec> for core::PlatformRequestSpec {
    fn from(p: FfiPlatformRequestSpec) -> Self {
        let mut video_only_feeds = Vec::new();
        for k in p.video_only_feeds {
            match k {
                crate::bili_source::FfiFeedKind::Recommend => {
                    video_only_feeds.push(core::FeedKind::Recommend);
                }
                crate::bili_source::FfiFeedKind::Following => {
                    video_only_feeds.push(core::FeedKind::Following);
                }
            }
        }
        core::PlatformRequestSpec {
            id: p.id,
            playable_video: p.playable_video,
            video_only_feeds,
        }
    }
}

/// 这次要请求哪些平台：按 enabled 顺序筛已安装插件。
#[uniffi::export]
pub fn settings_requested_platforms(
    kind: crate::bili_source::FfiFeedKind,
    content_type: FfiSettingsContentType,
    enabled: Vec<String>,
    plugins: Vec<FfiPlatformRequestSpec>,
) -> Vec<String> {
    let core_kind = match kind {
        crate::bili_source::FfiFeedKind::Recommend => core::FeedKind::Recommend,
        crate::bili_source::FfiFeedKind::Following => core::FeedKind::Following,
    };
    let core_plugins: Vec<core::PlatformRequestSpec> =
        plugins.into_iter().map(Into::into).collect();
    core::requested_platforms(core_kind, content_type.into(), &enabled, &core_plugins)
}

fn ffi_proxy_choice_to_core(choice: &FfiProxyChoice) -> feedhub_core::proxy::ProxyChoice {
    match choice {
        FfiProxyChoice::Follow => feedhub_core::proxy::ProxyChoice::Follow,
        FfiProxyChoice::Direct => feedhub_core::proxy::ProxyChoice::Direct,
        FfiProxyChoice::Custom { host, port, kind } => {
            let core_kind = match kind {
                FfiProxyType::Http => feedhub_core::proxy::ProxyType::Http,
                FfiProxyType::Socks5 => feedhub_core::proxy::ProxyType::Socks5,
            };
            feedhub_core::proxy::ProxyChoice::Custom {
                host: host.clone(),
                port: *port,
                kind: core_kind,
            }
        }
    }
}

fn ffi_proxy_choice_from_core(choice: &feedhub_core::proxy::ProxyChoice) -> FfiProxyChoice {
    match choice {
        feedhub_core::proxy::ProxyChoice::Follow => FfiProxyChoice::Follow,
        feedhub_core::proxy::ProxyChoice::Direct => FfiProxyChoice::Direct,
        feedhub_core::proxy::ProxyChoice::Custom { host, port, kind } => {
            let ffi_kind = match kind {
                feedhub_core::proxy::ProxyType::Http => FfiProxyType::Http,
                feedhub_core::proxy::ProxyType::Socks5 => FfiProxyType::Socks5,
            };
            FfiProxyChoice::Custom {
                host: host.clone(),
                port: *port,
                kind: ffi_kind,
            }
        }
    }
}

fn ffi_proxy_mode_to_core(mode: &FfiProxyMode) -> feedhub_core::proxy::ProxyMode {
    match mode {
        FfiProxyMode::Direct => feedhub_core::proxy::ProxyMode::Direct,
        FfiProxyMode::System => feedhub_core::proxy::ProxyMode::System,
        FfiProxyMode::Custom { host, port, kind } => {
            let core_kind = match kind {
                FfiProxyType::Http => feedhub_core::proxy::ProxyType::Http,
                FfiProxyType::Socks5 => feedhub_core::proxy::ProxyType::Socks5,
            };
            feedhub_core::proxy::ProxyMode::Custom {
                host: host.clone(),
                port: *port,
                kind: core_kind,
            }
        }
        FfiProxyMode::BuiltinCore { port, reason } => {
            feedhub_core::proxy::ProxyMode::BuiltinCore {
                port: *port,
                reason: reason.clone(),
            }
        }
    }
}

/// 这个平台单独选的那一档代理。
#[uniffi::export]
pub fn settings_proxy_choice_for(
    stored: Option<FfiProxyChoice>,
    is_proxied: bool,
    overseas: bool,
) -> FfiProxyChoice {
    let core_stored = stored.as_ref().map(ffi_proxy_choice_to_core);
    let choice = core::proxy_choice_for(core_stored.as_ref(), is_proxied, overseas);
    ffi_proxy_choice_from_core(&choice)
}

/// 存储中按平台指定的代理项。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPlatformProxyEntry {
    pub platform: String,
    pub choice: FfiProxyChoice,
}

/// 修改单平台代理档的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWithProxyChoiceResult {
    pub platform_proxy: Vec<FfiPlatformProxyEntry>,
    pub proxied_platforms: Vec<String>,
}

/// 改一个平台的代理档，同步计算新的 (platformProxy, proxiedPlatforms)。
#[uniffi::export]
pub fn settings_with_proxy_choice(
    platform: String,
    choice: FfiProxyChoice,
    overseas: bool,
    current_platform_proxy: Vec<FfiPlatformProxyEntry>,
    current_proxied_platforms: Vec<String>,
) -> FfiWithProxyChoiceResult {
    let core_choice = ffi_proxy_choice_to_core(&choice);
    let core_platform_proxy: Vec<(String, feedhub_core::proxy::ProxyChoice)> =
        current_platform_proxy
            .into_iter()
            .map(|e| (e.platform, ffi_proxy_choice_to_core(&e.choice)))
            .collect();
    let (new_map, new_proxied) = core::with_proxy_choice(
        &platform,
        &core_choice,
        overseas,
        &core_platform_proxy,
        &current_proxied_platforms,
    );
    FfiWithProxyChoiceResult {
        platform_proxy: new_map
            .into_iter()
            .map(|(p, c)| FfiPlatformProxyEntry {
                platform: p,
                choice: ffi_proxy_choice_from_core(&c),
            })
            .collect(),
        proxied_platforms: new_proxied,
    }
}

/// 这个平台这次该走什么代理（一步到位入口）。
#[uniffi::export]
pub fn settings_proxy_for(
    stored: Option<FfiProxyChoice>,
    is_proxied: bool,
    overseas: bool,
    global: FfiProxyMode,
    exit_lock: bool,
) -> FfiProxyMode {
    let core_stored = stored.as_ref().map(ffi_proxy_choice_to_core);
    let core_global = ffi_proxy_mode_to_core(&global);
    let mode = core::resolve_proxy_for(
        core_stored.as_ref(),
        is_proxied,
        overseas,
        &core_global,
        exit_lock,
    );
    crate::proxy::FfiProxyMode::from_core(&mode)
}

/// 因为出口锁关着、代理用不了而改成直连时的通知文案（一步到位入口）。
#[uniffi::export]
pub fn settings_direct_fallback_reason(
    stored: Option<FfiProxyChoice>,
    is_proxied: bool,
    overseas: bool,
    global: FfiProxyMode,
    exit_lock: bool,
) -> Option<String> {
    let core_stored = stored.as_ref().map(ffi_proxy_choice_to_core);
    let core_global = ffi_proxy_mode_to_core(&global);
    core::direct_fallback_reason(
        core_stored.as_ref(),
        is_proxied,
        overseas,
        &core_global,
        exit_lock,
    )
}

/// 单次完成代理判定与出口锁回退通知去重的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiResolvedProxy {
    pub mode: FfiProxyMode,
    pub fallback_notice: Option<String>,
}

/// 出口锁关闭回退直连的通知去重跟踪器。
#[derive(uniffi::Object, Default)]
pub struct FfiDirectFallbackTracker {
    inner: core::DirectFallbackTracker,
}

#[uniffi::export]
impl FfiDirectFallbackTracker {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track(&self, is_follow: bool, reason: Option<String>) -> Option<String> {
        self.inner.track(is_follow, reason.as_deref())
    }

    pub fn resolve(
        &self,
        stored: Option<FfiProxyChoice>,
        is_proxied: bool,
        overseas: bool,
        global: FfiProxyMode,
        exit_lock: bool,
    ) -> FfiResolvedProxy {
        let core_stored = stored.as_ref().map(ffi_proxy_choice_to_core);
        let core_global = ffi_proxy_mode_to_core(&global);
        let (mode, fallback_notice) = self.inner.resolve_and_track(
            core_stored.as_ref(),
            is_proxied,
            overseas,
            &core_global,
            exit_lock,
        );
        FfiResolvedProxy {
            mode: crate::proxy::FfiProxyMode::from_core(&mode),
            fallback_notice,
        }
    }
}
