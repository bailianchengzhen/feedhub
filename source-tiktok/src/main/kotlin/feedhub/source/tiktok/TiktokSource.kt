package feedhub.source.tiktok

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.HttpResult
import feedhub.core.LoginSpec
import feedhub.core.MinIntervalInterceptor
import feedhub.core.NativeDetailSource
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.SourceContext
import feedhub.core.SourceException
import feedhub.core.UnsupportedFeedException
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.fetch
import feedhub.core.long
import feedhub.core.text
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import kotlinx.serialization.json.JsonObject
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import java.io.IOException

/**
 * TikTok 网页版（www.tiktok.com）。**只读**：推荐流、详情、一级评论、楼中楼、播放地址。
 *
 * ## 状态
 *
 * - 签名：[TiktokSign] 是 f2（Apache-2.0）`xgnarly.py` 的逐行 Kotlin 移植，**3 组对拍向量逐字节一致**
 *   （`TiktokSignTest`）。
 * - 请求构造：query 字段 / 值 / 顺序照 Chrome 153 抓包（TiktokApis 的 `_current_params` +
 *   `api/tiktok_web.py`，catbus 的对拍数据）。**所有参数按 ASCII 键名升序**——这是从抓包 URL 里读出来的
 *   规律（`WebIdLastTime` < `aid` < … < `webcast_language`），`TiktokSourceTest` 拿 catbus 那几条对拍
 *   URL 逐字节比对。
 *
 * ## 接口
 *
 * | 用途 | 请求 |
 * |---|---|
 * | 登录态 + 设备数据 | `GET /`（HTML 的 `__UNIVERSAL_DATA_FOR_REHYDRATION__`） |
 * | 推荐流 | `GET /api/recommend/item_list/`（带签名） |
 * | 详情 / 播放地址 | `GET /@<作者>/video/<id>`（HTML 的 `webapp.video-detail.itemInfo.itemStruct`） |
 * | 一级评论 | `GET /api/comment/list/`（带签名） |
 * | 楼中楼 | `GET /api/comment/list/reply/`（带签名） |
 *
 * 详情为什么走 HTML：参考实现里被两处独立印证（TiktokApis `_video_detail_from_html`、catbus `videoDetail`），
 * 而 `/api/item/detail/`（f2 的 `AWEME_DETAIL`）没有出现在 TiktokApis 的 `CONFIRMED_READ_ENDPOINTS` 里、
 * 参数槽位也不确定。HTML 那条还**不需要签名**，能把"签名层"和"TLS 指纹层"分开看。
 *
 * ## 登录
 *
 * `sessionid` / `sid_tt` / `multi_sids` 任一即算登录。**推荐流要求登录**：没有登录态时
 * 直接抛 [NotLoggedInException]、一个请求都不发。`s_v_web_id`（verifyFp）与 `msToken` 来自导入的 Cookie，
 * `msToken` **绝不伪造**。
 *
 * ## 关注流
 *
 * **不做**：`/api/following/item_list/` 要 `tt-ticket-guard-*` 五个头，没有任何仓库能凭空生成 → [feed] 抛 [UnsupportedFeedException]。
 *
 * ## 风控
 *
 * 三层拒绝（签名环境码 / 伪造 msToken / TLS 指纹）的表现**完全一样**：HTTP 200 + 空 body +
 * 响应头 `tt_orcas_res: 1`。识别到就抛 [RiskControlException] 停下：**不重试、不换参数、不解验证码**。
 * 另有 HTTP 403 / 429、JS 挑战页、Security Check 页、业务码（8 / 10000 / 2483 / `url doesn't match`）。
 *
 * [base] 只给测试用（指向 MockWebServer）；[random] / [clock] 也只给测试用（让签名可复现）。
 */
