package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.SourceContext
import feedhub.core.UserRef
import uniffi.feedhub_ffi.FfiPage
import uniffi.feedhub_ffi.FfiXhsHostedComments
import uniffi.feedhub_ffi.FfiXhsHostedDetail
import uniffi.feedhub_ffi.FfiXhsAuthor
import uniffi.feedhub_ffi.FfiXhsSignRandom
import uniffi.feedhub_ffi.FfiXhsSignRequest
import uniffi.feedhub_ffi.FfiXhsSignState
import uniffi.feedhub_ffi.FfiXhsQr
import uniffi.feedhub_ffi.FfiXhsSource
import uniffi.feedhub_ffi.FfiXhsFollowAuthor
import uniffi.feedhub_ffi.FfiXhsFollowPage
import uniffi.feedhub_ffi.HostXhsClock
import kotlinx.coroutines.runBlocking
import uniffi.feedhub_ffi.HostXhsSelfId
import uniffi.feedhub_ffi.HostXhsSignRandom

/**
 * **小红书平台对象的 Kotlin 门面**：`FfiXhsSource`（Rust 里的 `uniffi::Object`）+ 三个宿主回调。
 *
 * 和 [RustXhs]（纯函数门面）的分工：
 * - [RustXhs] 只做"给我一段 body / 一组随机量、我给你一页 / 一个签名"；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数顺序 / body / 请求头顺序、签名（X-s / X-t / X-S-Common /
 *   两个 traceid / x-rap-param / xy-direction）、b1 的生成与 store 缓存、翻页游标、状态码与业务码判定、解析，
 *   真正的收发回调宿主用 [client] 做（必须是 `XhsApi.http` 那一个客户端——限速 / 默认头兜底 / Cookie 罐 /
 *   代理 / fail-closed 全在它上面）。
 *
 * **随机数 / 时间不在 Rust 里取**：[rand] 是调用方按 `XhsSigner.sign` 的取用顺序从自己的 `Random` 里抽好的；
 * `x-rap-param` 的补位长度用 [homefeedPadLen] / [postedPadLen] 先问再取（纯计算，不占状态）。
 *
 * **签名计数在 Rust 那边（一个 Source 一份）**：四条自建请求和 [sign]（详情 / 评论 / 扫码 / 表情表这些
 * 还没搬的请求）共用它，所以一次会话里的 `seq` / `signCount` / `xraySeq` 是一条序列。
 *
 * **线程模型**：网络调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来），所以 `.so` 里
 * 没有 tokio、没有后台线程；`homefeed()` 返回时一切都结束了。协程被取消时 [HostCallScope] 会把在飞的
 * OkHttp Call 掐掉（见 [RustHttpHost]）。[sign] / [homefeedPadLen] / [postedPadLen] 是同步的纯计算
 * （被 OkHttp 拦截器在 IO 线程上直接调）。
 */
