package feedhub.core

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator
import okhttp3.Interceptor
import okhttp3.Response
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Proxy
import java.net.ProxySelector
import java.net.SocketAddress
import java.net.URI

/**
 * 按平台指定代理。
 *
 * 一条内容要用到的网络有三处，三处都要跟着这个平台走，否则卡片上的图、视频就加载不出来：
 * 1. 接口请求：各 Source 用 [SourceContext.client] 派生自己的客户端；
 * 2. 图片：各平台的图混在一个信息流里，图片加载器只有一个客户端，按域名认平台（[Source.hosts] + [platformForHost]）；
 * 3. 视频流：播放器不走 OkHttp，由各引擎自己设（桌面见 `app/.../video/PlayerProxy.kt`）。
 *
 * 配置从哪来：core 不依赖 app，所以这里只有一个 [ProxyProvider] 接口，app 用设置（`app.settings`）实现它，cli / 测试用
 * [ProxyProvider.SYSTEM]（= 加这个功能之前的行为）。
 */

/** 代理类型。 */
@Serializable
enum class ProxyType {
    HTTP,
    SOCKS5,
}

/**
 * 一个平台怎么连网络。
 *
 * 存进设置里，所以每个子类的字段都带默认值 / 是基本类型；以后加类型要给旧存档留退路（老存档里没有这个字段，
 * 读出来是 [System]，见 `AppSettings` 的注释）。
 *
 * 判别键用 `mode` 而不是 kotlinx 默认的 `type`：[Custom] 自己就有一个叫 `type` 的字段（HTTP / SOCKS5），
 * 不换名字会撞在一起。
 */
@OptIn(ExperimentalSerializationApi::class)
@JsonClassDiscriminator("mode")
@Serializable
sealed interface ProxyMode {
    /**
     * 直连：不走任何代理（OkHttp 的 `Proxy.NO_PROXY`）。
     *
     * **播放器这一侧「直连」目前做不到**：GStreamer / VLC 由引擎自己下载，我们把 `souphttpsrc.proxy`
     * 留空、VLC 不加代理选项时，它们**仍然**会读 `http_proxy` 这类环境变量，所以"直连"只对 OkHttp
     * （接口和图片）有效。
     */
    @Serializable
    @SerialName("direct")
    data object Direct : ProxyMode

    /**
     * 跟随系统：交给 JVM 的系统代理设置（[java.net.ProxySelector.getDefault]）。
     *
     * JVM 只有在**读了 `java.net.useSystemProxies=true`** 之后才会去问系统 / 环境变量里的代理；
     * 这个属性在进程启动时只读一次，所以在建任何网络客户端之前调 [enableSystemProxies]（`LocalFiles.context`
     * 里已经调了）。不设的话 JVM 不读 `http_proxy` 环境变量、也不读桌面设置，这一档实际等于直连。
     * 这是没有配代理时的行为，也是加这个功能之前的行为。
     *
     * 播放器（GStreamer / VLC）的"系统"是它们**自己**读环境变量，和 JVM 的这套不是一回事。
     */
    @Serializable
    @SerialName("system")
    data object System : ProxyMode

    /** 指定代理：[type] 是 HTTP（CONNECT 隧道）还是 SOCKS5。 */
    @Serializable
    @SerialName("custom")
    data class Custom(
        val host: String,
        val port: Int,
        val type: ProxyType = ProxyType.HTTP,
    ) : ProxyMode {
        /** 界面上显示用，例如 `127.0.0.1:10808`（SOCKS5 前面带类型）。 */
        val label: String get() = "$host:$port"

        /**
         * 地址 / 端口填全了没有。设置页是边打字边存的，中间状态（host 空着、端口不是数字）会是 false。
         *
         * false 时**不再退回系统代理**。现在这个平台的请求会直接失败：[ProxyConfigGuardInterceptor]
         * 在发请求前抛一个说得清的 [IOException]，域名选择器也会给一个必然连不上的替身地址
         * （[PlatformProxySelector.select]，双保险：任何一条路径都不会悄悄直连）。
         */
        val valid: Boolean get() = host.isNotBlank() && port in 1..65535
    }

