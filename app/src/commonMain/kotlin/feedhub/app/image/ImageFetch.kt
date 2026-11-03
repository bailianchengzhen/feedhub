package feedhub.app.image

import coil3.network.NetworkClient
import coil3.network.NetworkHeaders
import coil3.network.NetworkRequest
import coil3.network.NetworkResponse
import coil3.network.NetworkResponseBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okio.Buffer
import java.io.IOException

/**
 * **取一张图**。
 *
 * 为什么中间要隔这一层，而不是让 Coil 直接拿 `OkHttpClient`：
 * 1. **产品路径上不再有 OkHttp**：真实的实现是 `feedhub.rust.RustImageFetch`
 *    （`feedhub-net`）；演示 / 截图 / 测试用 [OkHttpImageFetch]（它们的"网络"本来就是拦截器现画的，
 *    本来也不出机器，换来换去没有意义）。
 * 2. 代理是**按域名**选的（图属于哪个平台就用那个平台的代理），而且 [feedhub.core.ProxyMode.System]
 *    这一档要现问 JVM 的选择器——这些判定在 `AppGraph` 里（跟接口那条路同一份），不在 Coil 里。
 *
 * 口径和以前的 `OkHttpNetworkFetcherFactory(callFactory = { graph.imageHttp })` 一样：
 * **不带 Cookie**、带浏览器 UA（[DefaultHeadersImageFetch] 补）、按域名走那个平台的代理、
 * 没填全 / 核心没在跑时 fail-closed（抛 [feedhub.core.ProxyUnavailableException]）。
 */
interface ImageFetch {
    /** 取一张图。**不跟重定向**的语义和 OkHttp 一样是"跟"（`followRedirects` 默认 true）。 */
    suspend fun fetch(url: String, headers: List<Pair<String, String>>): ImageResult

    /**
     * 接上"按域名选代理"的判定（[choose] 就是 `AppGraph.imageProxyMode`）。
     *
     * 为什么是这一步而不是构造参数：代理判定在 `AppGraph` 里（要读设置），而 `ImageFetch` 是
     * 调用方先造好再传进来的。默认**什么都不做**：
     * - [feedhub.app.image.RustImageFetch] 本来就逐请求按域名算（它在构造时已经拿到判定），这里不用接；
     * - [OkHttpImageFetch] 要接——演示 / 截图 / 测试的那份客户端得和以前一样按域名走代理 + fail-closed。
     */
    fun withProxy(choose: (host: String) -> feedhub.core.ProxyMode): ImageFetch = this
}

/** 一次图片请求的结果：Coil 的 `NetworkResponse` 需要的那几样（状态码 / 头 / 字节）。 */
class ImageResult(
    val code: Int,
    val headers: List<Pair<String, String>>,
    val bytes: ByteArray,
    /** 这次请求是什么时候发的 / 响应头什么时候回来的（Coil 的缓存年龄用它，毫秒）。 */
    val requestMillis: Long = System.currentTimeMillis(),
    val responseMillis: Long = System.currentTimeMillis(),
) {
    /** 把结果的头折成 Coil 的那个类型（同名多次按原顺序保留）。 */
    fun toNetworkHeaders(): NetworkHeaders {
        val builder = NetworkHeaders.Builder()
        for ((name, value) in headers) builder.add(name, value)
        return builder.build()
    }
}

/**
 * 演示 / 截图 / 测试用的实现：老路（一份 OkHttp 客户端，通常是挂着个现画图的拦截器）。
 *
 * 它**不是产品路径**（产品是 `RustImageFetch`）：留着它是因为那些环境里的"网络"本来就是拦截器现造的，
 * 而且它们的断言（`ProxyWiringTest` / `YoutubeProxyFailClosedTest`）要看的就是"这个客户端会怎么发"。
 */
