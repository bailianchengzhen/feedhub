package feedhub.source.tieba

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedCookieJar
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.FollowKind
import feedhub.core.FollowSource
import feedhub.core.FollowTarget
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
import feedhub.core.UserAgents
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.core.fetch
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import feedhub.rust.RustTieba
import feedhub.rust.RustTiebaHosted
import okhttp3.Cookie
import okhttp3.CookieJar
import okhttp3.FormBody
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import java.util.UUID

/**
 * 百度贴吧的 Source：**请求、签名、解析、错误判定都在 Rust**（`rust/crates/feedhub-core/src/tieba/`），
 * 收发走 Rust 联网层（[feedhub.rust.RustNetStack]，代理 / Cookie / 限速 / fail-closed 都在那一层）。
 *
 * 这里只剩宿主那几件事：
 * - 平台错误码 → 异常（[TiebaErrors]，规则只有一份）；
 * - 关注的吧列表的本地缓存（[ForumStore]，格式和以前一样）和模型映射；
 * - 二维码图片解码（zxing 只有 Java 版，见 [QrImage]）；
 * - 调试用的 [raw]（App 表单的补参 + 签名，签名本身也在 Rust）。
 *
 * 各接口：
 * - 推荐流：App 协议 `/c/f/excellent/personalized`（protobuf），游标是页码；
 * - 关注流：关注的吧里的新帖子，调度 / 归并 / 存盘都在 Rust（`tieba/following.rs`）；
 * - 详情 / 楼层 / 楼中楼：`/c/f/pb/page`、`/c/f/pb/floor`；播放：`main` 复用详情取过的第一页（5 分钟），`url:<mp4>` 不发请求；
 * - 搜索：`request.community` 为空走全吧搜索（网页 GET），给了吧名走吧内搜索（App 表单 POST）；
 * - 关注 / 取关：唯一的写操作，见 [setFollowing]；
 * - whoami：网页接口 `newmoindex`；扫码登录：百度通行证的网页扫码（`tpl=tb`）。
 *
 * Kotlin 原来那份实现（`TiebaSource` 和它的 protobuf 定义）只在测试里当对拍参照。
 */
