package feedhub.cli

import feedhub.core.FeedJson
import feedhub.core.KeyValueStore
import feedhub.core.Platform
import feedhub.core.PlatformProxySelector
import feedhub.core.ProxyChoice
import feedhub.core.ProxyConfig
import feedhub.core.ProxyMode
import feedhub.core.ProxyProvider
import feedhub.core.ProxyType
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.plugin.PlatformCatalog
import feedhub.proxy.ProxyRules
import okhttp3.Interceptor
import okhttp3.Response
import java.io.IOException
import java.net.ProxySelector
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

/**
 * cli 的代理设置。
 *
 * **规则和 App 一模一样**（`app/.../Settings.kt` 的 `AppSettings.proxyFor`，判定本身在 `core` 的 `resolveProxyMode`）：
 * 0. 这个平台在 App 的「网络 → 按平台」里**单独指定**了代理（[CliProxySettings.platformProxy] 里的
 *    [ProxyChoice.Custom]）→ 就用它；地址没填全时由 core 的 `ProxyConfigGuardInterceptor` 在发请求前拦下
 *    （fail-closed，**出口锁对它不起作用**，和 App 一致）；
 * 1. 平台在「走代理的平台」（`proxiedPlatforms`）里 → 用设置里的全局代理；
 * 2. 不在集合里 → 直连；
 * 3. 设置里的模式是「不用」→ 都直连。
 *
 * 在此之上 cli 多两条（App 侧没有 / 不需要的）：
 * - `--proxy socks5://host:port | http://host:port | none` 覆盖本次运行（**不写回设置**，**优先级最高**，
 *   连按平台指定的那一档也盖掉）；
 * - **fail-closed**：点亮的平台但这次跑起来没有可用的代理（设置是「不用」、cli 不认识的模式、
 *   地址 / 端口没填全）→ 发请求前抛 [IOException]，**绝不悄悄直连**。错误文案里写清楚怎么改。
 *
 * **出口锁**：读 App 的同一个开关
 * （[CliProxySettings.proxyExitLock]，默认开 = 上面那条 fail-closed 的行为）。
 * 关着时上面这些情况**改为直连**，并且在真正发请求前打一行"出口锁关闭，直连"（每个平台一次，不含地址），
 * 免得静默直连。
 *
 * 设置从哪读：App 存在同一个 `state.json` 的 `app.settings` 键里（`LocalFiles` / [SourceContext.store]）。
 * 这里只解出五个字段（`proxy` / `proxiedPlatforms` / `proxyExitLock` / `knownPlatforms` / `platformProxy`），
 * **未知字段一概忽略**（`FeedJson` 的 `ignoreUnknownKeys`，判别键 `mode` 和 `ProxyConfig` 一致）——
 * App 以后加设置项不会把 cli 读崩。
 */
@Serializable
data class CliProxySettings(
    /** 和 App 同一份结构（`core` 的 [ProxyConfig]）：`不用 / 系统代理 / 指定`。 */
    val proxy: ProxyConfig = ProxyConfig(),
    /**
     * 点亮的平台（= 这些平台「跟随全局」时走全局代理）。老存档 / 没存过这个字段时用**境外的四家**
     * （和 `AppSettings.proxiedPlatforms` 的默认值一致，来自插件描述）：它们不加代理本来就连不上，
     * 默认直连等于用不了。
     */
    val proxiedPlatforms: Set<Platform> = DEFAULT_PROXIED,
    /**
     * **按平台指定代理**：存了
     * [ProxyChoice.Custom] 的平台走它自己那个代理，**优先于** [proxiedPlatforms] 和全局 [proxy]，
     * 也不受出口锁影响（fail-closed）。没存过 = 跟随全局。
     */
    val platformProxy: Map<Platform, ProxyChoice> = emptyMap(),
    /**
     * 「代理不可用时禁止直连」（出口锁，和 `AppSettings.proxyExitLock` 是同一个键）。**默认开**：
     * 点亮的平台拿不到可用代理就一个请求都不发（[CliProxyGuardInterceptor] 报错）。
     * **关着**：改成直连 + 一行提示。老存档没有这个字段时读出来是默认的 true。
     */
    val proxyExitLock: Boolean = true,
    /**
     * App 存下来的"见过的平台"（`AppSettings.knownPlatforms`）；null = 老存档。存档之后新装的插件
     * 按它的 `defaults.proxied` 补进 [proxiedPlatforms]（[parse] 里做，和 App 的 `decodeSettings` 同一条规则）——
     * 不然 App 还没把设置写回去之前，cli 跑新装的境外插件就是直连。
     */
    val knownPlatforms: Set<Platform>? = null,
) {
    companion object {
        /** `AppSettings.proxiedPlatforms` 的默认值：装了的插件里 `defaults.proxied` 的那些（两边是同一份描述）。 */
        val DEFAULT_PROXIED: Set<Platform> get() = installed.defaultProxiedPlatforms

        /** App 存设置的键（`app/Settings.kt` 的 `SettingsStore.KEY`）。 */
        const val KEY = "app.settings"

        fun parse(text: String, platforms: PlatformCatalog = installed): CliProxySettings {
            val settings = FeedJson.decodeFromString(serializer(), text)
            val fresh = platforms.freshSince(settings.knownPlatforms).filter { it.defaults.proxied }.map { it.platform }
            // 老存档里被关掉的不算数——想让它不走全局代理只能给它单独「指定代理」（`platformProxy`）。
            val overseas = platforms.plugins.filter { it.overseas }.mapTo(LinkedHashSet()) { it.platform }
            return settings.copy(
                proxiedPlatforms = settings.proxiedPlatforms + fresh + overseas,
                // 境外平台的「直连」不认（手改存档 / 以后的新版本写进来的），按「跟随全局」读。
                platformProxy = settings.platformProxy.filter { (p, c) -> !(c == ProxyChoice.Direct && p in overseas) },
            )
        }
    }
}

