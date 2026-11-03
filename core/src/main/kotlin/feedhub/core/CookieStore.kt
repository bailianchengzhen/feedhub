package feedhub.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import okhttp3.Cookie
import okhttp3.HttpUrl
import java.io.File

/**
 * **Cookie 罐的入口**。罐子本身、`cookies.json` 的落盘（原子替换 / 600 / 坏文件留底 / 跨进程文件锁）都在
 * Rust：`crates/feedhub-core/src/cookies.rs`（罐子）和 `cookies/file.rs`（落盘 + `cookies.json.lock`）。
 *
 * 这些都搬进 Rust 了：
 *
 * - 桌面 / cli：`~/.config/feedhub/cookies.json`，Rust 侧 `FileStorage`（`std::fs` 原子替换 + 600 +
 *   `<文件>.lock` 上的 `fcntl` 排他锁，和旧版 Kotlin 的 `FileChannel.lock()` 是同一套锁，
 *   老 cli 进程和新 App 能同时开着、互相不丢改动）；
 * - 安卓：`filesDir/cookies.json`，同一个 `FileStorage`（权限靠应用沙箱，`.lock` 也只在本进程里排队）。
 *
 * 这里留下的是**依赖方向的壳**：`:core` 在依赖顺序上在 `:rust-bridge` 下面（`rust-bridge: api(project(":core"))`），
 * 编译期看不见生成的 UniFFI 绑定；而 `MemoryCookieJar` / `PersistentCookieJar` / `CookieJson` 三个名字
 * 被几百处调用。所以 `:rust-bridge` 在加载时把"怎么做罐子 / 怎么编解码"注册进来
 * （[CookieJarFactory] / [CookieJsonBridge]），这里只是转发。
 */
object CookieJson {
    fun decode(text: String): List<Cookie> = CookieJsonBridge.decode(text).map { it.toCookie() }

    fun encode(cookies: List<Cookie>): String = CookieJsonBridge.encode(cookies.map(StoredCookie::of))
}

@Serializable
data class StoredCookie(
    val name: String,
    val value: String,
    val domain: String,
    val path: String = "/",
    val expiresAt: Long,
    val secure: Boolean = false,
    val httpOnly: Boolean = false,
    val hostOnly: Boolean = false,
) {
    fun toCookie(): Cookie = Cookie.Builder()
        .name(name).value(value)
        .apply { if (hostOnly) hostOnlyDomain(domain) else domain(domain) }
        .path(path).expiresAt(expiresAt)
        .apply { if (secure) secure() }
        .apply { if (httpOnly) httpOnly() }
        .build()

    companion object {
        fun of(c: Cookie) = StoredCookie(c.name, c.value, c.domain, c.path, c.expiresAt, c.secure, c.httpOnly, c.hostOnly)
    }
}

/**
 * 存在一份 JSON 文本里的 Cookie 罐。
 *
 * 罐子的**全部逻辑**在 Rust（`crates/feedhub-core/src/cookies.rs`）：域名 / 路径匹配、过期、
 * hostOnly / secure、去重键、写回时"读磁盘 → 合并 → 写磁盘"、读不懂留底、`cookies.json` 的编解码、
 * 落盘（原子替换 + 600 + 跨进程锁）。这个类只把"文件在哪"交给 Rust。
 *
 * - 新代码优先用 `feedhub.cookies.RustCookieJar.open(file)` / `.inMemory()`（在 `:rust-bridge`）；
 * - 这个类和 [feedhub.core.local.FileCookieJar] 是**老调用的兼容入口**——转发到同一个 Rust 实现
 *   （`CookieJarFactory` 由 `:rust-bridge` 注册），行为（合并、留底、600 权限、跨进程锁、
 *   `cookies.json` 的字节）**一样**，没有第二份实现。
 *
 * @see feedhub.cookies.RustCookieJar
 */
class PersistentCookieJar(file: File) : FeedCookieJar, RustJarHandle {
    private val delegate: FeedCookieJar = CookieJarFactory.open(file)

