package feedhub.source.douyin

import feedhub.core.ApiException
import feedhub.core.NotLoggedInException
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.bool
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.text
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import java.security.MessageDigest

/**
 * 抖音网页扫码登录（`login.douyin.com` 的 passport 接口）的参数拼装与响应解析：纯函数，不发请求。
 *
 * 协议读的是两个只读参考（都没有运行）：
 * - `juelianxyt/douyin-web-qr-login: `；
 * - `…/抖音/cv-cat__DouYin_Spider/dy_apis/login_api.py`（无 License，只学口径：`get_qrcode` 宽松、`check_qrconnect` 严，
 *   `error_code=7` 是轮询限频、`2046` 是短信二次验证）。
 *
 * ## 和参考实现的差别（都是我们的规矩，不是省事）
 *
 * 1. **不做 acrawler / challenge / dtrait**：cv-cat 的实录说首个 passport 请求要带 `__ac_signature`（执行服务端下发的
 *    混淆脚本算出来）、登录域还要 `x-tt-session-dtrait`（跑 `dtrait-core` 的 JS）。那是绕过人机校验，
 * 所以这里只做我们自己能算的：**a_bogus**（[AbogusSigner]）+ 本机已有的 Cookie。
 * 2. **不编设备身份**：`account_sdk_source_info` 只放 [DouyinDevice] 里已有的那几个值（窗口 / 屏幕 / 核数），
 *    `webgl` / `plugins` / `navigationTiming` 这些我们没有真值的字段**干脆不写**（参考实现里是凭空编的）。
 * 3. **不做短信二次验证**（`error_code=2046`）：只做扫码，遇到就停（[RiskControlException]）。
 *
 * 参数名 / 顺序照 juelianxyt 的 `createQR` / `buildCheckRequest`；`p_no` 是 `p_*` 排序后的 SHA-256
 * （他叫 `pNo`）。
 */
internal object DouyinQr {
    /** 平台标识（模块里只有这一份，见 [DOUYIN]）。 */
    private val P: Platform = DOUYIN

    /** 二维码生成（GET）。 */
    const val GET_QR_PATH = "/passport/web/get_qrcode/"

    /** 轮询（POST，`token` 在表单 body 里）。 */
    const val CHECK_QR_PATH = "/passport/web/check_qrconnect/"

    /** 扫码成功后落会话 Cookie 的落地页。 */
    const val NEXT = "https://www.douyin.com"

    /** `ctx.store` 里可以放一份新的 passport 版本号（照 `douyin.abogus_profile` 的做法）。 */
    const val KEY = "douyin.passport_profile"

    /**
     * passport SDK 那几个**随页面版本变**的常量。随包一份（`passport_profile.json`），`ctx.store` 的 [KEY] 能覆盖。
     *
     * MIT），
     * **不是我们自己抓的**，也没有在线验证过；版本对不上时服务端会回 `error_code=4031`
     * （cv-cat 的注释："passport SDK 版本对不上也是 4031"）。
     */
    @Serializable
    data class Profile(
        val aid: String = "6383",
        val passportJssdkVersion: String = "3.2.0",
        val pUi: String = "2.2.0",
        val pCa: String = "4.0.26",
        val pCaReal: String = "1.0.0.843",
        val pZt: String = "3.3.11",
        val pVer: String = "1.1.3",
        val pBd: String = "1.0.1.20",
    ) {
        companion object {
            private val json = Json { ignoreUnknownKeys = true }

            fun parse(text: String): Profile = json.decodeFromString(serializer(), text)

            /** 随包的版本：`resources/feedhub/source/douyin/passport_profile.json`；读不到就用上面那组默认值。 */
            fun bundled(): Profile {
                val text = Profile::class.java
                    .getResourceAsStream("/feedhub/source/douyin/passport_profile.json")
                    ?.use { it.readBytes().toString(Charsets.UTF_8) }
                    ?: return Profile()
                return runCatching { parse(text) }.getOrElse { Profile() }
            }
        }
    }

    /** 第 1 步的 `data`。 */
    data class Generated(val token: String, val url: String, val expireAtSec: Long?, val frontier: Boolean)

    /** 第 2 步的状态（两种写法都认：字符串 `new/scanned/confirmed/expired`，数字 `1/2/3/4/5`）。 */
    sealed interface Status {
        data object Waiting : Status
        data object Scanned : Status
        data class Confirmed(val redirectUrl: String?) : Status
        data object Expired : Status

        /** `4`：平台把二维码换了（带新 token），这张已经作废。 */
        data object Refreshed : Status
    }

