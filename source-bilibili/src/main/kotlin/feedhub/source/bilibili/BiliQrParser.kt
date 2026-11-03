package feedhub.source.bilibili

import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.QrLoginState
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.text
import kotlinx.serialization.json.JsonObject
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/**
 * B站网页扫码登录（`passport.bilibili.com/x/passport-login/web/qrcode/{generate,poll}`）的响应解析，纯函数。
 *
 * 接口和状态码来自公开的 bilibili-API-collect 文档，参考实现 keluokeda/Bilibili 的 `BilibiliApi.kt`
 * （`generateQrcode` / `pollQrcode`）和 jay3-yy/BiliPai 的 `ApiClient.kt`（`PassportApi.generateQrCode` / `pollQrCode`），
 * 86090 和成功 0 要用户真的扫码才能见到）。
 */
internal object BiliQrParser {
    /** 二维码的内容（B站 App 扫的 URL）和轮询用的 key。 */
    data class Generated(val url: String, val key: String)

    /** `poll` 响应里 `data` 的内容。 */
    data class Poll(val code: Long, val message: String, val url: String?)

    /** `generate`：外壳的 code 已经由调用方检查过。 */
    fun generated(json: JsonObject): Generated {
        val data = json.obj("data") ?: throw ParseException(BILIBILI, "二维码响应缺少 data")
        return Generated(
            url = data.text("url") ?: throw ParseException(BILIBILI, "二维码响应缺少 url"),
            key = data.text("qrcode_key") ?: throw ParseException(BILIBILI, "二维码响应缺少 qrcode_key"),
        )
    }

    fun poll(json: JsonObject): Poll {
        val data = json.obj("data") ?: throw ParseException(BILIBILI, "轮询响应缺少 data")
        val code = data.long("code") ?: throw ParseException(BILIBILI, "轮询响应缺少 data.code")
        return Poll(code, data.text("message").orEmpty(), data.text("url"))
    }

    /** 轮询状态码 → 状态。成功（0）由调用方单独处理（要写 Cookie）。 */
    fun waiting(code: Long): QrLoginState? = when (code) {
        86101L -> QrLoginState.Waiting
        86090L -> QrLoginState.Scanned
        86038L -> QrLoginState.Expired
        else -> null
    }

    /**
     * 成功时 `data.url` 是个跨域登录跳转地址：`…/crossDomain?DedeUserID=…&DedeUserID__ckMd5=…&Expires=…&SESSDATA=…&bili_jct=…&gourl=…`，
     * 登录 Cookie 的值都在 query 里（取**未解码**的原样，和 Set-Cookie 里的写法一样，例如 SESSDATA 里的逗号是 `%2C`）。
     * 正常情况 Cookie 由 Set-Cookie 种进临时罐，这里只在罐里没有 SESSDATA 时当后备。
     */
    fun cookiesFromUrl(url: String?, nowMs: Long): List<Cookie> {
        val query = url?.toHttpUrlOrNull()?.encodedQuery ?: return emptyList()
        val params = query.split('&').mapNotNull { p ->
            val i = p.indexOf('=')
            if (i <= 0) null else p.substring(0, i) to p.substring(i + 1)
        }.toMap()
        val expiresAt = params["Expires"]?.toLongOrNull()?.let { it * 1000 }?.takeIf { it > nowMs } ?: (nowMs + DEFAULT_TTL_MS)
        return LOGIN_COOKIES.mapNotNull { name ->
            val value = params[name]?.takeIf { it.isNotEmpty() } ?: return@mapNotNull null
            Cookie.Builder().name(name).value(value).domain("bilibili.com").path("/").expiresAt(expiresAt)
                .apply { if (name == "SESSDATA") httpOnly() }.secure().build()
        }
    }

    /** 登录 Cookie 的名字（照浏览器登录后的 Cookie 和 `crossDomain` 的参数）。 */
    val LOGIN_COOKIES = listOf("SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMd5")

    /** 换账号时要先清掉的旧登录 Cookie：上面四个加 `sid`；buvid3 / buvid4 / b_nut 这些设备标识不动。 */
    val OLD_LOGIN_NAMES = (LOGIN_COOKIES + "sid").toSet()

    private const val DEFAULT_TTL_MS = 180L * 24 * 3600 * 1000
}
