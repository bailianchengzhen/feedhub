package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.FollowTarget
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo

import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import feedhub.core.UserRef
import uniffi.feedhub_ffi.FfiBiliQr
import uniffi.feedhub_ffi.FfiBiliSearchKind
import uniffi.feedhub_ffi.FfiBiliSearchSort
import uniffi.feedhub_ffi.FfiBiliSource

/**
 * **B站 平台对象的 Kotlin 门面**：`FfiBiliSource`（Rust 里的 `uniffi::Object`）+ 三个宿主回调。
 *
 * 和 [RustBili]（纯函数门面）的分工：
 * - [RustBili] 只做"给我一段 body、我给你一页"；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数 / 头、翻页游标、WBI key 的获取与缓存、错误判定，
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 */
class RustBiliHosted(
    private val ctx: SourceContext,
    private val base: String,
    /** passport 的根（线上 `https://passport.bilibili.com`；测试里和 [base] 同一个 MockWebServer）。 */
    private val passportBase: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，
     * 见 [RustNative]），而且失败时要走 [HostedCall.guard] 的统一报错。
     */
    private val native: FfiBiliSource by HostedCall.native {
        FfiBiliSource.withNet(
            base,
            net.inner,
            // B站：桌面 Chrome + Referer / Origin，500ms 限速（`BilibiliSource.MIN_INTERVAL_MS`）
            net.spec(
                platform = platform,
                defaultHeaders = biliHeaders(),
                minIntervalMs = BiliRate.MIN_INTERVAL_MS,
            ),
            RustStoreHost(ctx.store),
        )
    }

    /**
     * 扫码登录的平台对象（**另一个** Rust 对象）：多一个"临时 Cookie 罐"的宿主能力。
     * 懒建，和 [native] 一样。二维码图解码用不到（百度那条流程才需要）。
     */
    private val qrNative: FfiBiliQr by HostedCall.native {
        FfiBiliQr(
            base,
            passportBase,
            // 扫码请求和接口请求**同一份口径**（默认头 / 500ms 限速），只是罐子换成临时的（Rust 里逐请求给）
            qrHostOf(
                net = net,
                platform = platform,
                defaultHeaders = biliHeaders(),
                minIntervalMs = BiliRate.MIN_INTERVAL_MS,
            ),
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /**
     * 取消在飞的请求（`HostCallScope` 在协程被取消时调；见 `HostedCall.blocking(cancelInFlight)`）。
     *
     * 走 [RustNetStack.cancel]：它记着自己建出来的那些 host，逐个掐在飞的那条。
     */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 一页。[nowSec] 是秒级时间戳（WBI 的 `wts`、Cookie 过期、"今天"的 WBI key 都用它）——
     * 由调用方注入，测试才能把两次运行的请求逐字节比。
     */
    suspend fun feed(kind: FeedKind, cursor: Cursor?, nowSec: Long): Page<FeedItem> {
        val ffiKind = kind.toFfi()
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feed(ffiKind, cursor?.value, nowSec), platform)
            }
        }
    }

    /** 详情（视频 / 动态 / 图文 / 专栏）：**整套请求 + 解析都在 Rust**（WBI、游标、错误码）。 */
    suspend fun detail(id: String, nowSec: Long): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id, nowSec), platform)
        }
    }

    /** 一级评论。游标是上一页给的 `next_offset`（`null` = 第一页）。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, nowSec: Long): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.comments(id, cursor?.value, sort.toFfi(), nowSec), platform)
            }
        }

    /**
     * 搜一页：**请求怎么拼和结果怎么解析都在 Rust**（`feedhub-core/src/bili/search.rs`）：
     * WBI 签名、`Origin` / `Referer`、页码游标、`{code}` 外壳、`<em>` 高亮的清理。
     *
     * [pageSize] 是请求口径（插件描述的 30，不是响应实际条数）；[nowSec] 同时是 WBI 的 `wts`
     * 和"今天"（WBI key 的跨天刷新）。用户 / 话题类**发请求之前**就在 Rust 里报错。
     */
    suspend fun search(request: SearchRequest, cursor: Cursor?, pageSize: Int, nowSec: Long): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(
                    native.search(
                        request.text,
                        request.kind.toFfiBili(),
                        request.sort.toFfiBili(),
                        cursor?.value,
                        pageSize,
                        nowSec,
                    ),
                    platform,
                )
            }
        }

    /** 楼中楼。游标是下一页的页码（`null` = 第 1 页）。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?, nowSec: Long): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.replies(id, commentId, cursor?.value, nowSec), platform)
            }
        }

    /**
     * 播放地址（`/x/player/wbi/playurl`）：**WBI 签名、`fnval=4048`、`qn` 的默认值、错误码全在 Rust**。
     *
     * [quality] 是 `Quality.id`，`null` = 让 B站 给"能给的最高"（Rust 侧是 `qn=127`）。
     * 返回的 `headers`（Referer + UA）拉流时必须原样带上，否则 CDN 403。
     */
    suspend fun play(id: String, partId: String, quality: Int?, nowSec: Long): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toPlayInfo(native.play(id, partId, quality, nowSec))
        }
    }

    /**
     * 分段弹幕（`/x/v2/dm/wbi/web/seg.so`，裸 protobuf）：签名、`pid` 的 aid 换算、304 空段、
     * "风控时回 JSON"的判定、protobuf 解析都在 Rust。
     */
    suspend fun danmaku(id: String, partId: String, segment: Int, nowSec: Long): List<Danmaku> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toDanmakuList(native.danmaku(id, partId, segment, nowSec))
            }
        }

    // 账号：我是谁 / 关注列表 / 关注·取关

    /** 「我是谁」：没登录就是 `null`（不抛）。 */
    suspend fun whoami(nowSec: Long): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.whoami(nowSec)?.let { RustModel.toUser(it) }
        }
    }

    /** 关注的 UP 主，一页 50 个（游标是页码）。 */
    suspend fun followings(cursor: Cursor?, nowSec: Long): Page<FollowTarget> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toFollowPage(native.followings(cursor?.value, nowSec))
        }
    }

    /** 关注（`follow = true`）/ 取关。**唯一的写操作**，只在用户点按钮时调用。 */
    suspend fun setFollowing(target: FollowTarget, follow: Boolean, nowSec: Long) = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.setFollowing(target.toFfi(), follow, nowSec)
            Unit
        }
    }

    // 扫码登录

    /** 第一步：要一张二维码（`nowMs` 是毫秒时间戳，二维码 180 秒有效）。 */
    suspend fun startQrLogin(nowMs: Long): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toQrLogin(qrNative.start(nowMs))
        }
    }

    /** 第二步：问一次状态。 */
    suspend fun pollQrLogin(login: QrLogin, nowMs: Long): QrLoginState = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs))
        }
    }
}

