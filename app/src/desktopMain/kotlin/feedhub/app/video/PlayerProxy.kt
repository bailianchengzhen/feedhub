package feedhub.app.video

import com.sun.jna.Library
import com.sun.jna.Native
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import java.net.URI

/**
 * 播放器的代理：**视频流也要走这个平台的代理**。
 *
 * 从"分片中转"之后，视频这一侧分两种：
 *
 * - **DASH / HLS**（B站和以后任何 adaptive 流）：清单和分片全走中转，代理由那份 OkHttp 客户端决定
 *   （[MediaSource.relayClient]），播放器自己只连本机回环；
 * - **音画一体的 mp4**：多数情况还是 `souphttpsrc` 直接取，代理设 `souphttpsrc.proxy`（`跟随系统` 不设、
 *   `HTTP 代理` 设地址）。「直连」和「指定代理是 SOCKS5」两种 `souphttpsrc` **表达不了**，这两种交给中转，由 OkHttp（它支持 `NO_PROXY` / SOCKS5）去连——
 *   见 `GstPlayerEngine.startCurrent` 里的 `needsRelay`。
 *
 * 这两个函数是纯的、好测；真正设下去的地方是 [GstPlayerEngine.setupSource]（`souphttpsrc` 的 `proxy` 属性）
 * 和 [VlcPlayerEngine.startCurrent]（媒体的 `:http-proxy=` / `:socks=` 选项）。
 */

/**
 * GStreamer `souphttpsrc` 的 `proxy` 属性值：`http://host:port`。返回 null = 不设（交给 GStreamer 自己，
 * 也就是走 GIO 的默认代理解析器，读 `http_proxy` 等环境变量）。
 *
 * - **直连**：返回 null，**调用方改走中转**。`souphttpsrc` 这边做不到"直连"：空串会被元素存成 `http://`
 * ；不设也一样。
 *   只有中转里那份明确 `Proxy.NO_PROXY` 的 OkHttp 客户端才谈得上直连。
 * - **指定代理（HTTP）**：地址 / 端口填全 → `http://host:port`；**没填全 → 一个必然连不上的替身**
 *   （`http://127.0.0.1:1`，fail-closed）。以前这里返回 null，等于"没填全就悄悄直连"，
 *   和接口 / 图片那两路（[feedhub.core.ProxyConfigGuardInterceptor]）的约定不一致，对齐一下。
 * - **指定代理（SOCKS5）**：返回 null，**调用方改走中转**。
 *   在 libsoup2 时代，`souphttpsrc` 会对缺少 `http://` 前缀的代理串做 `g_strconcat("http://", proxy, NULL)`，
 *   导致 `socks5://...` 变成 `http://socks5://...` 前缀失效并掉回 GIO 的默认解析器；而在 libsoup3（GStreamer 1.26+）
 *   下，虽然遇到已带 `://` 的 URI 不再强拼 `http://` 且 `GSimpleProxyResolver` 会发起无认证 SOCKS5 握手，但：
 *   1) `souphttpsrc` 的 `proxy-id` / `proxy-pw` 只走 HTTP Basic 认证，不支持完整 SOCKS5 凭据；
 *   2) 老发行版环境不认 `socks5://` 协议；
 *   3) 统一走中转由 `feedhub-net` 处理，能完整保证凭据认证、连接池复用与连接指纹一致。
 *   因此这里一律返回 null，交给中转。
 * - **跟随系统**：返回 null——播放器的"系统"就是它自己读环境变量，和 JVM 那套不是一回事。
 * - **本机回环地址**：返回 `""`。中转出来的清单全在 127.0.0.1 上，**必须显式设空串**：不设的话元素初始化时
 *   会把 env `http_proxy` 记成显式代理，而 GSimpleProxyResolver **不忽略** localhost / 127.0.0.1，
 *   回环请求就被代理接走了；设成空串之后回环是直连。
 *   设了空串之后代理交给 GIO 的默认解析器（libproxy），它看 `no_proxy`，所以还要配合
 *   [ensureLoopbackNotProxied] 把回环写进 `no_proxy`——两个一起才是确定性的直连（`GstProxyEnvTest` 量过）。
 */
