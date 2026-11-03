package feedhub.source.douyin

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
import feedhub.core.MemoryCookieJar
import feedhub.core.MinIntervalInterceptor
import feedhub.core.NativeDetailSource
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.QrSessions
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.SourceContext
import feedhub.core.UnsupportedFeedException
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.commitQrLogin
import feedhub.core.fetch
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.parseJsonObject
import feedhub.core.parseQrJson
import feedhub.core.text
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import feedhub.core.withCookieJar
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

/**
 * [DouyinSource.play] 复用 [DouyinSource.detail] 刚取过的 `aweme/detail` 的时限。播放地址的有效期**没核实**
 * ，拿不准就只取 60 秒：
 * 够覆盖"点开详情 → 马上要播放地址"；换清晰度、很久以后再播都重新取。
 */
internal const val AWEME_REUSE_MS = 60_000L

/** 记住最近几条作品（够覆盖"连着点开几条"）。 */
private const val RECENT_AWEMES = 8

/**
 * 抖音（web 端）。推荐流 + 登录态 + 签名 + 解析 + `raw`，
 *
 * 常量按用户 Chrome 155 / Linux 的抓包定下来（`abogus_profile.json` 的 `capturedFrom`），
 * **UA / 窗口指纹 / `sdkMinor` 是成套的**（见 [DouyinDevice]）。
 *
 * ## 接口
 *
 * - 推荐流 [FeedKind.RECOMMEND]：`GET /aweme/v1/web/module/feed/`（"精选"）。
 * - 详情：`GET /aweme/v1/web/aweme/detail/`（`aweme_detail`），**在 secsdk 的 webSign 策略表里**，
 *   所以除了 a_bogus 还要 `x-secsdk-web-signature`（[SecSdkSigner]）。
 * - 评论：`GET /aweme/v1/web/comment/list/`；楼中楼 `GET /aweme/v1/web/comment/list/reply/`。
 *   两个都不在 secsdk 表里，只要 a_bogus；楼中楼另有两个"和浏览器逐字对齐"的要求（见 [getJson]）。
 * - 播放地址：同一个 `aweme/detail` 里的 `video.bit_rate[].play_addr`（地址带时效参数，会过期）；
 *   [AWEME_REUSE_MS] 内刚取过（点开详情时）就直接用，不然重新取。
 * - 表情表：`GET /aweme/v1/web/emoji/list`（缓存一周）。
 * - [FeedKind.FOLLOWING]：**不做**。
 *
 * ## 签名
 *
 * - [AbogusSigner]：a_bogus（骨架来自 ylcangel，Apache-2.0；常量在 `resources/.../abogus_profile.json`，
 *   `ctx.store` 的 `douyin.abogus_profile` 能覆盖）。
 * - [SecSdkSigner]：`x-secsdk-web-signature`，只对 [SecSdkSigner.isProtected] 列出的路径加
 *   （`aweme/detail` 在表里，`module/feed` 和评论不在）。
 * - 两者都在 **OkHttp 拦截器** [SignInterceptor] 里算，排在限速拦截器**之后**：
 *   限速睡完了再取时间戳，签的就是 OkHttp 规范化之后、马上要发出去的那串 query。
 *   楼中楼要的"`verifyFp` / `fp` 放在 a_bogus 之后、不参与签名"由 [SignTag] 的 `afterSign` 实现。
 *
 * ## 风控
 *
 * 识别到就**停下抛异常，不重试、不解验证码**：空 body、`__ac_nonce` / `_$jsvmprt` 挑战页、
 * `X-Vc-Bdturing-Parameters`、`X-Tt-Verify-Passport-Decision`、HTTP 403 的 Argus / Sign Invalid、
 * 业务码 8（未登录，也可能是被写操作连坐作废了会话）。
 *
 * [base] 只给测试用（指向 MockWebServer）；[abogusRandom] / [clock] / [enterPageTs] 也只给测试用
 * （让签名可复现）；[parse] / [signFactory] 是接缝：默认是 Kotlin 实现，`RustDouyinSource` 传 Rust 实现。
 *
 * 拼请求 + 发请求 + 解析都在 Rust），不再调 [recommend]。这里的请求构造 / 签名 / 解析一行
 * 没删，`DouyinRequestEquivalenceTest` 拿它当逐字节的参照；详情 / 评论 / 播放 / 表情表 / 扫码 / `whoami` 仍在用它。
 */
