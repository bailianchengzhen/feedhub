package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.HttpResult
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.parseJsonObject
import kotlinx.serialization.json.JsonObject
import feedhub.rust.RustXhs
import feedhub.rust.RustXhsHosted
import uniffi.feedhub_ffi.FfiXhsSignRandomHead
import uniffi.feedhub_ffi.FfiXhsSignRandomTail
import uniffi.feedhub_ffi.HostXhsClock
import uniffi.feedhub_ffi.HostXhsSignRandom
import kotlin.random.Random
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient

/**
 * 把"从 `Random` 里取签名随机量"的那一串单独拎出来。**取用顺序和 [XhsSigner.sign] 一模一样**
 *
 * `version → [xy-direction] → 16 位 xray 尾巴 → 16 位 b3 → [rap: 掩码 → 16 会话密钥 → 16 密钥 → 盐长 + 盐
 * →（问补位长度）→ 补位 → 加密耗时]`
 *
 * 为什么补位长度要先问：它取决于 gzip 之后的长度（`(16 - size % 16) % 16`），而补位字符又紧挨着盐从同一个
 * `Random` 里取——所以先让 Rust 纯算一遍长度，再按那个数取字符，两边的随机流才对得齐。
 */
internal class XhsSignRandomDrawer(private val random: Random) : HostXhsSignRandom {
    /**
     * **关注流的随机量来源**（`HostXhsSignRandom`）：Rust 一次刷新要发 N 个请求，每个的签名随机量都不一样，
     * 而且补位长度取决于那个请求的 body——所以它在"要发请求的那一刻"回调进来，分两步抽：
     * [drawHead]（补位长度之前）→ Rust 算长度 → [drawTail]（补位 + 加密耗时）。
     *
     * **取用顺序和 [XhsSigner.sign] 逐位一致**（同一个 `Random`，所以和 Kotlin 参照实现同一条随机流）。
     */
    override fun drawHead(xy: Boolean, xySeed: String?, rap: Boolean): FfiXhsSignRandomHead {
        val version = random.nextInt()
        val xyRandom = if (xy && xySeed.isNullOrEmpty()) 10 + random.nextInt(91) else 0
        val xrayTail = hexRandom(16)
        val b3 = hexRandom(16)
        if (!rap) {
            return FfiXhsSignRandomHead(version, xyRandom, xrayTail, b3, 0, "", "", "")
        }
        val mask = XhsRap.ALPHABET36[random.nextInt(XhsRap.ALPHABET36.length)].code
        val sessionKey = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
        val key = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
        val salt = String(XhsRap.randomAscii(random, 4 + random.nextInt(3)), Charsets.US_ASCII)
        return FfiXhsSignRandomHead(version, xyRandom, xrayTail, b3, mask, sessionKey, key, salt)
    }

    override fun drawTail(padLen: UInt): FfiXhsSignRandomTail {
        val pad = String(XhsRap.randomAscii(random, padLen.toInt()), Charsets.US_ASCII)
        val encTime = 60 + random.nextInt(181)
        return FfiXhsSignRandomTail(pad, encTime)
    }

    fun draw(
        xy: Boolean,
        xySeed: String?,
        rap: Boolean,
        padLenFor: ((mask: Int, sessionKey: String) -> Int)? = null,
    ): RustXhs.SignRandom {
        val version = random.nextInt()
        // `XhsSigner` 里是 `xyDirection(req.userId ?: req.a1)`：种子是空串才取随机，否则用 murmur3 算
        val xyRandom = if (xy && xySeed.isNullOrEmpty()) 10 + random.nextInt(91) else 0
        val xrayTail = hexRandom(16)
        val b3 = hexRandom(16)
        if (!rap) {
            return RustXhs.SignRandom(version, b3, xrayTail, xyRandom, 0, "", "", "", "", 0)
        }
        val mask = XhsRap.ALPHABET36[random.nextInt(XhsRap.ALPHABET36.length)].code
        val sessionKey = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
        val key = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
        val salt = String(XhsRap.randomAscii(random, 4 + random.nextInt(3)), Charsets.US_ASCII)
        val padLen = requireNotNull(padLenFor) { "带 x-rap-param 的请求要先能问出补位长度" }(mask, sessionKey)
        val pad = String(XhsRap.randomAscii(random, padLen), Charsets.US_ASCII)
        val encTime = 60 + random.nextInt(181)
        return RustXhs.SignRandom(version, b3, xrayTail, xyRandom, mask, sessionKey, key, salt, pad, encTime)
    }

