package feedhub.rust

import feedhub.core.FeedCookieJar
import feedhub.core.KeyValueStore
import feedhub.core.ProxyUnavailableException
import feedhub.core.RequestPriority
import okhttp3.Call
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import uniffi.feedhub_ffi.FfiCookie
import uniffi.feedhub_ffi.FfiHeader
import uniffi.feedhub_ffi.FfiHostException
import uniffi.feedhub_ffi.FfiHttpRequest
import uniffi.feedhub_ffi.FfiHttpResponse
import uniffi.feedhub_ffi.HostCookies
import uniffi.feedhub_ffi.HostHttp
import uniffi.feedhub_ffi.HostRandom
import uniffi.feedhub_ffi.HostStore
import java.io.IOException

/**
 * **Rust 回调宿主的三件能力**（见 `rust/crates/feedhub-ffi/src/host.rs`）：发请求、读写 store、读写 Cookie 罐。
 *
 * 三件都在这儿、都只做"翻译"：真正的数据库、Cookie 罐、代理、限速、TLS 全在原来那套 Kotlin 代码里，
 * 所以平台看到的连接特征一个字节都不变。
 */

/**
 * **内部伪请求头**：Rust 平台对象用它表达"这次是用户正等着的请求"（`RequestPriority.INTERACTIVE`），
 * 和 `feedhub_core::hosted::PRIORITY_HEADER` **逐字相同**。
 *
 * 加字段会让那些字面量编不过。走请求头只动"要表达优先级的那一家"，`HostHttp` 的签名也一个字节没改
 * （理由写在 `rust/crates/feedhub-core/src/hosted.rs` 那个常量上）。
 *
 * **它绝不会出现在线上请求里**：[`RustHttpHost`] 读到时先记下来、再从头发里删掉，最后转成 OkHttp 的
 * `RequestPriority` tag。`XhsPriorityWireTest` / `XhsDetailRequestEquivalenceTest` 都逐字节断言了这一点。
 */
internal const val PRIORITY_HEADER: String = "X-Feedhub-Priority"

/** [`PRIORITY_HEADER`] 的取值（和 Rust 那边 `PRIORITY_INTERACTIVE` 一样）。 */
internal const val PRIORITY_INTERACTIVE: String = "interactive"

/**
 * `HostHttp` 的实现：**用传进来的那个客户端**发（必须是 `ctx.client(platform)` 派生出来的那个，
 * 上面挂着默认头 / 限速 / 代理选择 / Cookie 罐 / fail-closed 检查）。**绝不在这里新建客户端**。
 *
 * 同步执行（`execute()`）：调用方是 `Dispatchers.IO` 上的线程，Rust 同步回调进来，这里阻塞等 OkHttp。
 * `.so` 里因此不需要 tokio。
 */
class RustHttpHost(private val client: OkHttpClient) : HostHttp {
    override fun execute(request: FfiHttpRequest): FfiHttpResponse {
        // 协程被取消时把在飞的 Call 掐掉（和改动前 Kotlin 版的行为一样）。见 [HostCallScope]。
        val scope = HostCallScope.current
        val call = try {
            client.newCall(request.toOkHttpRequest())
        } catch (e: IllegalArgumentException) {
            // URL / 方法拼得宿主认不出（Rust 侧拼错了）：这不是网络错误，但也不能崩
            throw FfiHostException.Other("宿主不接受这个请求：${e.message}")
        }
        scope?.register(call)
        try {
            call.execute().use { response ->
                return FfiHttpResponse(
                    status = response.code.toUShort(),
                    headers = response.headers.map { (name, value) -> FfiHeader(name, value) },
                    body = response.body?.bytes() ?: ByteArray(0),
                    finalUrl = response.request.url.toString(),
                )
            }
        } catch (e: ProxyUnavailableException) {
            // fail-closed：一个字节都没发。必须和普通网络错误分开（cli 的退出码 6 靠它）
            throw FfiHostException.ProxyUnavailable(e.message ?: "代理不可用")
        } catch (e: IOException) {
            throw FfiHostException.Network(e.message ?: e.toString())
        } catch (e: RuntimeException) {
            throw FfiHostException.Other(e.toString())
        } finally {
            scope?.unregister(call)
        }
    }

    private fun FfiHttpRequest.toOkHttpRequest(): Request {
        val body = if (this.body.isEmpty()) null else this.body.toRequestBody(null)
        val builder = Request.Builder().url(url).method(method, body)
        // 有序头：用 addHeader（同名也按顺序保留），不能用 header()（它会先删同名再追加）。
        // 内部伪请求头 [PRIORITY_HEADER] 不写进请求：它只用来表达"这次是用户正等着的请求"，
        // 读到就记下来、丢掉，最后转成 OkHttp 的 tag（限速器靠它插队）。
        var interactive = false
        headers.forEach {
            if (it.name.equals(PRIORITY_HEADER, ignoreCase = true)) {
                if (it.value.equals(PRIORITY_INTERACTIVE, ignoreCase = true)) interactive = true
            } else {
                builder.addHeader(it.name, it.value)
            }
        }
        // 没有这个头时**一个 tag 都不打**，和加优先级之前逐字节一样（别的平台一个字节不受影响）。
        // 在 `backgroundRequestScope` 里（沉浸预取下一条）时不插队：Kotlin 原版在那里一律按后台排，这里照办。
        if (interactive && HostCallScope.current?.background != true) {
            builder.tag(RequestPriority::class.java, RequestPriority.INTERACTIVE)
        }
        return builder.build()
    }
}

/** `HostStore` 的实现：就是 `ctx.store`。键值格式归平台（Rust）管，这里不解释。 */
class RustStoreHost(private val store: KeyValueStore) : HostStore {
    override fun get(key: String): String? = store.get(key)

