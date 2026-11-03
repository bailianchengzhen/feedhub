package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiEmote
import uniffi.feedhub_ffi.FfiPage
import uniffi.feedhub_ffi.FfiXhsPosted
import uniffi.feedhub_ffi.FfiXhsSignRandom
import uniffi.feedhub_ffi.FfiXhsSignRequest
import uniffi.feedhub_ffi.FfiXhsSignState
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.xhsDecodeB1
import uniffi.feedhub_ffi.xhsNewB1
import uniffi.feedhub_ffi.xhsParseComments
import uniffi.feedhub_ffi.xhsParseDetail
import uniffi.feedhub_ffi.xhsParseFollowings
import uniffi.feedhub_ffi.xhsParseHomefeed
import uniffi.feedhub_ffi.xhsParseEmotes
import uniffi.feedhub_ffi.xhsParsePlay
import uniffi.feedhub_ffi.xhsParsePosted
import uniffi.feedhub_ffi.xhsParseReplies
import uniffi.feedhub_ffi.xhsParseUnread
import uniffi.feedhub_ffi.xhsRapPadLen
import uniffi.feedhub_ffi.xhsReadEmotes
import uniffi.feedhub_ffi.xhsFollowDue
import uniffi.feedhub_ffi.xhsFollowNextPage
import uniffi.feedhub_ffi.xhsFollowNoteKeyDecode
import uniffi.feedhub_ffi.xhsFollowNoteKeyEncode
import uniffi.feedhub_ffi.xhsFollowParseAuthors
import uniffi.feedhub_ffi.FfiXhsAuthors
import uniffi.feedhub_ffi.xhsSign

/**
 * 小红书的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustBili` / `RustZhihu` 同一套做法（错误映射 / "节点数组 + 下标"还原 / 加载失败说清楚），
 * 但**故意没有抽公共父类**：这块按平台各放一个文件，
 *
 * 小红书这边是三块纯函数：推荐流 / 关注流三个接口的解析、请求签名（`X-s` / `X-S-Common` / `x-rap-param`）、
 * `b1` 的生成。**时间和随机数都是 Kotlin 给的**：`nowMs` / `nowSec` 当参数传，签名要的随机量由调用方
 * （`RustXhsParse`，它有那个 `Random`）取好、会话计数也在它那里；Rust 不取时钟、不取随机数、不存状态。
 */
object RustXhs {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("xhs") { coreVersion() }

    // ---------------- 解析 ----------------

    /** 推荐流的一页：条目 + 游标 + 标题拼串（调用方拿它去判断要不要取表情表）。 */
    class Homefeed(val page: Page<FeedItem>, val titles: String)

