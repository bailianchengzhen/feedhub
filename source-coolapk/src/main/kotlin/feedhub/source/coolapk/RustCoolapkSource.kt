package feedhub.source.coolapk

import feedhub.core.CaptchaChallenge
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.CaptchaRequiredException
import feedhub.rust.RustCoolapk
import feedhub.rust.RustCoolapkHosted

/**
 * 酷安 Source 的 Rust 版：**连"请求怎么发"也在 Rust 里**（URL / 参数顺序 / 请求头顺序 / 翻页游标 /
 * `X-App-Token` 的计算与按秒缓存 / 状态码与错误码判定），只有真正的收发回调宿主。
 *
 * 三层（和 `source-bilibili` 的 `RustBiliSource` 同一个结构）：
 * 1. [RustCoolapkHosted]（`rust-bridge`）建一个 Rust 的 `FfiCoolapkSource` 对象，把宿主能力塞给它：
 *    **本类的那个 OkHttpClient**（`delegate.httpClient`，所以拦截器 / 代理 / Cookie / 限速 / 连接池和原来
 *    **同一份**）、**B站 的短链客户端**（`delegate.linkClient`，`b23.tv` 跟重定向用）、
 *    **媒体客户端**（`delegate.mediaClient`，播放地址的 `Range` 探针：不带 Cookie、不占 600ms 限速）、
 *    `ctx.store`（设备 / `installTime` 都在里面）、`ctx.cookies`；
 * 2. 推荐流 / 关注流 / **详情 / 评论 / 楼中楼 / 搜索 / 播放 / whoami** 走它；
 * 3. 只剩 `raw`（开发者工具）还委托给 Kotlin 的 [CoolapkSource]；**扫码这一家从来没有**
 *    （酷安网页登录没有扫码入口，`CoolapkPlugin.login.noQrHint` 让 cli 提示粘贴 Cookie）。
 *
 * **人机验证**：流程照 Kotlin 版一字不差，只是"认出要验证"这一步在 Rust 里
 * ——Rust 返回 [CaptchaRequiredException]，这里用 [SourceContext.captcha] 让**用户本人**完成一次验证，
 * 然后把答案带上**重试同一个请求一次**；没有 handler / 用户取消 / 重试还要验证，就翻成
 * [feedhub.core.RiskControlException]（文案在 [CoolapkCaptcha]，和参照实现共用一份）。**不循环、不自动绕**，
 * 版本号 / 身份参数一个字节都没改（评论那套 16.2.0 / API 16 是 Kotlin 现有实现里的，Rust 逐字节照抄）。
 *
 * 为什么还要那个委托对象：`CoolapkSource` 实现两个接口（VideoSource / SearchSource），
 * 用包装类逐个方法显式转发能保证"没搬的部分"一个不漏、`PluginContract` 也照样能检查它的
 * platform / feeds / hosts / login / search。它同时也是**平台客户端的所有者**（Rust 和它共用同一个
 * `OkHttpClient`；两份各自派生会把 600ms 限速拆成两个窗口）。
 *
 * 设备（`X-App-Device`）和 `installTime` **不从 Kotlin 传**：Rust 直接读 `ctx.store` 里那两份
 * （`delegate` 的构造函数保证它们已经在那儿了），所以"只用 store 里那一份"是结构上的保证。
 *
 * **没有开关**：插件工厂直接建这个类，
 * 原来的 Kotlin 实现（[CoolapkSource] + [KotlinCoolapkParse]）只留给对拍 / 请求等价测试当参照。
 */
