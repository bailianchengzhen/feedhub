package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.toOkHttpRequest
import uniffi.feedhub_ffi.FfiHeader
import uniffi.feedhub_ffi.FfiHttpRequest
import uniffi.feedhub_ffi.FfiYoutubeLoginUrls
import uniffi.feedhub_ffi.FfiYoutubePoll
import uniffi.feedhub_ffi.FfiYoutubeSearchKind
import uniffi.feedhub_ffi.FfiYoutubeSearchSort
import uniffi.feedhub_ffi.FfiYoutubeSource
import uniffi.feedhub_ffi.HostScript

/**
 * **YouTube 平台对象的 Kotlin 门面**：`FfiYoutubeSource`（Rust 里的 `uniffi::Object`）+ 宿主 store / QuickJS 端口。
 *
 * 所有业务与会话编排（信息流、搜索、详情、评论/楼中楼、播放取流、设备码登录状态机、token 存取与单飞刷新、
 * 401 换一次重试、`raw` 调试请求）全部在 Rust（`feedhub-core` + `feedhub-net`）里闭环执行。
 */
class RustYoutubeHosted(
    ctx: SourceContext,
    private val base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
    /**
     * 设备码 / 换 token 的四个端点（线上就是默认那四个；测试换成 MockWebServer 的本地地址）。
     * `null` = 用 Rust 那边的默认值。
     */
    loginUrls: RustYoutubeLoginUrls? = null,
    /**
     * n/sig 的 JS 求解（QuickJS 在 Kotlin 那边）。`null` = 不提供，Rust 会把需要解 n/sig 的响应
     * 当成"这个客户端失败"。生产路径由 `RustYoutubeSource` 传进来（见 `YoutubeEjs`）。
     */
    script: HostScript? = null,
    /** 最小请求间隔。 */
    private val minIntervalMs: Long = YoutubeRate.MIN_INTERVAL_MS,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiYoutubeSource by HostedCall.native {
        FfiYoutubeSource.withNet(
            base,
            loginUrls?.toFfi(),
            net.inner,
            net.spec(
                platform = platform,
                minIntervalMs = minIntervalMs,
            ),
            RustStoreHost(ctx.store),
            script,
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /** 搜一页（`POST /youtubei/v1/search`，**WEB 客户端**，一律匿名）。 */
    suspend fun search(request: SearchRequest, cursor: Cursor?, nowSec: Long): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(
                    native.search(
                        request.text,
                        request.kind.toFfiYoutube(),
                        FfiYoutubeSearchSort.RELEVANCE,
                        cursor?.value,
                        nowSec,
                    ),
                    platform,
                )
            }
        }

    suspend fun feedSession(kind: FeedKind, cursor: Cursor?, nowSec: Long, nowMs: Long): Page<FeedItem> {
        val ffiKind = kind.toFfi()
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feedSession(ffiKind, cursor?.value, nowSec, nowMs), platform)
            }
        }
    }

    suspend fun whoamiSession(nowMs: Long): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.userRef(native.whoamiSession(nowMs)) }
    }

    suspend fun detailSession(id: String, nowSec: Long, nowMs: Long): FeedDetail =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toDetail(native.detailSession(id, nowSec, nowMs), platform)
            }
        }

    suspend fun commentsSession(
        id: String,
        cursor: Cursor?,
        sort: CommentSort,
        nowSec: Long,
        nowMs: Long,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(
                native.commentsSession(id, cursor?.value, sort.toFfi(), nowSec, nowMs),
                platform,
            )
        }
    }

    suspend fun repliesSession(
        id: String,
        commentId: String,
        cursor: Cursor?,
        nowSec: Long,
        nowMs: Long,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(
                native.repliesSession(id, commentId, cursor?.value, nowSec, nowMs),
                platform,
            )
        }
    }

    suspend fun playSession(
        id: String,
        partId: String,
        quality: Int?,
        nowMs: Long,
    ): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toPlayInfo(
                native.playSession(id, partId, quality, nowMs),
            )
        }
    }

    suspend fun startDeviceCodeSession(nowMs: Long): RustDeviceCode =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                val code = native.startDeviceCodeSession(nowMs)
                RustDeviceCode(
                    deviceCode = code.deviceCode,
                    userCode = code.userCode,
                    verificationUrl = code.verificationUrl,
                    pollIntervalMs = code.pollIntervalMs,
                    expiresAtMs = code.expiresAtMs,
                    hint = code.hint,
                )
            }
        }

    suspend fun pollDeviceCodeSession(deviceCode: String, nowMs: Long): FfiYoutubePoll =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) { native.pollDeviceCodeSession(deviceCode, nowMs) }
        }

    fun logoutSession() {
        native.logoutSession()
    }

    suspend fun keepSessionFresh(nowMs: Long) = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.keepSessionFresh(nowMs) }
    }

    suspend fun refreshInStore(nowMs: Long): Boolean = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.refreshSessionInStore(nowMs) }
    }

    fun hasSession(nowMs: Long): Boolean = native.hasSession(nowMs)

    /** 调试 `raw` 请求：整套在 Rust 里经 `RustNetStack` 发送（共享连接池 / 限速 / 代理 fail-closed）。 */
    suspend fun raw(request: RawRequest): RawResponse = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val okHttpReq = request.toOkHttpRequest(base)
            val bodyBytes = okHttpReq.body?.let { reqBody ->
                val buffer = okio.Buffer()
                reqBody.writeTo(buffer)
                buffer.readByteArray()
            } ?: ByteArray(0)
            val ffiHeaders = buildList {
                okHttpReq.headers.forEach { (name, value) -> add(FfiHeader(name, value)) }
                val contentType = okHttpReq.body?.contentType()?.toString()
                if (contentType != null && okHttpReq.header("Content-Type") == null) {
                    add(FfiHeader("Content-Type", contentType))
                }
            }
            val ffiReq = FfiHttpRequest(
                method = okHttpReq.method,
                url = okHttpReq.url.toString(),
                headers = ffiHeaders,
                body = bodyBytes,
            )
            val res = native.raw(ffiReq)
            RawResponse(
                code = res.status.toInt(),
                url = if (res.finalUrl.isNotEmpty()) res.finalUrl else okHttpReq.url.toString(),
                headers = res.headers
                    .filterNot { it.name.equals("set-cookie", ignoreCase = true) }
                    .map { it.name to it.value },
                body = res.body.toString(Charsets.UTF_8),
            )
        }
    }
}

