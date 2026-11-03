package feedhub.source.bilibili

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.FollowSource
import feedhub.core.FollowTarget
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
import feedhub.rust.RustBili
import feedhub.rust.ProxyTarget
import feedhub.rust.RustBiliHosted
import feedhub.rust.RustNetStack

/**
 * B站 Source 的 Rust 版：**连"请求怎么发"也在 Rust 里**（URL / 参数顺序 / 请求头 / 翻页游标 /
 * WBI key 的获取与缓存 / 状态码与 `{code}` 外壳的判定），只有真正的收发回调宿主。
 *
 * 三层：
 * 1. [RustBiliHosted]（`rust-bridge`）建一个 Rust 的 `FfiBiliSource` 对象，把三件宿主能力塞给它：
 *    **本类的那个 OkHttpClient**（`delegate.httpClient`，所以拦截器 / 代理 / Cookie / 限速 / 连接池和原来**同一份**）、
 *    `ctx.store`、`ctx.cookies`；
 * 3. **播放 / 弹幕**也走它：`/x/player/wbi/playurl` 的 WBI 签名、
 *    `seg.so` 的 protobuf 全部在 Rust，Kotlin 只把 `FfiPlayInfo` / `FfiDanmaku` 还原成模型；
 * 4. 其余方法（whoami / 搜索 / 扫码 / 关注管理）还委托给 Kotlin 的 [BilibiliSource]。
 *
 * 为什么还要那个委托对象：`BilibiliSource` 实现四个接口（VideoSource / FollowSource / QrLoginSource /
 * SearchSource），用包装类逐个方法显式转发能保证"没搬的部分"一个不漏、`PluginContract` 也照样能检查
 * 它的 platform / feeds / hosts / login / search。它同时也是**平台客户端的所有者**：Rust 的宿主回调
 * 和它共用同一个 `OkHttpClient`（两份各自派生会把限速和拦截器拆成两套，见 [BilibiliSource.httpClient]）。
 *
 * 签名：还没搬的方法（搜索 / 关注管理）仍在 Kotlin 里用 [RustBiliParse] 调 Rust 的
 * WBI 纯函数——WBI key 的缓存键和格式与 Rust 平台对象**同一份**（`bili.wbi_mixin_key`），所以两边不会各取一次。
 */
class RustBiliSource internal constructor(
    ctx: SourceContext,
    base: String = BilibiliSource.API,
    /** WBI 签名用的秒级时间戳。测试里注入固定值，才能把两条实现的请求逐字节比。 */
    private val nowSec: () -> Long = { System.currentTimeMillis() / 1000 },
    /** 扫码登录用的毫秒时间戳（二维码有效期 / 轮询）。测试里和 [nowSec] 一起注入。 */
    private val nowMs: () -> Long = { System.currentTimeMillis() },
) : VideoSource, FollowSource, QrLoginSource, SearchSource {
    constructor(ctx: SourceContext) : this(ctx, BilibiliSource.API)

    private val delegate = BilibiliSource(ctx, base, RustBiliParse(BILIBILI), nowSec)

    /** passport 的根：线上和 `api.bilibili.com` 是两个域名，测试里（[base] 指向 MockWebServer）都在同一个地址。 */
    private val passportBase = if (base == BilibiliSource.API) BilibiliSource.PASSPORT else base

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = RustNetStack.create(
        proxies = listOf(BILIBILI to ProxyTarget.of(ctx.proxy.proxyFor(BILIBILI), BilibiliPlugin.hosts)),
        defaultProxy = ProxyTarget.of(ctx.proxy.proxyFor(BILIBILI), BilibiliPlugin.hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustBiliHosted(ctx, base, passportBase, net, BILIBILI)

    override val platform = delegate.platform
    override val feeds = delegate.feeds
    override val login = delegate.login
    override val apiBase = delegate.apiBase
    override val hosts = delegate.hosts
    override val search = delegate.search

    /** Rust 核心的版本（排障时看一眼：确认加载到的不是旧的 .so）。 */
    fun rustVersion(): String = RustBili.version()

    /** 「我是谁」：**请求 + 解析都在 Rust**（`/x/web-interface/nav`；没登录返回 null，不抛）。 */
    override suspend fun whoami(): UserRef? = hosted.whoami(nowSec())

    /** 推荐流 / 关注流：**整套请求逻辑在 Rust**（拼 URL / 签名 / 游标 / WBI key 缓存 / 错误判定）。 */
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = hosted.feed(kind, cursor, nowSec())

    override suspend fun raw(request: RawRequest): RawResponse = delegate.raw(request)

    /** 详情：**整套请求 + 解析在 Rust**（视频 `view/detail`、动态 `v1/detail` + `opus/detail`、错误码）。 */
    override suspend fun detail(id: String): FeedDetail = hosted.detail(id, nowSec())

    /** 一级评论：Rust 拼请求（WBI）/ 翻页 / 判错，Kotlin 只把 `FfiCommentPage` 还原成模型。 */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hosted.comments(id, cursor, sort, nowSec())

    /** 楼中楼：同上（游标是页码）。 */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hosted.replies(id, commentId, cursor, nowSec())

    /**
     * 播放地址：**整套请求 + 解析在 Rust**（WBI 签名、`fnval=4048`、`qn` 默认 127、清晰度表、
     * DASH / durl、CDN 排序）。拉流头（Referer + UA）也由 Rust 给，按顺序设到请求上。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.play(id, partId, quality, nowSec())

    /** 分段弹幕：Rust 拼请求（WBI）/ 换算 aid / 判 304 与 JSON 错误，protobuf 也在 Rust 解。 */
    override suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> =
        hosted.danmaku(id, partId, segment, nowSec())

    /** 关注的 UP 主：请求 / 游标 / 解析在 Rust（`/x/relation/followings`，每页 50 个）。 */
    override suspend fun followings(cursor: Cursor?): Page<FollowTarget> = hosted.followings(cursor, nowSec())

    /** 关注 / 取关：请求 / 表单 / 错误码在 Rust。唯一的写操作，只在用户点按钮时调用。 */
    override suspend fun setFollowing(target: FollowTarget, follow: Boolean) =
        hosted.setFollowing(target, follow, nowSec())

    /** 扫码登录：请求 / 状态码 / Cookie 落地都在 Rust（临时罐在宿主，成功之前不碰 `ctx.cookies`）。 */
    override suspend fun startQrLogin(): QrLogin = hosted.startQrLogin(nowMs())

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = hosted.pollQrLogin(login, nowMs())

    /**
     * 搜一页：**整套请求 + 解析在 Rust**（`/x/web-interface/wbi/search/type`、WBI 签名、
     * `Origin` / `Referer`、页码游标、`{code}` 外壳、`<em>` 高亮清理）。
     *
     * 原来那一条委托（`delegate.search`）留着当验收门的参照（`BiliSearchEquivalenceTest`）。
     * `pageSize` 用插件描述里的那一个（30），不是响应实际条数。
     */
    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> =
        hosted.search(request, cursor, search.pageSize, nowSec())
}