class TiktokSource internal constructor(
    private val ctx: SourceContext,
    private val base: String,
    private val random: TiktokRandom = TiktokRandom.SYSTEM,
    private val clock: () -> Long = System::currentTimeMillis,
) : NativeDetailSource, VideoSource {
    constructor(ctx: SourceContext) : this(ctx, API)

    // 身份、流、登录、域名都以插件描述为准（[TiktokPlugin]），这里不再写第二份
    override val platform = TIKTOK
    override val feeds = TiktokPlugin.capabilities.feeds
    override val login = TiktokPlugin.login.web
    override val apiBase = base
    override val hosts = TiktokPlugin.hosts

    internal val device = TiktokDevice(ctx.store, ctx.cookies, clock)

    // ctx.client(platform)：套上这个平台的代理配置
    private val http = ctx.client(platform)
        .addInterceptor(
            DefaultHeadersInterceptor(
                browserHeaders(),
                // `nottiktok.com` 这类域名不算；测试里的 MockWebServer 主机名照收
                hostFilter = { isTiktokHost(it) || it == base.toHttpUrl().host },
            ),
        )
        // TikTok 风控严：间隔 1500ms + 0~800ms 抖动，和 X / 小红书一档
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS, MIN_INTERVAL_JITTER_MS))
        // 签名排在限速**之后**：睡完了才算时间戳，签的是 OkHttp 规范化之后要发出去的 query
        .addInterceptor(SignInterceptor())
        .build()

    /**
     * 把平台客户端借给 Rust 用（`RustTiktokSource` 的 `RustTiktokHosted` 必须用它，**不能新建客户端**）：
     * 两份各自派生会把 1500ms 限速拆成两个窗口，风控上更危险。
     */
    internal val httpClient: OkHttpClient get() = http

    // ---------- 登录态 ----------

    /**
     * 有 `sessionid` / `sid_tt` / `multi_sids` 才算登录。没有就**不发请求**返回 null。
     * 账号信息从首页 HTML 的 hydration 里取。
     */
    override suspend fun whoami(): UserRef? {
        if (!loggedIn()) return null
        val scope = fetchHomeScope() ?: return null
        // 页面结构里找不到账号就返回 null（宁可显示"登录已失效"，也不编一个账号）
        return TiktokParser.selfUser(scope)
    }

    private fun loggedIn(): Boolean = LOGIN_COOKIES.any { ctx.cookies.find(WWW, it) != null }

    /**
     * 给 Rust 平台层准备设备指纹：`verifyFp`（= `s_v_web_id`）的**生成留在 Kotlin 的 [TiktokDevice]**
     * （和酷安设备同一条规矩：Rust 只读、不新造），没登录时不生成（和 [recommend] 的判定顺序一致）。
     *
     * 生成出来会落到 `ctx.store` 的 `tiktok.verify_fp`，Rust 那边读的就是这一份。
     */
    internal fun ensureHostedDevice() {
        if (!loggedIn()) return
        device.verifyFp
    }

    /**
     * 给 Rust 平台层准备 `verifyFp`（**评论 / 楼中楼**用）：无条件生成并落到 store，
     * 和 Kotlin 版 `comments` / `replies` 里读 `device.verifyFp` 的行为一样（那两个接口不要求登录）。
     */
    internal fun ensureVerifyFp(): String = device.verifyFp

    // ---------- 信息流 ----------

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> recommend(cursor)
        FeedKind.FOLLOWING -> throw UnsupportedFeedException(platform, kind)
    }

    /**
     * 推荐流：`GET /api/recommend/item_list/`。参数照 TiktokApis 的 `get_recommend_feed`。
     *
     * 游标就是响应里的 `cursor`；`hasMore` 为假结束。
     */
    private suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        if (!loggedIn()) throw NotLoggedInException(platform, "TikTok 推荐流要登录：先在“我的”里导入 TikTok Cookie")
        ensureDevice()
        val params = commonParams(
            queryReferer = "",
            rootReferer = "",
            fromPage = "fyp",
            historyLen = 2,
            appLanguage = device.appLanguage,
            userIsLogin = false,
        ) + mapOf(
            "clientABVersions" to device.clientAbVersions,
            "count" to PAGE_SIZE.toString(),
            "coverFormat" to "2",
            "cpu_core_number" to device.cpuCoreNumber.toString(),
            "dark_mode" to "false",
            "day_of_week" to "0",
            "enable_cache" to "false",
            "isNonPersonalized" to "false",
            "is_new_user" to "true",
            "language" to "zh-Hans",
            "pullType" to "1",
            "showAboutThisAd" to "true",
            "showAds" to "true",
            "time_of_day" to "5",
            "video_encoding" to "dash",
            "vv_count" to "0",
            "vv_count_fyp" to "0",
            "watchLiveLastTime" to clock().toString(),
        ) + (cursor?.value?.let { mapOf("cursor" to it) } ?: emptyMap())
        return TiktokParser.feed(getJson(Get(RECOMMEND_PATH, params, headerReferer = ROOT)), fallbackCursor = clock().toString())
    }

    // ---------- 详情 ----------

    /**
     * 原生详情：视频页 HTML 的 `webapp.video-detail.itemInfo.itemStruct`。
     * id 形态见 [TiktokParser.detailId]（`<作者>/<itemId>`）。
     */
    override suspend fun detail(id: String): FeedDetail {
        // id 里没有作者时**先报错再发请求**（详情页地址必须带作者）
        val uniqueId = requireUniqueId(id)
        ensureDevice()
        val item = fetchVideoItem(id, uniqueId)
        return TiktokParser.detail(item, id)
    }

    // ---------- 评论 ----------

    /**
     * 一级评论：`GET /api/comment/list/`，参数照 TiktokApis 的 `get_comments`（捕获的那个视频页发
     * `app_language=ja-JP`、`fromWeb=1`、`enter_from=tiktok_web`、`history_len=3`）。
     * `cursor` 翻页，`hasMore` 结束；`current_region` 用 [TiktokDevice.region]。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        ensureDevice()
        val itemId = TiktokParser.itemIdOf(id)
        val pageUrl = TiktokParser.uniqueIdOf(id)?.let { videoPageUrl(it, itemId) } ?: DEFAULT_VIDEO_REF
        val params = commonParams(
            queryReferer = pageUrl,
            rootReferer = pageUrl,
            fromPage = "video",
            historyLen = 3,
            appLanguage = "ja-JP",
            userIsLogin = true,
        ) + mapOf(
            "aweme_id" to itemId,
            "count" to COMMENT_PAGE_SIZE.toString(),
            "current_region" to device.region,
            "cursor" to (cursor?.value ?: "0"),
            "enter_from" to "tiktok_web",
            "fromWeb" to "1",
            "is_non_personalized" to "false",
        )
        return TiktokParser.comments(getJson(Get(COMMENT_LIST_PATH, params, headerReferer = pageUrl)))
    }

    /**
     * 楼中楼：`GET /api/comment/list/reply/`。参数照 TiktokApis 的 `get_comment_replies`
     * （`history_len=6`、`cursor` 默认 `0`、`count=3`）；query 里的 `referer` 与 `root_referer`
     * **必须是完整的视频页 URL**（两个参考实现都在本地校验这一点），所以只能从信息流里带作者的入口进来。
     */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        val uniqueId = TiktokParser.uniqueIdOf(id)
            ?: throw ApiException(
                platform,
                "no_referer",
                "楼中楼要完整的视频页地址：请从信息流里打开这条内容（详情 id 形如 `<作者>/<itemId>`）",
            )
        ensureDevice()
        val itemId = TiktokParser.itemIdOf(id)
        val pageUrl = videoPageUrl(uniqueId, itemId)
        val params = commonParams(
            queryReferer = pageUrl,
            rootReferer = pageUrl,
            fromPage = "video",
            historyLen = 6,
            appLanguage = device.appLanguage,
            userIsLogin = true,
        ) + mapOf(
            "comment_id" to commentId,
            "count" to REPLY_PAGE_SIZE.toString(),
            "cursor" to (cursor?.value ?: "0"),
            "item_id" to itemId,
        )
        return TiktokParser.comments(getJson(Get(COMMENT_REPLY_PATH, params, headerReferer = pageUrl)))
    }

    // ---------- 视频 ----------

    /**
     * 播放地址：**每次都重新取一次视频页**（`itemStruct.video.playAddr` / `bitrateInfo` 里的地址带
     * 时效签名，会过期），不复用打开详情时那份（和抖音 / 小黑盒 / 小红书一样）。
     *
     * 拉流要带的头只有 `Referer` + 浏览器 UA：3MH 记的是"只带 UA 和 Range"也能拿到 206（它用的是
     * embed 接口的直链），抖音那边则是"不带 Referer 403"。**这里不留 Cookie 头**：`PlayInfo` 会进日志 /
     * 本机中转，Cookie 不是它该带的东西。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        val uniqueId = requireUniqueId(id)
        ensureDevice()
        val item = fetchVideoItem(id, uniqueId)
        return TiktokVideo.play(
            item,
            partId,
            quality,
            streamHeaders(videoPageUrl(uniqueId, TiktokParser.itemIdOf(id))),
        )
    }

    /** 拉流要带的头：视频页 Referer（没有作者时退回站根）+ 与接口一致的浏览器 UA。 */
    private fun streamHeaders(referer: String): Map<String, String> = linkedMapOf(
        "Referer" to referer,
        "User-Agent" to device.userAgent,
    )

    // ---------- raw ----------

    override suspend fun raw(request: RawRequest): RawResponse {
        val plain = request.toOkHttpRequest(base)
        val builder = plain.newBuilder()
        // 签名交给 [SignInterceptor]；这里只打一个"要签"的标记，sign=false 的不打
        if (request.sign) builder.tag(SignTag::class.java, SignTag())
        val res = http.fetch(builder.build())
        checkTransport(res)
        return res.toRawResponse()
    }

    // ---------- 内部 ----------

    /** [getJson] 的一次调用。[headerReferer] 是请求头 Referer（query 里的 referer 在 params 里）。 */
    private class Get(
        val path: String,
        val params: Map<String, String> = emptyMap(),
        val headerReferer: String,
        val checkStatus: Boolean = true,
    )

    private suspend fun getJson(get: Get): JsonObject {
        val url = urlOf(get.path, get.params)
        val request = Request.Builder()
            .url(url)
            .tag(SignTag::class.java, SignTag())
            .apply { apiHeaders(get.headerReferer).forEach { (k, v) -> header(k, v) } }
            .build()
        val res = http.fetch(request)
        checkTransport(res)
        val json = feedhub.core.parseJsonObject(platform, res.body)
        return if (get.checkStatus) TiktokParser.checkStatus(json) else json
    }

    /** 文档请求（首页 / 视频页）：不带签名，用浏览器的 document 头。 */
    private suspend fun getDocument(path: String, referer: String): HttpResult {
        val request = Request.Builder()
            .url(base.trimEnd('/') + path)
            .apply { documentHeaders(referer).forEach { (k, v) -> header(k, v) } }
            .build()
        val res = http.fetch(request)
        checkTransport(res)
        // 服务端在响应里下发的新 msToken（3MH：`set-cookie` 与 `x-ms-token` 两条路）
        res.headers.firstOrNull { it.first.equals("x-ms-token", ignoreCase = true) }?.second
            ?.let(device::rememberMsToken)
        return res
    }

    /**
     * 首页：`GET /`。一次请求同时办两件事：取 `webapp.app-context` 里的设备数据（wid / odinId /
     * region / webIdCreatedTime，catbus 的 `bootstrap()`），以及让调用方从 hydration 里找当前登录者。
     */
    private suspend fun fetchHomeScope(): JsonObject? {
        bootstrapped = true
        val res = getDocument("/", ROOT)
        val scope = TiktokParser.hydration(res.body) ?: return null
        val context = TiktokParser.appContext(scope)
        device.rememberDevice(
            wid = context?.text("wid"),
            odinId = context?.text("odinId"),
            region = context?.text("region") ?: context?.text("userRegion"),
            webIdCreatedTime = context?.long("webIdCreatedTime"),
        )
        return scope
    }

    /**
     * 探针用：首页的 hydration scope（`whoami` 走的就是它）。**只给 `TiktokProbe` 看 scope 的键用**，
     * 生产路径不要调（会多一个请求）。
     */
    internal suspend fun homeScopeForProbe(): JsonObject? = fetchHomeScope()

    /** 缺 `device_id` 时先补一次首页。 */
    @Volatile
    private var bootstrapped = false

    private suspend fun ensureDevice() {
        if (device.deviceId != null || bootstrapped) return
        fetchHomeScope()
    }

    /** 详情 / 播放的 id 必须带作者（TikTok 的视频页地址就是 `/@<作者>/video/<id>`）。 */
    private fun requireUniqueId(id: String): String = TiktokParser.uniqueIdOf(id)
        ?: throw ApiException(
            platform,
            "no_author",
            "TikTok 的详情页地址必须带作者（`<作者>/<itemId>`），这个 id 里没有：请从信息流里打开这条内容",
        )

    /** 视频页地址（query 的 referer / root_referer / 请求头 Referer / 拉流 Referer 都用它）。 */
    private fun videoPageUrl(uniqueId: String, itemId: String) =
        "https://www.tiktok.com/@$uniqueId/video/$itemId"

    /** 视频页 hydration 里的 itemStruct（详情 / 播放共用）。 */
    private suspend fun fetchVideoItem(id: String, uniqueId: String): JsonObject {
        val path = "/@$uniqueId/video/${TiktokParser.itemIdOf(id)}"
        val res = getDocument(path, base.trimEnd('/') + path)
        val scope = TiktokParser.hydration(res.body)
            ?: throw ParseException(
                platform,
                "视频页里没有 __UNIVERSAL_DATA_FOR_REHYDRATION__（可能是验证页 / 登录墙）",
            )
        return TiktokParser.videoDetail(scope) ?: run {
            val (status, message) = TiktokParser.videoDetailStatus(scope)
            throw ApiException(platform, status?.toString() ?: "no_item", notFoundMessage(status, message, id))
        }
    }

    /**
     * 公共参数（TiktokApis 的 `_current_params`）。顺序由 [urlOf] 统一按 ASCII 键名升序排：
     * 抓包 URL 里就是这个顺序，而顺序是签名输入的一部分（X-Dynosaur 的 `0x2E` 覆盖整串）。
     */
    private fun commonParams(
        queryReferer: String,
        rootReferer: String,
        fromPage: String?,
        historyLen: Int,
        appLanguage: String,
        userIsLogin: Boolean,
    ): Map<String, String> {
        val out = LinkedHashMap<String, String>()
        out["WebIdLastTime"] = device.webIdLastTime.toString()
        out["aid"] = "1988"
        out["app_language"] = appLanguage
        out["app_name"] = "tiktok_web"
        out["browser_language"] = device.browserLanguage
        out["browser_name"] = device.browserName
        out["browser_online"] = "true"
        out["browser_platform"] = device.browserPlatform
        out["browser_version"] = device.browserVersion
        out["channel"] = "tiktok_web"
        out["cookie_enabled"] = "true"
        out["data_collection_enabled"] = "true"
        device.deviceId?.let { out["device_id"] = it }
        out["device_platform"] = "web_pc"
        out["focus_state"] = "true"
        if (fromPage != null) out["from_page"] = fromPage
        out["history_len"] = historyLen.toString()
        out["is_fullscreen"] = "false"
        out["is_page_visible"] = "true"
        device.odinId?.let { out["odinId"] = it }
        out["os"] = device.os
        out["priority_region"] = device.priorityRegion
        out["referer"] = queryReferer
        out["region"] = device.region
        out["root_referer"] = rootReferer
        out["screen_height"] = device.screenHeight.toString()
        out["screen_width"] = device.screenWidth.toString()
        out["tz_name"] = device.tzName
        if (userIsLogin) out["user_is_login"] = "true"
        out["verifyFp"] = device.verifyFp
        out["webcast_language"] = "zh-Hans"
        return out
    }

    /** 还没签名的 URL：业务参数按 RFC 3986 编码、按 ASCII 键名升序。 */
    private fun urlOf(path: String, params: Map<String, String>): HttpUrl {
        val query = TiktokSign.rfc3986Query(params.entries.sortedBy { it.key }.map { it.key to it.value })
        return (base.trimEnd('/') + path + if (query.isEmpty()) "" else "?$query").toHttpUrl()
    }

    /**
     * 签名拦截器：给打了 [SignTag] 的请求补 `X-Dynosaur` / `msToken` / `X-Bogus` / `X-Gnarly`。
     *
     * 签的 query 直接从要发出去的请求里读（[HttpUrl.encodedQuery]，OkHttp 规范化之后），UA 也从请求头里读
     * —— X-Dynosaur 的 `0x30` 是 UA 的 hash_state，两边必须是同一个串。
     */
    private inner class SignInterceptor : Interceptor {
        override fun intercept(chain: Interceptor.Chain): Response {
            val request = chain.request()
            if (request.tag(SignTag::class.java) == null) return chain.proceed(request)
            val body = request.body?.let { b ->
                val buffer = okio.Buffer()
                b.writeTo(buffer)
                buffer.readByteArray()
            } ?: ByteArray(0)
            val signed = TiktokSign.sign(
                query = request.url.encodedQuery.orEmpty(),
                userAgent = request.header("User-Agent") ?: device.userAgent,
                msToken = device.msToken,
                body = body,
                timestamp = clock() / 1000,
                nonce = TiktokSign.clockNonce(clock),
                nonce2 = TiktokSign.clockNonce(clock).inv() and TiktokSign.MASK32,
                sequence = TiktokSign.CALL_SEQUENCE_START,
                key = LongArray(TiktokSign.KEY_WORDS) { random.nextWord() },
                key2 = LongArray(TiktokSign.KEY_WORDS) { random.nextWord() },
            )
            val url = request.url.newBuilder().encodedQuery(signed.query).build()
            return chain.proceed(request.newBuilder().url(url).build())
        }
    }

    /** 每条请求都带的浏览器身份头（和签名里的环境是同一套，见 [TiktokDevice]）。 */
    private fun browserHeaders(): Map<String, String> = linkedMapOf(
        "User-Agent" to device.userAgent,
        "Accept-Language" to device.acceptLanguage,
        "sec-ch-ua" to device.secChUa,
        "sec-ch-ua-mobile" to "?0",
        "sec-ch-ua-platform" to device.uaPlatform,
    )

    /** 接口（XHR）头，照 TiktokApis 的 `HeaderBuilder`（GET / same-origin）。 */
    private fun apiHeaders(referer: String): Map<String, String> = linkedMapOf(
        "Accept" to "*/*",
        "Referer" to referer,
        "priority" to "u=1, i",
        "sec-fetch-dest" to "empty",
        "sec-fetch-mode" to "cors",
        "sec-fetch-site" to "same-origin",
    )

    /** 文档（页面）头，照 TiktokApis 的 `HeaderBuilder`（DOC）。 */
    private fun documentHeaders(referer: String): Map<String, String> = linkedMapOf(
        "Accept" to
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8," +
            "application/signed-exchange;v=b3;q=0.7",
        "Cache-Control" to "no-cache",
        "Pragma" to "no-cache",
        "Referer" to referer,
        "priority" to "u=0, i",
        "sec-fetch-dest" to "document",
        "sec-fetch-mode" to "navigate",
        "sec-fetch-site" to "same-origin",
        "sec-fetch-user" to "?1",
        "Upgrade-Insecure-Requests" to "1",
    )

    // ---------- 风控 / 错误映射 ----------

    /** 传输层的风控信号。识别到就**停下抛异常，不重试、不换参数**。 */
    private fun checkTransport(res: HttpResult) {
        fun header(name: String) = res.headers.firstOrNull { it.first.equals(name, ignoreCase = true) }?.second

        // 三层拒绝（签名环境码 / 伪造 msToken / TLS 指纹）的统一表现：HTTP 200 + 空 body + tt_orcas_res: 1
        if (header("tt_orcas_res") != null) {
            throw RiskControlException(platform, "tt_orcas_res", "Orcas 拒绝了这次请求（签名 / msToken / TLS 指纹三层之一）")
        }
        if (res.code == 403 || res.code == 429) {
            throw RiskControlException(platform, "HTTP ${res.code}", "服务端拒绝（403 / 429；也可能签名不对）")
        }
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(120))
        if (res.body.isEmpty()) {
            throw RiskControlException(platform, "empty-body", "HTTP 200 + 空 body（签名 / msToken / TLS 指纹拒绝的统一表现）")
        }
        if (res.body.contains("__ac_nonce") || res.body.contains("\$jsvmprt")) {
            throw RiskControlException(platform, "acrawler", "拿到 JS 挑战页，不是接口响应")
        }
        if (res.body.contains("<title>Security Check</title>")) {
            throw RiskControlException(platform, "security-check", "拿到 Security Check 页（人机验证）")
        }
    }

    /** 这条请求要不要签名。[raw] 的 `sign = false` 不打这个标记（调试用）。 */
    private class SignTag

    internal companion object {
        const val API = "https://www.tiktok.com"

        /** 站根：推荐流请求头的 Referer，也是播放地址的兜底 Referer。 */
        const val ROOT = "https://www.tiktok.com/"

        /** 评论接口的默认 referer。 */
        const val DEFAULT_VIDEO_REF = "https://www.tiktok.com/@tiktok?lang=zh-Hans"

        val WWW = "https://www.tiktok.com/".toHttpUrl()

        const val RECOMMEND_PATH = "/api/recommend/item_list/"
        const val COMMENT_LIST_PATH = "/api/comment/list/"
        const val COMMENT_REPLY_PATH = "/api/comment/list/reply/"

        /** 抓包的 `count=6`（TiktokApis / catbus 的默认值，浏览器一次就请求这么多）。 */
        const val PAGE_SIZE = 6
        const val COMMENT_PAGE_SIZE = 20
        const val REPLY_PAGE_SIZE = 3

        /** 判定登录态用的 Cookie。 */
        val LOGIN_COOKIES = setOf("sessionid", "sid_tt", "multi_sids")

        const val MIN_INTERVAL_MS = 1500L
        const val MIN_INTERVAL_JITTER_MS = 800L
    }
}

