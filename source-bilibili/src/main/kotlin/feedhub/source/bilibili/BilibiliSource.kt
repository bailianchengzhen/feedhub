package feedhub.source.bilibili

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.FollowKind
import feedhub.core.FollowSource
import feedhub.core.FollowTarget
import feedhub.core.LoginSpec
import feedhub.core.MemoryCookieJar
import feedhub.core.MinIntervalInterceptor
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.QrSessions
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RiskControlException
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SignatureException
import feedhub.core.SourceContext
import feedhub.core.UnsupportedFeedException
import feedhub.core.UserAgents
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.commitQrLogin
import feedhub.core.fetch
import feedhub.core.long
import feedhub.core.parseJsonObject
import feedhub.core.parseQrJson
import feedhub.core.path
import feedhub.core.recordHttpCall
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import feedhub.core.withCookieJar
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Cookie
import okhttp3.FormBody
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Request
import okhttp3.Response
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId

/**
 * B站：网页接口 + Cookie。
 * - 推荐流 `/x/web-interface/wbi/index/top/feed/rcmd`：未登录也能用（内容是通用推荐），按网页的做法加 WBI 签名。
 * - 关注动态 `/x/polymer/web-dynamic/v1/feed/all`：要登录，不用签名。
 * - 视频详情 `/x/web-interface/wbi/view/detail`（View + Card + Related 一次拿到）；评论 `/x/v2/reply/wbi/main`（WBI 签名），
 *   楼中楼 `/x/v2/reply/reply`。评论区的 oid 是 aid，由 BV 号直接算（[BvAv]）。
 *
 * - 播放地址 `/x/player/wbi/playurl`（`fnval=4048` 要 DASH 全家桶），弹幕 `/x/v2/dm/wbi/web/seg.so`（protobuf，每段 6 分钟）。
 *
 * - 关注 / 取关 `/x/relation/modify`（POST，csrf 取自 Cookie 的 `bili_jct`），关注列表 `/x/relation/followings`。
 * - 动态 / 图文 / 专栏：`/x/polymer/web-dynamic/v1/detail` + `opus/detail`（全文），评论区的 type / oid 取自详情的 `basic`。
 *
 * - 扫码登录 [startQrLogin] / [pollQrLogin]：网页端 `passport.bilibili.com/x/passport-login/web/qrcode/{generate,poll}`，见方法说明。
 *
 * **Native id**（详见 [BiliIds]）：视频就是 `BV…`，和信息流里的 [feedhub.core.FeedItem.id] 相同；
 * 动态 / 图文 / 专栏是 `dyn:<id_str>`（`FeedItem.id` 是不带前缀的 `id_str`）。直播、番剧继续 [feedhub.core.DetailTarget.Web]。
 *
 * [base] 只给测试用（指向 MockWebServer）。
 */
