package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.FeedCookieJar
import feedhub.core.HttpResult
import feedhub.core.KeyValueStore
import feedhub.core.NotLoggedInException
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RateLimitWait
import feedhub.core.RequestPriority
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.currentRateLimitWait
import feedhub.core.fetch
import feedhub.core.inBackgroundRequestScope
import feedhub.core.long
import feedhub.core.parseJsonObject
import feedhub.core.parseQrJson
import feedhub.core.text
import kotlinx.serialization.json.JsonObject
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.IOException
import java.util.concurrent.TimeUnit

/**
 * 小红书网页接口（`edith.xiaohongshu.com`）的 HTTP 层：请求头、限速、签名、响应检查。
 *
 * - 签名在**拦截器**里做，排在限速拦截器之后：限速 sleep 完了再取时间戳，X-t 和实际发出的时间一致；
 *   签名的内容直接从要发出去的请求里读（路径、查询串、请求体），不会和实际发送的不一致。
 * - 限速：[MIN_INTERVAL_MS] + 随机抖动 [JITTER_MS]（风控严，参考项目的经验值是详情 ≤5 次/分钟，评论更严，这里的间隔只是底线，
 *   评论只在用户打开时才拉，见 [XhsSource]）。
 * - 风控（HTTP 461 / 471 验证码，业务码 300011 账号限制、300012 IP 限制）一律立刻抛 [RiskControlException]，**绝不重试**。
 *
 * [apiBase] 只给测试用（指向 MockWebServer）。
 */