    /**
     * **内置代理核心**在跑：走它在本机开的 mixed 入站的 SOCKS5。
     *
     * 这一档**不会**被序列化进设置（设置里存的是 [ProxyConfig.Mode.BUILTIN]），每次请求由
     * `ProxyCore.proxyMode()` 现造：核心在跑给真端口，没在跑给 `port = 0` + [reason]。
     *
     * 没在跑时**直接失败**（fail-closed），绝不悄悄直连：和 [Custom] 没填全时同一个机制——
     * [ProxyConfigGuardInterceptor] 在发请求之前抛 [IOException]，[PlatformProxySelector] 给一个
     * 必然连不上的替身地址。
     */
    data class BuiltinCore(
        /** 核心的 mixed 入站端口；不在 1..65535 时 = 没在跑（[valid] 为 false）。 */
        val port: Int = 0,
        /** 没在跑的原因（已经是不含机密的文案），会出现在抛给用户的异常里。 */
        val reason: String? = null,
    ) : ProxyMode {
        val valid: Boolean get() = port in 1..65535

        /** 界面上显示用，例如 `内置核心 · 127.0.0.1:41234`。 */
        val label: String get() = if (valid) "内置核心 · 127.0.0.1:$port" else "内置核心 · 没在运行"
    }
}

/**
 * **一个平台**的网络怎么走。
 *
 * 三档，和设置页每平台那一行一一对应：
 * - [Follow]：跟随全局（[ProxyConfig] + 出口锁），**默认**；这就是加这个功能之前的行为
 *   （境外平台在「走代理的平台」里 → 用全局代理，国内平台不在 → 直连）。
 * - [Direct]：直连。**境外平台不提供这一档**（红线：境外平台绝不直连）；
 *   设置（`AppSettings.proxyChoiceFor`）和 Rust 的判定（`feedhub.proxy.ProxyRules.resolve`）拿到境外平台的
 *   [Direct] 会当成 [Follow]。
 * - [Custom]：这个平台单独走一个代理（HTTP / SOCKS5，地址 + 端口）。
 *
 * 存进设置里（`AppSettings.platformProxy`），所以每个子类的字段都要能序列化、带默认值。
 * 判别键用 `mode`（和 [ProxyMode] 一样）：[Custom] 自己就有一个叫 `type` 的字段。
 */
@OptIn(ExperimentalSerializationApi::class)
@JsonClassDiscriminator("mode")
@Serializable
sealed interface ProxyChoice {
    /** 跟随全局（默认）：按「走代理的平台」+ 全局 [ProxyConfig] + 出口锁，也就是加这个功能之前的行为。 */
    @Serializable
    @SerialName("follow")
    data object Follow : ProxyChoice

    /**
     * 直连（只给国内平台；境外平台**没有**这一档，见 `feedhub.proxy.ProxyRules.resolve`）。
     * 和「走代理的平台」不点亮是同一个意思，所以切换回 [Follow] 时会恢复原来的点亮状态。
     */
    @Serializable
    @SerialName("direct")
    data object Direct : ProxyChoice

    /**
     * 这个平台单独走一个代理。[host] + [port] 没填全时 [valid] 为 false，
     * 这个平台的请求**直接失败**（fail-closed，绝不回退直连——出口锁对这两档不起作用）。
     */
    @Serializable
    @SerialName("custom")
    data class Custom(
        val host: String = "",
        val port: Int = 0,
        val type: ProxyType = ProxyType.HTTP,
    ) : ProxyChoice {
        /** 界面上显示用，例如 `127.0.0.1:10808`。 */
        val label: String get() = "$host:$port"

        /** 地址 / 端口填全了没有（设置页边打字边存，中间状态是 false）。 */
        val valid: Boolean get() = host.isNotBlank() && port in 1..65535

        /** 换成实际发请求用的 [ProxyMode]（地址没填全时是一个 `valid == false` 的 Custom，请求会直接失败）。 */
        fun toProxyMode(): ProxyMode = ProxyMode.Custom(host, port, type)
    }
}

