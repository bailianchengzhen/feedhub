package feedhub.settings

import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.core.ProxyChoice
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiDirectFallbackTracker
import uniffi.feedhub_ffi.FfiFeedKind
import uniffi.feedhub_ffi.FfiPlatformProxyEntry
import uniffi.feedhub_ffi.FfiPlatformRequestSpec
import uniffi.feedhub_ffi.FfiPluginSpec
import uniffi.feedhub_ffi.FfiProxyChoice
import uniffi.feedhub_ffi.FfiProxyMode
import uniffi.feedhub_ffi.FfiProxyType
import uniffi.feedhub_ffi.FfiSettingsContentType
import uniffi.feedhub_ffi.settingsDefault
import uniffi.feedhub_ffi.settingsDirectFallbackReason
import uniffi.feedhub_ffi.settingsNormalize
import uniffi.feedhub_ffi.settingsProxyChoiceFor
import uniffi.feedhub_ffi.settingsProxyFor
import uniffi.feedhub_ffi.settingsRequestedPlatforms
import uniffi.feedhub_ffi.settingsSaveDebounceMs
import uniffi.feedhub_ffi.settingsWithProxyChoice

/**
 * **设置的读 / 迁移 / 归一化 / 默认值与运行时规则**。
 *
 * 原实现是 `app/.../Settings.kt`：
 * - `decodeSettings` / `defaultSettings`；
 * - `requestedPlatforms`（按内容类型与插件能力筛选待请求平台）；
 * - `proxyChoiceFor` / `withProxyChoice`（单平台代理档读取与状态同步，境外平台红线保卫）；
 * - `proxyFor` / `directFallbackReason`（解析实际代理档位与回退直连原因）；
 * - `SettingsStore.proxyProvider`（回退直连通知去重状态机）。
 *
 * 规则全部在 `rust/crates/feedhub-core/src/settings.rs`，这里是它的 Kotlin 门面。
 *
 * **没有开关**：`.so` 加载失败就抛（和 `ProxyRules` / `Brain` 一样，不悄悄退回 Kotlin 实现）。
 */
object RustSettings {
    /** 设置改动多久没有新的改动才写回存储（毫秒）。原 `SETTINGS_SAVE_DEBOUNCE_MS`。 */
    val saveDebounceMs: Long by lazy {
        native { settingsSaveDebounceMs() }
    }

    /**
     * 把一份存档 JSON 归一化（原 `decodeSettings`）。
     *
     * [plugins] 是装着的插件（顺序 = 界面顺序，**新装的插件按它的默认值补进几组集合**靠它）。
     * 读不出来（不是 JSON 对象）返回 `null` —— 调用方退回"全套默认"。
     */
    fun normalize(text: String, plugins: List<PluginSpec>): String? =
        native { settingsNormalize(text, plugins.map { it.toFfi() }) }

    /** 全套默认值的 JSON（原 `defaultSettings(platforms)`）。 */
    fun defaults(plugins: List<PluginSpec>): String =
        native { settingsDefault(plugins.map { it.toFfi() }) }

    /**
     * 一个平台插件里设置用得上的那几项（`PlatformPlugin` 的 `platform` / `defaults` / `overseas`）。
     *
     * 只在这道边界上出现：`PlatformPlugin` 本身（带工厂、能力、登录、文案）不往 Rust 过。
     */
    class PluginSpec(
        val id: String,
        /** `defaults.feedOn`：首页 / 关注默认开。 */
        val feedOn: Boolean = false,
        /** `defaults.proxied`：「走代理的平台」默认点亮。 */
        val proxied: Boolean = false,
        /** `defaults.autoTranslate`：「自动翻译」默认点亮。 */
        val autoTranslate: Boolean = false,
        /** 境外平台（**不能直连**的红线靠它）。 */
        val overseas: Boolean = false,
    ) {
        internal fun toFfi(): FfiPluginSpec = FfiPluginSpec(
            id = id,
            feedOn = feedOn,
            proxied = proxied,
            autoTranslate = autoTranslate,
            overseas = overseas,
        )
    }