internal class XhsApi(
    private val cookies: FeedCookieJar,
    private val store: KeyValueStore,
    baseClient: OkHttpClient,
    val apiBase: String,
    private val profile: XhsProfile,
    /** 解析 / 签名走哪一边（Kotlin 参照实现 / Rust 核心）。HTTP 这一层两条路完全一样。 */
    private val parse: XhsParse,
    minIntervalMs: Long = MIN_INTERVAL_MS,
    jitterMs: Long = JITTER_MS,
    /** 当前时间（毫秒）。签名里的 `X-t`、x3 的时间戳、b1 都用它；测试里固定。 */
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private val platform = XHS
    private val apiHost = apiBase.toHttpUrl().host

    /**
     * 真正的客户端（默认头 / 限速 / 签名拦截器都在上面）。
     *
     * `internal` 是因为搬进 Rust 的那几条流要**和这里共用同一个客户端**（`RustHttpHost` 用它发）：
     * 各自 `newBuilder()` 一份的话 `MinIntervalInterceptor` 会变成两个实例，1.5 秒窗口被拆成两个。
     */
    internal val http: OkHttpClient = baseClient.newBuilder()
        .readTimeout(30, TimeUnit.SECONDS)
        .addInterceptor(defaultHeaders())
        .addInterceptor(feedhub.core.MinIntervalInterceptor(minIntervalMs, jitterMs))
        .addInterceptor(SignInterceptor())
        .build()

    // ---------- 会话状态 ----------

    /** 导入的 Cookie 里有没有 a1（签名要用）。 */
    fun hasSession(): Boolean = !cookie("a1").isNullOrBlank()

    fun hasLogin(): Boolean = hasSession() && !cookie("web_session").isNullOrBlank()

    private fun cookie(name: String): String? = cookies.find(WEB_URL, name)

    /** 签名要的 `a1`（Rust 侧算 `xy-direction` 的种子时也要看它）。 */
    internal fun a1(): String? = cookie("a1")

    /** 当前登录账号的 user_id（`whoami` 取到后存下来，算 xy-direction 用）。 */
    var userId: String?
        get() = store.get(KEY_USER_ID)
        set(value) = store.put(KEY_USER_ID, value)

    /** b1 一次生成后一直用（网页存在 localStorage 里），存进 [KeyValueStore]。 */
    private fun b1(loadtsMs: Long): String {
        store.get(KEY_B1)?.takeIf { it.length > 200 }?.let { return it }
        return parse.newB1(nowMs(), loadtsMs).also { store.put(KEY_B1, it) }
    }

    // ---------- 请求 ----------

    /** GET [path]（可带已经编好码的查询串，也可以用 [params]）。返回 `data` 外面的整个 JSON 对象，已检查业务码。 */
    suspend fun get(
        path: String,
        params: List<Pair<String, String>> = emptyList(),
        sign: Boolean = true,
        headers: Map<String, String> = emptyMap(),
        priority: RequestPriority = RequestPriority.BACKGROUND,
    ): JsonObject = checkResponse(getRaw(path, params, sign, headers, priority))

    /**
     * 扫码登录用的 GET / POST：和 [get] / [post] 一样（限速、签名、风控映射），
     * 只是**出错信息里不带响应内容**（成功的响应里有 `login_info.session` 这种凭据，见 [parseQrJson]）。
     */
    suspend fun getQr(path: String, params: List<Pair<String, String>>, headers: Map<String, String> = emptyMap()): JsonObject =
        checkResponse(getRaw(path, params, sign = true, headers = headers), secret = true)

    /** POST [path]，[body] 是紧凑 JSON（签名和实际发出的是同一串）。 */
    suspend fun post(
        path: String,
        body: String,
        sign: Boolean = true,
        priority: RequestPriority = RequestPriority.BACKGROUND,
    ): JsonObject = checkResponse(postRaw(path, body, sign, priority))

    /**
     * 和 [get] 一样发请求，但**不检查状态码 / 业务码**，把原始响应交出去。
     * 搬到 Rust 的那几条（推荐流 / 关注流）要让 Rust 一次做完"映射错误 + 解析"，所以需要原始 body。
     *
     * [priority] 默认 [RequestPriority.BACKGROUND]：用户正盯着结果等的那几个（详情 / 评论 / 楼中楼 / 播放）
     * 传 [RequestPriority.INTERACTIVE]，限速器就把它们插到刷新 / 预取 / 表情表前面（见 [feedhub.core.MinIntervalInterceptor]）。
     * 在 [feedhub.core.backgroundRequestScope] 里调时一律按后台排（沉浸预取的详情 / 播放地址走这条）。
     */
    suspend fun getRaw(
        path: String,
        params: List<Pair<String, String>> = emptyList(),
        sign: Boolean = true,
        headers: Map<String, String> = emptyMap(),
        priority: RequestPriority = RequestPriority.BACKGROUND,
    ): HttpResult {
        val url = buildUrl(path, encodeQuery(params))
        val b = Request.Builder().url(url).get().apply {
            if (sign) tag(SignTag::class.java, SignTag)
            headers.forEach { (k, v) -> header(k, v) }
            tagPriority(priority)
        }
        return fetchRaw(b.build())
    }

    /** 和 [post] 一样发请求，但不检查响应（见 [getRaw]）。 */
    suspend fun postRaw(
        path: String,
        body: String,
        sign: Boolean = true,
        priority: RequestPriority = RequestPriority.BACKGROUND,
    ): HttpResult {
        val req = Request.Builder().url(buildUrl(path, null))
            .post(body.toRequestBody(JSON_TYPE))
            .apply {
                if (sign) tag(SignTag::class.java, SignTag)
                tagPriority(priority)
            }
            .build()
        return fetchRaw(req)
    }

    /** 扫码登录用的 POST，见 [getQr]。 */
    suspend fun postQr(path: String, body: String): JsonObject =
        checkResponse(postRaw(path, body, sign = true), secret = true)

    /** 调试用（cli `raw`）：发任意请求，不检查业务码。[sign] 为 true 时加签名。 */
    suspend fun rawRequest(request: Request, sign: Boolean): HttpResult =
        http.fetch(if (sign) request.newBuilder().tag(SignTag::class.java, SignTag).build() else request)

    private suspend fun fetchRaw(request: Request): HttpResult {
        if (request.tag(SignTag::class.java) != null && !hasSession()) {
            throw NotLoggedInException(platform, "没有小红书的 a1 Cookie，先导入登录态")
        }
        val b = request.newBuilder()
        // 沉浸预取那种后台作用域里的请求一律按后台排队（覆盖 detail / play 给的默认）
        if (inBackgroundRequestScope()) b.tag(RequestPriority::class.java, RequestPriority.BACKGROUND)
        // 限速器把"在队列里睡了多久"记到这里（性能记录的 queue_ms，见 RateLimitWait）
        currentRateLimitWait()?.let { b.tag(RateLimitWait::class.java, it) }
        return http.fetch(b.build())
    }

    /** 只有用户正等着的请求才打 tag（没 tag = 后台，限速器和加优先级之前一样按先来后到）。 */
    private fun Request.Builder.tagPriority(priority: RequestPriority) = apply {
        if (priority == RequestPriority.INTERACTIVE) tag(RequestPriority::class.java, priority)
    }

    private fun buildUrl(path: String, encodedQuery: String?): HttpUrl =
        apiBase.toHttpUrl().newBuilder().encodedPath(path).apply { if (!encodedQuery.isNullOrEmpty()) encodedQuery(encodedQuery) }.build()

    // ---------- 响应 ----------

    companion object {
        const val WEB = "https://www.xiaohongshu.com"
        const val WEB_URL = "$WEB/"
        const val API = "https://edith.xiaohongshu.com"
        const val MIN_INTERVAL_MS = 1500L
        const val JITTER_MS = 800L
        const val KEY_USER_ID = "xhs.user_id"
        const val KEY_B1 = "xhs.b1"
        private val JSON_TYPE = "application/json;charset=UTF-8".toMediaType()

        /** 「当前笔记暂时无法浏览」：xsec_token 无效 / 过期、笔记被删或仅自己可见。 */
        const val NOTE_UNAVAILABLE = 300031L

        internal fun noteUnavailable(p: Platform) = ApiException(
            p, NOTE_UNAVAILABLE.toString(),
            "当前笔记暂时无法浏览：入口里的 xsec_token 失效了（很久以前收藏的会这样），或者笔记被删除 / 仅自己可见。请从信息流重新打开",
        )

        /**
         * 状态码 + 业务码一起判：461 / 471 里 `300031`（笔记打不开）单独区分，406 / 416 当签名问题，401 当未登录；
         * 其余状态码先看业务码、再退回"HTTP n"。返回整个 JSON 对象（成功时）。
         */
        internal fun checkResponse(res: HttpResult, secret: Boolean = false): JsonObject {
            when (res.code) {
                in 200..299 -> Unit
                461, 471 -> {
                    // 无效 / 过期的 xsec_token 也会回 HTTP 461，但 body 是业务码 300031「当前笔记暂时无法浏览」，
                    // 不是验证码，别让用户以为账号被风控了；别的 461 / 471 都按验证码处理
                    if (bizCode(res.body) == NOTE_UNAVAILABLE) throw noteUnavailable(XHS)
                    throw RiskControlException(XHS, "HTTP ${res.code}", "需要验证码，先别请求了，到网页版里过一下验证")
                }
                406 -> throw SignatureException(XHS, "HTTP 406（多半是签名格式过期，也可能是 TLS 指纹被拒）")
                416 -> throw SignatureException(XHS, "HTTP 416（签名错误；网页上登录的账号可能被踢下线）")
                401 -> throw NotLoggedInException(XHS)
                else -> {
                    bizError(res.body)?.let { throw it }
                    throw if (secret) {
                        ApiException(XHS, "HTTP ${res.code}", "扫码登录接口回 HTTP ${res.code}")
                    } else {
                        ApiException(XHS, "HTTP ${res.code}", res.body.take(120))
                    }
                }
            }
            val json = if (secret) parseQrJson(XHS, res.body) else parseJsonObject(XHS, res.body)
            return checkBiz(json)
        }

        private fun bizCode(body: String): Long? = runCatching { parseJsonObject(XHS, body).long("code") }.getOrNull()

        private fun bizError(body: String): Exception? = try {
            checkBiz(parseJsonObject(XHS, body))
            null
        } catch (e: Exception) {
            e
        }

        /** 业务码：`{code, success, msg, data}`。 */
        internal fun checkBiz(json: JsonObject): JsonObject {
            val p = XHS
            val code = json.long("code") ?: return json // 个别接口（redmoji）没有 code，按成功算
            val msg = json.text("msg")?.take(100) ?: ""
            return when (code) {
                0L -> json
                -100L, -101L -> throw NotLoggedInException(p, "登录已失效（$code）")
                300011L -> throw RiskControlException(p, "300011", "账号被限制，停止一切请求，先到网页版看看有没有验证或提示")
                300012L -> throw RiskControlException(p, "300012", "当前 IP 被限制，换网络或等一阵")
                300015L -> throw SignatureException(p, "300015 $msg")
                NOTE_UNAVAILABLE -> throw noteUnavailable(p)
                else -> throw ApiException(p, code.toString(), msg)
            }
        }

        /**
         * 查询串编码：值按 `encodeURIComponent` 编码，但逗号保持原样（网页的 `image_formats=jpg,webp,avif`），
         * 键值对按给定顺序。签名用的就是这一串。
         */
        fun encodeQuery(params: List<Pair<String, String>>): String =
            params.joinToString("&") { (k, v) -> encodeComponent(k) + "=" + encodeComponent(v) }

        internal fun encodeComponent(s: String): String {
            val sb = StringBuilder()
            for (b in s.toByteArray(Charsets.UTF_8)) {
                val c = b.toInt() and 0xFF
                if (c < 0x80 && (c.toChar().isLetterOrDigit() || c.toChar() in "-_.!~*'(),")) sb.append(c.toChar())
                else sb.append('%').append("0123456789ABCDEF"[c shr 4]).append("0123456789ABCDEF"[c and 15])
            }
            return sb.toString()
        }

        /** 这些接口要带 `x-rap-param`（浏览器实抓）。 */
        internal fun needsRap(path: String): Boolean = path in RAP_PATHS

        /** 只有 homefeed 和 feed 带 `xy-direction`。 */
        internal fun needsXy(path: String): Boolean = path in XY_PATHS

        private val RAP_PATHS = setOf(
            "/api/sns/web/v1/homefeed",
            "/api/sns/web/v1/feed",
            "/api/sns/web/v1/user_posted",
            "/api/sns/web/v1/search/notes",
            "/api/sns/web/v2/search/notes",
        )
        private val XY_PATHS = setOf("/api/sns/web/v1/homefeed", "/api/sns/web/v1/feed")
        private val NO_CACHE_PATHS = setOf("/api/sns/web/v2/user/me", "/api/sns/web/v1/config", "/api/sns/web/v1/system/config")
    }

    // ---------- 拦截器 ----------

    private object SignTag

    private fun defaultHeaders() = Interceptor { chain ->
        val r = chain.request()
        val b = r.newBuilder()
        if (r.url.host == apiHost) {
            fun set(k: String, v: String) {
                if (r.header(k) == null) b.header(k, v)
            }
            set("User-Agent", profile.release.userAgent)
            set("Accept", "application/json, text/plain, */*")
            set("Accept-Language", profile.release.acceptLanguage)
            set("Origin", WEB)
            set("Referer", WEB_URL)
            set("sec-ch-ua", profile.release.secChUa)
            set("sec-ch-ua-mobile", "?0")
            set("sec-ch-ua-platform", "\"${profile.release.platform}\"")
            set("sec-fetch-dest", "empty")
            set("sec-fetch-mode", "cors")
            set("sec-fetch-site", "same-site")
            set("priority", "u=1, i")
            if (r.url.encodedPath in NO_CACHE_PATHS) {
                set("cache-control", "no-cache")
                set("pragma", "no-cache")
            }
        }
        chain.proceed(b.build())
    }

    private inner class SignInterceptor : Interceptor {
        override fun intercept(chain: Interceptor.Chain): Response {
            val r = chain.request()
            if (r.tag(SignTag::class.java) == null) return chain.proceed(r)
            val a1 = cookie("a1")?.takeIf { it.isNotBlank() } ?: throw IOException("没有 a1 Cookie")
            val loadts = cookie("loadts")?.toLongOrNull() ?: (nowMs() - 10 * 60_000L)
            val path = r.url.encodedPath
            val api = path + (r.url.encodedQuery?.let { "?$it" } ?: "")
            val body = if (r.method == "POST") {
                val buf = okio.Buffer()
                r.body?.writeTo(buf)
                buf.readUtf8()
            } else {
                null
            }
            val headers = parse.sign(
                XhsSigner.Request(
                    api = api,
                    body = body,
                    a1 = a1,
                    loadtsMs = loadts,
                    webBuild = cookie("webBuild")?.takeIf { it.isNotBlank() } ?: profile.release.webBuild,
                    b1 = b1(loadts),
                    userId = userId,
                    rap = needsRap(path),
                    xy = needsXy(path),
                ),
                nowMs(),
            )
            val b = r.newBuilder()
            headers.forEach { (k, v) -> b.header(k, v) }
            return chain.proceed(b.build())
        }
    }
}