// **判定搬进 Rust 了**：`resolveProxyMode` / `proxyFallbackReason`。这个文件里只剩
// **宿主侧的活**——把 `ProxyMode` 换成 `java.net.Proxy` / `ProxySelector`、在发请求前拦下"没填全的指定代理"
// （fail-closed）。判定的唯一一份在 `rust/crates/feedhub-core/src/proxy.rs`，Kotlin 门面在
// `rust-bridge/.../feedhub/proxy/ProxyRules.kt`（App 的 `AppSettings.proxyFor` 和 cli 的
// `CliProxyPlan.proxyFor` 都走它）。原来的 Kotlin 实现在 `core/src/testFixtures/.../feedhub/core/legacy/ProxyLegacy.kt`，
// 只给 `ProxyRulesParityTest` 对拍用。

/**
 * 「网络」设置里那一份**全局**代理配置：`不用 / 系统代理 / 指定`，
 * 「指定」时用 [host] / [port] / [type]。
 *
 * 和 [ProxyMode] 的分工：[ProxyConfig] 是**存进设置里的用户选择**（全局一份，不再是"默认 + 每个平台四档覆盖"）；
 * [ProxyMode] 是**解析之后**这次请求实际怎么走（[ProxyProvider] 的返回值，按平台给）。
 * 一个平台走不走代理由两处决定：这个平台在 [ProxyChoice] 里**单独指定**了就走它
 * （判定在 Rust，见 `feedhub.proxy.ProxyRules.resolve`），否则看全局：平台在「走代理的平台」集合里 →
 * [toProxyMode]；不在 → 直连；
 * [mode] 是 [Mode.NONE] → 一律直连。境外平台（YouTube / X / Reddit / TikTok）默认在那个集合里。
 */
@Serializable
data class ProxyConfig(
    /**
     * 默认 [Mode.SYSTEM]：加这个功能之前的行为就是跟系统的代理设置。
     * 「走代理的平台」默认只点亮 YouTube（境外平台默认走代理），国内七家默认直连。
     */
    val mode: Mode = Mode.SYSTEM,
    val host: String = "",
    val port: Int = 0,
    val type: ProxyType = ProxyType.HTTP,
) {
    /** 界面上那四档：`不用`（直连）/ `系统代理`（JVM 的系统代理）/ `指定`（[host] + [port]）/ `内置核心`。 */
    @Serializable
    enum class Mode { NONE, SYSTEM, CUSTOM, BUILTIN }

    /** 「指定」时这份地址对应的 [ProxyMode.Custom]（地址没填全时它是 invalid 的，见 [valid]）。 */
    val custom: ProxyMode.Custom get() = ProxyMode.Custom(host, port, type)

    /**
     * [mode] 对应的 [ProxyMode]：`不用` = 直连。
     *
     * `内置核心` 那一档的端口 / 是否在跑只有 `ProxyCore` 知道，所以调用方要把
     * `ProxyCore.proxyMode()` 的结果当 [builtin] 传进来（不传 = 当成"没在运行"，fail-closed）。
     */
    fun toProxyMode(builtin: ProxyMode = ProxyMode.BuiltinCore(0, "内置核心没有运行")): ProxyMode = when (mode) {
        Mode.NONE -> ProxyMode.Direct
        Mode.SYSTEM -> ProxyMode.System
        Mode.CUSTOM -> custom
        Mode.BUILTIN -> builtin
    }

    /**
     * 这份配置现在可用吗：`不用` / `系统代理` / `内置核心` 永远可用（内置核心有没有起来由
     * `ProxyCore` 那边判断，起来了才给端口）；`指定` 要地址和端口都填了（[ProxyMode.Custom.valid]）。
     * 不可用时点亮的平台会**直接失败**，不会悄悄退回系统代理。
     */
    val valid: Boolean get() = mode != Mode.CUSTOM || custom.valid
}

