package feedhub.source.douyin

import java.security.MessageDigest

/**
 * 抖音 web 端的 `x-secsdk-web-signature`（secsdk 的 `webSignUrl` 策略）。
 *
 * ## 算法
 *
 * ```
 * x-secsdk-web-signature = MD5("{uifid}_{timestamp10位秒}_{固定盐}_{规范化 query}")
 * ```
 *
 * 规范化规则（保持参数顺序、不排序、重复参数原样保留）：
 * - **value**：先解码（`+` → 空格、`%XX` → 字节），再用 `encodeURIComponent` 重编码。
 *   不编码 `A-Za-z0-9 - _ . ! ~ * ' ( )`，空格变 `%20`，中文按 UTF-8 编码。
 * - **key**：只解码，**不**重编码。
 * - 没有 `=` 的裸参数补成 `k=`。
 * - 末尾追加 `&timestamp={ts}` 之后才参与哈希；query 里本来没有 uifid 时，用 Cookie 里的 `UIFID` 补一条。
 * - **发出去的必须是这个规范化后的 query**（服务端按收到的 query 校验），不能拿原始 URL 直接发。
 *
 * ## 来源
 *
 * **按 cv-cat/DouYin_Spider `utils/secsdk_web_sign.py` 的实现翻译**
 * （规范化循环、`names` 检查、补 uifid、受保护路径表和盐逐项对应；下面的规则说明基本是它 docstring 的直译）。
 * 同一作者后来把这段以 **MIT** 收进 cv-cat/catbus（`src/platforms/douyin/web/sign.ts` 的 `canonicalQuery` / `signedUrl`），
 * 规则和盐与那里一致。固定盐是 VM 常量池里的写死值，与账号 / 会话无关。
 *
 * ## 期望值来源（见 `SecSdkSignerTest`）
 *
 * 1. **独立来源**：`mafqla__douyin-api/lib/reverse/browser_feed_signed.json` 是**真实 Chrome 抓到的完整
 *    请求 URL**（`tab/feed`，带 `uifid` / `timestamp` / `x-secsdk-web-signature`）。本实现在开发时用它
 *    的 `uifid` + `timestamp` + query 复算，签名逐字节命中。为避免把第三方设备的 uifid 落进本仓库，
 *    那条向量没有写进测试代码，只在 `docs` 级说明里记着。
 * 2. `cv-cat__catbus/tests/golden/douyin/pure.json`（**MIT**）的 `sign_url` 给了两条边界用例
 *    （空格、分号、百分号、中文、空值、无等号、带空格的 uifid），用的是明显的假 uifid，测试里直接用。
 *
 * @param uifidFallback query 里没有 `uifid` 时用它补（对应 Cookie 里的 `UIFID` / `UIFID_TEMP`）。
 */
internal object SecSdkSigner {
    /** VM 常量池里的固定盐（与账号 / 会话无关）。 */
    const val SALT = "A96D855A08C0A9707F8BEF0D9A527E4E"

    const val HEADER = "x-secsdk-web-signature"
    const val TIMESTAMP_PARAM = "timestamp"

    /** `encodeURIComponent` 不编码的字符（除 `A-Za-z0-9` 与 `- _ . ~` 之外）。 */
    private const val JS_SAFE = "-_.!~*'()"

    /**
     * SDK 的 `webSign` 策略保护的表（GET）。注释和 `module/feed` 都**不在**表里，所以推荐流只要 a_bogus。
     * 表是抖音网页 SDK 自己的策略配置（cv-cat 在 DouYin_Spider 里记录的），我们没有单独实测。
     */
    val PROTECTED_GET = setOf(
        "/aweme/v1/web/aweme/detail/",
        "/aweme/v1/web/aweme/post/",
        "/aweme/v1/web/aweme/favorite/",
        "/aweme/v1/web/aweme/listcollection/",
        "/aweme/v1/web/mix/aweme/",
        "/aweme/v1/web/tab/feed/",
        "/aweme/v1/web/mix/list/",
        "/aweme/v1/web/music/aweme/",
        "/aweme/v1/web/music/list/",
        "/aweme/v1/web/mix/detail/",
        "/aweme/v1/web/mix/listcollection/",
        "/aweme/v1/web/music/detail/",
        "/aweme/v1/web/collects/list/",
        "/aweme/v1/web/collects/video/list/",
    )

    /** POST 的前 6 条同上。 */
    val PROTECTED_POST = PROTECTED_GET.take(6).toSet()

