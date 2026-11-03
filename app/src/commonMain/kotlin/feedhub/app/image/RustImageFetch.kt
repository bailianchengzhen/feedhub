package feedhub.app.image

import feedhub.core.ProxyUnavailableException
import feedhub.rust.ProxyTarget
import feedhub.rust.RustImageIo
import uniffi.feedhub_ffi.FfiNetException
import java.io.IOException

/**
 * **产品路径的取图**：走 Rust 联网层 `feedhub-net`。
 *
 * 和以前那条 OkHttp 路逐条对齐（`AppGraph.imageClientFrom` + `OkHttpNetworkFetcherFactory`）：
 *
 * | 这条路上的事 | 以前在哪儿 | 现在在哪儿 |
 * | **不带 Cookie** | `CookieJar.NO_COOKIES` 的客户端 | 这份 `FfiNetClient` 根本不挂罐子（`NoCookies`） |
 * | **浏览器 UA** | `DefaultHeadersInterceptor` | [DefaultHeadersImageFetch]（同一个 `UserAgents.DESKTOP_CHROME`） |
 * | **按域名的代理** | `platformProxySelector` + `PlatformProxySelector.select` | [proxyFor]：每个请求按 `url` 的主机名算一档（见下） |
 * | **fail-closed** | `ProxyConfigGuardInterceptor` + 死代理 | `ProxyTarget.Unavailable` → `NetError::ProxyUnavailable`（**在 DNS / TCP 之前**） |
 * | 跟重定向 / gzip / 连接池 | OkHttp 默认 | `feedhub-net` 默认 |
 *
 * 代理为什么**逐请求**算：图片加载器是**一个客户端服务所有域名**的（信息流里各平台的图混在一起），
 * 而代理是**按域名**选的。`feedhub-net` 按代理档分连接池（`client_for`），所以逐请求给档位既保住了
 * 连接复用，又不用每个域名建一份栈。`ProxyMode.System` 这一档走 [ProxyTarget.of]——和平台对象那条路
 * **同一个函数**（问 JVM 的选择器、解析不出来就是 `Unavailable`，绝不悄悄直连）。
 *
 * @param proxyFor 按主机名给一档代理（`AppGraph` 传进来的那份，含"认不出的域名跟随系统"）。
 */
class RustImageFetch(
    private val proxyFor: (host: String) -> ProxyTarget,
    platform: String = "image",
) : ImageFetch {
    private val io = RustImageIo(platform)

    override suspend fun fetch(url: String, headers: List<Pair<String, String>>): ImageResult {
        val host = hostOf(url)
        val target = if (host.isEmpty()) {
            ProxyTarget.Unavailable("图片地址里没有主机名（不是 http/https 地址）：按约定不发请求。")
        } else {
            proxyFor(host)
        }
        val result = try {
            io.fetch(url, headers, target)
        } catch (e: FfiNetException) {
            throw e.toImageIOException()
        }
        return ImageResult(
            code = result.code,
            headers = result.headers,
            bytes = result.bytes,
            requestMillis = result.requestMillis,
            responseMillis = result.responseMillis,
        )
    }

    /**
     * 把边界上的错误换成 OkHttp 那个家族（**图片这条路以前抛的就是这些**）：
     *
     * - `ProxyUnavailable` → [ProxyUnavailableException]（`IOException` 的子类，**文案一个字不改**：
     *   `ProxyTarget.of` 给的就是 `invalidCustomProxy` / `builtinCoreNotRunning` 那两句，
     *   `YoutubeProxyFailClosedTest` / `ProxyWiringTest` 靠它断言 fail-closed）；
     * - 其余（网络 / 超时 / 请求不合法）→ [IOException]：`ApiImageService` 靠"是不是 `IOException`"
     *   决定这一趟算不算一个请求。
     */
    private fun FfiNetException.toImageIOException(): IOException = when (this) {
        is FfiNetException.ProxyUnavailable -> ProxyUnavailableException(detail)
        else -> IOException(message ?: javaClass.simpleName, this)
    }

    private companion object {
        /**
         * 从 URL 里取主机名（**不引入 URI 的解析语义**：这里只要一个能喂给代理判定的主机名）。
         *
         * 和 `PlatformProxySelector` 收到的 `uri.host` 同一个东西：scheme 之后、第一个 `/` / `?` / `#` 之前，
         * 去掉 userinfo 和端口。取不出来就是空串（调用方按 fail-closed 处理）。
         */
        fun hostOf(url: String): String {
            val scheme = url.indexOf("://")
            if (scheme <= 0) return ""
            val rest = url.substring(scheme + 3)
            val end = rest.indexOfFirst { it == '/' || it == '?' || it == '#' }
            val authority = if (end < 0) rest else rest.substring(0, end)
            val afterUser = authority.substringAfterLast('@')
            // IPv6 字面量（`[::1]:8080`）里的冒号不是端口分隔符
            if (afterUser.startsWith("[")) {
                return afterUser.substringAfter('[').substringBefore(']')
            }
            return afterUser.substringBefore(':')
        }
    }
}
