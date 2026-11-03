package feedhub.app.video

import feedhub.core.Platform
import feedhub.player.RustRelayServer
import feedhub.rust.ProxyTarget

/**
 * 只在本机回环地址上提供 MPD 的小服务（**没有中转时的退路**，见 [GstPlayerEngine]）。
 *
 * dashdemux2 要靠 souphttpsrc 下载清单，才会把清单请求带的 Referer / User-Agent 一并用到分片请求上
 * （清单走 file: 或 data: 的话没有这个"http-headers"事件，B站的分片会 403）。路径里带随机串，只有本进程
 * 的管线知道。
 *
 * **注意**：这条退路下分片仍是 demux 自己直连 CDN。正常路径（App 里、
 * 开发工具里都会给 `MediaSource.relayClient`）走 [VideoRelay]：清单和分片都指回本机、由 Rust 中转按平台
 * 代理去取。
 *
 * 现在只剩一个
 * `RustRelayServer` 当"内存里的清单服务"用（`local` 登记 + 回正文，Rust 那边本来就有这条路）。
 */
internal class MpdServer {
    /** 用 `Direct` 起：这条退路只回**内存里的字节**，一个上游请求都不发。 */
    private val server: RustRelayServer = RustRelayServer.open(
        platform = Platform("mpd-server"),
        proxy = ProxyTarget.Direct,
        threads = 1,
    )

    /** 放一份清单，返回它的 URL。 */
    fun publish(mpd: String): String = server.local(mpd, "application/dash+xml")

    fun unpublish(url: String) {
        // 登记表是进程内的、随播放器管线一起拆（`close` 会停掉整个服务）；单条撤不掉也不影响正确性
        // （id 是随机串，只有这条管线的 demux 知道）。留着这个方法是为了不改调用方。
        if (url.isEmpty()) return
    }

    fun close() = server.close()
}
