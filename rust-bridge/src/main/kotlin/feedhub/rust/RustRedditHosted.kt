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
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import feedhub.core.UserRef
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.FfiRedditSource
import uniffi.feedhub_ffi.FfiRedditSearchSort

/**
 * **Reddit 平台对象的 Kotlin 门面**：`FfiRedditSource`（Rust 里的 `uniffi::Object`）+ 两个宿主回调。
 *
 * 和别的平台（B站 / 酷安）不一样的地方：Reddit 当前**没有纯函数那一半**，
 * 解析和请求一起搬的，所以这里没有 `RustReddit` 那种"给我一段 body、我给你一页"的门面，
 * 只有这个平台对象。
 *
 * Rust 管**整个请求**：拼 URL / 参数顺序 / 请求头顺序、翻页游标（`data.after`）、登录态判定、
 * 状态码与风控信号判定；真正的收发回调宿主用 [client] 做（必须是 `ctx.client(platform)` 派生出来的那个）。
 * 登录态只看 `ctx.cookies` 里的 `reddit_session`（**值不读、不打印**），Reddit 这条路上**没有**
 * 设备码 / 时间戳 / 签名要传，所以构造参数里没有 store、也没有 `nowSec`。
 *
 * 走这里的六件事：推荐流（`/best.json`）、关注流（`/new.json`）、**搜索**，
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 */
class RustRedditHosted(
    ctx: SourceContext,
    base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiRedditSource by HostedCall.native {
        // **`withNet` 而不是 `new(base, HostHttp, HostCookies)`**：请求由 `feedhub-net` 在 Rust 里直接发，
        // 一次 FFI 回调都不走（走回调的话，fail-closed 抛的 `ProxyUnavailableException` 会被 UniFFI
        // 包成 `UnexpectedUniFFICallbackError`，那一档的异常类型就丢了）。
        FfiRedditSource.withNet(
            base,
            net.inner,
            // Reddit：桌面 Chrome 的四个头 + 1000ms 限速 + 0~500ms 抖动（`RedditSource.MIN_INTERVAL_MS`）
            net.spec(
                platform = platform,
                defaultHeaders = redditHeaders(),
                minIntervalMs = RedditRate.MIN_INTERVAL_MS,
                jitterMs = RedditRate.JITTER_MS,
            ),
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /**
     * **取消在飞的请求**：`HostCallScope` 在协程被取消时调它。
     *
     * 走 [RustNetStack.cancel]（它记着自己建出来的那些 host，逐个掐在飞的那条）。
     * 为什么不让宿主逐个注册：`withNet` 那条路上**没有 Kotlin 侧的 host 对象**可注册
     * （请求是 Rust 内部直接发的），所以注册点放在栈上。
     */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 登录态 / 账号：没登录 / Cookie 失效时是 `null`
     * （**不发请求**或把 401 / 403+JSON 当"没登录"），风控信号照样抛 [feedhub.core.RiskControlException]。
     */
    suspend fun whoami(): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.userRef(native.whoami())
        }
    }

    /** 推荐流（`/best.json`）/ 关注流（`/new.json`）的一页：**整套请求逻辑在 Rust**。 */
    suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        val ffiKind = kind.toFfi()
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feed(ffiKind, cursor?.value), platform)
            }
        }
    }

    /**
     * 搜索的一页：**请求怎么拼 + 解析都在 Rust**。
     *
     * 游标是 `data.after`（不透明字符串）；[sort] 是 Reddit 的 `sort` 参数（`relevance` / `new` / `hot`）。
     * 未登录时 Rust 在发请求之前就返回 `NotLoggedIn`（宿主一个字节都不发）。
     */
    suspend fun search(text: String, sort: SearchSort, cursor: Cursor?): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.search(text, sort.toFfi(), cursor?.value), platform)
            }
        }

    /**
     * 详情（`/comments/<id>.json`，`limit=1`）：帖子 + 正文（markdown → 富文本）+ 视频分 P。
     * 正文作者头像由 Rust 批量取一次（账号在那边的平台对象里缓存）。
     */
    suspend fun detail(id: String): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id), platform)
        }
    }

    /**
     * 一级评论。游标是 `more:<id1>,<id2>,…`（`null` = 第一页），翻页走 `/api/morechildren`；
     * 每页最多多发一个请求取作者头像（和 Kotlin 版一样）。
     */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.comments(id, cursor?.value, sort.toFfi()), platform)
            }
        }

    /**
     * 楼中楼：`/comments/<postId>/_/<commentId>.json`（[commentId] 以 `more:` 开头时走
     * `/api/morechildren`）。**没有下一页参数**（和 Kotlin 一样）：再往下翻靠 `more:` 那条游标。
     */
    suspend fun replies(id: String, commentId: String): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(native.replies(id, commentId), platform)
        }
    }

    /**
     * 播放地址（`partId = main`）：**请求 + MPD 解析全在 Rust**（`feedhub-core/src/reddit/play.rs`）。
     *
     * 回退链和 Kotlin 一样：清单取得到就 DASH，取不到就 `hls_url`，再不行 `fallback_url`。
     * 清单那一个请求失败**不算错误**（它就是回退链的一环）。
     */
    suspend fun play(id: String, partId: String): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toPlayInfo(native.play(id, partId)) }
    }

    private fun SearchSort.toFfi(): FfiRedditSearchSort = when (this) {
        SearchSort.RELEVANCE -> FfiRedditSearchSort.RELEVANCE
        SearchSort.LATEST -> FfiRedditSearchSort.LATEST
        SearchSort.HOT -> FfiRedditSearchSort.HOT
    }
}

/**
 * Reddit 的请求头（`RedditSource.headers()` 那张表，**顺序就是线上顺序**）。
 *
 * 和 `feedhub-core/src/reddit/source.rs` 的 `request_headers()` **逐字一样**（那边是平台层自己写全的
 * 那一份；这边给"只写了 `Accept` 的页面 / 清单请求"兜底）。UA 和导入 Cookie 的那个浏览器一致。
 */
private fun redditHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Accept" to "application/json, text/plain, */*",
    "Accept-Language" to "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7",
    "Referer" to "https://www.reddit.com/",
)

/** Reddit 的限速参数（`RedditSource.MIN_INTERVAL_MS` / `JITTER_MS`，**只在这一处**）。 */
private object RedditRate {
    const val MIN_INTERVAL_MS = 1000L
    const val JITTER_MS = 500L
}
