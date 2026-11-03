package feedhub.core.plugin

import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.core.Source
import feedhub.core.SourceContext

/**
 * 一组启用的平台插件，**顺序就是界面上的顺序**。
 *
 * 正式的那一份由构建按 `plugins.toml` 生成（`feedhub.plugins.InstalledPlugins.catalog`，app 和 cli 共用）；
 * 测试 / 演示可以自己拼一份。主程序关于"有哪些平台、某个平台能干什么"的问题都问它，不写死平台 id。
 */
class PlatformCatalog(val plugins: List<PlatformPlugin>) {
    private val byId: Map<String, PlatformPlugin> = LinkedHashMap<String, PlatformPlugin>().also { map ->
        for (p in plugins) {
            val old = map.put(p.id, p)
            require(old == null) { "两个插件用了同一个平台 id：${p.id}" }
        }
    }

    /** 全部平台，按界面顺序。 */
    val platforms: List<Platform> = plugins.map { it.platform }

    operator fun get(platform: Platform): PlatformPlugin? = byId[platform.key]

    operator fun contains(platform: Platform): Boolean = platform.key in byId

    /** 按 id 找一个**启用了的**平台；不认识（没装这个插件）返回 null。 */
    fun find(key: String): Platform? = byId[key]?.platform

    /** "bili / zhihu / …"：报错时告诉用户认哪些 id。 */
    fun keysText(separator: String = " / "): String = platforms.joinToString(separator) { it.key }

    /** 平台 → 工厂。 */
    val factories: Map<Platform, (SourceContext) -> Source>
        get() = plugins.associateTo(LinkedHashMap()) { p -> p.platform to p::create }

    /** 平台 → 会去连的域名后缀（不建 Source 就能读，按域名选代理用）。 */
    val hosts: Map<Platform, List<String>>
        get() = plugins.associateTo(LinkedHashMap()) { it.platform to it.hosts }

    /** 只留能跑在 [target] 上的插件。 */
    fun forTarget(target: PluginTarget): PlatformCatalog = PlatformCatalog(plugins.filter { it.supports(target) })

    /** 不能跑在 [target] 上的平台 → 给用户看的原因。 */
    fun unavailableOn(target: PluginTarget): Map<Platform, String> =
        plugins.filterNot { it.supports(target) }.associateTo(LinkedHashMap()) { p ->
            p.platform to (p.unavailableNote ?: "${p.label}还不支持这一端")
        }

    /** 按条件挑出一组平台（保持界面顺序）。 */
    fun select(predicate: (PlatformPlugin) -> Boolean): Set<Platform> =
        plugins.filter(predicate).mapTo(LinkedHashSet()) { it.platform }

    /** 设置里几个默认集合。 */
    val defaultFeedPlatforms: Set<Platform> get() = select { it.defaults.feedOn }
    val defaultProxiedPlatforms: Set<Platform> get() = select { it.defaults.proxied }
    val defaultTranslationPlatforms: Set<Platform> get() = select { it.defaults.autoTranslate }

    /**
     * 存档之后**新装**的插件：不在 [known]（存档里记着"见过"的平台）里的那些。设置读出来时要把它们按各自的
     * [PlatformDefaults] 补进几个平台集合——不然新装的境外插件不在「走代理的平台」里，等于直连。
     * [known] = null：老存档（插件化之前，没有这个字段），按 [Platform.LEGACY] 那 11 个平台算。
     */
    fun freshSince(known: Set<Platform>?): List<PlatformPlugin> {
        val seen = known ?: Platform.LEGACY
        return plugins.filter { it.platform !in seen }
    }

    /** 「只看视频」请求哪些平台。 */
    val videoContentPlatforms: Set<Platform> get() = select { it.capabilities.playableVideo }

    /** 这个流在「只看图文」时要不要请求这个平台（全是视频的流不请求）。不认识的平台按"要"算。 */
    fun hasText(platform: Platform, kind: FeedKind): Boolean =
        this[platform]?.let { kind !in it.capabilities.videoOnlyFeeds } ?: true

    companion object {
        val EMPTY = PlatformCatalog(emptyList())
    }
}
