package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.FollowTarget
import feedhub.core.PlayInfo
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.UserRef
import feedhub.core.Platform
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import uniffi.feedhub_ffi.FfiTiebaHostedFeed
import uniffi.feedhub_ffi.FfiTiebaHostedForums
import uniffi.feedhub_ffi.FfiTiebaParsedComments
import uniffi.feedhub_ffi.FfiTiebaParsedDetail
import uniffi.feedhub_ffi.FfiTiebaParsedMainVideo
import uniffi.feedhub_ffi.FfiTiebaParsedPlay
import uniffi.feedhub_ffi.FfiTiebaQr
import uniffi.feedhub_ffi.FfiTiebaSearchSort
import uniffi.feedhub_ffi.FfiTiebaSource

/**
 * **贴吧平台对象的 Kotlin 门面**：`FfiTiebaSource`（Rust 里的 `uniffi::Object`）+ 宿主回调。
 *
 * 和 [RustTieba]（纯函数门面）的分工：那个只做"给我一段 body、我给你一页"；这个让 Rust 管**整个请求**——
 * 拼 URL / 参数 / 头（**顺序**）/ multipart 请求体（**逐字节**）/ 页码 / 状态码与错误外壳的判定 / 解析，
 * 真正的收发回调宿主用传进来的两个客户端做。
 *
 * **两条端口**（和 `TiebaSource` 里那两个客户端一样，不能合成一个）：
 * - App 接口（`tiebac.baidu.com`）：**不带 Cookie、没有默认头**，登录凭据在 protobuf 字段里；
 * - 网页接口（`tieba.baidu.com`）：带 Cookie + 桌面 UA / Referer（只对百度域生效）。
 *
 * 两条在 Rust 里是**同一个 `NetClient` 上的两份 spec**（连接池共用），限速器各一个实例——
 * 和 Kotlin 那边"两个客户端、各自一个 `MinIntervalInterceptor`"逐条对齐。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来），
 * 所以 `.so` 里没有 tokio、没有后台线程；`recommend()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会把在飞的 OkHttp Call 掐掉。
 */
