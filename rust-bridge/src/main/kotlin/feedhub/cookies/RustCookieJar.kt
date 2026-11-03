package feedhub.cookies

import feedhub.core.FeedCookieJar
import feedhub.rust.RustNative
import okhttp3.Cookie
import okhttp3.HttpUrl
import uniffi.feedhub_ffi.CookieJar as FfiCookieJar
import uniffi.feedhub_ffi.FfiCookieEntry
import uniffi.feedhub_ffi.FfiCookieException
import uniffi.feedhub_ffi.FfiHttpUrl
import uniffi.feedhub_ffi.cookieJarOpenFile
import uniffi.feedhub_ffi.cookieJarOpenMemory
import uniffi.feedhub_ffi.cookieJsonDecode
import uniffi.feedhub_ffi.cookieJsonEncode
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong

/**
 * **Rust 的 Cookie 罐的 Kotlin 门面**。
 *
 * 罐子的**全部逻辑**在 Rust（`crates/feedhub-core/src/cookies.rs`）：域名 / 路径匹配、过期、hostOnly /
 * secure、去重、读改写合并、`cookies.json` 的编解码、读不懂留底。这个文件只有三件事：
 *
 * 1. OkHttp 的 `Cookie` ⇄ [`FfiCookieEntry`] 的搬运（[toFfi] / [toOkHttp]）；
 * 2. `HttpUrl` → [`FfiHttpUrl`]：**host 和 encodedPath 都用 OkHttp 规范化之后的**（小写 / punycode /
 *    IPv6 去括号 / 路径编码），重算一遍就等于改线上行为；
 * 3. 告诉 Rust **文件在哪**（[open] 收一个 `File`）。
 *
 * 现在 `cookies/file.rs` 自己用 `std::fs` 原子替换 + 600 + `fcntl` 跨进程锁，
 * 跨边界只有一个路径字符串——**"读文件 / 文件锁"不是留在宿主的理由**。
 *
 * `now_ms` 是注入的：生产里传 `System.currentTimeMillis()`，测试里固定（Rust 不读时钟）。
 *
 * **线程安全**：OkHttp 的连接池线程会同时调 `loadForRequest` / `saveFromResponse`；Rust 罐子内部一把
 * `Mutex`，这里不用再锁。
 */