    private fun hexRandom(n: Int): String =
        buildString { repeat(n) { append("0123456789abcdef"[random.nextInt(16)]) } }
}

/**
 * [XhsParse] 的"Rust 平台对象"版本：签名走 `FfiXhsSource`（**和 Rust 自建的请求共用一份会话计数**），
 * 其余的解析方法只是为了让接口完整（搬过来的流由平台对象自己发 + 解析，不会再调它们）。
 *
 * `XhsSource` 建好 `XhsApi` 之后会调 [bind] 把客户端交进来——**必须是 `XhsApi.http` 那一个**
 * （否则限速 / 默认头 / Cookie / 代理全丢）。
 */
internal interface XhsHostedParse : XhsParse {
    /**
     * `XhsSource` 建好 `XhsApi` 之后调（平台对象和 Kotlin 共用同一个 OkHttpClient）。
     *
     * [warmEmotes] 是 `XhsSource` 的表情表接缝：**表已经收进 Rust 常驻了**，详情 / 评论
     * 直接用平台对象里那份、`has_emotes` 由 Rust 算好带回来，Kotlin 只负责在后台补一次
     */
    fun bind(
        api: XhsApi,
        now: () -> Long,
        warmEmotes: () -> Unit = {},
        rememberNote: (String, JsonObject) -> Unit = { _, _ -> },
        /** 关注流的"我自己是谁"：Rust 在真要拉关注列表时才问（可能因此发一次 `user/me`）。 */
        selfId: suspend () -> String? = { null },
    )

    /** 推荐 / 关注流"取一页"的 Rust 实现（和 [bind] 之后才可用）。 */
    val feedApi: XhsFeedApi

    /** 搜索"取一页"的 Rust 实现（同上）。 */
    val searchApi: XhsSearchApi

    /** 详情 / 评论 / 楼中楼"取一次"的 Rust 实现。 */
    val detailApi: XhsDetailApi

    /** 播放地址的 Rust 实现：**"最近取过的笔记"那份缓存也在平台对象里**。 */
    val playApi: XhsPlayApi

    /** 表情表的 Rust 实现：**抓取 / 解析 / 缓存都在平台对象里**。 */
    val emoteApi: XhsEmoteApi

    /** 关注流的 Rust 实现：**调度也在平台对象里**，Kotlin 只给游标 / 自己的 user_id。 */
    val followApi: XhsFollowRustApi

    /**
     * "我是谁" + 扫码登录的 Rust 实现：**请求 / 签名 / 轮询状态码 / Cookie 落地都在平台对象里**。
     *
     * 它和 [sign] 用**同一份**签名计数（`FfiXhsSource::qr_object` 拿的是平台对象那一份 `Arc`），
     * 所以一次会话里的 `seq` / `signCount` / `xraySeq` 是一条序列——和 Kotlin 那边同一个 `XhsSigner` 一样。
     */
    val accountApi: XhsAccountApi
}