    fun parseHomefeed(
        body: String,
        httpCode: Int,
        noteIndex: Int,
        xsecSource: String,
        nowSec: Long,
        emotes: Map<String, String>,
        platform: Platform,
    ): Homefeed = guard(platform) {
        val ffi = xhsParseHomefeed(body, httpCode, noteIndex, xsecSource, nowSec, emotes.toFfi())
        Homefeed(RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, ffi.next), platform), ffi.titles)
    }

    /** 关注列表里的一个人。和 `source-xiaohongshu` 的 `FollowedAuthor` 是同一个东西（桥接层不依赖插件模块）。 */
    class Author(val id: String, val name: String, val avatar: String?, val xsecToken: String?)

    class Followings(
        val users: List<Author>,
        val cursor: String?,
        val hasMore: Boolean,
        val total: Int?,
    )

    fun parseFollowings(body: String, httpCode: Int, platform: Platform): Followings = guard(platform) {
        val ffi = xhsParseFollowings(body, httpCode)
        Followings(
            ffi.users.map { Author(it.id, it.name, it.avatar, it.xsecToken) },
            ffi.cursor,
            ffi.hasMore,
            ffi.total,
        )
    }

    fun parseUnread(body: String, httpCode: Int, platform: Platform): Map<String, Int> = guard(platform) {
        val out = LinkedHashMap<String, Int>()
        for (e in xhsParseUnread(body, httpCode)) out[e.userId] = e.count
        out
    }

    /** `user_posted` 的一页：每条带"是不是置顶"，还有标题拼串。 */
    class Posted(val notes: List<Note>, val hasMore: Boolean, val titles: String)

    class Note(val item: FeedItem, val sticky: Boolean)

    fun parsePosted(
        body: String,
        httpCode: Int,
        author: Author,
        nowSec: Long,
        emotes: Map<String, String>,
        platform: Platform,
    ): Posted = guard(platform) {
        val ffi: FfiXhsPosted = xhsParsePosted(
            body,
            httpCode,
            author.id,
            author.name,
            author.avatar,
            author.xsecToken,
            nowSec,
            emotes.toFfi(),
        )
        val nodes = ffi.nodes
        val notes = ffi.roots.mapIndexed { i, index ->
            Note(RustModel.buildItem(index, nodes, platform), ffi.sticky[i])
        }
        Posted(notes, ffi.hasMore, ffi.titles)
    }

    // ---------------- 详情 / 评论 / 楼中楼 ----------------

    /** 表情表：`GET /api/im/redmoji/detail` 的响应 → 表 + 存盘那串。 */
    class Emotes(val map: Map<String, String>, val fetchedAtSec: Long, val stored: String)

    fun parseEmotes(body: String, httpCode: Int, nowSec: Long, platform: Platform): Emotes = guard(platform) {
        val ffi = xhsParseEmotes(body, httpCode, nowSec)
        val map = LinkedHashMap<String, String>(ffi.emotes.size)
        for (e in ffi.emotes) map[e.code] = e.url
        Emotes(map, ffi.fetchedAtSec, ffi.stored)
    }

    /** `ctx.store` 里那串读回来（**保持顺序**；读不出来是 null，和 `XhsEmoteTable.fromStored` 一样）。 */
    fun readEmotes(stored: String, platform: Platform): Map<String, String>? = guard(platform) {
        xhsReadEmotes(stored)?.let { list ->
            val map = LinkedHashMap<String, String>(list.size)
            for (e in list) map[e.code] = e.url
            map
        }
    }

    /**
     * 播放解析：`POST /api/sns/web/v1/feed` 的响应 → [PlayInfo]。
     *
     * 和平台对象里的 `play` 是同一份解析（`feedhub-core/src/xhs/play.rs`），区别只在平台对象那边还管
     * "复用详情刚取过的那份"（缓存 / 请求条数），这里不发请求、不看缓存。
     */
    fun parsePlay(body: String, httpCode: Int, platform: Platform): PlayInfo = guard(platform) {
        RustModel.toPlayInfo(xhsParsePlay(body, httpCode))
    }

    /**
     * 详情解析：[body] 是 `POST /api/sns/web/v1/feed` 的响应，[id] 是卡片 `detail` 里那个
     * `<笔记 id>@<xsec_source>@<xsec_token>`（Rust 自己拆）。
     *
     * 生产路径不调它（`RustXhsHosted.detail` 一次做完"发 + 判错 + 解析"），这里给对拍 / 排障用。
     */
    fun parseDetail(
        body: String,
        httpCode: Int,
        id: String,
        nowSec: Long,
        emotes: Map<String, String>,
        platform: Platform,
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(xhsParseDetail(body, httpCode, id, nowSec, emotes.toFfi()), platform)
    }

    /** 一页一级评论的解析（`GET comment/page` 的响应）。 */
    fun parseComments(
        body: String,
        httpCode: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(xhsParseComments(body, httpCode, emotes.toFfi()), platform)
    }

    /** 一页楼中楼的解析（`GET comment/sub/page` 的响应）。 */
    fun parseReplies(
        body: String,
        httpCode: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(xhsParseReplies(body, httpCode, emotes.toFfi()), platform)
    }

    // ---------------- 签名 ----------------

    /** 一个待签名的请求（和 `XhsSigner.Request` 一一对应，但不依赖插件模块的类型）。 */
    class SignRequest(
        val api: String,
        val body: String?,
        val a1: String,
        val loadtsMs: Long,
        val webBuild: String,
        val b1: String,
        val userId: String?,
        val rap: Boolean,
        val xy: Boolean,
    )

    /** 这一签要用的随机量（调用方从自己的 `Random` 里按 Kotlin 版同样的顺序取好）。 */
    class SignRandom(
        val version: Int,
        val b3TraceId: String,
        val xrayTail: String,
        val xyRandom: Int,
        val rapMask: Int,
        val rapSessionKey: String,
        val rapKey: String,
        val rapSalt: String,
        val rapPad: String,
        val rapEncTimeMs: Int,
    )

    /** 签名器的会话计数（Kotlin 存着，每次签名后换成 [Signed.state]）。 */
    class SignState(val seq: Int, val signCount: Int, val dsllt: Long, val xraySeq: Int)

    class Signed(val headers: LinkedHashMap<String, String>, val state: SignState)

    /** 签一个请求：返回要加的请求头（**保持顺序**）和新状态。 */
    fun sign(
        profileJson: String,
        request: SignRequest,
        nowMs: Long,
        random: SignRandom,
        state: SignState,
    ): Signed = guard("xhs") {
        val ffi = xhsSign(
            request = FfiXhsSignRequest(
                api = request.api,
                body = request.body,
                a1 = request.a1,
                loadtsMs = request.loadtsMs,
                webBuild = request.webBuild,
                b1 = request.b1,
                userId = request.userId,
                rap = request.rap,
                xy = request.xy,
            ),
            profileJson = profileJson,
            nowMs = nowMs,
            rand = FfiXhsSignRandom(
                version = random.version,
                b3TraceId = random.b3TraceId,
                xrayTail = random.xrayTail,
                xyRandom = random.xyRandom,
                rapMask = random.rapMask,
                rapSessionKey = random.rapSessionKey,
                rapKey = random.rapKey,
                rapSalt = random.rapSalt,
                rapPad = random.rapPad,
                rapEncTimeMs = random.rapEncTimeMs,
            ),
            state = FfiXhsSignState(state.seq, state.signCount, state.dsllt, state.xraySeq),
        )
        val headers = LinkedHashMap<String, String>(ffi.headers.size)
        for (h in ffi.headers) headers[h.name] = h.value
        Signed(headers, SignState(ffi.state.seq, ffi.state.signCount, ffi.state.dsllt, ffi.state.xraySeq))
    }

    /**
     * 这一次 `x-rap-param` 要取几个补位字符（`(16 - gzip 长度 % 16) % 16`）。
     * Kotlin 版里这个长度是 gzip 之后才知道的，补位字符又紧挨着盐从同一个 `Random` 里取，所以要先问长度。
     */
    fun rapPadLen(
        profileJson: String,
        request: SignRequest,
        nowMs: Long,
        rapMask: Int,
        sessionKey: String,
    ): Int = guard("xhs") {
        xhsRapPadLen(
            request = FfiXhsSignRequest(
                api = request.api,
                body = request.body,
                a1 = request.a1,
                loadtsMs = request.loadtsMs,
                webBuild = request.webBuild,
                b1 = request.b1,
                userId = request.userId,
                rap = request.rap,
                xy = request.xy,
            ),
            profileJson = profileJson,
            nowMs = nowMs,
            rapMask = rapMask,
            sessionKey = sessionKey,
        ).toInt()
    }

    /** `b1`（`X-S-Common` 的 x8）：一次生成、之后一直用（网页存在 localStorage 里）。 */
    fun newB1(profileJson: String, nowMs: Long, loadtsMs: Long): String =
        guard("xhs") { xhsNewB1(profileJson, nowMs, loadtsMs) }

    /** 把 `b1` 解回 JSON（测试 / 排障用）。 */
    fun decodeB1(b1: String): String = guard("xhs") { xhsDecodeB1(b1) }

    // ---------------- 内部 ----------------

    private fun Map<String, String>.toFfi(): List<FfiEmote> = map { FfiEmote(it.key, it.value) }

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /** 纯函数入口：加载 `.so` + 把 `FfiException` 折成现有异常（实现在 [RustModel.guard]）。 */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = RustModel.guard(platform, block)
    // ---------------- 关注流的纯逻辑 / 存盘 ----------------

    /**
     * **作者缓冲的明文 JSON**（`gz1:` 解开之后那段）→ 摘要。
     *
     * 仓库里只有这一条路是"Kotlin 写一份、Rust 读一份"用来验收存盘格式的（生产路径上 Rust
     * 自己读自己写，见 `HostedXhsFollowApi`）。版本不对 / 坏 JSON 都是 `null`（和 Kotlin
     * `XhsFollowStore.loadAuthors` 一样）。
     */
    class Authors(val extraAt: Long, val json: String, val states: List<State>)

    /** 作者缓冲里的一个作者（摘要；[items] 是条数）。 */
    class State(
        val id: String,
        val name: String,
        val xsecToken: String?,
        val items: Int,
        val fetchedAt: Long,
        val unread: Int,
        val unreadAtFetch: Int,
        val closedUntil: Long,
    )

    fun parseAuthors(json: String, platform: Platform): Authors? = guard(platform) {
        xhsFollowParseAuthors(json)?.toModel()
    }

    /** 谁该重拉（顺序 = Kotlin `XhsFollowFreshness.due`）。 */
    fun dueAuthors(json: String, nowMs: Long, limit: Int, platform: Platform): List<String>? = guard(platform) {
        xhsFollowDue(json, nowMs, limit.toUInt())
    }

    /** 跨作者归并出一页；第二个元素 = 后面还有没有（`more`）。 */
    fun nextPage(json: String, before: String?, limit: Int, platform: Platform): Pair<Page<FeedItem>, Boolean>? =
        guard(platform) {
            val ffi = xhsFollowNextPage(json, before, limit.toUInt()) ?: return@guard null
            RustModel.toPage(FfiPage(ffi.nodes, ffi.roots, ffi.next), platform) to (ffi.titles == "more")
        }

    fun encodeNoteKey(time: Long, id: String, platform: Platform): String = guard(platform) {
        xhsFollowNoteKeyEncode(time, id)
    }

    fun decodeNoteKey(text: String, platform: Platform): String? = guard(platform) {
        xhsFollowNoteKeyDecode(text)
    }

    private fun FfiXhsAuthors.toModel(): Authors = Authors(
        extraAt,
        json,
        states.map {
            State(
                it.id,
                it.name,
                it.xsecToken,
                it.items.toInt(),
                it.fetchedAt,
                it.unread,
                it.unreadAtFetch,
                it.closedUntil,
            )
        },
    )

}