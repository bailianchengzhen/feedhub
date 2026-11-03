package feedhub.app.video

import feedhub.core.Platform
import feedhub.rust.ProxyTarget

/**
 * 桌面的本机中转：就是 [VideoRelay]（Rust `FfiRelayServer` 的门面，见 `feedhub.player.RustRelayServer`）。
 *
 * 安卓那一份在同名文件的 `androidMain`：Media3 用 `ResolvingDataSource` 把分片请求放进自己的 OkHttp，
 * 所以不需要"回环中转"这一层，实现直接抛。
 */
actual fun createRelay(
    platform: Platform,
    proxy: ProxyTarget,
    headers: Map<String, String>,
    readTimeoutMs: Int,
): RelayHandle = VideoRelay(platform = platform, proxy = proxy, headers = headers, readTimeoutMs = readTimeoutMs)
