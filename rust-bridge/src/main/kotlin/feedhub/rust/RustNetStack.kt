package feedhub.rust

import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import feedhub.core.ProxyUnavailableException
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.CookieJar as FfiCookieJarHandle
import uniffi.feedhub_ffi.FfiHostSpec
import uniffi.feedhub_ffi.FfiHttpRequest
import uniffi.feedhub_ffi.FfiHttpResponse
import uniffi.feedhub_ffi.FfiHostException
import uniffi.feedhub_ffi.FfiNetCookies
import uniffi.feedhub_ffi.FfiNetHostHandle
import uniffi.feedhub_ffi.FfiNetPlatformProxy
import uniffi.feedhub_ffi.FfiNetProxy
import uniffi.feedhub_ffi.FfiNetStack
import uniffi.feedhub_ffi.HostCookies
import uniffi.feedhub_ffi.HostHttp
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Proxy
import java.net.ProxySelector
import java.net.URI

/**
 * **Rust 联网层的 Kotlin 门面**。
 *
 * 切过来之后平台请求的收发在 Rust 里（[FfiNetStack] + `feedhub-net`），Kotlin 只剩三件事：
 *
 * 1. 把"这次用哪份代理"算成 [FfiNetPlatformProxy] 列表（判定在 `feedhub-core/src/proxy.rs`，
 *    门面在 `feedhub.proxy.ProxyRules`）；
 * 2. 把平台那几样"原来写在 `addInterceptor` 上的参数"（默认头 / 最小间隔 / 抖动 / 读超时 /
 *    跟不跟重定向）说给 Rust（[FfiHostSpec]）；
 * 3. 把 Rust 的 `HostHttp` / `HostCookies` 句柄塞进平台对象（[host] / [cookies]）。
 *
 * **平台门面一行不用改**：[host] 返回的就是 `HostHttp`、[cookies] 返回的就是 `HostCookies`，
 * 和原来的 [RustHttpHost] / [RustCookieHost] 是同一个接口。
 *
 * ## 代理档变了就整份换新
 *
 * 和 `AppGraph.watchProxySettings` / `SourceContext.withFreshConnectionPool()` 对齐：代理设置变了
 * 建一份新的 [RustNetStack]（新的连接池），旧的那份让它的连接自然回收。
 *
 * ## 红线
 *
 * 代理不可用时 **Rust 侧一个字节都不发**（`NetClient::execute` 的第一步判 `ProxyUnavailable`），
 * 这里只把 `FfiHostException.ProxyUnavailable` 翻回 [ProxyUnavailableException]（cli 的退出码 6）。
 */
