package feedhub.core.sync

import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiLoginPending
import uniffi.feedhub_ffi.ffiSyncLoginFetch
import uniffi.feedhub_ffi.ffiSyncLoginPush

/**
 * 扫码方：拿二维码里的地址去对方设备上**取一次**密文（`Send` 方向），或把本机登录**推上去**
 * （`Receive` 方向）。
 *
 * 两个和安全有关的细节（都在 Rust 里）：
 * - **显式局域网直连**：不跟 App 的"境外平台代理"走，也确实只去用户扫到的那个内网地址；
 * - 不跟 3xx、不重试、不带 Cookie、不带任何多余请求头：除了"我要取 / 推这个会话"之外
 *   不给对端任何信息。
 *
 * 不解密就不知道里面是什么；解不开、长度不对、格式不对全部当失败，**不重试**（服务端一次性的）。
 */
class SyncClient {
    /**
     * 取回 + 解密 + 校验（**不改本机罐子**）。只认 `Send` 方向的码。
     * @throws SyncException 方向不对 / 连不上 / 过期 / 密钥不对 / 内容被改 / 格式不对。
     */
    fun fetch(offer: SyncOffer): SyncPending =
        SyncPending(syncGuard { ffiSyncLoginFetch(offer.toFfi()) })

    /**
     * 把本机选中的平台推给 `Receive` 方向的码。
     * 返回本机打包的个数（对方应用成功与否在这边看不到，对方那边有它自己的确认框）。
     */
    fun push(
        offer: SyncOffer,
        jar: FeedCookieJar,
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): SyncPushResult {
        val result = syncGuard {
            ffiSyncLoginPush(
                offer.toFfi(),
                jar.rustJarOrThrow(),
                platforms.map { it.key },
                scopes.toFfiScopes(),
                now,
            )
        }
        return SyncPushResult(
            counts = result.counts.toCounts(),
            skipped = result.skipped.map(::Platform),
        )
    }
}

/** 扫码方取回来的、已经解密校验过的包。**写罐子之前先让用户确认**。 */
class SyncPending internal constructor(private val inner: FfiLoginPending) : SyncInbound {
    override val platforms: List<SyncPlatformCount>
        get() = inner.platforms().toPlatformCounts()

    /** 写进本机罐子（**只能成功一次**）；调用方已经确认过。 */
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
}

/**
 * 资料库同步的配对（`SyncPairing`）还用它：同一局域网直连、不走代理、不跟 3xx、不重试、不带 Cookie。
 * 登录互传本身已经不用 OkHttp 了（Rust `std::net`），这个客户端只留给配对那一层。
 */
internal object SyncLanHttp {
    fun defaultClient(): okhttp3.OkHttpClient = okhttp3.OkHttpClient.Builder()
        .proxy(java.net.Proxy.NO_PROXY)
        .followRedirects(false)
        .followSslRedirects(false)
        .retryOnConnectionFailure(false)
        .connectTimeout(5, java.util.concurrent.TimeUnit.SECONDS)
        .readTimeout(10, java.util.concurrent.TimeUnit.SECONDS)
        .callTimeout(20, java.util.concurrent.TimeUnit.SECONDS)
        .build()
}

/** 推送成功时本机打包的个数（哪些平台带上了、各几条、跳过谁）。 */
data class SyncPushResult(
    val counts: Map<Platform, Int>,
    val skipped: List<Platform>,
)