internal class HostedXhsParse(
    private val profile: XhsProfile,
    private val random: Random,
    private val ctx: SourceContext,
    private val apiBase: String,
    private val platform: Platform = XHS,
    /**
     * 限速参数（`XhsApi` 收的那两个）。**必须跟着传下来**：测试把 [XhsSource] 的 `minIntervalMs`
     * 传成 0，Rust 这条路也得是 0，否则两条路的节奏不一样（见 `RustXhsHosted` 里的注释）。
     */
    private val minIntervalMs: Long = XhsApi.MIN_INTERVAL_MS,
    private val jitterMs: Long = XhsApi.JITTER_MS,
) : XhsHostedParse {
    private val profileJson = XhsProfile.encode(profile)
    private val drawer = XhsSignRandomDrawer(random)

    /** 签名计数的初值：顺序和 [XhsSigner] 的构造函数一模一样（两边从同一个 `Random(seed)` 出发才对得齐）。 */
    private val initialState = RustXhs.SignState(
        seq = 12 + random.nextInt(29),
        signCount = 5 + random.nextInt(60),
        dsllt = 0,
        xraySeq = random.nextInt(0x7fffff),
    )

    private lateinit var api: XhsApi

    /** 注入的时钟（`bind` 里给；关注流的 `extraAt` / `fetchedAt` / `closedUntil` 都用它）。 */
    private var now: () -> Long = System::currentTimeMillis

    /** 关注流的"我自己是谁"（`bind` 里给）。 */
    private var selfIdProvider: (suspend () -> String?)? = null

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        // 主域名从插件描述来（`System` 那一档拿它去问 JVM 的系统代理，见 `ProxyTarget.of`）
        proxies = listOf(platform to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(platform), XhsPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(platform), XhsPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted: RustXhsHosted by lazy {
        RustXhsHosted(
            ctx, apiBase, profileJson, net, platform,
            minIntervalMs = minIntervalMs, jitterMs = jitterMs,
            initialState = initialState,
            // 关注流：随机量 / 时钟走这两个回调（**别的流一个字节都不走**）
            signRandom = drawer,
            clock = object : HostXhsClock {
                override fun nowMs(): Long = now()
            },
            // 关注流的"我自己是谁"：**Rust 在真要拉关注列表时才问**（可能发一次 `user/me`）
            selfId = { selfIdProvider?.invoke() },
        )
    }

    override lateinit var feedApi: XhsFeedApi
        private set

    override lateinit var searchApi: XhsSearchApi
        private set

    override lateinit var detailApi: XhsDetailApi
        private set

    override lateinit var playApi: XhsPlayApi
        private set

    override lateinit var emoteApi: XhsEmoteApi
        private set

    override lateinit var followApi: XhsFollowRustApi
    override lateinit var accountApi: XhsAccountApi
        private set

    override fun bind(
        api: XhsApi,
        now: () -> Long,
        warmEmotes: () -> Unit,
        rememberNote: (String, JsonObject) -> Unit,
        selfId: suspend () -> String?,
    ) {
        this.api = api
        this.now = now
        this.selfIdProvider = selfId
        feedApi = HostedXhsFeedApi(
            hosted = { hosted },
            drawer = drawer,
            platform = platform,
            // `xy-direction` 的种子：`req.userId ?: req.a1`（和 `XhsSigner` 一样；不知道才取随机值）
            xySeed = { api.userId ?: api.a1() },
        )
        searchApi = HostedXhsSearchApi(hosted = { hosted }, drawer = drawer, random = random, platform = platform)
        detailApi = HostedXhsDetailApi(
            hosted = { hosted },
            drawer = drawer,
            platform = platform,
            xySeed = { api.userId ?: api.a1() },
            // 表不在手上、Rust 又说正文里有记号时（`hasEmotes`），后台补取。
            warmEmotes = warmEmotes,
            // 原始响应回填 `recentNotes`：视频笔记点开之后播放 5 分钟内复用（不再多发一个详情请求）。
            // Rust 自己的平台对象也在 `detail` 里回填它自己那份（播放走 Rust 时用那一份）。
            rememberNote = rememberNote,
        )
        playApi = HostedXhsPlayApi(
            hosted = { hosted },
            drawer = drawer,
            platform = platform,
            xySeed = { api.userId ?: api.a1() },
        )
        emoteApi = HostedXhsEmoteApi(hosted = { hosted }, drawer = drawer, platform = platform, now = now)
        followApi = HostedXhsFollowApi(hosted = { hosted }, platform = platform, selfId = selfId)
        accountApi = HostedXhsAccountApi(hosted = { hosted }, drawer = drawer, platform = platform, now = now)
    }

    @Synchronized
    override fun sign(request: XhsSigner.Request, nowMs: Long): LinkedHashMap<String, String> {
        val bridge = request.toBridge()
        val rand = drawer.draw(request.xy, request.userId ?: request.a1, request.rap) { mask, session ->
            RustXhs.rapPadLen(profileJson, bridge, nowMs, mask, session)
        }
        return hosted.sign(bridge, nowMs, rand)
    }

    override fun newB1(nowMs: Long, loadtsMs: Long): String = RustXhs.newB1(profileJson, nowMs, loadtsMs)

    // 下面四个是 [XhsParse] 的解析那一半。搬过来的流由平台对象自己"发 + 解析"，`XhsSource` 不会再调它们；
    // 真被调到就退回纯函数（语义和 `RustXhsParse` 一样），不抛异常、不悄悄降级。
    override fun homefeed(
        res: HttpResult,
        noteIndex: Int,
        xsecSource: String,
        nowSec: Long,
        emotes: Map<String, String>,
    ): Homefeed {
        val page = RustXhs.parseHomefeed(res.body, res.code, noteIndex, xsecSource, nowSec, emotes, platform)
        return Homefeed(page.page.items, page.page.next?.value, page.titles)
    }

    override fun followings(res: HttpResult): XhsFollowParser.FollowingsPage {
        val page = RustXhs.parseFollowings(res.body, res.code, platform)
        return XhsFollowParser.FollowingsPage(
            page.users.map { FollowedAuthor(it.id, it.name, it.avatar, it.xsecToken) },
            page.cursor,
            page.hasMore,
            page.total,
        )
    }

    override fun unread(res: HttpResult): Map<String, Int> = RustXhs.parseUnread(res.body, res.code, platform)

    override fun posted(
        res: HttpResult,
        author: FollowedAuthor,
        nowSec: Long,
        emotes: Map<String, String>,
    ): XhsFollowParser.PostedPage {
        val page = RustXhs.parsePosted(
            res.body,
            res.code,
            RustXhs.Author(author.id, author.name, author.avatar, author.xsecToken),
            nowSec,
            emotes,
            platform,
        )
        return XhsFollowParser.PostedPage(
            page.notes.map { XhsFollowParser.PostedNote(it.item, it.sticky) },
            page.hasMore,
            page.titles,
        )
    }
}

/**
 * 推荐 / 关注流"取一页"的 Rust 实现：按 [XhsSignRandomDrawer] 的顺序取随机量（补位长度问平台对象），
 * 然后交给平台对象拼请求 / 发 / 判错 / 解析。
 */
internal class HostedXhsFeedApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val platform: Platform,
    private val xySeed: () -> String?,
) : XhsFeedApi {
    override suspend fun homefeed(cursor: Cursor?, nowMs: Long): Homefeed {
        val rand = drawer.draw(xy = true, xySeed = xySeed(), rap = true) { mask, session ->
            hosted().homefeedPadLen(cursor, nowMs, mask, session)
        }
        val page = hosted().homefeed(cursor, nowMs, rand)
        return Homefeed(page.page.items, page.page.next?.value, page.titles)
    }

    override suspend fun followings(
        selfId: String,
        cursor: String,
        nowMs: Long,
    ): XhsFollowParser.FollowingsPage {
        val rand = drawer.draw(xy = false, xySeed = null, rap = false)
        val page = hosted().followings(selfId, cursor, nowMs, rand)
        return XhsFollowParser.FollowingsPage(
            page.users.map { FollowedAuthor(it.id, it.name, it.avatar, it.xsecToken) },
            page.cursor,
            page.hasMore,
            page.total,
        )
    }

    override suspend fun unread(selfId: String, userIds: List<String>, nowMs: Long): Map<String, Int> {
        val rand = drawer.draw(xy = false, xySeed = null, rap = false)
        return hosted().unread(selfId, userIds, nowMs, rand)
    }

    override suspend fun posted(author: FollowedAuthor, nowMs: Long): XhsFollowParser.PostedPage {
        val bridge = RustXhs.Author(author.id, author.name, author.avatar, author.xsecToken)
        val rand = drawer.draw(xy = false, xySeed = null, rap = true) { mask, session ->
            hosted().postedPadLen(bridge, nowMs, mask, session)
        }
        val page = hosted().posted(bridge, nowMs, rand)
        val notes = page.page.items.mapIndexed { index, item ->
            XhsFollowParser.PostedNote(item, page.sticky.getOrElse(index) { false })
        }
        return XhsFollowParser.PostedPage(notes, page.hasMore, page.titles)
    }
}