/** [ProxyType] 换成 OkHttp / JDK 的类型。 */
fun ProxyType.toOkHttpType(): Proxy.Type = when (this) {
    ProxyType.HTTP -> Proxy.Type.HTTP
    ProxyType.SOCKS5 -> Proxy.Type.SOCKS
}

/**
 * 换成 JDK 的 [Proxy]；null = 不覆盖（跟随系统）。[ProxyMode.Custom] 地址没填全时给一个**必然连不上**的
 * 替身地址，不会悄悄退回系统代理。
 * 给不走 [PlatformProxySelector] 的地方用（目前只有测试）；接线都用选择器 + [ProxyConfigGuardInterceptor]。
 */
fun ProxyMode.toOkHttpProxy(): Proxy? = when (this) {
    ProxyMode.System -> null
    ProxyMode.Direct -> Proxy.NO_PROXY
    is ProxyMode.Custom -> if (valid) Proxy(type.toOkHttpType(), InetSocketAddress(host, port)) else deadProxy(this)
    is ProxyMode.BuiltinCore -> if (valid) {
        Proxy(Proxy.Type.SOCKS, InetSocketAddress("127.0.0.1", port))
    } else {
        deadBuiltinProxy()
    }
}

/**
 * 打开 JVM 的"跟随系统代理"（`java.net.useSystemProxies`）。**要在建任何网络客户端之前调**
 * （[feedhub.core.local.LocalFiles.context] 里已经调了；cli / 桌面 App 都经过它）：
 * [ProxyMode.System] 用的是 [java.net.ProxySelector.getDefault]，JVM 只在启动时读一次这个属性；
 * 不设的话它既不读 `http_proxy` 环境变量、也不读桌面设置，"系统"实际等于直连。
 * 重复调用没副作用。
 */
fun enableSystemProxies() {
    System.setProperty("java.net.useSystemProxies", "true")
}

/** 「指定代理」的地址 / 端口没填全时的错误：要能直接看懂该怎么办。 */
internal fun invalidCustomProxy(mode: ProxyMode.Custom): ProxyUnavailableException = ProxyUnavailableException(
    "「指定代理」的地址或端口没填全（当前 host=\"${mode.host}\" port=${mode.port}）：按约定这个平台先不发请求，" +
        "不会退回系统代理。请在设置里把地址和端口填全，或者改成「直连」/「系统」。",
)

/** 没填全的「指定代理」的替身：本机必然连不上的地址（不会真的走任何网络，也绝不悄悄直连）。 */
internal fun deadProxy(mode: ProxyMode.Custom): Proxy =
    Proxy(mode.type.toOkHttpType(), InetSocketAddress("127.0.0.1", 1))

/** 内置代理核心没在跑时的替身：同样是本机必然连不上的地址（fail-closed，绝不悄悄直连）。 */
internal fun deadBuiltinProxy(): Proxy = Proxy(Proxy.Type.SOCKS, InetSocketAddress("127.0.0.1", 1))

/**
 * 内置代理核心没在跑时的错误：说清楚"内置代理没有运行"，以及为什么（`reason` 已经是不含机密的文案）。
 * 和「指定代理没填全」一样在发请求之前抛（fail-closed），绝不悄悄直连。
 */
internal fun builtinCoreNotRunning(mode: ProxyMode.BuiltinCore): ProxyUnavailableException = ProxyUnavailableException(
    "内置代理没有运行" + (mode.reason?.takeIf { it.isNotBlank() }?.let { "：$it" } ?: "") +
        "。按约定走代理的平台先不发请求，不会退回直连；请到「设置 → 进阶设置 → 内置代理核心」里看一下。",
)

/**
 * 「指定代理」没填全时，在请求发出去之前抛一个说得清的错误。
 *
 * **为什么不直接让 [PlatformProxySelector] 抛**：OkHttp 的 `RouteSelector` 把 `select()` 抛出的异常
 * 当作"这条线路不可用"而**无限重试**；而应用拦截器里抛 [IOException] 时
 * 还没开始找连接，`RetryAndFollowUpInterceptor.recover()` 不会重试（和 `XhsApi.SignInterceptor` 里
 * "没有 a1 Cookie" 的写法一样）。
 *
 * @param choose 这次请求的主机用哪份代理配置（null = 认不出平台 / 没有配置，不拦）。
 */
