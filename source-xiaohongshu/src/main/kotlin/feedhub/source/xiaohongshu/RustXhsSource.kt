package feedhub.source.xiaohongshu

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.RustXhs
import kotlin.random.Random

/**
 * 小红书 Source 的 "Rust 平台层" 版本：**HTTP 一个字节都不换**，只把"请求怎么拼 / 怎么签 / 怎么解析"交给 Rust。
 *
 * 做法是**委托**：内部持有一个 [XhsSource]（取一页的接缝换成 `HostedXhsFeedApi` / `HostedXhsSearchApi`、
 * 详情 / 评论 / 楼中楼换成 `HostedXhsDetailApi`、签名换成 `HostedXhsParse`），
 * 推荐流 / 关注流 / 搜索 / 详情 / 评论 / 楼中楼 / 播放 / 表情表 / **`whoami` / 扫码登录**的每一页由 Rust
 * 平台对象拼请求、签名、回调宿主发出去、判错、解析；
 * 只剩 `raw`（开发者工具）还走 Kotlin，这里逐个转发
 * （[XhsSource] 实现了哪些能力接口，这里就全部转发）。
 *
 * **没有开关**：插件工厂直接建这个类，
 * 原来的 Kotlin 解析 / 签名（[KotlinXhsParse]）只留给对拍当参照。
 */
class RustXhsSource internal constructor(
    ctx: SourceContext,
    api: String = XhsApi.API,
    profile: XhsProfile? = null,
    random: Random = Random.Default,
    minIntervalMs: Long = XhsApi.MIN_INTERVAL_MS,
    jitterMs: Long = XhsApi.JITTER_MS,
    nowMs: () -> Long = System::currentTimeMillis,
) : VideoSource, QrLoginSource, SearchSource {
    private val delegate = XhsSource(
        ctx, api, profile, random, minIntervalMs, jitterMs,
        // 签名 / 请求拼装 / 解析都在 Rust 平台对象里；`XhsSource` 建好 `XhsApi` 之后会把客户端交给它
        parseFactory = { p ->
            HostedXhsParse(p, random, ctx, api, minIntervalMs = minIntervalMs, jitterMs = jitterMs)
        },
        nowMs = nowMs,
    )

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts
    override val search = delegate.search

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustXhs.version()

    override suspend fun whoami(): UserRef? = delegate.whoami()

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = delegate.feed(kind, cursor)

    /** 搜索一页：搜索整个都在 Rust 平台对象里（请求 / 签名 / 判错 / 解析），这里只转发。 */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> = delegate.search(request, cursor)

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    override suspend fun detail(id: String): FeedDetail = delegate.detail(id)

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        delegate.comments(id, cursor, sort)

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        delegate.replies(id, commentId, cursor)

    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        delegate.play(id, partId, quality)

    override suspend fun startQrLogin(): QrLogin = delegate.startQrLogin()

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = delegate.pollQrLogin(login)
}