fun gstProxyUri(proxy: ProxyMode, url: String): String? {
    // 回环（中转出来的清单）：显式设空串——把元素初始化时从 env 读进来的那份代理清掉，
    // 之后由 GIO 默认解析器按 no_proxy 判断（[ensureLoopbackNotProxied] 把回环写了进去）
    if (isLoopbackUrl(url)) return ""
    return when (proxy) {
        ProxyMode.System -> null
        ProxyMode.Direct -> null // 做不到：调用方改走中转（needsRelay）
        is ProxyMode.Custom -> when {
            !proxy.valid -> "http://127.0.0.1:1" // 必然连不上，绝不悄悄直连（和 core 的 deadProxy 一个意思）
            proxy.type == ProxyType.SOCKS5 -> null // souphttpsrc 设不上 SOCKS5：调用方改走中转（needsRelay）
            else -> "http://${proxy.host}:${proxy.port}"
        }

        // 内置核心：在跑是本机 SOCKS5，souphttpsrc 同样设不上 → 中转；没在跑给必然连不上的替身（fail-closed）
        is ProxyMode.BuiltinCore -> if (proxy.valid) null else "http://127.0.0.1:1"
    }
}

/**
 * VLC 的代理媒体选项（加在 `player.media().play(url, *options)` 那一串里）。
 *
 * - HTTP 代理：`:http-proxy=http://host:port`（http / https 的 access 模块读它）；
 * - SOCKS5：`:socks=host:port`（libvlc 的 `--socks`，由 `net_ConnectTCP` 走）。
 *
 * 直连和跟随系统都返回空表：libvlc 没有在媒体选项这一层"强制直连"的做法（不设就是它自己的默认，
 * 也就是看环境变量），所以 [ProxyMode.Direct] 在这里和 [ProxyMode.System] 效果一样——**这一条仍是已知局限**。
 * DASH / HLS 走中转，由 OkHttp 决定代理，libvlc 只请求本机回环；但 env `http_proxy` 会不会把 libvlc 的
 * 回环请求也截走**没在本机验证过**（libvlc 的 http access 自己读环境变量，GIO 的 `no_proxy` 管不到它）——
 * 这一条如实写在这里。
 */
fun vlcProxyOptions(proxy: ProxyMode): List<String> = when (proxy) {
    is ProxyMode.Custom -> if (!proxy.valid) {
        emptyList()
    } else {
        when (proxy.type) {
            ProxyType.HTTP -> listOf(":http-proxy=http://${proxy.host}:${proxy.port}")
            ProxyType.SOCKS5 -> listOf(":socks=${proxy.host}:${proxy.port}")
        }
    }

    // 内置核心就是本机的 SOCKS5（libvlc 的 --socks）；没在跑时不设（VLC 这一档本来就是已知局限）
    is ProxyMode.BuiltinCore -> if (proxy.valid) listOf(":socks=127.0.0.1:${proxy.port}") else emptyList()
    else -> emptyList()
}

/**
 * 让 GIO 的默认代理解析器（libproxy）**不要**代理回环：`no_proxy` / `NO_PROXY` 里补上
 * `127.0.0.1` / `localhost` / `::1`。已有的值保留（逗号分隔），设过 `*` 就不动。
 *
 * 为什么需要：中转出来的清单 / 整段 mp4 在 127.0.0.1 上，取它们的 `souphttpsrc` 被设成空串之后走的就是
 * GIO 默认解析器，而 libproxy 按 `http_proxy` 选代理、**并不忽略回环**。只设空串不够稳，空串 + `no_proxy` 才是确定性的直连。
 *
 * 只影响 GIO / libsoup 这一侧（JVM 的系统代理看的是 `http.nonProxyHosts`，本来就排除回环）。
 * [GstRuntime] 初始化时调一次；重复调用没有副作用。
 */
fun ensureLoopbackNotProxied() {
    val want = listOf("127.0.0.1", "localhost", "::1")
    for (name in listOf("no_proxy", "NO_PROXY")) {
        val current = System.getenv(name)?.trim().orEmpty()
        val have = current.split(',').map { it.trim().lowercase() }.filter { it.isNotEmpty() }
        if ("*" in have || want.all { it in have }) continue
        val merged = (if (current.isEmpty()) emptyList() else listOf(current)) + want.filter { it !in have }
        runCatching { LibC.INSTANCE?.setenv(name, merged.joinToString(","), 1) }
    }
}

private interface LibC : Library {
    fun setenv(name: String, value: String, overwrite: Int): Int

    companion object {
        val INSTANCE: LibC? = runCatching { Native.load("c", LibC::class.java) }.getOrNull()
    }
}

/** 远端的音画一体地址是不是一份 DASH 清单（平台直接给 `.mpd` 直链时也走中转）。**判定在 Rust**。 */
internal fun isDashManifestUrl(url: String): Boolean = feedhub.player.RustPlayer.isDashUrl(url)

/** [url] 的主机是不是本机回环（`127.0.0.0/8`、`::1`、`localhost`）；解析不出来的地址当不是。**判定在 Rust**。 */
internal fun isLoopbackUrl(url: String): Boolean = feedhub.player.RustPlayer.isLoopbackUrl(url)
