package feedhub.app.video

import feedhub.core.Platform
import feedhub.rust.ProxyTarget

/**
 * 安卓还没有本机中转：Media3 的 `ResolvingDataSource` 把清单和分片的请求放回它自己的 OkHttp（平台代理 /
 * fail-closed / 请求头都在里面），不需要"把地址换成本机回环"这一层。
 *
 * 所以安卓的 `MediaSource.relayFactory` 一直是 null（引擎根本不问）；真有人问到了就是一处接线错误，
 * 让它响亮地失败，而不是悄悄不中转（那会变成分片直连 CDN）。
 */
actual fun createRelay(
    platform: Platform,
    proxy: ProxyTarget,
    headers: Map<String, String>,
    readTimeoutMs: Int,
): RelayHandle = throw UnsupportedOperationException(
    "安卓不走本机分片中转（Media3 的分片请求在自己的 OkHttp 里过代理）：platform=$platform",
)
