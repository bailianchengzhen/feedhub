package feedhub.app.video

import feedhub.player.RustPlayer
import feedhub.player.RustRelay

/**
 * **DASH 清单改写** + **HLS 播放列表改写**。
 *
 * 这一侧只剩两层薄壳：
 *
 * - [DashRewrite.rewrite] / [HlsRewrite.rewrite]：把 [RelayUrls] 换成 Rust 的 `FfiRelay`（登记口子）；
 * - [DashRewrite.resolveUrl]：`URI.resolve` 的口径（**也走 Rust**，和改写用的是同一份实现）。
 */
internal object DashRewrite {
    /**
     * 把清单里**所有指向远端的分片地址**换成本机中转地址（[RelayUrls]）。
     *
     * @param baseUrl 这份清单自己的地址（`null` / 空 = 不知道，那就只对绝对地址登记）。
     */
    fun rewrite(mpd: String, baseUrl: String?, urls: RustRelayUrls): String =
        RustPlayer.rewriteDash(mpd, baseUrl, urls.relay)

    /** [ref] 相对 [base] 解析成绝对地址（`java.net.URI(base).resolve(ref)` 的口径，实现在 Rust）。 */
    internal fun resolveUrl(base: String, ref: String): String = RustPlayer.resolveUrl(base, ref)
}

/**
 * HLS 播放列表改写：m3u8 里的分片 URI、子清单、`#EXT-X-KEY` / `#EXT-X-MAP` 这些带 `URI="…"` 的标签
 * 都换成 [RelayUrls] 的本机地址（实现在 Rust）。
 */
internal object HlsRewrite {
    fun rewrite(playlistUrl: String, text: String, urls: RustRelayUrls): String =
        RustPlayer.rewriteHls(playlistUrl, text, urls.relay)
}

/**
 * [RelayUrls] 的 Rust 实现：登记进 `FfiRelay`（**判定全在 Rust**），`id` 由这里生成（Kotlin 的
 * `UUID.randomUUID()`——Rust 那边不该为了"生成一个随机串"把随机源搬进来）。
 *
 * 登记和路由在同一份 Rust 状态里，少一趟边界——`VideoRelay.publishMpd` 走的是
 * Rust 侧的 `publish_mpd`）。它留着给 `ManifestRewriteTest` 这种"只想验改写、不想起一个服务器"的地方。
 */
internal class RustRelayUrls(val relay: RustRelay, private val newId: () -> String) : RelayUrls {
    /** 登记过什么（测试 / 检查工具用）：按 id 顺序回放。 */
    fun entriesInOrder(): List<Pair<String, feedhub.player.RelayEntryInfo>> =
        relay.registeredIds().mapNotNull { id -> relay.entry(id)?.let { id to it } }

    override fun resource(remoteUrl: String): String = relay.resource(remoteUrl, newId())

    override fun playlist(remoteUrl: String): String = relay.playlist(remoteUrl, newId())

    override fun template(remoteTemplate: String): String = relay.template(remoteTemplate, newId())
}
