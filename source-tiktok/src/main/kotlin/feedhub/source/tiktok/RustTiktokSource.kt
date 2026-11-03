package feedhub.source.tiktok

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.NativeDetailSource
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SourceContext
import feedhub.core.UnsupportedFeedException
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.RustTiktok
import feedhub.rust.RustTiktokHosted

/**
 * TikTok Source 的 Rust 版：**连"请求怎么发"也在 Rust 里**（URL / 参数顺序 / 请求头顺序 /
 * `X-Dynosaur` + `X-Gnarly` 签名 / 翻页游标 / 首页设备参数的首次获取 / 状态码与 `tt_orcas_res` 判定），
 * 只有真正的收发回调宿主。
 *
 * 三层（和 `source-bilibili` 的 `RustBiliSource` 同一个结构）：
 * 1. [RustTiktokHosted]（`rust-bridge`）建一个 Rust 的 `FfiTiktokSource` 对象，把三件宿主能力塞给它：
 *    **本类的那个 OkHttpClient**（`delegate.httpClient`，所以拦截器 / 代理 / Cookie / 限速 / 连接池和原来
 *    **同一份**）、`ctx.store`（设备 / `msToken` / `verifyFp` 都在里面）、`ctx.cookies`；
 * 2. 推荐流走它（[feed]）；
 * 3. 详情 / 评论 / 楼中楼 / 播放也是 Rust；只有 `raw` 还委托给 Kotlin 的
 *    [TiktokSource]。
 *
 * **时间戳与随机数由 Kotlin 取**：每次 `feed` 从 [random] 按 Kotlin 版 `SignInterceptor` 的同一个顺序
 * 取 24 个 32 位密钥、从 [clock] 取毫秒时间戳，作为参数传给 Rust。所以签名对拍（请求等价门）能覆盖到
 * 签名整条链，而不是"Rust 自己取随机数、两边没法比"。
 *
 * **TikTok 没有关注流**（要 ticket-guard 的 security-sdk 私钥）：[feed] 对 [FeedKind.FOLLOWING] 抛
 * [UnsupportedFeedException]，和 Kotlin 版 `feed()` 的 `when` 一字不差，**在进 Rust 之前**就拒绝。
 *
 * **没有开关**：插件工厂直接建这个类，原来的 Kotlin
 * 实现（[TiktokSource]）只留给请求等价 / 结果等价测试当参照。
 */
class RustTiktokSource internal constructor(
    ctx: SourceContext,
    apiBase: String = TiktokSource.API,
    /** 签名用的随机源（生产 [TiktokRandom.SYSTEM]，测试注入固定种子以便逐字节比）。 */
    private val random: TiktokRandom = TiktokRandom.SYSTEM,
    /** 毫秒时钟（`timestamp` / `nonce` / `WebIdLastTime` 的兜底都用它）。 */
    private val clock: () -> Long = System::currentTimeMillis,
) : NativeDetailSource, VideoSource {
    constructor(ctx: SourceContext) : this(ctx, TiktokSource.API)

    /** 没搬的那些方法的主人，同时是平台 OkHttpClient 的所有者。 */
    private val delegate = TiktokSource(ctx, apiBase, random, clock)

    /** Rust 平台对象（收发走 [delegate] 那个客户端，和它共用同一份拦截器 / 连接池 / 限速）。 */
    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(TIKTOK to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(TIKTOK), TiktokPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(TIKTOK), TiktokPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustTiktokHosted(ctx, apiBase, net, TIKTOK)

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustTiktok.version()

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    /**
     * 登录态 / 账号：**首页 hydration 的请求 + 解析都在 Rust**。没登录时 0 个请求；页面里没有账号 → `null`。
     */
    override suspend fun whoami(): UserRef? = hosted.whoami()

    /**
     * 推荐流：**整套请求逻辑在 Rust**（拼 URL / 头 / 游标 / 签名 / 错误判定）。
     *
     * 顺序照 Kotlin 版 `recommend()`：先让 [TiktokSource.ensureHostedDevice] 把 `verifyFp` 落到 store
     * （生成留在 Kotlin），再取随机密钥与时间戳，再进 Rust（Rust 内部先判登录、再补设备数据）。
     */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> {
            delegate.ensureHostedDevice()
            val nowMs = clock()
            // 取用顺序照 Kotlin `SignInterceptor`：先两组各 12 个密钥字（key 再 key2）
            val key = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
            val key2 = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
            hosted.feed(cursor, nowMs, key, key2)
        }
        FeedKind.FOLLOWING -> throw UnsupportedFeedException(platform, kind)
    }

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    /**
     * 详情：**视频页 HTML 的 hydration**（不签名），整套请求 + 解析都在 Rust。
     * id 里没有作者时 Rust 在发请求之前就报 `no_author`（和 Kotlin `requireUniqueId` 一样）。
     */
    override suspend fun detail(id: String): FeedDetail = hosted.detail(id)

    /**
     * 一级评论：请求 / 签名 / 解析在 Rust；`nowMs` 与两组密钥照 [feed] 一样由 Kotlin 取
     * （Rust 不读时钟、不取随机数，两边的随机流才对得上）。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        delegate.ensureVerifyFp()
        val nowMs = clock()
        val key = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
        val key2 = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
        return hosted.comments(id, cursor, nowMs, key, key2)
    }

    /** 楼中楼：同 [comments]；id 里没有作者时 Rust 报 `no_referer`、一个请求都不发。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        delegate.ensureVerifyFp()
        val nowMs = clock()
        val key = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
        val key2 = List(TiktokSign.KEY_WORDS) { random.nextWord().toUInt() }
        return hosted.replies(id, commentId, cursor, nowMs, key, key2)
    }

    /**
     * 播放：**请求 + 解析都在 Rust**（视频页 HTML 的 `itemStruct.video`，不签名）。
     * 地址带时效签名，所以**每次都重新取一次视频页**，不复用详情那份（和 Kotlin 版一字不差）。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId, quality)
}
