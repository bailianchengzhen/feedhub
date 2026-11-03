package feedhub.source.zhihu

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.MinIntervalInterceptor
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
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
import feedhub.rust.RustZhihu
import feedhub.rust.RustZhihuHosted
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okio.Buffer
import kotlin.random.Random

/**
 * 知乎 Source 的 Rust 版：**推荐流 / 关注动态连"请求怎么发"也在 Rust 里**（URL / 参数顺序 / 请求头 /
 * `x-zse-96` 签名 / 没 `d_c0` 时先 `POST /udid` / 翻页游标 / 状态码与 `Location` 的判定），只有真正的收发回调宿主。
 *
 * 生产路径 `ZhihuPlugin` 只建 `RustZhihuSource`；原对拍参照（`ZhihuSource` / `ZseSigner` 等）移入测试源集。
 */
class RustZhihuSource internal constructor(
    private val ctx: SourceContext,
    base: String = WWW,
    private val seed: () -> Int = { Random.nextInt(127) },
    /** 扫码登录用的毫秒时间戳（二维码有效期）。测试里和 [seed] 一起注入。 */
    private val nowMs: () -> Long = { System.currentTimeMillis() },
    /** 视频地址接口的根（测试指向 MockWebServer；和 `ZhihuSource(lensBase = …)` 同一个参数）。 */
    internal val lensBase: String = LENS,
    /**
     * 回答 / 问题用**问题版面**。
     * 生产（[ZhihuPlugin] 用的那个构造）开着；对拍参照测试比的是原来的"回答详情 + 回答评论"，那边关着。
     */
    internal val questionView: Boolean = false,
) : VideoSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, WWW, questionView = true)

    private val baseUrl = "$base/".toHttpUrl()
    internal val cookieDomain: String = baseUrl.topPrivateDomain() ?: baseUrl.host

    override val platform = ZHIHU
    override val feeds = ZhihuPlugin.capabilities.feeds
    override val login = ZhihuPlugin.login.web
    override val apiBase = base
    override val hosts = ZhihuPlugin.hosts
    override val search = ZhihuPlugin.capabilities.search!!

    private val http = ctx.client(platform)
        .followRedirects(false)
        .followSslRedirects(false)
        .addInterceptor(
            DefaultHeadersInterceptor(
                mapOf(
                    "User-Agent" to UserAgents.DESKTOP_CHROME,
                    "Referer" to "https://www.zhihu.com/",
                    "Accept" to "*/*",
                    "Accept-Language" to "zh-CN,zh;q=0.9",
                    "x-api-version" to "3.0.91",
                    "x-app-za" to "OS=Web",
                ),
                hostFilter = { it == "zhihu.com" || it.endsWith(".zhihu.com") || it == baseUrl.host },
            ),
        )
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS))
        .build()

    internal val httpClient get() = http

    private val lensClient = ctx.client(platform)
        .cookieJar(okhttp3.CookieJar.NO_COOKIES)
        .addInterceptor(DefaultHeadersInterceptor(mapOf("User-Agent" to UserAgents.DESKTOP_CHROME), hostFilter = { true }))
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS))
        .build()

    internal val lensHttp get() = lensClient

    private val dc0Mutex = Mutex()

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(ZHIHU to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(ZHIHU), ZhihuPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(ZHIHU), ZhihuPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustZhihuHosted(
        ctx,
        base,
        net,
        ZHIHU,
        ZhihuSticker.urls,
        cookieDomain = cookieDomain,
        lensClient = lensHttp,
        lensBase = lensBase,
        questionCards = questionView,
    )

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustZhihu.version()

    /** 「我是谁」：没有 `z_c0` 时一个请求都不发（和 Kotlin 版同一行）。 */
    override suspend fun whoami(): UserRef? = hosted.whoami(seed())

    /** 推荐流 / 关注流：**整套请求逻辑在 Rust**（拼 URL / 签名 / 游标 / `d_c0` 的领取 / 错误判定）。 */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        if (kind !in feeds) throw UnsupportedFeedException(platform, kind)
        return hosted.feed(kind, cursor, seed())
    }

    override suspend fun raw(request: RawRequest): RawResponse {
        var req = request.toOkHttpRequest(apiBase)
        if (request.sign && req.url.host == HOST) {
            ensureDc0()
            req = sign(req)
        }
        return http.fetch(req).toRawResponse()
    }

    private fun sign(req: Request, dc0: String? = ctx.cookies.find(req.url, "d_c0")): Request {
        if (dc0 == null) return req
        val url = req.url
        val pathAndQuery = url.encodedPath + (url.encodedQuery?.let { "?$it" } ?: "")
        val body = req.body?.let { b -> Buffer().also(b::writeTo).readUtf8() }?.takeIf { it.isNotEmpty() }
        return req.newBuilder()
            .header("x-zse-93", ZSE93)
            .header("x-zse-96", RustZhihu.zse96(pathAndQuery, dc0, body, seed()))
            .header("x-requested-with", "fetch")
            .build()
    }

    private suspend fun ensureDc0() = dc0Mutex.withLock {
        if (ctx.cookies.find(baseUrl, "d_c0") != null) return@withLock
        val req = Request.Builder()
            .url("$apiBase/udid".toHttpUrl())
            .post("{}".toRequestBody("application/json;charset=UTF-8".toMediaType()))
            .header("Origin", apiBase)
            .header("x-requested-with", "fetch")
            .apply { ctx.cookies.find(baseUrl, "_xsrf")?.let { header("x-xsrftoken", it) } }
            .build()
        val res = http.fetch(req)
        val location = res.headers.firstOrNull { it.first.equals("location", true) }?.second
        val redirectedRisk = location?.let { "/account/unhuman" in it || "/account/risk_control" in it } == true
        if (redirectedRisk || res.code !in 200..299 || "\"error\"" in res.body) {
            try {
                RustZhihu.parseMoments(res.body, res.code, location, emptyMap(), platform)
            } catch (_: NotLoggedInException) {
                // 和原版一致：没领到 d_c0 就算了，风控/签名/接口错误照常抛
            } catch (_: ParseException) {
                // 2xx 且非错误体的响应（如空 body）不要求是合法卡片页 JSON
            }
        }
    }

    /** 详情：**整套请求在 Rust**（`/api/v4/{集合}/{id}?include=…`、签名、错误判定、解析）。 */
    override suspend fun detail(id: String): FeedDetail =
        if (questionView) hosted.qaDetail(id, seed()) else hosted.detail(id, seed())

    /** 一级评论：`order_by`、游标、表情表的取用都在 Rust。 */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        if (questionView) hosted.qaAnswers(id, cursor, sort, seed()) else hosted.comments(id, cursor, sort, seed())

    /** 楼中楼：同上（`commentId` 是一级评论的 id）。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        if (questionView) hosted.qaReplies(commentId, cursor, seed()) else hosted.replies(id, commentId, cursor, seed())

    /** 播放地址：**请求 + 解析全在 Rust**（匿名 lens 接口，带签名的地址一小时过期，每次都现取）。 */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(partId)

    /** 扫码登录：请求 / 签名 / 状态 / Cookie 落地都在 Rust。 */
    override suspend fun startQrLogin(): QrLogin = hosted.startQrLogin(nowMs(), seed())

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = hosted.pollQrLogin(login, nowMs(), seed())

    /**
     * 搜一页：**整套请求 + 解析在 Rust**（`/api/v4/search_v3`、`x-zse-96`、第一页固定参数 /
     * 后面几页把 `paging.next` 的 query 原样接上、`is_end` 判断、`<em>` 高亮清理）。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> =
        hosted.search(request, cursor, search.pageSize, seed())

    companion object {
        const val HOST = "www.zhihu.com"
        const val WWW = "https://$HOST"
        const val LENS = "https://lens.zhihu.com"
        private const val ZSE93 = "101_3_3.0"
        private const val MIN_INTERVAL_MS = 600L
    }
}
