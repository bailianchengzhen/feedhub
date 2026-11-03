package feedhub.core

import okhttp3.Cookie
import okhttp3.CookieJar
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl

/**
 * 所有平台共用一个 Cookie 罐，靠域名区分（和浏览器一样）。
 * App 里用 Android CookieManager 实现，让 WebView 和原生请求共用一份；cli 和测试用 [FileCookieJar]。
 */
interface FeedCookieJar : CookieJar {
    /** 发往 [url] 的请求会带上的、名为 [name] 的 Cookie 值。 */
    fun find(url: HttpUrl, name: String): String? =
        loadForRequest(url).firstOrNull { it.name == name }?.value

    fun find(url: String, name: String): String? = find(url.toHttpUrl(), name)

    /** 直接写入（例如 B站的 buvid3 是从接口拿到值后自己种的）。 */
    fun put(url: HttpUrl, cookies: List<Cookie>) = saveFromResponse(url, cookies)

    /** 删掉某个域（含子域）下的全部 Cookie。 */
    fun clearDomain(domain: String)

    /**
     * 删掉某个域（含子域）下名字在 [names] 里的 Cookie（不管路径），其他 Cookie 不动。
     * 默认实现用"写一个已过期的同名 Cookie"来删（各实现的 [saveFromResponse] 都按过期时间处理），实现方有更直接的办法可以覆盖。
     * 扫码登录成功后换账号用：只清登录凭据，保留 buvid3 / d_c0 这类设备标识。
     */
    fun remove(domain: String, names: Set<String>) {
        val d = domain.trimStart('.')
        for (c in all()) {
            if (c.name !in names || !(c.domain == d || c.domain.endsWith(".$d"))) continue
            val expired = Cookie.Builder().name(c.name).value("").domain(c.domain).path(c.path).expiresAt(0).build()
            saveFromResponse("https://${c.domain}/".toHttpUrl(), listOf(expired))
        }
    }

    /** 全部 Cookie，cli 的 `cookie list` 用。 */
    fun all(): List<Cookie>
}

/**
 * **能给出 Rust 罐子句柄的罐子**。
 *
 * 为什么需要它：Rust 的联网层（`feedhub-net`）要把 `Set-Cookie` 写进**同一个罐子**、也要在发请求前
 * 从同一个罐子取 Cookie——不能有第二份，否则登录态立刻分叉。而罐子本体已经整个在 Rust 里
 * （`feedhub-core/src/cookies.rs`）：
 * - [RustCookieJar][feedhub.cookies.RustCookieJar] 直接持有那个句柄；
 * - [MemoryCookieJar] / [PersistentCookieJar] 是**转发壳**（`CookieJarFactory` 注册的实现），
 *   它们后面那个也是 Rust 罐子。
 *
 * 所以只需要一个"把后面的句柄交出来"的口子。**`:core` 不认识 Rust 的类型**（依赖方向倒不过来），
 * 所以这里用 `Any`：`RustNetStack` 拿到之后自己 cast 成 UniFFI 的 `CookieJar`。
 * `SimpleMemoryCookieJar`（纯 `:core` 单测里那个）没有句柄 → 返回 `null`，Rust 联网层会明确报错
 * （**不悄悄退回 OkHttp**，理由见 `RustNetStack.create`）。
 */
interface RustJarHandle {
    /** 后面那个 Rust 罐子的句柄（`uniffi.feedhub_ffi.CookieJar`）；没有就 `null`。 */
    fun rustJarHandle(): Any?
}

/**
 * 测试 / 宿主里那种**转发壳**（`SpyJar` / `CountingJar` 之类）：把句柄问到底下那个罐子。
 *
 * 为什么要这个口子：`source-*` 的账号 / 扫码等价测试都用"包一层数读写次数"的罐子，
 * 而 Rust 联网层要求"罐子得是 Rust 的那个"（不然登录态会分叉）。这些壳本身不是 Rust 罐子，
 * 但它们**底下**是。所以让壳实现这个接口、把句柄转出去——测试一行不用改行为，只是多一个 `override`。
 *
 * 生产里没有壳套壳：`RustCookieJar` / `MemoryCookieJar` / `PersistentCookieJar` 直接实现
 * [RustJarHandle]，壳只有测试在用。
 */