class OkHttpImageFetch(
    /** 那份客户端。测试会直接拿它发请求来断言代理接线，所以是公开的。 */
    val http: OkHttpClient,
) : ImageFetch {
    /**
     * 按域名选代理 + fail-closed，口径和以前 `AppGraph.imageClientFrom` 那个客户端一致
     * （`PlatformProxySelector` + `ProxyConfigGuardInterceptor`）。
     */
    override fun withProxy(choose: (host: String) -> feedhub.core.ProxyMode): ImageFetch = OkHttpImageFetch(
        http.newBuilder()
            // 这个平台的代理地址没填全时，图片请求也给一个说得清的错误
            .addInterceptor(feedhub.core.ProxyConfigGuardInterceptor(choose))
            .proxySelector(feedhub.core.PlatformProxySelector(choose))
            .build(),
    )

    override suspend fun fetch(url: String, headers: List<Pair<String, String>>): ImageResult {
        val builder = Request.Builder().url(url)
        for ((name, value) in headers) builder.addHeader(name, value)
        val requestMillis = System.currentTimeMillis()
        http.newCall(builder.build()).execute().use { resp ->
            val bytes = resp.body?.bytes() ?: ByteArray(0)
            return ImageResult(
                code = resp.code,
                headers = resp.headers.map { (k, v) -> k to v },
                bytes = bytes,
                requestMillis = requestMillis,
                responseMillis = System.currentTimeMillis(),
            )
        }
    }
}

/**
 * 补默认头（`DefaultHeadersInterceptor` 的口径：**请求里已经有同名头就不补**）。
 *
 * 图片这条路上只有一条：浏览器 UA。
 */
class DefaultHeadersImageFetch(
    private val delegate: ImageFetch,
    private val headers: Map<String, String>,
) : ImageFetch {
    override suspend fun fetch(url: String, headers: List<Pair<String, String>>): ImageResult {
        val given = headers.map { it.first.lowercase() }.toSet()
        val merged = headers + this.headers.entries
            .filter { (name, _) -> name.lowercase() !in given }
            .map { (name, value) -> name to value }
        return delegate.fetch(url, merged)
    }
}

/**
 * 把 [ImageFetch] 接成 Coil 的 `NetworkClient`（`coil3.network.NetworkFetcher.Factory` 要的就是它）。
 *
 * 为什么 [ImageFetch] 是 `() -> ImageFetch` 而不是一个实例：**代理设置变了整份换掉**（新的连接池 /
 * 新的代理快照，和 `AppGraph.resetProxyClients()` 一致），而 Coil 的单例是进程级、只能建一次
 * （`appImageLoader` 也是这么处理 `imageHttp` 的）。
 *
 * 失败**原样抛 `IOException`**（Coil 把它记成 ErrorResult）：网络错 / 超时 / 取消 / fail-closed 都是
 * `IOException` 家族，`ApiImageService` 靠"是不是 `IOException`"决定这次算不算一个请求。
 */
class CoilImageClient(
    private val fetch: () -> ImageFetch,
) : NetworkClient {
    override suspend fun <T> executeRequest(
        request: NetworkRequest,
        block: suspend (NetworkResponse) -> T,
    ): T {
        val headers = request.headers.asMap().flatMap { (name, values) -> values.map { name to it } }
        // `NetworkRequest.method` 目前只有 GET（Coil 的图片请求就是 GET）；POST 那种没有 body 的实现，
        // 真出现就直接报清楚，别悄悄发一个空 body 的 POST 出去。
        if (!request.method.equals("GET", ignoreCase = true)) {
            throw IOException("图片请求只支持 GET（收到 ${request.method}）")
        }
        val result = try {
            fetch().fetch(request.url, headers)
        } catch (e: IOException) {
            throw e
        } catch (e: Exception) {
            throw IOException("取图失败：${e.javaClass.simpleName}: ${e.message}", e)
        }
        val response = NetworkResponse(
            code = result.code,
            requestMillis = result.requestMillis,
            responseMillis = result.responseMillis,
            headers = result.toNetworkHeaders(),
            body = NetworkResponseBody(Buffer().write(result.bytes)),
            delegate = result,
        )
        return block(response)
    }
}