/**
 * [host] 是不是 TikTok 的域名：`tiktok.com` 本身或它的子域。
 * 不能写成 `endsWith("tiktok.com")`——那样 `nottiktok.com` 也会命中。
 */
internal fun isTiktokHost(host: String): Boolean {
    val h = host.lowercase().substringBefore(':')
    return h == "tiktok.com" || h.endsWith(".tiktok.com")
}

/**
 * 业务码 → 异常。`statusCode` 与 `status_code` 用同一套。
 */
internal fun tiktokBusinessError(code: Long, message: String?): SourceException {
    val msg = message?.takeIf { it.isNotBlank() }
    return when (code) {
        8L, 2483L -> NotLoggedInException(TIKTOK, msg ?: "登录已失效")
        10000L, 10001L, 2154L -> RiskControlException(TIKTOK, code.toString(), msg ?: "服务端要求验证码 / 风控")
        else -> if (msg?.contains("url doesn't match", ignoreCase = true) == true) {
            SignatureException(TIKTOK, msg)
        } else {
            ApiException(TIKTOK, code.toString(), msg ?: "接口返回非 0 的业务码")
        }
    }
}

/** 作品不存在 / 不可见时的说明（`statusCode` 10204 / 10216，catbus `videoDetail`）。 */
internal fun notFoundMessage(status: Long?, message: String?, id: String): String = when (status) {
    10204L, 10216L -> "作品不存在或不可见（statusCode=$status${message?.let { "，$it" } ?: ""}）"
    null -> "视频页里没有作品对象：$id"
    else -> "视频页返回 statusCode=$status${message?.let { "（$it）" } ?: ""}"
}
