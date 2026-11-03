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
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.toOkHttpRequest
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.FfiHeader
import uniffi.feedhub_ffi.FfiHttpRequest
import uniffi.feedhub_ffi.FfiParam
import uniffi.feedhub_ffi.FfiXFlag
import uniffi.feedhub_ffi.FfiXSource
import uniffi.feedhub_ffi.FfiXSourceConfig
import uniffi.feedhub_ffi.HostTxid

/** `feed()` 的结果：拿到一页，或者"queryId 可能过期了"（调用方换 id 再打一次，照 Kotlin 的重试一次）。 */
sealed interface RustXFeed {
    data class Loaded(val page: Page<FeedItem>) : RustXFeed

    /** 服务端回 404 / 422：调用方扫一遍网页脚本换 queryId 后重试一次。 */
    data object StaleQueryId : RustXFeed
}

/**
 * **X 平台对象的 Kotlin 门面**：`FfiXSource`（Rust 里的 `uniffi::Object`）+ 宿主回调。
 *
 * 和 [RustX]（纯函数门面）的分工：
 * - [RustX] 只做"给我页面 / sign 模块，我给你一个 txid 生成器"；
 * - 这个类让 Rust 管**整个请求**：GraphQL 的 URL / 参数顺序 / features 的字段顺序 / 请求头顺序 /
 *   翻页游标 / 页数账本 / 状态码与 `errors[]` 判定，真正的收发回调宿主用 [client] 做
 *   （必须是 `ctx.client(platform)` 派生出来的那个客户端，拦截器 / 代理 / Cookie / 限速一个不少）。
 *
 * `RustXSource` 把它接到 `XSource` 的那个
 * `XRateLimiter` 上——和 Kotlin 版"发完请求就记录"是同一个时机、同一个实例，所以详情 / 评论那些还走
 * Kotlin 的方法看到的是同一份额度。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来），
 * 所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会把在飞的 OkHttp Call 掐掉。
 */