interface ForwardingRustJar : RustJarHandle {
    /** 底下的那个罐子（壳转发给它的那一个）。 */
    val innerJar: FeedCookieJar

    override fun rustJarHandle(): Any? = (innerJar as? RustJarHandle)?.rustJarHandle()
}

/**
 * 内存实现（单元测试 / 演示 / 截图用）。
 *
 * 只是落盘端口什么都不存）。
 * 这样"内存罐子"和"真罐子"的域名 / 路径匹配、过期、去重、`clearDomain` / `remove` 的语义**只有一份**，
 * 测试里假出来的东西不会和生产漂（以前这里就有一份独立的 `matches` 调用，很容易和 `PersistentCookieJar`
 * 的合并语义对不上）。
 *
 * 为什么 `:core` 能调 `:rust-bridge`：`MemoryCookieJar` 是唯一一个**主源码集里需要罐子逻辑**的地方
 * （`SourceContext.inMemory()` / `DemoWorld`），而 `:core` 在依赖顺序上在 `:rust-bridge` 下面
 * （`rust-bridge: api(project(":core"))`）。所以这里不直接引用 Rust 门面，而是走一个**由
 * `:rust-bridge` 在启动时注册进来的工厂**（[CookieJarFactory]）——`:core` 编译期不认识 Rust，
 * 只在"有人注册过"的时候才需要它；没注册时（例如纯 `:core` 的单元测试直接 new 一个）
 * 退回到一个**只用在测试里**的极简内存实现（`SimpleMemoryCookieJar`，行为和 Rust 那份一致）。
 */
class MemoryCookieJar : FeedCookieJar, RustJarHandle {
    private val delegate: FeedCookieJar = CookieJarFactory.inMemory() ?: SimpleMemoryCookieJar()

    /** 后面那个 Rust 罐子的句柄（`SimpleMemoryCookieJar` 时是 null，见 [RustJarHandle]）。 */
    override fun rustJarHandle(): Any? = (delegate as? RustJarHandle)?.rustJarHandle()

    override fun loadForRequest(url: HttpUrl): List<Cookie> = delegate.loadForRequest(url)

    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) = delegate.saveFromResponse(url, cookies)

    override fun clearDomain(domain: String) = delegate.clearDomain(domain)

    override fun remove(domain: String, names: Set<String>) = delegate.remove(domain, names)

    override fun all(): List<Cookie> = delegate.all()
}

/**
 * `:rust-bridge` 往 `:core` 注册"内存罐子怎么做"的钩子（**只有这一个方向**：`:core` 不认识 Rust，
 * Rust 的门面在自己的 `init` 里注册）。
 *
 * 为什么要有它：`MemoryCookieJar` 在 `:core` 的主源码集里（`SourceContext.inMemory()`），
 * 而罐子逻辑在 Rust。不这样的话要么把 `MemoryCookieJar` 也搬到 `:rust-bridge`（几百处 `import` 要改、
 * 而且 `:core` 的测试会失去内存罐子），要么在 `:core` 里留第二份匹配逻辑。
 * 注册是幂等的，`feedhub.cookies.RustCookieJar` 的 `init` 调一次。
 */
object CookieJarFactory {
    @Volatile
    private var factory: (() -> FeedCookieJar)? = null

    @Volatile
    private var opener: ((java.io.File) -> FeedCookieJar)? = null

    /** `:rust-bridge` 在加载时注册（重复注册覆盖，行为一样）。 */
    fun register(create: () -> FeedCookieJar) {
        factory = create
    }

    /**
     * `:rust-bridge` 注册"从**这个文件**打开"（[PersistentCookieJar] 的兼容入口用它）。
     *
     * 宿主这边只剩"文件在哪"这一件事（见 `CookieStore.kt` 的文件头）。
     */
    fun registerFile(open: (java.io.File) -> FeedCookieJar) {
        opener = open
    }

    /** 现在能不能造 Rust 版的内存罐子（`:rust-bridge` 注册过没有）。 */
    val registered: Boolean get() = factory != null

    internal fun inMemory(): FeedCookieJar? = resolve()?.invoke()

    internal fun openOrNull(file: java.io.File): FeedCookieJar? {
        resolve() ?: return null
        return opener?.invoke(file)
    }

