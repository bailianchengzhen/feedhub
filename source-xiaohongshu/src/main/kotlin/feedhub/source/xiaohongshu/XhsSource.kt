package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Emotes
import feedhub.core.FeedCookieJar
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.MemoryCookieJar
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
import feedhub.core.RequestPriority
import feedhub.core.RiskControlException
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.commitQrLogin
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import feedhub.core.withCookieJar
import kotlin.random.Random
import kotlinx.serialization.json.JsonObject
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/** 详情（`POST`）、一级评论、楼中楼的路径。**`internal`**：`KotlinXhsDetailApi` 那个参照实现也要用。 */
internal const val FEED = "/api/sns/web/v1/feed"
internal const val COMMENT_PAGE = "/api/sns/web/v2/comment/page"
internal const val SUB_COMMENT_PAGE = "/api/sns/web/v2/comment/sub/page"
internal const val REDMOJI_DETAIL = "/api/im/redmoji/detail"

/** 评论 / 楼中楼的 `image_formats`，逗号**不编码**（见 `XhsApi.encodeComponent` 的保留集）。 */
internal const val IMAGE_FORMATS = "jpg,webp,avif"

/** [XhsSource.play] 复用 [XhsSource.detail] 刚取过的笔记的时限（播放地址够新；重试 / 很久以后再播就重新取）。 */
internal const val NOTE_REUSE_MS = 5 * 60_000L

/** 记住最近几条笔记（够覆盖"连着点开几条"）。 */
private const val RECENT_NOTES = 8

internal fun homefeedBody(cursorScore: String, refreshType: Int, noteIndex: Int): String =
    "{\"cursor_score\":${XhsCodec.jsonString(cursorScore)},\"num\":39,\"refresh_type\":$refreshType,\"note_index\":$noteIndex," +
        "\"unread_begin_note_id\":\"\",\"unread_end_note_id\":\"\",\"unread_note_count\":0," +
        "\"category\":\"homefeed_recommend\",\"search_key\":\"\",\"need_num\":14," +
        "\"image_formats\":[\"jpg\",\"webp\",\"avif\"],\"need_filter_image\":false}"

/** `POST feed` 的请求体：字段和顺序照网页实抓。 */
internal fun feedBody(noteId: String, xsecSource: String, xsecToken: String): String =
    "{\"source_note_id\":${XhsCodec.jsonString(noteId)},\"image_formats\":[\"jpg\",\"webp\",\"avif\"]," +
        "\"extra\":{\"need_body_topic\":\"1\"},\"xsec_source\":${XhsCodec.jsonString(xsecSource)},\"xsec_token\":${XhsCodec.jsonString(xsecToken)}}"

/**
 * 小红书：电脑版网页（www.xiaohongshu.com）用的 `edith.xiaohongshu.com` 接口 + 导入的登录 Cookie（`a1`、`web_session` 等）。
 *
 * - 每个请求都要签名（X-s / X-t / X-S-Common，见 [XhsSigner]），签名在 [XhsApi] 的拦截器里做；
 * - 登录先做"从 Chrome 导入 Cookie"（`tools/import_chrome_cookies.py --only xhs`，App 里的"从 Chrome 导入"）；
 * - 实测普通 OkHttp 就能过，不看 TLS 指纹；
 * - 风控最严：461 / 471 验证码、300011 账号限制、300012 IP 限制一律立刻抛 [RiskControlException]，不重试；
 *   请求间隔至少 1.5 秒再加随机抖动；评论、详情只在用户打开时才拉。
 *
 * 随版本变的值（签名版本、webBuild、环境常量、指纹模板）在 `sign_profile.json`；`ctx.store` 的 `xhs.profile` 键里放一份新的可以覆盖它。
 *
 * **详情入口 id**：`<笔记 id>@<xsec_source>@<xsec_token>`（见 [XhsParser.nativeId]）。详情和评论都要这篇笔记的 `xsec_token`，
 * 信息流卡片里自带；token 会过期，过期的入口（例如很久以前收藏的）会报"入口已失效，请重新从信息流打开"。
 */
