package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import okhttp3.HttpUrl.Companion.toHttpUrl
import uniffi.feedhub_ffi.FfiEmote
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.UserRef
import uniffi.feedhub_ffi.FfiZhihuFeedKind
import uniffi.feedhub_ffi.FfiZhihuQr
import uniffi.feedhub_ffi.FfiZhihuSearchKind
import uniffi.feedhub_ffi.FfiZhihuSearchSort
import uniffi.feedhub_ffi.FfiZhihuSource

/**
 * **知乎平台对象的 Kotlin 门面**：`FfiZhihuSource`（Rust 里的 `uniffi::Object`）+ 两个宿主回调。
 *
 * 和 [RustZhihu]（纯函数门面）的分工：
 * - [RustZhihu] 只做"给我一段 body、我给你一页"；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数 / 头、`x-zse-96` 签名、没 `d_c0` 时先 `POST /udid`、
 *   翻页游标、状态码 / `Location` / 错误码判定，真正的收发回调宿主用 [client] 做
 *   （必须是 `ctx.client(platform)` 派生出来的那个客户端）。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；`feed()` 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 *
 * 和 `RustBiliHosted` 的差别：没有 store 回调（知乎不缓存设备参数），表情表构造时给一次
 * （它是插件的静态表，不像 B站 的 WBI key 会变）；每次请求多一个 `seed`（`x-zse-96` 的随机首字节）。
 */
/** lens（视频地址接口）的线上根，和 `ZhihuSource.LENS` 一字不差。 */
const val LENS_DEFAULT: String = "https://lens.zhihu.com"

class RustZhihuHosted(
    ctx: SourceContext,
    base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
    emotes: Map<String, String>,
    /** 扫码登录复制设备 Cookie 用的域（`HttpUrl.topPrivateDomain()`，见 `ZhihuSource.cookieDomain`）。 */
    private val cookieDomain: String = "zhihu.com",
    /**
     * lens 那份**匿名**客户端（不带 Cookie）**在 Kotlin 那边存的有没有**。
     *
     * `null` = 不给
     * （那时 Rust 的 `play` 报内部错误、一个请求都不发，比"悄悄带上 Cookie"好）。
     */
    lensClient: Any? = null,
    /** lens 的根（线上 `https://lens.zhihu.com`；测试指向 MockWebServer，和 `ZhihuSource(lensBase = …)` 对应）。 */
    private val lensBase: String = LENS_DEFAULT,
    /** 信息流卡片走问题版面；生产打开，对拍参照测试不开。 */
    private val questionCards: Boolean = false,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，
     * 见 [RustNative]），而且失败时要走 [HostedCall.guard] 的统一报错。
     */
    private val native: FfiZhihuSource by HostedCall.native {
        FfiZhihuSource.withNet(
            base,
            net.inner,
            net.spec(
                platform = platform,
                defaultHeaders = zhihuHeaders(),
                // 线上是 `www.zhihu.com`（前两个条件）；第三个条件让**测试基址**也吃同一批默认头。
                defaultHeaderHosts = listOf("zhihu.com", baseHost),
                minIntervalMs = ZhihuRate.MIN_INTERVAL_MS,
                // **不跟重定向**：`ZhihuSource` 的客户端是 `followRedirects(false)` ——
                // 知乎用 302 跳 `unhuman` 表示风控，跟过去就变成"下载了一个 HTML 页面"，
                // 风控判定就丢了（`RustZhihuSourceTest` 的"302 意外重定向"门抓的）。
                followRedirects = false,
            ),
            emotes.map { (code, url) -> FfiEmote(code, url) },
            // lens 是**匿名端口**：不带 Cookie、只补 UA、600ms 限速（见 `ZhihuSource.lensHttp`）
            lensClient?.let {
                net.spec(
                    platform = platform,
                    defaultHeaders = linkedMapOf("User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME),
                    defaultHeaderHosts = listOf("zhihu.com", lensHost),
                    minIntervalMs = ZhihuRate.MIN_INTERVAL_MS,
                    followRedirects = false,
                )
            },
            lensBase,
        ).also { if (questionCards) it.setQuestionCards(true) }
    }

    /** 扫码登录的平台对象（另一个 Rust 对象：多一个"临时 Cookie 罐"的宿主能力）。 */
    private val qrNative: FfiZhihuQr by HostedCall.native {
        FfiZhihuQr(
            base,
            cookieDomain,
            qrHostOf(
                net = net,
                platform = platform,
                defaultHeaders = zhihuHeaders(),
                defaultHeaderHosts = listOf("zhihu.com", baseHost),
                minIntervalMs = ZhihuRate.MIN_INTERVAL_MS,
                // 知乎用 302 跳 `unhuman` 表示风控，扫码那条路也**不跟重定向**
                followRedirects = false,
            ),
        )
    }

    /** 测试基址的 host（`MockWebServer` 的 localhost）也要吃同一批默认头（照 Kotlin 的 `hostFilter`）。 */
    private val baseHost: String = runCatching { base.toHttpUrl().host }.getOrDefault("")

    /** lens 的 host（同理）。 */
    private val lensHost: String = runCatching { lensBase.toHttpUrl().host }.getOrDefault("")

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 一页。[seed] 是 `x-zse-96` 的明文首字节（0..255），由调用方生成——测试里固定它，
     * 两次运行的请求才能逐字节比。
     */
    suspend fun feed(kind: FeedKind, cursor: Cursor?, seed: Int): Page<FeedItem> {
        val ffiKind = when (kind) {
            FeedKind.RECOMMEND -> FfiZhihuFeedKind.RECOMMEND
            FeedKind.FOLLOWING -> FfiZhihuFeedKind.FOLLOWING
        }
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feed(ffiKind, cursor?.value, seed), platform)
            }
        }
    }

    /**
     * 详情。`id` 是 `answer:123` 那种带类型的 id（拆不开时 Rust 抛 `ApiException("bad_id")`，
     * 和 Kotlin 版同一个码、同一句文案）；`seed` 和 [feed] 一样每次新给。
     */
    suspend fun detail(id: String, seed: Int): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id, seed), platform)
        }
    }

    /**
     * 搜一页：**请求怎么拼和结果怎么解析都在 Rust**（`feedhub-core/src/zhihu/search.rs`）：
     * `x-zse-96` 签名、第一页的固定参数 / 后面几页把 `paging.next` 的 query 原样接上、
     * `is_end` / 没有 `next` 就是最后一页、`<em>` 高亮清理、`search_result` 之外的卡片丢掉。
     *
     * [pageSize] 是请求口径（20）。排序**能力里只有"相关"**：`sort=upvoted_count` 在线被服务端忽略，
     * 所以聚合层不会传别的；Rust 那边的映射照 Kotlin 抄（`created_time` / `upvoted_count`）。
     */
    suspend fun search(request: SearchRequest, cursor: Cursor?, pageSize: Int, seed: Int): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(
                    native.search(
                        request.text,
                        request.kind.toFfiZhihu(),
                        request.sort.toFfiZhihu(),
                        cursor?.value,
                        pageSize,
                        seed,
                    ),
                    platform,
                )
            }
        }

    /** 一级评论（`order_by` 的映射在 Rust：`NEWEST` → `ts`，别的 → `score`）。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, seed: Int): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.comments(id, cursor?.value, sort.toFfi(), seed), platform)
            }
        }

    /**
     * **播放地址**（`GET {lens}/api/v4/videos/{video_id}`）：请求 + 解析全在 Rust
     * （`feedhub-core/src/zhihu/play.rs` + `ZhihuHostedSource::play`）。
     *
     * 匿名接口：Rust 用一份**不带 Cookie 的匿名 host**发它——不签名、不需要 Referer。
     * [partId] 是 lens 视频 id；写法不对时 Rust 在发请求之前就报 `bad_id`。
     * 地址带签名（约一小时过期），**每次都现取**，不复用详情里的。
     */
    suspend fun play(partId: String): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toPlayInfo(native.play(partId)) }
    }

    /** 楼中楼：`commentId` 是一级评论的 id。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?, seed: Int): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.replies(commentId, cursor?.value, seed), platform)
            }
        }

    /** 详情的问题版面：回答 / 问题显示问题，别的类型同 [detail]。 */
    suspend fun qaDetail(id: String, seed: Int): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.qaDetail(id, seed), platform)
        }
    }

    /** 评论区放回答（推过来的那条第一页置顶）；别的类型同 [comments]。 */
    suspend fun qaAnswers(id: String, cursor: Cursor?, sort: CommentSort, seed: Int): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.qaAnswers(id, cursor?.value, sort.toFfi(), seed), platform)
            }
        }

    /** 楼中楼：`answer:<id>` 那一楼是这条回答的评论；别的同 [replies]。 */
    suspend fun qaReplies(commentId: String, cursor: Cursor?, seed: Int): Page<Comment> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toCommentPage(native.qaReplies(commentId, cursor?.value, seed), platform)
            }
        }

    /** 「我是谁」：没登录就是 `null`（不抛，也不发请求）。 */
    suspend fun whoami(seed: Int): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.whoami(seed)?.let { RustModel.toUser(it) } }
    }

    /** 扫码登录第一步：要一张二维码。 */
    suspend fun startQrLogin(nowMs: Long, seed: Int): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toQrLogin(qrNative.start(nowMs, seed)) }
    }

    /** 扫码登录第二步：问一次状态。 */
    suspend fun pollQrLogin(login: QrLogin, nowMs: Long, seed: Int): QrLoginState = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs, seed)) }
    }
}