/**
 * 搜索"取一页"的 Rust 实现：按 [XhsSignRandomDrawer] 的顺序取随机量（`x-rap-param` 的补位长度先问平台对象），
 * 然后交给平台对象拼请求 / 发 / 判错 / 解析。
 *
 * **随机量的取用顺序**：新一轮搜索先取 `randomPart`（造 `search_id` 用），再走 [XhsSignRandomDrawer.draw]
 * （`version → 两个 traceid → rap 的一串`）——`KotlinXhsSearchApi` 在同一位置取同一个数，
 * 所以两边从同一个 `Random(seed)` 出发会产出同一串请求（`XhsSearchEquivalenceTest` 守着）。
 */
internal class HostedXhsSearchApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val random: Random,
    private val platform: Platform,
) : XhsSearchApi {
    override suspend fun search(keyword: String, cursor: Cursor?, nowMs: Long): XhsSearchPage {
        val randomPart = if (cursor == null) random.nextLong(1, XhsSearch.RANDOM_BOUND) else 0L
        val rand = drawer.draw(xy = false, xySeed = null, rap = true) { mask, session ->
            hosted().searchPadLen(keyword, cursor, nowMs, randomPart, mask, session)
        }
        val page = hosted().search(keyword, cursor, nowMs, randomPart, rand)
        return XhsSearchPage(page.page.items, page.page.next, page.titles)
    }
}

