package feedhub.rust

import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.SourceContext
import feedhub.core.UserRef
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.FfiTiktokSource

/**
 * **TikTok 平台对象的 Kotlin 门面**：`FfiTiktokSource`（Rust 里的 `uniffi::Object`）+ 三个宿主回调。
 *
 * 和 [RustTiktok]（纯函数门面）的分工：
 * - [RustTiktok] 只做"给我一段 body、我给你一页"和签名；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数顺序 / 请求头顺序、`X-Dynosaur` / `X-Gnarly` 签名、
 *   翻页游标、首页设备参数的首次获取、状态码 / `tt_orcas_res` 判定，真正的收发回调宿主用 [client] 做
 *   （必须是 `ctx.client(platform)` 派生出来的那个客户端）。
 *
 * **随机数与时间戳从 Kotlin 注入**：[feed] 收 24 个 32 位密钥（两组各 12 个）和毫秒时间戳，
 * Rust 不读时钟、不取随机数，测试才能把两次运行的请求逐字节比。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 */
class RustTiktokHosted(
    ctx: SourceContext,
    base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，
     * 见 [RustNative]），失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiTiktokSource by HostedCall.native {
        FfiTiktokSource.withNet(
            base,
            net.inner,
            net.spec(
                platform = platform,
                minIntervalMs = TiktokRate.MIN_INTERVAL_MS,
                jitterMs = TiktokRate.JITTER_MS,
            ),
            RustStoreHost(ctx.store),
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 登录态 / 账号：没登录时**一个请求都不发**、
     * 直接 `null`；页面里没有账号也 `null`（不编一个）；风控照 [feed] 那套抛。
     */
    suspend fun whoami(): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.userRef(native.whoami())
        }
    }

    /**
     * 一页推荐流。[nowMs] 是毫秒时间戳，[key] / [key2] 是两组各 12 个 32 位签名密钥——
     * 由调用方（[feedhub.source.tiktok.RustTiktokSource]）注入，测试里才有可复现的签名。
     */
    suspend fun feed(cursor: Cursor?, nowMs: Long, key: List<UInt>, key2: List<UInt>): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feed(cursor?.value, nowMs, key, key2), platform)
            }
        }

    /**
     * 详情：视频页 HTML 的 hydration（`/@<作者>/video/<id>`，**不签名**）。
     * id 里没有作者时在 Rust 里**发请求之前**就报 `no_author`。
     */
    suspend fun detail(id: String): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id), platform)
        }
    }

    /** 一级评论（`comment/list/`）。[nowMs] / [key] / [key2] 由调用方注入（和 [feed] 一样）。 */
    suspend fun comments(
        id: String,
        cursor: Cursor?,
        nowMs: Long,
        key: List<UInt>,
        key2: List<UInt>,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(native.comments(id, cursor?.value, nowMs, key, key2), platform)
        }
    }

    /**
     * 播放地址：**每次都重新取一次视频页**（地址带时效签名），不复用详情那份。
     * 视频页 HTML 不签名，所以不需要时间戳 / 密钥。id 里没有作者时报 `no_author`、一个请求都不发。
     */
    suspend fun play(id: String, partId: String, quality: Int?): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toPlayInfo(native.play(id, partId, quality))
        }
    }

    /** 楼中楼（`comment/list/reply/`）。id 里没有作者时报 `no_referer`、一个请求都不发。 */
    suspend fun replies(
        id: String,
        commentId: String,
        cursor: Cursor?,
        nowMs: Long,
        key: List<UInt>,
        key2: List<UInt>,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(native.replies(id, commentId, cursor?.value, nowMs, key, key2), platform)
        }
    }
}

/** TikTok 的限速（`TiktokSource.MIN_INTERVAL_MS` / `MIN_INTERVAL_JITTER_MS`，**只在这一处**）。 */
private object TiktokRate {
    const val MIN_INTERVAL_MS = 1500L
    const val JITTER_MS = 800L
}
