package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiDouyinAbogusEnv
import uniffi.feedhub_ffi.FfiDouyinAbogusProfile
import uniffi.feedhub_ffi.FfiEmote
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.douyinAbogusParts
import uniffi.feedhub_ffi.douyinAbogusRandomCount
import uniffi.feedhub_ffi.douyinAbogusSign
import uniffi.feedhub_ffi.douyinCommentTexts
import uniffi.feedhub_ffi.douyinParseComments
import uniffi.feedhub_ffi.douyinParseDetail
import uniffi.feedhub_ffi.douyinParseFeed
import uniffi.feedhub_ffi.douyinParseItem
import uniffi.feedhub_ffi.douyinSecsdkCanonicalQuery
import uniffi.feedhub_ffi.douyinSecsdkDecode
import uniffi.feedhub_ffi.douyinSecsdkEncode
import uniffi.feedhub_ffi.douyinSecsdkIsProtected
import uniffi.feedhub_ffi.douyinSecsdkSign
import uniffi.feedhub_ffi.douyinSm3Hex

/**
 * 抖音的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustZhihu` / `RustXhs` / `RustCoolapk` 同一套做法（错误映射 / "节点数组 + 下标"还原 /
 * 加载失败说清楚）。按平台一个文件，
 *
 * 抖音这边是四块纯函数+ 一个平台对象：
 * - **推荐流解析**（`module/feed`）：`cards[].aweme` 是 JSON 字符串，二次解析也在 Rust 里；
 * - **a_bogus**：`abogus_profile.json` 的常量（[AbogusProfile]）和浏览器环境（[AbogusEnv]）由 Kotlin 传，
 *   时间（`nowMs` / `enterPageTs`）和**随机数流**也由 Kotlin 给（[abogusRandomCount] 先问个数）；
 * - **`x-secsdk-web-signature`**：受保护路径（`aweme/detail` 等）才加；
 * - **SM3**：自己实现的国标算法（a_bogus 里用三次），[sm3Hex] 只给对拍用。
 *
 * Rust **不取时钟、不取随机数、不存状态、不发请求**。
 */
object RustDouyin {
    private const val PLATFORM = "douyin"

    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("douyin") { coreVersion() }

    // ---------------- 解析 ----------------

    /** 推荐流一页：条目 + 游标 + 这一页所有 `desc` 的拼串（调用方拿它决定要不要取表情表）。 */
    class Feed(val page: Page<FeedItem>, val texts: String)

