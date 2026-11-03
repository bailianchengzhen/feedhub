package feedhub.rust

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.feedhub_ffi.FfiNetCancel
import uniffi.feedhub_ffi.FfiNetClient
import uniffi.feedhub_ffi.FfiNetException
import uniffi.feedhub_ffi.FfiNetProxy
import uniffi.feedhub_ffi.FfiNetRequest

/**
 * **一次性的图片 GET**：用 Rust 联网层 `feedhub-net` 发，**逐请求给代理档**。
 *
 * 为什么不直接让 `:app` 用 `FfiNetClient`：`.so` 的加载（[RustNative] 要先解压到缓存目录、把路径告诉 JNA）
 * 和 `FfiNetException` → `IOException` 的翻译都在这条边界上，`app` 只该看到"取到了 / 抛了什么"。
 *
 * **一份客户端服务所有域名**：代理逐请求给（`feedhub-net` 按代理档分连接池，所以连接照样复用），
 * 这是图片加载器要的形状——信息流里各平台的图混在一起，而代理是按域名选的。
 *
 * 不带 Cookie（这份客户端根本不挂罐子）、不占平台限速（`platform` 只用来记账）。
 */
class RustImageIo(
    /** 平台 id（只影响记账 / 连接池分桶，不影响行为）。 */
    private val platform: String = "image",
) {
    private val client: FfiNetClient by lazy {
        RustNative.ensureLoaded()
        // 默认直连：真正的档位逐请求给（`FfiNetRequest.proxy`）
        FfiNetClient(emptyList(), FfiNetProxy.Direct)
    }

    /**
     * 取一次。**取消时抛 [kotlinx.coroutines.CancellationException]**（这是挂起函数，取消照规矩往上抛）；
     * 网络 / fail-closed / 超时抛 [uniffi.feedhub_ffi.FfiNetException]——由调用方翻译成它那一层的错误。
     */
    suspend fun fetch(url: String, headers: List<Pair<String, String>>, proxy: ProxyTarget): RustImageResponse =
        withContext(Dispatchers.IO) {
            // **先加载 `.so`**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]）。
            // 不先加载就直接造 `FfiNetCancel` / `FfiNetRequest` 的话，`UniffiLib` 的初始化会走
            // `Native.load("feedhub_ffi")` 拿到 `UnsatisfiedLinkError`（`RustLoadOrderTest` 守着这条）。
            RustNative.ensureLoaded()
            val request = FfiNetRequest(
                platform = platform,
                method = "GET",
                url = url,
                headers = headers.map { (k, v) -> "$k: $v" },
                body = ByteArray(0),
                interactive = false,
                readTimeoutMs = 0u,
                proxy = proxy.toFfi(),
            )
            val requestMillis = System.currentTimeMillis()
            val cancel = FfiNetCancel()
            val resp = coroutineScope {
                // 取消时掐掉**这一张**（逐请求的令牌，不影响同时在飞的别的图）
                val watcher = launch {
                    try {
                        awaitCancellation()
                    } finally {
                        runCatching { cancel.cancel() }
                    }
                }
                try {
                    client.executeCancellable(request, cancel)
                } catch (e: FfiNetException) {
                    // 取消被 Rust 报成 `Cancelled`：那说明是**我们**掐的，按挂起函数的规矩往上抛取消
                    if (e is FfiNetException.Cancelled) throw kotlinx.coroutines.CancellationException("图片请求已取消")
                    throw e
                } finally {
                    watcher.cancel()
                }
            }
            RustImageResponse(
                code = resp.status.toInt(),
                headers = resp.headers.mapNotNull { line ->
                    val i = line.indexOf(':')
                    if (i <= 0) null else line.substring(0, i) to line.substring(i + 1).trim()
                },
                bytes = resp.body,
                requestMillis = requestMillis,
                responseMillis = System.currentTimeMillis(),
            )
        }
}

/** [RustImageIo.fetch] 的返回（和 `coil3.network.NetworkResponse` 要的那几样一一对应）。 */
class RustImageResponse(
    val code: Int,
    val headers: List<Pair<String, String>>,
    val bytes: ByteArray,
    val requestMillis: Long,
    val responseMillis: Long,
)
