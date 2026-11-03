package feedhub.source.douyin

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
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.RustDouyin
import feedhub.rust.RustDouyinHosted

/**
 * 抖音 Source 的 Rust 版：**推荐流的整条请求逻辑也在 Rust 里**（URL / 参数顺序 / `version_code` 那几套常量 /
 * 请求头顺序 / 翻页游标 / a_bogus / 空 body 与业务码 8 与两条风控头的判定），只有真正的收发回调宿主。
 *
 * 三层（和 `source-bilibili` 的 `RustBiliSource` 同一个结构）：
 * 1. [RustDouyinHosted]（`rust-bridge`）建一个 Rust 的 `FfiDouyinSource` 对象，把三件宿主能力塞给它：
 *    **本类的那个 OkHttpClient**（`delegate.httpClient`，所以拦截器 / 代理 / Cookie / 限速 / 连接池和原来
 *    **同一份**）、`ctx.store`（`webid` / `msToken` 在里面）、`ctx.cookies`（登录态 / `uifid` / `s_v_web_id`）；
 * 2. 推荐流走它（[feed] → [recommend]）；
 * 3. 详情 / 评论 / 楼中楼 / 播放也是 Rust；只有 whoami / 扫码登录还委托给
 *    Kotlin 的 [DouyinSource]。
 *
 * Rust **在同一个调用里**多签一次 `emoji/list`（请求条数和 Kotlin 参照实现一样），然后用**同一份 body**
 * 映射第二遍。所以 `texts` / `body` / `reparseFeed` 这条"回宿主再解析一遍"的路已经不用了——
 * 只在 `DouyinRequestEquivalenceTest` / `DouyinDetailRequestEquivalenceTest` 的对拍里还有意义。
 *
 * 为什么还要那个委托对象：`DouyinSource` 实现三个接口（`NativeDetailSource` / `VideoSource` / `QrLoginSource`），
 * 用包装类逐个方法显式转发能保证"没搬的部分"一个不漏、`PluginContract` 也照样能检查它的
 * platform / feeds / hosts / login（含 `login.qrLogin`）。它同时也是**平台客户端的所有者**——
 * Rust 和它共用同一个 `OkHttpClient`；两份各自派生会把 1 秒限速拆成两个窗口。
 *
 * 设备（`DouyinDevice`）**留在 Kotlin**：UA / 平台串 / 屏幕 / 核数 / 内存 / `install_time` / `verify_fp`
 * 由它读好（后两个来自 `ctx.store`）交给 Rust，Rust 不生成设备、不伪造身份。
 *
 * **没有开关**：插件工厂直接建这个类，
 * 原来的 Kotlin 实现（[DouyinSource] + `KotlinDouyinParse` / `KotlinDouyinSign`）只留给对拍当参照。
 */
class RustDouyinSource internal constructor(
    ctx: SourceContext,
    base: String = DOUYIN_API,
    abogusRandom: AbogusRandom = AbogusRandom.SYSTEM,
    clock: () -> Long = System::currentTimeMillis,
    enterPageTs: Long = clock(),
    parse: DouyinParse = RustDouyinParse(),
) : VideoSource, QrLoginSource {
    constructor(ctx: SourceContext) : this(ctx, DOUYIN_API)

    /** 没搬的那些方法的主人，同时是平台 OkHttpClient 的所有者（和 Rust 共用同一份）。 */
    private val delegate = DouyinSource(
        ctx = ctx,
        base = base,
        abogusRandom = abogusRandom,
        clock = clock,
        enterPageTs = enterPageTs,
        parse = parse,
        // profile / env / 时间都是 DouyinSource 建的时候才知道的（profile 还可能被 ctx.store 覆盖），
        // 所以用工厂把签名的活儿交出去；随机源还是同一个实例，两条路的随机流才对得上。
        signFactory = { profile, env, ts -> RustDouyinSign(profile, env, abogusRandom, ts) },
    )

    /** 推荐流的整条请求：Rust 拼请求 + 判定 + 第一遍解析，收发回调 [DouyinSource.httpClient]。 */
    /** passport 的根：线上和 `www.douyin.com` 是两个域名，测试里（[base] 指向 MockWebServer）都在同一个地址。 */
    private val passportBase = if (base == DOUYIN_API) DOUYIN_LOGIN else base

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(DOUYIN to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(DOUYIN), DouyinPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(DOUYIN), DouyinPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustDouyinHosted(
        ctx = ctx,
        base = base,
        net = net,
        passportBase = passportBase,
        client = delegate.httpClient,
        platform = delegate.platform,
        device = delegate.device.toRustDevice(),
        profile = delegate.abogusProfile.toRustProfile(),
        env = delegate.device.abogusEnv.toRustEnv(),
        enterPageTs = enterPageTs,
        random = { abogusRandom.nextDouble() },
        clock = clock,
    )

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustDouyin.version()

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    /** 「我是谁」：`profile/self`，请求 + 签名 + 解析在 Rust（没登录一个请求都不发）。 */
    override suspend fun whoami(): UserRef? = hosted.whoami()

    /** 推荐流：**整套请求逻辑在 Rust**；其余流仍然走 Kotlin（抖音没有关注流，会抛不支持）。 */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = when (kind) {
        FeedKind.RECOMMEND -> recommend(cursor)
        else -> delegate.feed(kind, cursor)
    }

    /**
     * 推荐流一页（和 [DouyinSource.recommend] 的步骤逐条对应，只是第一遍换了主人）：
     * Rust 发请求 + 第一遍解析（不带表情表）→ 文案里有表情记号才取表情表 → 同一份 body 映射第二遍。
     */
    private suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        // 表情表在 Rust（同一个调用里可能多签一次 `emoji/list`），这里不再回宿主解析第二遍
        return hosted.feed(cursor).page
    }

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    /**
     * 详情：**请求 + 解析都在 Rust**（`aweme/detail` 是 secsdk 保护路径）。
     *
     * 顺序照 Kotlin `detail`：取详情 → 文案里有表情记号才取表情表 → 用**同一份 body** 带表情表
     * 映射第二遍（[RustDouyin.parseDetail]）。
     *
     * 所以这里那一份没人读了——以前每次详情都要
     * 把整份 body 解析成 `JsonObject`（400 KB）只为了放缓存，现在省掉。
     */
    override suspend fun detail(id: String): FeedDetail = hosted.detail(id).detail

    /** 一级评论：请求 / 游标 / 解析在 Rust；表情表归 Kotlin，有记号才用同一份 body 再映射一遍。 */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hosted.comments(id, cursor).page

    /** 楼中楼：同 [comments]（`verifyFp` / `fp` 在 `a_bogus` 之后、不参与签名）。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hosted.replies(id, commentId, cursor).page

    /**
     * 播放：**请求 + 解析都在 Rust**（`aweme/detail` 的 `video`）。
     *
     * **60 秒内详情取过同一条作品就不再发请求**（`recentAwemes` 从 Kotlin 搬进了 Rust），所以
     * "详情 → 播放"仍然只有**一个** `aweme/detail` 请求；超过 60 秒才重新取一次（地址带时效参数）。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId, quality)

    /**
     * 扫码登录：请求 / a_bogus / 状态码 / Cookie 落地都在 Rust（临时罐在宿主，成功之前不碰 `ctx.cookies`）。
     * `biz_trace_id` 和 Kotlin 版一样每次生成新的（`newTraceId()`）。
     */
    override suspend fun startQrLogin(): QrLogin = hosted.startQrLogin(delegate.newTraceId())

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState =
        hosted.pollQrLogin(login, delegate.newTraceId())
}
