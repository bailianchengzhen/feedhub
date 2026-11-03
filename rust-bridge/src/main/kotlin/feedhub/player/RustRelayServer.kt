package feedhub.player

import feedhub.rust.ProxyTarget
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiRelayConfig
import uniffi.feedhub_ffi.FfiRelayPlan
import uniffi.feedhub_ffi.FfiRelayServer
import uniffi.feedhub_ffi.FfiRelayUpstreamHook
import uniffi.feedhub_ffi.FfiRelayUpstreamStat

/**
 * **本机回环上的视频分片中转**。
 *
 * 搬之前这套东西是 `VideoRelay` + `MpdServer` + `ManifestRewrite` 三份 Kotlin（424 行）：一个
 * `com.sun.net.httpserver.HttpServer`、一个 8 线程池、OkHttp 收发、清单改写的两条薄壳。现在
 * **服务器、登记表、路由、清单改写、上游收发全在 Rust**（`feedhub-ffi/src/relay_server.rs` +
 * `feedhub-core/src/player/{relay,manifest}`），Kotlin 只剩这个门面：起、停、登记地址。
 *
 * ## 为什么要有它
 *
 * GStreamer 的 dashdemux2 / hlsdemux2 用它们**自己的** libsoup session 下分片，不会产生 `souphttpsrc`、
 * 也不回调 `source-setup`，所以"给清单设代理"对分片没用——配了代理也是分片直连 CDN。中转把清单里
 * 所有远端地址改写成本机回环地址，由播放器去这里取；中转再用**这个平台的代理 / 请求头**去取真正的
 * 分片。于是平台代理、fail-closed、Referer / UA 全都跟着同一条判定走。
 *
 * ## 红线（和搬之前一样）
 *
 * - 只监听 `127.0.0.1`（[base] 里那个端口由内核选）；
 * - 上游只按 [plan] 那一档走：代理不可用时**一个字节都不发**，播放器收到 502，绝不悄悄直连 CDN；
 * - 播放器断开 → 立刻掐掉上游（Rust 里那条 `NetStream` 的 `drop`）；
 * - `Range` / `206` / `Content-Range` 原样透传（DASH 的 SegmentBase / indexRange 靠它）；
 * - **不带 Cookie**：分片在 CDN 上，登录凭据不该发过去。
 *
 * 线程安全：Rust 那边带锁（播放引擎线程、界面线程、检查工具会同时碰），Kotlin 这边不再加锁。
 */
class RustRelayServer private constructor(private val inner: FfiRelayServer) : AutoCloseable {

    /** 播放器看到的基地址（`http://127.0.0.1:<port>`）。 */
    val base: String = native { inner.base() }

    /** 监听端口（检查工具 / 测试用）。 */
    val port: Int = native { inner.port().toInt() }

    // ---------- 登记 ----------

    /**
     * 登记一个远端资源（分片、单文件、一把密钥），返回本机地址。
     *
     * `id` 由**这里**生成（[newId]）：URL 里的 id 要能对回远端地址，而"生成随机串"留在宿主
     * （Rust 那边不该为了一个 UUID 把随机源搬进来——那会让"同一份输入两次调用结果一样"不成立）。
     */
    fun resource(remoteUrl: String): String = native { inner.resource(remoteUrl, newId()) }

    /** 登记一个远端播放列表 / 清单（m3u8，或远端 `.mpd`）：播放器取的时候现取现改写。 */
    fun playlist(remoteUrl: String): String = native { inner.playlist(remoteUrl, newId()) }

    /** 登记一个远端分片模板（带 `$Number$` / `$Time$` 这类占位符），返回本机模板地址。 */
    fun template(remoteTemplate: String): String = native { inner.template(remoteTemplate, newId()) }

    /** 放一份我们自己拼好、已经改写过的清单（[DashManifest] 拼的那份）。 */
    fun local(body: String, contentType: String?): String = native {
        inner.local(body, contentType, newId())
    }

    /** 我们自己的 DASH 清单：把里面的远端地址改写成本机地址之后放进来（[DashManifest] 的出口）。 */
    fun publishMpd(mpd: String, baseUrl: String?): String = native {
        inner.publishMpd(mpd, baseUrl, newId())
    }

    // ---------- 记账（检查工具 / 吞吐测量台） ----------