class RustXhsHosted(
    private val ctx: SourceContext,
    private val base: String,
    private val profileJson: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    private val platform: Platform,
    /**
     * 限速（`XhsApi.MIN_INTERVAL_MS` / `JITTER_MS`）。
     *
     * **必须从外面传**：`RustXhsSource` 的构造函数允许测试传 0（等价门要发几百个请求，真睡 1.5 秒
     * 会撞上 `runTest` 的 60 秒超时）。写死成常量的话，测试传的 0 只作用于 Kotlin 参照实现那条路，
     * 两条路的节奏就不一样了。
     */
    private val minIntervalMs: Long = XhsRate.MIN_INTERVAL_MS,
    private val jitterMs: Long = XhsRate.JITTER_MS,
    initialState: RustXhs.SignState,
    /**
     * 关注流的**签名随机源**（`HostXhsSignRandom` 的实现，见 `XhsHosted.kt` 的 `XhsSignRandomDrawer`）。
     *
     * 为什么非关注流也要给：UniFFI 的 `with_foreign` 参数没有默认值，构造时必须给一个；
     * 其它流（推荐 / 搜索 / 详情 / 评论 / 播放 / 表情表）的随机量仍然是"调用方抽好、按参数传"，
     * **一个字节都不走这个回调**。
     */
    private val signRandom: HostXhsSignRandom = XhsHostedSignRandom(RustXhs.SignRandom(0, "", "", 0, 0, "", "", "", "", 0)),
    /**
     * 关注流的**时钟**（`HostXhsClock`）。Kotlin 一次刷新里调很多次 `now()`，对拍要注入同一个假钟。
     *
     * 默认就是 `System.currentTimeMillis()`；别的流不用它（时间还是按参数传）。
     */
    private val clock: HostXhsClock = XhsHostedClock(System::currentTimeMillis),
    /**
     * 关注流的 "我自己是谁"（`HostXhsSelfId`）。`null` = 还没接（这时 Rust 回落到 `ctx.store` 的
     * `xhs.user_id`，都没有就是未登录）。
     */
    selfId: (suspend () -> String?)? = null,
) {
    /**
     * 真正交给 Rust 的那个回调：Rust 的宿主回调是**同步**的，而这个"我是谁"是挂起的
     * （没存过 `user_id` 要现发一次 `user/me`）。所以在这里 `runBlocking` 一次，并且**挂在
     * 外层那个 Job 下**（[HostCallScope.job]）——外层协程被取消时它跟着取消、在飞的 OkHttp Call 也会被掐掉。
     */
    private val selfIdHost: HostXhsSelfId = XhsHostedSelfId { selfId?.let { ask -> blockingSelfId(ask) } }

    private fun blockingSelfId(ask: suspend () -> String?): String? {
        val job = HostCallScope.current?.job
        return if (job == null) runBlocking { ask() } else runBlocking(job) { ask() }
    }
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiXhsSource by HostedCall.native {
        FfiXhsSource.withNet(
            base,
            profileJson,
            FfiXhsSignState(
                initialState.seq,
                initialState.signCount,
                initialState.dsllt,
                initialState.xraySeq,
            ),
            net.inner,
            net.spec(
                platform = platform,
                minIntervalMs = minIntervalMs,
                jitterMs = jitterMs,
            ),
            RustStoreHost(ctx.store),
            signRandom,
            clock,
            selfIdHost,
        )
    }

    /**
     * 扫码登录的平台对象（**另一个** Rust 对象，和 [native] **共用**签名计数 / profile / `ctx.store`）：
     * 多一个"临时 Cookie 罐"的宿主能力。懒建，和 [native] 一样。
     */
    private val qrNative: FfiXhsQr by HostedCall.native {
        native.qrObject(
            qrHostOf(
                net = net,
                platform = platform,
                minIntervalMs = minIntervalMs,
                jitterMs = jitterMs,
            ),
        )
    }

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /** 推荐流一页 + 这一页的标题拼串（Kotlin 用它决定要不要去取表情表，和改造前一样）。 */
    class Homefeed(val page: Page<FeedItem>, val titles: String)

    /** `user_posted` 的一页；`page` 里的条目顺序和 [sticky] 一一对应。 */
    class Posted(
        val page: Page<FeedItem>,
        val sticky: List<Boolean>,
        val hasMore: Boolean,
        val titles: String,
    )

    /** `followings/self` 的一页。 */
    class Followings(
        val users: List<RustXhs.Author>,
        val cursor: String?,
        val hasMore: Boolean,
        val total: Int?,
    )

    /** 搜索的一页：条目 + 游标 + 标题拼串（Kotlin 拿它判断要不要去取表情表，和推荐流一样）。 */
    class Search(val page: Page<FeedItem>, val titles: String)

    /**
     * 搜索一页（Rust 拼请求 + 发 + 判错 + 解析）。
     *
     * [cursor] 是 `<search_id>|<下一页页码>`，`null` = 新一轮搜索：Rust 按 `(nowMs, randomPart)` 现造
     * `search_id`、从第 1 页开始。[randomPart] 是 `1..=2147483646` 的随机数（Kotlin 取），翻页时可以给 0。
     * [searchPadLen] 要用**同一对** `(nowMs, randomPart)`，否则 `x-rap-param` 的补位长度和 body 对不上。
     */
    suspend fun search(
        keyword: String,
        cursor: Cursor?,
        nowMs: Long,
        randomPart: Long,
        rand: RustXhs.SignRandom,
    ): Search = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi = native.search(keyword, cursor?.value, nowMs, randomPart, rand.toFfi())
            Search(RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, ffi.next), platform), ffi.titles)
        }
    }

    /** 这一次搜索的 `x-rap-param` 要取几个补位字符（纯计算：不占状态、不发请求）。 */
    fun searchPadLen(
        keyword: String,
        cursor: Cursor?,
        nowMs: Long,
        randomPart: Long,
        rapMask: Int,
        sessionKey: String,
    ): Int = HostedCall.guard(platform) {
        native.searchPadLen(keyword, cursor?.value, nowMs, randomPart, rapMask, sessionKey).toInt()
    }

    /** 推荐流一页（Rust 拼请求 + 发 + 判错 + 解析）。 */
    suspend fun homefeed(cursor: Cursor?, nowMs: Long, rand: RustXhs.SignRandom): Homefeed = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi = native.homefeed(cursor?.value, nowMs, rand.toFfi())
            Homefeed(RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, ffi.next), platform), ffi.titles)
        }
    }

    /** 关注列表一页。[cursor] 是上一页响应的游标，第一页空串。 */
    suspend fun followings(
        selfId: String,
        cursor: String,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Followings = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi = native.followings(selfId, cursor, nowMs, rand.toFfi())
            Followings(
                ffi.users.map { RustXhs.Author(it.id, it.name, it.avatar, it.xsecToken) },
                ffi.cursor,
                ffi.hasMore,
                ffi.total,
            )
        }
    }

    /** 谁有新笔记（`followings/extra`）：用户 id → 未看数（响应里没有的人由调用方当 0）。 */
    suspend fun unread(
        selfId: String,
        userIds: List<String>,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Map<String, Int> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val out = LinkedHashMap<String, Int>(userIds.size)
            for (item in native.unread(selfId, userIds, nowMs, rand.toFfi())) out[item.userId] = item.count
            out
        }
    }

    /** 一个作者的主页第一页（`user_posted`）。 */
    suspend fun posted(
        author: RustXhs.Author,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Posted = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi = native.posted(author.toFfi(), nowMs, rand.toFfi())
            Posted(
                RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, null), platform),
                ffi.sticky,
                ffi.hasMore,
                ffi.titles,
            )
        }
    }

    /**
     * 这一次推荐流的 `x-rap-param` 要取几个补位字符（纯计算：不占状态、不发请求、不锁签名计数）。
     * Kotlin 那边要先问长度、再照这个数从自己的 `Random` 里取补位字符（和 `XhsSigner` 的取用顺序对齐）。
     */
    fun homefeedPadLen(cursor: Cursor?, nowMs: Long, rapMask: Int, sessionKey: String): Int =
        HostedCall.guard(platform) { native.homefeedPadLen(cursor?.value, nowMs, rapMask, sessionKey).toInt() }

    /** 这一次 `user_posted` 的 `x-rap-param` 要取几个补位字符（纯计算）。 */
    fun postedPadLen(author: RustXhs.Author, nowMs: Long, rapMask: Int, sessionKey: String): Int =
        HostedCall.guard(platform) { native.postedPadLen(author.toFfi(), nowMs, rapMask, sessionKey).toInt() }

    // ---------------- 关注流 ----------------

    /**
     * **关注流一页**：调度（关注列表 TTL / 谁该重拉 / 2 秒预算 / 关 6 小时 / 存盘）和三个接口的
     * 请求 + 签名 + 判错 + 解析 + 归并**全在 Rust 平台对象里**（`xhs/follow_feed.rs`）。
     *
     * [cursor] 是上一页最后一条的 `<发布时间秒>:<笔记 id>`；`null` = 刷新（会联网），非空 = **只在缓存里
     * 继续往下排、一个请求都不发**。
     *
     * [selfId] 是 Kotlin 算好的自己的 user_id（`whoami` 没搬：`xhs.user_id` 有就用，没有就现问一次 `user/me`）。
     *
     * 随机量 / 时钟走构造时挂上的两个回调：**一次刷新要发 N 个请求**，每个请求的签名随机量都不一样，
     * 而且补位长度取决于那个请求的 body——只能在"要发请求的那一刻"问 Kotlin 抽
     * （顺序和 Kotlin 参照实现的 `XhsSignRandomDrawer` 逐位一致）。
     */
    suspend fun followPage(cursor: Cursor?): FollowPage = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiXhsFollowPage = native.followPage(cursor?.value)
            FollowPage(
                RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, ffi.next), platform),
                ffi.refreshed,
            )
        }
    }

    /** 关注流的一页 + 这次刷新有没有真的联网（测试 / 排障看这个）。 */
    class FollowPage(val page: Page<FeedItem>, val refreshed: Boolean)

    /** 关注流**手上那份缓存**（排障 / 测试用，不发请求、不读盘）。 */
    suspend fun followCached(): List<FollowAuthor> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            native.followCached().map {
                FollowAuthor(
                    it.id,
                    it.name,
                    it.xsecToken,
                    it.items.toInt(),
                    it.fetchedAt,
                    it.unread,
                    it.unreadAtFetch,
                    it.closedUntil,
                )
            }
        }
    }

    /** 关注流缓存里的一个作者（摘要；[items] 是条数）。 */
    class FollowAuthor(
        val id: String,
        val name: String,
        val xsecToken: String?,
        val items: Int,
        val fetchedAt: Long,
        val unread: Int,
        val unreadAtFetch: Int,
        val closedUntil: Long,
    )

    /** 关注流的 `extraAt`（上次问 `followings/extra` 的时间；没读过就是 null）。 */
    suspend fun followExtraAt(): Long? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.followExtraAt() }
    }

    // ---------------- 播放 ----------------

    /**
     * Rust 手上有没有这条笔记 5 分钟内取过的详情（纯查询，不发请求、不签名）。
     *
     * 调用方在 [play] 之前先问它：命中时不会发请求，也就**不该**从 `Random` 里抽随机量
     * （签名的取用顺序要和 Kotlin 参照实现逐位对齐）。
     */
    fun playCached(id: String, nowMs: Long): Boolean = HostedCall.guard(platform) { native.playCached(id, nowMs) }

    /**
     * 播放地址：`partId` 只认 `main`（不对时 Rust 一个请求都不发）；先复用**详情刚取过**的那份
     * （5 分钟内、最多记 8 条），没有才用 `INTERACTIVE` 优先级现取一次详情。
     *
     * 小红书没有按清晰度挑流的参数（[quality] 收下但一个字节都不影响），也没有弹幕。
     * 返回的 `headers` 里只有一条 `Referer`（CDN 其实不要它，和网页一致）。
     */
    suspend fun play(
        id: String,
        partId: String,
        quality: Int?,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toPlayInfo(native.play(id, partId, quality, nowMs, rand.toFfi()))
        }
    }

    // ---------------- 详情 / 评论 / 楼中楼 ----------------

    /**
     * 笔记详情（Rust 拼请求 + 发 + 判错 + 解析）。
     *
     * [id] 是卡片 `detail` 里的 `<笔记 id>@<xsec_source>@<xsec_token>`；缺 `xsec_token` 时 Rust
     * **一个请求都不发**、直接抛 `ApiException("xsec_token")`（和 Kotlin 版同一句文案）。
     *
     * 表情表**住在 Rust 平台对象里**：详情直接用那份、**不为表发任何请求**；
     * 表不在手上时正文按原文显示，`hasEmotes` 为 true 由调用方后台补一次（不挡首屏）。
     *
     * 这是"用户正等着的请求"：Rust 在请求头里挂一个内部伪请求头，[RustHttpHost] 发出去之前删掉、
     * 转成 OkHttp 的 `RequestPriority.INTERACTIVE` tag（限速器让它插队）。
     */
    suspend fun detail(
        id: String,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Detail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiXhsHostedDetail = native.detail(id, nowMs, rand.toFfi())
            Detail(RustModel.toDetail(ffi.detail, platform), ffi.hasEmotes, ffi.body)
        }
    }

    /**
     * 详情 + "正文 / 标题里有没有表情记号"。
     *
     * [hasEmotes] 为 true 而 [RustXhsHosted.detail] 收到的是空表时，调用方照旧在**后台**补取表情表
     * （不挡首屏）——这个布尔就是 Kotlin `emotesForBody` 里那次 `EMOTE_TOKEN.containsMatchIn` 的替身：
     * 表在 Rust 解析完才拿得到正文，所以由 Rust 顺手算好带回来，否则 Kotlin 只能"表不在手上就先取"。
     */
    class Detail(
        val detail: FeedDetail,
        val hasEmotes: Boolean,
        /**
         * **原始响应 body**。视频笔记要拿它回填 `XhsSource.recentNotes`：点开之后再点播放时
         * 直接复用这一份 `POST feed`（5 分钟内），不再多发一个详情请求——小红书风控最看详情次数。
         */
        val body: String,
    )

    /** 一页评论 / 楼中楼 + 同一个"有没有表情记号"的标志（口径见 [Detail.hasEmotes]）。 */
    class Comments(val page: Page<Comment>, val hasEmotes: Boolean)

    /** 这一次详情请求的 `x-rap-param` 要取几个补位字符（纯计算：不占状态、不发请求）。 */
    fun detailPadLen(id: String, nowMs: Long, rapMask: Int, sessionKey: String): Int =
        HostedCall.guard(platform) { native.detailPadLen(id, nowMs, rapMask, sessionKey).toInt() }

    /**
     * 一页一级评论。[sort] 收下但**不影响请求**（小红书的一级评论没有排序参数，和 Kotlin 版一样）。
     */
    suspend fun comments(
        id: String,
        cursor: Cursor?,
        sort: CommentSort,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Comments = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiXhsHostedComments =
                native.comments(id, cursor?.value, sort.toFfi(), nowMs, rand.toFfi())
            Comments(RustModel.toCommentPage(ffi.page, platform), ffi.hasEmotes)
        }
    }

    /** 一页楼中楼。[commentId] 是一级评论的 id，游标同上。 */
    suspend fun replies(
        id: String,
        commentId: String,
        cursor: Cursor?,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Comments = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val ffi: FfiXhsHostedComments =
                native.replies(id, commentId, cursor?.value, nowMs, rand.toFfi())
            Comments(RustModel.toCommentPage(ffi.page, platform), ffi.hasEmotes)
        }
    }

    // ---------------- 表情表 ----------------

    /**
     * 下一趟 [ensureEmotes] 会不会真的发请求（纯查询：不取表、不发请求）。
     *
     * 调用方先问这一句：**不发请求的那一趟不能从 `Random` 里抽随机量**（签名的取用顺序要和
     * Kotlin 参照实现逐位对齐）。
     */
    fun emoteFetchNeeded(nowMs: Long): Boolean = HostedCall.guard(platform) { native.emoteFetchNeeded(nowMs) }

    /** 手上已经有的表（内存 → `ctx.store` 里一周内那份）。**不发请求**；没有就是空表。 */
    fun cachedEmotes(nowMs: Long): Map<String, String> = HostedCall.guard(platform) {
        val out = LinkedHashMap<String, String>()
        for (e in native.cachedEmotes(nowMs)) out[e.code] = e.url
        out
    }

    /**
     * 信息流 / 搜索 / 关注流用：`text` 里真有表情记号才去取表（可能要发一次请求），
     * 返回这段文字里**真出现过**的那几项。
     */
    suspend fun emotesFor(
        text: String,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): Map<String, String> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val out = LinkedHashMap<String, String>()
            for (e in native.emotesFor(text, nowMs, rand.toFfi())) out[e.code] = e.url
            out
        }
    }

    /** 后台补一次表情表；返回表里有多少条（0 = 这次没取到）。风控会照常抛出（调用方记下重试时间）。 */
    suspend fun ensureEmotes(nowMs: Long, rand: RustXhs.SignRandom): Int = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.ensureEmotes(nowMs, rand.toFfi()).toInt() }
    }

    /** 记下"这个时刻之前别再试表情表"（碰到风控时调，和 Kotlin 参照实现的 `warmEmotes` 一样）。 */
    fun deferEmotes(untilMs: Long) {
        HostedCall.guard(platform) { native.deferEmotes(untilMs) }
    }

    /**
     * 给 Kotlin 那边**还没搬**的请求签名（详情 / 评论 / 楼中楼 / 扫码 / 表情表）。
     *
     * 同步（不 suspend）：调用方是 `XhsApi.SignInterceptor`，本来就在 IO 线程上，签名是纯计算、不碰网络。
     * 参数（a1 / loadts / webBuild / b1 / userId）由调用方从那一次的 Cookie 罐 / store 里读好——扫码登录用的是
     * 临时罐，不能这里读全局那份。返回的头**保持顺序**。
     */
    fun sign(
        request: RustXhs.SignRequest,
        nowMs: Long,
        rand: RustXhs.SignRandom,
    ): LinkedHashMap<String, String> = HostedCall.guard(platform) {
        val out = LinkedHashMap<String, String>()
        for (header in native.sign(request.toFfi(), nowMs, rand.toFfi())) out[header.name] = header.value
        out
    }

    // ---------------- 账号层：我是谁 / 扫码登录 ----------------

    /**
     * 「我是谁」：`GET /api/sns/web/v2/user/me`，**请求 + 签名 + 解析都在 Rust**。
     *
     * 没登录（没有 `a1` + `web_session`）时**一个请求都不发**、直接 `null`；401 / `-100` / `-101` / 游客态
     * 也是 `null`；风控 / 签名 / 网络照常抛（和 Kotlin 原版只吞 `NotLoggedInException` 一样）。
     */
    /**
     * 登录态查询（`a1` + `web_session` 都在）：纯查询，**不发请求、不占签名计数**。
     *
     * 调用方先问它，再决定要不要抽签名随机量（没登录时 `whoami` 一个请求都不发，抽了就对不齐随机流）。
     */
    fun hasLogin(): Boolean = HostedCall.guard(platform) { native.hasLogin() }

    suspend fun whoami(nowMs: Long, rand: RustXhs.SignRandom): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { native.whoami(nowMs, rand.toFfi())?.let { RustModel.toUser(it) } }
    }

    /**
     * 扫码登录第一步：要一张二维码（`POST /api/sns/web/v1/login/qrcode/create`）。
     *
     * [rands] 是**这一次调用要用的随机量**（每签一个请求一组；`start` 只签 1 次，多给的不用）。
     * 由调用方按 `XhsSigner.sign` 的取用顺序取好（`xy = false`、`rap = false`：
     * 这几个路径都不带 `x-rap-param` / `xy-direction`，所以不用先问补位长度）。
     * **没有 `a1` 设备 Cookie 时一个请求都不发**，直接抛 `ApiException("a1")`。
     */
    suspend fun startQrLogin(nowMs: Long, rands: List<RustXhs.SignRandom>): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) { RustModel.toQrLogin(qrNative.start(nowMs, rands.map { it.toFfi() })) }
    }

    /**
     * 扫码登录第二步：问一次状态（`POST /api/qrcode/userinfo`，确认后还有 `qrcode/status` + `user/me`）。
     *
     * 成功时 Cookie 已经由 Rust 落地到 `ctx.cookies`（先删旧登录 Cookie、再写新的）；
     * **成功之前一格都不动**（临时罐在宿主，和 `ctx.cookies` 完全隔离）。
     */
    suspend fun pollQrLogin(
        login: QrLogin,
        nowMs: Long,
        rands: List<RustXhs.SignRandom>,
    ): QrLoginState = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs, rands.map { it.toFfi() }))
        }
    }

    private fun RustXhs.SignRandom.toFfi(): FfiXhsSignRandom = FfiXhsSignRandom(
        version = version,
        b3TraceId = b3TraceId,
        xrayTail = xrayTail,
        xyRandom = xyRandom,
        rapMask = rapMask,
        rapSessionKey = rapSessionKey,
        rapKey = rapKey,
        rapSalt = rapSalt,
        rapPad = rapPad,
        rapEncTimeMs = rapEncTimeMs,
    )

    private fun RustXhs.SignRequest.toFfi(): FfiXhsSignRequest = FfiXhsSignRequest(
        api = api,
        body = body,
        a1 = a1,
        loadtsMs = loadtsMs,
        webBuild = webBuild,
        b1 = b1,
        userId = userId,
        rap = rap,
        xy = xy,
    )

    private fun RustXhs.Author.toFfi(): FfiXhsAuthor = FfiXhsAuthor(id, name, avatar, xsecToken)
}