    /** 生成二维码的 query（顺序照 `createQR`）。`p_ca` 这里传 `0`（第一次 get_qrcode 不带 device_fp）。 */
    fun getQrParams(profile: Profile, nowMs: Long, traceId: String, sourceInfo: String, msToken: String?): LinkedHashMap<String, String> {
        val ts = nowMs.toString()
        return linkedMapOf(
            "passport_jssdk_version" to profile.passportJssdkVersion,
            "passport_jssdk_type" to "normal",
            "is_from_ttaccountsdk" to "1",
            "aid" to profile.aid,
            "language" to "zh",
            "account_app_language" to "zh-CN",
            "next" to NEXT,
            "need_short_url" to "true",
            "need_logo" to "false",
            "is_new_login" to "1",
            "is_from_iesaccountsaas" to "1",
            "p_ui" to profile.pUi,
            "account_sdk_source" to "web",
            "account_sdk_source_info" to sourceInfo,
            "p_js_v" to profile.passportJssdkVersion,
            "p_js_t" to "pro",
            "p_zt" to profile.pZt,
            "p_ver" to profile.pVer,
            "p_ver_real" to "0",
            "request_host" to "https%3A%2F%2Fwww.douyin.com",
            "p_bd" to profile.pBd,
            "p_ts" to ts,
            "p_no" to pNo(profile, ts, "0"),
            "biz_trace_id" to traceId,
        ).also { if (msToken != null) it["msToken"] = msToken }
    }

    /** 轮询的 query（顺序照 `buildCheckRequest`）：比生成多 `p_ca` / `p_ca_real` / `fp` / `verifyFp`，`p_no` 用真正的 `p_ca`。 */
    fun checkQuery(
        profile: Profile,
        nowMs: Long,
        traceId: String,
        sourceInfo: String,
        frontier: Boolean,
        verifyFp: String,
        msToken: String?,
    ): LinkedHashMap<String, String> {
        val ts = nowMs.toString()
        return linkedMapOf(
            "passport_jssdk_version" to profile.passportJssdkVersion,
            "passport_jssdk_type" to "normal",
            "is_from_ttaccountsdk" to "1",
            "aid" to profile.aid,
            "language" to "zh",
            "account_app_language" to "zh-CN",
            "is_from_iesaccountsaas" to "1",
            "p_ui" to profile.pUi,
            "p_ca" to profile.pCa,
            "p_ca_real" to profile.pCaReal,
            "fp" to verifyFp,
            "verifyFp" to verifyFp,
            "account_sdk_source" to "web",
            "account_sdk_source_info" to sourceInfo,
            "p_js_v" to profile.passportJssdkVersion,
            "p_js_t" to "pro",
            "p_zt" to profile.pZt,
            "p_ver" to profile.pVer,
            "p_ver_real" to "0",
            "request_host" to "https%3A%2F%2Fwww.douyin.com",
            "p_bd" to profile.pBd,
            "p_ts" to ts,
            "p_no" to pNo(profile, ts, profile.pCa),
            "biz_trace_id" to traceId,
            "is_new_login" to "1",
        ).also { if (msToken != null) it["msToken"] = msToken }
    }

    /** 轮询的表单 body（`token` 在这里，不在 query 里）。 */
    fun checkForm(token: String): LinkedHashMap<String, String> = linkedMapOf(
        "need_logo" to "false",
        "is_frontier" to "true",
        "token" to token,
        "is_new_login" to "1",
        "next" to NEXT,
        "need_short_url" to "true",
    )

    /**
     * `p_no`：`p_*` 那几项按**键名排序**拼成 `k=v&…` 再 SHA-256（小写十六进制）。
     * 排序后的顺序是 `p_bd, p_ca, p_ts, p_ver, p_zt, passport_jssdk_version`（`'_'` 比 `'a'` 小）。
     */
    fun pNo(profile: Profile, ts: String, pca: String): String {
        val values = sortedMapOf(
            "p_bd" to profile.pBd,
            "p_ca" to pca,
            "passport_jssdk_version" to profile.passportJssdkVersion,
            "p_ts" to ts,
            "p_ver" to profile.pVer,
            "p_zt" to profile.pZt,
        )
        val text = values.entries.joinToString("&") { (k, v) -> "$k=$v" }
        return MessageDigest.getInstance("SHA-256").digest(text.toByteArray(Charsets.UTF_8)).toHex()
    }

