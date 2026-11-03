package feedhub.source.bilibili

import feedhub.core.ApiException
import feedhub.core.FeedItem
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.long
import feedhub.core.parseJsonObject
import feedhub.core.text
import feedhub.rust.RustBili
import kotlinx.serialization.json.JsonObject

/**
 * "拿到 body 之后谁来解析 / 签名"这一个接缝。
 *
 * 两个实现：
 * - [KotlinBiliParse]：现有的 [BiliParser] / [WbiSigner]，行为和这个功能存在之前**逐字节一样**；
 * - [RustBiliParse]：同样的语义在 Rust 里（`rust/crates/feedhub-core`），经 UniFFI 过来。
 *
 * 只有解析和签名在这里；**HTTP / Cookie / 代理 / 限速 / 请求头全在 [BilibiliSource] 里**，两条路完全一样
 * （`BiliRequestEquivalenceTest` 用 MockWebServer 录两次请求对比守着）。
 */
internal interface BiliParse {
    fun recommend(body: String, freshIdx: Int): Page<FeedItem>

    fun dynamics(body: String): Page<FeedItem>

    fun wbiKeys(nav: JsonObject): Pair<String, String>?

    fun mixinKey(imgKey: String, subKey: String): String

    /** 返回补上 `wts` / `w_rid` 的参数表，顺序必须与 Kotlin 实现一致（请求字节）。 */
    fun sign(params: Map<String, String>, mixinKey: String, ts: Long): Map<String, String>
}

/** Kotlin 自己的解析（默认）。就是原来那几行，一行语义都没改。 */
internal object KotlinBiliParse : BiliParse {
    private val P = BILIBILI

    override fun recommend(body: String, freshIdx: Int): Page<FeedItem> =
        BiliParser.recommend(checkBiliEnvelope(P, parseJsonObject(P, body)), freshIdx)

    override fun dynamics(body: String): Page<FeedItem> =
        BiliParser.dynamics(checkBiliEnvelope(P, parseJsonObject(P, body)))

    override fun wbiKeys(nav: JsonObject): Pair<String, String>? = BiliParser.wbiKeys(nav)

    override fun mixinKey(imgKey: String, subKey: String): String = WbiSigner.mixinKey(imgKey, subKey)

    override fun sign(params: Map<String, String>, mixinKey: String, ts: Long): Map<String, String> =
        WbiSigner.sign(params, mixinKey, ts)
}

/**
 * Rust 解析 / 签名。**错误由 Rust 带类型回来**，再映射成和 Kotlin 实现同一批异常
 * （`NotLoggedInException` / `RiskControlException` / `SignatureException` / `ApiException` / `ParseException`），
 * 所以上层 `catch` 一行不用改。
 */
internal class RustBiliParse(private val platform: Platform = BILIBILI) : BiliParse {
    override fun recommend(body: String, freshIdx: Int): Page<FeedItem> =
        RustBili.parseRecommend(body, freshIdx, platform)

    override fun dynamics(body: String): Page<FeedItem> = RustBili.parseDynamics(body, platform)

    override fun wbiKeys(nav: JsonObject): Pair<String, String>? = RustBili.wbiKeys(nav.toString())

    override fun mixinKey(imgKey: String, subKey: String): String = RustBili.wbiMixinKey(imgKey, subKey)

    override fun sign(params: Map<String, String>, mixinKey: String, ts: Long): Map<String, String> =
        RustBili.wbiSign(params, mixinKey, ts)
}

/**
 * 检查 B站统一的 `{code, message, data}` 外壳，非 0 抛对应异常。
 *
 * 从 [BilibiliSource] 里提出来（行为不变），这样 Kotlin 解析实现和 Rust 实现用的是**同一套**错误语义：
 * Rust 侧 `bili::check_envelope` 照着这个函数一条一条抄的。
 */
internal fun checkBiliEnvelope(platform: Platform, json: JsonObject): JsonObject {
    val code = json.long("code") ?: throw ParseException(platform, "响应缺少 code")
    val msg = json.text("message") ?: json.text("msg") ?: ""
    return when (code) {
        0L -> json
        -101L -> throw NotLoggedInException(platform)
        // -352 风控校验失败；-412 请求被拦截；-509 / -799 请求过于频繁
        -352L, -412L, -509L, -799L -> throw RiskControlException(platform, code.toString(), msg)
        -403L -> throw SignatureException(platform, "$code $msg")
        // -111 csrf 校验失败（bili_jct 过期）；22015 账号异常，要先过验证
        -111L -> throw NotLoggedInException(platform, "csrf 校验失败，登录可能已过期")
        22015L -> throw RiskControlException(platform, code.toString(), msg)
        else -> throw ApiException(platform, code.toString(), msg)
    }
}
