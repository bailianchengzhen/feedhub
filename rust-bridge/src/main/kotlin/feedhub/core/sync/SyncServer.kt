package feedhub.core.sync

import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiLoginPhase
import uniffi.feedhub_ffi.FfiLoginServer
import uniffi.feedhub_ffi.ffiSyncLoginStartServer

/**
 * 出码方起的一次性小服务：**只在这个对话框开着的时候**在选定的局域网地址上监听，随机端口，
 * 两分钟或成功一次就关。
 *
 * 方向由出码方选：
 * - [SyncDirection.SEND]：只认 `GET <路径>`，响应体是密文（扫码方取走）；
 * - [SyncDirection.RECEIVE]：只认 `POST <路径>`，扫码方把密文推上来；收到之后等本机确认再写罐子。
 *
 * 别的路径 / 方法一律 404 / 405，连接立刻关掉；累计 3 次不对就提前收摊；总共最多接 8 个连接。
 * **实现在 Rust**（`feedhub-core/src/sync/login.rs`，`std::net` 局域网直连）；这里只是薄门面。
 *
 * 为什么绑**具体网卡地址**而不是 `0.0.0.0`：绑 0.0.0.0 等于在所有网卡（包括 VPN、docker 网桥、
 * 甚至以后插上的公网口）上开同一个门，而界面只让用户看到 / 选一个地址，实际暴露面应该和它一致。
 * 模拟器端到端验证是特例：测试工具把监听地址设成 127.0.0.1、二维码里写 10.0.2.2
 * （模拟器访问宿主的方式）。
 *
 * 这个类**不认识** Cookie / 密钥，不写日志、不落盘。
 */
class SyncServer private constructor(
    /** 二维码里那一条（含实际绑定端口）。 */
    val offer: SyncOffer,
    val direction: SyncDirection,
    private val inner: FfiLoginServer,
    /** 发出去模式：会传哪些平台、各几条；收进来模式是空的。 */
    val counts: Map<Platform, Int> = emptyMap(),
    val skipped: List<Platform> = emptyList(),
) : SyncInbound {

    enum class State {
        /** 还没 start。 */
        IDLE,

        /** 正在监听，等对方扫码。 */
        LISTENING,

        /** 发出去：对方取走了（响应写完），服务已关。 */
        DELIVERED,

        /** 收进来：对方推上来了，等本机确认后写罐子。 */
        RECEIVED,

        /** 两分钟到了，没等到对方。 */
        EXPIRED,

        /** 用户关掉对话框 / 主动停 / 连续收到错误请求，服务已关。 */
        STOPPED,

        /** 监听本身失败（端口被占、地址不可用……），[failure] 是给用户看的一句话。 */
        FAILED,
    }

    /** 实际监听的端口（start 之后才有值）。 */
    val boundPort: Int get() = inner.state().boundPort.toInt()

    /** 什么时候过期（Unix 毫秒，start 之后才有值）。 */
    val expiresAtMs: Long get() = inner.state().expiresAtMs

    /** [State.FAILED] 时的原因（不含密钥 / Cookie）。 */
    val failure: String? get() = inner.state().failure

    val state: State
        get() = when (inner.state().phase) {
            FfiLoginPhase.IDLE -> State.IDLE
            FfiLoginPhase.LISTENING -> State.LISTENING
            FfiLoginPhase.DELIVERED -> State.DELIVERED
            FfiLoginPhase.RECEIVED -> State.RECEIVED
            FfiLoginPhase.EXPIRED -> State.EXPIRED
            FfiLoginPhase.STOPPED -> State.STOPPED
            FfiLoginPhase.FAILED -> State.FAILED
        }

    /** 收进来模式：对方推上来哪些平台、各几条（确认框用；没收到的时侯是空）。 */
    override val platforms: List<SyncPlatformCount>
        get() = inner.pendingPlatforms()?.toPlatformCounts().orEmpty()

    /** 关掉（可重复调用）。已经 DELIVERED / RECEIVED / EXPIRED 时只把套接字收掉。 */
    fun stop() {
        inner.stop()
    }

    /** 收进来模式：确认之后写进本机罐子（**只能成功一次**）。 */
    override fun apply(
        jar: FeedCookieJar,
        scopes: Map<Platform, SyncScope>,
        now: Long,
    ): SyncApplyResult {
        val result = syncGuard { inner.apply(jar.rustJarOrThrow(), scopes.toFfiScopes(), now) }
        return SyncApplyResult(
            applied = result.applied.toCounts(),
            skipped = result.skipped.associate { it.platformId to it.reason },
        )
    }

    companion object {
        /**
         * 开始监听。发出去模式会在 Rust 里先打包（`platforms` 只在这一种模式用）；
         * 一个平台都没有时抛 [SyncException]（界面显示）。
         *
         * @param bindHost 真正绑定的局域网地址（来自 `LanAddresses`）
         * @param advertiseHost 写进二维码的地址；生产里和 [bindHost] 相同，验收工具可以不同
         */
        fun start(
            jar: FeedCookieJar,
            bindHost: String,
            direction: SyncDirection,
            platforms: Collection<Platform>,
            scopes: Map<Platform, SyncScope>,
            ttlMs: Long = SyncProtocol.TTL_MS,
            now: Long = System.currentTimeMillis(),
            advertiseHost: String = bindHost,
        ): SyncServer {
            val jarHandle = jar.rustJarOrThrow()
            val built = if (direction == SyncDirection.SEND) {
                SyncPackageBuilder.build(jar, platforms, scopes, now)
            } else {
                null
            }
            val inner = syncGuard {
                ffiSyncLoginStartServer(
                    jarHandle,
                    bindHost,
                    advertiseHost,
                    direction.toFfi(),
                    platforms.map { it.key },
                    scopes.toFfiScopes(),
                    now,
                    ttlMs,
                )
            }
            return SyncServer(
                offer = SyncOffer.fromFfi(inner.offer()),
                direction = direction,
                inner = inner,
                counts = built?.counts.orEmpty(),
                skipped = built?.skipped.orEmpty(),
            )
        }
    }
}

/** 用二维码里的 IPv4 字面量起服务（界面只拿得到字符串）。 */
object SyncServers {
    fun start(
        jar: FeedCookieJar,
        host: String,
        direction: SyncDirection,
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
        ttlMs: Long = SyncProtocol.TTL_MS,
        now: Long = System.currentTimeMillis(),
    ): SyncServer = SyncServer.start(jar, host, direction, platforms, scopes, ttlMs, now)
}