    /**
     * 拿工厂；**第一次没注册时用反射把 `:rust-bridge` 的注册装上**。
     *
     * 为什么要反射：`:core` 编译期看不见 `:rust-bridge`（依赖方向反着），而第一个碰罐子的地方可能是
     * `SourceContext.inMemory()` 这种"还没人加载过任何 Rust 门面"的时刻。类名是常量、
     * `:rust-bridge` 在所有生产 / 测试配置的运行时类路径上；装不上（例如只跑 `:core` 的单元测试）
     * 就退回到测试用的极简实现，**不会**因此改变生产行为。
     */
    /**
     * 反射把 `:rust-bridge` 的注册装上（[resolve] 的说明）。**装不上要能看见原因**：
     * 静默失败的话，生产路径会悄悄跑到测试用的 [SimpleMemoryCookieJar] 上，"罐子逻辑只有一份"就破了。
     */
    private fun loadWiring() {
        try {
            // Kotlin `object` 的 `install` 是**实例方法**：先拿 INSTANCE，不能 `invoke(null)`
            val cls = Class.forName("feedhub.cookies.RustCookieWiring")
            val instance = cls.getField("INSTANCE").get(null)
            cls.getMethod("install").invoke(instance)
        } catch (e: ClassNotFoundException) {
            // 只跑 :core 自己的单元测试时走这里，没装是对的
        } catch (e: Throwable) {
            System.err.println("feedhub：Cookie 罐的 Rust 实现装配失败（退回测试用的内存实现）：$e")
        }
    }

    private fun resolve(): (() -> FeedCookieJar)? {
        factory?.let { return it }
        synchronized(this) {
            factory?.let { return it }
            loadWiring()
            return factory
        }
    }
}

/**
 * **只在测试里**用的极简内存罐子（`:rust-bridge` 没注册时的退路；`:core` 自己的单元测试会走到）。
 *
 * 它和 Rust 那份的口径**逐条对齐**（[PersistentCookieJar] 的合并语义在"不落盘"时退化成"整份就是内存"，
 * 也就是原来 `MemoryCookieJar` 的行为）：同名同域同路径先删、`expiresAt > now` 才留、
 * `loadForRequest` 过滤过期 + `Cookie.matches`、`clearDomain` 连子域一起清。
 *
 * 生产路径**永远走不到这里**（App / cli 的组装都会先加载 `:rust-bridge`），所以它的存在不会让
 * "罐子逻辑有两份"这件事回到主路径上。
 */
internal class SimpleMemoryCookieJar : FeedCookieJar {
    private val cookies = mutableListOf<Cookie>()

    @Synchronized
    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) {
        for (c in cookies) {
            this.cookies.removeAll { it.name == c.name && it.domain == c.domain && it.path == c.path }
            if (c.expiresAt > System.currentTimeMillis()) this.cookies += c
        }
    }

    @Synchronized
    override fun loadForRequest(url: HttpUrl): List<Cookie> {
        val now = System.currentTimeMillis()
        cookies.removeAll { it.expiresAt <= now }
        return cookies.filter { it.matches(url) }
    }

    @Synchronized
    override fun clearDomain(domain: String) {
        val d = domain.trimStart('.')
        cookies.removeAll { it.domain == d || it.domain.endsWith(".$d") }
    }

    @Synchronized
    override fun all(): List<Cookie> = cookies.toList()
}

/**
 * 把浏览器请求头里的 `Cookie: a=1; b=2` 解析成 Cookie 列表，统一记到 [domain]（如 ".bilibili.com"）。
 * 浏览器请求头里没有过期时间，这里给一年，真正失效以服务端为准。
 */
fun parseCookieHeader(header: String, domain: String): List<Cookie> {
    val d = domain.trimStart('.')
    val expires = System.currentTimeMillis() + 365L * 24 * 3600 * 1000
    return header.removePrefix("Cookie:").removePrefix("cookie:")
        .split(';')
        .mapNotNull { part ->
            val idx = part.indexOf('=')
            if (idx <= 0) return@mapNotNull null
            val name = part.substring(0, idx).trim()
            val value = part.substring(idx + 1).trim()
            if (name.isEmpty()) return@mapNotNull null
            Cookie.Builder().name(name).value(value).domain(d).path("/").expiresAt(expires).build()
        }
}