class ProxyConfigGuardInterceptor(private val choose: (host: String) -> ProxyMode?) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        when (val mode = choose(chain.request().url.host)) {
            is ProxyMode.Custom -> if (!mode.valid) throw invalidCustomProxy(mode)
            // 「内置核心」没在跑：直接失败，绝不悄悄直连（fail-closed）
            is ProxyMode.BuiltinCore -> if (!mode.valid) throw builtinCoreNotRunning(mode)
            else -> {}
        }
        return chain.proceed(chain.request())
    }
}

/** 按平台查代理配置。app 用设置实现；cli 用它自己的那个；单元测试用 [SYSTEM] / [DIRECT]。 */
fun interface ProxyProvider {
    fun proxyFor(platform: Platform): ProxyMode

    companion object {
        /** 全都跟随系统——没有接设置的地方（cli、单元测试、[SourceContext.inMemory]）用它。 */
        val SYSTEM: ProxyProvider = ProxyProvider { ProxyMode.System }

        /**
         * 全都直连。**单元测试里造平台对象（尤其是 Rust 联网层那种）用这个**。
         *
         * 为什么测试不该随手用 [SYSTEM]：`System` 这一档的语义是"问真的系统代理"（OkHttp 侧是
         * `PlatformProxySelector` → JVM 的 `ProxySelector`；Rust 侧见 `ProxyTarget.of`），而测试是离线跑的
         * （根 `build.gradle.kts` 的 `offlineGuard()` 把 `-DsocksProxyHost` 指到一个必然连不上的端口）。
         * 那个选择器对 `127.0.0.1`（MockWebServer）回直连、对平台主域名回那个死代理；走 OkHttp 的代码每个
         * 请求现问选择器、所以碰巧还通，而 Rust 联网层是**建栈时按平台主域名问一次**，就会去连那个死端口。
         * 测试要的是"这次请求别走代理"，[DIRECT] 就是这个意思。
         */
        val DIRECT: ProxyProvider = ProxyProvider { ProxyMode.Direct }
    }
}

/**
 * 每次请求现查代理的 [ProxySelector]。
 *
 * 为什么用 ProxySelector 而不是 `OkHttpClient.Builder.proxy(...)`：Source 的客户端是构造时建一次、之后长期用的，
 * 而设置页改了代理要立刻生效。`select` 每个请求都会调，所以 [choose] 读到的一定是最新配置，不用重建客户端。
 * （OkHttp 会拿 ProxySelector 判断"两个客户端算不算同一个地址"，所以同一个平台要共用一份实例，
 * 见 [SourceContext.client]；不同平台之间因此不会串连接池。）
 *
 * **两个已知的边角**（都在 `AppGraph` / `SourceContext` 侧处理）：
 * 1. OkHttp 的连接池对"主机名相同"的连接直接判为可用（`RealConnection.isEligible`）：用 ProxySelector 时
 *    选出来的代理**不进** `Address`（`Address.proxy` 是 null），所以改完设置后那条主机上已经建好的连接会被
 *    接着用。换代理要**换掉整份客户端（新的连接池）**，见 `AppGraph.watchProxySettings`。
 * 2. 地址没填全的 [ProxyMode.Custom] 给一个**必然连不上**的替身地址（[deadProxy]），真正的报错由
 *    [ProxyConfigGuardInterceptor] 在发请求之前抛出——`select()` 自己**不能**抛，OkHttp 会无限重试。
 *
 * @param choose 按主机名给代理。绑定到某个平台的客户端直接返回那个平台的配置；共用的客户端（图片加载器）用 [platformForHost]。
 */