/**
 * 详情 / 评论 / 楼中楼"取一次"的 Rust 实现：按 [XhsSignRandomDrawer] 的顺序取随机量
 * （`x-rap-param` 的补位长度先问平台对象，只有详情带 rap / xy），然后交给平台对象
 * 拼请求 / 发 / 判错 / 解析。
 *
 * 三条都是**用户正等着的请求**：Rust 在请求头里挂内部伪请求头，宿主（`RustHttpHost`）
 * 发出去之前删掉、转成 OkHttp 的 `RequestPriority.INTERACTIVE` tag——和 Kotlin 参照实现
 * 给 `XhsApi` 传 `priority = INTERACTIVE` 是同一件事、同一个 tag。
 *
 * 表情表**不在这里取**：手上已有的那份（[emotes]）过边界；Rust 说正文里有记号而表又不在手上时，
 * 调 [warmEmotes] 在后台补一次（正文先出来，下一次点开就有图）。
 */
internal class HostedXhsDetailApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val platform: Platform,
    private val xySeed: () -> String?,
    private val warmEmotes: () -> Unit,
    private val rememberNote: (String, JsonObject) -> Unit = { _, _ -> },
) : XhsDetailApi {
    override suspend fun detail(id: String, nowMs: Long): FeedDetail {
        // 和 XhsSigner 一样：`xy-direction` 的种子是 `userId ?: a1`，不知道才取随机值。
        val rand = drawer.draw(xy = true, xySeed = xySeed(), rap = true) { mask, session ->
            hosted().detailPadLen(id, nowMs, mask, session)
        }
        val out = hosted().detail(id, nowMs, rand)
        // 原始响应回填给 `recentNotes`：点开视频之后播放要复用这一份（不再多发一个详情请求）。
        // 解析失败就算了（那说明 body 不是能用的 JSON，播放那边会自己现取）。
        runCatching { parseJsonObject(platform, out.body) }
            .onSuccess { rememberNote(XhsParser.parseNativeId(id).noteId, it) }
        // `has_emotes` 现在是"表不在手上、正文里有记号"：只在那种时候后台补一次（不挡首屏）。
        if (out.hasEmotes) warmEmotes()
        return out.detail
    }

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, nowMs: Long): Page<Comment> {
        // 评论不带 rap / xy：随机量的取用顺序和 `XhsSigner.sign` 里 `rap=false, xy=false` 那一支一样。
        val rand = drawer.draw(xy = false, xySeed = null, rap = false)
        val out = hosted().comments(id, cursor, sort, nowMs, rand)
        if (out.hasEmotes) warmEmotes()
        return out.page
    }

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?, nowMs: Long): Page<Comment> {
        val rand = drawer.draw(xy = false, xySeed = null, rap = false)
        val out = hosted().replies(id, commentId, cursor, nowMs, rand)
        if (out.hasEmotes) warmEmotes()
        return out.page
    }
}

/**
 * 播放地址的 Rust 实现：Rust 拼详情请求 / 签名 / 判错，"最近取过的笔记"缓存也在平台对象里
 * （[RustXhsHosted.playCached]），`XhsVideo` 的解析在 `feedhub-core/src/xhs/play.rs`。
 *
 * **随机量的取用要和参照实现逐位对齐**（两边从同一个 `Random(seed)` 出发时请求才逐字节相同）：
 * - `partId` 不对：一个随机量都不取、不开请求（和 `KotlinXhsPlayApi` 一样先判）；
 * - 缓存命中：Rust 不发请求、不签名，所以**不取随机量**——先问 [RustXhsHosted.playCached]，
 *   命中就给一份占位随机量（Rust 用不到它）；
 * - 没命中：先问补位长度（纯计算，不取随机量），再按 `XhsSigner.sign` 的顺序取一份，交给 Rust。
 */
