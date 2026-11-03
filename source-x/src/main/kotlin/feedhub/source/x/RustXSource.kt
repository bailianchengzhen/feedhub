package feedhub.source.x

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.RustX
import feedhub.rust.RustXFeed
import feedhub.rust.RustXHosted

/** txid 生成器接口（测试注入固定值 / 生产直接用 Rust 实现）。 */
internal fun interface XtxGenerator {
    fun generate(method: String, path: String, nowMs: Long, randomByte: Int): String
}

/**
 * X Source 的纯 Rust 实现门面：
 * 所有的业务请求编排（GraphQL URL、参数顺序、features 顺序、请求头顺序、翻页、限速记账、404 重试、txid、raw）
 * 全部下沉在 Rust 核心层中完成，宿主仅提供网络收发和 Cookie 罐支持。
 */
class RustXSource internal constructor(
    private val ctx: SourceContext,
    private val base: String = API,
    private val pageOrigin: String = X_ORIGIN,
    private val txidEnabled: Boolean = true,
    /** 预置一个 txid 生成器（测试用：两边注入同一个固定时间 / 随机字节）。 */
    private val presetTxid: XtxGenerator? = null,
    minIntervalMs: Long = API_MIN_INTERVAL_MS,
    jitterMs: Long = API_JITTER_MS,
    pageIntervalMs: Long = PAGE_MIN_INTERVAL_MS,
    pageJitterMs: Long = PAGE_JITTER_MS,
    signChainJson: String? = null,
    /** txid 的时间 / 随机字节来源（测试注入固定值，两边才能逐字节比）。 */
    private val txidNowMs: () -> Long = System::currentTimeMillis,
    private val txidRandomByte: () -> Int = { kotlin.random.Random.nextInt(256) },
) : VideoSource {
    constructor(ctx: SourceContext) : this(ctx, API)

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(X to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(X), XPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(X), XPlugin.hosts),
        jar = ctx.cookies,
    )

    private var hostedRef: RustXHosted? = null

    private val hosted: RustXHosted = RustXHosted(
        ctx = ctx,
        base = base,
        pageOrigin = pageOrigin,
        net = net,
        platform = X,
        queryIds = emptyMap(),
        features = emptyMap(),
        fieldToggles = emptyMap(),
        detailFieldToggles = emptyMap(),
        txid = if (!txidEnabled && presetTxid == null) {
            null
        } else {
            { method, path ->
                if (!path.startsWith("/i/api/graphql/")) {
                    null
                } else if (presetTxid != null) {
                    presetTxid.generate(method, path, txidNowMs(), txidRandomByte())
                } else if (txidEnabled) {
                    hostedRef?.generateTxid(method, path, txidNowMs(), txidRandomByte())
                } else {
                    null
                }
            }
        },
        txidEnabled = txidEnabled && presetTxid == null,
        signChainJson = signChainJson,
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        pageIntervalMs = pageIntervalMs,
        pageJitterMs = pageJitterMs,
    ).also { hostedRef = it }

    override val platform = X
    override val feeds: Set<FeedKind> = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING)
    override val login = XPlugin.login.web
    override val apiBase = base
    override val hosts = XPlugin.hosts

    fun rustVersion(): String = RustX.version()
    fun hostedBase(): String = hosted.apiBase()
    val lastTxidError: String? get() = hosted.lastTxidError()

    internal fun isLoggedIn(): Boolean =
        ctx.cookies.find(COOKIE_URL, "ct0")?.isNotBlank() == true &&
        ctx.cookies.find(COOKIE_URL, "auth_token")?.isNotBlank() == true

    override suspend fun whoami(): UserRef? = hosted.whoami()

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        if (!isLoggedIn()) {
            throw NotLoggedInException(
                platform,
                "X 的时间线要登录：先在“我的”里导入 X 的 Cookie（auth_token + ct0）",
            )
        }
        var outcome = hosted.feed(kind, cursor, staleRetry = true)
        if (outcome is RustXFeed.StaleQueryId) {
            hosted.refreshQueryId(kind)?.let { id ->
                val opName = when (kind) {
                    FeedKind.RECOMMEND -> "HomeTimeline"
                    FeedKind.FOLLOWING -> "HomeLatestTimeline"
                }
                hosted.setQueryIds(mapOf(opName to id))
            }
            outcome = hosted.feed(kind, cursor, staleRetry = false)
        }
        return when (outcome) {
            is RustXFeed.Loaded -> outcome.page
            RustXFeed.StaleQueryId -> throw ParseException(
                platform,
                "Rust 平台层在 staleRetry=false 时回了 stale（不该发生）",
            )
        }
    }

    override suspend fun raw(request: RawRequest): RawResponse = hosted.raw(request)

    override suspend fun detail(id: String): FeedDetail = hosted.detail(id)

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hosted.comments(id, cursor, sort)

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hosted.replies(id, commentId)

    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId)

    companion object {
        const val API: String = "https://x.com"
        const val X_ORIGIN: String = "https://x.com"
        const val COOKIE_URL: String = "https://x.com/"
        const val BEARER_TOKEN: String =
            "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA"
        const val API_MIN_INTERVAL_MS: Long = 2500L
        const val API_JITTER_MS: Long = 800L
        const val PAGE_MIN_INTERVAL_MS: Long = 1000L
        const val PAGE_JITTER_MS: Long = 300L
    }
}