/** 从信息流点进笔记时网页用的 `xsec_source`。 */
internal const val SOURCE_FEED = "pc_feed"

class XhsSource internal constructor(
    private val ctx: SourceContext,
    api: String,
    /** 显式指定的签名版本数据；`null` 时先看 `ctx.store` 的覆盖版、再退回随包的那份。 */
    profileOverride: XhsProfile?,
    private val random: Random,
    private val minIntervalMs: Long,
    private val jitterMs: Long,
    /**
     * 解析 / 签名走哪一边的工厂。默认是 Kotlin 参照实现（[KotlinXhsParse]）；
     * [RustXhsSource] 传 `{ RustXhsParse(it, random) }`。
     * 做成工厂是因为 `sign_profile.json` / `ctx.store` 里的覆盖版要等 [profile] 解析出来才知道。
     */
    private val parseFactory: (XhsProfile) -> XhsParse = { KotlinXhsParse(it, random) },
    private val nowMs: () -> Long = System::currentTimeMillis,
) : VideoSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, XhsApi.API, null, Random.Default, XhsApi.MIN_INTERVAL_MS, XhsApi.JITTER_MS)

    // 身份、流、登录、域名都以插件描述为准（[XhsPlugin]），这里不再写第二份
    override val platform = XHS
    override val feeds = XhsPlugin.capabilities.feeds
    override val login = XhsPlugin.login.web
    override val apiBase = api
    override val hosts = XhsPlugin.hosts

    /** 搜索能力（描述里那一份，`PluginContract` 会比对）。只有「相关」一档、只有内容类。 */
    override val search = XhsPlugin.capabilities.search!!

    /** 随版本变的值（随包的，或 `ctx.store` 里覆盖的那一份）。扫码的临时罐也用它。 */
    private val profile: XhsProfile = profileOverride
        ?: ctx.store.get(XhsProfile.KEY)?.let { runCatching { XhsProfile.parse(it) }.getOrNull() }
        ?: XhsProfile.bundled()

    private val parse: XhsParse = parseFactory(profile)

    internal val api = XhsApi(
        cookies = ctx.cookies,
        store = ctx.store,
        // 小红书自己的代理配置，套在 XhsApi 内部那个客户端上（它负责限速和签名拦截器的顺序）
        baseClient = ctx.client(platform).build(),
        apiBase = api,
        profile = profile,
        parse = parse,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        nowMs = nowMs,
    )

    init {
        // Rust 平台对象必须和 Kotlin 共用同一个客户端（限速 / 默认头 / Cookie / 代理都在它上面）。
        (parse as? XhsHostedParse)?.bind(
            this.api,
            nowMs,
            { warmEmotes() },
            { noteId, json -> synchronized(recentNotes) { recentNotes[noteId] = nowMs() to json } },
            // 关注流的"我自己是谁"：Rust 在真要拉关注列表时才问（可能因此发一次 `user/me`，
            // 位置和参照实现的 `selfId()` 一样）。
            { selfId() },
        )
    }

    /** 取一页的接缝：Rust 平台对象（`HostedXhsParse`）或 Kotlin 参照实现（`KotlinXhsFeedApi`）。 */
    private val feedApi: XhsFeedApi = (parse as? XhsHostedParse)?.feedApi ?: KotlinXhsFeedApi(this.api, parse)

    /** 搜索"取一页"的接缝：Rust 平台对象或 Kotlin 参照实现（`KotlinXhsSearchApi`）。 */
    private val searchApi: XhsSearchApi =
        (parse as? XhsHostedParse)?.searchApi ?: KotlinXhsSearchApi(this.api, random)

    /**
     * 最近取过的笔记（笔记 id → 取的时刻 + 原始响应）。点开一条视频是 [detail] 先取笔记、[play] 紧接着要同一条笔记的播放地址——
     * 两个用的是**同一个** `POST /api/sns/web/v1/feed`。以前 play 再取一次：多一个详情请求（小红书风控最看详情次数），
     * 还要在限速里排 1.5～2.3 秒（[XhsApi.MIN_INTERVAL_MS] + 抖动），点开到开播多等这么久。
     * 现在 [play] 先用这里 [NOTE_REUSE_MS] 内取过的那份；[detail] 照旧每次现取（正文 / 计数要新的）。
     */
    private val recentNotes = object : LinkedHashMap<String, Pair<Long, JsonObject>>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Pair<Long, JsonObject>>) = size > RECENT_NOTES
    }

    /**
     * 详情 / 评论 / 楼中楼"取一次"的接缝：Rust 平台对象或 Kotlin 参照实现。
     *
     * 表情表在**两个实现里都不取**（不挡首屏）：这里传的是"手上已有的那份"（[cachedEmoteTable]），
     * 没有就空表、后台补取，
     */
    private val detailApi: XhsDetailApi =
        (parse as? XhsHostedParse)?.detailApi
            ?: KotlinXhsDetailApi(
                this.api,
                parse,
                { emotesForBodyTable() },
                { warmEmotes() },
                { noteId, json -> synchronized(recentNotes) { recentNotes[noteId] = nowMs() to json } },
            )

    /**
     * 播放地址"取一次"的接缝：**缓存也在里面**。Rust 实现那边 `recentNotes` 住在平台对象里，
     * 点开视频再播放一个请求都不发；参照实现还是下面那个 [recentNotes]（详情取过的也往里写）。
     */
    private val playApi: XhsPlayApi =
        (parse as? XhsHostedParse)?.playApi
            ?: KotlinXhsPlayApi(
                this.api,
                platform,
                nowMs,
                { noteId -> recentNote(noteId) },
                { noteId, json -> synchronized(recentNotes) { recentNotes[noteId] = nowMs() to json } },
            )

    /**
     * `GET /api/sns/web/v2/user/me`：当前登录账号。没有 Cookie、登录失效、游客态（`guest`）都返回 null。
     * 顺手把 user_id 存下来，算 homefeed / feed 的 `xy-direction` 用。
     */
    override suspend fun whoami(): UserRef? = accountApi.whoami()

    /** 扫码第一步：接缝分派（Rust 平台对象 / Kotlin 参照实现）。 */
    override suspend fun startQrLogin(): QrLogin = accountApi.start()

    /** 扫码第二步：接缝分派（Rust 平台对象 / Kotlin 参照实现）。 */
    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = accountApi.poll(login)

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> recommend(cursor)
        // 关注流的**调度也在 Rust 平台对象里**；没有 Rust 平台对象时走参照实现。
        FeedKind.FOLLOWING -> followApi?.page(cursor)?.first ?: follow.page(cursor)
    }

    /**
     * 关注流"取一页"的接缝：**生产走这个**（`XhsHostedParse` 那边建好）。
     * `null` = 纯 Kotlin。
     */
    private val followApi: XhsFollowRustApi? = (parse as? XhsHostedParse)?.followApi

    /** 关注流（拼出来的）：见 [XhsFollowFeed]。 */
    // 关注流的 TTL / 调度用同一个时钟（生产上都是 System.currentTimeMillis；测试里能一起固定）
    private val follow = XhsFollowFeed(
        feedApi, ctx.store,
        hasLogin = this.api::hasLogin,
        selfId = ::selfId,
        emotesFor = ::emotesFor,
        now = nowMs,
    )

    /** 自己的 user_id（关注列表接口的参数）：`whoami` 存过就直接用，否则现问一次 `user/me`。 */
    private suspend fun selfId(): String =
        api.userId?.takeIf { it.isNotBlank() } ?: whoami()?.id ?: throw NotLoggedInException(platform)

    // ---------- 推荐流 ----------

    /**
     * 推荐流一页。请求体字段和顺序照网页实抓（现在由 [XhsFeedApi] 那一半负责，Rust 实现里逐字节照抄）：
     * 第一页 `cursor_score:""、refresh_type:1、note_index:0`，之后 `cursor_score` 用上一页响应的 `data.cursor_score`、
     * `refresh_type:3`、`note_index` 累加已经拿到的条数。
     * 一页 `num:39`（网页的值，一次返回 39 条左右）。游标 = `<cursor_score>|<note_index>`；响应没有 `cursor_score`
     * 或没有条目就是最后一页。推荐流卡片没有发布时间：`publishedAt` 用笔记 id 前 8 位（创建时间，见 [XhsParser.idTime]）。
     *
     * 表情表还在 Kotlin（缓存 / TTL / 重试，见 [emoteTable]）：标题里真有记号才去取，取到之后用 [Emotes.used]
     * 把表情贴回已经解析好的条目——和"取到表再解析一次"逐字段相同，但少一次 60 KB 的解析。
     */
    private suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        val page = feedApi.homefeed(cursor, nowMs())
        val emotes = emotesFor(page.titles)
        val items = if (emotes.isEmpty()) {
            page.items
        } else {
            page.items.map { it.copy(emotes = Emotes.used(emotes, it.title)) }
        }
        return Page(items, page.next?.let(::Cursor))
    }

    // ---------- 搜索 ----------

    /**
     * 搜一页：`POST /api/sns/web/v1/search/notes`（v1 / edith，带 `x-rap-param`，不带 `xy-direction`）。
     *
     * 只声明了「综合」一档和内容类结果（`note_type=0`，视频 / 图文都收）：传别的档直接报错，不静默退化
     * （和知乎那条"能力里没有就报错"的口径一样）。游标是 `<search_id>|<下一页页码>`，同一轮搜索复用同一个 id。
     *
     * 表情表还在 Kotlin（和推荐流一样：标题里真有记号才去取，取到之后贴回已经解析好的条目）。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        if (request.kind != SearchKind.ALL) {
            throw ApiException(platform, "kind", "小红书的搜索只声明了内容类结果（ALL），不支持 ${request.kind}")
        }
        if (request.sort != SearchSort.RELEVANCE) {
            throw ApiException(platform, "sort", "小红书的搜索只声明了「相关」一档，不支持 ${request.sort}")
        }
        val page = searchApi.search(request.text, cursor, nowMs())
        val emotes = emotesFor(page.titles)
        val items = if (emotes.isEmpty()) {
            page.items
        } else {
            page.items.map { it.copy(emotes = Emotes.used(emotes, it.title)) }
        }
        return Page(items, page.next)
    }

    // ---------- 详情、评论、楼中楼 ----------

    /**
     * `POST /api/sns/web/v1/feed`（带 `x-rap-param`、`xy-direction`）。图文笔记的多张图都放进正文（`Block.Image`，`original` 是原图），
     * 视频笔记的视频放 [FeedDetail.video]（`partId = main`）。发布时间用详情里真正的 `time`。
     *
     * 这是**用户正在等**的请求（[RequestPriority.INTERACTIVE]）：限速器把它插到刷新 / 预取 / 表情表前面。
     * 表情表**不挡首屏**（[emotesForBodyTable]）：表不在手上就先显示原文 `[笑哭R]`，同时后台去取，下一次点开就有了。
     */
    override suspend fun detail(id: String): FeedDetail = detailApi.detail(id, nowMs())

    private fun recentNote(noteId: String): JsonObject? = synchronized(recentNotes) {
        recentNotes[noteId]?.takeIf { nowMs() - it.first <= NOTE_REUSE_MS }?.second
    }

    /**
     * 一级评论 `GET /api/sns/web/v2/comment/page?note_id=&cursor=&top_comment_id=&image_formats=&xsec_token=`（不带 rap）。
     * 游标就是响应的 `data.cursor`，`has_more` 为 false 结束。一次返回 10 条，每条带 1 条楼中楼预览。没有排序参数，按平台默认。
     * 评论是最容易触发风控的接口，只在用户打开时才拉，翻页由用户滚动触发。
     *
     * 用户正等着看（[RequestPriority.INTERACTIVE]）；表情表和详情一样不挡首屏（[emotesForBodyTable]）。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        detailApi.comments(id, cursor, sort, nowMs())

    /**
     * 楼中楼 `GET /api/sns/web/v2/comment/sub/page?note_id=&root_comment_id=&num=10&cursor=&image_formats=&top_comment_id=&xsec_token=`。
     * 第一页 `cursor` 留空就是从第一条回复开始，之后用响应的 `data.cursor`。
     */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        detailApi.replies(id, commentId, cursor, nowMs())

    private fun requireToken(ref: XhsParser.NoteRef): String = ref.xsecToken
        ?: throw ApiException(platform, "xsec_token", "评论要带这篇笔记的 xsec_token，入口里没有（很久以前收藏的？），请从信息流重新打开")

    // ---------- 视频 ----------

    /**
     * 播放地址：**5 分钟内取过这条笔记的详情就直接用**（[recentNotes]，见 [NOTE_REUSE_MS]）——
     * 点开视频再播放只有一个 `POST feed`（小红书风控最看详情次数）；缓存没命中才现取一次，
     * 那次是用户正等着开播（[RequestPriority.INTERACTIVE]）。沉浸预取调它时在 `backgroundRequestScope` 里，
     * 会退成后台（宿主那一层覆盖优先级）。
     *
     * 所有分组（EF4~EF7）、所有编码都列出来（见 [XhsVideo]）；小红书没有弹幕。
     * 解析在 `xhs/play.rs`。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        playApi.play(id, partId, quality, nowMs())

    // ---------- 表情 ----------

    /**
     * 表情表"从哪儿来"的接缝：Rust 实现那边**抓取 / 解析 / 缓存都在平台对象里**，
     * 详情 / 评论直接用它那份（不再每次把 166 条过 FFI 边界）；参照实现就是原来那一套（见 [KotlinXhsEmoteApi]）。
     */
    private val emoteApi: XhsEmoteApi =
        (parse as? XhsHostedParse)?.emoteApi ?: KotlinXhsEmoteApi(this.api, ctx.store, platform)

    /**
     * 文字里有 `[笑哭R]` 这样的记号才去取表情表；取不到就降级成文字。
     * 风控异常照常往外抛（不吞），其他错误这次用旧表，10 分钟内不再试。
     *
     * **信息流 / 关注流 / 搜索用它**（刷新本来就在后台跑，多等一个限速槽没关系）。
     */
    internal suspend fun emotesFor(text: String): Map<String, String> = emoteApi.used(text)

    /**
     * 详情 / 评论 / 楼中楼用的那份表：**只用手上已经有的**，一个请求都不发。
     *
     * 正文前面白白多排一个 1.5～2.3 秒的限速槽。现在正文先出来（表情记号按原文 `[笑哭R]` 显示），表在后台补取，
     * 下一次点开 / 刷新就能用上；真的取不到也还是原文，和以前的降级一样。
     *
     * 所以这里只回答"手上有什么"。
     */
    internal fun emotesForBodyTable(): Map<String, String> = emoteApi.cached()

    /**
     * 后台把表情表补上（同一时间只起一个）：**不挡任何人的正文**，失败就算了（10 分钟内不再试）；
     * 风控异常同样记下重试时间、不重试。Rust 实现那边这套节流在平台对象里。
     */
    private fun warmEmotes() {
        emoteApi.warm()
    }

    /** 测试用：等后台那次表情表补取跑完（生产代码不调）。 */
    internal suspend fun awaitEmoteWarm() {
        emoteApi.awaitWarm()
    }

    /**
     * "我是谁" + 扫码登录的接缝：Rust 平台对象（`HostedXhsAccountApi`）或 Kotlin 参照实现
     * （[KotlinXhsAccountApi]，就是把下面那几段原样挪过去）。
     *
     * 三条口径两个实现必须一样：**成功之前一格 `ctx.cookies` 都不动**、每次签名要一组随机量、
     * 没有 `a1` 时一个请求都不发。
     */
    private val accountApi: XhsAccountApi =
        (parse as? XhsHostedParse)?.accountApi ?: KotlinXhsAccountApi(this)

    // ---------- 扫码登录 ----------

    internal val qrSessions = QrSessions()

    /**
     * 生成二维码之前临时罐里的游客 `web_session`（按扫码 token 存）。
     * 小红书游客态本来就有 `a1` / `web_session`，
     * 所以成功后要拿它比对，确认**真的换成了登录态**，不是把游客会话当登录。
     */
    internal val guestSessions = HashMap<String, String>()

    /**
     * 小红书网页扫码登录第一步：`POST /api/sns/web/v1/login/qrcode/create`，body `{"qr_type":1}`
     * （照 cv-cat `apis/xhs_pc_login_apis.py` 的 `generate_qrcode`）→ `data {qr_id, code, url}`。
     * `url` 是小红书 App 扫的内容，[QrLogin.content] 原样给它；`qr_id` / `code` 轮询用（`code` 放 [QrLogin.extra]）。
     *
     * **签名 / 头 / 设备种子都用现有的那一套**：临时罐里装本机已经有的小红书 Cookie（`a1`、`loadts`、`webBuild`……），
     * 请求走同一个 [XhsApi]（[XhsSigner] 签名、1.5 秒限速、461 / 471 / 3000xx 映射成 [RiskControlException]）。
     * 不新造设备身份：**没有 `a1` 就不发请求**，让用户先「网页登录」打开一次官网（服务端会种 `a1`）或从电脑同步 / 导入。
     *
     * 成功之前不碰 `ctx.cookies`（[QrSessions] 的约定）。
     */
    internal suspend fun kotlinStartQrLogin(): QrLogin {
        val jar = MemoryCookieJar()
        seedQrJar(jar)
        val generated = XhsQr.generated(apiFor(jar).postQr(XhsQr.CREATE, QR_CREATE_BODY))
        val guest = jar.all().firstOrNull { it.name == "web_session" }?.value.orEmpty()
        synchronized(guestSessions) { guestSessions[generated.qrId] = guest }
        qrSessions.begin(generated.qrId, jar)
        return QrLogin(
            platform = platform,
            content = generated.url,
            token = generated.qrId,
            extra = mapOf(XhsQr.CODE to generated.code),
            // 二维码多久失效平台会告诉我们（`codeStatus = 3`），不猜时间；限速靠 XhsApi 的 1.5 秒 + 抖动
            pollIntervalMs = QR_POLL_INTERVAL_MS,
            hint = "用小红书 App 扫码，并在手机上确认登录",
        )
    }

    /**
     * 第二步：`POST /api/qrcode/userinfo`，body `{"qrId":…,"code":…}` → `data.codeStatus`：
     * 0 未扫 → [QrLoginState.Waiting]，1 已扫待确认 → [QrLoginState.Scanned]，3 过期 → [QrLoginState.Expired]，
     * 2 已确认 → 再走一次 `GET /api/sns/web/v1/login/qrcode/status`（[finishQrLogin]）拿正式会话。
     *
     * 风控（HTTP 461 / 471、300011 / 300012）照常抛 [RiskControlException]，**弹框里停、不重试**；
     * 这时也把这次扫码的临时罐丢掉。
     */
    internal suspend fun kotlinPollQrLogin(login: QrLogin): QrLoginState {
        val code = login.extra[XhsQr.CODE]?.takeIf { it.isNotBlank() }
            ?: throw ParseException(platform, "这次扫码的 token 里没有 code，重新生成二维码")
        val jar = qrSessions.jar(login.token)
        val api = apiFor(jar)
        val status = try {
            XhsQr.status(api.postQr(XhsQr.USERINFO, XhsQr.checkBody(login.token, code)))
        } catch (e: RiskControlException) {
            endQr(login.token)
            throw e
        }
        return when (status) {
            XhsQr.WAITING -> QrLoginState.Waiting
            XhsQr.SCANNED -> QrLoginState.Scanned
            XhsQr.EXPIRED -> {
                endQr(login.token)
                QrLoginState.Expired
            }
            XhsQr.CONFIRMED -> finishQrLogin(login, api, jar, code)
            null -> {
                endQr(login.token)
                QrLoginState.Failed("二维码状态接口没有给 codeStatus，现有登录态没有改动")
            }
            else -> {
                endQr(login.token)
                QrLoginState.Failed("二维码状态是没见过的值（$status），现有登录态没有改动")
            }
        }
    }

    /**
     * 手机上点了确认之后：`GET /api/sns/web/v1/login/qrcode/status?qr_id=…&code=…`（**空的 `x-login-mode` 头照着网页发**）。
     * `data.code_status` 是 2 才算成功，正式会话在 `data.login_info.session`（`web_session`）。
     *
     * 三条判定（照 cv-cat，另外补了"和游客会话比对"）：
     * - 有 `login_info.session`：直接用它（哪怕罐子里有另外的 `web_session`）；
     * - 只从 Set-Cookie 拿到 `web_session`：它必须**和生成二维码时的游客会话不一样**；
     * - 都没有 / 没变：报失败，**现有登录态一个都不动**。
     *
     * 通过之后用临时罐调一次 `whoami`（[feedhub.core.commitQrLogin]），确认平台真的认这个会话，再清掉旧的
     * `web_session` / `id_token`、写入新 Cookie（`a1` 等设备 Cookie 保留）。
     */
    private suspend fun finishQrLogin(login: QrLogin, api: XhsApi, jar: MemoryCookieJar, code: String): QrLoginState {
        val result = XhsQr.login(api.getQr(XhsQr.STATUS, listOf("qr_id" to login.token, "code" to code), QR_STATUS_HEADERS))
        if (!result.confirmed) {
            endQr(login.token)
            return QrLoginState.Failed("平台说已经确认了，但登录状态接口没给成功（code_status 不是 ${XhsQr.LOGIN_OK}），现有登录态没有改动")
        }
        val guest = synchronized(guestSessions) { guestSessions[login.token] }.orEmpty()
        val fromJar = jar.all().lastOrNull { it.name == "web_session" && it.value.isNotBlank() }?.value
        val fresh = result.session ?: fromJar
        if (fresh.isNullOrBlank() || fresh == guest) {
            endQr(login.token)
            return QrLoginState.Failed("登录成功了但拿到的还是游客会话（没有换成正式 web_session），现有登录态没有改动")
        }
        if (result.session != null) putWebSession(jar, result.session)
        val state = ctx.commitQrLogin(this.login, XHS_LOGIN_COOKIES, jar.all()) {
            XhsSource(ctx.withCookieJar(platform, jar), apiBase, profile, random, minIntervalMs, jitterMs, parseFactory, nowMs).whoami()
        }
        endQr(login.token)
        return state
    }

    /** 临时罐起步：本机已有的小红书 Cookie（不含别的平台的）。没有 `a1` 时**一个请求都不发**。 */
    internal fun seedQrJar(jar: MemoryCookieJar) {
        val domain = login.cookieDomain.trimStart('.')
        val existing = ctx.cookies.all().filter { c ->
            val d = c.domain.trimStart('.')
            d == domain || d.endsWith(".$domain")
        }
        if (existing.isNotEmpty()) jar.put(login.cookieUrl.toHttpUrl(), existing)
        if (existing.none { it.name == "a1" && it.value.isNotBlank() }) {
            throw ApiException(
                platform,
                "a1",
                "小红书扫码要用现有的 a1 设备 Cookie（每个请求的签名用它）：先把本机的登录态弄进来——" +
                    "「网页登录」打开一次小红书官网会种下 a1，或者从电脑「同步到手机」/「从 Chrome 导入」",
            )
        }
    }

    /** 正式会话（`login_info.session`）写回临时罐：同域同路径的旧值会被替换。 */
    private fun putWebSession(jar: MemoryCookieJar, session: String) {
        val expires = nowMs() + YEAR_MS
        jar.put(
            login.cookieUrl.toHttpUrl(),
            listOf(Cookie.Builder().name("web_session").value(session).domain(WEB_COOKIE_DOMAIN).path("/").expiresAt(expires).build()),
        )
    }

    internal fun endQr(token: String) {
        qrSessions.end(token)
        synchronized(guestSessions) { guestSessions.remove(token) }
    }

    /**
     * 同一个签名资料 / 限速 / 随机源、换一份 Cookie 罐的 [XhsApi]：扫码全程用临时罐，
     * 成功前不读写 `ctx.cookies`。
     */
    internal fun apiFor(jar: FeedCookieJar): XhsApi = XhsApi(
        cookies = jar,
        store = ctx.store,
        baseClient = ctx.client(platform).build(),
        apiBase = apiBase,
        profile = profile,
        parse = parse,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        nowMs = nowMs,
    )

    // ---------- 调试 ----------

    /**
     * 调试用：按网页的规则发任意请求。`sign` 为 true 时加签名（POST 的请求体：`form` 里只有一项 `json` 时原样当 JSON 发，
     * 否则把 `form` 的各项拼成一个字符串值的 JSON 对象）。
     */
    override suspend fun raw(request: RawRequest): RawResponse {
        val base = request.toOkHttpRequest(apiBase)
        val b = Request.Builder().url(base.url)
        request.headers.forEach { (k, v) -> b.header(k, v) }
        if (base.method != "POST") {
            b.method(base.method, null)
        } else {
            val body = if (request.form.size == 1 && request.form[0].first == "json") request.form[0].second
            else request.form.joinToString(",", "{", "}") { (k, v) ->
                XhsCodec.jsonString(k) + ":" + XhsCodec.jsonString(v)
            }
            b.post(body.toRequestBody("application/json;charset=UTF-8".toMediaType()))
        }
        return api.rawRequest(b.build(), request.sign).toRawResponse()
    }

    private companion object {
        /** 生成二维码的请求体（网页就是这个，只有一个字段）。 */
        const val QR_CREATE_BODY = """{"qr_type":1}"""

        /** 轮询间隔：参考实现 2 秒一次；[XhsApi] 的 1.5 秒 + 抖动限速仍然生效。 */
        const val QR_POLL_INTERVAL_MS = 2000L

        /** `qrcode/status` 要带一个**空的** `x-login-mode` 头（照网页实抓）。 */
        val QR_STATUS_HEADERS = mapOf("x-login-mode" to "")

        /** 换账号时要清掉的登录 Cookie（`a1` / `loadts` / `webBuild` 这些设备 Cookie 保留）。 */
        val XHS_LOGIN_COOKIES = setOf("web_session", "id_token")

        /** 写 Cookie 时的域：和 [XhsPlugin] 的 `LoginSpec.cookieDomain` 一致（不带点）。 */
        const val WEB_COOKIE_DOMAIN = "xiaohongshu.com"

        const val YEAR_MS = 365L * 24 * 3600 * 1000
    }
}