class RustCookieJar private constructor(
    private val jar: FfiCookieJar,
    private val clock: () -> Long,
) : FeedCookieJar, feedhub.core.RustJarHandle {

    /**
     * 后面那个 Rust 罐子的句柄（**Rust 联网层要用同一个罐子**，见 `feedhub.core.RustJarHandle`）。
     *
     * 返回 `Any` 是因为 `:core` 不认识 UniFFI 的类型；`RustNetStack` 拿到之后自己 cast。
     */
    override fun rustJarHandle(): Any = jar


    /**
     * Rust 那一侧的句柄（`uniffi.feedhub_ffi.CookieJar`）。
     *
     * 只给"要一次调用做完一串罐子动作"的地方用（Chrome 导入的"先清域、再按域分组写"：
     * `cookieJarImportChrome` 收的就是这个句柄）。**不要**用它绕开 [FeedCookieJar] 的语义。
     */
    val ffi: FfiCookieJar get() = jar

    /** 排障 / 性能用：过边界取 Cookie 的次数和总耗时。 */
    val stats: JarStats = JarStats()

    /** 过边界的次数与耗时（`loadForRequest` 每次算一条）。 */
    class JarStats {
        private val loads = AtomicLong()
        private val saves = AtomicLong()
        private val totalNanos = AtomicLong()

        fun recordLoad(nanos: Long) {
            loads.incrementAndGet()
            totalNanos.addAndGet(nanos)
        }

        fun recordSave() {
            saves.incrementAndGet()
        }

        fun reset() {
            loads.set(0)
            saves.set(0)
            totalNanos.set(0)
        }

        val loadCount: Long get() = loads.get()
        val saveCount: Long get() = saves.get()

        /** 取 Cookie 的平均耗时（纳秒）；没取过是 0。 */
        val averageLoadNanos: Long
            get() = loadCount.let { if (it == 0L) 0L else totalNanos.get() / it }

        override fun toString(): String = "loads=$loadCount saves=$saveCount avg=${averageLoadNanos}ns"
    }

    override fun loadForRequest(url: HttpUrl): List<Cookie> {
        val started = System.nanoTime()
        val list = call { jar.loadForRequest(url.toFfi(), clock()) }
        stats.recordLoad(System.nanoTime() - started)
        return list.map(::toOkHttp)
    }

    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) {
        if (cookies.isEmpty()) return
        stats.recordSave()
        call { jar.saveFromResponse(url.toFfi(), cookies.map { it.toFfi() }, clock()) }
    }

    override fun clearDomain(domain: String) {
        call { jar.clearDomain(domain, clock()) }
    }

    override fun remove(domain: String, names: Set<String>) {
        if (names.isEmpty()) return
        call { jar.remove(domain, names.toList(), clock()) }
    }

    override fun all(): List<Cookie> = call { jar.all() }.map(::toOkHttp)

    /** 现在有几条（`all().size` 的便宜版）。 */
    fun size(): Int = call { jar.len().toInt() }

    /** 开启 / 调整写盘防抖窗口（毫秒；`<= 0` = 每次改动同步落盘）。 */
    fun setDebounceMs(debounceMs: Long) {
        jar.setDebounceMs(debounceMs)
    }

    /** 立刻把防抖窗口内的待落盘改动写回磁盘（没有脏数据时是空操作）。 */
    fun flush() {
        call { jar.flush(clock()) }
    }

    companion object {
        /**
         * 从这个**文件**打开（= Kotlin `PersistentCookieJar(file)` 的构造函数）。
         *
         * 读 / 写 / 留底 / 跨进程锁全在 Rust（`feedhub_core::cookies::FileStorage`）；这边只给路径。
         *
         * 存档读不懂又留不了底时抛 [IllegalStateException]——**和 Kotlin 那边同一句话、同一个时机**
         * （宁可 App 起不来，也不覆盖用户所有平台的登录态）。
         */
        fun open(file: java.io.File, clock: () -> Long = System::currentTimeMillis): RustCookieJar {
            RustCookieWiring.install()
            RustNative.ensureLoaded()
            val ffi = try {
                cookieJarOpenFile(file.absolutePath, clock())
            } catch (e: FfiCookieException.CannotQuarantine) {
                throw IllegalStateException(
                    "cookies.json 解析失败，也没法留底；为了不覆盖已有登录，不加载也不写回：${e.detail}",
                )
            }
            return RustCookieJar(ffi, clock)
        }

        /** 内存罐子（不落盘；测试 / 演示 / 截图用，等于 Kotlin `MemoryCookieJar()`）。 */
        fun inMemory(clock: () -> Long = System::currentTimeMillis): RustCookieJar {
            RustCookieWiring.install()
            RustNative.ensureLoaded()
            return RustCookieJar(cookieJarOpenMemory(clock()), clock)
        }

        /**
         * 错误出口：**写不进去还是 `IOException`**（和改动前 `CookieFile.write` 抛的那一类一样，
         * 调用方 / 测试的 `catch` 一行不用改）；"读不懂又留不了底"是 `IllegalStateException`
         * （和 Kotlin 的构造函数一致）。
         */
        internal inline fun <T> call(block: () -> T): T = try {
            block()
        } catch (e: FfiCookieException.CannotQuarantine) {
            throw IllegalStateException(
                "cookies.json 解析失败，也没法留底；为了不覆盖已有登录，不加载也不写回：${e.detail}",
                e,
            )
        } catch (e: FfiCookieException.Storage) {
            throw IOException("写 cookies.json 失败：${e.detail}", e)
        } catch (e: FfiCookieException) {
            throw IOException("Cookie 罐出错：${e.message}", e)
        }

        /** OkHttp `HttpUrl` → 边界上的那三样（**都用 OkHttp 规范化之后的**）。 */
        fun HttpUrl.toFfi(): FfiHttpUrl = FfiHttpUrl(host = host, encodedPath = encodedPath, isHttps = isHttps)

        fun Cookie.toFfi(): FfiCookieEntry = FfiCookieEntry(
            name = name,
            value = value,
            domain = domain,
            path = path,
            expiresAtMs = expiresAt,
            secure = secure,
            httpOnly = httpOnly,
            hostOnly = hostOnly,
        )

        fun toOkHttp(c: FfiCookieEntry): Cookie = Cookie.Builder()
            .name(c.name)
            .value(c.value)
            .apply { if (c.hostOnly) hostOnlyDomain(c.domain) else domain(c.domain) }
            .path(c.path)
            .expiresAt(c.expiresAtMs)
            .apply { if (c.secure) secure() }
            .apply { if (c.httpOnly) httpOnly() }
            .build()
    }
}