/** `SearchKind` → `FfiZhihuSearchKind`。 */
internal fun SearchKind.toFfiZhihu(): FfiZhihuSearchKind = when (this) {
    SearchKind.ALL -> FfiZhihuSearchKind.ALL
    SearchKind.VIDEO -> FfiZhihuSearchKind.VIDEO
    SearchKind.POST -> FfiZhihuSearchKind.POST
    SearchKind.USER -> FfiZhihuSearchKind.USER
    SearchKind.TOPIC -> FfiZhihuSearchKind.TOPIC
}

/** `SearchSort` → `FfiZhihuSearchSort`（知乎能力里只有 `RELEVANCE`，映射本身照 Kotlin 抄）。 */
internal fun SearchSort.toFfiZhihu(): FfiZhihuSearchSort = when (this) {
    SearchSort.RELEVANCE -> FfiZhihuSearchSort.RELEVANCE
    SearchSort.LATEST -> FfiZhihuSearchSort.LATEST
    SearchSort.HOT -> FfiZhihuSearchSort.HOT
}

/** 知乎的默认头（`ZhihuSource` 那张表，**顺序就是线上顺序**）。 */
private fun zhihuHeaders(): Map<String, String> = linkedMapOf(
    "User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME,
    "Referer" to "https://www.zhihu.com/",
    "Accept" to "*/*",
    "Accept-Language" to "zh-CN,zh;q=0.9",
    "x-api-version" to "3.0.91",
    "x-app-za" to "OS=Web",
)

/** 知乎的限速（`ZhihuSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object ZhihuRate {
    const val MIN_INTERVAL_MS = 600L
}
