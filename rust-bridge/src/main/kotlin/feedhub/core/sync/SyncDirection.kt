package feedhub.core.sync

import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import feedhub.core.RustJarHandle
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiSyncDirection
import uniffi.feedhub_ffi.FfiSyncException

/**
 * 登录互传的方向。由**出码方**选：
 *
 * - [SEND]「发出去」：出码方把本机登录发出去，扫码方取走（以前的单向流程）；
 * - [RECEIVE]「收进来」：出码方要收，扫码方把自己的登录推上来。
 *
 * 方向写进二维码（`d=send` / `d=receive`）、路径（`/v2/sync/` 对 v1）和 GCM 的 AAD，
 * 所以"拿一份发出去的密文冒充推上来的包"会被直接拒掉。
 */
enum class SyncDirection {
    SEND,
    RECEIVE;

    internal fun toFfi(): FfiSyncDirection = when (this) {
        SEND -> FfiSyncDirection.SEND
        RECEIVE -> FfiSyncDirection.RECEIVE
    }

    /** 给界面用的一句话（"发出去" / "收进来"）。 */
    val label: String get() = when (this) {
        SEND -> "发出去"
        RECEIVE -> "收进来"
    }

    companion object {
        internal fun fromFfi(direction: FfiSyncDirection): SyncDirection = when (direction) {
            FfiSyncDirection.SEND -> SEND
            FfiSyncDirection.RECEIVE -> RECEIVE
        }
    }
}

/** 一个平台 + 条数（确认框 / 完成页显示）。 */
data class SyncPlatformCount(val platform: Platform, val count: Int)

/**
 * 一次"可应用"的登录互传结果：扫码方取回来的包，或者出码方收进来的包。
 *
 * 两边都要**先让用户确认**再 [apply]：写入是"按平台各自的域先清后写"，会顶掉本机同平台的登录态。
 * `platforms` 只含平台 id + 条数，不含 Cookie 值；`apply` 只成功一次。
 */
interface SyncInbound {
    /** 包里有哪些平台、各多少条（不含 Cookie 值）。 */
    val platforms: List<SyncPlatformCount>

    /** 真正写进本机罐子（**只能成功一次**）。调用方已经确认过。 */
    fun apply(
        jar: FeedCookieJar,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): SyncApplyResult
}

/**
 * 登录互传的门面统一走这一个 `guard`：`FfiSyncException.Format` → [SyncException]
 * （消息可以直接给用户看；**不含 Cookie 值**）。
 *
 * 不用 `RustSync.guard`：那是资料库同步的（会把错误折成 `LibrarySyncFormatException`）。
 */
internal inline fun <T> syncGuard(block: () -> T): T {
    try {
        RustNative.ensureLoaded()
        return block()
    } catch (e: FfiSyncException.Format) {
        throw SyncException(e.detail)
    } catch (e: FfiSyncException.Internal) {
        throw SyncException("同步的内部错误：${e.detail}")
    } catch (e: UnsatisfiedLinkError) {
        throw SyncException("Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}")
    } catch (e: IllegalStateException) {
        throw SyncException("Rust 核心不可用：${e.message}")
    }
}

/**
 * 拿到 Rust 罐子的句柄。
 *
 * 生产路径上所有罐子（`RustCookieJar` / `MemoryCookieJar` / `PersistentCookieJar`）都是 Rust 罐子；
 * 纯 `:core` 单测里的 `SimpleMemoryCookieJar` 没有句柄，这里**明着报错**，不悄悄退回 OkHttp 那一份
 * （否则登录态会分叉，而且写进去的东西下一次读不到）。
 */
internal fun FeedCookieJar.rustJarOrThrow(): uniffi.feedhub_ffi.CookieJar =
    ((this as? RustJarHandle)?.rustJarHandle() as? uniffi.feedhub_ffi.CookieJar)
        ?: throw SyncException("这个 Cookie 罐子不是 Rust 罐子，登录互传写不进去")
