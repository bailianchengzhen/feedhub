package feedhub.cli

import feedhub.brain.ContentFilter
import feedhub.core.FeedJson
import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog
import kotlinx.serialization.Serializable

/**
 * App 设置里 cli 需要的几项。
 *
 * 为什么不在 cli 里依赖 `app` 模块：那是 Compose 桌面 / 安卓的界面模块，cli 只依赖 `core` + 插件注册表
 * 。所以这里和 [CliProxySettings] 一样，只解出自己用的字段，
 * **未知字段一概忽略**（[FeedJson] 的 `ignoreUnknownKeys`），App 以后加设置项不会把 cli 读崩。
 *
 * 规则本身不在这里：`all` 的搜索规则在 core 的 `SearchPlan` / `SearchMerge`，内容过滤在 core 的
 * [ContentFilter]，这里只把设置搬过去。
 */
@Serializable
data class CliFeedSettings(
    /**
     * 首页点亮的平台（`AppSettings.homePlatforms`）。老存档没有这一项时用插件描述的默认值
     * （`defaults.feedOn` 的那些），和 App 的 `decodeSettings` 一致。
     */
    val homePlatforms: Set<Platform> = installed.defaultFeedPlatforms,
    /** 屏蔽词（一直生效）。读进来按 [ContentFilter.normalizeRules] 收干净。 */
    val blockWords: List<String> = emptyList(),
    /** 外出模式：关着时它那两份名单完全不看。 */
    val outingMode: Boolean = false,
    val outingKeywords: List<String> = emptyList(),
    val outingCommunities: List<String> = emptyList(),
    /** App 存下来的"见过的平台"：存档之后新装的插件按 `defaults.feedOn` 补进 [homePlatforms]（和 App 一条规则）。 */
    val knownPlatforms: Set<Platform>? = null,
) {
    /** 交给 `SearchPlan.keep` / `ContentFilter` 的那一份规则。 */
    val contentFilter: ContentFilter.Rules
        get() = ContentFilter.Rules(
            blockWords = ContentFilter.normalizeRules(blockWords),
            outingMode = outingMode,
            outingKeywords = ContentFilter.normalizeRules(outingKeywords),
            outingCommunities = ContentFilter.normalizeRules(outingCommunities),
        )

    companion object {
        /** App 存设置的键（`app/Settings.kt` 的 `SettingsStore.KEY`，和 [CliProxySettings.KEY] 是同一个）。 */
        const val KEY: String = CliProxySettings.KEY

        fun parse(text: String, platforms: PlatformCatalog = installed): CliFeedSettings {
            val settings = FeedJson.decodeFromString(serializer(), text)
            // 新装的插件（存设置时还没见过）按描述的默认值补进来：和 App 的 decodeSettings 同一条规则
            val fresh = platforms.freshSince(settings.knownPlatforms).filter { it.defaults.feedOn }.map { it.platform }
            return settings.copy(homePlatforms = settings.homePlatforms + fresh)
        }
    }
}