    /** 这个路径要不要加 [HEADER]（[method] 不看大小写）。 */
    fun isProtected(path: String, method: String = "GET"): Boolean =
        if (method.equals("POST", ignoreCase = true)) path in PROTECTED_POST else path in PROTECTED_GET

    /** 一次签名的结果：签名值 + **实际应该发出去的** query。 */
    data class Signed(val signature: String, val query: String, val timestamp: Long)

    /**
     * 规范化 query（不含 `timestamp`，也不含签名本身）。
     * 已经有了 `timestamp` / 签名字段时会先剥掉，所以对同一个 URL 反复调用是幂等的。
     */
    fun canonicalQuery(query: String): String {
        val parts = ArrayList<String>()
        for (pair in query.split('&')) {
            if (pair.isEmpty()) continue
            val eq = pair.indexOf('=')
            val rawKey = if (eq >= 0) pair.substring(0, eq) else pair
            val rawValue = if (eq >= 0) pair.substring(eq + 1) else ""
            val key = decodeComponent(rawKey)
            if (key == TIMESTAMP_PARAM || key == HEADER) continue
            parts += "$key=${encodeComponent(decodeComponent(rawValue))}"
        }
        return parts.joinToString("&")
    }

    /**
     * 算签名。
     *
     * @param query 原始 query（不带 `?`；可以已经含 `timestamp` / 签名字段，会自动剥掉）
     * @param uifidFallback query 里没有 uifid 时的兜底值
     * @param timestamp 10 位秒级时间戳
     */
    fun sign(query: String, uifidFallback: String? = null, timestamp: Long): Signed {
        var canon = canonicalQuery(query)
        val names = canon.split('&').filter { it.isNotEmpty() }.map { it.substringBefore('=') }
        if ("uifid" !in names && !uifidFallback.isNullOrEmpty()) {
            val extra = "uifid=${encodeComponent(uifidFallback)}"
            canon = if (canon.isEmpty()) extra else "$canon&$extra"
        }
        val signed = if (canon.isEmpty()) "$TIMESTAMP_PARAM=$timestamp" else "$canon&$TIMESTAMP_PARAM=$timestamp"

        // 参与拼明文的 uifid 是「解码后的原值」
        val uifid = signed.split('&')
            .firstOrNull { it.startsWith("uifid=") }
            ?.substring("uifid=".length)
            ?.let(::decodeComponent)
            ?: uifidFallback.orEmpty()

        val plain = "${uifid}_${timestamp}_${SALT}_$signed"
        return Signed(md5Hex(plain), signed, timestamp)
    }

    /** 等价于 JS 的 `decodeURIComponent`（外加 `+` → 空格）。 */
    internal fun decodeComponent(text: String): String {
        if ('%' !in text && '+' !in text) return text
        val out = java.io.ByteArrayOutputStream(text.length)
        var i = 0
        while (i < text.length) {
            val c = text[i]
            when {
                c == '+' -> {
                    out.write(' '.code)
                    i++
                }
                c == '%' && i + 2 < text.length -> {
                    val hex = text.substring(i + 1, i + 3)
                    val b = hex.toIntOrNull(16)
                    if (b == null) {
                        out.write(c.code)
                        i++
                    } else {
                        out.write(b)
                        i += 3
                    }
                }
                else -> {
                    // 非 ASCII 按 UTF-8 写，和 JS 一致
                    out.write(c.toString().toByteArray(Charsets.UTF_8))
                    i++
                }
            }
        }
        return String(out.toByteArray(), Charsets.UTF_8)
    }

    /** 等价于 JS 的 `encodeURIComponent`。 */
    internal fun encodeComponent(text: String): String {
        val sb = StringBuilder(text.length)
        for (byte in text.toByteArray(Charsets.UTF_8)) {
            val c = byte.toInt() and 0xFF
            val ch = c.toChar()
            if (ch in 'A'..'Z' || ch in 'a'..'z' || ch in '0'..'9' || ch in JS_SAFE) {
                sb.append(ch)
            } else {
                sb.append('%').append(HEX[c ushr 4]).append(HEX[c and 0x0F])
            }
        }
        return sb.toString()
    }

    private fun md5Hex(text: String): String {
        val digest = MessageDigest.getInstance("MD5").digest(text.toByteArray(Charsets.UTF_8))
        return digest.joinToString("") { "%02x".format(it) }
    }

    private const val HEX = "0123456789ABCDEF"
}
