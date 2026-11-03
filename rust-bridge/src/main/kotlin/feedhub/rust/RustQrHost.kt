package feedhub.rust

import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiHttpRequest
import uniffi.feedhub_ffi.FfiHttpResponse
import uniffi.feedhub_ffi.FfiNetQrHost
import uniffi.feedhub_ffi.FfiQrCookie
import uniffi.feedhub_ffi.HostQrDecoder
import uniffi.feedhub_ffi.HostQrLogin

/**
 * **扫码登录的宿主能力**：临时 Cookie 罐 / 收发 / 落地**都在 Rust**。
 *
 * 搬之前（`RustQrHost`）这里做四件事：临时罐（`MemoryCookieJar`）、用
 * `client.newBuilder().cookieJar(临时罐)` 发请求、落地（先删再写）、二维码图解码（zxing）。
 * 现在只剩**最后一件**：
 *
 * | 这件事 | 现在在哪儿 | 为什么 |
 * | 临时罐（和 `ctx.cookies` 隔离） | Rust（`CookieJar::in_memory`，
 * 跟着罐子走 |
 * | **二维码图解码** | **这里**（[decode] → zxing） | zxing 只有 Java 版，`.so` 里没有它 |
 *
 * 口径**一个字没变**：临时罐挂在"发请求的那个客户端"上这件事，等价于"逐请求把临时罐交给
 * `NetRequest::cookies`"——代理 / fail-closed / 连接池 / 限速（和接口共用一个排队器）全照旧。
 * 落地还是先 `remove` 再 `put`，属性（`secure` / `httpOnly` / `hostOnly`）逐条照搬。
 *
 * @param net 平台那份 [RustNetStack]（扫码和接口**共用同一个 `NetClient`**，所以代理档也同一份）。
 * @param spec 和接口那个 host **同一份** `HostSpec`（默认头 / 最小间隔 / 读超时 / 跟不跟重定向）。
 * @param qrImage 二维码图解码（zxing）；`null` = 这个平台用不到（B站 / 知乎 / 抖音 / 小黑盒 / 小红书）。
 */
class RustQrHost(
    net: RustNetStack,
    spec: uniffi.feedhub_ffi.FfiHostSpec,
    qrImage: ((ByteArray) -> String?)? = null,
) : HostQrLogin {
    private val native: FfiNetQrHost by HostedCall.native {
        RustNative.ensureLoaded()
        net.inner.qrHost(spec, qrImage?.let { decode -> HostQrDecoderAdapter(decode) })
    }

    /** 取消在飞的那一跳（`HostedCall.blocking(cancelInFlight = …)` 用）。 */
    fun cancel() {
        native.cancel()
    }

    /**
     * `ctx.cookies.all()`：知乎 / 抖音的扫码要从本机设备 Cookie 起步（换掉登录凭据）。
     * 读的是**正式罐**（Rust 那份），不是临时罐。
     */
    override fun all(): List<FfiQrCookie> = native.all()

    override fun begin(): String = native.begin()

    /**
     * 用临时罐发一个请求。
     *
     * **`FfiHostException` 让它原样往上抛**（不在这里翻成 `ProxyUnavailableException` / `IOException`）：
     * 这个方法是**给 Rust 回调的**（`HostQrLogin` 是 `with_foreign` 的 trait），UniFFI 的 Kotlin 那一侧
     * 只认得**它自己声明的错误类型**——把一个别的异常扔回去会变成 `UnexpectedUniFFICallbackError`，
     * 平台层就看不到 `proxyUnavailable` 了（`BiliAccountEquivalenceTest` 的 fail-closed 门当场抓的）。
     * 翻成 Kotlin 异常是**外面那一层**的事：`HostedCall.guard` 把 `FfiException` 折回现有异常；
     * 这一层的 `FfiHostException` 由 Rust 的 `QrAdapter` 接住、再经 `map_error` 变成 `FfiError`。
     */
    override fun execute(
        session: String,
        request: FfiHttpRequest,
        timeoutMs: Long?,
    ): FfiHttpResponse = native.execute(session, request, timeoutMs)

    override fun cookies(session: String): List<FfiQrCookie> = native.cookies(session)

    override fun find(session: String, url: String, name: String): String? = native.find(session, url, name)

    override fun put(session: String, url: String, cookies: List<FfiQrCookie>) = native.put(session, url, cookies)

    override fun end(session: String) = native.end(session)

    override fun commit(
        url: String,
        removeDomain: String,
        removeNames: List<String>,
        cookies: List<FfiQrCookie>,
    ) = native.commit(url, removeDomain, removeNames, cookies)

    /** 二维码图解码：**唯一还留在宿主的那件事**（zxing）。 */
    override fun decodeQrImage(png: ByteArray): String? = native.decodeQrImage(png)
}

/**
 * 二维码解码器：把 Kotlin 的 `(ByteArray) -> String?` 包成 UniFFI 的 [HostQrDecoder]。
 *
 * 为什么只有这一个方法过边界：`net_host::NetQrHost` 里其余七件事（临时罐 / 收发 / 落地）都在 Rust 了，
 * 只有"PNG → 文本"这件事没搬（zxing 只有 Java 版，`QrImage` 自己解 PNG 不依赖 `javax.imageio`，安卓能用）。
 */
internal class HostQrDecoderAdapter(
    private val decode: (ByteArray) -> String?,
) : HostQrDecoder {
    override fun decodeQrImage(png: ByteArray): String? = decode(png)
}

/**
 * 为一家平台建扫码宿主（各家的 `RustXxxHosted` 都调它）。
 *
 * [spec] 通常就是接口那个 `net.spec(...)` 的**同一份参数**——扫码请求和接口请求走同一个口径
 * （默认头 / 限速 / 读超时 / 重定向），区别只有"罐子换成临时的"（在 Rust 里逐请求给）。
 */
internal fun qrHostOf(
    net: RustNetStack,
    platform: Platform,
    defaultHeaders: Map<String, String> = emptyMap(),
    defaultHeaderHosts: List<String> = emptyList(),
    minIntervalMs: Long = 0,
    jitterMs: Long = 0,
    backgroundGraceMs: Long = 0,
    readTimeoutMs: Long = 0,
    followRedirects: Boolean = true,
    qrImage: ((ByteArray) -> String?)? = null,
): RustQrHost = RustQrHost(
    net = net,
    spec = net.spec(
        platform = platform,
        defaultHeaders = defaultHeaders,
        defaultHeaderHosts = defaultHeaderHosts,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        backgroundGraceMs = backgroundGraceMs,
        readTimeoutMs = readTimeoutMs,
        followRedirects = followRedirects,
    ),
    qrImage = qrImage,
)