/** `SearchKind` → `FfiBiliSearchKind`（B站 和 YouTube 各一份，名字带平台前缀免得撞）。 */
internal fun SearchKind.toFfiBili(): FfiBiliSearchKind = when (this) {
    SearchKind.ALL -> FfiBiliSearchKind.ALL
    SearchKind.VIDEO -> FfiBiliSearchKind.VIDEO
    SearchKind.POST -> FfiBiliSearchKind.POST
    SearchKind.USER -> FfiBiliSearchKind.USER
    SearchKind.TOPIC -> FfiBiliSearchKind.TOPIC
}

/** `SearchSort` → `FfiBiliSearchSort`（B站 三档都有）。 */
internal fun SearchSort.toFfiBili(): FfiBiliSearchSort = when (this) {
    SearchSort.RELEVANCE -> FfiBiliSearchSort.RELEVANCE
    SearchSort.LATEST -> FfiBiliSearchSort.LATEST
    SearchSort.HOT -> FfiBiliSearchSort.HOT
}

/** B站 的默认头（`BilibiliSource.headers` 那张表，**顺序就是线上顺序**）。 */
private fun biliHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Referer" to "https://www.bilibili.com/",
    "Origin" to "https://www.bilibili.com",
)

/** B站 的限速（`BilibiliSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object BiliRate {
    const val MIN_INTERVAL_MS = 500L
}