    override fun loadForRequest(url: HttpUrl): List<Cookie> = delegate.loadForRequest(url)

    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) = delegate.saveFromResponse(url, cookies)

    override fun clearDomain(domain: String) = delegate.clearDomain(domain)

    override fun remove(domain: String, names: Set<String>) = delegate.remove(domain, names)

    override fun all(): List<Cookie> = delegate.all()

    /** 后面那个 Rust 罐子的句柄（见 [RustJarHandle]）。 */
    override fun rustJarHandle(): Any? = (delegate as? RustJarHandle)?.rustJarHandle()
}

/**
 * `cookies.json` 编解码的**桥**（实现注册在 `:rust-bridge`，见 [CookieJarFactory]）。
 *
 * 为什么要桥而不是直接调 Rust：`:core` 在依赖顺序上在 `:rust-bridge` 下面（`api(project(":core"))`），
 * 编译期看不见生成的 UniFFI 绑定。所以 `:rust-bridge` 在加载时把"怎么编解码 / 怎么造罐子"注册进来；
 * 没注册（纯 `:core` 的单元测试）时用 [SimpleCookieJson]——**只在测试里**，生产路径永远先加载 `:rust-bridge`。
 */
object CookieJsonBridge {
    @Volatile
    private var encoder: ((List<StoredCookie>) -> String)? = null

    @Volatile
    private var decoder: ((String) -> List<StoredCookie>)? = null

    fun register(encode: (List<StoredCookie>) -> String, decode: (String) -> List<StoredCookie>) {
        encoder = encode
        decoder = decode
    }

    val registered: Boolean get() = encoder != null && decoder != null

    fun encode(cookies: List<StoredCookie>): String = (encoder ?: install()?.let { encoder } ?: SimpleCookieJson::encode)(cookies)

    fun decode(text: String): List<StoredCookie> = (decoder ?: install()?.let { decoder } ?: SimpleCookieJson::decode)(text)

    /** 反射装一次（[CookieJarFactory] 里那段说明同样适用）。 */
    private fun install(): Unit? {
        if (registered) return Unit
        synchronized(this) {
            if (registered) return Unit
            try {
                // Kotlin `object` 的 `install` 是**实例方法**：先拿 INSTANCE，不能 `invoke(null)`
                val cls = Class.forName("feedhub.cookies.RustCookieWiring")
                val instance = cls.getField("INSTANCE").get(null)
                cls.getMethod("install").invoke(instance)
            } catch (e: ClassNotFoundException) {
                // 只跑 :core 自己的单元测试时走这里
            } catch (e: Throwable) {
                System.err.println("feedhub：cookies.json 编解码的 Rust 实现装配失败（退回测试用的实现）：$e")
            }
            return if (registered) Unit else null
        }
    }
}

/**
 * `:rust-bridge` 注册进来的"怎么从**一个文件**造罐子"（见 [CookieJarFactory] 的说明）；
 * 没注册时抛——那条路只有 `:core` 自己的测试会走到，而它们都用 `MemoryCookieJar`。
 */
private fun CookieJarFactory.open(file: File): FeedCookieJar =
    openOrNull(file) ?: error(
        "Cookie 罐的实现还没注册：:rust-bridge 的 feedhub.cookies.RustCookieJar 要在加载时调 CookieJarFactory.register(...)",
    )

/**
 * **只在 `:rust-bridge` 没注册时**用的 `cookies.json` 编解码（`:core` 自己的单元测试会走到）。
 *
 * 它**必须**和 Rust 那份逐字节一致（`core/src/test` 的 `CookieJsonTest` 和 `rust-bridge` 的
 * `CookieJsonByteTest` 各钉一遍：Kotlin 写 → Kotlin 读、Rust 写 → Rust 读、两边交叉读）。
 * 生产路径不会用它（`:rust-bridge` 一加载就注册），所以它不会变成"第二份实现"。
 */
object SimpleCookieJson {
    private val json = Json { prettyPrint = true; ignoreUnknownKeys = true }
    private val serializer = ListSerializer(StoredCookie.serializer())

    fun encode(cookies: List<StoredCookie>): String = json.encodeToString(serializer, cookies)

    fun decode(text: String): List<StoredCookie> = json.decodeFromString(serializer, text)
}