class RustTiebaSource internal constructor(
    private val ctx: SourceContext,
    /** App 接口主机。测试里指向 MockWebServer。 */
    private val appBase: String = APP,
    /** 网页接口主机。 */
    webBase: String = WEB,
    /** 时钟（毫秒）：请求里的时间戳和播放地址的复用窗口都用它；测试里换成假的，请求字节才可比。 */
    private val nowMs: () -> Long = System::currentTimeMillis,
    /** [raw] 的 App 表单签名。 */
    private val signer: TiebaSigner = RustTiebaSigner(TIEBA),
) : VideoSource, FollowSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, APP, WEB)

    // 身份、流、登录、域名都以插件描述为准（[TiebaPlugin]）
    override val platform = TIEBA
    override val feeds = TiebaPlugin.capabilities.feeds
    override val login = TiebaPlugin.login.web
    override val apiBase = appBase
    override val hosts = TiebaPlugin.hosts
    override val search = TiebaPlugin.capabilities.search!!

    /** 扫码长轮询的读超时（秒）。服务端约 30 秒才回，这里 10 秒就先断开、下一次再来；测试里改小。 */
    internal var unicastReadTimeoutS: Long = UNICAST_READ_TIMEOUT_S

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(TIEBA to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(TIEBA), TiebaPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(TIEBA), TiebaPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted: RustTiebaHosted by lazy {
        RustTiebaHosted(
            ctx,
            appBase,
            webBase,
            net,
            TIEBA,
            passportBase = if (appBase == APP) PASSPORT else appBase,
            // 二维码是一张 PNG：zxing 只有 Java 版，所以解码留在 Kotlin（`QrImage`）
            qrDecoder = { png -> runCatching { QrImage.decode(png) }.getOrNull() },
            unicastTimeoutMs = { unicastReadTimeoutS * 1000 },
        )
    }

    /** Rust 平台对象带回来的错误码在这里按 [TiebaErrors] 归类。 */
    private val hostedFetch by lazy { RustTiebaHostedFetch(ctx, hosted, nowMs, TIEBA) }

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 `.so`）。 */
    fun rustVersion(): String = RustTieba.version(platform)

    /** 「我是谁」：网页接口 `newmoindex`（没登录一个请求都不发）。 */
    override suspend fun whoami(): UserRef? = hosted.whoami()

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> hostedFetch.recommend(cursor)
        FeedKind.FOLLOWING -> {
            requireLogin()
            hosted.followingPage(cursor, nowMs())
        }
    }

    /** 关注流手上那份缓冲（排障 / 测试用：每个吧一条摘要，不发请求、不读盘）。 */
    internal suspend fun followingForums(): List<RustTiebaHosted.TiebaForumState> = hosted.followingForums()

    /** 搜一页（全吧 / 吧内由 `request.community` 决定）。 */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> =
        hostedFetch.search(request, cursor)

    /** 关注的吧（`forumGuide`）。顺手更新关注的吧列表缓存（[ForumStore]）。 */
    override suspend fun followings(cursor: Cursor?): Page<FollowTarget> {
        if (cursor != null) return Page(emptyList(), null)
        requireLogin()
        val forums = hostedFetch.followedForums()
        ForumStore.saveForums(ctx.store, ForumStore.ForumList(System.currentTimeMillis(), forums))
        return Page(forums.map { it.toFollowTarget() }, null)
    }

    /**
     * 关注 / 取关一个吧。**唯一的写操作**：先读一次关注的吧（顺手更新缓存）→ 已经是目标状态就不写
     * → 写请求在 Rust（tbs / 签名 / 表单 / 错误归类 / 不重试）。
     */
    override suspend fun setFollowing(target: FollowTarget, follow: Boolean) {
        if (target.platform != platform || target.kind != FollowKind.FORUM) {
            throw ApiException(platform, "-", "贴吧只能关注 / 取关吧")
        }
        val fid = target.id.toLongOrNull()?.takeIf { it > 0 }
            ?: throw ParseException(platform, "吧 id 不对：${target.id}")
        requireLogin()
        val forums = hostedFetch.followedForums()
        ForumStore.saveForums(ctx.store, ForumStore.ForumList(System.currentTimeMillis(), forums))
        val current = forums.firstOrNull { it.id == fid }
        if ((current != null) == follow) return
        hosted.setFollowing(target, follow, current?.name, nowMs())
        ForumStore.invalidateForums(ctx.store)
    }

    override suspend fun detail(id: String): FeedDetail = hostedFetch.detail(id)

    /** 播放地址：取地址 + `PlayInfo` 都在 Rust（`main` 5 分钟复用一楼的 `pb/page`，`url:<mp4>` 0 个请求）。 */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hostedFetch.play(id, partId) ?: throw ParseException(platform, "帖子 $id 一楼里没有视频")

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hostedFetch.comments(id, cursor, sort)

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hostedFetch.replies(id, commentId, cursor)

    /**
     * 扫码登录：请求 / 状态 / Cookie 落地都在 Rust（临时罐在宿主，成功之前不碰 `ctx.cookies`）。
     * `gid` 每次生成新的（`UUID.randomUUID()` 大写）。
     */
    override suspend fun startQrLogin(): QrLogin =
        hosted.startQrLogin(nowMs(), UUID.randomUUID().toString().uppercase())

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = hosted.pollQrLogin(
        login,
        nowMs(),
        login.extra["gid"] ?: UUID.randomUUID().toString().uppercase(),
    )

    // ---------- raw（调试用） ----------

    /** App 和网页两个调试客户端共用一个限速器（只有 [raw] 用）。 */
    private val limiter by lazy { MinIntervalInterceptor(MIN_INTERVAL_MS) }

    /** App 接口：不带 Cookie 罐（App 请求只带自己的 Cookie 头）。 */
    private val app: OkHttpClient by lazy {
        ctx.client(platform).cookieJar(CookieJar.NO_COOKIES).addInterceptor(limiter).build()
    }

    /** 网页接口：读 Cookie 罐，不写回。 */
    private val web: OkHttpClient by lazy {
        ctx.client(platform)
            .cookieJar(ReadOnlyCookieJar(ctx.cookies))
            .addInterceptor(
                DefaultHeadersInterceptor(
                    mapOf("User-Agent" to UserAgents.DESKTOP_CHROME, "Referer" to "https://tieba.baidu.com/"),
                    hostFilter = { it == "baidu.com" || it.endsWith(".baidu.com") },
                ),
            )
            .addInterceptor(limiter)
            .build()
    }

    /**
     * 调试用。规则：
     * - 相对路径（如 `/c/s/initNickname`）发往 App 主机 [apiBase]；完整 URL 原样发（网页接口带 Cookie 罐里的 Cookie）。
     * - App 主机（`tiebac.baidu.com` / `c.tieba.baidu.com`）的**表单**请求、且没加 `--no-sign` 时，按 App 规则补
     *   `_client_type=2`、`_client_version`、`BDUSS`（没给的才补，BDUSS 从 Cookie 罐取），排序后加签名。GET 请求不签名。
     * - protobuf 接口（`/c/f/...?cmd=...`）**不能**用 raw 调：请求体是 multipart 里的 protobuf 字节，[RawRequest] 表达不了。
     * - 响应里出现的 BDUSS / STOKEN 值会被替换成占位符再返回，避免打印到终端。
     */
    override suspend fun raw(request: RawRequest): RawResponse {
        var req = request.toOkHttpRequest(apiBase)
        val isApp = req.url.host in APP_HOSTS
        if (isApp && request.sign && request.form.isNotEmpty()) {
            val params = request.form.toMutableList()
            fun addIfAbsent(k: String, v: String?) {
                if (v != null && params.none { it.first == k }) params += k to v
            }
            addIfAbsent("_client_type", "2")
            addIfAbsent("_client_version", CLIENT_VERSION)
            addIfAbsent("BDUSS", cookie("BDUSS"))
            val form = FormBody.Builder().apply { signer.signed(params).forEach { (k, v) -> add(k, v) } }.build()
            req = req.newBuilder().method("POST", form).build()
        }
        if (isApp && req.header("User-Agent") == null) {
            req = req.newBuilder().header("User-Agent", "bdtb for Android $CLIENT_VERSION").build()
        }
        val res = (if (isApp) app else web).fetch(req).toRawResponse()
        return res.copy(body = scrub(res.body))
    }

    private fun cookie(name: String): String? = ctx.cookies.find(COOKIE_URL, name)?.takeIf { it.isNotBlank() }

    /** 没有 BDUSS 就不发请求。 */
    private fun requireLogin() {
        if (cookie("BDUSS") == null) throw NotLoggedInException(platform)
    }

    /** 把 BDUSS / STOKEN 的值从响应文本里抹掉。 */
    private fun scrub(text: String): String {
        var out = text
        for (s in listOfNotNull(cookie("BDUSS"), cookie("STOKEN")).filter { it.length >= 8 }) out = out.replace(s, "<redacted>")
        return SECRET_FIELD.replace(out) { "\"${it.groupValues[1]}\":\"<redacted>\"" }
    }

    companion object {
        const val APP = "https://tiebac.baidu.com"
        const val WEB = "https://tieba.baidu.com"
        const val PASSPORT = "https://passport.baidu.com"

        /** App 请求报的客户端版本（推荐流那一档）。 */
        internal const val CLIENT_VERSION = "12.52.1.0"

        private val APP_HOSTS = setOf("tiebac.baidu.com", "c.tieba.baidu.com")
        private val COOKIE_URL = "https://tieba.baidu.com/".toHttpUrl()
        private const val MIN_INTERVAL_MS = 600L
        private const val UNICAST_READ_TIMEOUT_S = 10L
        private val SECRET_FIELD = Regex("\"(BDUSS|bduss|STOKEN|stoken|passwd)\"\\s*:\\s*\"[^\"]*\"")
    }
}

/** 只读 Cookie 罐：请求带上 [jar] 里的 Cookie，响应的 Set-Cookie 丢弃（免得改动用户导入的 Cookie）。 */
internal class ReadOnlyCookieJar(private val jar: FeedCookieJar) : CookieJar {
    override fun loadForRequest(url: HttpUrl): List<Cookie> = jar.loadForRequest(url)
    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) = Unit
}
