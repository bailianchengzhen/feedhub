package feedhub.core.sync

import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiLoginCount
import uniffi.feedhub_ffi.FfiLoginScope

/**
 * 一个平台的"Cookie 地盘"：同步时收哪些域、写之前清哪些域。
 *
 * 规则仍然从插件描述来（`login.chromeImport` 优先，没有就用 `login.web.cookieDomain`）——
 * 那是编译期插件元数据，宿主给 Rust **只给规则**；**判断和执行**（先清后写、不信任报文里的域）
 * 在 `feedhub-core/src/sync/login.rs`。
 */
class SyncScope private constructor(
    /** 写之前先 `clearDomain` 的域（主域 + 老域），顺序固定、去重。 */
    val clearDomains: List<String>,
    /** 后缀匹配（本身或子域）。 */
    internal val suffixes: List<String>,
    /** 精确 host。 */
    internal val exactHosts: List<String>,
) {
    /** 这条 Cookie 的域在不在这个平台的地盘里。 */
    fun matches(domain: String): Boolean {
        val h = domain.trimStart('.').lowercase()
        if (exactHosts.any { it.trimStart('.').lowercase() == h }) return true
        return suffixes.any { s ->
            val d = s.trimStart('.').lowercase()
            h == d || h.endsWith(".$d")
        }
    }

    override fun toString(): String = "SyncScope(clear=$clearDomains)"

    internal fun toFfi(platformId: String): FfiLoginScope = FfiLoginScope(
        platformId = platformId,
        clearDomains = clearDomains,
        suffixes = suffixes,
        exactHosts = exactHosts,
    )

    companion object {
        /** 这个平台能不能按 Cookie 同步：没有登录页 / 没有导入规则也不算（现在没有这种平台）。 */
        fun of(plugin: feedhub.core.plugin.PlatformPlugin): SyncScope? {
            val import = plugin.login.chromeImport
            if (import != null) {
                val clear = (listOf(import.domain) + import.suffixes + import.exactHosts)
                    .map { it.trimStart('.').lowercase() }
                    .filter { it.isNotBlank() }
                    .distinct()
                return SyncScope(
                    clearDomains = clear,
                    suffixes = import.suffixes.map { it.trimStart('.').lowercase() },
                    exactHosts = import.exactHosts.map { it.trimStart('.').lowercase() },
                )
            }
            val d = plugin.login.web.cookieDomain.trimStart('.').lowercase()
            if (d.isEmpty()) return null
            return SyncScope(listOf(d), listOf(d), emptyList())
        }

        /** 按界面顺序，每个平台一份。 */
        fun of(catalog: feedhub.core.plugin.PlatformCatalog): Map<Platform, SyncScope> =
            catalog.plugins.mapNotNull { plugin -> of(plugin)?.let { plugin.platform to it } }.toMap()
    }
}

/** 平台 + 地盘 → FFI record 列表（顺序 = 界面 / 插件顺序）。 */
internal fun Map<Platform, SyncScope>.toFfiScopes(): List<FfiLoginScope> =
    entries.map { (platform, scope) -> scope.toFfi(platform.key) }

/** `FfiLoginCount` → 模型（平台 id 不认识的丢掉，调用方按 `graph.platforms` 找名字）。 */
internal fun List<FfiLoginCount>.toCounts(): Map<Platform, Int> =
    associate { Platform(it.platformId) to it.count.toInt() }

/** 发送方打包的结果：哪些平台带上了、各多少条、哪些被跳过（一条 Cookie 都没有）。 */
data class SyncBuildResult(
    val pkg: SyncPackage,
    val counts: Map<Platform, Int>,
    /** 选了但没有可同步 Cookie 的平台（不会出现在 [pkg] 里，接收端也就不会动它们的域）。 */
    val skipped: List<Platform>,
)

/** 接收方应用的结果：各平台写了多少条、哪些平台没动（原因只有平台 id + 一句短话，不含值）。 */
data class SyncApplyResult(
    val applied: Map<Platform, Int>,
    /** 平台 id → 没应用的原因（不认识的平台 / 一条可用 Cookie 都没有）。 */
    val skipped: Map<String, String>,
)