    /**
     * `account_sdk_source_info`：把一段描述浏览器的 JSON **逐字节 XOR 5 再转 hex**（`createQR` 里就是这么来的）。
     *
     * 只放 [DouyinDevice] 里**真有出处**的值（窗口 / 屏幕 / 核数 / 时间原点）；参考实现里的
     * `webgl.vendor` / `plugins: 5` / `navigationTiming` / `browser.t` 是它自己编的，这里不写。
     * 服务端如果非要那几个字段，不补编。
     */
    fun sourceInfo(device: DouyinDevice, nowMs: Long): String {
        val json = buildString {
            append("{\"hardwareConcurrency\":").append(device.cpuCoreNum)
            append(",\"webdriver\":false,\"chromedriver\":false,\"shelldriver\":false")
            append(",\"plugins\":0")
            append(",\"innerHeight\":").append(device.windowHeight)
            append(",\"innerWidth\":").append(device.windowWidth)
            append(",\"outerHeight\":").append(device.windowHeight)
            append(",\"outerWidth\":").append(device.windowWidth)
            append(",\"performance\":{\"timeOrigin\":").append(nowMs - 500).append("}}")
        }
        val bytes = json.toByteArray(Charsets.UTF_8)
        for (i in bytes.indices) bytes[i] = (bytes[i].toInt() xor 5).toByte()
        return bytes.toHex()
    }

    /** 第 1 步的 `data`：`token` / `qrcode_index_url` / `expire_time`（Unix 秒）/ `is_frontier`。 */
    fun generated(json: JsonObject): Generated {
        val d = data(json)
        val token = d.text("token")?.takeIf { it.isNotBlank() } ?: throw ParseException(P, "二维码响应缺少 token")
        val url = d.text("qrcode_index_url")?.takeIf { it.isNotBlank() }
            ?: throw ParseException(P, "二维码响应里只有图片、没有可编码的链接（qrcode_index_url），扫不了")
        return Generated(
            token = token,
            url = url,
            expireAtSec = d.long("expire_time")?.takeIf { it > 0 },
            frontier = d.bool("is_frontier") ?: true,
        )
    }

    /** 第 2 步的状态。 */
    fun status(json: JsonObject): Status {
        val d = data(json)
        val raw = (d["status"] as? JsonPrimitive)?.content
            ?: throw ParseException(P, "二维码状态响应里没有 status")
        return when (raw) {
            "new", "1" -> Status.Waiting
            "scanned", "2" -> Status.Scanned
            "confirmed", "3" -> Status.Confirmed(d.text("redirect_url"))
            "expired", "5" -> Status.Expired
            "4", "refreshed" -> Status.Refreshed
            else -> throw ParseException(P, "二维码状态是没见过的值（$raw）")
        }
    }

    /**
     * passport 的外壳：`{message:"success", data:{error_code:0, …}}`。
     *
     * 业务码映射：
     * - `7`：轮询限频（"访问太频繁"）→ [RiskControlException]，**不重试**（参考实现是退避重试，我们的规矩是停）；
     * - `2046`：要短信二次验证（`account_flow=verify`）→ [RiskControlException]，这里只做扫码，不做短信；
     * - `4031`：passport 参数 / SDK 版本对不上（多半也含"要 acrawler 签名"）→ [ApiException]，说明原因；
     * - `2156`：会话缺 verifycenter 的信任 Cookie→ [ApiException]。
     */
    fun data(json: JsonObject): JsonObject {
        val d = json.obj("data") ?: throw ParseException(P, "passport 响应里没有 data")
        val code = d.long("error_code") ?: 0L
        if (code == 0L) return d
        val description = d.text("description")?.take(120).orEmpty()
        return when (code) {
            7L -> throw RiskControlException(P, "7", "抖音说访问太频繁（轮询限频），按规矩停下，过几分钟再试")
            2046L -> throw RiskControlException(P, "2046", "抖音要求在手机上做短信二次验证（这里只做扫码），按规矩停下")
            8L -> throw NotLoggedInException(P)
            4031L -> throw ApiException(
                P, "4031",
                "passport 拒绝了这次扫码请求（参数 / SDK 版本对不上，或者要页面脚本的人机校验）" + if (description.isEmpty()) "" else "：$description",
            )
            2156L -> throw ApiException(P, "2156", "抖音说系统繁忙（会话缺少 verifycenter 的信任 Cookie）：$description")
            else -> throw ApiException(P, code.toString(), description.ifEmpty { "passport 业务码 $code" })
        }
    }

    private fun ByteArray.toHex(): String = buildString(size * 2) {
        for (b in this@toHex) {
            append("0123456789abcdef"[(b.toInt() shr 4) and 15])
            append("0123456789abcdef"[b.toInt() and 15])
        }
    }

}
