package feedhub.app

import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import feedhub.core.ProxyConfig
import feedhub.core.ProxyType

/**
 * debug 构建的启动参数里那几个**只给截图 / 离线验证用**的开关（纯函数，`DebugLaunchTest` 离线测）。
 *
 * 真正读 Intent 的是 `:androidApp` 的 `DebugStartActivity`（只在 debug 源集里）。为什么要有它们：
 * 部分安卓真机吞掉 adb 注入的触摸，点按不了，所以"这次只跑哪几个平台""这次走哪个代理"
 * 只能靠启动参数说清楚。**这些参数不改用户的设置**（`SettingsStore.override` 只留在内存里）。
 *
 * @see startScreenOf 起始页那三个参数（`screen` / `item`）
 */


/**
 * `--es proxy socks5://10.0.2.2:10808` / `http://host:port` / `none` → 这次运行的代理配置。
 * 认不出来（协议没写、缺端口、端口不是数字）**抛异常**（活动那边接住、退回首页并打日志），
 * 不静默当成"没给"——参数写错时截出来的图不能长得像"代理没生效"。
 */
fun proxyOverrideOf(text: String?): ProxyConfig? {
    val t = text?.trim().orEmpty()
    if (t.isEmpty()) return null
    if (t.equals("none", ignoreCase = true)) return ProxyConfig(mode = ProxyConfig.Mode.NONE)
    val type = when {
        t.startsWith("socks5://", ignoreCase = true) -> ProxyType.SOCKS5
        t.startsWith("http://", ignoreCase = true) -> ProxyType.HTTP
        else -> throw IllegalArgumentException("proxy 要写成 socks5://host:port / http://host:port / none（收到 $text）")
    }
    val rest = t.substringAfter("://")
    val host = rest.substringBeforeLast(':', "")
    val port = rest.substringAfterLast(':', "").toIntOrNull()
    if (host.isBlank() || port == null || port !in 1..65535) {
        throw IllegalArgumentException("proxy 的地址 / 端口不完整：$text")
    }
    return ProxyConfig(mode = ProxyConfig.Mode.CUSTOM, host = host, port = port, type = type)
}

/**
 * `--es platforms bili,x` → 这次运行只跑这几个平台（首页 / 关注各一份，内存里，不写回设置）。
 * 空 / 没给 → null（照设置里的来）；`none`（或 `-`）= 一个都不点亮。认不出的 key 抛异常。
 */
fun platformsOf(text: String?, platforms: PlatformCatalog = InstalledPlugins.catalog): Set<Platform>? {
    val t = text?.trim().orEmpty()
    if (t.isEmpty()) return null
    // `none` = 一个平台都不点亮：设置里全点灭的样子
    if (t.equals("none", ignoreCase = true) || t == "-") return emptySet()
    val keys = t.split(',', ' ', ';').map { it.trim().lowercase() }.filter { it.isNotEmpty() }
    if (keys.isEmpty()) return null
    return keys.mapTo(LinkedHashSet()) { key ->
        platforms.find(key) ?: throw IllegalArgumentException("未知平台：$key（认 ${platforms.keysText()}）")
    }
}

/**
 * 把 debug 参数套到设置上（返回新的设置；调用方用 `SettingsStore.override` 塞回去，**不落盘**）。
 *
 * **fail-closed**：只跑境外平台（插件描述里 `overseas = true` 的）但没给 `proxy` 时抛异常。
 * 和 `cli/CliProxy.kt` 同一个理由：境外平台直连 = 用不了，还可能把请求打到不该去的地方；
 * 参数里没写代理就宁可什么都不做，也不悄悄直连。
 */
fun applyDebugOverrides(
    settings: AppSettings,
    platforms: Set<Platform>?,
    proxy: ProxyConfig?,
    catalog: PlatformCatalog = InstalledPlugins.catalog,
): AppSettings {
    if (platforms == null && proxy == null) return settings
    val overseas = platforms.orEmpty().filter { catalog[it]?.overseas == true }
    if (platforms != null && proxy == null && overseas.isNotEmpty()) {
        throw IllegalArgumentException(
            "只跑境外平台（${overseas.joinToString("、") { it.label }}）时必须给 --es proxy，" +
                "没给就是直连，不发请求",
        )
    }
    var next = settings
    if (platforms != null) next = next.copy(homePlatforms = platforms, followingPlatforms = platforms)
    if (proxy != null) {
        // 这次指定的平台一律走这个代理（只跑 B站 时它不在集合里，等于直连）；
        // `--es proxy` 说了算（不然按平台的会盖过它）
        next = next.copy(
            proxy = proxy,
            proxiedPlatforms = platforms ?: next.proxiedPlatforms,
            platformProxy = if (platforms != null) next.platformProxy - platforms else next.platformProxy,
        )
    }
    return next
}