class BilibiliSource internal constructor(
    private val ctx: SourceContext,
    private val base: String,
    /**
     * 解析 / 签名的实现。默认是 Kotlin 自己的（[KotlinBiliParse]，和以前逐字节一样）；
     * 打开了「实验：B站 推荐 / 关注用 Rust 解析」时是 [RustBiliParse]。
     * **HTTP 永远是同一套 OkHttp**，这个参数只决定"拿到 body 之后谁来解析、签名谁来算"。
     */
    private val parse: BiliParse = KotlinBiliParse,
    /** WBI 签名用的秒级时间戳。测试里注入固定值，才能把开关开 / 关两次的请求逐字节比。 */
    private val nowSec: () -> Long = { System.currentTimeMillis() / 1000 },
) : VideoSource, FollowSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, API)

    // 身份、流、登录、域名都以插件描述为准（[BilibiliPlugin]），这里不再写第二份
    override val platform = BILIBILI
    override val feeds = BilibiliPlugin.capabilities.feeds
    override val login = BilibiliPlugin.login.web
    override val apiBase = base
    override val hosts = BilibiliPlugin.hosts
    override val search = BilibiliPlugin.capabilities.search!!

    private val http = ctx.client(platform)
        .addInterceptor(
            DefaultHeadersInterceptor(
                mapOf(
                    "User-Agent" to UserAgents.DESKTOP_CHROME,
                    "Referer" to "https://www.bilibili.com/",
                    "Origin" to "https://www.bilibili.com",
                ),
                // `notbilibili.com` 这类域名不算
                hostFilter = { it == "bilibili.com" || it.endsWith(".bilibili.com") || it == base.toHttpUrl().host },
            ),
        )
        .addInterceptor(MinIntervalInterceptor(MIN_INTERVAL_MS))
        .build()

    /**
     * **平台客户端**（默认头 + 500ms 限速，套在 `ctx.client(platform)` 上：连接池 / Cookie 罐 / 代理选择器都是共享的那份）。
     *
     * 只给 [RustBiliSource] 用：Rust 的宿主回调必须用**同一个对象**发请求，否则限速会变成两套
     * （各自的 `MinIntervalInterceptor` 实例各有一个"下次允许的时间"），环回和拦截器链也会多一份。
     */
    internal val httpClient get() = http

    private val buvidMutex = Mutex()
    private val wbiMutex = Mutex()

    override suspend fun whoami(): UserRef? {
        val json = getJson("/x/web-interface/nav", emptyMap(), checkCode = false)
        return when (json.long("code")) {
            0L -> BiliParser.nav(json)
            -101L -> null
            else -> check(json).let { BiliParser.nav(it) }
        }
    }

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> recommend(cursor)
        FeedKind.FOLLOWING -> following(cursor)
        else -> throw UnsupportedFeedException(platform, kind)
    }

    private suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        ensureBuvid()
        val idx = cursor?.value?.toIntOrNull() ?: 1
        // 参数照网页首页和 PiliPlus `lib/http/video.dart` 的 rcmdVideoList
        val params = linkedMapOf(
            "web_location" to "1430650",
            "feed_version" to "V8",
            "homepage_ver" to "1",
            "fresh_type" to "4",
            "ps" to "12",
            "fresh_idx" to idx.toString(),
            "fresh_idx_1h" to idx.toString(),
            "brush" to idx.toString(),
        )
        // body 直接交给解析器：Kotlin 自己解析时和以前一样（parseJsonObject + check），
        // Rust 解析时**只解析一遍**（外壳检查也在 Rust 里，错误码映射回同样的异常）。
        return parse.recommend(getBody("/x/web-interface/wbi/index/top/feed/rcmd", params, wbi = true), idx)
    }

    private suspend fun following(cursor: Cursor?): Page<FeedItem> {
        ensureBuvid()
        val params = linkedMapOf(
            "type" to "all",
            "timezone_offset" to "-480",
            "platform" to "web",
            "features" to DYN_FEATURES,
        )
        cursor?.let { params["offset"] = it.value }
        return parse.dynamics(getBody("/x/polymer/web-dynamic/v1/feed/all", params))
    }

    // ---------- 搜索 ----------

    /**
     * 搜索：`/x/web-interface/wbi/search/type`（WBI 签名，现成的 [signUrl]），
     * 必带 `Origin` / `Referer: https://search.bilibili.com`（照参考实现 ApiClient.kt:1565-1585）。
     * 游标是页码；`-412` / `-352` 等按 B站统一错误映射抛 [RiskControlException]，**不重试**。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        ensureBuvid()
        val page = cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1
        val params = BiliSearchParser.params(request, page, search.pageSize)
        val plain = (base + "/x/web-interface/wbi/search/type").toHttpUrl().newBuilder().apply {
            params.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build()
        val res = http.fetch(
            Request.Builder().url(signUrl(plain))
                .header("Origin", "https://search.bilibili.com")
                .header("Referer", "https://search.bilibili.com/")
                .build(),
        )
        if (res.code == 412) throw RiskControlException(platform, "HTTP 412", "请求被拦截")
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(200))
        return BiliSearchParser.page(check(parseJsonObject(platform, res.body)), page)
    }

    // ---------- 详情和评论 ----------

    override suspend fun detail(id: String): FeedDetail {
        ensureBuvid()
        return when (val t = parseId(id)) {
            is BiliId.Video -> {
                val json = getJson("/x/web-interface/wbi/view/detail", linkedMapOf("bvid" to t.bvid), wbi = true)
                BiliVideoParser.detail(json, loggedIn = loggedIn())
            }
            is BiliId.Dynamic -> dynamicDetail(t.id)
        }
    }

    /**
     * 动态 / 图文 / 专栏：`v1/detail` 给条目、计数、转发的原动态和评论区位置；图文和专栏的全文另从 `opus/detail` 取。
     * `v1/detail` 没登录会 -352，这里翻译成"需要登录"。
     */
    private suspend fun dynamicDetail(dynId: String): FeedDetail {
        val json = dynamicJson(dynId)
        BiliDynamicParser.commentTarget(json)?.let { cacheTarget(BiliIds.dynamic(dynId), it) }
        val opus = if (BiliDynamicParser.isOpus(json)) {
            try {
                getJson("/x/polymer/web-dynamic/v1/opus/detail", linkedMapOf("id" to dynId, "timezone_offset" to "-480", "features" to "htmlNewStyle"))
            } catch (_: ApiException) {
                null // 全文拿不到就用动态自带的摘要
            }
        } else null
        return BiliDynamicParser.detail(json, opus)
    }

    private suspend fun dynamicJson(dynId: String): JsonObject = try {
        getJson("/x/polymer/web-dynamic/v1/detail", linkedMapOf("id" to dynId, "timezone_offset" to "-480", "features" to DYN_FEATURES))
    } catch (e: RiskControlException) {
        if (!loggedIn() && e.code == "-352") throw NotLoggedInException(platform, "动态详情需要登录") else throw e
    }

    /**
     * 一级评论：`mode` 3 热门 / 2 最新（默认热门），翻页用 `pagination_str={"offset":"…"}`，游标就是上一页给的 `next_offset`。
     * 参数照网页（`web_location=1315875`）。评论区的 `type` / `oid`：视频是 (1, aid)，动态要看详情里的 `basic`（见 [commentTarget]）。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        ensureBuvid()
        val target = commentTarget(id)
        val params = linkedMapOf(
            "oid" to target.oid,
            "type" to target.type.toString(),
            "mode" to if (sort == CommentSort.NEWEST) "2" else "3",
            "pagination_str" to buildJsonObject { put("offset", cursor?.value.orEmpty()) }.toString(),
            "plat" to "1",
            "web_location" to "1315875",
        )
        return BiliCommentParser.mainList(getJson("/x/v2/reply/wbi/main", params, wbi = true))
    }

    /** 楼中楼：页码翻页（游标是下一页的页码），每页 20 条。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        ensureBuvid()
        val target = commentTarget(id)
        val pn = cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1
        val params = linkedMapOf(
            "oid" to target.oid,
            "type" to target.type.toString(),
            "root" to commentId,
            "pn" to pn.toString(),
            "ps" to SUB_REPLY_PAGE_SIZE.toString(),
        )
        return BiliCommentParser.subReplies(getJson("/x/v2/reply/reply", params), pn, SUB_REPLY_PAGE_SIZE)
    }

    private fun loggedIn() = ctx.cookies.find(WWW, "SESSDATA") != null

    private fun parseId(id: String): BiliId = BiliIds.parse(id) ?: throw ApiException(platform, "-400", "不认识的内容 id：$id")

    /** 评论区位置按 Native id 缓存：动态的 oid 不是 id 本身，要先查一次详情；打开详情时已经顺手存了。 */
    private val targets = object : LinkedHashMap<String, BiliDynamicParser.CommentTarget>(64, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, BiliDynamicParser.CommentTarget>) = size > TARGET_CACHE_SIZE
    }

    private fun cacheTarget(id: String, t: BiliDynamicParser.CommentTarget) = synchronized(targets) { targets[id] = t }

    private suspend fun commentTarget(id: String): BiliDynamicParser.CommentTarget = when (val t = parseId(id)) {
        is BiliId.Video -> BiliDynamicParser.CommentTarget(1, aidOf(t.bvid).toString())
        is BiliId.Dynamic -> {
            val key = BiliIds.dynamic(t.id)
            synchronized(targets) { targets[key] } ?: (BiliDynamicParser.commentTarget(dynamicJson(t.id))
                ?: throw ParseException(platform, "动态 ${t.id} 的详情里没有评论区信息")).also { cacheTarget(key, it) }
        }
    }

    // ---------- 关注 ----------

    /** 当前账号关注的 UP 主，按最近关注倒序，每页 50 个，游标是页码。 */
    override suspend fun followings(cursor: Cursor?): Page<FollowTarget> {
        ensureBuvid()
        val pn = cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1
        val params = linkedMapOf(
            "vmid" to selfMid(),
            "pn" to pn.toString(),
            "ps" to FOLLOWINGS_PAGE_SIZE.toString(),
            "order" to "desc",
            "order_type" to "",
        )
        return BiliFollowParser.followings(getJson("/x/relation/followings", params), pn, FOLLOWINGS_PAGE_SIZE)
    }

    /**
     * 关注（act=1）/ 取关（act=2）。这是唯一的写操作：只在用户点按钮时调用，**不重试**。
     * 请求照网页（bilby `RelationRepository.modify`、PiliPlus `video.dart` 的 `relationMod`）：
     * `statistics` 和 `x-bili-device-req-json` 放 query，业务字段放表单，Referer / Origin 指向 UP 主的空间页。
     * 关注时服务端回 22014（已经关注了）按成功处理。
     */
    override suspend fun setFollowing(target: FollowTarget, follow: Boolean) {
        if (target.platform != platform || target.kind != FollowKind.USER || target.id.isEmpty() || !target.id.all(Char::isDigit)) {
            throw ApiException(platform, "-400", "B站只能关注 UP 主（kind=USER，id 是 mid）：${target.kind} ${target.id}")
        }
        val csrf = ctx.cookies.find(WWW, "bili_jct")?.takeIf { it.isNotEmpty() } ?: throw NotLoggedInException(platform)
        ensureBuvid()
        val url = (base + "/x/relation/modify").toHttpUrl().newBuilder()
            .addQueryParameter("statistics", """{"appId":100,"platform":5}""")
            .addQueryParameter("x-bili-device-req-json", """{"platform":"web","device":"pc","spmid":"333.1387"}""")
            .build()
        val form = FormBody.Builder()
            .add("fid", target.id)
            .add("act", if (follow) "1" else "2")
            .add("re_src", RE_SRC_SPACE)
            .add("gaia_source", "web_main")
            .add("spmid", "333.1387")
            .add("extend_content", """{"entity":"user","entity_id":${target.id},"fp":"${UserAgents.DESKTOP_CHROME}"}""")
            .add("csrf", csrf)
            .build()
        val request = Request.Builder().url(url).post(form)
            .header("Referer", "https://space.bilibili.com/${target.id}/dynamic")
            .header("Origin", "https://space.bilibili.com")
            .build()
        val res = http.fetch(request)
        if (res.code == 412) throw RiskControlException(platform, "HTTP 412", "请求被拦截")
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(200))
        val json = parseJsonObject(platform, res.body)
        if (follow && json.long("code") == CODE_ALREADY_FOLLOWING) return
        check(json)
    }

    /** 自己的 mid：Cookie 里的 `DedeUserID`，没有就问 nav。 */
    private suspend fun selfMid(): String =
        ctx.cookies.find(WWW, "DedeUserID")?.takeIf { it.isNotEmpty() && it.all(Char::isDigit) }
            ?: whoami()?.id
            ?: throw NotLoggedInException(platform)

    // ---------- 播放和弹幕 ----------

    /**
     * 播放地址。`fnval=4048` + `fourk=1` 要全部 DASH 变体（各清晰度 × AVC / HEVC / AV1，另有杜比 / 无损音轨），
     * `qn` 没指定时用 127（8K，即"能给的最高"），B站只会给当前账号有权限的那些。
     * 拉流要带 [PLAY_HEADERS]（Referer + UA），否则 CDN 403。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        ensureBuvid()
        val params = linkedMapOf(
            "bvid" to bvidOf(id),
            "cid" to partId,
            "qn" to (quality ?: 127).toString(),
            "fnval" to "4048",
            "fnver" to "0",
            "fourk" to "1",
        )
        return BiliPlayParser.play(getJson("/x/player/wbi/playurl", params, wbi = true), PLAY_HEADERS)
    }

    /**
     * 弹幕第 [segment] 段（从 1 开始，每段 6 分钟）。响应是裸 protobuf；超出视频长度的段 B站回 304 空响应，这里按空列表处理。
     * 出错时（风控等）B站回的是 JSON，不是 protobuf。
     */
    override suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> {
        ensureBuvid()
        val aid = aidOf(bvidOf(id))
        val params = linkedMapOf(
            "type" to "1",
            "oid" to partId,
            "pid" to aid.toString(),
            "segment_index" to segment.toString(),
        )
        val url = signUrl((base + "/x/v2/dm/wbi/web/seg.so").toHttpUrl().newBuilder().apply {
            params.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build())
        val (code, bytes) = http.newCall(Request.Builder().url(url).build()).awaitBytes()
        if (code == 304) return emptyList()
        if (code == 412) throw RiskControlException(platform, "HTTP 412", "请求被拦截")
        if (code !in 200..299) throw ApiException(platform, "HTTP $code", "弹幕请求失败")
        if (bytes.firstOrNull() == '{'.code.toByte()) {
            check(parseJsonObject(platform, String(bytes, Charsets.UTF_8))) // code 非 0 会抛
            return emptyList()
        }
        return BiliDanmakuParser.segment(bytes)
    }

    /** 和 [feedhub.core.await] 一样，只是 body 读成字节（protobuf 不能走 String）。 */
    private suspend fun Call.awaitBytes(): Pair<Int, ByteArray> {
        // 和 core 的 `Call.await()` 一样记一笔（对外接口的 done.requests 按调用计数）
        recordHttpCall()
        return suspendCancellableCoroutine { cont ->
            cont.invokeOnCancellation { cancel() }
            enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (cont.isActive) cont.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    val result = try {
                        response.use { it.code to (it.body?.bytes() ?: ByteArray(0)) }
                    } catch (e: IOException) {
                        if (cont.isActive) cont.resumeWithException(e)
                        return
                    }
                    cont.resume(result)
                }
            })
        }
    }

    /** 播放和弹幕只对视频有意义。 */
    private fun bvidOf(id: String): String =
        id.takeIf(BvAv::isBvid) ?: throw ApiException(platform, "-400", "不是视频 id：$id")

    private fun aidOf(bvid: String): Long =
        BvAv.toAid(bvid) ?: throw ApiException(platform, "-400", "无效的 BV 号：$bvid")

    override suspend fun raw(request: RawRequest): RawResponse {
        if (request.url.startsWith("/") || request.url.contains("bilibili.com")) ensureBuvid()
        var req = request.toOkHttpRequest(apiBase)
        // 路径里带 /wbi/ 的接口，以及少数路径里不带但也要签的接口，加 WBI 签名
        if (request.sign && ("/wbi/" in req.url.encodedPath || req.url.encodedPath in WBI_PATHS)) {
            val withDmImg = if (req.url.encodedPath in DM_IMG_PATHS) {
                req.url.newBuilder().apply { DmImgParams.next().forEach { (k, v) -> setQueryParameter(k, v) } }.build()
            } else req.url
            req = req.newBuilder().url(signUrl(withDmImg)).build()
        }
        return http.fetch(req).toRawResponse()
    }

    // ---------- 扫码登录 ----------

    private val qrSessions = QrSessions()

    /** 扫码登录接口的主机。测试里（[base] 指向 MockWebServer）所有接口都在同一个地址。 */
    private val passportBase = if (base == API) PASSPORT else base

    /**
     * 网页端扫码登录第一步：`GET passport.bilibili.com/x/passport-login/web/qrcode/generate`，没有参数、不需要 Cookie。
     * 响应 `data.url` 是 B站 App 扫的地址（`account.bilibili.com/h5/account-h5/auth/scan-web?...qrcode_key=…`），原样当二维码内容；
     * `data.qrcode_key` 是轮询用的 key；二维码 180 秒有效。
     * 全程用独立的临时 Cookie 罐，不碰 `ctx.cookies`。
     */
    override suspend fun startQrLogin(): QrLogin {
        val jar = MemoryCookieJar()
        val generated = BiliQrParser.generated(qrGet(jar, "/x/passport-login/web/qrcode/generate", emptyMap()))
        qrSessions.begin(generated.key, jar)
        return QrLogin(
            platform = platform,
            content = generated.url,
            token = generated.key,
            expiresAtMs = System.currentTimeMillis() + QR_TTL_MS,
            hint = "用哔哩哔哩 App 扫码，并在手机上确认登录",
        )
    }

    /**
     * 第二步：`GET …/web/qrcode/poll?qrcode_key=…`（普通短请求，不是长轮询）。外壳 `code` 为 0 时看 `data.code`：
     * 86101 未扫码 → Waiting，86090 已扫码待确认 → Scanned，86038 失效 → Expired，0 成功，其他 → Failed。
     *
     * 成功时登录 Cookie（SESSDATA / bili_jct / DedeUserID / DedeUserID__ckMd5，可能还有 sid）由响应的 Set-Cookie 种进临时罐，
     * 罐里没有 SESSDATA 时改用 `data.url` 里的参数（[BiliQrParser.cookiesFromUrl]）。
     * 然后用临时罐问一次 nav 确认确实登录了，再清掉旧的登录 Cookie（[BiliQrParser.OLD_LOGIN_NAMES]，buvid3 等设备标识保留）、写入新的。
     * `data.refresh_token`（网页 Cookie 刷新用）我们不做 Cookie 刷新，不保存。
     */
    override suspend fun pollQrLogin(login: QrLogin): QrLoginState {
        val jar = qrSessions.jar(login.token)
        val poll = BiliQrParser.poll(qrGet(jar, "/x/passport-login/web/qrcode/poll", mapOf("qrcode_key" to login.token)))
        BiliQrParser.waiting(poll.code)?.let { state ->
            if (state == QrLoginState.Expired) qrSessions.end(login.token)
            return state
        }
        if (poll.code != 0L) {
            qrSessions.end(login.token)
            return QrLoginState.Failed("扫码登录失败（${poll.code} ${poll.message}）")
        }
        if (jar.all().none { it.name == "SESSDATA" }) jar.put(WWW, BiliQrParser.cookiesFromUrl(poll.url, System.currentTimeMillis()))
        if (jar.all().none { it.name == "SESSDATA" }) {
            qrSessions.end(login.token)
            return QrLoginState.Failed("登录成功了但响应里没有 SESSDATA，现有登录态没有改动")
        }
        val state = ctx.commitQrLogin(this.login, BiliQrParser.OLD_LOGIN_NAMES, jar.all()) {
            BilibiliSource(ctx.withCookieJar(platform, jar), base).whoami()
        }
        qrSessions.end(login.token)
        return state
    }

    /** 扫码登录的 GET：用临时罐 [jar]，检查 HTTP 状态和 B站的 `{code}` 外壳。出错信息里不带响应内容（成功的响应里有凭据）。 */
    private suspend fun qrGet(jar: MemoryCookieJar, path: String, params: Map<String, String>): JsonObject {
        val url = (passportBase + path).toHttpUrl().newBuilder().apply { params.forEach { (k, v) -> addQueryParameter(k, v) } }.build()
        val res = http.newBuilder().cookieJar(jar).build().fetch(Request.Builder().url(url).build())
        if (res.code == 412) throw RiskControlException(platform, "HTTP 412", "请求被拦截")
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", "扫码登录接口 $path")
        return check(parseQrJson(platform, res.body))
    }

    // ---------- 内部 ----------

    /** 只要 body（不做 JSON 解析）：解析交给 [parse]，开关开着时 body 只解析一遍。 */
    private suspend fun getBody(path: String, params: Map<String, String>, wbi: Boolean = false): String {
        val plain = (base + path).toHttpUrl().newBuilder().apply {
            params.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build()
        val url = if (wbi) signUrl(plain) else plain
        val res = http.fetch(Request.Builder().url(url).build())
        if (res.code == 412) throw RiskControlException(platform, "HTTP 412", "请求被拦截")
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(200))
        return res.body
    }

    private suspend fun getJson(
        path: String,
        params: Map<String, String>,
        wbi: Boolean = false,
        checkCode: Boolean = true,
    ): JsonObject {
        val json = parseJsonObject(platform, getBody(path, params, wbi))
        return if (checkCode) check(json) else json
    }

    /** 检查 B站统一的 `{code, message, data}` 外壳，非 0 抛对应异常。 */
    private fun check(json: JsonObject): JsonObject = checkBiliEnvelope(platform, json)

    private suspend fun signUrl(url: HttpUrl): HttpUrl {
        val params = linkedMapOf<String, String>()
        for (i in 0 until url.querySize) {
            val name = url.queryParameterName(i)
            if (name == "w_rid" || name == "wts") continue
            params[name] = url.queryParameterValue(i).orEmpty()
        }
        val signed = parse.sign(params, mixinKey(), nowSec())
        return url.newBuilder().query(null).apply { signed.forEach { (k, v) -> addQueryParameter(k, v) } }.build()
    }

    /**
     * WBI 混淆密钥，按北京时间每天从 nav 取一次，缓存在 store 里。
     *
     * "今天"用 [nowSec]（而不是 `LocalDate.now()`）算：生产里 [nowSec] 就是当前时间，行为一样；
     * 但测试注入固定时间戳时，Kotlin 和 Rust 才算得出**同一个**日期、共用同一份缓存
     * （Rust 平台对象那边也是拿传进去的 `now_sec` 算的，见 `bili::source::beijing_date`）。
     */
    private suspend fun mixinKey(): String = wbiMutex.withLock {
        val today = LocalDate.ofInstant(Instant.ofEpochSecond(nowSec()), ZoneId.of("Asia/Shanghai")).toString()
        ctx.store.get(KEY_WBI)?.split('|')?.let { if (it.size == 2 && it[0] == today) return it[1] }
        // nav 未登录时 code=-101，但 wbi_img 照样给，所以不检查 code
        val nav = getJson("/x/web-interface/nav", emptyMap(), checkCode = false)
        val (img, sub) = parse.wbiKeys(nav) ?: throw ParseException(platform, "nav 里没有 wbi_img")
        val key = parse.mixinKey(img, sub)
        ctx.store.put(KEY_WBI, "$today|$key")
        key
    }

    /**
     * 没有 buvid3 时很多接口会 -352。从浏览器导入的 Cookie 里一般都有；没有就按网页的做法从 spi 接口领一个，种进 Cookie 罐。
     */
    private suspend fun ensureBuvid() = buvidMutex.withLock {
        if (ctx.cookies.find(WWW, "buvid3") != null) return@withLock
        val json = getJson("/x/frontend/finger/spi", emptyMap())
        val data = json.path("data") ?: throw ParseException(platform, "spi 缺少 data")
        val expires = System.currentTimeMillis() + 365L * 24 * 3600 * 1000
        val cookies = listOfNotNull(
            data.string("b_3")?.let { "buvid3" to it },
            data.string("b_4")?.let { "buvid4" to it },
        ).map { (k, v) -> Cookie.Builder().name(k).value(v).domain("bilibili.com").path("/").expiresAt(expires).build() }
        ctx.cookies.put(WWW, cookies)
    }

    companion object {
        const val API = "https://api.bilibili.com"
        internal const val PASSPORT = "https://passport.bilibili.com"
        private const val QR_TTL_MS = 180_000L
        private val WWW = "https://www.bilibili.com/".toHttpUrl()
        private const val KEY_WBI = "bili.wbi_mixin_key"
        private const val MIN_INTERVAL_MS = 500L
        private const val SUB_REPLY_PAGE_SIZE = 20
        private const val TARGET_CACHE_SIZE = 256
        private const val FOLLOWINGS_PAGE_SIZE = 50

        /** 网页空间页关注按钮的来源标记。 */
        private const val RE_SRC_SPACE = "11"

        /** x/relation/modify：已经关注该用户，无法重复关注。 */
        private const val CODE_ALREADY_FOLLOWING = 22014L

        /** 拉视频 / 音频流必须带的请求头（B站 CDN 校验 Referer，缺了 403）。 */
        val PLAY_HEADERS = mapOf("Referer" to "https://www.bilibili.com/", "User-Agent" to UserAgents.DESKTOP_CHROME)

        /** 路径里没有 /wbi/ 但网页也带 WBI 签名调用的接口。 */
        private val WBI_PATHS = setOf("/x/polymer/web-dynamic/v1/feed/space")

        /** 不带 dm_img_* 会 -352 的接口，raw 调试时自动补上。 */
        private val DM_IMG_PATHS = setOf("/x/polymer/web-dynamic/v1/feed/space", "/x/space/wbi/arc/search")

        /** 动态接口的 features，照 bilby `BiliConstants.DYN_FEATURES`。 */
        private const val DYN_FEATURES = "itemOpusStyle,listOnlyfans,onlyfansQaCard"
    }
}
