package feedhub.rust

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.UserRef
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import uniffi.feedhub_ffi.FfiHeyboxSearchSort
import uniffi.feedhub_ffi.FfiHeyboxQr
import uniffi.feedhub_ffi.FfiHeyboxSource
import uniffi.feedhub_ffi.HostNonce

/**
 * **小黑盒平台对象的 Kotlin 门面**：`FfiHeyboxSource`（Rust 里的 `uniffi::Object`）+ 三个宿主回调。
 *
 * 和 [RustHeybox]（纯函数门面）的分工：
 * - [RustHeybox] 只做"给我一段 body、我给你一页 / 一份签名"；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数顺序 / 公共参数 / `hkey` 签名 / 请求头顺序、翻页游标、
 *   表情表的取用与缓存（`ctx.store` 的 `heybox.emotes`，和 Kotlin 那条路**共用同一份**）、状态码与错误码判定，
 *
 * 错误映射和取消都走 [HostedCall]（网络错误 / fail-closed 不包装成 `SourceException`，
 * 那是平台层和纯函数入口唯一的不同）。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 */
class RustHeyboxHosted(
    ctx: SourceContext,
    base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
    /**
     * nonce 来源：一次调用里第二个起的请求由 Rust 回头问它要新的（参照实现就是每个请求调一次）。
     * null = 不接（Rust 自己从调用方给的那个派生）。
     */
    private val nonceFor: ((Long) -> String)? = null,
) {
    private val nonceHost: HostNonce? = nonceFor?.let { f ->
        object : HostNonce {
            override fun nonce(timestamp: Long): String = f(timestamp)
        }
    }

    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiHeyboxSource by HostedCall.native {
        FfiHeyboxSource.withNet(
            base,
            net.inner,
            net.spec(
                platform = platform,
                defaultHeaders = heyboxHeaders(),
                minIntervalMs = HeyboxRate.MIN_INTERVAL_MS,
            ),
            RustStoreHost(ctx.store),
        ).also { s -> nonceHost?.let(s::setNonceSource) }
    }

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /** 扫码登录的平台对象（另一个 Rust 对象：多一个"临时 Cookie 罐"的宿主能力）。 */
    private val qrNative: FfiHeyboxQr by HostedCall.native {
        FfiHeyboxQr(
            base,
            qrHostOf(
                net = net,
                platform = platform,
                defaultHeaders = heyboxHeaders(),
                minIntervalMs = HeyboxRate.MIN_INTERVAL_MS,
            ),
        ).also { q -> nonceHost?.let(q::setNonceSource) }
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /**
     * 推荐流一页。[nowSec] 是秒级时间戳（`_time` 和表情表的"今天"都用它），[nonce] 由调用方生成
     * （`SecureRandom` 留在 Kotlin）——两个都从外面传，测试才能把两次运行的请求逐字节比。
     */
    suspend fun feed(cursor: Cursor?, nowSec: Long, nonce: String): Page<FeedItem> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toPage(native.feed(cursor?.value, nowSec, nonce), platform) }
    }

    /**
     * 详情：`link/tree` 第一页 +（正文里有游戏卡片时）`/game/get_game_infos/`，请求和正文（含 HTML）
     * 全在 Rust。返回完整的 [FeedDetail]。
     *
     * [nowSec] / [nonce] 和 [feed] 一样由调用方给；一次 [detail] 只取一次时间戳。
     */
    suspend fun detail(id: String, nowSec: Long, nonce: String): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toDetail(native.detail(id, nowSec, nonce), platform) }
    }

    /** 一级评论。游标是页码（`null` = 第 1 页）；[sort] 三档和 Kotlin 一一对应。 */
    suspend fun comments(
        id: String,
        cursor: Cursor?,
        sort: CommentSort,
        nowSec: Long,
        nonce: String,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(
                native.comments(id, cursor?.value, sort.toFfi(), nowSec, nonce),
                platform,
            )
        }
    }

    /** 楼中楼。游标是 `lastval`（`null` = 第一条）。 */
    suspend fun replies(
        id: String,
        commentId: String,
        cursor: Cursor?,
        nowSec: Long,
        nonce: String,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(
                native.replies(id, commentId, cursor?.value, nowSec, nonce),
                platform,
            )
        }
    }

    /**
     * 播放地址（`partId=main` 或正文里夹着的 `seg:<n>`）：**请求和解析全在 Rust**
     * （`feedhub-core/src/heybox/play.rs`）。60 秒内 [detail] 取过同一条帖子时直接复用那份
     * `link/tree` 响应（**不再发请求**），没有才现取一次。
     *
     * `partId` 的写法不认识时 Rust 在发请求之前就报错（和 Kotlin 一样：一个请求都不发）。
     */
    suspend fun play(id: String, partId: String, nowSec: Long, nonce: String): PlayInfo =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPlayInfo(native.play(id, partId, nowSec, nonce))
            }
        }

    /**
     * 详情之后 60 秒内的同一条帖子原始响应（`link/tree` 的 JSON）。**播放不再需要它**
     * ，留着给测试 / 排障确认两边看到的是同一份 body。
     * **不经过 [HostedCall.blocking]**：这是一次纯内存查询。
     */
    fun recentLink(id: String, nowSec: Long): String? = native.recentLink(id, nowSec)

    /**
     * 搜一页（`/bbs/app/api/general/search/v1`）：**请求怎么拼和结果怎么解析都在 Rust**
     * （`feedhub-core/src/heybox/search.rs`），这里只做排序枚举的映射和错误 / 取消。
     *
     * [sort] 只有相关 / 最新两档（和插件描述 `capabilities.search.sorts` 一致）；最热当前**没做**，
     * 传进来直接报错、**不静默退化成相关**（仓库的规矩）。[nowSec] / [nonce] 和 [feed] 一样从外面注入，
     * 一次 [search] 只用同一个时间戳。
     */
    suspend fun search(
        text: String,
        sort: SearchSort,
        cursor: Cursor?,
        nowSec: Long,
        nonce: String,
    ): Page<FeedItem> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        val ffiSort = when (sort) {
            SearchSort.RELEVANCE -> FfiHeyboxSearchSort.RELEVANCE
            SearchSort.LATEST -> FfiHeyboxSearchSort.LATEST
            SearchSort.HOT -> throw ApiException(platform, "bad_sort", "小黑盒搜索没有「最热」排序（能力里没声明）")
        }
        HostedCall.guard(platform) {
            RustModel.toPage(native.search(text, ffiSort, cursor?.value, nowSec, nonce), platform)
        }
    }
    /** 「我是谁」：没登录就是 `null`（不抛）。 */
    suspend fun whoami(nowSec: Long, nonce: String): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.whoami(nowSec, nonce)?.let { RustModel.toUser(it) } }
    }

    /**
     * 手上已经有的表情表（内存 → `ctx.store` 里 7 天内那份）。**不发请求、不改状态。**
     *
     * 详情 / 评论 / 搜索都由 Rust 自己按表切 Span，
     * 这里只留给"宿主想知道现在手上有几张"的场合（排障 / 测试）。
     */
    suspend fun cachedEmotes(nowSec: Long): Map<String, String> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.cachedEmotes(nowSec).associate { it.code to it.url } }
    }

    /** 下一趟取表会不会真的发请求（纯查询）。 */
    suspend fun emoteFetchNeeded(nowSec: Long): Boolean = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.emoteFetchNeeded(nowSec) }
    }

    /** 后台把表情表补上（同一时间只许一个在飞）。返回表里有几项。 */
    suspend fun ensureEmotes(nowSec: Long, nonce: String): Int = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.ensureEmotes(nowSec, nonce).toInt() }
    }

    /** 扫码登录第一步：要一张二维码。 */
    suspend fun startQrLogin(nowMs: Long, nowSec: Long, nonce: String): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toQrLogin(qrNative.start(nowMs, nowSec, nonce)) }
    }

    /** 扫码登录第二步：问一次状态。 */
    suspend fun pollQrLogin(login: QrLogin, nowMs: Long, nowSec: Long, nonce: String): QrLoginState =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs, nowSec, nonce))
            }
        }
}

/** 小黑盒的默认头（`HeyboxSource` 那张表，**顺序就是线上顺序**）。 */
private fun heyboxHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Origin" to "https://www.xiaoheihe.cn",
    "Referer" to "https://www.xiaoheihe.cn/",
    "Accept" to "application/json, text/plain, */*",
)

/** 小黑盒的限速（`HeyboxSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object HeyboxRate {
    const val MIN_INTERVAL_MS = 600L
}