/**
 * 一次设备码登录的 Kotlin 侧形状。`deviceCode` 是**机密**：只往 `DeviceCodeLogin` 里放，不打印。
 */
data class RustDeviceCode(
    val deviceCode: String,
    val userCode: String,
    val verificationUrl: String,
    val pollIntervalMs: Long,
    val expiresAtMs: Long?,
    val hint: String,
)

/**
 * 设备码 / 换 token 的四个端点（Kotlin `source-youtube` 的 `LoginUrls` 的那个形状）。
 */
data class RustYoutubeLoginUrls(
    val tvDeviceCode: String = "https://www.youtube.com/o/oauth2/device/code",
    val tvToken: String = "https://www.youtube.com/o/oauth2/token",
    val googleDeviceCode: String = "https://accounts.google.com/o/oauth2/device/code",
    val googleToken: String = "https://www.googleapis.com/oauth2/v4/token",
) {
    internal fun toFfi(): FfiYoutubeLoginUrls = FfiYoutubeLoginUrls(
        tvDeviceCode = tvDeviceCode,
        tvToken = tvToken,
        googleDeviceCode = googleDeviceCode,
        googleToken = googleToken,
    )
}

/** `SearchKind` → `FfiYoutubeSearchKind`。 */
internal fun SearchKind.toFfiYoutube(): FfiYoutubeSearchKind = when (this) {
    SearchKind.ALL -> FfiYoutubeSearchKind.ALL
    SearchKind.VIDEO -> FfiYoutubeSearchKind.VIDEO
    SearchKind.POST -> FfiYoutubeSearchKind.POST
    SearchKind.USER -> FfiYoutubeSearchKind.USER
    SearchKind.TOPIC -> FfiYoutubeSearchKind.TOPIC
}

/** YouTube 的限速（`YoutubeSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object YoutubeRate {
    const val MIN_INTERVAL_MS = 500L
}