class DouyinSource internal constructor(
    internal val ctx: SourceContext,
    private val base: String,
    internal val abogusRandom: AbogusRandom = AbogusRandom.SYSTEM,
    internal val clock: () -> Long = System::currentTimeMillis,
    /** 进入页面（SDK 加载）的时间，只参与 a_bogus 的 `U[28]`；默认在建 Source 时取一次。 */
    internal val enterPageTs: Long = clock(),
    /** 推荐流的解析交给谁（默认 Kotlin；Rust 版传 [RustDouyinParse]）。 */
    parse: DouyinParse? = null,
    /** 签名交给谁（默认 Kotlin；Rust 版传 [RustDouyinSign]，构造时才知道 profile / env / 时间）。 */
    signFactory: ((profile: AbogusProfile, env: AbogusEnv, enterPageTs: Long) -> DouyinSign)? = null,
) : NativeDetailSource, VideoSource, QrLoginSource {
    constructor(ctx: SourceContext) : this(ctx, API)

    private val parse: DouyinParse = parse ?: KotlinDouyinParse(this)

    // 身份、流、登录、域名都以插件描述为准（[DouyinPlugin]），这里不再写第二份
    override val platform = DOUYIN
    override val feeds = DouyinPlugin.capabilities.feeds
    override val login = DouyinPlugin.login.web
    override val apiBase = base
    override val hosts = DouyinPlugin.hosts

    internal val device = DouyinDevice(ctx.store, clock)

    /**
     * a_bogus 的常量：随包 JSON（[AbogusProfile.bundled]）打底，`ctx.store` 的 `douyin.abogus_profile`
     * 里放一份新的整份 JSON 可以覆盖。
     */
    internal val abogusProfile: AbogusProfile =
        ctx.store.get(AbogusProfile.KEY)?.let { runCatching { AbogusProfile.parse(it) }.getOrNull() }
            ?: AbogusProfile.bundled()

    // ctx.client(platform)：套上这个平台的代理配置
    private val http = ctx.client(platform)
        .addInterceptor(
            DefaultHeadersInterceptor(
                headers(),
                // `notdouyin.com` 这类域名不算
                hostFilter = { isDouyinHost(it) || it == base.toHttpUrl().host },
            ),
        )
        // 抖音风控严，间隔比别的平台大一些并加抖动
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS, MIN_INTERVAL_JITTER_MS))
        // 签名排在限速**之后**：睡完了才算时间戳、签 OkHttp 规范化之后要发出去的 query
        .addInterceptor(SignInterceptor())
        .build()

    /**
     * 这个平台**自己的那个 OkHttpClient**（默认头 + 1 秒限速 + 签名拦截器 + 代理 + Cookie 罐）。
     *
     * `RustDouyinSource` 把它交给 `RustDouyinHosted`：Rust 拼好请求、回调这个客户端发出去。
     * **必须共用同一份**——各自 `newBuilder()` 一份的话，`MinIntervalInterceptor` 会变成两个实例，
     * 等于把 1 秒限速拆成两个窗口（风控上更危险）。
     */
    internal val httpClient: OkHttpClient get() = http

    /**
     * 扫码登录用的 passport 主机。测试里（[base] 指向 MockWebServer）所有接口在同一个地址。
     */
    private val passportBase = if (base == API) LOGIN else base

    private val abogus = AbogusSigner(
        profile = abogusProfile,
        env = device.abogusEnv,
        random = abogusRandom,
        clock = clock,
        enterPageTs = enterPageTs,
    )

    /** a_bogus / secsdk 归谁算（`RustDouyinSource` 建的时候换成 Rust 版）。 */
    private val signer: DouyinSign =
        signFactory?.invoke(abogusProfile, device.abogusEnv, enterPageTs) ?: KotlinDouyinSign(abogus)

    // ---------- 登录态 ----------

    /**
     * 有 `sessionid` 系列 Cookie 才算登录。没有就不发请求——`profile/self` 未登录会返回业务码 8。
     * 未登录返回 null，不抛异常（[feedhub.core.Source.whoami] 的约定）。
     */
    override suspend fun whoami(): UserRef? {
        if (!loggedIn()) return null
        val json = getJson(Get(PROFILE_SELF_PATH, checkCode = false))
        return when (json.long("status_code")) {
            0L -> DouyinParser.selfUser(json)
            8L -> null
            else -> DouyinParser.selfUser(check(json))
        }
    }

    private fun loggedIn(): Boolean = LOGIN_COOKIES.any { ctx.cookies.find(WWW, it) != null }

    // ---------- 信息流 ----------

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> recommend(cursor)
        else -> throw UnsupportedFeedException(platform, kind)
    }

    /**
     * 推荐流。**不是**首页"推荐"那个 `tab/feed`（没人证明跑通），是"精选"的 `module/feed`。
     *
     * **没登录就直接抛 [NotLoggedInException]，一个请求都不发**：未登录时
     * `module/feed` 只会被空 body / 业务码 8 拒掉；与其拿用户的 IP / 账号去换风控标记，不如先要求登录。
     * 登录态是导入的抖音 Cookie（`sessionid` 系列），见 [login]。
     */
    private suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        if (!loggedIn()) throw NotLoggedInException(platform, "抖音推荐流要登录：先在“我的”里导入抖音 Cookie")
        val refreshIndex = cursor?.value?.toIntOrNull() ?: 1
        val res = fetch(
            Get(
                path = MODULE_FEED_PATH,
                params = linkedMapOf(
                    "module_id" to MODULE_ID,
                    "count" to PAGE_SIZE.toString(),
                    "filterGids" to "",
                    "presented_ids" to "",
                    "refresh_index" to refreshIndex.toString(),
                    "refer_id" to "",
                    "refer_type" to "10",
                    "awemePcRecRawData" to """{"is_client":false}""",
                    "Seo-Flag" to "0",
                    "install_time" to device.installTime.toString(),
                ),
            ),
        )
        // 先解析一遍（错误映射 + desc 拼串一起回来），文案里有表情记号才去取表、再映射一次——
        // 没表情的常见情况只解析一次，**请求条数和以前完全一样**。
        val first = parse.feed(res, refreshIndex, emptyMap())
        val emotes = emotesFor(first.texts)
        val page = if (emotes.isEmpty()) first else parse.feed(res, refreshIndex, emotes)
        return Page(page.items, page.next?.let(::Cursor))
    }

    // ---------- 详情 ----------

    /**
     * 原生详情：`GET /aweme/v1/web/aweme/detail/?aweme_id=…`。
     *
     * 参数照 cv-cat 的 `get_work_info`（`request_source=600`、`origin_type=video_page`；
     * **这个接口的 `version_code` 是 190500 / 19.5.0**，和 `module/feed` 的 170400 不同 ——
     * 真实页面每个前端模块自带版本号）。这个路径在 secsdk 保护表里，所以 [signedUrl] 会再补
     * `timestamp` + `x-secsdk-web-signature`。
     */
    override suspend fun detail(id: String): FeedDetail {
        val json = fetchAweme(id)
        // 文案里有 `[赞]` 这种文字表情才去取表情表（和卡片 / 评论同一张表，缓存一周）；取不到就保持文字
        val desc = json.obj("aweme_detail")?.text("desc").orEmpty()
        return DouyinDetail.detail(json, id, emotesFor(desc))
    }

    /**
     * `aweme/detail` 的响应（详情和 [play] 都用它）。取到有 `aweme_detail` 的就记进 [recentAwemes]。
     * 参数照 cv-cat 的 `get_work_info`（`request_source=600`、`origin_type=video_page`；
     * **这个接口的 `version_code` 是 190500 / 19.5.0**，和 `module/feed` 的 170400 不同 ——
     * 真实页面每个前端模块自带版本号）。这个路径在 secsdk 保护表里，所以 [signedUrl] 会再补
     * `timestamp` + `x-secsdk-web-signature`。
     */
    private suspend fun fetchAweme(id: String): JsonObject = getJson(
        Get(
            path = AWEME_DETAIL_PATH,
            params = linkedMapOf(
                "aweme_id" to id,
                "request_source" to "600",
                "origin_type" to "video_page",
            ),
            referer = DouyinParser.videoUrl(id),
            versionCode = DETAIL_VERSION_CODE,
            versionName = DETAIL_VERSION_NAME,
            roundTripTime = "50",
        ),
    ).also { json -> if (json.obj("aweme_detail") != null) synchronized(recentAwemes) { recentAwemes[id] = clock() to json } }

    /**
     * 最近取过的作品（aweme_id → 取的时刻 + 原始响应）。点开一条视频是 [detail] 先取 `aweme/detail`、[play] 紧接着要
     * 同一条作品的播放地址——两个用的是**同一个**接口。以前 play 再取一次：多一个 secsdk 保护路径上的请求，
     * 还要在限速里排 1～1.5 秒（[MIN_INTERVAL_MS] + 抖动）。现在 [play] 先用这里 [AWEME_REUSE_MS] 内取过的那份；
     * [detail] 照旧每次现取（正文 / 计数要新的）。
     */
    private val recentAwemes = object : LinkedHashMap<String, Pair<Long, JsonObject>>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Pair<Long, JsonObject>>) = size > RECENT_AWEMES
    }

    private fun recentAweme(id: String): JsonObject? = synchronized(recentAwemes) {
        recentAwemes[id]?.takeIf { clock() - it.first <= AWEME_REUSE_MS }?.second
    }

    /**
     * Rust 版整个搬进了 Rust 的 `DouyinHostedSource`，不再需要外部回填）。
     *
     * 留着的理由：`play` 的 Kotlin 实现（本类）是对拍的参照，它的 60 秒缓存仍由 [fetchAweme] 自己维护；
     * 这个入口曾经是"Rust 详情 → Kotlin 缓存"的桥，搬走之后桥拆了。真要删就等 Kotlin 版 `play` 一起删。
     */
    internal fun rememberAwemeFromBody(id: String, body: String) {
        val json = try {
            parseJsonObject(platform, body)
        } catch (_: Exception) {
            return
        }
        if (json.obj("aweme_detail") != null) {
            synchronized(recentAwemes) { recentAwemes[id] = clock() to json }
        }
    }

    /** 响应里的作品对象（`aweme_detail`）。取不到时和 [DouyinDetail] 一样把平台的原因带出来。 */
    private fun awemeOf(json: JsonObject, id: String): JsonObject =
        json.obj("aweme_detail") ?: throw DouyinDetail.notFound(json, id)

    // ---------- 评论 ----------

    /**
     * 一级评论：`GET /aweme/v1/web/comment/list/`。参数照 cv-cat 的 `get_work_out_comment`：
     * `whale_cut_token` / `rcFT` 是**空值字段**（浏览器确实发，别丢），`cut_version=1`。
     * Referer 必须是 `https://www.douyin.com/video/<aweme_id>`（mafqla / cv-cat 都这么说）。
     * 游标就是响应里的 `cursor`，`has_more` 为假结束。评论不在 secsdk 表里，只要 a_bogus。
     *
     * cv-cat 说浏览器在这个接口上还带 4 个 `bd-ticket-guard-*` 头，服务端是否强制**未确认**；
     * 我们先不发。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        val json = getJson(
            Get(
                path = COMMENT_LIST_PATH,
                params = linkedMapOf(
                    "aweme_id" to id,
                    "cursor" to (cursor?.value ?: "0"),
                    "count" to COMMENT_PAGE_SIZE.toString(),
                    "item_type" to "0",
                    "whale_cut_token" to "",
                    "cut_version" to "1",
                    "rcFT" to "",
                ),
                referer = DouyinParser.videoUrl(id),
            ),
        )
        val page = DouyinComments.page(json, emotesFor(DouyinComments.texts(json)))
        return Page(page.items, page.next?.let(::Cursor))
    }

    /**
     * 楼中楼：`GET /aweme/v1/web/comment/list/reply/`。
     *
     * 我们照做：
     * ① 公共参数组用同一套（`round_trip_time=0`）；② **不带 `uifid`**；③ `verifyFp` / `fp` 放在
     * `a_bogus` **之后**、不参与签名（[Get.afterSign]）。三者错一个就会被判 bdturing。
     */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        val fp = verifyFp()
        val json = getJson(
            Get(
                path = COMMENT_REPLY_PATH,
                params = linkedMapOf(
                    "item_id" to id,
                    "comment_id" to commentId,
                    "cut_version" to "1",
                    "cursor" to (cursor?.value ?: "0"),
                    "count" to REPLY_PAGE_SIZE.toString(),
                    "item_type" to "0",
                ),
                referer = DouyinParser.videoUrl(id),
                ids = Ids(uifid = false, verifyFp = false),
                afterSign = linkedMapOf("verifyFp" to fp, "fp" to fp),
            ),
        )
        val page = DouyinComments.page(json, emotesFor(DouyinComments.texts(json)))
        return Page(page.items, page.next?.let(::Cursor))
    }

    // ---------- 视频 ----------

    /**
     * 播放地址：`aweme/detail` 里的 `video.bit_rate[].play_addr`。[AWEME_REUSE_MS]（60 秒）内取过同一条作品
     * （点开视频时 [detail] 刚取过）就直接用，没有才重新取——地址里有 `l` / `ft` / `sign` 这类时效参数，旧的会过期。
     * 各档清晰度、AVC / HEVC 都列出来（[DouyinVideo]），由前端按设置和硬解能力挑。
     * 抖音是音画一体的 mp4，所以走 [PlayInfo.progressive]，没有 DASH 轨、也没有弹幕。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        // 刚打开详情时取过这条作品就直接用（见 [recentAwemes]），不再发一次 aweme/detail
        val json = recentAweme(id) ?: fetchAweme(id)
        return DouyinVideo.play(awemeOf(json, id), partId, quality, streamHeaders())
    }

    /** UA 用和接口一致的那套。 */
    private fun streamHeaders(): Map<String, String> = linkedMapOf(
        "Referer" to "https://www.douyin.com/",
        "User-Agent" to device.userAgent,
    )

    // ---------- 表情 ----------

    @Volatile
    private var emoteCache: DouyinEmoteTable? = null

    @Volatile
    private var emoteRetryAtMs = 0L

    /**
     * 文字里有 `[赞]` 这样的记号才去取表情表（`GET /aweme/v1/web/emoji/list`），缓存一周
     * （和小红书 / 小黑盒一样）。风控异常照常往外抛（不吞）；接口不在 / 结构不认 时这次用旧表，
     * 10 分钟内不再试 —— 表情只是锦上添花，不该因为它让详情 / 评论打不开。
     */
    internal suspend fun emotesFor(text: String): Map<String, String> {
        if (text.isEmpty() || !DouyinEmoteTable.EMOTE_TOKEN.containsMatchIn(text)) return emptyMap()
        emoteCache?.let { return it.map }
        val nowMs = System.currentTimeMillis()
        val nowSec = nowMs / 1000
        val stored = ctx.store.get(DouyinEmoteTable.KEY)?.let(DouyinEmoteTable::fromStored)
        if (stored != null && nowSec - stored.fetchedAtSec < DouyinEmoteTable.TTL_SEC) {
            emoteCache = stored
            return stored.map
        }
        if (nowMs < emoteRetryAtMs) return stored?.map.orEmpty()
        val table = try {
            val json = getJson(
                Get(EMOJI_LIST_PATH, params = linkedMapOf("channel" to "channel_pc_web"), checkCode = false),
            )
            // 结构不认（没有列表、status_code 不是 0）就给空表，不当成错误
            checkEmoji(json)?.let { DouyinEmoteTable.fromList(it, nowSec) } ?: DouyinEmoteTable.EMPTY
        } catch (e: RiskControlException) {
            throw e
        } catch (_: feedhub.core.SourceException) {
            emoteRetryAtMs = nowMs + EMOTE_RETRY_MS
            return stored?.map.orEmpty()
        } catch (_: IOException) {
            emoteRetryAtMs = nowMs + EMOTE_RETRY_MS
            return stored?.map.orEmpty()
        }
        if (!table.isEmpty()) ctx.store.put(DouyinEmoteTable.KEY, table.toStored())
        emoteCache = table
        return table.map
    }

    /** 表情表接口的 `status_code`：不是 0 就当没有（不抛，降级成文字）。 */
    private fun checkEmoji(json: JsonObject): JsonObject? =
        json.long("status_code")?.takeIf { it == 0L }?.let { json }

    /**
     * 用**同一份响应体**再解析一遍（带表情表）。**不发请求**——`RustDouyinSource` 的第一遍是 Rust 的
     * 平台对象做的（它把原始 body 一起交回来），第二遍借这个入口走同一个解析接缝。
     *
     * 合成出来的 [HttpResult] 是 200 + 无风控头：第一遍已经成功解析过同一份 body，传输层 / 业务码判定
     * 早就过了，这里不会再抛；`headers = emptyList()` 也不影响（`checkTransport` 只看 body / 码）。
     */
    internal fun reparseFeed(body: String, refreshIndex: Int, emotes: Map<String, String>): ParsedPage =
        parse.feed(
            HttpResult(
                code = 200,
                url = (base.trimEnd('/') + MODULE_FEED_PATH).toHttpUrl(),
                headers = emptyList(),
                body = body,
            ),
            refreshIndex,
            emotes,
        )

    // ---------- 扫码登录 ----------

    private val qrSessions = QrSessions()

    /** passport 那几个随页面版本变的常量（随包 JSON → `ctx.store` 覆盖），见 [DouyinQr.Profile]。 */
    private val passportProfile: DouyinQr.Profile =
        ctx.store.get(DouyinQr.KEY)?.let { runCatching { DouyinQr.Profile.parse(it) }.getOrNull() }
            ?: DouyinQr.Profile.bundled()

    /**
     * 生成二维码时定下来、轮询要沿用的一小组值：`is_frontier`、`account_sdk_source_info`、`biz_trace_id`。
     * `account_sdk_source_info` 声称的是"同一台浏览器"，**不能每跳重算**（每跳变一次就是自相矛盾的指纹）。
     */
    private class QrState(val frontier: Boolean, val sourceInfo: String, val traceId: String)

    private val qrStates = HashMap<String, QrState>()

    /**
     * 抖音网页扫码登录第一步：`GET https://login.douyin.com/passport/web/get_qrcode/`，
     * 参数（passport SDK 那一套 + `p_no` + `account_sdk_source_info`）由 [DouyinQr] 拼，`a_bogus` 由 [SignInterceptor] 加。
     * 响应的 `data.qrcode_index_url` 是抖音 App 扫的链接（原样当二维码内容），`token` 是轮询用的 key。
     *
     * **签名 / 头 / 设备种子都用现有的那一套**：a_bogus 用 [AbogusProfile]，
     * UA / 窗口 / 核数用 [DouyinDevice]，Cookie 用本机已有的抖音 Cookie（没有也能发，passport 会下发 ttwid 之类）。
     * 设备身份不新造：`account_sdk_source_info` 只写 [DouyinDevice] 里**真有出处**的值，缺的字段不编（见 [DouyinQr.sourceInfo]）。
     *
     * 失败时的映射见 [DouyinQr.data] 和 [checkQrTransport]：`4031` / `2156` → [ApiException]，
     * `7`（轮询限频）/ `2046`（短信二次验证）/ bdturing / 空 body / acrawler 挑战页 → [RiskControlException]，**不重试**。
     *
     * 没做的：参考实现的真实链路还要 `__ac_signature`（执行服务端下发的 acrawler 脚本）、登录域的 `ttwid/check`、
     * `x-tt-session-dtrait`（跑 `dtrait-core` 的 JS）——那都是绕过人机校验，按风控红线不做。
     * `get_qrcode` 和 `check_qrconnect`（未扫）都正常返回了；扫码确认之后
     * 会不会被要求，**没有验证**（没人扫码）。
     */
    override suspend fun startQrLogin(): QrLogin {
        val jar = MemoryCookieJar()
        // 临时罐只带**设备标识**（ttwid / UIFID / s_v_web_id / msToken 之类），不带旧的登录凭据：
        // 带着上一个账号的 sessionid 去要二维码是自相矛盾的（成功后 [commitQrLogin] 还会把它当成新 Cookie 写回去）
        val existing = ctx.cookies.all().filter { isDouyinHost(it.domain) && it.name !in QR_LOGIN_COOKIES }
        if (existing.isNotEmpty()) jar.put(WWW, existing)
        val client = http.newBuilder().cookieJar(jar).build()
        val now = clock()
        val traceId = newTraceId()
        val sourceInfo = DouyinQr.sourceInfo(device, now)
        val params = DouyinQr.getQrParams(passportProfile, now, traceId, sourceInfo, storeOrCookie("douyin.ms_token", "msToken"))
        val generated = DouyinQr.generated(passport(client, "GET", DouyinQr.GET_QR_PATH, params, emptyMap()))
        synchronized(qrStates) { qrStates[generated.token] = QrState(generated.frontier, sourceInfo, traceId) }
        qrSessions.begin(generated.token, jar)
        return QrLogin(
            platform = platform,
            content = generated.url,
            token = generated.token,
            // 平台给了 expire_time（Unix 秒）就用它；没给按参考实现的"二维码约 60 秒"兜底
            expiresAtMs = generated.expireAtSec?.times(1000) ?: (now + QR_TTL_MS),
            pollIntervalMs = QR_POLL_INTERVAL_MS,
            hint = "用抖音 App 扫码，并在手机上确认登录",
        )
    }

    /**
     * 第二步：`POST /passport/web/check_qrconnect/`（`token` 在表单 body 里，`fp` / `verifyFp` 在 query 里）。
     * 状态两种写法都认（字符串 `new/scanned/confirmed/expired`、数字 `1/2/3/5`）：
     * 未扫 → [QrLoginState.Waiting]，已扫 → [QrLoginState.Scanned]，过期 → [QrLoginState.Expired]，
     * `4`（平台把二维码换了）→ [QrLoginState.Failed]（这张已经作废，让用户刷新一张），
     * 确认 → [finishQrLogin]。
     *
     * 风控（bdturing / 限频 / 空 body / 挑战页）抛 [RiskControlException]，弹框里停、不重试；
     * 参考实现遇到 `error_code=7` 会退避重试，我们的规矩是停。
     */
    override suspend fun pollQrLogin(login: QrLogin): QrLoginState {
        val jar = qrSessions.jar(login.token)
        val state = synchronized(qrStates) { qrStates[login.token] }
            ?: QrState(frontier = true, sourceInfo = DouyinQr.sourceInfo(device, clock()), traceId = newTraceId())
        val client = http.newBuilder().cookieJar(jar).build()
        val query = DouyinQr.checkQuery(
            passportProfile,
            clock(),
            state.traceId,
            state.sourceInfo,
            state.frontier,
            verifyFp(),
            storeOrCookie("douyin.ms_token", "msToken"),
        )
        val form = DouyinQr.checkForm(login.token).apply { if (!state.frontier) put("is_frontier", "false") }
        val json = try {
            passport(client, "POST", DouyinQr.CHECK_QR_PATH, query, form)
        } catch (e: RiskControlException) {
            endQr(login.token)
            throw e
        }
        return when (val status = DouyinQr.status(json)) {
            DouyinQr.Status.Waiting -> QrLoginState.Waiting
            DouyinQr.Status.Scanned -> QrLoginState.Scanned
            DouyinQr.Status.Expired -> {
                endQr(login.token)
                QrLoginState.Expired
            }
            DouyinQr.Status.Refreshed -> {
                endQr(login.token)
                QrLoginState.Failed("平台把二维码换了一张（这张已经作废），点「刷新二维码」重新来")
            }
            is DouyinQr.Status.Confirmed -> finishQrLogin(login, client, jar, status.redirectUrl)
        }
    }

    /**
     * 手机上确认之后：先 GET 平台给的 `redirect_url` 落地页（会话 Cookie 是那一步种下来的，照参考实现），
     * 然后用临时罐问一次 `whoami`（`profile/self`）确认真的登录了（[feedhub.core.commitQrLogin]），
     * 通过后清掉旧的登录 Cookie（[QR_LOGIN_COOKIES]），写入临时罐里的全部 Cookie（`ttwid` / `UIFID` / `s_v_web_id` 这些设备标识保留）。
     *
     * 落地页只跟抖音自己的域名走（别的地址不发，免得把临时罐里的 Cookie 送到外面）。
     */
    private suspend fun finishQrLogin(login: QrLogin, client: OkHttpClient, jar: MemoryCookieJar, redirectUrl: String?): QrLoginState {
        val redirect = redirectUrl?.takeIf { it.isNotBlank() }?.let { runCatching { it.toHttpUrl() }.getOrNull() }
        if (redirect != null && isDouyinHost(redirect.host)) {
            // 落地页的响应只是 HTML / 跳转，Cookie 在 Set-Cookie 里；不解析 body，只按传输层风控识别
            checkQrTransport(client.fetch(Request.Builder().url(redirect).get().build()), expectJson = false)
        }
        val state = ctx.commitQrLogin(this.login, QR_LOGIN_COOKIES, jar.all()) {
            DouyinSource(ctx.withCookieJar(platform, jar), base, abogusRandom, clock).whoami()
        }
        endQr(login.token)
        return state
    }

    /**
     * passport 的一次请求。`a_bogus` 由 [SignInterceptor] 加在 query 最后（和别的接口同一条路），
     * POST 的 body 是表单编码（`token=…&is_frontier=…&…`）。
     *
     * **已知缺口**：参考实现的 a_bogus 把 POST 的 body 一起签进去（`params.with_a_bogus(data)`），
     * 而我们的 [AbogusSigner] 只签 query（`abogus_profile.json` 的 `_pending` 里就写着"POST body 签名没实现"）。
     * 没有独立测试向量，不猜着写，
     */
    private suspend fun passport(
        client: OkHttpClient,
        method: String,
        path: String,
        params: Map<String, String>,
        form: Map<String, String>,
    ): JsonObject {
        val query = params.entries.joinToString("&") { (k, v) -> "$k=${formEncode(v)}" }
        val url = (passportBase.trimEnd('/') + path + if (query.isEmpty()) "" else "?$query").toHttpUrl()
        val builder = Request.Builder().url(url).tag(SignTag::class.java, SignTag()).withDefaultHeaders()
        if (method == "POST") {
            val body = form.entries.joinToString("&") { (k, v) -> "${formEncode(k)}=${formEncode(v)}" }
            builder.post(body.toRequestBody(FORM_TYPE))
        } else {
            builder.get()
        }
        val res = client.fetch(builder.build())
        checkQrTransport(res)
        // 返回**整份**响应：`DouyinQr.data` 在 [DouyinQr.generated] / [DouyinQr.status] 里各剥一次外壳
        return parseQrJson(platform, res.body)
    }

    /**
     * 扫码登录的传输层风控识别。和 [checkTransport] 认同一批信号，但**错误信息里不贴 body**
     * （passport 的 body 里可能有我们自己发出去的东西）。
     */
    private fun checkQrTransport(res: HttpResult, expectJson: Boolean = true) {
        fun header(name: String) = res.headers.firstOrNull { it.first.equals(name, ignoreCase = true) }?.second
        if (header("X-Vc-Bdturing-Parameters") != null) throw RiskControlException(platform, "bdturing", "服务端要求过人机验证")
        if (header("X-Tt-Verify-Passport-Decision") != null) throw RiskControlException(platform, "passport-decision", "服务端要求二次身份验证")
        when (res.code) {
            412, 429 -> throw RiskControlException(platform, "HTTP ${res.code}", "被限速 / 风控拦了，停了")
            403 -> throw RiskControlException(platform, "HTTP 403", "网关拒绝了扫码请求（多半要 Argus 签名 / 人机校验），停了")
            in 200..299 -> Unit
            else -> throw ApiException(platform, "HTTP ${res.code}", "扫码登录的 passport 接口回 HTTP ${res.code}")
        }
        // HTTP 200 + 空 body：签名 / 指纹 / WAF 拒绝
        if (res.body.isEmpty()) throw RiskControlException(platform, "empty-body", "服务端返回空 body（签名 / 指纹 / WAF 拒绝）")
        if (res.body.contains("__ac_nonce") || res.body.contains("\$jsvmprt")) {
            throw RiskControlException(platform, "acrawler", "拿到 acrawler 的 JS 挑战页（要执行页面脚本才过），按规矩停了")
        }
        // 只有 passport 的接口要求 JSON；落地页（[finishQrLogin] 的 redirect_url）本来就是 HTML
        if (expectJson && !res.body.trimStart().startsWith("{")) {
            throw RiskControlException(platform, "challenge", "响应不是 JSON（多半是登录风控页），停了")
        }
    }

    private fun endQr(token: String) {
        qrSessions.end(token)
        synchronized(qrStates) { qrStates.remove(token) }
    }

    /**
     * `biz_trace_id`：8 位十六进制（参考实现是 UUID 去掉横线后取前 8 位）。
     *
     * 随机量还是这个 Source 的
     * [DouyinDevice] 那一份）：形状不变、每个扫码请求现生成一个、不落盘。
     */
    internal fun newTraceId(): String = device.traceId()

    // ---------- raw ----------

    override suspend fun raw(request: RawRequest): RawResponse {
        val plain = request.toOkHttpRequest(base)
        val builder = plain.newBuilder().apply { withDefaultHeaders() }
        // 签名交给 [SignInterceptor]；这里只打一个"要签"的标记，sign=false 的不打
        if (request.sign) builder.tag(SignTag::class.java, SignTag())
        val res = http.fetch(builder.build())
        checkTransport(res)
        return res.toRawResponse()
    }

    // ---------- 内部 ----------

    /** [getJson] 的一次调用（参数、请求头、签名口径、接口版本）。 */
    private class Get(
        val path: String,
        val params: Map<String, String> = emptyMap(),
        /** 这个接口要的 Referer（评论是作品页）。null 用默认的站根。 */
        val referer: String? = null,
        /** 哪些"服务端签发 / 页面上才有"的标识进 query、且参与签名。 */
        val ids: Ids = Ids(),
        /** 放在 `a_bogus` **之后**、不参与签名的参数（楼中楼的 `verifyFp` / `fp`）。 */
        val afterSign: Map<String, String> = emptyMap(),
        val versionCode: String = VERSION_CODE,
        val versionName: String = VERSION_NAME,
        val roundTripTime: String = "0",
        val checkCode: Boolean = true,
    )

    /** [Get.ids]：哪些身份参数参与签名。楼中楼要 `uifid=false` / `verifyFp=false`。 */
    private data class Ids(
        val webid: Boolean = true,
        val uifid: Boolean = true,
        val verifyFp: Boolean = true,
        val msToken: Boolean = true,
    )

    private suspend fun getJson(get: Get): JsonObject {
        val res = fetch(get)
        checkTransport(res)
        val json = parseJsonObject(platform, res.body)
        return if (get.checkCode) check(json) else json
    }

    /**
     * 只发请求、不做任何判定（推荐流那条路把 body / 状态码交给接缝 `parse.feed`，
     * 由它一次做完"错误映射 + 解析"；其余接口仍走 [getJson]）。
     */
    private suspend fun fetch(get: Get): HttpResult {
        val url = unsignedUrl(get.path, get.params, get.ids, get.versionCode, get.versionName, get.roundTripTime)
        return http.fetch(
            Request.Builder()
                .url(url)
                .tag(SignTag::class.java, SignTag(get.afterSign))
                .apply {
                    get.referer?.let { header("Referer", it) }
                    withDefaultHeaders()
                }
                .build(),
        )
    }

    /**
     * 按平台规则拼一份**还没有签名**的 URL。
     *
     * `a_bogus`（以及受保护路径的 `timestamp` + `x-secsdk-web-signature`、[Get.afterSign] 那几个）由 [SignInterceptor] 加上。
     *
     * 值按页面那种"表单式"编码（[formEncode]：空格是 `+`，不是 `%20`）。
     * 页面自己拼的参数（`browser_platform=Linux+x86_64`）**13 条全是 `+`**，
     * 而 a_bogus 签的正是**改写前的那串原样字符**。受 secsdk 保护的路径上，secsdk 会把这串再规范化成 `%20` 才发出去（和真实页面一样），
     * 所以两条路径的编码都和真实页面一致。
     *
     * OkHttp 解析这个 URL 时会做一次规范化（例如把 `'` 编成 `%27`），签名签的正是规范化**之后**的串，
     * 所以签名和实际发出去的永远一致。
     */
    private fun unsignedUrl(
        path: String,
        params: Map<String, String>,
        ids: Ids,
        versionCode: String,
        versionName: String,
        roundTripTime: String,
    ): HttpUrl {
        val all = LinkedHashMap<String, String>()
        all.putAll(params)
        all.putAll(runtimeParams(versionCode, versionName, roundTripTime))
        identityParams(ids).forEach { (k, v) -> if (k !in all) all[k] = v }
        val query = all.entries.joinToString("&") { (k, v) -> "$k=${formEncode(v)}" }
        return (base.trimEnd('/') + path + if (query.isEmpty()) "" else "?$query").toHttpUrl()
    }

    /**
     * `encodeURIComponent` **再把空格写成 `+`**（页面用的 `URLSearchParams` / 表单编码就是这个行为）。
     *
     * 抓包证据：真实页面的 `browser_platform` 是 `Linux+x86_64`（13/13 条），受 secsdk 保护的路径上
     * 才被改写成 `Linux%20x86_64`。a_bogus 签的是**页面拼出来的原串**，所以这里也拼成 `+`。
     * 其余字符的编码规则不变（`%2C` / `%7B` 之类在真实页面的 query 里也都是 `encodeURIComponent`
     * 的样子）。
     */
    private fun formEncode(value: String): String = SecSdkSigner.encodeComponent(value).replace("%20", "+")

    /**
     * a_bogus / secsdk 的签名拦截器。
     *
     * 排在限速拦截器**之后**，所以：
     * - 时间戳是"真要发出去"的那一刻，不会比实际发送早一整个限速间隔；
     * - 签的内容直接从要发出去的请求里读（[HttpUrl.encodedQuery]，已经过 OkHttp 规范化），
     *   不会出现"签的是 `'`、发的是 `%27`"这种不一致。
     */
    private inner class SignInterceptor : Interceptor {
        override fun intercept(chain: Interceptor.Chain): Response {
            val request = chain.request()
            val tag = request.tag(SignTag::class.java) ?: return chain.proceed(request)
            return chain.proceed(request.newBuilder().url(signedUrl(request.url, request.method, tag.afterSign)).build())
        }
    }

    /**
     * 给 [url] 补上签名：先 a_bogus，受保护路径再补 `timestamp` + `x-secsdk-web-signature`，
     * 最后是 [afterSign] 那几个**不参与签名**的参数（楼中楼的 `verifyFp` / `fp`）。
     *
     * 一个**残留的边角**（受保护路径才有）：secsdk 的规范化会先解码再按 `encodeURIComponent` 重编码，
     * 所以值里带 `'` 时它会算成 `'`，而 OkHttp 把最终 URL 再规范化一次会写成 `%27`——这一处不一致
     * 只在受保护路径 + 值里带 `'` 时出现（`aweme/detail` 的参数是数字 id，不受影响）。
     */
    private fun signedUrl(url: HttpUrl, method: String, afterSign: Map<String, String>): HttpUrl {
        val query = url.encodedQuery.orEmpty()
        val bogus = signer.abogus(query, clock())
        var signed = if (query.isEmpty()) "a_bogus=$bogus" else "$query&a_bogus=$bogus"
        if (SecSdkSigner.isProtected(url.encodedPath, method)) {
            val secsdk = signer.secSdk(signed, uifid(), clock() / 1000)
            signed = "${secsdk.query}&${SecSdkSigner.HEADER}=${secsdk.signature}"
        }
        for ((k, v) in afterSign) signed += "&$k=${formEncode(v)}"
        return url.newBuilder().encodedQuery(signed).build()
    }

    private fun runtimeParams(versionCode: String, versionName: String, roundTripTime: String): Map<String, String> =
        linkedMapOf(
            "device_platform" to "webapp",
            "aid" to AID,
            "channel" to "channel_pc_web",
            "update_version_code" to UPDATE_VERSION_CODE,
            "pc_client_type" to "1",
            "pc_libra_divert" to device.libraDivert,
            "support_h265" to "1",
            "support_dash" to "1",
            "cpu_core_num" to device.cpuCoreNum.toString(),
            "version_code" to versionCode,
            "version_name" to versionName,
            "cookie_enabled" to "true",
            "screen_width" to device.screenWidth.toString(),
            "screen_height" to device.screenHeight.toString(),
            "browser_language" to "zh-CN",
            "browser_platform" to device.platform,
            "browser_name" to "Chrome",
            "browser_version" to device.browserMajor + ".0.0.0",
            "browser_online" to "true",
            "engine_name" to "Blink",
            "engine_version" to device.browserMajor + ".0.0.0",
            "os_name" to device.osName,
            "os_version" to device.osVersion,
            "device_memory" to device.deviceMemory.toString(),
            "platform" to "PC",
            "downlink" to "10",
            "effective_type" to "4g",
            "round_trip_time" to roundTripTime,
        )

    /**
     * 三个**服务端签发 / 页面上才拿得到**的标识。本机没有就**不塞**：
     *
     * - `uifid`：Cookie `UIFID` / `UIFID_TEMP`，真实可用。
     * - `webid`：页面上 `user_unique_id` 或 `POST mcs.zijieapi.com/webid`；**当前拿不到**，
     *   需要在线步骤取一次存进 `ctx.store` 的 `douyin.webid`。
     * - `msToken`：服务端签发（`POST mssdk.bytedance.com/web/r/token`，登录响应头 `X-Ms-Token` 也会带）；
     *   **当前拿不到**，需要在线步骤存进 `ctx.store` 的 `douyin.ms_token`。
     *
     * 缺这两个值会不会被服务端静默拒绝，`module/feed` / `profile/self`
     * 在没有它们的情况下是通的。
     */
    private fun identityParams(ids: Ids): Map<String, String> = linkedMapOf<String, String>().apply {
        if (ids.webid) storeOrCookie("douyin.webid", "webid")?.let { put("webid", it) }
        if (ids.uifid) uifid()?.let { put("uifid", it) }
        // 这样 query 里的 verifyFp / fp 和随请求发出去的 Cookie 一致；没有才用本地生成并记下来的那个。
        if (ids.verifyFp) {
            val fp = verifyFp()
            put("verifyFp", fp)
            put("fp", fp)
        }
        if (ids.msToken) storeOrCookie("douyin.ms_token", "msToken")?.let { put("msToken", it) }
    }

    private fun verifyFp(): String =
        ctx.cookies.find(WWW, "s_v_web_id")?.takeIf { it.isNotBlank() } ?: device.verifyFp

    private fun storeOrCookie(storeKey: String, cookieName: String): String? =
        ctx.store.get(storeKey)?.takeIf { it.isNotBlank() }
            ?: ctx.cookies.find(WWW, cookieName)?.takeIf { it.isNotBlank() }

    private fun uifid(): String? =
        ctx.cookies.find(WWW, "UIFID")?.takeIf { it.isNotBlank() }
            ?: ctx.cookies.find(WWW, "UIFID_TEMP")?.takeIf { it.isNotBlank() }

    /** 且和 UA 一致。 */
    private fun headers(): Map<String, String> = linkedMapOf(
        "User-Agent" to device.userAgent,
        "Accept" to "application/json,text/plain,*/*",
        "Accept-Language" to "zh-CN,zh;q=0.9,en;q=0.8,zh-TW;q=0.7,ja;q=0.6",
        "sec-ch-ua" to "\"Not=A?Brand\";v=\"99\", \"Google Chrome\";v=\"${device.browserMajor}\", " +
            "\"Chromium\";v=\"${device.browserMajor}\"",
        "sec-ch-ua-mobile" to "?0",
        "sec-ch-ua-platform" to device.uaPlatform,
        "priority" to "u=1, i",
        "sec-fetch-dest" to "empty",
        "sec-fetch-mode" to "cors",
        "sec-fetch-site" to "same-origin",
        "Referer" to "https://www.douyin.com/",
        // 称网关只校验这个头是否存在；**没有实测**
        "x-tt-argus" to "1",
    )

    /** 每次请求都补一遍按当前 Cookie 算出来的头（`uifid` 等）。 */
    private fun Request.Builder.withDefaultHeaders(): Request.Builder = apply {
        uifid()?.let { header("uifid", it) }
    }

    // ---------- 风控 / 错误映射 ----------

    /** 传输层的风控信号。识别到就停下。（`internal`：`KotlinDouyinParse` 复用同一份判定） */
    internal fun checkTransport(res: HttpResult) {
        fun header(name: String) = res.headers.firstOrNull { it.first.equals(name, ignoreCase = true) }?.second

        // 触发人机验证 / 二次身份验证：看响应头
        if (header("X-Vc-Bdturing-Parameters") != null) {
            throw RiskControlException(platform, "bdturing", "服务端要求过人机验证")
        }
        if (header("X-Tt-Verify-Passport-Decision") != null) {
            throw RiskControlException(platform, "passport-decision", "服务端要求二次身份验证")
        }
        if (res.code == 403) {
            val body = res.body
            when {
                body.contains("ArgusSecurityPlugin") ->
                    throw SignatureException(platform, "网关 Argus 拒绝（缺 x-tt-argus / uifid）")
                body.contains("Sign Invalid") ->
                    throw SignatureException(platform, "a_bogus / x-secsdk-web-signature 被拒")
                body.contains("Signature Not Found") ->
                    throw SignatureException(platform, "网关没收到签名")
                else -> throw ApiException(platform, "HTTP 403", body.take(120))
            }
        }
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(120))
        // HTTP 200 + 空 body：签名 / 指纹 / WAF 拒绝
        if (res.body.isEmpty()) {
            throw RiskControlException(platform, "empty-body", "服务端返回空 body（签名 / 指纹 / WAF 拒绝）")
        }
        // acrawler 挑战页
        if (res.body.contains("__ac_nonce") || res.body.contains("\$jsvmprt")) {
            throw RiskControlException(platform, "acrawler", "拿到 JS 挑战页，不是接口响应")
        }
    }

    /** 业务码。（`internal`：`KotlinDouyinParse` 复用同一份判定） */
    internal fun check(json: JsonObject): JsonObject {
        val code = json.long("status_code") ?: throw ParseException(platform, "响应缺少 status_code")
        val msg = json.text("status_msg") ?: json.text("message").orEmpty()
        return when (code) {
            0L -> json
            // 8 = 未登录；被拒的写操作也会让整个会话作废，之后读接口马上返回 8
            8L -> throw NotLoggedInException(platform)
            5L -> throw ApiException(platform, "5", "这个接口只接受 POST$msg")
            2096L -> throw ApiException(platform, "2096", "对方设置了隐私，列表不可见")
            200010001L -> throw ApiException(platform, "200010001", "rpc failed（多半缺 x-secsdk-csrf-token）")
            else -> throw ApiException(platform, code.toString(), msg)
        }
    }

    /** 这条请求要不要签名、`afterSign` 有哪些参数。[raw] 的 `sign = false` 不打这个标记（调试用）。 */
    private class SignTag(val afterSign: Map<String, String> = emptyMap())

    private companion object {
        const val API = DOUYIN_API
        val WWW = "https://www.douyin.com/".toHttpUrl()

        const val MODULE_FEED_PATH = "/aweme/v1/web/module/feed/"
        const val PROFILE_SELF_PATH = "/aweme/v1/web/user/profile/self/"
        const val AWEME_DETAIL_PATH = "/aweme/v1/web/aweme/detail/"
        const val COMMENT_LIST_PATH = "/aweme/v1/web/comment/list/"
        const val COMMENT_REPLY_PATH = "/aweme/v1/web/comment/list/reply/"
        const val EMOJI_LIST_PATH = "/aweme/v1/web/emoji/list"

        const val MODULE_ID = "3003101"
        const val AID = "6383"
        const val PAGE_SIZE = 20
        const val COMMENT_PAGE_SIZE = 20

        /** cv-cat：楼中楼翻页用 5（第一页它用 3），这里统一 5。 */
        const val REPLY_PAGE_SIZE = 5

        /** 大多数接口的公共组。 */
        const val VERSION_CODE = "170400"
        const val VERSION_NAME = "17.4.0"
        const val UPDATE_VERSION_CODE = "170400"

        /** `aweme/detail` 的前端模块自带另一套版本号。 */
        const val DETAIL_VERSION_CODE = "190500"
        const val DETAIL_VERSION_NAME = "19.5.0"

        /** 表情表取不到时多久不再试。 */
        const val EMOTE_RETRY_MS = 10 * 60_000L

        /** 判定登录态用的 Cookie。 */
        val LOGIN_COOKIES = setOf("sessionid", "sessionid_ss", "sid_tt")

        const val MIN_INTERVAL_MS = 1000L
        const val MIN_INTERVAL_JITTER_MS = 500L

        /** 扫码登录的 passport 主机（www 和 login 是两个域，见 [passportBase]）。 */
        const val LOGIN = "https://login.douyin.com"

        /** 轮询间隔：浏览器实测 5.14 秒一发（cv-cat 注释），取 5 秒。 */
        const val QR_POLL_INTERVAL_MS = 5000L

        /** 平台没给 `expire_time` 时的兜底：参考实现说二维码约 60 秒失效。 */
        const val QR_TTL_MS = 60_000L

        /** 换账号时要清掉的登录 Cookie；设备标识（`ttwid` / `UIFID` / `s_v_web_id` / `msToken`）保留。 */
        val QR_LOGIN_COOKIES = setOf(
            "sessionid", "sessionid_ss", "sid_tt", "sid_guard",
            "uid_tt", "uid_tt_ss", "passport_auth_status", "passport_auth_status_ss",
        )

        /** passport 的表单编码。 */
        val FORM_TYPE = "application/x-www-form-urlencoded".toMediaType()
    }
}

/** 抖音 web 的接口根（测试指向 MockWebServer）。 */
internal const val DOUYIN_API = "https://www.douyin.com"

/** 扫码登录的 passport 主机（和 `DouyinSource.LOGIN` 一样；`companion` 是私有的，这里单独一份）。 */
internal const val DOUYIN_LOGIN = "https://login.douyin.com"

/**
 * [host] 是不是抖音的域名：`douyin.com` 本身，或者以 `.douyin.com` 结尾。
 * 不能写成 `endsWith("douyin.com")`——那样 `notdouyin.com` 也会命中。
 */
internal fun isDouyinHost(host: String): Boolean {
    val h = host.lowercase().substringBefore(':')
    return h == "douyin.com" || h.endsWith(".douyin.com")
}