/** 这次运行某个平台实际怎么连（[CliProxyPlan.proxyFor] 的返回值，带来源信息方便排查）。 */
data class CliProxyChoice(
    val mode: ProxyMode,
    /** 这个选择从哪来：`--proxy` / `设置` / `设置（没有存档，用默认值）`。 */
    val from: String,
    /** 点亮了要代理、但这次没拿到可用代理——发请求前直接报错（fail-closed）。 */
    val blocked: Boolean = false,
    /**
     * **出口锁关着**、代理又用不了，于是改成直连的原因（不含地址）；正常情况是 null。
     * 真正发请求前会把它打成一行提示（每个平台一次）。
     */
    val fallbackReason: String? = null,
)

/** cli 这次运行的代理计划：设置 + `--proxy` 覆盖，解析成一次运行的答案。 */
class CliProxyPlan(
    private val settings: CliProxySettings,
    private val override: ProxyOverride?,
    private val settingsFound: Boolean,
    /**
     * 存档里 `proxy.mode` 的**原始字符串**（null = 存档里没这一项）。用来认出 cli 不认识的模式：
     * `ProxyConfig.Mode` 解不出来时会 `coerceInputValues` 到默认值（「系统代理」），那样就变成 fail-open 了。
     */
    private val rawMode: String? = null,
) {
    /** 给 [SourceContext] 用：每次请求现算（和 App 的 `proxyProvider` 一个用法）。 */
    val provider: ProxyProvider = ProxyProvider { proxyFor(it).mode }

    /**
     * 这个平台这次怎么走。
     *
     * `--proxy` 覆盖时只看覆盖值（`--proxy none` 就是**用户显式**要直连，报错就不合适了）；
     * 没覆盖时按设置：「不用」而平台点亮了 → [CliProxyChoice.blocked]，cli 不认识的模式同理。
     *
     * **[CliProxySettings.proxyExitLock] 关着**（和 App 的「代理不可用时禁止直连」是同一个开关）时，
     * 上面这些"拿不到可用代理"的情况一律**改为直连**（`blocked = false` + [CliProxyChoice.fallbackReason]），
     * 和 `AppSettings.proxyFor` 的行为对齐。
     */
    fun proxyFor(platform: Platform): CliProxyChoice {
        val ov = override
        if (ov != null) {
            return when (ov) {
                ProxyOverride.None -> CliProxyChoice(ProxyMode.Direct, "--proxy none")
                is ProxyOverride.Custom ->
                    CliProxyChoice(ProxyMode.Custom(ov.host, ov.port, ov.type), "--proxy ${ov.type.name.lowercase()}://${ov.host}:${ov.port}")
            }
        }
        val from = if (settingsFound) "设置" else "设置（没有存档，用默认值）"
        // 也不受出口锁影响）。地址 / 端口没填全时给一个 valid = false 的 Custom，core 的
        // ProxyConfigGuardInterceptor 会在发请求前拦下（fail-closed，不回退直连）。
        (settings.platformProxy[platform] as? ProxyChoice.Custom)?.let { custom ->
            return CliProxyChoice(custom.toProxyMode(), "$from（按平台）")
        }
        if (platform !in settings.proxiedPlatforms) return CliProxyChoice(ProxyMode.Direct, from)
        // 出口锁关着：下面每一处"用不了"都改成直连（并留一行提示），不再拦
        val lock = settings.proxyExitLock
        // cli 不认识的模式（App 以后加的档，例如「内置核心」）：开锁时 fail-closed，关锁时直连
        if (rawMode != null && rawMode !in KNOWN_MODES) {
            return CliProxyChoice(
                ProxyMode.Direct, from,
                blocked = lock,
                fallbackReason = if (lock) null else "cli 不认识的代理模式 \"$rawMode\"",
            )
        }
        return when (settings.proxy.mode) {
            ProxyConfig.Mode.NONE -> CliProxyChoice(
                ProxyMode.Direct, from,
                blocked = lock,
                fallbackReason = if (lock) null else "设置里「代理」是「不用」",
            )
            ProxyConfig.Mode.SYSTEM -> CliProxyChoice(ProxyMode.System, from)
            // 地址 / 端口没填全时给一个 valid = false 的 Custom：core 的 ProxyConfigGuardInterceptor
            // 会在发请求前抛出说得清的错误
            ProxyConfig.Mode.CUSTOM -> {
                val mode = settings.proxy.toProxyMode()
                if (lock || !ProxyRules.unavailable(mode)) {
                    CliProxyChoice(mode, from)
                } else {
                    CliProxyChoice(ProxyMode.Direct, from, fallbackReason = ProxyRules.unavailableReason(mode))
                }
            }
            // 「内置核心」：cli 不起 sing-box，开锁时点亮的平台 fail-closed（要走代理就用 --proxy），
            // 关锁时直连。rawMode 那一支通常已经先拦下了，这里是 when 穷举的兜底
            ProxyConfig.Mode.BUILTIN -> CliProxyChoice(
                ProxyMode.Direct, from,
                blocked = lock,
                fallbackReason = if (lock) null else "「内置核心」这一档 cli 里没有（cli 不起 sing-box）",
            )
        }
    }

    /** 这个模式 cli 不认识（报错文案要用）。 */
    internal fun unsupportedMode(): String? = rawMode?.takeIf { it !in KNOWN_MODES }

    /** 点亮了要代理、但这次没拿到可用代理的平台。 */
    fun blockedPlatforms(): Set<Platform> = installed.platforms.filter { proxyFor(it).blocked }.toSet()

    /** 按平台生成 [ProxySelector]（每个平台一份；`SourceContext.client` 内部也是这么挂的）。 */
    fun selector(platform: Platform): ProxySelector =
        PlatformProxySelector({ proxyFor(platform).mode })

    companion object {
        /** cli 认识的模式（和 `ProxyConfig.Mode` 的序列化名一致；多一个都算"不认识"）。 */
        val KNOWN_MODES = setOf("NONE", "SYSTEM", "CUSTOM")

        /** [override] 是 `--proxy` 的原始值（null = 没给）。 */
        fun of(store: KeyValueStore, override: String?): CliProxyPlan {
            val raw = store.get(CliProxySettings.KEY)
            val parsed = raw?.let { runCatching { CliProxySettings.parse(it) }.getOrNull() }
            return CliProxyPlan(
                settings = parsed ?: CliProxySettings(),
                override = override?.let(Parser::parseProxyOverride),
                settingsFound = parsed != null,
                rawMode = raw?.let(::rawProxyMode),
            )
        }

        /** 从存档 JSON 里抠 `proxy.mode` 的原文（不看别的字段；抠不出来当没有）。 */
        internal fun rawProxyMode(text: String): String? {
            val root = runCatching { FeedJson.parseToJsonElement(text) as? JsonObject }.getOrNull() ?: return null
            val proxy = root["proxy"] as? JsonObject ?: return null
            return (proxy["mode"] as? JsonPrimitive)?.contentOrNull
        }
    }
}