    enum class ContentType { MIXED, VIDEO, TEXT }

    class PlatformRequestSpec(
        val id: String,
        val playableVideo: Boolean,
        val videoOnlyFeeds: Set<FeedKind>,
    )

    /**
     * 这次要请求哪些平台：按 enabled 顺序筛已安装插件（原 `requestedPlatforms`）。
     */
    fun requestedPlatforms(
        kind: FeedKind,
        contentType: ContentType,
        enabled: Set<Platform>,
        plugins: List<PlatformRequestSpec>,
    ): Set<Platform> = native {
        val ffiKind = when (kind) {
            FeedKind.RECOMMEND -> FfiFeedKind.RECOMMEND
            FeedKind.FOLLOWING -> FfiFeedKind.FOLLOWING
        }
        val ffiContentType = when (contentType) {
            ContentType.MIXED -> FfiSettingsContentType.MIXED
            ContentType.VIDEO -> FfiSettingsContentType.VIDEO
            ContentType.TEXT -> FfiSettingsContentType.TEXT
        }
        val enabledIds = enabled.map { it.key }
        val ffiPlugins = plugins.map { p ->
            FfiPlatformRequestSpec(
                id = p.id,
                playableVideo = p.playableVideo,
                videoOnlyFeeds = p.videoOnlyFeeds.map { k ->
                    when (k) {
                        FeedKind.RECOMMEND -> FfiFeedKind.RECOMMEND
                        FeedKind.FOLLOWING -> FfiFeedKind.FOLLOWING
                    }
                },
            )
        }
        val result = settingsRequestedPlatforms(ffiKind, ffiContentType, enabledIds, ffiPlugins)
        val enabledMap = enabled.associateBy { it.key }
        result.mapNotNullTo(LinkedHashSet()) { enabledMap[it] }
    }

    /**
     * 这个平台单独选的那一档代理（原 `AppSettings.proxyChoiceFor`）。
     */
    fun proxyChoiceFor(
        platform: Platform,
        platformProxy: Map<Platform, ProxyChoice>,
        proxiedPlatforms: Set<Platform>,
        overseas: Boolean,
    ): ProxyChoice = native {
        val storedFfi = platformProxy[platform]?.toFfi()
        val isProxied = platform in proxiedPlatforms
        settingsProxyChoiceFor(storedFfi, isProxied, overseas).toCore()
    }

    /**
     * 改一个平台的代理档，同步计算新的 (platformProxy, proxiedPlatforms)（原 `AppSettings.withProxyChoice`）。
     */
    fun withProxyChoice(
        platform: Platform,
        choice: ProxyChoice,
        overseas: Boolean,
        platformProxy: Map<Platform, ProxyChoice>,
        proxiedPlatforms: Set<Platform>,
    ): Pair<Map<Platform, ProxyChoice>, Set<Platform>> = native {
        val currentProxyEntries = platformProxy.map { (p, c) ->
            FfiPlatformProxyEntry(p.key, c.toFfi())
        }
        val currentProxied = proxiedPlatforms.map { it.key }
        val result = settingsWithProxyChoice(
            platform.key,
            choice.toFfi(),
            overseas,
            currentProxyEntries,
            currentProxied,
        )
        val nextProxy = result.platformProxy.associate { (p, c) ->
            Platform(p) to c.toCore()
        }
        val nextProxied = result.proxiedPlatforms.mapTo(LinkedHashSet()) { Platform(it) }
        Pair(nextProxy, nextProxied)
    }

    /**
     * 这个平台这次该走什么代理（原 `AppSettings.proxyFor`）。
     */
    fun proxyFor(
        platform: Platform,
        platformProxy: Map<Platform, ProxyChoice>,
        proxiedPlatforms: Set<Platform>,
        overseas: Boolean,
        global: ProxyMode,
        exitLock: Boolean,
    ): ProxyMode = native {
        val storedFfi = platformProxy[platform]?.toFfi()
        val isProxied = platform in proxiedPlatforms
        settingsProxyFor(storedFfi, isProxied, overseas, global.toFfi(), exitLock).toCore()
    }

