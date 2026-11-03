package feedhub.source.heybox

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.MinIntervalInterceptor
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UnsupportedFeedException
import feedhub.core.UserAgents
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.fetch
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import feedhub.rust.RustHeybox
import feedhub.rust.RustHeyboxHosted
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * 小黑盒 Source 的 Rust 版：**推荐流的整条请求逻辑也在 Rust 里**（拼 URL / 参数顺序 / 公共参数 /
 * `hkey` 签名 / 请求头顺序 / 翻页游标 / 表情表的取用与缓存 / 状态码与错误码判定），只有真正的收发
 * 回调宿主（`RustHeyboxHosted` + 本类持有的那个 OkHttpClient）。
 *
 * 生产路径 `HeyboxPlugin` 只建 `RustHeyboxSource`；原对拍参照（`HeyboxSource` / `HkeySigner` 等）移入测试源集。
 */
class RustHeyboxSource internal constructor(
    ctx: SourceContext,
    api: String = API,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val nowSec: () -> Long = { nowMs() / 1000 },
    private val nonceFor: (Long) -> String = ::newNonce,
) : VideoSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, API)

    override val platform = HEYBOX
    override val feeds = HeyboxPlugin.capabilities.feeds
    override val login = HeyboxPlugin.login.web
    override val apiBase = api
    override val hosts = HeyboxPlugin.hosts

    /** [api] 的 host（测试里是 MockWebServer 的地址）。给下面那条 [DefaultHeadersInterceptor] 用。 */
    private val baseHost: String = api.toHttpUrl().host

    private val http = ctx.client(platform)
        .addInterceptor(
            DefaultHeadersInterceptor(
                mapOf(
                    "User-Agent" to UserAgents.DESKTOP_CHROME,
                    "Origin" to WEB,
                    "Referer" to "$WEB/",
                    "Accept" to "application/json, text/plain, */*",
                ),
                hostFilter = {
                    it == "xiaoheihe.cn" || it.endsWith(".xiaoheihe.cn") || it == baseHost
                },
            ),
        )
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS))
        .build()

    internal val httpClient get() = http

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(HEYBOX to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(HEYBOX), HeyboxPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(HEYBOX), HeyboxPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustHeyboxHosted(ctx, api, net, HEYBOX, nonceFor)

    /** 搜索能力（和 [HeyboxPlugin] 的 `capabilities.search` 是同一份，`PluginContract` 会比对）。 */
    override val search = HeyboxPlugin.capabilities.search!!

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustHeybox.version()

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    /** 「我是谁」：`/account/restore_login`，请求 + 解析 + 未登录判定都在 Rust。 */
    override suspend fun whoami(): UserRef? {
        val timestamp = nowSec()
        return hosted.whoami(timestamp, nonceFor(timestamp))
    }

    /**
     * 推荐流：**整套请求逻辑在 Rust**（拼 URL / 公共参数 / 头 / 签名 / 游标 / 表情表 / 错误判定）。
     */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> {
            val timestamp = nowSec()
            hosted.feed(cursor, timestamp, nonceFor(timestamp))
        }
        else -> throw UnsupportedFeedException(platform, kind)
    }

    /**
     * 搜一页：**请求怎么拼和结果怎么解析都在 Rust**（`feedhub-core/src/heybox/search.rs`）。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        val timestamp = nowSec()
        return hosted.search(request.text, request.sort, cursor, timestamp, nonceFor(timestamp))
    }

    override suspend fun raw(request: RawRequest): RawResponse {
        var req = request.toOkHttpRequest(apiBase)
        if (request.sign && req.url.host == apiBase.toHttpUrl().host) {
            req = req.newBuilder().url(signNow(req.url)).build()
        }
        return http.fetch(req).toRawResponse()
    }

    private fun signNow(url: HttpUrl): HttpUrl {
        val timestamp = nowSec()
        return signUrl(url, timestamp, nonceFor(timestamp))
    }

    /**
     * 详情：**请求和正文（含 HTML）全在 Rust**（`feedhub-core/src/heybox/detail.rs`）。
     */
    override suspend fun detail(id: String): FeedDetail {
        val timestamp = nowSec()
        return hosted.detail(id, timestamp, nonceFor(timestamp))
    }

    /** 一级评论：同一个 `link/tree`，游标是页码；请求怎么拼、表情表怎么取都在 Rust。 */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        val timestamp = nowSec()
        return hosted.comments(id, cursor, sort, timestamp, nonceFor(timestamp))
    }

    /** 楼中楼：`/bbs/app/comment/sub/comments`，游标是 `lastval`。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        val timestamp = nowSec()
        return hosted.replies(id, commentId, cursor, timestamp, nonceFor(timestamp))
    }

    /**
     * 播放地址：**请求怎么拼 + 解析全在 Rust**（`feedhub-core/src/heybox/play.rs`）。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        val timestamp = nowSec()
        return hosted.play(id, partId, timestamp, nonceFor(timestamp))
    }

    /** 扫码登录：请求 / 签名 / 状态 / Cookie 落地都在 Rust。 */
    override suspend fun startQrLogin(): QrLogin {
        val timestamp = nowSec()
        return hosted.startQrLogin(nowMs(), timestamp, nonceFor(timestamp))
    }

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState {
        val timestamp = nowSec()
        return hosted.pollQrLogin(login, nowMs(), timestamp, nonceFor(timestamp))
    }

    companion object {
        const val API = "https://api.xiaoheihe.cn"
        private const val WEB = "https://www.xiaoheihe.cn"
        private const val MIN_INTERVAL_MS = 600L
        private val random = SecureRandom()
        private val COMMON_PARAMS = listOf(
            "os_type" to "web",
            "app" to "heybox",
            "client_type" to "web",
            "version" to "999.0.4",
            "web_version" to "3.0",
            "x_app" to "heybox_website",
            "x_client_type" to "web",
            "x_os_type" to "Mac",
        )
        private val SIGN_PARAMS = listOf("hkey", "_time", "nonce")

        /** 32 位大写十六进制随机 nonce（宿主 SecureRandom + JDK MD5，与网页 signing.py 一致）。 */
        internal fun newNonce(timestamp: Long): String {
            val bytes = ByteArray(16).also(random::nextBytes)
            val raw = timestamp.toString() + bytes.joinToString("") { "%02x".format(it) }
            return MessageDigest.getInstance("MD5")
                .digest(raw.toByteArray(Charsets.UTF_8))
                .joinToString("") { "%02X".format(it) }
        }

        internal fun signUrl(
            url: HttpUrl,
            timestamp: Long,
            nonce: String = newNonce(timestamp),
        ): HttpUrl {
            val b = url.newBuilder()
            for ((k, v) in COMMON_PARAMS) if (url.queryParameter(k) == null) b.addQueryParameter(k, v)
            SIGN_PARAMS.forEach(b::removeAllQueryParameters)
            RustHeybox.signedParams(url.encodedPath, timestamp, nonce).forEach { (k, v) -> b.addQueryParameter(k, v) }
            return b.build()
        }
    }
}