/**
 * `HostXhsSignRandom` 的默认实现：**给别的流用的占位**（它们一个随机量都不从这里取）。
 *
 * 关注流的真实现是 `XhsSignRandomDrawer`（在 `source-xiaohongshu` 里，因为它要按 `XhsSigner` 的
 * 取用顺序从**那一个** `Random` 里抽）。这个类只是让 `RustXhsHosted` 的构造参数有个默认值，
 * 免得 12 家平台的调用点全都要改。
 */
internal class XhsHostedSignRandom(
    private val fixed: RustXhs.SignRandom,
) : HostXhsSignRandom {
    override fun drawHead(xy: Boolean, xySeed: String?, rap: Boolean) = uniffi.feedhub_ffi.FfiXhsSignRandomHead(
        version = fixed.version,
        xyRandom = fixed.xyRandom,
        xrayTail = fixed.xrayTail,
        b3TraceId = fixed.b3TraceId,
        rapMask = fixed.rapMask,
        rapSessionKey = fixed.rapSessionKey,
        rapKey = fixed.rapKey,
        rapSalt = fixed.rapSalt,
    )

    override fun drawTail(padLen: UInt) = uniffi.feedhub_ffi.FfiXhsSignRandomTail(
        rapPad = "0".repeat(padLen.toInt()),
        rapEncTimeMs = fixed.rapEncTimeMs,
    )
}

/** `HostXhsClock` 的默认实现：就是宿主给的那个钟（默认 `System.currentTimeMillis`）。 */
internal class XhsHostedClock(private val now: () -> Long) : HostXhsClock {
    override fun nowMs(): Long = now()
}

/** `HostXhsSelfId` 的实现：把 Rust 的同步回调转给一个 Kotlin 闭包（关注流用）。 */
internal class XhsHostedSelfId(private val ask: () -> String?) : HostXhsSelfId {
    override fun selfId(): String? = ask()
}

/** 小红书的限速（`XhsApi.MIN_INTERVAL_MS` / `JITTER_MS`，**只在这一处**）。 */
private object XhsRate {
    const val MIN_INTERVAL_MS = 1500L
    const val JITTER_MS = 800L
}