    /**
     * 因为"出口锁关着、全局代理又用不了"而被改成直连时的一句话原因（原 `AppSettings.directFallbackReason`）。
     */
    fun directFallbackReason(
        platform: Platform,
        platformProxy: Map<Platform, ProxyChoice>,
        proxiedPlatforms: Set<Platform>,
        overseas: Boolean,
        global: ProxyMode,
        exitLock: Boolean,
    ): String? = native {
        val storedFfi = platformProxy[platform]?.toFfi()
        val isProxied = platform in proxiedPlatforms
        settingsDirectFallbackReason(storedFfi, isProxied, overseas, global.toFfi(), exitLock)
    }

    /**
     * 创建出口锁回退直连通知去重跟踪器。
     */
    fun createFallbackTracker(): FfiDirectFallbackTracker = native {
        FfiDirectFallbackTracker()
    }

    /**
     * 一次 FFI 调用同时判定平台代理档位并更新出口锁回退通知去重状态。
     */
    fun resolveAndTrackProxy(
        tracker: FfiDirectFallbackTracker,
        platform: Platform,
        platformProxy: Map<Platform, ProxyChoice>,
        proxiedPlatforms: Set<Platform>,
        overseas: Boolean,
        global: ProxyMode,
        exitLock: Boolean,
    ): Pair<ProxyMode, String?> = native {
        val storedFfi = platformProxy[platform]?.toFfi()
        val isProxied = platform in proxiedPlatforms
        val resolved = tracker.resolve(storedFfi, isProxied, overseas, global.toFfi(), exitLock)
        resolved.mode.toCore() to resolved.fallbackNotice
    }

    private fun ProxyType.toFfi(): FfiProxyType = when (this) {
        ProxyType.HTTP -> FfiProxyType.HTTP
        ProxyType.SOCKS5 -> FfiProxyType.SOCKS5
    }

    private fun FfiProxyType.toCore(): ProxyType = when (this) {
        FfiProxyType.HTTP -> ProxyType.HTTP
        FfiProxyType.SOCKS5 -> ProxyType.SOCKS5
    }

    private fun ProxyChoice.toFfi(): FfiProxyChoice = when (this) {
        ProxyChoice.Follow -> FfiProxyChoice.Follow
        ProxyChoice.Direct -> FfiProxyChoice.Direct
        is ProxyChoice.Custom -> FfiProxyChoice.Custom(host, port, type.toFfi())
    }

    private fun FfiProxyChoice.toCore(): ProxyChoice = when (this) {
        FfiProxyChoice.Follow -> ProxyChoice.Follow
        FfiProxyChoice.Direct -> ProxyChoice.Direct
        is FfiProxyChoice.Custom -> ProxyChoice.Custom(host, port, kind.toCore())
    }

    private fun ProxyMode.toFfi(): FfiProxyMode = when (this) {
        ProxyMode.Direct -> FfiProxyMode.Direct
        ProxyMode.System -> FfiProxyMode.System
        is ProxyMode.Custom -> FfiProxyMode.Custom(host, port, type.toFfi())
        is ProxyMode.BuiltinCore -> FfiProxyMode.BuiltinCore(port, reason)
    }

    private fun FfiProxyMode.toCore(): ProxyMode = when (this) {
        FfiProxyMode.Direct -> ProxyMode.Direct
        FfiProxyMode.System -> ProxyMode.System
        is FfiProxyMode.Custom -> ProxyMode.Custom(host, port, kind.toCore())
        is FfiProxyMode.BuiltinCore -> ProxyMode.BuiltinCore(port, reason)
    }

    /**
     * 走 Rust：`.so` 加载失败**不悄悄退回 Kotlin**，给一句说得清的话（和 `ProxyRules` / `Brain` 一样）。
     */
    private inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: Exception) {
        throw IllegalStateException("设置的规则在 Rust 里，但 .so 没加载起来：${e.message}", e)
    }
}