class RustNetStack private constructor(
    private val stack: FfiNetStack,
    /** 这份栈用的罐子（[cookies] 和它指向同一个）。 */
    private val jar: FeedCookieJar,
) {
    /**
     * 建一家平台的 [HostHttp]。
     *
     * [platform] / [defaultHeaders] / [minIntervalMs] / [jitterMs] / [readTimeoutMs] /
     * [followRedirects] 就是原来 `ctx.client(platform)` 后面那几个 `addInterceptor` 的参数。
     */
    fun host(
        platform: Platform,
        defaultHeaders: Map<String, String> = emptyMap(),
        /** 默认头只对这些主机生效（`DefaultHeadersInterceptor` 的 `hostFilter`）。空 = 全部生效。 */
        defaultHeaderHosts: List<String> = emptyList(),
        minIntervalMs: Long = 0,
        jitterMs: Long = 0,
        backgroundGraceMs: Long = 0,
        readTimeoutMs: Long = 0,
        followRedirects: Boolean = true,
        randomSeed: Long = System.nanoTime(),
    ): RustNetHttpHost = hostFor(
        platform = platform,
        defaultHeaders = defaultHeaders,
        defaultHeaderHosts = defaultHeaderHosts,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        backgroundGraceMs = backgroundGraceMs,
        readTimeoutMs = readTimeoutMs,
        followRedirects = followRedirects,
        randomSeed = randomSeed,
    )

    private fun hostFor(
        platform: Platform,
        defaultHeaders: Map<String, String>,
        defaultHeaderHosts: List<String>,
        minIntervalMs: Long,
        jitterMs: Long,
        backgroundGraceMs: Long,
        readTimeoutMs: Long,
        followRedirects: Boolean,
        randomSeed: Long,
    ): RustNetHttpHost {
        RustNative.ensureLoaded()
        val spec = FfiHostSpec(
            platform = platform.key,
            // **有序**：`Map` 的插入顺序就是 OkHttp 上那层 `DefaultHeadersInterceptor` 补头的顺序
            defaultHeaders = defaultHeaders.map { (k, v) -> "$k: $v" },
            defaultHeadersHosts = defaultHeaderHosts,
            minIntervalMs = minIntervalMs,
            jitterMs = jitterMs,
            backgroundGraceMs = backgroundGraceMs,
            randomSeed = randomSeed,
            readTimeoutMs = readTimeoutMs,
            followRedirects = followRedirects,
            // 登录头 / txid 是 **X 专用**的（别的平台一个都不带），所以默认空；
            // 只有 X 的门面用 [specWithHooks] 填。
            authHeaders = emptyList(),
            txid = false,
        )
        return RustNetHttpHost(stack.host(spec))
    }

    /**
     * Rust 侧的栈句柄（平台对象的 `withNet` 构造函数要它）。
     *
     * **只给平台门面用**：正常路径是 [host] / [cookies]（给出 `HostHttp` / `HostCookies`）；
     * 走 `withNet` 的那几家要的是"Rust 自己在内部发"，所以要把栈本身交过去。
     */
    internal val inner: FfiNetStack get() = stack

    /**
     * 把"这家平台的联网配置"翻成过边界的 [FfiHostSpec]（`withNet` 那条路用）。
     *
     * 和 [host] 是同一份口径，只是不立刻建句柄。
     */
    internal fun spec(
        platform: Platform,
        defaultHeaders: Map<String, String> = emptyMap(),
        /** 默认头只对这些主机生效（`hostFilter`）。空 = 全部生效。 */
        defaultHeaderHosts: List<String> = emptyList(),
        minIntervalMs: Long = 0,
        jitterMs: Long = 0,
        backgroundGraceMs: Long = 0,
        readTimeoutMs: Long = 0,
        followRedirects: Boolean = true,
        randomSeed: Long = System.nanoTime(),
    ): FfiHostSpec = FfiHostSpec(
        platform = platform.key,
        defaultHeaders = defaultHeaders.map { (k, v) -> "$k: $v" },
        defaultHeadersHosts = defaultHeaderHosts,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        backgroundGraceMs = backgroundGraceMs,
        randomSeed = randomSeed,
        readTimeoutMs = readTimeoutMs,
        followRedirects = followRedirects,
        authHeaders = emptyList(),
        txid = false,
    )

    /**
     * 和 [spec] 一样，但**带 X 的两个钩子**（登录头 / txid）。
     *
     * 为什么单独一个函数：这两个钩子只有 X 用，塞进 [spec] 会让其余 10 家的调用点多两个
     * `= emptyList()` / `= false` 的噪音（而它们对"这条请求带不带登录头"根本没有概念）。
     */
    internal fun specWithHooks(
        platform: Platform,
        defaultHeaders: Map<String, String> = emptyMap(),
        defaultHeaderHosts: List<String> = emptyList(),
        minIntervalMs: Long = 0,
        jitterMs: Long = 0,
        backgroundGraceMs: Long = 0,
        readTimeoutMs: Long = 0,
        followRedirects: Boolean = true,
        randomSeed: Long = System.nanoTime(),
    ): FfiHostSpec = FfiHostSpec(
        platform = platform.key,
        defaultHeaders = defaultHeaders.map { (k, v) -> "$k: $v" },
        defaultHeadersHosts = defaultHeaderHosts,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        backgroundGraceMs = backgroundGraceMs,
        randomSeed = randomSeed,
        readTimeoutMs = readTimeoutMs,
        followRedirects = followRedirects,
        authHeaders = emptyList(),
        txid = true,
    )

    /** 同一个罐子的 [HostCookies] 门（登录态判定 / 种 Cookie）。 */
    fun cookies(): HostCookies {
        RustNative.ensureLoaded()
        return RustNetCookies(stack.cookies())
    }

    /** 取消这份栈上所有在飞的请求（`HostedCall.blocking` 在协程取消时调它）。 */
    fun cancel() {
        stack.cancel()
    }

    companion object {
        /**
         * 建一份（[proxies] 是"平台 → 代理档"，[defaultProxy] 是没列出那些平台的档；null = 直连）。
         *
         * [jar] 必须和 App 实际用的那个罐子**是同一个**（不然登录态会分叉）。
         */
        fun create(
            proxies: List<Pair<Platform, ProxyTarget>>,
            defaultProxy: ProxyTarget?,
            jar: FeedCookieJar,
        ): RustNetStack {
            RustNative.ensureLoaded()
            val ffiJar = (jar as? feedhub.core.RustJarHandle)?.rustJarHandle() as? FfiCookieJarHandle
                ?: error(
                    "Rust 联网层要求 Cookie 罐也是 Rust 的（RustCookieJar / PersistentCookieJar / " +
                        "MemoryCookieJar），拿到的是 ${jar.javaClass.name}",
                )
            return RustNetStack(
                FfiNetStack(
                    proxies.map { (platform, target) ->
                        FfiNetPlatformProxy(platform.key, target.toFfi())
                    },
                    defaultProxy?.toFfi(),
                    ffiJar,
                    System.currentTimeMillis(),
                ),
                jar,
            )
        }
    }
}