/** `--proxy` 解析后的结果。 */
sealed interface ProxyOverride {
    /** `--proxy none`：显式直连。 */
    data object None : ProxyOverride

    data class Custom(val host: String, val port: Int, val type: ProxyType) : ProxyOverride
}

/**
 * 参数 / 本地文件里读出来的东西有问题（发请求之前就报，不连任何网络）。
 * 映射成 `bad_input`（退出码 2）。
 */
open class CliUsageError(message: String) : Exception(message)

/** `--proxy` 写法不对 / 读不懂设置里的代理模式。 */
class CliProxyError(message: String) : CliUsageError(message)

/**
 * 点亮的平台这次**没有可用的代理**（fail-closed）：一个请求都没发。
 * 映射成 `proxy_unavailable`（退出码 6）——和普通网络错误分开，agent 才知道要让人去改设置 / 加 `--proxy`。
 *
 * 类型本身在 `:core`（`feedhub.core.ProxyUnavailableException`）：**Rust 平台层的宿主回调也抛它**，
 * 这样 App / cli / Rust 三处对"代理不可用"是同一个类型，跨 FFI 也能原样带回来。
 */
typealias ProxyUnavailableException = feedhub.core.ProxyUnavailableException

object Parser {
    /**
     * 认 `socks5://host:port` / `socks://host:port` / `http://host:port` / `https://host:port` / `none`。
     * 别的写法抛 [CliProxyError]（clikt 会打出来当参数错误）。
     */
    fun parseProxyOverride(text: String): ProxyOverride {
        val t = text.trim()
        if (t.equals("none", ignoreCase = true) || t.equals("direct", ignoreCase = true)) return ProxyOverride.None
        val scheme = t.substringBefore("://", missingDelimiterValue = "").lowercase()
        if (scheme.isEmpty()) {
            throw CliProxyError("--proxy 要写成 socks5://host:port、http://host:port 或 none（收到的是 \"$t\"）")
        }
        val type = when (scheme) {
            "socks5", "socks" -> ProxyType.SOCKS5
            "http", "https" -> ProxyType.HTTP
            else -> throw CliProxyError("--proxy 不认识的协议 $scheme（只支持 socks5:// 和 http://，或者 none）")
        }
        val rest = t.substringAfter("://")
        val host = rest.substringBeforeLast(":", missingDelimiterValue = "")
        val portText = rest.substringAfterLast(":", missingDelimiterValue = "")
        val port = portText.toIntOrNull()
        if (host.isBlank() || port == null || port !in 1..65535) {
            throw CliProxyError("--proxy 的地址 / 端口不对：\"$t\"（要写成 $scheme://host:port）")
        }
        return ProxyOverride.Custom(host, port, type)
    }
}