internal class HostedXhsPlayApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val platform: Platform,
    private val xySeed: () -> String?,
) : XhsPlayApi {
    override fun cached(id: String, nowMs: Long): Boolean = hosted().playCached(id, nowMs)

    override suspend fun play(id: String, partId: String, quality: Int?, nowMs: Long): PlayInfo {
        if (partId != XhsVideo.MAIN) throw ApiException(platform, "part", "小红书的视频只有一个分段 main，没有 $partId")
        if (cached(id, nowMs)) {
            // 缓存命中：Rust 不签名，这份随机量不会被用到（也不该从 Random 里取——参照实现这时同样不取）。
            return hosted().play(id, partId, quality, nowMs, UNUSED_RANDOM)
        }
        // 详情和播放用的是同一个 `POST feed`：补位长度照详情那一份问（Rust 侧两个 builder 完全相同）。
        val rand = drawer.draw(xy = true, xySeed = xySeed(), rap = true) { mask, session ->
            hosted().detailPadLen(id, nowMs, mask, session)
        }
        return hosted().play(id, partId, quality, nowMs, rand)
    }

    private companion object {
        /** 只有"Rust 不会签名"的那条路才用得上（值本身没有意义）。 */
        val UNUSED_RANDOM = RustXhs.SignRandom(0, "", "", 0, 0, "", "", "", "", 0)
    }
}

/**
 * 表情表的 Rust 实现：**抓取 / 解析 / 缓存 / 一次的节流都在平台对象里**（`xhs/emotes.rs` +
 * `XhsHostedSource`），Kotlin 只留"什么时候调"。
 *
 * 现在详情 / 评论用的是 Rust 里那份，只有信息流 / 关注流还过一次边界——而且传的是
 * "这段文字里真出现过的那几项"，不是整张表。
 *
 * **随机量取用要和参照实现逐位对齐**（两边从同一个 `Random(seed)` 出发时请求才逐字节相同）：
 * 表不在手上时这一次会真的发 `GET /api/im/redmoji/detail`（要签名），先问
 * [RustXhsHosted.emoteFetchNeeded]，要发才从 `Random` 里抽（`xy=false, rap=false`，和 Kotlin
 * 那边 `XhsApi.get` 的签名取用顺序一样）。
 */
internal class HostedXhsEmoteApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val platform: Platform,
    private val now: () -> Long,
) : XhsEmoteApi {
    /** 后台补取的任务（一次只起一个；真正的"只许一个在飞"由 Rust 平台对象保证）。 */
    @Volatile
    private var job: Job? = null

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override suspend fun used(text: String): Map<String, String> {
        if (!XhsBody.EMOTE_TOKEN.containsMatchIn(text)) return emptyMap()
        val rand = if (hosted().emoteFetchNeeded(now())) {
            drawer.draw(xy = false, xySeed = null, rap = false)
        } else {
            UNUSED_RANDOM
        }
        return hosted().emotesFor(text, now(), rand)
    }

    override fun cached(): Map<String, String> = hosted().cachedEmotes(now())

    override fun warm() {
        if (job?.isActive == true) return
        job = scope.launch {
            val now = now()
            // 不发请求的那一趟不抽随机量（和 `used` 同一个道理）
            val rand = if (hosted().emoteFetchNeeded(now)) {
                drawer.draw(xy = false, xySeed = null, rap = false)
            } else {
                UNUSED_RANDOM
            }
            try {
                hosted().ensureEmotes(now, rand)
            } catch (_: feedhub.core.RiskControlException) {
                // 风控：和 Kotlin 参照实现的 `warmEmotes` 一样，记下 10 分钟不再试（表本身不重试）
                hosted().deferEmotes(now() + 10 * 60_000L)
            } catch (_: Exception) {
                // 其它错误核心里已经降级成"用旧表 + 10 分钟不再试"，这里只是别让异常跑到协程外面
            }
        }
    }

    override suspend fun awaitWarm() {
        job?.join()
    }

    private companion object {
        /** 只有"Rust 不会签名"的那条路才用得上（值本身没有意义）。 */
        val UNUSED_RANDOM = RustXhs.SignRandom(0, "", "", 0, 0, "", "", "", "", 0)
    }
}