    /**
     * 解析一页推荐流。`httpCode` / `bdturing` / `passportDecision` 一起传进去，
     * **错误映射也在 Rust 里做**（和 Kotlin `DouyinSource.checkTransport` + `check` 逐行对应）。
     */
    fun parseFeed(
        body: String,
        httpCode: Int,
        bdturing: Boolean,
        passportDecision: Boolean,
        refreshIndex: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Feed = guard(platform) {
        val ffi = douyinParseFeed(
            body = body,
            httpCode = httpCode,
            bdturing = bdturing,
            passportDecision = passportDecision,
            refreshIndex = refreshIndex.toLong(),
            emotes = emotes.toFfi(),
        )
        Feed(RustModel.toPage(ffi.page, platform), ffi.texts)
    }

    /** 一份**不带外壳**的 aweme 对象（详情 fixture 里的 `aweme_detail`；测试 / 边界样本用）。 */
    fun parseItem(body: String, emotes: Map<String, String>, platform: Platform): FeedItem? =
        guard(platform) {
            val node = douyinParseItem(body, emotes.toFfi())
            node.root?.let { RustModel.buildItem(it, node.nodes, platform) }
        }

    // ---------------- 详情 / 评论 ----------------

    /**
     * 解析详情（`aweme/detail` 的响应）。`emotes` 是表情表（文字代码 → 图片地址），
     * 由 Kotlin 的表情表传进来——Rust 不复制表、也不发取表的请求。
     */
    fun parseDetail(
        body: String,
        id: String,
        emotes: Map<String, String>,
        platform: Platform,
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(douyinParseDetail(body, id, emotes.toFfi()), platform)
    }

    /** 解析一页评论（`comment/list` 与 `comment/list/reply` 同一个结构）。 */
    fun parseComments(
        body: String,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(douyinParseComments(body, emotes.toFfi()), platform)
    }

    /** 一页评论里所有评论正文的拼串。 */
    fun commentTexts(body: String, platform: Platform): String = guard(platform) {
        douyinCommentTexts(body)
    }

    // ---------------- a_bogus ----------------

    /** a_bogus 的常量（Kotlin `AbogusProfile`）：`abogus_profile.json` / `ctx.store` 的整份覆盖。 */
    class AbogusProfile(
        val aid: Int,
        val pageId: Int,
        val sdkVersion: String,
        val sdkMinor: Int,
        val u17: Int,
        val u37: List<Int>,
        val counter: Int,
    )

    /** 签名时用的浏览器环境（Kotlin `AbogusEnv`）。 */
    class AbogusEnv(
        val userAgent: String,
        val platform: String,
        val innerWidth: Int,
        val innerHeight: Int,
        val outerWidth: Int,
        val outerHeight: Int,
        val availWidth: Int,
        val availHeight: Int,
        val screenSizeWidth: Int,
        val screenSizeHeight: Int,
        val fingerprintOverride: String?,
    )

    /** 算法中间的几段字节。 */
    class AbogusParts(
        val u86: IntArray,
        val u88: IntArray,
        val u89: IntArray,
        val u90: IntArray,
    )

    /**
     * 这一次签名要从随机数流里取几个 `nextDouble()`。
     *
     * 只是纯计算（指纹串 / 时间串的长度决定 `u88` 有几段），**不取随机数**；
     * 调用方照这个数从自己的随机源里按顺序取好，再交给 [abogusSign]——这样两边的随机流完全对齐。
     */
    fun abogusRandomCount(env: AbogusEnv, nowMs: Long, platform: Platform = Platform(PLATFORM)): Int =
        guard(platform) { douyinAbogusRandomCount(env.toFfi(), nowMs).toInt() }

    /** 生成 a_bogus。`rand` 的长度要 ≥ [abogusRandomCount]。 */
    fun abogusSign(
        profile: AbogusProfile,
        env: AbogusEnv,
        query: String,
        nowMs: Long,
        enterPageTs: Long,
        rand: DoubleArray,
        platform: Platform,
    ): String = guard(platform) {
        douyinAbogusSign(profile.toFfi(), env.toFfi(), query, nowMs, enterPageTs, rand.toList())
    }

    /** a_bogus 的中间字节。 */
    fun abogusParts(
        profile: AbogusProfile,
        env: AbogusEnv,
        query: String,
        nowMs: Long,
        enterPageTs: Long,
        rand: DoubleArray,
        platform: Platform,
    ): AbogusParts = guard(platform) {
        val p = douyinAbogusParts(profile.toFfi(), env.toFfi(), query, nowMs, enterPageTs, rand.toList())
        AbogusParts(p.u86.toIntArray(), p.u88.toIntArray(), p.u89.toIntArray(), p.u90.toIntArray())
    }

    // ---------------- x-secsdk-web-signature ----------------

    /** 一次签名的结果：签名值 + **实际应该发出去的** query。 */
    class SecSdkSigned(val signature: String, val query: String, val timestamp: Long)

    fun secSdkSign(
        query: String,
        uifidFallback: String?,
        timestampSec: Long,
        platform: Platform,
    ): SecSdkSigned = guard(platform) {
        val s = douyinSecsdkSign(query, uifidFallback, timestampSec)
        SecSdkSigned(s.signature, s.query, s.timestamp)
    }

    /** 规范化 query（测试 / 排障用）。 */
    fun secSdkCanonicalQuery(query: String, platform: Platform): String =
        guard(platform) { douyinSecsdkCanonicalQuery(query) }

    /** 这个路径要不要加 `x-secsdk-web-signature`。 */
    fun secSdkIsProtected(path: String, method: String, platform: Platform): Boolean =
        guard(platform) { douyinSecsdkIsProtected(path, method) }

    /** 等价于 JS 的 `encodeURIComponent`（拼 query 用，测试对拍用）。 */
    fun secSdkEncode(text: String, platform: Platform): String = guard(platform) { douyinSecsdkEncode(text) }

    /** 等价于 JS 的 `decodeURIComponent`（外加 `+` → 空格）。 */
    fun secSdkDecode(text: String, platform: Platform): String = guard(platform) { douyinSecsdkDecode(text) }

    /** SM3 的小写十六进制（和 Kotlin `Sm3.hex` 对拍用）。 */
    fun sm3Hex(text: String, platform: Platform = Platform(PLATFORM)): String =
        guard(platform) { douyinSm3Hex(text) }

    // ---------------- 内部 ----------------

    private fun Map<String, String>.toFfi(): List<FfiEmote> = map { FfiEmote(it.key, it.value) }

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /** 纯函数入口：加载 `.so` + 把 `FfiException` 折成现有异常（实现在 [RustModel.guard]，和其它几家共用）。 */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = RustModel.guard(platform, block)
}

/**
 * `AbogusProfile` → 过边界的 record。
 *
 * **顶层**而不是 `RustDouyin` 里的成员：平台对象（`RustDouyinHosted`）也要用同一份转换，
 * 成员扩展从对象外面看不见，各写一份就迟早会漂。
 */
internal fun RustDouyin.AbogusProfile.toFfi() = FfiDouyinAbogusProfile(
    aid = aid,
    pageId = pageId,
    sdkVersion = sdkVersion,
    sdkMinor = sdkMinor,
    u17 = u17,
    u37 = u37,
    counter = counter,
)

/** `AbogusEnv` → 过边界的 record（见上一条的理由）。 */
internal fun RustDouyin.AbogusEnv.toFfi() = FfiDouyinAbogusEnv(
    userAgent = userAgent,
    platform = platform,
    innerWidth = innerWidth,
    innerHeight = innerHeight,
    outerWidth = outerWidth,
    outerHeight = outerHeight,
    availWidth = availWidth,
    availHeight = availHeight,
    screenSizeWidth = screenSizeWidth,
    screenSizeHeight = screenSizeHeight,
    fingerprintOverride = fingerprintOverride,
)