/**
 * 发请求前的 fail-closed 检查（位置和 App 的 `ProxyConfigGuardInterceptor` 一样：**应用拦截器、
 * 还没开始找连接**，所以 OkHttp 的 `RetryAndFollowUpInterceptor` 不会重试——在 `ProxySelector.select()`
 * 里抛反而会被当成"这条线路不可用"而无限重试，见 `core/Proxy.kt` 的注释）。
 *
 * 只拦 [blocked] 里那几个平台对应的域名；别的域名原样放行（没点亮的平台照常直连）。
 *
 * **出口锁关着**时不拦，而是把"改成直连"的原因打一行提示（每个平台一次，不含地址）——
 * 用户主动关掉了出口锁，直连是他要的，但"说好的走代理结果直连了"不该无声无息。
 */
class CliProxyGuardInterceptor(
    private val blocked: Map<String, Platform>,
    /** 出口锁关闭而直连时的提示写到哪（默认 stderr）。 */
    private val notice: (String) -> Unit = { System.err.println(it) },
    private val plan: () -> CliProxyPlan?,
) : Interceptor {
    /** 已经提示过的平台（同一次运行里每个平台只提示一次）。 */
    private val told: MutableSet<Platform> = java.util.Collections.synchronizedSet(mutableSetOf())

    private fun platformOf(host: String): Platform? {
        val h = host.lowercase().substringBefore(':')
        if (h.isEmpty()) return null
        return blocked.entries.firstOrNull { (suffix, _) -> h == suffix || h.endsWith(".$suffix") }?.value
    }

    override fun intercept(chain: Interceptor.Chain): Response {
        val host = chain.request().url.host
        val platform = platformOf(host) ?: return chain.proceed(chain.request())
        val current = plan() ?: return chain.proceed(chain.request())
        val choice = current.proxyFor(platform)
        if (!choice.blocked) {
            val reason = choice.fallbackReason
            if (reason != null && told.add(platform)) {
                notice("出口锁关闭，直连：${platform.label}（${platform.key}）—— $reason")
            }
            return chain.proceed(chain.request())
        }
        throw blockedMessage(platform, current)
    }
}

/** 点亮的平台这次没有可用代理时的错误：说清楚现在的状态和怎么改。 */
internal fun blockedMessage(platform: Platform, plan: CliProxyPlan): IOException {
    val reason = plan.unsupportedMode()?.let { mode ->
        "设置里的「代理」是 cli 不认识的模式 \"$mode\"（可能是 App 里新加的档，cli 还没跟上）"
    } ?: "设置里「代理」是「不用」"
    return ProxyUnavailableException(
        "${platform.label}（${platform.key}）在「走代理的平台」里，但${reason}——这样会直连，" +
            "cli 不这么干（fail-closed），一个请求都不会发。要么在 App 的设置里把「代理」改成" +
            "「系统代理」/「指定」，要么这次加 --proxy socks5://host:port；确实要直连就显式写 --proxy none，" +
            "或者把设置里的「代理不可用时禁止直连」关掉（那样 cli 和 App 都会直连）。",
    )
}
