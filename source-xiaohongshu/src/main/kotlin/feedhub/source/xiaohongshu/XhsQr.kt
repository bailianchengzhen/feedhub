package feedhub.source.xiaohongshu

import feedhub.core.ParseException
import feedhub.core.int
import feedhub.core.obj
import feedhub.core.text
import kotlinx.serialization.json.JsonObject

/**
 * 小红书网页扫码登录的响应解析。
 * catbus（MIT）的 `src/platforms/xhs/web/login.ts`
 * 是同一作者的同一流程）：纯函数，不发请求。
 *
 * 三步（全部在 `edith.xiaohongshu.com`，和小红书的其它接口同一个 host、同一套 [XhsSigner] 签名）：
 * 1. `POST /api/sns/web/v1/login/qrcode/create`，body `{"qr_type":1}` → `data {qr_id, code, url}`；
 *    `url` 是小红书 App 扫的内容（`https://www.xiaohongshu.com/...`），`qr_id` + `code` 是轮询用的。
 * 2. `POST /api/qrcode/userinfo`，body `{"qrId":…,"code":…}` → `data.codeStatus`（0 未扫 / 1 已扫待确认 / 2 已确认 / 3 过期）。
 * 3. 确认后 `GET /api/sns/web/v1/login/qrcode/status?qr_id=…&code=…`（**另加一个空的 `x-login-mode` 头**）→
 *    `data.code_status` 必须是 2，正式会话在 `data.login_info.session`（就是 `web_session`）。
 *
 * 为什么第 3 步不能省：游客态本来就有 `a1` / `web_session`，
 * 只有拿到 `login_info.session` 才算真的换成了登录态。
 */
internal object XhsQr {
    const val CREATE = "/api/sns/web/v1/login/qrcode/create"
    const val USERINFO = "/api/qrcode/userinfo"
    const val STATUS = "/api/sns/web/v1/login/qrcode/status"

    /** [check] 的 `data.codeStatus`。 */
    const val WAITING = 0
    const val SCANNED = 1
    const val CONFIRMED = 2
    const val EXPIRED = 3

    /** [status] 响应里的 `data.code_status`（和下面对数字的字段名不一样，照实抓）。 */
    const val LOGIN_OK = 2

    /** `QrLogin.extra` 里存二维码 `code` 用的键。 */
    const val CODE = "code"

    /** 二维码响应 `data`：`qr_id` / `code` / `url`。 */
    data class Generated(val qrId: String, val code: String, val url: String)

    /** 第 1 步。字段缺一个就没法轮询，直接报错（错误信息里不带响应内容）。 */
    fun generated(json: JsonObject): Generated {
        val d = json.obj("data") ?: throw ParseException(XHS, "二维码接口的响应里没有 data")
        return Generated(
            qrId = d.text("qr_id")?.takeIf { it.isNotBlank() } ?: throw ParseException(XHS, "二维码响应缺少 qr_id"),
            code = d.text("code")?.takeIf { it.isNotBlank() } ?: throw ParseException(XHS, "二维码响应缺少 code"),
            url = d.text("url")?.takeIf { it.isNotBlank() } ?: throw ParseException(XHS, "二维码响应缺少 url"),
        )
    }

    /** 第 2 步：`data.codeStatus`；没有这个字段返回 null（由调用方当失败处理）。 */
    fun status(json: JsonObject): Int? = json.obj("data")?.int("codeStatus")

    /** 第 2 步的请求体（键序照网页：`qrId` 在前）。 */
    fun checkBody(qrId: String, code: String): String =
        "{\"qrId\":${XhsCodec.jsonString(qrId)},\"code\":${XhsCodec.jsonString(code)}}"

    /** 第 3 步的结果。 */
    data class Login(val confirmed: Boolean, val session: String?)

    /**
     * 第 3 步：`data.code_status` 是 2 才算成功，正式会话在 `data.login_info.session`
     * （老版本 / 灰度里可能只在响应的 Set-Cookie 里，那时 [session] 是 null，由调用方看罐子）。
     */
    fun login(json: JsonObject): Login {
        val d = json.obj("data") ?: return Login(false, null)
        val confirmed = d.int("code_status") == LOGIN_OK
        val session = d.obj("login_info")?.text("session")?.takeIf { it.isNotBlank() }
        return Login(confirmed, session)
    }
}