    override fun put(key: String, value: String?) = store.put(key, value)
}

/**
 * `HostRandom` 的实现：**从调用方给的随机源里按签名顺序抽**。
 *
 * 谁在用：抖音的平台对象。表情表搬进 Rust 之后，"这一趟要不要多签一个 `emoji/list`"只有 Rust
 * 解析完响应才知道，所以随机数改成"签的那一刻问宿主抽"——抓的个数和顺序天然和 Kotlin 的
 * `AbogusSigner` 逐位一致（参照实现的随机流从第一个请求起就对齐）。
 *
 * **不自己造随机源**：`draw` 直接落到传进来的那个 `nextDouble`（生产是 `Random` / `Math.random()`，
 * 测试是固定种子），和生产 / 对拍用的是同一个实例。
 */
class RustRandomHost(private val nextDouble: () -> Double) : HostRandom {
    override fun draw(count: UInt): List<Double> = List(count.toInt()) { nextDouble() }
}

/** `HostCookies` 的实现：就是 `ctx.cookies`。登录态的唯一真相还在宿主这边。 */
class RustCookieHost(private val jar: FeedCookieJar) : HostCookies {
    override fun find(url: String, name: String): String? = jar.find(url, name)

    override fun put(url: String, cookies: List<FfiCookie>) {
        if (cookies.isEmpty()) return
        val u = url.toHttpUrl()
        jar.put(
            u,
            cookies.map { c ->
                Cookie.Builder()
                    .name(c.name)
                    .value(c.value)
                    // **必须是 `domain()`（域 Cookie，hostOnly = false）**，不能写 `hostOnlyDomain()`：
                    // Kotlin 的 `ensureBuvid` 就是 `Cookie.Builder().domain("bilibili.com")`，
                    // 那样种下的 buvid3 对 `https://www.bilibili.com/` 才算"带得上"；
                    // 写成 host-only 的话 domain=bilibili.com 的 Cookie 在 www.bilibili.com 上**匹配不到**，
                    // 结果就是每次请求都去领一次 spi（`RustBiliSourceTest` 的"第二次不再领 spi"用例守着）。
                    .domain(c.domain)
                    .path(c.path)
                    .expiresAt(c.expiresAtMs)
                    .build()
            },
        )
    }
}

/**
 * **取消**：Rust 的调用是同步阻塞的，协程被取消时打断不了 JNA 那次调用；但我们能在取消的瞬间
 * 把**在飞的 OkHttp Call** 掐掉（`Call.cancel()`），Rust 那边就会拿到一个"连接被取消"的 IO 错误、
 * 立刻返回，然后 Kotlin 侧把结果丢掉、抛 `CancellationException`。
 *
 * 作用域放在**线程**上（`ThreadLocal`）：Rust 的宿主回调是在调用它的那个线程上同步回来的，
 * 所以回调里能拿到"这次调用属于哪个协程"。
 */
internal class HostCallScope(
    /** 这次 Rust 调用是不是在 `backgroundRequestScope` 里发起的（是就不让伪请求头插队）。 */
    val background: Boolean = false,
    /**
     * 这次调用所属的协程 Job（`null` = 不在协程里）。
     *
     * 为什么要有它：Rust 的宿主回调也可能**反过来调 Kotlin 的挂起函数**（小红书关注流的
     * `HostXhsSelfId`：没存过 `user_id` 时现发一次 `user/me`）。那种嵌套调用要挂在同一个 Job 下，
     * 否则外层取消时它不会跟着取消（`runBlocking(job)` 把它变成子协程）。
     */
    val job: kotlinx.coroutines.Job? = null,
) {
    private val lock = Any()
    private val calls = mutableListOf<Call>()
    /**
     * **Rust 联网层的在飞请求**：`feedhub-net` 发出去了、还没回来的那个。
     *
     * 和 [calls] 并列而不是合并：OkHttp 的 `Call.cancel()` 和 Rust 的 `CancelToken::cancel()`
     * 是两套机制（前者关 socket，后者让 `NetClient` 的 `select!` 立刻返回 `Cancelled`），
     * 但**它们都必须在同一个取消点上被触发**——一次调用要么走 OkHttp（还剩几个没搬的方法），
     * 要么走 Rust（现在的大多数），不会被两边同时用。
     */
    private val netCancels = mutableListOf<() -> Unit>()
    private var cancelled = false

    fun register(call: Call) {
        val cancelNow = synchronized(lock) {
            if (cancelled) true else { calls += call; false }
        }
        if (cancelNow) call.cancel()
    }

    fun unregister(call: Call) {
        synchronized(lock) { calls.remove(call) }
    }

    /** 登记一个 Rust 联网层的取消动作（`FfiNetHostHandle::cancel`）。 */
    fun registerNet(cancel: () -> Unit) {
        val cancelNow = synchronized(lock) {
            if (cancelled) true else { netCancels += cancel; false }
        }
        if (cancelNow) cancel()
    }

    fun unregisterNet(cancel: () -> Unit) {
        synchronized(lock) { netCancels.remove(cancel) }
    }

    fun cancelAll() {
        val (pendingCalls, pendingNet) = synchronized(lock) {
            cancelled = true
            val a = calls.toList().also { calls.clear() }
            val b = netCancels.toList().also { netCancels.clear() }
            a to b
        }
        pendingCalls.forEach { it.cancel() }
        pendingNet.forEach { it() }
    }

    companion object {
        private val threadLocal = ThreadLocal<HostCallScope?>()

        val current: HostCallScope? get() = threadLocal.get()

        fun <T> withScope(scope: HostCallScope, block: () -> T): T {
            val previous = threadLocal.get()
            threadLocal.set(scope)
            try {
                return block()
            } finally {
                threadLocal.set(previous)
            }
        }
    }
}
