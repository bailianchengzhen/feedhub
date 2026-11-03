package feedhub.source.youtube

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.DeviceCodeLogin
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.DeviceCodeLoginState
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.ScriptEngineProbe
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.VideoSource
import feedhub.rust.ProxyTarget
import feedhub.rust.RustNetStack
import feedhub.rust.RustScriptHost
import feedhub.rust.RustYoutubeHosted
import feedhub.rust.RustYoutubeLoginUrls
import uniffi.feedhub_ffi.FfiYoutubePollKind

/**
 * YouTube Source 的 Rust 版：
 * - 信息流（推荐 / 关注）、搜索、详情、评论、楼中楼、播放取流、`raw` 调试请求；
 * - 设备码登录状态机、token 存取与过期计算、单飞 refresh 三态编排、401 换一次 token 重试；
 * - 全部在 Rust（`feedhub-core` + `feedhub-ffi` + `feedhub-net`）中闭环执行，仅 QuickJS n/sig 留在宿主。
 */
class RustYoutubeSource internal constructor(
    ctx: SourceContext,
    private val base: String = API,
    loginUrls: LoginUrls = LoginUrls.DEFAULT,
    /**
     * 秒级时钟。信息流 / 详情 / 评论的相对时间（"3 days ago"）两边要用同一个，对拍才稳定；
     * 生产里就是系统时钟，测试里注入固定值。
     */
    private val nowSec: () -> Long = { System.currentTimeMillis() / 1000 },
    /** 最小请求间隔；只有测量端到端耗时 / 高步数对拍的测试会设成 0（生产 500ms）。 */
    minIntervalMs: Long = MIN_INTERVAL_MS,
    /**
     * n/sig 的 JS 求解（QuickJS 在 Kotlin 那边）。生产的默认值按需建一个 `YoutubeEjs`（和 Kotlin 版
     * 一样整个 Source 共用一个、空闲 20 秒释放）；测试可以塞一个假的。
     */
    private val scriptEngine: YoutubeNjs? = null,
    /** 毫秒时钟。过期计算、健康冷却与播放器脚本 TTL 使用它。 */
    private val clockMs: () -> Long = { System.currentTimeMillis() },
) : VideoSource, DeviceCodeLoginSource, SearchSource, ScriptEngineProbe {
    constructor(ctx: SourceContext) : this(ctx, API, LoginUrls.DEFAULT)

    override val platform = YoutubePlugin.platform
    override val feeds = YoutubePlugin.capabilities.feeds
    override val login = YoutubePlugin.login.web
    override val apiBase = base
    override val hosts = YoutubePlugin.hosts
    override val search = YoutubePlugin.capabilities.search!!

    /** EJS 运行时：用到才建，整个 Source 共用一个。 */
    private val ejs: YoutubeNjs by lazy { scriptEngine ?: YoutubeEjs() }

    /** Rust 联网层（一份 = 一个代理快照 + 一个连接池）。 */
    private val net = RustNetStack.create(
        proxies = listOf(platform to ProxyTarget.of(ctx.proxy.proxyFor(platform), hosts)),
        defaultProxy = ProxyTarget.of(ctx.proxy.proxyFor(platform), hosts),
        jar = ctx.cookies,
    )

    private val hosted = RustYoutubeHosted(
        ctx = ctx,
        base = base,
        net = net,
        platform = platform,
        loginUrls = RustYoutubeLoginUrls(
            tvDeviceCode = loginUrls.tvDeviceCode,
            tvToken = loginUrls.tvToken,
            googleDeviceCode = loginUrls.googleDeviceCode,
            googleToken = loginUrls.googleToken,
        ),
        script = RustScriptHost { playerJs, playerKey, requests ->
            ejs.solve(playerJs, playerKey, requests.map { (type, challenges) -> YoutubeEjs.Request(type, challenges) })
        },
        minIntervalMs = minIntervalMs,
    )

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun hostedBase(): String = hosted.apiBase()

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> =
        hosted.feedSession(kind, cursor, nowSec(), clockMs())

    override suspend fun whoami(): UserRef? = hosted.whoamiSession(clockMs())

    override suspend fun detail(id: String): FeedDetail = hosted.detailSession(id, nowSec(), clockMs())

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        hosted.commentsSession(id, cursor, sort, nowSec(), clockMs())

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        hosted.repliesSession(id, commentId, cursor, nowSec(), clockMs())

    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo =
        hosted.playSession(id, partId, quality, clockMs())

    override suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> = emptyList()

    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> =
        hosted.search(request, cursor, nowSec())

    override suspend fun startDeviceCodeLogin(): DeviceCodeLogin {
        val code = hosted.startDeviceCodeSession(clockMs())
        return DeviceCodeLogin(
            platform = platform,
            deviceCode = code.deviceCode,
            userCode = code.userCode,
            verificationUrl = code.verificationUrl,
            pollIntervalMs = code.pollIntervalMs,
            expiresAtMs = code.expiresAtMs,
            hint = code.hint,
        )
    }

    override suspend fun pollDeviceCodeLogin(login: DeviceCodeLogin): DeviceCodeLoginState {
        val poll = hosted.pollDeviceCodeSession(login.deviceCode, clockMs())
        return when (poll.kind) {
            FfiYoutubePollKind.WAITING -> DeviceCodeLoginState.Waiting
            FfiYoutubePollKind.EXPIRED -> DeviceCodeLoginState.Expired
            FfiYoutubePollKind.DENIED -> DeviceCodeLoginState.Failed("用户在设备上拒绝了授权")
            FfiYoutubePollKind.FAILED -> DeviceCodeLoginState.Failed(poll.message ?: "设备码轮询失败")
            FfiYoutubePollKind.SUCCEEDED -> DeviceCodeLoginState.Success(null)
        }
    }

    override fun logout() {
        hosted.logoutSession()
    }

    override suspend fun keepSessionFresh() {
        hosted.keepSessionFresh(clockMs())
    }

    internal suspend fun refreshForTest(): Boolean = hosted.refreshInStore(clockMs())

    internal fun hasSession(): Boolean = hosted.hasSession(clockMs())

    override suspend fun raw(request: RawRequest): RawResponse = hosted.raw(request)

    override fun scriptEngineReady(): Boolean = runCatching { YoutubeEjs.Scripts.fromResources() }.isSuccess

    override suspend fun solveChallenges(
        playerJs: String,
        playerKey: String,
        requests: List<Pair<String, List<String>>>,
    ): Map<String, String> {
        val mapped = requests.map { (type, challenges) -> YoutubeEjs.Request(type, challenges) }
        val injected = scriptEngine
        return if (injected != null && injected !is YoutubeEjs) {
            injected.solve(playerJs, playerKey, mapped)
        } else {
            YoutubeEjs().use { engine ->
                engine.solve(playerJs, playerKey, mapped)
            }
        }
    }

    companion object {
        const val API = "https://www.youtube.com"
        const val MIN_INTERVAL_MS = 500L
    }
}