/**
 * 为底层 Rust Cookie 罐开启防抖写盘（默认 2 秒合并窗口；非 Rust 罐忽略）。
 */
fun enableCookieJarDebounce(jar: FeedCookieJar, debounceMs: Long = 2_000L) {
    val ffi = (jar as? feedhub.core.RustJarHandle)?.rustJarHandle() as? FfiCookieJar ?: return
    ffi.setDebounceMs(debounceMs)
}

/**
 * 将底层 Rust Cookie 罐的防抖待写改动立即刷盘（`onStop` / 退出时调用；无脏数据时是空操作）。
 */
fun flushCookieJar(jar: FeedCookieJar, nowMs: Long = System.currentTimeMillis()) {
    val ffi = (jar as? feedhub.core.RustJarHandle)?.rustJarHandle() as? FfiCookieJar ?: return
    runCatching { ffi.flush(nowMs) }
}

/**
 * **临时 Cookie 罐**（扫码登录用）：不落盘，成功之前一个字节都不碰真实罐子
 */
fun temporaryCookieJar(clock: () -> Long = System::currentTimeMillis): RustCookieJar =
    RustCookieJar.inMemory(clock)

/**
 * **往 `:core` 注册"罐子怎么做"**。
 *
 * `:core` 不能依赖 `:rust-bridge`（依赖方向是 `rust-bridge → core`），而 `MemoryCookieJar` /
 * `PersistentCookieJar` / `CookieJson` 这三个入口都在 `:core` 里、被几百处调用。所以反过来由这里注册：
 * `:core` 的 [feedhub.core.CookieJarFactory] / [feedhub.core.CookieJsonBridge] 拿到的是 lambda，
 * 编译期不认识 Rust；跑起来之后（`:rust-bridge` 一加载）**每个调用都走 Rust**。
 *
 * 注册在类初始化时做一次（[RustCookieJar] 的 `init` 是对象初始化，`:rust-bridge` 的其它门面
 * 引用 `RustCookieJar` 时就会触发）。重复注册只是覆盖同一个 lambda，没有副作用。
 */
object RustCookieWiring {
    init {
        // 类初始化时就装一次：反射那条路（`:core` 的 `CookieJarFactory` / `CookieJsonBridge`）只要
        // `Class.forName` 到这个类、拿到 INSTANCE，注册就已经生效了。
        install()
    }

    fun install() {
        feedhub.core.CookieJarFactory.register { RustCookieJar.inMemory() }
        feedhub.core.CookieJarFactory.registerFile { file -> RustCookieJar.open(file) }
        feedhub.core.CookieJsonBridge.register(
            encode = { cookies ->
                // `.so` 的加载**必须在真正调 uniffi 之前**（桌面要先解压到缓存目录再告诉 JNA，
                // 见 RustNative）：注册本身只是放一个 lambda，真正的编解码发生在后面。
                RustNative.ensureLoaded()
                cookieJsonEncode(cookies.map(RustCookieWiring::toFfiEntry))
            },
            decode = { text ->
                RustNative.ensureLoaded()
                cookieJsonDecode(text).map(RustCookieWiring::fromFfiEntry)
            },
        )
    }

    fun toFfiEntry(c: feedhub.core.StoredCookie) = FfiCookieEntry(
        name = c.name,
        value = c.value,
        domain = c.domain,
        path = c.path,
        expiresAtMs = c.expiresAt,
        secure = c.secure,
        httpOnly = c.httpOnly,
        hostOnly = c.hostOnly,
    )

    fun fromFfiEntry(c: FfiCookieEntry) = feedhub.core.StoredCookie(
        name = c.name,
        value = c.value,
        domain = c.domain,
        path = c.path,
        expiresAt = c.expiresAtMs,
        secure = c.secure,
        httpOnly = c.httpOnly,
        hostOnly = c.hostOnly,
    )
}