class RustXHosted(
    ctx: SourceContext,
    private val base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    /** 取页面（`whoami` 的 `x.com/home`）的站点根；测试里指向 MockWebServer。 */
    private val pageOrigin: String,
    private val platform: Platform,
    queryIds: Map<String, String>,
    features: Map<String, Boolean>,
    fieldToggles: Map<String, Boolean>,
    /** **推文详情**那一份 fieldToggles（和 `fieldToggles` 不是一回事）。 */
    detailFieldToggles: Map<String, Boolean>,
    /** **txid**：`(方法, 路径) -> 头值`（拿不到就是 null）。返回 null 时那个头不加。 */
    private val txid: ((String, String) -> String?)? = null,
    private val txidEnabled: Boolean = false,
    private val signChainJson: String? = null,
    private val minIntervalMs: Long = XRate.API_MIN_INTERVAL_MS,
    private val jitterMs: Long = XRate.API_JITTER_MS,
    private val pageIntervalMs: Long = XRate.PAGE_MIN_INTERVAL_MS,
    pageJitterMs: Long = XRate.PAGE_JITTER_MS,
) {

    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiXSource by HostedCall.native {
        val defaultHosts = computeDefaultHeaderHosts(base, pageOrigin)
        val profileJson = ctx.store.get("x.profile")
        val chainJson = signChainJson ?: ctx.store.get("x.sign_chain")
        FfiXSource.withNet(
            // 数据参数收成一个记录
            FfiXSourceConfig(
                base = base,
                pageOrigin = pageOrigin,
                txidEnabled = txidEnabled,
                profileJson = profileJson,
                signChainJson = chainJson,
                queryIds = queryIds.map { (name, value) -> FfiParam(name, value) },
                features = features.map { (name, value) -> FfiXFlag(name, value) },
                fieldToggles = fieldToggles.map { (name, value) -> FfiXFlag(name, value) },
                detailFieldToggles = detailFieldToggles.map { (name, value) -> FfiXFlag(name, value) },
                // ① API：浏览器默认头 + 限速 + **txid**（`after_limit`，睡完了再算）；
                //    登录头与 `x-rate-limit-*` 记账均已在 `feedhub-core/src/x/source.rs` 内部完成。
                apiSpec = net.specWithHooks(
                    platform = platform,
                    defaultHeaders = xBrowserHeaders(),
                    defaultHeaderHosts = defaultHosts,
                    minIntervalMs = minIntervalMs,
                    jitterMs = jitterMs,
                ),
                // ② 页面：页面头 + 页面限速，**没有** auth / txid
                pageSpec = net.spec(
                    platform = platform,
                    defaultHeaders = xPageHeaders(),
                    defaultHeaderHosts = defaultHosts,
                    minIntervalMs = pageIntervalMs,
                    jitterMs = pageJitterMs,
                ),
            ),
            net.inner,
            null,
            // txid（睡完了再算）
            txid?.let { f ->
                object : HostTxid {
                    override fun txid(method: String, path: String): String? = f(method, path)
                }
            },
            null,
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 登录态 / 账号。
     *
     * 没登录时**一个请求都不发**；其余情况**任何失败都是 `null`**（Kotlin 原版是 `runCatching{}` 吞掉，
     * 代理不可用 / 连不上 / 非 2xx 都算"拿不到"，和 [UserRef] 的约定一致）。
     */
    suspend fun whoami(): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.userRef(native.whoami())
        }
    }

    /**
     * 一页。[staleRetry] 为 true 时 404 / 422 回 [RustXFeed.StaleQueryId]（调用方换 queryId 后再用 false 调一次）。
     */
    suspend fun feed(kind: FeedKind, cursor: Cursor?, staleRetry: Boolean): RustXFeed {
        val ffiKind = kind.toFfi()
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                val outcome = native.feed(ffiKind, cursor?.value, staleRetry)
                val page = outcome.page
                if (outcome.staleQueryId || page == null) {
                    RustXFeed.StaleQueryId
                } else {
                    RustXFeed.Loaded(RustModel.toPage(page, platform))
                }
            }
        }
    }

    /**
     * 详情（`TweetDetail` 的 `focalTweetId`）：焦点推文 + 正文（"整条就是视频"时带分 P）。
     *
     * 请求怎么拼（`variables` 的字段顺序、`fieldToggles` 用详情那一份、头顺序）**全在 Rust**；
     * 没登录时 Rust 在发请求之前就返回 `NotLoggedIn`。`x-rate-limit-*` 的记账照旧走
     * [onResponseHeaders]（详情 / 评论和 feed 共用一份额度）。
     */
    suspend fun detail(id: String): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id), platform)
        }
    }

    /** 一级评论（同一条 `TweetDetail`）：焦点推文**之后**的推文才是评论，游标是 `bottom` 那条。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.comments(id, cursor?.value, sort.toFfi()), platform)
            }
        }

    /** 楼中楼：**当前返回空页**（和 Kotlin 的 `XSource.replies` 一致）。 */
    suspend fun replies(id: String, commentId: String): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(native.replies(id, commentId), platform)
        }
    }

    /**
     * 播放地址（`partId = media:<下标>`）：**请求 + variant 选路全在 Rust**（`feedhub-core/src/x/play.rs`
     * + `XHostedSource::play`）。每次都重新拉一次 `TweetDetail`——X 的地址带时效签名，会过期。
     *
     * `partId` 写法不对在发请求之前就报错；没登录时 Rust 一条 GraphQL 都不发（和详情 / 评论同一份判断）。
     */
    suspend fun play(id: String, partId: String): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toPlayInfo(native.play(id, partId)) }
    }

    /** 换一份 queryId（宿主扫完网页脚本后调它）。 */
    suspend fun setQueryIds(queryIds: Map<String, String>) {
        if (queryIds.isEmpty()) return
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                native.setQueryIds(queryIds.map { (name, value) -> FfiParam(name, value) })
            }
        }
    }

    /** raw 请求：整套在 Rust 里跑（先 ensure_txid，相对路径拼 base，记录响应头）。 */
    suspend fun raw(request: RawRequest): RawResponse = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val okHttpReq = request.toOkHttpRequest(base)
            val bodyBytes = okHttpReq.body?.let { reqBody ->
                val buffer = okio.Buffer()
                reqBody.writeTo(buffer)
                buffer.readByteArray()
            } ?: ByteArray(0)
            val ffiHeaders = okHttpReq.headers.map { (name, value) -> FfiHeader(name, value) }
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

    /** 上一次 txid 初始化失败的原因（排障用）。 */
    fun lastTxidError(): String? = native.lastTxidError()

    /** 运行时按需生成 txid（仅对 /i/api/graphql/ 生效）。 */
    fun generateTxid(method: String, path: String, nowMs: Long, randomByte: Int): String? =
        native.generateTxid(method, path, nowMs, randomByte.toUByte())

    /** 扫脚本刷新 queryId。 */
    suspend fun refreshQueryId(kind: FeedKind): String? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.refreshQueryId(kind.toFfi())
        }
    }
}

/** 动态计算默认头允许生效的 host 列表。 */
private fun computeDefaultHeaderHosts(base: String, pageOrigin: String): List<String> {
    val hosts = linkedSetOf("x.com", "twitter.com", "twimg.com")
    runCatching { java.net.URI(base).host }.getOrNull()?.takeIf { !it.isNullOrBlank() }?.let { hosts.add(it) }
    runCatching { java.net.URI(pageOrigin).host }.getOrNull()?.takeIf { !it.isNullOrBlank() }?.let { hosts.add(it) }
    return hosts.toList()
}

/** X 的 **API** 默认头（`XSource.browserHeaders()`，**顺序就是线上顺序**）。 */
private fun xBrowserHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Accept" to "*/*",
    "Accept-Language" to "en-US,en;q=0.9",
    "Referer" to "https://x.com/",
    "Origin" to "https://x.com",
    "x-twitter-active-user" to "yes",
    "x-twitter-client-language" to "en",
    "Sec-Fetch-Dest" to "empty",
    "Sec-Fetch-Mode" to "cors",
    "Sec-Fetch-Site" to "same-origin",
)

/** X 的**页面**默认头（`XSource.pageHeaders()`）。 */
private fun xPageHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Accept" to "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    "Accept-Language" to "en-US,en;q=0.9",
    "Cache-Control" to "no-cache",
    "Referer" to "https://x.com",
    "x-twitter-active-user" to "yes",
    "x-twitter-client-language" to "en",
)

/** X 的限速（`XRatePolicy`，**只在这一处**）。 */
private object XRate {
    /** API：2500ms + 0~800ms 抖动。 */
    const val API_MIN_INTERVAL_MS = 2500L
    const val API_JITTER_MS = 800L
    /** 页面 / 脚本：1000ms + 0~300ms 抖动。 */
    const val PAGE_MIN_INTERVAL_MS = 1000L
    const val PAGE_JITTER_MS = 300L
}