/** 一份代理档（和 `feedhub_net::Proxy` 一一对应）。 */
sealed interface ProxyTarget {
    data object Direct : ProxyTarget

    data class Http(val host: String, val port: Int) : ProxyTarget

    data class Socks5(val host: String, val port: Int) : ProxyTarget

    /** fail-closed：这个平台这次要用代理但没有可用的（一个字节都不发）。 */
    data class Unavailable(val reason: String) : ProxyTarget

    fun toFfi(): FfiNetProxy = when (this) {
        Direct -> FfiNetProxy.Direct
        is Http -> FfiNetProxy.Http(host, port.toUShort())
        is Socks5 -> FfiNetProxy.Socks5(host, port.toUShort())
        is Unavailable -> FfiNetProxy.Unavailable(reason)
    }

    companion object {
        /**
         * 把 `feedhub.proxy.ProxyRules.resolve` 判出来的 [ProxyMode] 翻成这一层的代理档。
         *
         * 口径逐条照 `core/Proxy.kt` 的 `PlatformProxySelector`（OkHttp 那条路看到的就是它）：
         * - `Direct` → 直连；
         * - **`System` → 问 JVM 的 [ProxySelector]**（`PlatformProxySelector` 对 `System` 这一档就是
         *   `system.select(uri)`，空表当直连）。**Rust 这边没有"系统代理"这一说**（`wreq` 的
         *   `auto_sys_proxy` 被显式关掉了，那会和 `feedhub-core/src/proxy.rs` 的判定打架、也会破坏
         *   fail-closed），所以**由宿主在造平台对象时就把这一档解析成具体的一档**
         *   （`Http` / `Socks5` / `Direct`），Rust 收到的仍是"每个平台一档"的快照。
         *
         *   问哪个域名：[hosts] 是这个平台的主机表（**从插件描述来，不在这里硬编码平台**），
         *   取第一个当主域名，问 `https://<主域名>/`——就是 OkHttp 那边 `PlatformProxySelector`
         *   会拿到的那个 URI。取 [ProxySelector] 返回的**第一个** [java.net.Proxy]：
         *   `HTTP` → [Http]、`SOCKS` → [Socks5]、`DIRECT` → [Direct]。
         *   **解析不出来时是 [Unavailable]，不是直连**（`Unavailable` 一个字节都不发）。
         *   境外平台（X / YouTube / Reddit / TikTok）在"跟随全局 + 全局是系统代理"时就是这一档，
         *   **绝不能因为解析失败就悄悄直连**（红线）。
         * - `Custom` / `BuiltinCore` 没填全 / 没在跑 → **`Unavailable`**（fail-closed，一个字节都不发，
         *   文案和 Kotlin 的 `invalidCustomProxy` / `builtinCoreNotRunning` 一致）。
         *
         * **代理设置变了会重新解析**：`AppGraph.watchProxySettings` 盯着一份 `ProxySettingsKey`，
         * 变了就 debounce 之后调 `resetProxyClients()`——它换一份新连接池（`withFreshConnectionPool`）**并清空
         * Source 缓存**，于是下一次 `AppGraph.source(p)` 会把平台对象整个重造，这里就在新设置下再问一次选择器。
         * （OkHttp 那条路是每个请求现问 `PlatformProxySelector`，两边最后都跟着设置走；差别只是 Rust 这边
         * 多一次"重造对象"的时机。）
         *
         * @param hosts 平台的主机表（插件描述的 `hosts`，顺序有意义：第一个是主域名）。
         *   空表 → [Unavailable]（描述坏了，绝不退回直连）。
         * @param system 交给它问"系统代理"；默认就是 JVM 当前的那个（和 `PlatformProxySelector` 一样，
         *   `getDefault()` 为 null 时当直连）。测试注入用。
         */
        fun of(
            mode: ProxyMode,
            hosts: List<String>,
            system: ProxySelector? = ProxySelector.getDefault(),
        ): ProxyTarget = when (mode) {
            ProxyMode.Direct -> Direct
            ProxyMode.System -> resolveSystem(hosts, system)
            is ProxyMode.Custom -> if (mode.valid) {
                when (mode.type) {
                    ProxyType.HTTP -> Http(mode.host, mode.port)
                    ProxyType.SOCKS5 -> Socks5(mode.host, mode.port)
                }
            } else {
                Unavailable(
                    "「指定代理」的地址或端口没填全（当前 host=\"${mode.host}\" port=${mode.port}）：" +
                        "按约定这个平台先不发请求，不会退回系统代理。请在设置里把地址和端口填全，" +
                        "或者改成「直连」/「系统」。",
                )
            }
            is ProxyMode.BuiltinCore -> if (mode.valid) {
                // 内置核心是本机 mixed 入站的 SOCKS5（和 `PlatformProxySelector` 一样）
                Socks5("127.0.0.1", mode.port)
            } else {
                Unavailable(
                    "内置代理没有运行" + (mode.reason?.takeIf { it.isNotBlank() }?.let { "：$it" } ?: "") +
                        "。按约定走代理的平台先不发请求，不会退回直连；" +
                        "请到「设置 → 进阶设置 → 内置代理核心」里看一下。",
                )
            }
        }

        /**
         * `System` 那一档：问 JVM 的 [ProxySelector]，把答案折成具体的一档。
         *
         * 口径照 `core/Proxy.kt` 的 `PlatformProxySelector.select`（OkHttp 那条路）：
         * - 选择器是 null（`ProxySelector.getDefault()` 为 null，很少见）→ 直连（它就是 `NoProxySelector`）；
         * - 返回空表（= 没有代理）→ 直连（OkHttp 需要一个明确的答案，`PlatformProxySelector` 也是这么办的）；
         * - 取**第一个**：`HTTP` → [Http]、`SOCKS` → [Socks5]、`DIRECT` → [Direct]。
         *
         * 问的 URI 是 `https://<主域名>/`（[hosts] 的第一个）——和 OkHttp 那边按请求的 URL 问选择器
         * 是同一口径（同一个平台的主域名）。**解析不出来时给 [Unavailable]，绝不给直连**：
         * 境外平台在"跟随全局 + 系统代理"这一档上直连违反红线。
         */
        private fun resolveSystem(hosts: List<String>, system: ProxySelector?): ProxyTarget {
            val host = hosts.firstOrNull()?.trim()?.trimStart('.')?.takeIf { it.isNotEmpty() }
                ?: return Unavailable(
                    "「系统代理」这一档要用平台的主域名去问系统代理设置，但这个平台没有声明域名（插件描述的 hosts 是空的）：" +
                        "按约定先不发请求，不会退回直连。",
                )
            val selector = system ?: return Direct
            val picked = runCatching { selector.select(URI("https://$host/")) }
                .getOrElse { e ->
                    return Unavailable(
                        "问系统代理设置（ProxySelector）时出错（${e.javaClass.simpleName}）：" +
                            "按约定先不发请求，不会退回直连。请检查系统 / JVM 的代理设置。",
                    )
                }
            val proxy = picked.firstOrNull() ?: return Direct
            val address = proxy.address() as? InetSocketAddress
            return when (proxy.type()) {
                Proxy.Type.DIRECT -> Direct
                Proxy.Type.HTTP, Proxy.Type.SOCKS -> {
                    val targetHost = (address?.hostString ?: address?.address?.hostAddress)
                    val port = address?.port ?: 0
                    if (targetHost.isNullOrBlank() || port !in 1..65535) {
                        Unavailable(
                            "系统代理给的地址看不懂（缺少主机名或端口不在 1..65535）：" +
                                "按约定先不发请求，不会退回直连。请检查系统 / JVM 的代理设置。",
                        )
                    } else if (proxy.type() == Proxy.Type.HTTP) {
                        Http(targetHost, port)
                    } else {
                        // java.net 的 SOCKS 在本地解析域名 = wreq 的 `socks5://`（和 OkHttp 一样）
                        Socks5(targetHost, port)
                    }
                }
            }
        }
    }
}