class RustTiebaHosted(
    private val ctx: SourceContext,
    private val appBase: String,
    private val webBase: String,
    /** **Rust 联网层**：接口请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
    /** passport 的根（线上 `https://passport.baidu.com`；测试里和 [webBase] 同一个 MockWebServer）。 */
    private val passportBase: String = "https://passport.baidu.com",
    /** 扫码二维码（一张 PNG）的解码器：zxing 只有 Java 版，所以由平台模块注入。 */
    private val qrDecoder: (ByteArray) -> String? = { null },
    /** unicast 长轮询的读超时（毫秒；测试里改小）。 */
    private val unicastTimeoutMs: () -> Long = { 10_000 },
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，
     * 见 [RustNative]），失败时走 [HostedCall.guard] 的统一报错。
     */
    private val native: FfiTiebaSource by HostedCall.native {
        FfiTiebaSource.withNet(
            appBase,
            webBase,
            net.inner,
            // **App 接口：不带 Cookie、没有默认头**（接口自己带签名）。`followRedirects = true`
            // 是客户端的默认值；贴吧 App 接口不靠重定向。
            net.spec(
                platform = platform,
                defaultHeaders = emptyMap(),
                minIntervalMs = TiebaRate.MIN_INTERVAL_MS,
            ),
            // **网页接口：带 Cookie + 桌面 UA / Referer**（网页按登录态回内容）。
            net.spec(
                platform = platform,
                defaultHeaders = tiebaWebHeaders(),
                // 网页默认头**只对百度域**生效（`TiebaSource` 的 `hostFilter`）：
                // App 接口那条走另一个客户端，一个默认头都不该有。
                defaultHeaderHosts = listOf("baidu.com"),
                minIntervalMs = TiebaRate.MIN_INTERVAL_MS,
            ),
            RustStoreHost(ctx.store),
        )
    }

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 扫码登录的平台对象（另一个 Rust 对象：多一个"临时 Cookie 罐"的宿主能力）。
     * 二维码图解码用 [decodeQr]（贴吧的二维码是百度给的一张 PNG，内容得扫出来）。
     *
     * 客户端必须是"不跟重定向"的那一份（Kotlin `qrClient`：STOKEN 那一步要逐跳看 Location）。
     */
    private val qrNative: FfiTiebaQr by HostedCall.native {
        FfiTiebaQr(
            passportBase,
            appBase,
            webBase,
            unicastTimeoutMs(),
            // 扫码和网页接口**同一份口径**（桌面 UA / Referer、只对百度域生效、600ms 限速），
            // 另外**不跟重定向**（`qrClient`：STOKEN 那一步要逐跳看 Location）+ 二维码图用 zxing 解
            qrHostOf(
                net = net,
                platform = platform,
                defaultHeaders = tiebaWebHeaders(),
                defaultHeaderHosts = listOf("baidu.com"),
                minIntervalMs = TiebaRate.MIN_INTERVAL_MS,
                followRedirects = false,
                qrImage = qrDecoder,
            ),
        )
    }

    /** Rust 侧记住的两个接口根（排障用：确认拿到的是同一份 base）。 */
    fun appBase(): String = native.appBase()

    fun webBase(): String = native.webBase()

    /**
     * 一页 + 平台错误码（`error` 非空时 `page` 是空的）。
     *
     * 平台错误**不在这里归类**：`TiebaSource.mapError` 是唯一一份规则（未登录 / 风控 / 110001 签名 / Api），
     * 所以这里只把 `errorCode` / `userMsg` / `errorMsg` 带出去。
     */
    class Feed(
        val page: Page<FeedItem>,
        /** 吧内帖子列表：这是不是这个吧的最后一页（推荐流恒为 false）。 */
        val last: Boolean,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        /** 文案规则和 Kotlin 那两处一致：`user_msg.ifBlank { error_msg }`。 */
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 详情 + 平台错误（`error` 为 true 时 [detail] 是 null）。 */
    class Detail(
        val detail: FeedDetail?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        /** 和 `TiebaSource.mapError` 的调用方一样：`user_msg.ifBlank { error_msg }`。 */
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 一页评论（一级评论 / 楼中楼）+ 平台错误。 */
    class Comments(
        val page: Page<Comment>?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 一楼的主视频（`play(partId=main)` 的地址）。 */
    class MainVideo(
        val url: String,
        val cover: String?,
        val durationSec: Int?,
        val width: Int?,
        val height: Int?,
    )

    /** 主视频 + 平台错误。`error` 为 false 而 [video] 是 null 时表示"这条帖子一楼没有视频"。 */
    class Play(
        val video: MainVideo?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /**
     * 播放地址 + 平台错误（`FfiTiebaSource::play` 的还原）。
     *
     * `error` 为 true 时按 `TiebaSource.mapError` 归类后抛；`error` 为 false 而 [info] 是 null 时表示
     * "帖子里没有那个视频"（`main` 时是一楼没视频），调用方翻成 `ParseException`。
     */
    class PlayResult(
        val info: PlayInfo?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 关注的吧 + 登录状态。`error` 为 true 时 `errorCode` / `errorMsg` 是平台给的错。 */
    class Forums(
        val forums: List<RustTieba.Forum>,
        /** `forumGuide` 的 `is_login`；null = 响应里没有这个字段（和 Kotlin `json.long("is_login")` 一样）。 */
        val isLogin: Long?,
        val error: Boolean,
        val errorCode: Int,
        val errorMsg: String,
    )

    /**
     * 关注流里一个吧的缓冲摘要（排障 / 测试用）。
     *
     * @property items 缓冲里现在有几条
     * @property tailPos 服务端列表里"缓冲末尾"对应的位置数
     * @property exhausted 缓冲末尾已经是这个吧的最后一条
     * @property headFetchedAt 上次抓第一页的时间（毫秒，0 = 从没抓过）
     * @property closedUntil 吧读不了时关到什么时候（毫秒）
     */
    class TiebaForumState(
        val id: Long,
        val name: String,
        val items: Int,
        val tailPos: Int,
        val exhausted: Boolean,
        val headFetchedAt: Long,
        val closedUntil: Long,
    )

    /**
     * 推荐流一页。[nowMs] 是毫秒时间戳（请求体里的 `_timestamp` / `event_day`、`client_logid` 都用它）——
     * 由调用方注入，测试才能把两次运行的请求逐字节比。
     */
    suspend fun recommend(cursor: Cursor?, nowMs: Long): Feed = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaHostedFeed = native.recommend(cursor?.value, nowMs)
            Feed(
                RustModel.toPage(ffi.page, platform),
                ffi.last,
                ffi.error,
                ffi.errorCode,
                ffi.userMsg,
                ffi.errorMsg,
            )
        }
    }

    /** 关注流里某个吧的一页（匿名请求）。`page` 从 1 起。 */
    suspend fun frsPage(forumName: String, page: Int, nowMs: Long): Feed = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaHostedFeed = native.frsPage(forumName, page, nowMs)
            Feed(
                RustModel.toPage(ffi.page, platform),
                ffi.last,
                ffi.error,
                ffi.errorCode,
                ffi.userMsg,
                ffi.errorMsg,
            )
        }
    }

    /**
     * 帖子详情：**整套请求 + 解析都在 Rust**（`pb/page` 第一页、正序、只要一楼、带登录凭据）。
     * 平台错误码原样带回去（`error`），归类仍走 `TiebaSource.mapError`。
     */
    suspend fun detail(id: String, nowMs: Long): Detail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaParsedDetail = native.detail(id, nowMs)
            Detail(
                detail = ffi.detail?.let { RustModel.toDetail(it, platform) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }
    }

    /** 一级评论（楼层）：游标是页码，[sort] 由 Rust 映射成 `r`（正序 / 倒序 / 热门）。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, nowMs: Long): Comments =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                val ffi: FfiTiebaParsedComments = native.comments(id, cursor?.value, sort.toFfi(), nowMs)
                Comments(
                    page = ffi.page?.let { RustModel.toCommentPage(it, platform) },
                    error = ffi.error,
                    errorCode = ffi.errorCode,
                    userMsg = ffi.userMsg,
                    errorMsg = ffi.errorMsg,
                )
            }
        }

    /** 某一楼的楼中楼：`commentId` 是楼层的 pid，游标也是页码。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?, nowMs: Long): Comments =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                val ffi: FfiTiebaParsedComments = native.replies(id, commentId, cursor?.value, nowMs)
                Comments(
                    page = ffi.page?.let { RustModel.toCommentPage(it, platform) },
                    error = ffi.error,
                    errorCode = ffi.errorCode,
                    userMsg = ffi.userMsg,
                    errorMsg = ffi.errorMsg,
                )
            }
        }

    /**
     * 播放地址（`partId=main` 或 `url:<mp4>`）：**请求和解析全在 Rust**
     * （`feedhub-core/src/tieba/play.rs`）。
     *
     * `main` 时 5 分钟内 [detail] 取过的一楼直接复用（**不再发请求**）；`url:<mp4>` 一个请求都不发。
     * `partId` 写法不对时 Rust 在发请求之前就报错。
     */
    suspend fun play(id: String, partId: String, nowMs: Long): PlayResult = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaParsedPlay = native.play(id, partId, nowMs)
            PlayResult(
                info = ffi.info?.let { RustModel.toPlayInfo(it) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }
    }

    /**
     * 帖子的主视频（**只取地址**，不带 `PlayInfo`）：Rust 那边 5 分钟内详情取过的第一页直接复用，
     * 没有才现取一次。没有主视频时 `video` 是 null、`error` 是 false。
     *
     * 这个方法留着给对拍参照 / 排障。
     */
    suspend fun mainVideo(id: String, nowMs: Long): Play = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaParsedMainVideo = native.mainVideo(id, nowMs)
            Play(
                video = ffi.video?.let { MainVideo(it.url, it.cover, it.durationSec, it.width, it.height) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }
    }

    /**
     * 搜一页：**请求怎么拼和结果怎么解析都在 Rust**（`feedhub-core/src/tieba/search.rs`）。
     *
     * [request]`(SearchRequest)` 的 `community` 决定走哪种（和 Kotlin 版同一行）：空 → **全吧搜索**
     * （网页 GET，PC salt 签名）；给了吧名 → **吧内搜索**（App 表单 POST，App salt 签名）。
     * 平台错误码原样带回去（`error`），归类仍走 `TiebaSource.mapError`。
     * [nowMs] 只给吧内搜索的 `client_logid` 用。
     */
    suspend fun search(request: SearchRequest, cursor: Cursor?, nowMs: Long): Feed = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaHostedFeed = native.search(
                request.text,
                request.sort.toFfiTieba(),
                request.community?.takeIf { it.isNotBlank() },
                cursor?.value,
                nowMs,
            )
            Feed(
                RustModel.toPage(ffi.page, platform),
                ffi.last,
                ffi.error,
                ffi.errorCode,
                ffi.userMsg,
                ffi.errorMsg,
            )
        }
    }

    /** 关注的吧（网页接口的表单 POST）。 */
    suspend fun forumGuide(): Forums = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiTiebaHostedForums = native.forumGuide()
            Forums(
                ffi.forums.map { RustTieba.Forum(it.id, it.name, it.dayThreads, it.avatar, it.memberCount, it.threadCount) },
                ffi.isLogin,
                ffi.error,
                ffi.errorCode,
                ffi.errorMsg,
            )
        }
    }
    /**
     * **关注流一页**（Kotlin `TiebaSource.following`）：吧列表 TTL → 哪些吧该重抓第一页 →
     * 惰性 k 路归并 → 真发 `frs/page` → 存盘，**全在 Rust**。
     *
     * [cursor] 是上一页最后一条的 `"<秒>:<tid>"`；`null` = 刷新（会联网），非空 = 只在缓存里继续往下排。
     */
    suspend fun followingPage(cursor: Cursor?, nowMs: Long): Page<FeedItem> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val page = native.followingPage(cursor?.value, nowMs)
            RustModel.toPage(page.page, platform)
        }
    }

    /** 关注流手上那份缓冲（排障 / 测试用：每个吧一条摘要，不发请求、不读盘）。 */
    suspend fun followingForums(): List<TiebaForumState> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.followingForums().map {
                TiebaForumState(it.id, it.name, it.items.toInt(), it.tailPos, it.exhausted, it.headFetchedAt, it.closedUntil)
            }
        }
    }

    /** 「我是谁」：网页接口 `newmoindex`。没登录就是 `null`（不抛、不发请求）。 */
    suspend fun whoami(): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.whoami()?.let { RustModel.toUser(it) } }
    }

    /**
     * 关注（`follow = true`）/ 取关一个吧。**写请求在 Rust**（tbs、签名、表单、错误归类），
     * 只在用户点按钮时调用、不重试。
     */
    suspend fun setFollowing(
        target: FollowTarget,
        follow: Boolean,
        forumName: String?,
        nowMs: Long,
    ) = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.setFollowing(target.toFfi(), follow, forumName, nowMs)
            Unit
        }
    }

    /** 扫码登录第一步：要一张二维码（[gid] 由调用方生成）。 */
    suspend fun startQrLogin(nowMs: Long, gid: String): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toQrLogin(qrNative.start(nowMs, gid)) }
    }

    /** 扫码登录第二步：问一次状态（长轮询，读超时在构造时给）。 */
    suspend fun pollQrLogin(login: QrLogin, nowMs: Long, gid: String): QrLoginState =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) { RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs, gid)) }
        }

}

/**
 * `SearchSort` → `FfiTiebaSearchSort`。贴吧只有两档（没有"最热"），但变体照三档给——
 * 传 `HOT` 进来 Rust 会映射成"相关"，插件能力里没声明它，聚合层不会传。
 */
internal fun SearchSort.toFfiTieba(): FfiTiebaSearchSort = when (this) {
    SearchSort.RELEVANCE -> FfiTiebaSearchSort.RELEVANCE
    SearchSort.LATEST -> FfiTiebaSearchSort.LATEST
    SearchSort.HOT -> FfiTiebaSearchSort.HOT
}

/** 贴吧网页接口的默认头（`TiebaSource` 那张表，**顺序就是线上顺序**）。 */
private fun tiebaWebHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Referer" to "https://tieba.baidu.com/",
)

/** 贴吧的限速（`TiebaSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object TiebaRate {
    const val MIN_INTERVAL_MS = 600L
}