    /** 登记表里有多少条。 */
    val size: Int get() = native { inner.len().toInt() }

    /** 登记过的 id（登记顺序）。 */
    fun registeredIds(): List<String> = native { inner.registeredIds() }

    /** 某个 id 的条目摘要（**不含 URL 的 query**）。 */
    fun entryDebug(id: String): String? = native { inner.entryDebug(id) }

    /** 已经向上游取过多少次（证明分片真的是中转去取的）。 */
    val upstreamRequests: Int get() = native { inner.upstreamRequests().toInt() }

    /** 向上游取过的地址的主机名。 */
    val upstreamHostNames: Set<String> get() = native { inner.upstreamHosts().toSet() }

    /** 曾经同时有几个上游请求在飞（看 demux 是串行取还是并发取）。 */
    val maxInFlight: Int get() = native { inner.maxInFlight().toInt() }

    /** 现在有几个上游请求在飞（测试用：播放器断开之后它必须很快归零）。 */
    val inFlight: Int get() = native { inner.inFlight().toInt() }

    /**
     * 装一个"一次上游请求结束"的回调（检查工具 / 吞吐测量台用；`null` = 卸掉）。
     *
     * 在**中转的处理线程**里调，别做重活——和搬之前 `VideoRelay.debugOnUpstreamDone` 一个口径。
     * 实现里**不许往外抛异常**：这是 `with_foreign` 的 trait，抛出去会被 UniFFI 包成
     * `UnexpectedUniFFICallbackError`。
     */
    fun setOnUpstreamDone(hook: ((UpstreamStat) -> Unit)?) {
        native {
            inner.setOnUpstreamDone(hook?.let { h ->
                object : FfiRelayUpstreamHook {
                    override fun upstreamDone(stat: FfiRelayUpstreamStat) {
                        runCatching { h(stat) }
                    }
                }
            })
        }
    }

    /** 关掉（幂等）：掐在飞的上游请求、停 accept。 */
    override fun close() = native { inner.shutdown() }

    /** 起一个的入口（[RustRelayServer.open]）。 */
    companion object {
        /**
         * 起一个中转服务。
         *
         * @param platform 平台 id（只影响记账口径；中转**不挂限速器**——播放器要的是"这一块现在就到"）。
         * @param proxy 这一档代理。`Unavailable` = fail-closed：那个平台一个请求都不发。
         * @param headers 拉流要带的请求头（[feedhub.app.video.MediaSource.headers]，B站是 Referer / UA），
         *   **有序**。
         * @param readTimeoutMs 读超时（`0` = 客户端默认）。
         * @param threads 同时处理几个请求（`0` = 8，和搬之前 `THREADS` 一样）。
         * @param debug 调试日志（打 stderr；`-Dfeedhub.relay.debug=1` 时由调用方传 true）。
         */
        fun open(
            platform: feedhub.core.Platform,
            proxy: ProxyTarget,
            headers: Map<String, String> = emptyMap(),
            readTimeoutMs: Int = 0,
            threads: Int = 0,
            debug: Boolean = false,
        ): RustRelayServer {
            val config = FfiRelayConfig(
                plan = FfiRelayPlan(
                    platform = platform.key,
                    proxy = proxy.toFfi(),
                    readTimeoutMs = readTimeoutMs.toUInt(),
                ),
                // 有序（`Map` 在 Kotlin 里是 `LinkedHashMap`：`MediaSource.headers` 的顺序就是发出去的头顺序）
                headers = headers.map { (k, v) -> "$k: $v" },
                threads = threads.toUInt(),
                debug = debug,
            )
            return native { RustRelayServer(FfiRelayServer(config)) }
        }

        /** 本机地址里的随机串（**不含 `-`**：和搬之前 `UUID.replace("-", "")` 一样长 32 个十六进制字符）。 */
        private fun newId(): String = java.util.UUID.randomUUID().toString().replace("-", "")

        private inline fun <T> native(block: () -> T): T = try {
            RustNative.ensureLoaded()
            block()
        } catch (e: UnsatisfiedLinkError) {
            throw IllegalStateException("视频中转：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
        }
    }
}

/** 一次上游请求的记账（检查工具 / 测试用）。 */
typealias UpstreamStat = FfiRelayUpstreamStat

