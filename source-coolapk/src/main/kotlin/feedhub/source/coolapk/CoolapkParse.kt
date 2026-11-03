package feedhub.source.coolapk

import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.rust.RustCoolapk

/**
 * "拿到响应之后谁来解析 / 算 token"这一个接缝（照 `source-bilibili` 的 `BiliParse`、
 * `source-zhihu` 的 `ZhihuParse`）。
 *
 * 两个实现：
 * - [KotlinCoolapkParse]：现有的 [CoolapkParser] / [AppToken]，行为和这个功能存在之前**逐字节一样**；
 * - [RustCoolapkParse]：同样的语义在 Rust 里（`rust/crates/feedhub-core/src/coolapk`），经 UniFFI 过来。
 *
 * 只有解析和 token 在这里；**HTTP / Cookie / 代理 / 限速 / 请求头全在 [CoolapkSource] 里**——两条路用的是
 * 同一个 `CoolapkSource.request()`。
 *
 * 酷安的错误判断是"看 JSON 里的 message / status，没有 JSON 才看 HTTP 状态码"，所以 [feed] 把
 * **HTTP 状态码**也带进来：Kotlin 实现走 `CoolapkSource.decodeResponse`，Rust 实现在 Rust 里一次做完。
 *
 * 所以 [feed] 现在只剩 [KotlinCoolapkParse] 在用——它是 `CoolapkRequestEquivalenceTest` /
 * `RustCoolapkHostedTest` 的**参照实现**，生产里 [RustCoolapkSource] 的 `feed` 不走它。
 * token 那一半两边都还在用（详情 / 评论 / 搜索 / `raw` 还在 Kotlin，token 由 [RustCoolapkParse] 算）。
 */
internal interface CoolapkParse {
    fun feed(kind: FeedKind, httpCode: Int, body: String, cursor: FeedCursor): Page<FeedItem>

    /** v2 token（信息流 / 关注流 / 详情：13.4.1 / API 13）。 */
    fun v2(deviceCode: String, timestampSec: Long): String

    /** v3 token（评论接口：16.2.0 / API 16）。 */
    fun v3(deviceCode: String, timestampSec: Long, appCode: Long): String
}

/** Kotlin 自己的解析 / token（默认）。就是原来那几行，一行语义都没改。 */
internal object KotlinCoolapkParse : CoolapkParse {
    override fun feed(kind: FeedKind, httpCode: Int, body: String, cursor: FeedCursor): Page<FeedItem> {
        val json = CoolapkSource.decodeResponse(body, httpCode)
        // 登录失效时服务端不报错，而是回一张 unLoginCard（"更多精彩内容请登录"）加一堆"值得看"文章，
        // 不拦下来就会把这些冒充成关注流（Kotlin 原来只拦关注流，这里照旧）。
        if (kind == FeedKind.FOLLOWING && CoolapkParser.isUnLoginPage(json)) throw NotLoggedInException(COOLAPK)
        return CoolapkParser.feedList(json, cursor)
    }

    override fun v2(deviceCode: String, timestampSec: Long): String = AppToken.v2(deviceCode, timestampSec)

    override fun v3(deviceCode: String, timestampSec: Long, appCode: Long): String =
        AppToken.v3(deviceCode, timestampSec, appCode)
}

/**
 * Rust 解析 / token。**错误由 Rust 带类型回来**，再映射成和 Kotlin 实现同一批异常
 * （`NotLoggedInException` / `RiskControlException` / `SignatureException` / `ApiException` / `ParseException`），
 * 所以上层 `catch` 一行不用改。
 *
 * 表情表（`CoolapkPlugin.emotes.codes`）和 v3 的密码表（插件资源 `auth_blob.b64`）都由插件给，Rust 不复制这两张表。
 */
internal class RustCoolapkParse(
    private val platform: Platform = COOLAPK,
    private val blob: ByteArray = AppToken.v3BlobBytes(),
) : CoolapkParse {
    private val emotes: Map<String, String> = CoolapkPlugin.emotes?.codes.orEmpty()

    override fun feed(kind: FeedKind, httpCode: Int, body: String, cursor: FeedCursor): Page<FeedItem> =
        RustCoolapk.parseFeed(
            kind = kind,
            httpCode = httpCode,
            page = cursor.page,
            firstItem = cursor.firstItem,
            lastItem = cursor.lastItem,
            body = body,
            emotes = emotes,
            platform = platform,
        )

    override fun v2(deviceCode: String, timestampSec: Long): String =
        RustCoolapk.v2Token(deviceCode, timestampSec)

    override fun v3(deviceCode: String, timestampSec: Long, appCode: Long): String =
        RustCoolapk.v3Token(deviceCode, timestampSec, appCode, blob)
}
