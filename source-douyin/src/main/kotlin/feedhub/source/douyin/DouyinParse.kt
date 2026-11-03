package feedhub.source.douyin

import feedhub.core.FeedItem
import feedhub.core.HttpResult
import feedhub.core.Platform
import feedhub.core.parseJsonObject
import feedhub.rust.RustDouyin

/**
 * "拿到响应之后谁来解析"这一个接缝（照 `source-bilibili` 的 `BiliParse` / `source-zhihu` 的
 * `ZhihuParse` / `source-xiaohongshu` 的 `XhsParse`）。
 *
 * 两个实现：
 * - [KotlinDouyinParse]：现有的 [DouyinParser]（推荐流那一段），行为和这个功能存在之前**逐字节一样**；
 * - [RustDouyinParse]：同样的语义在 Rust 里（`rust/crates/feedhub-core/src/douyin`），经 UniFFI 过来。
 *
 * 只有推荐流的解析在这里；**HTTP / Cookie / 代理 / 限速 / 请求头全在 [DouyinSource] 里**，两条路完全一样
 * （`DouyinRequestEquivalenceTest` 用 MockWebServer 录两份请求对比守着）。详情 / 评论 / 表情表 / 扫码
 * 当前没搬，继续走 Kotlin。
 *
 * 抖音的错误判断要**同时看响应头**（`X-Vc-Bdturing-Parameters` / `X-Tt-Verify-Passport-Decision`）、
 * **HTTP 状态码**（403 的 Argus / Sign Invalid、空 body、acrawler 挑战页）和**业务码**（`status_code`），
 * 所以 [feed] 拿的是整个 [HttpResult]：Kotlin 实现先 `checkTransport` 再解析，Rust 实现在 Rust 里一次做完
 * （和 B站 / 知乎 / 小红书 的"错误只做一遍"同一个道理）。
 */
internal interface DouyinParse {
    /** 推荐流一页 + 这一页所有 `desc` 的拼串（调用方拿它判断要不要去取表情表）。 */
    fun feed(res: HttpResult, refreshIndex: Int, emotes: Map<String, String>): ParsedPage
}

/** 一页推荐流：条目 + 下一行游标 + `desc` 拼串。 */
internal class ParsedPage(
    val items: List<FeedItem>,
    val next: String?,
    val texts: String,
)

/** Kotlin 自己的解析（默认）。就是原来那几行，一行语义都没改。 */
internal class KotlinDouyinParse(private val source: DouyinSource) : DouyinParse {
    override fun feed(res: HttpResult, refreshIndex: Int, emotes: Map<String, String>): ParsedPage {
        source.checkTransport(res)
        val json = parseJsonObject(DOUYIN, res.body)
        val checked = source.check(json)
        val page = DouyinParser.feed(checked, refreshIndex, emotes)
        return ParsedPage(page.items, page.next?.value, DouyinParser.feedTexts(checked))
    }

}

/**
 * Rust 解析。**错误由 Rust 带类型回来**，再映射成和 Kotlin 实现同一批异常
 * （`NotLoggedInException` / `RiskControlException` / `SignatureException` / `ApiException` / `ParseException`），
 * 所以上层 `catch` 一行不用改。
 *
 * 表情表（`DouyinEmoteTable`）归 Kotlin：表由调用方传进来，Rust 只按表挑"标题里真的出现过的"。
 */
internal class RustDouyinParse(private val platform: Platform = DOUYIN) : DouyinParse {
    override fun feed(res: HttpResult, refreshIndex: Int, emotes: Map<String, String>): ParsedPage {
        fun hasHeader(name: String) = res.headers.any { it.first.equals(name, ignoreCase = true) }
        val parsed = RustDouyin.parseFeed(
            body = res.body,
            httpCode = res.code,
            // 只看"有没有"：风控头的值不打印、也不带进 Rust
            bdturing = hasHeader("X-Vc-Bdturing-Parameters"),
            passportDecision = hasHeader("X-Tt-Verify-Passport-Decision"),
            refreshIndex = refreshIndex,
            emotes = emotes,
            platform = platform,
        )
        return ParsedPage(parsed.page.items, parsed.page.next?.value, parsed.texts)
    }
}
