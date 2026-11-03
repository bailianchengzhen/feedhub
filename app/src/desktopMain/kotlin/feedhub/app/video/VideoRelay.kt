package feedhub.app.video

import feedhub.core.Platform
import feedhub.player.RustRelayServer
import feedhub.rust.ProxyTarget

/**
 * 改写清单时用来登记地址的口子（[VideoRelay] 实现，测试里换成记账用的假实现）。
 * 三个方法都返回**本机中转地址**，播放器只会去连它们。
 */
internal interface RelayUrls {
    /** 登记一个完整的远端地址（单个文件、一个分片、一把密钥），返回本机地址。 */
    fun resource(remoteUrl: String): String

    /** 登记一个远端播放列表 / 清单（m3u8，或远端 .mpd）：播放器取的时候现取现改写。 */
    fun playlist(remoteUrl: String): String

    /** 登记一个远端分片模板（带 `$Number$` / `$Time$` 这类占位符），返回本机模板地址。 */
    fun template(remoteTemplate: String): String
}

/**
 * 本机回环上的**视频分片中转**（DASH 清单 + 分片，HLS 播放列表 + 分片）。
 *
 * 一行 Kotlin
 * 都不剩。这个类只剩**起 / 停 / 登记**三个动作，外加检查工具要的那几个记账字段。
 *
 * 为什么要有它：GStreamer 的 dashdemux2 /
 * hlsdemux2 用它们**自己的** libsoup session 下载分片，不会产生 `souphttpsrc`、也不会回调
 * `source-setup`，所以"给清单设代理"对分片没用——配了代理也是分片直连 CDN。这里把清单里所有远端
 * 地址改写成本机回环地址（`http://127.0.0.1:<port>/…`），由播放器去这里取；中转再用**这个平台的
 * 代理 / 请求头**去取真正的分片。于是
 *
 * - 平台代理、「指定代理地址没填全就失败」的 fail-closed、Referer / User-Agent 这些请求头，
 *   全都跟着同一条判定走（Rust 的 `Proxy` 一档 + `RelayConfig.headers`）；
 * - 中转出来的地址全在 127.0.0.1 上：播放器那侧取清单的 souphttpsrc 被显式设成空串；下分片那一路
 *   （adaptivedemux2 自带一份 libsoup）整个插件里没有任何代理代码，本来就不看 env；
 * - 播放器断开时（写回环 socket 抛 IOException）Rust 那边把上游一起掐掉。
 *
 * 支持 `Range` / `206` / `Content-Range` 原样透传（DASH 的 SegmentBase / indexRange 就是靠字节范围取
 * 的）、`HEAD` 只回头、数据**边收边发**（不整段缓存：分片可以很大）。
 *
 * 用法：[publishMpd]（我们自己拼的 DASH）/ [playlist]（远端 .mpd 或 m3u8）/ [resource]（分片），
 * 用完 [close]（引擎在拆管线时调）。
 *
 * @param platform 这个平台（代理 / 记账口径）。
 * @param proxy 这个平台**当下**那一档代理（`Unavailable` = fail-closed：一个字节都不发）。
 * @param headers 拉流要带的请求头（[MediaSource.headers]，B站是 Referer / User-Agent）——由中转加，
 *   不再靠 demux 的 `http-headers` 事件。
 */
internal class VideoRelay(
    platform: Platform,
    proxy: ProxyTarget,
    headers: Map<String, String> = emptyMap(),
    readTimeoutMs: Int = 0,
) : RelayUrls, RelayHandle {

    /** 真正的服务（Rust）。 */
    private val server: RustRelayServer = RustRelayServer.open(
        platform = platform,
        proxy = proxy,
        headers = headers,
        readTimeoutMs = readTimeoutMs,
        debug = debug,
    )

    /** 给清单改写用的登记口子（[DashRewrite] / [HlsRewrite] 拿它登记远端地址）。 */
    internal val relayUrls: RelayUrls = this

    /** 播放器看到的基地址（回环 + 随机端口）。 */
    override val base: String = server.base

    /** 已经向上游取过多少次。 */
    override val upstreamRequests: Int get() = server.upstreamRequests

    /** 向上游取过的地址的主机名（检查工具用）。 */
    override val upstreamHostNames: Set<String> get() = server.upstreamHostNames

    /** 上游请求结束时回调一次（默认不装 = 不记）。在中转的处理线程里调，别做重活。 */
    override var onUpstreamDone: ((RelayUpstreamStat) -> Unit)? = null
        set(value) {
            field = value
            server.setOnUpstreamDone(
                value?.let { h ->
                    { stat: feedhub.player.UpstreamStat ->
                        h(RelayUpstreamStat(stat.bytes, stat.ms, stat.range, stat.ok))
                    }
                },
            )
        }

    /** 曾经同时有几个上游请求在飞（检查工具用：看 demux 是串行取还是并发取）。 */
    override val maxInFlight: Int get() = server.maxInFlight

    /** 现在有几个上游请求在飞（测试用：播放器断开之后它必须很快归零）。 */
    val inFlight: Int get() = server.inFlight

    // ---------- 登记 ----------

    /** 我们自己的 DASH 清单（[DashManifest] 拼的那份）：把里面的远端地址改写成本机地址后放进来。 */
    override fun publishMpd(mpd: String, baseUrl: String?): String = server.publishMpd(mpd, baseUrl)

    /**
     * 远端清单 / 播放列表（`.mpd` 直链 或 m3u8，HLS 的子清单也走这个）：
     * 登记成本机地址，播放器来取时现取现改写（直播的 m3u8 每次请求都可能是新的）。
     */
    override fun playlist(remoteUrl: String): String = server.playlist(remoteUrl)

    override fun resource(remoteUrl: String): String = server.resource(remoteUrl)

    override fun template(remoteTemplate: String): String = server.template(remoteTemplate)

    override fun close() = server.close()

    /** 登记表里有多少条（检查工具用）。 */
    internal val registeredCount: Int get() = server.size

    /** 登记过的 id（登记顺序；检查工具用）。 */
    internal fun registeredIds(): List<String> = server.registeredIds()

    /** 某个 id 的条目摘要（检查工具用；**不含 URL 的 query**）。 */
    internal fun entryDebug(id: String): String? = server.entryDebug(id)

    private companion object {
        /** 调试用：`-Dfeedhub.relay.debug=1` 时把中转收到的请求 / 上游结果打到 stderr。 */
        val debug: Boolean get() = System.getProperty("feedhub.relay.debug") != null
    }
}