class RustCoolapkSource internal constructor(
    private val ctx: SourceContext,
    apiBase: String = CoolapkSource.API,
    commentsBase: String = CoolapkSource.COMMENTS_API,
    /** 秒级时间戳（`X-App-Token` 只取决于设备码 + 秒）。测试里注入固定值才能逐字节比请求。 */
    private val nowSec: () -> Long = { System.currentTimeMillis() / 1000 },
) : VideoSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, CoolapkSource.API, CoolapkSource.COMMENTS_API)

    /**
     * 参照实现（只剩 `raw` —— 开发者工具；酷安**没有扫码登录**，见 [CoolapkSource] 的文件头），
     * 同时是**三个 OkHttpClient 的所有者**：
     * 设备在它的构造函数里落到 `ctx.store`，`httpClient` / `linkClient` / `mediaClient` 都从它拿，
     * 所以 Rust 和 Kotlin 共用同一份客户端（拦截器 / 代理 / Cookie / 限速 / 连接池）。
     */
    private val delegate = CoolapkSource(
        ctx,
        apiBase,
        commentsBase,
        parse = RustCoolapkParse(),
        nowSec = nowSec,
    )

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池；三个端口共用）。 */
    private val net = feedhub.rust.RustNetStack.create(
        proxies = listOf(COOLAPK to feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(COOLAPK), CoolapkPlugin.hosts)),
        defaultProxy = feedhub.rust.ProxyTarget.of(ctx.proxy.proxyFor(COOLAPK), CoolapkPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustCoolapkHosted(
        ctx,
        apiBase,
        commentsBase,
        net,
        delegate.deviceHeaders,
        COOLAPK,
        CoolapkPlugin.emotes?.codes.orEmpty(),
        // v3 token 的密码表（评论身份要用）：表归插件所有，构造时给一次
        AppToken.v3BlobBytes(),
    )

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts
    override val search = delegate.search

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustCoolapk.version()

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    /** Rust 侧记住的评论接口根（排障 / 测试用）。 */
    fun hostedCommentsBase(): String = hosted.commentsBase()

    /**
     * 当前登录用户：请求 + 解析都在 Rust（没有 `uid` Cookie 时**一个请求都不发**，直接 `null`）。
     * 服务端回"登录信息有误"也是 `null`（和 Kotlin 原逻辑一样）。
     */
    override suspend fun whoami(): UserRef? = hosted.whoami(nowSec())

    /** 推荐流 / 关注流：**整套请求逻辑在 Rust**（拼 URL / 头 / 游标 / token / 错误判定）。 */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> =
        hosted.feed(kind, cursor, nowSec())

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    /** 详情：请求 + 解析都在 Rust（`b23.tv` 短链跟重定向用 B站 那个客户端）。 */
    override suspend fun detail(id: String): FeedDetail = hosted.detail(id, nowSec())

    /**
     * 一级评论：Rust 认出 `err_request_captcha_v2` 就抛 [CaptchaRequiredException]，这里让用户本人验证一次，
     * 再带着答案**重试同一个请求一次**。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        withCaptcha { answer -> hosted.comments(id, cursor, sort, nowSec(), answer) }

    /** 楼中楼：和一级评论同一条路（同一次验证对 `feed_reply` 也生效）。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        withCaptcha { answer -> hosted.replies(commentId, cursor, nowSec(), answer) }

    /**
     * 播放：请求（`/v6/feed/detail` + 一次 `Range: bytes=0-0` 探针）和解析都在 Rust。
     *
     * `partId` 只认 `main`（别的报 `bad_part`），`quality` 不参与（酷安一条视频只有一档整段 mp4）。
     * 防盗链拒了就是 [feedhub.core.SignatureException]——**不返回注定播不了的地址**。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId, quality, nowSec())

    /** 搜索：`/v6/search` 的请求 + 解析都在 Rust（游标是 `page|lastItem`，三档排序都支持）。 */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> =
        hosted.search(request.text, request.sort, cursor, nowSec())

    // ---------------------------------------------------------------- 人机验证

    /**
     * 评论 / 楼中楼的"验证一次再重试一次"。
     *
     * 顺序照 `CoolapkSource.getJson` 的 `allowCaptcha` 分支：
     * 1. 先按原样请求一遍；Rust 说"要验证"就拿出挑战（`captchaId` / `field` / 服务端原话）；
     * 2. 没有 [SourceContext.captcha] → 风控错（说清楚没有可用的验证方式）；
     * 3. 用户取消（`solve` 返回 null）→ 风控错（说清楚再点会再弹）；
     * 4. `field` 为空：答案写进 `validate` cookie（[CoolapkCaptcha.storeValidate]，和 Kotlin 版一字不差）；
     *    非空：把 `field=proof` 作为查询参数交回 Rust（追加在参数表最后，和 `addQueryParameter` 同位置）；
     * 5. **只重试一次**：还要验证就抛风控错，不循环。
     */
    private suspend fun <T> withCaptcha(
        block: suspend (RustCoolapkHosted.CaptchaAnswer?) -> T,
    ): T {
        val challenge = try {
            return block(null)
        } catch (e: CaptchaRequiredException) {
            e
        }
        val handler = ctx.captcha ?: throw CoolapkCaptcha.noHandler(platform, challenge.serverMessage)
        val proof = handler.solve(captchaChallenge(challenge))
            ?: throw CoolapkCaptcha.cancelled(platform, challenge.serverMessage)
        if (challenge.field.isEmpty()) CoolapkCaptcha.storeValidate(ctx, proof)
        val answer = RustCoolapkHosted.CaptchaAnswer(challenge.field, proof)
        return try {
            block(answer)
        } catch (e: CaptchaRequiredException) {
            // 只重试一次：服务端仍然要验证就照 Kotlin 原逻辑报错（文案用**第一次**那句，和 Kotlin 一样）
            throw CoolapkCaptcha.stillRequired(platform, challenge.serverMessage)
        }
    }

    private fun captchaChallenge(e: CaptchaRequiredException): CaptchaChallenge =
        CoolapkCaptcha.challenge(platform, e.captchaId, e.field, e.serverMessage)
}