class PlatformProxySelector(
    private val choose: (host: String) -> ProxyMode,
    /** [ProxyMode.System] 时交给它（默认是 JVM 的系统代理选择器）。 */
    private val system: ProxySelector = ProxySelector.getDefault() ?: NoProxySelector,
) : ProxySelector() {
    override fun select(uri: URI): List<Proxy> = when (val mode = choose(uri.host.orEmpty())) {
        // 系统选择器可能返回空表（= 没有代理），OkHttp 需要一个明确的答案
        ProxyMode.System -> system.select(uri).ifEmpty { DIRECT }
        ProxyMode.Direct -> DIRECT
        // 地址没填全：给个必然连不上的替身（绝不悄悄直连）。报错在 ProxyConfigGuardInterceptor 里，
        // 因为 select() 抛异常会让 OkHttp 无限重试。
        is ProxyMode.Custom -> if (mode.valid) {
            listOf(Proxy(mode.type.toOkHttpType(), InetSocketAddress(mode.host, mode.port)))
        } else {
            listOf(deadProxy(mode))
        }
        // 内置核心：在跑就给本机 SOCKS5；没在跑给必然连不上的替身（报错在 ProxyConfigGuardInterceptor）
        is ProxyMode.BuiltinCore -> if (mode.valid) {
            listOf(Proxy(Proxy.Type.SOCKS, InetSocketAddress("127.0.0.1", mode.port)))
        } else {
            listOf(deadBuiltinProxy())
        }
    }

    override fun connectFailed(uri: URI, sa: SocketAddress, ioe: IOException) {
        // 连接失败的回调，系统选择器可能拿来标记代理不可用；我们只转发，自己不做任何重试
        runCatching { system.connectFailed(uri, sa, ioe) }
    }

    private companion object {
        val DIRECT = listOf<Proxy>(Proxy.NO_PROXY)

        /** [ProxySelector.getDefault] 为 null（很少见）时的替身：一律直连。 */
        object NoProxySelector : ProxySelector() {
            override fun select(uri: URI): List<Proxy> = DIRECT
            override fun connectFailed(uri: URI, sa: SocketAddress, ioe: IOException) {}
        }
    }
}

/**
 * 按主机名找平台的表：键是域名后缀（[Source.hosts]），值是这个后缀属于哪个平台。
 * 同一个后缀被两家声明时保留先出现的（插件清单 `plugins.toml` 的顺序）。
 */
fun platformHostTable(hostsByPlatform: Map<Platform, List<String>>): Map<String, Platform> {
    val table = LinkedHashMap<String, Platform>()
    for ((platform, hosts) in hostsByPlatform) for (h in hosts) table.putIfAbsent(h.lowercase(), platform)
    return table
}

/**
 * [host] 属于哪家：`host == 后缀` 或者 `host.endsWith(".后缀")` 才算命中（后缀前面必须有那个点，
 * 否则 `notbilibili.com` 会被当成 B站）。
 */
fun Map<String, Platform>.platformForHost(host: String): Platform? {
    val h = host.lowercase().substringBefore(':')
    if (h.isEmpty()) return null
    return entries.firstOrNull { (suffix, _) -> h == suffix || h.endsWith(".$suffix") }?.value
}

/**
 * 给共用的客户端（信息流里只建一个的图片加载器）按域名选代理：认识的域名用那个平台的配置，不认识就 [fallback]。
 * 播放器不用它（它不走 OkHttp）。
 */
fun platformProxySelector(
    provider: ProxyProvider,
    hostTable: Map<String, Platform>,
    fallback: ProxyMode = ProxyMode.System,
): ProxySelector = platformProxySelector(provider, { hostTable }, fallback)

/**
 * 同上，但表是**每次选代理现取**的（[hostTable] 可以晚于构造这个选择器才知道，甚至每次不一样）。
 *
 * 而 Source 是懒建的
 * （信息流不刷新就不该把它们建出来），只能等第一次真的发请求再建表。
 * 这个重载留着给"表晚于构造才知道"的调用方。
 */
fun platformProxySelector(
    provider: ProxyProvider,
    hostTable: () -> Map<String, Platform>,
    fallback: ProxyMode = ProxyMode.System,
): ProxySelector = PlatformProxySelector(choose = { host -> hostTable().platformForHost(host)?.let(provider::proxyFor) ?: fallback })
