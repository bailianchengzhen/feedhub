package feedhub.source.coolapk

import feedhub.core.ApiException
import feedhub.core.CaptchaChallenge
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
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RiskControlException
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SignatureException
import feedhub.core.SourceContext
import feedhub.core.SourceException
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.fetch
import feedhub.core.long
import feedhub.core.parseJsonObject
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request

/**
 * 酷安：官方 App 协议（`api2.coolapk.com` / `api.coolapk.com` 的 `/v6/...`），纯原生，所以除了信息流还实现了详情、评论、楼中楼。
 *
 * **两套客户端身份**：
 * - 信息流 / 详情 / 关注流继续用 **13.4.1 / 2312121 / API 13 + v2 token**（[AppVersion]）。这两条接口在这套身份下
 *   一直实测正常；换成新版本号反而会让详情接口撞验证码。
 * - 评论接口 `/v6/feed/replyList` 只认新客户端：**16.2.0 / 2604201 / API 16 + v3 token**，并且走 **api.coolapk.com**
 *   （不是 api2）。
 *   新身份能到验证码这一步；**一次人工验证（validate cookie）之后，服务端认多久就能读多久**
 *   （cookie 我们存 30 天；服务端提前失效会再弹一次验证，见 [CoolapkCaptcha] 的注释）。
 * - 设备（设备码、机型）和"安装时间"第一次用时生成，存进 `ctx.store`，以后不变。
 * - **解析和 `X-App-Token` 走 [CoolapkParse]**（Rust 版是默认的那一个，见 `RustCoolapkSource`）：
 *   网络 / 请求头 / 参数顺序 / Cookie / 代理 / 限速一个字节没动，只是响应 body 和 token 计算换了实现。
 * 真正的收发还是回调这个类的 [httpClient]（同一份
 *   OkHttp）。所以 [feed] 现在是**参照实现**，线上走的是 `RustCoolapkSource`。
 *   线上走 `RustCoolapkSource`）：请求在 `coolapk/source.rs`，解析在 `coolapk/detail.rs` / `coolapk/comment.rs`，
 *   **人机验证仍然只由用户本人做**（Rust 只负责认出 `err_request_captcha_v2` 并把挑战带回来）。
 *   所以现在只剩 `raw`还有这一份 Kotlin 实现。
 *   **扫码这一家从来没有**：酷安的网页登录只有密码 / 短信 / QQ + 网易易盾验证码，没有扫码入口
 *   （`CoolapkPlugin.login.noQrHint` 会告诉 cli 用户去粘贴 Cookie），所以它不实现 `QrLoginSource`。
 * - 登录态就是 `.coolapk.com` 下的 `uid` / `username` / `token` 三个 Cookie，由 Cookie 罐自动带上。
 * - 接口、参数照 c001apk-flutter（AGPL-3.0）、Coolapk-UWP（GPL-3.0）和 coolapk-desktop（MIT）。
 */
