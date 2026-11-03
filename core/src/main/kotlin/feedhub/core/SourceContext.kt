package feedhub.core

import okhttp3.ConnectionPool
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import java.net.ProxySelector
import java.util.concurrent.ConcurrentHashMap

/**
 * Source 运行需要的外部依赖。各 Source 在 [client] 的基础上派生自己的客户端
 * （加限速、默认请求头），这样连接池和 Cookie 罐都是共享的。
 */
class SourceContext(
    /** 已经挂好 [cookies] 的基础客户端。**它自己不套任何平台的代理**，各平台要用 [client]。 */
    val http: OkHttpClient,
    val cookies: FeedCookieJar,
    val store: KeyValueStore,
    /**
     * 按平台查代理（见 `Proxy.kt`）。默认 [ProxyProvider.SYSTEM]（跟随系统），
     * 也就是加"按平台指定代理"之前的行为。App 里由设置页实现的 provider 覆盖它。
     */
    val proxy: ProxyProvider = ProxyProvider.SYSTEM,
    /**
     * 平台在**读**接口上要人机验证时，由上层（App / cli）负责让用户完成。默认 null：
     * 没有处理方式时，Source 抛 [RiskControlException] 说清楚，不自动重试、不刷验证码。
     */
    val captcha: CaptchaHandler? = null,
    /**
     * 每从 [client] 派生的客户端发一个 HTTP 请求之前调一次（null = 不统计）。
     *
     * 目前只有**桌面 App 的对外接口**用它：`done` 行要报"这次调用真正发出去几个请求"。挂在 [client] 上而不是基础客户端上，是为了只数**平台接口**的请求
     * （图片 / 验证码的浏览器 / 代理核心的探测都不经这里）。cli / 测试不设它，行为一个字不变。
     */
    val onRequest: (() -> Unit)? = null,
) {
    /**
     * 每个平台一个 [ProxySelector]。同一个平台派生的客户端共用一份：OkHttp 把 ProxySelector 算进
     * "两个客户端算不算同一个地址"，共用才能共享连接池；不同平台之间因此不会串。
     */
    private val selectors = ConcurrentHashMap<Platform, ProxySelector>()

    /**
     * 派生某个平台自己的客户端：和 [http] 共用 Cookie 罐、连接池和线程池，只是套上这个平台的代理。
     *
     * **各 Source 用它替代 `http.newBuilder()`**：在这之后再用 `newBuilder()` 派生的小客户端
     * （网页接口客户端、限流 / 签名拦截器、扫码的临时 Cookie 罐）都会跟着走同一个代理。
     * 拦截器的相对顺序不受影响（这里只是换掉代理选择器、加一道配置检查，不动别的拦截器链）。
     *
     * [ProxyConfigGuardInterceptor] 排在所有拦截器最前面：代理地址没填全时直接抛出说得清的错误
     * ，别等到联网时才失败。
     *
     * [onRequest] 的计数拦截器（有的话）排在**最前**：被代理守卫拦下的那一次也算"试过发请求"。
     */
    fun client(platform: Platform): OkHttpClient.Builder {
        val builder = http.newBuilder()
        onRequest?.let { hook ->
            builder.addInterceptor(Interceptor { chain ->
                hook()
                chain.proceed(chain.request())
            })
        }
        return builder
            .addInterceptor(ProxyConfigGuardInterceptor { proxy.proxyFor(platform) })
            .proxySelector(
                selectors.computeIfAbsent(platform) { PlatformProxySelector({ proxy.proxyFor(platform) }) },
            )
    }

    /**
     * 换一份代理配置，[http] / [cookies] / [store] 不变。
     * App 里用它把"设置页里的代理"套到"本地文件组装出来的那一份"上（[SourceContext] 是先建好的，那时候还没有设置）。
     */
    fun withProxy(proxy: ProxyProvider, captchaHandler: CaptchaHandler? = captcha): SourceContext =
        SourceContext(http, cookies, store, proxy, captchaHandler, onRequest)

    /** 挂一个"每发一个请求调一次"的观察者（和已有的那个串起来），见 [onRequest]。 */
    fun withRequestObserver(observer: () -> Unit): SourceContext {
        val previous = onRequest
        val hook: () -> Unit = { previous?.invoke(); observer() }
        return SourceContext(http, cookies, store, proxy, captcha, hook)
    }

    /**
     * 在**基础客户端**上再挂一个应用拦截器（所有平台共用；各 Source 派生的客户端都会带上它——
     * `newBuilder()` 保留已有的拦截器，[client] 再往后面加）。
     *
     * 目前只有 cli 用：代理设置的 fail-closed 检查（点亮的平台但这次没有可用代理 → 发请求前直接报错，
     * 见 `cli/CliProxy.kt` 的 `CliProxyGuardInterceptor`）。**注意它排在 [client] 加的
     * [ProxyConfigGuardInterceptor] 之前**，所以"没代理"的报错先于"地址没填全"的报错。
     */
    fun withExtraInterceptor(interceptor: Interceptor): SourceContext =
        SourceContext(http.newBuilder().addInterceptor(interceptor).build(), cookies, store, proxy, captcha, onRequest)

    /**
     * 换一份基础客户端：**新的连接池**，Cookie / store / 代理配置不变。
     *
     * 代理设置变了用它：用 ProxySelector 时选出来的代理
     * **不进** `Address`（`Address.proxy` 是 null），连接池对"主机名相同"的旧连接照样判为可用，换代理后新请求
     * 会接着复用旧连接、走旧代理。换成新的连接池之后，旧连接留在旧池里（在飞的请求跑完自然回收），
     * 新请求一律走新池。
     */
    fun withFreshConnectionPool(): SourceContext =
        SourceContext(http.newBuilder().connectionPool(ConnectionPool()).build(), cookies, store, proxy, captcha, onRequest)

    companion object {
        /**
         * 单元测试用：内存 Cookie 和存储，**并且一律直连**。
         *
         * 为什么代理要给 [ProxyProvider.DIRECT] 而不是默认的 [ProxyProvider.SYSTEM]：`System` 这一档的
         * 语义是"问真的系统代理"（OkHttp 侧是 `PlatformProxySelector`，Rust 侧见 `ProxyTarget.of`），
         * 而测试是离线跑的（根 `build.gradle.kts` 的 `offlineGuard()` 把 `-DsocksProxyHost` 指到一个
         * 必然连不上的端口）。OkHttp 的代码每个请求现问选择器、回环地址（MockWebServer）还通，
         * **Rust 联网层是建栈时按平台主域名问一次**，就会去连那个死端口——那些测试想说的明明是
         * "这次请求别走代理"。真要验代理行为的测试自己用 [withProxy] 指定。
         */
        fun inMemory(): SourceContext {
            val jar = MemoryCookieJar()
            return SourceContext(
                OkHttpClient.Builder().cookieJar(jar).build(),
                jar,
                MemoryKeyValueStore(),
                ProxyProvider.DIRECT,
            )
        }
    }
}
