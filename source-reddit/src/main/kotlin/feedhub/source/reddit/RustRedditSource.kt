package feedhub.source.reddit

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.ProxyTarget
import feedhub.rust.RustNetStack
import feedhub.rust.RustRedditHosted

/**
 * Reddit Source 的 Rust 版：**连"请求怎么发"也在 Rust 里**（URL / 参数顺序 / 请求头顺序 /
 * 翻页游标 / 登录态判定 / 状态码与风控信号判定），只有真正的收发回调宿主。
 *
 * 三层（和 `source-bilibili` 的 `RustBiliSource` / `source-coolapk` 的 `RustCoolapkSource` 同一个结构）：
 * 1. [RustRedditHosted]（`rust-bridge`）建一个 Rust 的 `FfiRedditSource` 对象，把两件宿主能力塞给它：
 *    **本类的那个 OkHttpClient**（`delegate.httpClient`，所以拦截器 / 代理 / Cookie / 限速 / 连接池和原来
 *    **同一份**）、`ctx.cookies`（登录态只看 `reddit_session` 在不在）；
 * 2. 推荐流 / 关注流（[feed]）、**搜索**、**详情 / 评论 / 楼中楼**
 * 走它；
 * 3. **播放**也走它（`main`）：清单取回 + MPD 解析 + DASH / HLS / fallback 回退
 *    都在 Rust（`reddit/play.rs`）；其余方法（whoami / raw）还委托给 Kotlin 的 [RedditSource]。
 *
 * 为什么还要那个委托对象：`RedditSource` 实现 [VideoSource]（详情 / 评论 / 播放都在里面），
 * 用包装类逐个方法显式转发能保证"没搬的部分"一个不漏、`PluginContract` 也照样能检查它的
 * platform / feeds / hosts / login。它同时也是**平台客户端的所有者**（Rust 和它共用同一个
 * `OkHttpClient`；两份各自派生会把 1000ms 限速拆成两个窗口）。
 *
 * **没有开关**：插件工厂直接建这个类，
 * 原来的 Kotlin 实现（[RedditSource]）只留给请求等价 / 结果等价测试当参照。
 */
class RustRedditSource internal constructor(
    ctx: SourceContext,
    base: String = RedditSource.WWW,
    minIntervalMs: Long = RedditSource.MIN_INTERVAL_MS,
    jitterMs: Long = RedditSource.JITTER_MS,
) : VideoSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, RedditSource.WWW, RedditSource.MIN_INTERVAL_MS, RedditSource.JITTER_MS)

    /**
     * 没搬的那些方法的主人。**同时是"这次用哪份代理"的判定来源**（`ctx.proxy`）。
     *
     * 它还留着是因为 `raw` / 平台描述的
     * 那几个属性要走它，而且它持有 `ctx` 的那一份解析逻辑。**它现在只当参照 / 兜底**。
     */
    private val delegate = RedditSource(ctx, base, minIntervalMs, jitterMs)

    /**
     * 这次建栈时解析出来的代理档（**就是交给 Rust 的那一份**；测试 / 排障用）。
     *
     * `System` 那一档不能直接当直连（红线），所以在这里就解析成具体的一档：拿插件描述里的主域名
     * 去问 `ProxySelector.getDefault()`（和 OkHttp 的 `PlatformProxySelector` 同一个口径），
     * 见 [ProxyTarget.of]。代理设置变了 `AppGraph.watchProxySettings` 会重造这个对象 → 重新解析。
     */
    internal val proxyTarget: ProxyTarget =
        ProxyTarget.of(ctx.proxy.proxyFor(REDDIT), RedditPlugin.hosts)

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = RustNetStack.create(
        proxies = listOf(REDDIT to proxyTarget),
        defaultProxy = proxyTarget,
        jar = ctx.cookies,
    )

    private val hosted = RustRedditHosted(ctx, base, net, REDDIT)

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    /**
     * 登录态 / 账号：**`GET /api/me.json` 的请求 + 解析都在 Rust**。没登录时 0 个请求；
     * Cookie 失效（401 / 403 + JSON）→ `null`；风控信号照旧抛异常（和 Kotlin 原版一致）。
     */
    override suspend fun whoami(): UserRef? = hosted.whoami()

    /** 推荐流 / 关注流：**整套请求逻辑在 Rust**（拼 URL / 头 / 游标 / 登录态判定 / 错误判定）。 */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = hosted.feed(kind, cursor)

    /** 搜索能力必须和描述里那份是同一份（`PluginContract` 会查）。 */
    override val search = RedditPlugin.capabilities.search!!

    /**
     * 搜索：**请求怎么拼 + 解析都在 Rust**（`/search.json`，见 [RustRedditHosted.search]）。
     *
     * 只收帖子（`type=link`）：用户 / 版块的结果 `FeedItem` 画不出来，能力里也没声明。
     * `community`（贴吧那种"吧内搜索"）Reddit 第一阶段不做，传了就报错，**不静默忽略**。
     *
     * 未登录：Rust 侧在发请求之前就返回 `NotLoggedInException`（未登录的 `.json` 是 403，那是风控信号，
     * 不该被一次"没登录"触发）——所以 `capabilities.search.needsLogin = true`。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        if (request.kind != SearchKind.ALL && request.kind != SearchKind.POST) {
            // 照 B站 `BiliSearchParser.searchType` 的口径：能力里没声明的类型直接报错，不静默退化成 ALL
            throw ParseException(platform, "Reddit 搜索只收帖子（ALL / POST），不搜 ${request.kind.name}")
        }
        if (!request.community.isNullOrBlank()) {
            throw ParseException(platform, "Reddit 搜索不支持限定版块（community=${request.community}）")
        }
        return hosted.search(request.text, request.sort, cursor)
    }

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    /** 详情：**整套请求 + 解析都在 Rust**。 */
    override suspend fun detail(id: String): FeedDetail = hosted.detail(id)

    /** 一级评论：游标 `more:<id1>,<id2>,…`，翻页走 `/api/morechildren`（都在 Rust）。 */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hosted.comments(id, cursor, sort)

    /** 楼中楼：`/comments/<postId>/_/<commentId>.json`（`more:` 开头走 `/api/morechildren`）。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hosted.replies(id, commentId)

    /**
     * 播放地址：**请求 + MPD（XML）解析全在 Rust**（`reddit/play.rs`，手写的最小 XML 解析器）。
     * 回退链照 Kotlin：DASH → `hls_url` → `fallback_url`（清单那一个请求失败不算错）。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId)
}
