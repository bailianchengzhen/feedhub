package feedhub.source.xiaohongshu

import feedhub.core.FeedItem
import feedhub.core.HttpResult
import feedhub.core.Platform
import feedhub.rust.RustXhs
import kotlin.random.Random

/**
 * "拿到响应之后谁来解析 / 签名"这一个接缝（照 `source-bilibili` 的 `BiliParse` / `source-zhihu` 的 `ZhihuParse`）。
 *
 * 两个实现：
 * - [KotlinXhsParse]：现有的 [XhsParser] / [XhsFollowParser] / [XhsSigner]，行为和这个功能存在之前**逐字节一样**；
 * - [RustXhsParse]：同样的语义在 Rust 里（`rust/crates/feedhub-core/src/xhs`），经 UniFFI 过来。
 *
 * 只有解析和签名在这里；**HTTP / Cookie / 代理 / 限速 / 请求头全在 [XhsApi] 里**，两条路完全一样
 * （`XhsRequestEquivalenceTest` 用 MockWebServer 录两份请求对比守着）。
 *
 * 小红书的错误判断要看 **HTTP 状态码**（461 / 471 验证码、406 / 416 签名、401 未登录，再进业务码），
 * 所以这几个方法拿的是整个 [HttpResult]：Kotlin 实现自己先映射错误再解析，Rust 实现在 Rust 里一次做完
 * （和 B站 / 知乎 的"错误只做一遍"同一个道理）。
 *
 * 时间（`nowSec` / `nowMs`）由调用方给——Rust 不取时钟；签名要的随机量由 [RustXhsParse] 从自己的
 * [Random] 里按 Kotlin 版**同样的顺序**取好再传进去（Rust 不取随机数）。
 */
internal interface XhsParse {
    /** 推荐流一页。 */
    fun homefeed(
        res: HttpResult,
        noteIndex: Int,
        xsecSource: String,
        nowSec: Long,
        emotes: Map<String, String>,
    ): Homefeed

    fun followings(res: HttpResult): XhsFollowParser.FollowingsPage

    fun unread(res: HttpResult): Map<String, Int>

    fun posted(
        res: HttpResult,
        author: FollowedAuthor,
        nowSec: Long,
        emotes: Map<String, String>,
    ): XhsFollowParser.PostedPage

    /** 签一个请求，返回要加的请求头（小写名字，保持顺序）。[nowMs] 由调用方给（测试里固定）。 */
    fun sign(request: XhsSigner.Request, nowMs: Long): LinkedHashMap<String, String>

    /** `b1`（X-S-Common 的 x8），一次生成后由 [XhsApi] 存进 `ctx.store`。 */
    fun newB1(nowMs: Long, loadtsMs: Long): String
}

/** 推荐流一页 + 这一页的标题拼串（调用方拿它判断要不要去取表情表，保持原来的行为）。 */
internal class Homefeed(val items: List<FeedItem>, val next: String?, val titles: String)

/** Kotlin 自己的解析 / 签名（默认）。就是原来那几行，一行语义都没改（只多了时间参数）。 */
internal class KotlinXhsParse(profile: XhsProfile, random: Random) : XhsParse {
    private val signer = XhsSigner(profile, random)

    override fun homefeed(
        res: HttpResult,
        noteIndex: Int,
        xsecSource: String,
        nowSec: Long,
        emotes: Map<String, String>,
    ): Homefeed {
        val json = XhsApi.checkResponse(res)
        val page = XhsParser.homefeed(json, emotes, xsecSource, nowSec)
        val next = page.cursorScore?.takeIf { page.rawCount > 0 }?.let { "$it|${noteIndex + page.rawCount}" }
        return Homefeed(page.items, next, XhsParser.homefeedTitles(json))
    }

    override fun followings(res: HttpResult): XhsFollowParser.FollowingsPage =
        XhsFollowParser.followings(XhsApi.checkResponse(res))

    override fun unread(res: HttpResult): Map<String, Int> = XhsFollowParser.unread(XhsApi.checkResponse(res))

    override fun posted(
        res: HttpResult,
        author: FollowedAuthor,
        nowSec: Long,
        emotes: Map<String, String>,
    ): XhsFollowParser.PostedPage {
        val json = XhsApi.checkResponse(res)
        val page = XhsFollowParser.posted(json, author, emotes, nowSec)
        return XhsFollowParser.PostedPage(page.notes, page.hasMore, XhsFollowParser.postedTitles(json))
    }

    override fun sign(request: XhsSigner.Request, nowMs: Long): LinkedHashMap<String, String> =
        signer.sign(request, nowMs)

    override fun newB1(nowMs: Long, loadtsMs: Long): String = signer.newB1(nowMs, loadtsMs)
}