class CoolapkSource internal constructor(
    private val ctx: SourceContext,
    /** 接口根地址。默认线上；测试里指向 MockWebServer。 */
    apiBaseUrl: String = API,
    /** 评论走的那条接口在 api（不是 api2）上，见 [comments]。 */
    private val commentsBaseUrl: String = COMMENTS_API,
    /**
     * 解析和 `X-App-Token` 交给谁：默认是 Kotlin 自己（[KotlinCoolapkParse]），Rust 版走 [RustCoolapkParse]。
     * 请求构造、Cookie、代理、限速、请求头两条路完全一样（`CoolapkRequestEquivalenceTest` 守着）。
     */
    private val parse: CoolapkParse = KotlinCoolapkParse,
    /** 秒级时间戳从哪来（token 只取决于设备码 + 秒），测试里注入固定值才能逐字节比请求。 */
    private val nowSec: () -> Long = { System.currentTimeMillis() / 1000 },
): VideoSource, SearchSource {
    // 身份、流、登录、域名都以插件描述为准（[CoolapkPlugin]），这里不再写第二份
    override val platform = COOLAPK
    override val feeds = CoolapkPlugin.capabilities.feeds
    override val login = CoolapkPlugin.login.web
    override val apiBase = apiBaseUrl
    override val hosts = CoolapkPlugin.hosts
    override val search = CoolapkPlugin.capabilities.search!!

    private val device = CoolapkDevice.loadOrCreate(ctx.store)

    /** 信息流 / 详情 / 关注流的身份（13.4.1 / API 13 / v2）。 */
    private val defaultIdentity = Identity(AppVersion.NAME, AppVersion.CODE, AppVersion.API) { ts ->
        parse.v2(device.code, ts)
    }

    /** 评论接口的身份（16.2.0 / API 16 / v3）。v3 的 appCode 必须和 `X-App-Code` 一致。 */
    private val commentIdentity = Identity(COMMENT_VERSION_NAME, COMMENT_VERSION_CODE, COMMENT_API) { ts ->
        parse.v3(device.code, ts, COMMENT_VERSION_CODE.toLong())
    }

    /**
     * 一套版本号 + token 生成方式。请求头里的 UA / `X-App-Version` / `X-App-Code` / `X-Api-Version` /
     * `X-App-Supported` 都从这一处取，保证和 `X-App-Token` 对得上。
     */
    private inner class Identity(
        val name: String,
        val code: String,
        val api: String,
        val token: (Long) -> String,
    ) {
        /** token 只取决于设备码 + 秒级时间戳（+ 版本），同一秒内的多个请求复用同一个，不重复跑 bcrypt。 */
        private var cachedAtSec = Long.MIN_VALUE
        private var cachedToken = ""

        @Synchronized
        fun tokenFor(ts: Long): String {
            if (ts != cachedAtSec) {
                cachedAtSec = ts
                cachedToken = token(ts)
            }
            return cachedToken
        }

        fun headers(): Map<String, String> = mapOf(
            "User-Agent" to device.userAgentFor(name, code),
            "X-App-Version" to name,
            "X-App-Code" to code,
            "X-Api-Version" to api,
            "X-App-Supported" to code,
        )
    }

    /** indexV8 的 installTime 参数：c001apk 存的是首次启动的毫秒时间戳，这里也只生成一次。 */
    private val installTime: String = ctx.store.get(INSTALL_TIME_KEY)
        ?: System.currentTimeMillis().toString().also { ctx.store.put(INSTALL_TIME_KEY, it) }

    private val http = ctx.client(platform)
        .addInterceptor(DefaultHeadersInterceptor(device.headers(), hostFilter = { it in API_HOSTS }))
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS))
        .build()

    /**
     * 平台客户端（挂好了设备默认头 + 600ms 限速 + 代理选择 + Cookie 罐）。
     *
     * 只给 [RustCoolapkSource] 用：Rust 的宿主回调必须用**同一个对象**发请求，否则限速会变成两套
     * （各自的 `MinIntervalInterceptor` 实例各有一个"下次允许的时间"），拦截器链也会多一份。
     */
    internal val httpClient get() = http

    /**
     * 设备默认头（`device.headers()`）：Rust 联网层那份 `FfiHostSpec` 要用同一批。
     *
     * `internal`：Rust 版平台对象建 host 时传一次（默认头的顺序和内容必须和 Kotlin 完全一样，
     * 否则 `CoolapkRequestEquivalenceTest` 的逐字节门会当场对不上）。
     */
    internal val deviceHeaders: Map<String, String> get() = device.headers()

    // ---------- 账号 ----------

    /**
     * `/v6/account/checkLoginInfo`：官方 App、c001apk、c001apk-flutter 启动时都调它核对登录态。
     * 已登录时 data 里有 uid / username / userAvatar（还有 token，这里不碰）；没登录时 message 是"登录信息有误"之类。
     */
    override suspend fun whoami(): UserRef? {
        if (ctx.cookies.find(COOKIE_URL, "uid").isNullOrBlank()) return null
        val json = try {
            getJson("/v6/account/checkLoginInfo", emptyList())
        } catch (_: NotLoggedInException) {
            return null
        }
        return CoolapkParser.account(json)
    }

    // ---------- 信息流 ----------

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        val c = FeedCursor.parse(cursor)
        val res = when (kind) {
            // 参数照 c001apk-flutter getHomeFeed：page、firstLaunch、installTime、firstItem、lastItem、ids
            FeedKind.RECOMMEND -> {
                val params = listOf(
                    "page" to c.page.toString(),
                    "firstLaunch" to if (c.page == 1) "1" else "0",
                    "installTime" to installTime,
                ) + c.itemParams() + listOf("ids" to "")
                getRaw("/v6/main/indexV8", params)
            }
            // 参数照 c001apk-flutter getDataList（关注页 url=V9_HOME_TAB_FOLLOW，title=全部关注）
            FeedKind.FOLLOWING -> {
                if (ctx.cookies.find(COOKIE_URL, "uid").isNullOrBlank()) throw NotLoggedInException(platform)
                val params = listOf(
                    "url" to "V9_HOME_TAB_FOLLOW",
                    "title" to "全部关注",
                    "subTitle" to "",
                ) + c.itemParams() + listOf("page" to c.page.toString())
                getRaw("/v6/page/dataList", params)
            }
        }
        // 请求构造和以前逐字节一样（同一个 URL / 参数顺序 / 同一套 token 和头），只是拿到响应后交给 [parse]：
        // Kotlin 实现先映射错误再解析，Rust 实现在 Rust 里一次做完（含"关注流回 unLoginCard → 未登录"）。
        return parse.feed(kind, res.code, res.body, c)
    }

    // ---------- 搜索 ----------

    /**
     * 搜索：`GET /v6/search?type=feed&feedType=all&searchValue=…&page=…&lastItem=…`，
     * 和信息流同一套身份（13.4.1 / API 13 / v2 token）和限速。响应实体和信息流同构，直接复用 [CoolapkParser.feed]。
     * 撞上 `err_request_captcha*` 时按现有规则抛 [RiskControlException]（不自动弹验证、不重试）；
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        val (page, lastItem) = CoolapkSearch.decode(cursor)
        return CoolapkSearch.parse(getJson("/v6/search", CoolapkSearch.params(request, page, lastItem)), page)
    }

    // ---------- 详情和评论 ----------

    override suspend fun detail(id: String): FeedDetail {
        val json = getJson("/v6/feed/detail", listOf("id" to id))
        // 站外视频是 b23.tv 短链时，跟一次重定向才知道是不是 B站的哪个视频
        val media = json.obj("data")?.text("media_url")?.takeIf { CoolapkVideo.isShortLink(it) }
        return CoolapkParser.detail(json, media?.let { expandShortLink(it) })
    }

    /**
     * 播放地址：只有酷安自己托管的视频动态（`partId=main`）有。站外视频（B站 / 微博 / 抖音）在详情里是卡片，B站的转到 B站播放器。
     * 酷安托管的视频有 URL 签名防盗链（不签名 403 `need sign info`，见 [CoolapkVideo]），签名我们算不出来，
     * 所以先用 Range 探一下，被拒就抛 [SignatureException] 说清楚原因，不让播放器报一个不明不白的"播放失败"。酷安没有弹幕。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        if (partId != CoolapkVideo.MAIN) throw ApiException(platform, "bad_part", "不认识的视频 $partId")
        val json = getJson("/v6/feed/detail", listOf("id" to id))
        val hosted = json.obj("data")?.let(CoolapkVideo::hosted)
            ?: throw ApiException(platform, "no_video", "这条动态里没有酷安自己托管的视频（站外视频在详情里是卡片）")
        val info = CoolapkVideo.play(hosted)
        val probe = mediaHttp.fetch(
            Request.Builder().url(hosted.url).header("Range", "bytes=0-0").apply { info.headers.forEach { (k, v) -> header(k, v) } }.build(),
        )
        if (probe.code == 403 || probe.code == 401) {
            throw SignatureException(platform, "酷安托管的视频需要官方 App 才会算的 URL 签名（HTTP ${probe.code} ${probe.body.take(40)}），暂时放不了")
        }
        if (probe.code !in 200..299) throw ApiException(platform, "HTTP ${probe.code}", "视频地址不可用")
        return info
    }

    /**
     * 拉媒体文件用的客户端：不带 Cookie，也不加 App 请求头；代理照酷安自己的来。
     *
     * 暴露给 [RustCoolapkSource] 用（Rust 的 `play` 拿它发 `Range: bytes=0-0` 探针）：
     * 必须是**同一个对象**，否则就成了两套连接池；而且这个客户端不能换成 [http]——
     * `http` 带着 Cookie 罐和 600ms 限速，探针会跟着把 `.coolapk.com` 的登录 Cookie 带去 CDN。
     */
    internal val mediaClient: OkHttpClient get() = mediaHttp

    private val mediaHttp = ctx.client(platform).cookieJar(okhttp3.CookieJar.NO_COOKIES).build()

    /**
     * 跟 b23.tv 短链重定向用的客户端：**用 B站的代理配置**。这个地址属于 B站（动态里的站外 B站视频），
     * 用户可能只给其中一家配了代理，不能借用酷安那一份。
     */
    private val biliLinkHttp = ctx.client(Platform("bili")).cookieJar(okhttp3.CookieJar.NO_COOKIES).build()

    /**
     * [biliLinkHttp] 的"不跟重定向"那一版（每次现建一个，和 [expandShortLink] 里一样）。
     *
     * 只给 `RustCoolapkSource` 用：Rust 的第二个宿主端口必须用**同一个客户端**（同一份拦截器 / 代理 /
     * 连接池），否则线上字节和限速就跟 Kotlin 版不一样了。
     */
    internal val linkClient: OkHttpClient
        get() = biliLinkHttp.newBuilder().followRedirects(false).followSslRedirects(false).build()

    /** b23.tv 短链跟一次重定向（不自动跟随，只读 Location）；失败返回 null。 */
    private suspend fun expandShortLink(url: String): String? = try {
        val res = biliLinkHttp.newBuilder().followRedirects(false).followSslRedirects(false).build()
            .fetch(Request.Builder().url(url).header("User-Agent", feedhub.core.UserAgents.DESKTOP_CHROME).build())
        res.headers.firstOrNull { it.first.equals("location", ignoreCase = true) }?.second
    } catch (_: java.io.IOException) {
        null
    }

    /**
     * 一级评论：**`api.coolapk.com/v6/feed/replyList`，新客户端身份（16.2.0 / API 16 / v3）**。
     *
     * 请尝试升级到新版酷安」（版本闸）；
     * - 16.2.0 / API 16 能到 `err_request_captcha_v2`「当前访问需要验证码」这一步；
     * - **一次人工验证（`validate` cookie）之后，整个会话的 `replyList` 都能读**——这是 [ctx.captcha] 的活，
     * 验证码没解、没有 handler 时抛
     *   [RiskControlException] 说清楚，**不退回 hotReplyList、不静默给半份评论**。
     *
     * 排序：酷安自己的 `reply_order` 是 `lastupdate_desc,dateline_desc,popular,author`，这里映射
     * 默认 → 最近回复、热门 → popular、最新 → 最新发布；`author`（只看楼主）不做。
     *
     * 注意 `api2` 上的 `replyList` 和 `api` 上的 `hotReplyList` 都不是这条路：前者同样被验证码拦，
     * 后者只给热门的一部分，不能当"全部评论"。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        val c = FeedCursor.parse(cursor)
        val listType = when (sort) {
            CommentSort.DEFAULT -> "lastupdate_desc"
            CommentSort.HOT -> "popular"
            CommentSort.NEWEST -> "dateline_desc"
        }
        val params = listOf(
            "id" to id,
            "listType" to listType,
            "page" to c.page.toString(),
        ) + c.itemParams() + listOf(
            "discussMode" to "1",
            "feedType" to "feed",
            "blockStatus" to "0",
            "fromFeedAuthor" to "0",
        )
        return CoolapkParser.replyList(
            getJson("/v6/feed/replyList", params, commentsBaseUrl, commentIdentity, allowCaptcha = true),
            c,
        )
    }

    /**
     * 楼中楼：同一个 `replyList`、`id` 换成一级评论的 id、`feedType=feed_reply`，同样走 api + 新身份。
     * 换成 api + 上面的验证码流程后，
     * 一次验证的 `validate` cookie 对楼中楼也生效（ZaiLinn/c001apk 2.0.6 的做法）。
     */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        val c = FeedCursor.parse(cursor)
        val params = listOf(
            "id" to commentId,
            "listType" to "",
            "page" to c.page.toString(),
        ) + c.itemParams() + listOf(
            "discussMode" to "0",
            "feedType" to "feed_reply",
            "blockStatus" to "0",
            "fromFeedAuthor" to "0",
        )
        return CoolapkParser.replyList(
            getJson("/v6/feed/replyList", params, commentsBaseUrl, commentIdentity, allowCaptcha = true),
            c,
        )
    }

    // ---------- 调试 ----------

    override suspend fun raw(request: RawRequest): RawResponse {
        var req = request.toOkHttpRequest(apiBase)
        if (request.sign && req.url.host in API_HOSTS && req.header("X-App-Token") == null) {
            req = req.newBuilder().header("X-App-Token", newToken()).build()
        }
        return http.fetch(req).toRawResponse()
    }

    // ---------- 内部 ----------

    private fun newToken(): String = defaultIdentity.tokenFor(nowSec())

    /**
     * 发请求并解析 JSON。[allowCaptcha] 为 true 时，遇到 `err_request_captcha*` 会交给
     * [SourceContext.captcha] 让用户完成一次验证，拿回 proof 后写 `validate` cookie（或查询参数），
     * **在同一个 SESSID 会话里重试一次**。只重试一次：还是验证码就抛错，不循环刷验证码。
     */
    private suspend fun getJson(
        path: String,
        params: List<Pair<String, String>>,
        base: String = apiBase,
        identity: Identity = defaultIdentity,
        allowCaptcha: Boolean = false,
    ): JsonObject {
        var url = (base + path).toHttpUrl().newBuilder().apply {
            params.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build()
        var (res, json) = fetchJson(url, identity)
        if (allowCaptcha && json != null && isCaptcha(json)) {
            val challenge = captchaChallenge(json)
            val handler = ctx.captcha ?: throw CoolapkCaptcha.noHandler(platform, challenge.message)
            val proof = handler.solve(challenge)
                ?: throw CoolapkCaptcha.cancelled(platform, challenge.message)
            if (challenge.field.isEmpty()) {
                storeValidate(proof)
            } else {
                url = url.newBuilder().addQueryParameter(challenge.field, proof).build()
            }
            val retry = fetchJson(url, identity)
            res = retry.first
            json = retry.second
            if (json != null && isCaptcha(json)) {
                throw CoolapkCaptcha.stillRequired(platform, challenge.message)
            }
        }
        return decodeResponse(res.body, res.code, json)
    }

    /** 一次请求，只把响应原样交回去（解析 / 错误映射交给 [parse]）。 */
    private suspend fun getRaw(
        path: String,
        params: List<Pair<String, String>>,
        base: String = apiBase,
        identity: Identity = defaultIdentity,
    ): HttpResult {
        val url = (base + path).toHttpUrl().newBuilder().apply {
            params.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build()
        return http.fetch(request(url, identity))
    }

    /**
     * 带 `X-App-Token` + 那套版本头的请求（两条路共用的唯一一处构造）。
     *
     * 所以真实请求的字节一个都没变（拦截器现在只是兜底）；
     * 换来的是**请求本身自带全部头**——Rust 版照着同一份拼，MockWebServer 上也能逐字节比设备头。
     */
    private fun request(url: HttpUrl, identity: Identity): Request {
        // 用有序表拼一份"最终头"，再一次性写进 Builder：和拦截器"先放身份头、再补缺失的设备头"
        // 的先后顺序完全一样（`LinkedHashMap` 的插入顺序）。
        val headers = LinkedHashMap<String, String>()
        headers["X-App-Token"] = identity.tokenFor(nowSec())
        identity.headers().forEach { (k, v) -> headers[k] = v }
        device.headers().forEach { (k, v) -> if (!headers.containsKey(k)) headers[k] = v }
        return Request.Builder()
            .url(url)
            .apply { headers.forEach { (k, v) -> header(k, v) } }
            .build()
    }

    /** 一次请求 + 解析。版本头 / UA / token 都从 [identity] 取，覆盖掉拦截器里的默认值。 */
    private suspend fun fetchJson(url: HttpUrl, identity: Identity): Pair<HttpResult, JsonObject?> {
        val res = http.fetch(request(url, identity))
        return res to runCatching { parseJsonObject(platform, res.body) }.getOrNull()
    }

    private fun isCaptcha(json: JsonObject): Boolean =
        json.string("messageStatus")?.startsWith(CAPTCHA_STATUS) == true

    /**
     * 从 `messageExtra`（一个字符串形式的 JSON）里取 captchaId / captchaField。
     * 服务端没给 id 时用 coolapk-desktop 里记录的默认 id；`captchaField` 为空表示 proof 走 `validate` cookie。
     */
    private fun captchaChallenge(json: JsonObject): CaptchaChallenge {
        val extra: JsonObject? = when (val raw = json["messageExtra"] ?: json["extra"]) {
            is JsonObject -> raw
            is JsonPrimitive -> raw.takeIf { it.isString }?.content
                ?.let { runCatching { parseJsonObject(platform, it) }.getOrNull() }
            else -> null
        }
        val id = extra?.string("captchaId")?.takeIf { CAPTCHA_ID.matches(it) } ?: CAPTCHA_DEFAULT_ID
        val field = when (val f = extra?.string("captchaField")) {
            null, "" -> ""
            else -> if (CAPTCHA_FIELD.matches(f)) f else "_v2_post_token"
        }
        return CaptchaChallenge(platform, id, field, json.text("message") ?: "当前访问需要验证码")
    }

    /**
     * 把验证结果写成 `.coolapk.com` 的 `validate` cookie（URL 编码，和 ZaiLinn/c001apk 的做法一致）。
     * 挑战响应同时下的 `SESSID` 已经在共用 Cookie 罐里，重试会带上同一个会话。
     *
     * 规则在 [CoolapkCaptcha] 里（和 `RustCoolapkSource` 共用同一份，字节才不会漂）。
     */
    private fun storeValidate(proof: String) = CoolapkCaptcha.storeValidate(ctx, proof)

    companion object {
        const val API = "https://api2.coolapk.com"

        /**
         * 评论接口走 **api**（不是 api2）：`replyList` / `hotReplyList` 都在这里，
         */
        const val COMMENTS_API = "https://api.coolapk.com"

        /** 评论接口只认新客户端：16.2.0 / 2604201 / API 16（v3 token 的 appCode 必须一致）。 */
        const val COMMENT_VERSION_NAME = "16.2.0"
        const val COMMENT_VERSION_CODE = "2604201"
        const val COMMENT_API = "16"

        private val API_HOSTS = setOf("api2.coolapk.com", "api.coolapk.com")
        private const val COOKIE_URL = "https://api2.coolapk.com/"
        private const val INSTALL_TIME_KEY = "coolapk.install_time"
        private const val MIN_INTERVAL_MS = 600L

        /** 服务端没下发 captchaId 时的兜底（coolapk-desktop 的默认酷安 id；和 [CoolapkCaptcha] 同一份）。 */
        private const val CAPTCHA_DEFAULT_ID = CoolapkCaptcha.DEFAULT_ID
        private val CAPTCHA_ID = Regex("^[A-Za-z0-9]{16,64}$")
        private val CAPTCHA_FIELD = Regex("^_[A-Za-z0-9_]{1,63}$")

        /**
         * 酷安出错时不给 data，只给 `{status, error, message, messageStatus}`；正常响应只有 `data`。
         * token 过期或算错：status 1004 "请求验证错误"
         * - 没登录调 checkLoginInfo：status -1 "登录信息有误"
         * - 内容不存在：status -2 "你查看的内容不存在或已被删除"
         * - **App 版本过低**：HTTP 403 + message
         *   "评论接口异常，请尝试升级到新版酷安"。这是版本闸：评论改用 16.2.0 的独立身份后不会走到这里；
         *   `raw` 里用旧身份打还能复现。
         * - 要人机验证：评论路径在
         *   [getJson] 里先交给 [SourceContext.captcha]，处理完才可能落到这里；没有 handler / 验证仍不过时抛
         *   [RiskControlException]。
         * 异常里只放 status / messageStatus 和截短的 message。
         */
        /**
         * 一次响应 → JSON 对象，或者一条带类型的错误。`getJson` 的尾巴抽出来的，**Kotlin 版解析**
         * （[KotlinCoolapkParse]）也走这里，所以对拍连错误语义都一致。
         *
         * 出错时酷安也可能回非 200 + JSON 消息：先按消息判断，解析不了再按状态码。
         * [json] 是已经解析好的那份（`getJson` 里刚 parse 过，省一次），不给就自己 parse。
         */
        internal fun decodeResponse(
            body: String,
            httpCode: Int,
            json: JsonObject? = runCatching { parseJsonObject(COOLAPK, body) }.getOrNull(),
        ): JsonObject {
            if (json != null && (httpCode in 200..299 || json["message"] != null)) return checkStatus(json)
            throw statusError(httpCode, body)
        }

        /** 非 JSON / 没 message 时按状态码映射（和原来 `getJson` 的 `when` 一样）。 */
        private fun statusError(httpCode: Int, body: String): SourceException = when (httpCode) {
            in 200..299 -> ParseException(COOLAPK, "响应不是 JSON：${body.take(200)}")
            401 -> NotLoggedInException(COOLAPK)
            403 -> RiskControlException(COOLAPK, "HTTP 403", "请求被拒绝")
            429 -> RiskControlException(COOLAPK, "HTTP 429", "请求过于频繁")
            else -> ApiException(COOLAPK, "HTTP $httpCode", body.take(200))
        }

        internal fun checkStatus(json: JsonObject): JsonObject {
            val p = COOLAPK
            val data: JsonElement? = json["data"]
            val message = json.text("message")
            if (message == null) {
                if (data == null || data is JsonNull) throw ParseException(p, "响应既没有 data 也没有 message")
                return json
            }
            val status = json.long("status")
            val code = status?.toString() ?: json.string("messageStatus") ?: "?"
            val messageStatus = json.string("messageStatus")
            val msg = message.take(100)
            throw when {
                status in SIGNATURE_STATUS || "请求验证错误" in msg || "请求方式错误" in msg -> SignatureException(p, "$code $msg")
                // 版本闸：酷安按 X-App-Version / X-App-Code 拦接口，先于登录判断说出来
                APP_VERSION_WORDS.any { it in msg } ->
                    ApiException(p, code, "酷安要求更新 App 版本号（$msg）：改 AppVersion 的 NAME / CODE / API，步骤见 source-coolapk/README.md")
                "登录" in msg -> NotLoggedInException(p, "未登录或登录已失效（$msg）")
                messageStatus?.startsWith(CAPTCHA_STATUS) == true || RISK_WORDS.any { it in msg } ->
                    RiskControlException(p, messageStatus ?: code, msg)
                else -> ApiException(p, code, msg)
            }
        }

        private val SIGNATURE_STATUS = setOf(1001L, 1004L)
        private val RISK_WORDS = listOf("验证码", "频繁", "风险", "异常操作", "安全验证")

        /** 版本闸的文案。 */
        private val APP_VERSION_WORDS = listOf("升级到新版酷安", "升级到最新版", "请升级酷安")

        /** `err_request_captcha` / `err_request_captcha_v2`。 */
        private const val CAPTCHA_STATUS = "err_request_captcha"
    }
}

/**
 * 分页游标。酷安的列表都用 `page`（从 1 开始）加 `firstItem` / `lastItem`（第一页第一条、上一页最后一条的 id），
 * c001apk-flutter `common_controller.dart` 就是这么翻页的。编码成 `page=2&firstItem=..&lastItem=..`。
 */
internal data class FeedCursor(val page: Int = 1, val firstItem: String? = null, val lastItem: String? = null) {
    fun itemParams(): List<Pair<String, String>> =
        listOfNotNull(firstItem?.let { "firstItem" to it }, lastItem?.let { "lastItem" to it })

    fun encode(): Cursor = Cursor(
        listOfNotNull("page=$page", firstItem?.let { "firstItem=$it" }, lastItem?.let { "lastItem=$it" }).joinToString("&"),
    )

    companion object {
        fun parse(cursor: Cursor?): FeedCursor {
            if (cursor == null) return FeedCursor()
            val map = cursor.value.split('&').mapNotNull {
                val i = it.indexOf('=')
                if (i <= 0) null else it.substring(0, i) to it.substring(i + 1)
            }.toMap()
            // 也接受直接写一个页码，方便 cli 里 --cursor 3
            val page = map["page"]?.toIntOrNull() ?: cursor.value.toIntOrNull() ?: 1
            return FeedCursor(page.coerceAtLeast(1), map["firstItem"]?.ifEmpty { null }, map["lastItem"]?.ifEmpty { null })
        }
    }
}
