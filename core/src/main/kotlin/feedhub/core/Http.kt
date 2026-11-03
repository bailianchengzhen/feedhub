package feedhub.core

import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.FormBody
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** 已经读完 body 的响应。body 在 OkHttp 的回调线程里读完，调用方拿到时不会再阻塞。 */
data class HttpResult(
    val code: Int,
    val url: HttpUrl,
    val headers: List<Pair<String, String>>,
    val body: String,
)

/** 发请求并读完 body。不检查 HTTP 状态码，交给调用方判断。 */
suspend fun OkHttpClient.fetch(request: Request): HttpResult = newCall(request).await()

suspend fun Call.await(): HttpResult {
    // 「这次调用发起了几个请求」的记账点（异步发送；见 [RequestScope]）。不在接口调用里就什么都不做。
    recordHttpCall()
    return suspendCancellableCoroutine { cont ->
        cont.invokeOnCancellation { cancel() }
        enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (cont.isActive) cont.resumeWithException(e)
            }

            override fun onResponse(call: Call, response: Response) {
                val result = try {
                    response.use {
                        HttpResult(
                            code = it.code,
                            url = it.request.url,
                            headers = it.headers.map { (k, v) -> k to v },
                            body = it.body?.string().orEmpty(),
                        )
                    }
                } catch (e: IOException) {
                    if (cont.isActive) cont.resumeWithException(e)
                    return
                }
                cont.resume(result)
            }
        })
    }
}

fun HttpResult.toRawResponse() = RawResponse(
    code = code,
    url = url.toString(),
    headers = headers.filterNot { it.first.equals("set-cookie", ignoreCase = true) },
    body = body,
)

/**
 * 把 [RawRequest] 变成 OkHttp 请求（不含签名）。各 Source 的 `raw()` 先调它，再按需改写 URL 或 body 加签名。
 */
fun RawRequest.toOkHttpRequest(apiBase: String): Request {
    val base = if (url.startsWith("/")) (apiBase.trimEnd('/') + url) else url
    val httpUrl = base.toHttpUrl().newBuilder().apply {
        query.forEach { (k, v) -> addQueryParameter(k, v) }
    }.build()
    val body = when {
        json != null -> json.toRequestBody("application/json; charset=utf-8".toMediaType())
        form.isNotEmpty() -> FormBody.Builder().apply { form.forEach { (k, v) -> add(k, v) } }.build()
        method.equals("POST", ignoreCase = true) -> FormBody.Builder().build()
        else -> null
    }
    return Request.Builder()
        .url(httpUrl)
        .method(method.uppercase(), body)
        .apply { headers.forEach { (k, v) -> header(k, v) } }
        .build()
}

/** 限速器（`MinIntervalInterceptor`）**搬到了 `:rust-bridge`**。
 *
 * 为什么不在 `:core` 了：排队决策在 Rust（`feedhub-core/src/ratelimit.rs`），而 `:core` 在依赖顺序上
 * 在 `:rust-bridge` 下面，调不到 Rust。做法和 `feedhub.core.sync` / `feedhub.brain` 的先例一样：
 * 类跟着门面住到 `:rust-bridge`，**包名和类名不变**（`feedhub.core.MinIntervalInterceptor`），
 * 各平台 `import feedhub.core.MinIntervalInterceptor` 一行不用改。
 */

/** 给指定域名的请求补上默认请求头（请求里已有的同名头不覆盖）。 */
class DefaultHeadersInterceptor(
    private val headers: Map<String, String>,
    private val hostFilter: (String) -> Boolean = { true },
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        if (!hostFilter(request.url.host)) return chain.proceed(request)
        val builder = request.newBuilder()
        headers.forEach { (k, v) -> if (request.header(k) == null) builder.header(k, v) }
        return chain.proceed(builder.build())
    }
}

object UserAgents {
    /** 电脑版 Chrome。网页接口和详情 WebView 都用它，保持一致。 */
    const val DESKTOP_CHROME =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) " +
            "Chrome/141.0.0.0 Safari/537.36"
}