/**
 * Rust 解析 / 签名。**错误由 Rust 带类型回来**，再映射成和 Kotlin 实现同一批异常
 * （`NotLoggedInException` / `RiskControlException` / `SignatureException` / `ApiException` / `ParseException`），
 * 所以上层 `catch` 一行不用改。
 *
 * 签名器的会话计数（`seq` / `signCount` / `dsllt` / `xraySeq`）和 `Random` 都在这里——Rust 不存状态、不取随机数。
 * 每一笔随机的**取用顺序和 [XhsSigner] 里一模一样**（version → xy-direction → 两个 traceid → 掩码 / 会话密钥 /
 * 密钥 / 盐 / 补位 / 加密耗时），补位长度先问 Rust 再取（Kotlin 版是 gzip 之后才知道的），
 * 所以同一个 `Random(seed)` 在两边逐字节产出同一组请求头（`XhsDualRunTest` 守着）。
 */
internal class RustXhsParse(
    profile: XhsProfile,
    private val random: Random = Random.Default,
    private val platform: Platform = XHS,
) : XhsParse {
    private val profileJson = XhsProfile.encode(profile)

    private var state = RustXhs.SignState(
        seq = 12 + random.nextInt(29),
        signCount = 5 + random.nextInt(60),
        dsllt = 0,
        xraySeq = random.nextInt(0x7fffff),
    )

    override fun homefeed(
        res: HttpResult,
        noteIndex: Int,
        xsecSource: String,
        nowSec: Long,
        emotes: Map<String, String>,
    ): Homefeed {
        val page = RustXhs.parseHomefeed(res.body, res.code, noteIndex, xsecSource, nowSec, emotes, platform)
        return Homefeed(page.page.items, page.page.next?.value, page.titles)
    }

    override fun followings(res: HttpResult): XhsFollowParser.FollowingsPage {
        val page = RustXhs.parseFollowings(res.body, res.code, platform)
        return XhsFollowParser.FollowingsPage(
            page.users.map { FollowedAuthor(it.id, it.name, it.avatar, it.xsecToken) },
            page.cursor,
            page.hasMore,
            page.total,
        )
    }

    override fun unread(res: HttpResult): Map<String, Int> = RustXhs.parseUnread(res.body, res.code, platform)

    override fun posted(
        res: HttpResult,
        author: FollowedAuthor,
        nowSec: Long,
        emotes: Map<String, String>,
    ): XhsFollowParser.PostedPage {
        val page = RustXhs.parsePosted(
            res.body,
            res.code,
            RustXhs.Author(author.id, author.name, author.avatar, author.xsecToken),
            nowSec,
            emotes,
            platform,
        )
        return XhsFollowParser.PostedPage(
            page.notes.map { XhsFollowParser.PostedNote(it.item, it.sticky) },
            page.hasMore,
            page.titles,
        )
    }

    @Synchronized
    override fun sign(request: XhsSigner.Request, nowMs: Long): LinkedHashMap<String, String> {
        // ---- 下面这些取法（含顺序）和 XhsSigner.sign 一一对应 ----
        val version = random.nextInt()
        // `XhsSigner.sign` 里是 `xyDirection(req.userId ?: req.a1)`：种子是空串才取随机，否则用 murmur3 算
        val xySeed = request.userId ?: request.a1
        val xyRandom = if (request.xy && xySeed.isEmpty()) 10 + random.nextInt(91) else 0
        val xrayTail = hexRandom(16)
        val b3 = hexRandom(16)
        var mask = 0
        var sessionKey = ""
        var key = ""
        var salt = ""
        var pad = ""
        var encTime = 0
        if (request.rap) {
            mask = XhsRap.ALPHABET36[random.nextInt(XhsRap.ALPHABET36.length)].code
            sessionKey = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
            key = String(XhsRap.randomAscii(random, 16), Charsets.US_ASCII)
            salt = String(XhsRap.randomAscii(random, 4 + random.nextInt(3)), Charsets.US_ASCII)
            val padLen = RustXhs.rapPadLen(profileJson, request.toBridge(), nowMs, mask, sessionKey)
            pad = String(XhsRap.randomAscii(random, padLen), Charsets.US_ASCII)
            encTime = 60 + random.nextInt(181)
        }
        val signed = RustXhs.sign(
            profileJson = profileJson,
            request = request.toBridge(),
            nowMs = nowMs,
            random = RustXhs.SignRandom(
                version = version,
                b3TraceId = b3,
                xrayTail = xrayTail,
                xyRandom = xyRandom,
                rapMask = mask,
                rapSessionKey = sessionKey,
                rapKey = key,
                rapSalt = salt,
                rapPad = pad,
                rapEncTimeMs = encTime,
            ),
            state = state,
        )
        state = signed.state
        return signed.headers
    }

    override fun newB1(nowMs: Long, loadtsMs: Long): String = RustXhs.newB1(profileJson, nowMs, loadtsMs)

    private fun hexRandom(n: Int): String = buildString { repeat(n) { append("0123456789abcdef"[random.nextInt(16)]) } }
}

internal fun XhsSigner.Request.toBridge(): RustXhs.SignRequest = RustXhs.SignRequest(
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