/**
 * 关注流的 Rust 实现：**调度、三个接口的请求、归并、存盘、表情贴回全在平台对象里**
 * （`xhs/follow_feed.rs`），Kotlin 只回答两件事：
 *
 * - **自己的 user_id**（[selfId]）：`whoami` 没搬——`xhs.user_id` 有就直接用，没有就现问一次
 *   `user/me`（**位置和以前一样**，在关注列表请求之前，一次刷新最多一次）；
 * - 游标原样传（空游标 = 刷新，非空 = 纯缓存翻页）。
 *
 * 随机量 / 时钟走 [XhsSignRandomDrawer] 和注入的那个钟（见 [`XhsHostedParse.bind`] 的 `now`）。
 * 风控（461 / 471 / 300011 / 300012）照常抛出去，**已经拉到的那些在 Rust 的 finally 里已经落盘**。
 */
internal class HostedXhsFollowApi(
    private val hosted: () -> RustXhsHosted,
    private val platform: Platform,
    /** 自己的 user_id（Rust 在真要拉关注列表时才问）。**可能发一次 `user/me`**，位置和参照实现一样。 */
    private val selfId: suspend () -> String?,
) : XhsFollowRustApi {
    override suspend fun page(cursor: Cursor?): Pair<Page<FeedItem>, Boolean> {
        // 注意：这里**不能**先算 selfId 再调 Rust（缓存还新鲜的那一轮 Kotlin 一个请求都不发）。
        // 所以把"算 selfId"这件事交给 Rust 的 `HostXhsSelfId` 回调（见 `RustXhsHosted`）。
        val out = hosted().followPage(cursor)
        return out.page to out.refreshed
    }

    override suspend fun cached(): List<RustXhsHosted.FollowAuthor> = hosted().followCached()

    override suspend fun extraAt(): Long? = hosted().followExtraAt()
}

/**
 * "我是谁" + 扫码登录的 Rust 实现：请求 / 签名 / 轮询状态码 / Cookie 落地都在平台对象里。
 *
 * **随机量按"这次可能签几个请求"一次给足**：小红书每个请求都要一组（`version` / 两个 traceid / 补位……），
 * 而 Kotlin 那边每签一个就从自己的 `Random` 里取一组（`XhsApi` 的签名拦截器逐请求调 `XhsParse.sign`）。
 * 所以 [start] 取 **1** 组（只签 `qrcode/create`）、[poll] 取 **3** 组
 * （`userinfo` → 确认后的 `qrcode/status` → 验证 `user/me`；未扫 / 过期只用第 1 组）。
 *
 * **取用顺序和 [KotlinXhsAccountApi] 严格一致**：这三个路径都不带 `x-rap-param` / `xy-direction`
 * （`XhsApi.RAP_PATHS` / `XY_PATHS` 里没有它们），所以 [XhsSignRandomDrawer.draw] 走 `rap = false, xy = false`，
 * 不会去问补位长度——两边从同一个 `Random(seed)` 出发产出的是同一串请求。
 */
internal class HostedXhsAccountApi(
    private val hosted: () -> RustXhsHosted,
    private val drawer: XhsSignRandomDrawer,
    private val platform: Platform,
    private val now: () -> Long,
) : XhsAccountApi {
    override suspend fun whoami(): UserRef? {
        // 没登录就**不发请求**，也就不抽随机量（和 `HostedXhsFeedApi` 的"不发请求不取随机量"同一个道理）
        val random = if (hosted().hasLogin()) {
            drawer.draw(xy = false, xySeed = null, rap = false)
        } else {
            UNUSED_RANDOM
        }
        return hosted().whoami(now(), random)
    }

    override suspend fun start(): QrLogin =
        hosted().startQrLogin(now(), listOf(drawer.draw(xy = false, xySeed = null, rap = false)))

    override suspend fun poll(login: QrLogin): QrLoginState {
        // 最多三次签名：userinfo / qrcode/status / 验证 whoami。一次取足（多给的不用）。
        val rands = List(3) { drawer.draw(xy = false, xySeed = null, rap = false) }
        return hosted().pollQrLogin(login, now(), rands)
    }

    private companion object {
        /** `whoami` 在没登录时用不上随机量（值本身没有意义）。 */
        val UNUSED_RANDOM = RustXhs.SignRandom(0, "", "", 0, 0, "", "", "", "", 0)
    }
}