/**
 * **[HostHttp] 的 Rust 联网层实现**（替代 [RustHttpHost] 那条 OkHttp 路）。
 *
 * 形状和 [RustHttpHost] 一模一样（都是 `HostHttp`），所以 11 家平台门面一行不用改。
 * 区别只有一个：请求真的由 `feedhub-net` 发出去（wreq + btls + vendored http2），不再经 OkHttp。
 *
 * **顺序保证在 Rust 里**（`feedhub-ffi/src/net_host.rs` 的模块头有那张表）：
 * `onRequest` 计数 → 代理守卫（fail-closed）→ 默认头 → 限速 → txid → 连接。
 */
class RustNetHttpHost internal constructor(
    private val handle: FfiNetHostHandle,
) : HostHttp {
    override fun execute(request: FfiHttpRequest): FfiHttpResponse {
        // 协程被取消时把在飞的请求掐掉（和 `RustHttpHost` 里 `Call.cancel()` 同一个时机、
        // 同一个线程局部作用域）。Rust 的调用是同步阻塞的，JNA 那层打断不了，
        // 只能从外面把 `CancelToken` 置上，让 `NetClient` 的 `select!` 立刻返回 `Cancelled`。
        val scope = HostCallScope.current
        val cancel: () -> Unit = { handle.cancel() }
        scope?.registerNet(cancel)
        try {
            return handle.execute(request)
        } catch (e: FfiHostException.ProxyUnavailable) {
            // fail-closed：一个字节都没发。必须和普通网络错误分开（cli 的退出码 6 靠它）
            throw ProxyUnavailableException(e.detail)
        } catch (e: FfiHostException.Network) {
            throw IOException(e.detail)
        } catch (e: FfiHostException.Other) {
            throw IOException(e.detail)
        } finally {
            scope?.unregisterNet(cancel)
        }
    }

    /** 取消这个 host 上正在飞的那个请求。 */
    fun cancel() = handle.cancel()

    /** 上一次调用在限速队列里等了多久（毫秒；性能记录的 `queue_ms`）。 */
    fun lastWaitMs(): Long = handle.lastWaitMs()
}

/**
 * **[HostCookies] 的 Rust 联网层实现**（替代 [RustCookieHost] 那条"回调 `ctx.cookies`"的路）。
 *
 * `find` / `put` 直接落到 Rust 的罐子上（和 [RustCookieHost] 语义相同，只是少一趟 FFI 往返）。
 */
class RustNetCookies internal constructor(
    private val handle: FfiNetCookies,
) : HostCookies {
    override fun find(url: String, name: String): String? = handle.find(url, name)

    override fun put(url: String, cookies: List<uniffi.feedhub_ffi.FfiCookie>) {
        if (cookies.isEmpty()) return
        handle.put(url, cookies)
    }
}
