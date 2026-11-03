package feedhub.core

import kotlinx.serialization.Serializable

/**
 * 扫码登录。
 * B站、知乎、贴吧（百度）、小黑盒实现；酷安的网页登录没有扫码，不实现。
 *
 * 流程：[startQrLogin] 拿到二维码内容 → 前端画成二维码 → 每隔 [QrLogin.pollIntervalMs] 调一次 [pollQrLogin]，
 * 直到 [QrLoginState.Success] / [QrLoginState.Expired] / [QrLoginState.Failed]。
 *
 * 约定：
 * - **成功之前不动现有的 Cookie**（扫码中途放弃，原来的登录态还在）；成功时 Source 自己把新登录态写进 `ctx.cookies`，
 *   先清掉这个平台旧的登录 Cookie 再写新的，并顺手做完平台登录后需要的补充步骤（例如百度换 STOKEN）。
 * - 轮询接口如果是长轮询，Source 内部要给它设超时，单次 [pollQrLogin] 不超过 [QrLogin.pollIntervalMs] 太多。
 * - 二维码过期、用户在手机上取消，都给 [QrLoginState.Expired]，前端可以重新开始；不要自动重试。
 */
interface QrLoginSource : Source {
    suspend fun startQrLogin(): QrLogin

    suspend fun pollQrLogin(login: QrLogin): QrLoginState
}

@Serializable
data class QrLogin(
    val platform: Platform,
    /** 二维码里编码的内容（通常是一个 URL），前端自己画成二维码。 */
    val content: String,
    /** 平台用来查询状态的 key（B站 qrcode_key、知乎 token、百度 sign / channel_id……），Source 自己解释。 */
    val token: String,
    /** 平台另外需要记住的东西（例如百度轮询要的参数），Source 自己解释。 */
    val extra: Map<String, String> = emptyMap(),
    /** 什么时候过期（Unix 毫秒），不知道就 null。 */
    val expiresAtMs: Long? = null,
    /** 建议的轮询间隔。 */
    val pollIntervalMs: Long = 1500,
    /** 给用户的提示，例如"用哔哩哔哩 App 扫码"。 */
    val hint: String,
)

@Serializable
sealed interface QrLoginState {
    /** 还没扫。 */
    @Serializable
    data object Waiting : QrLoginState

    /** 扫了，等手机上确认。 */
    @Serializable
    data object Scanned : QrLoginState

    /** 登录成功，Cookie 已经存好；[user] 是新登录的账号（拿不到就 null，前端再调 whoami）。 */
    @Serializable
    data class Success(val user: UserRef? = null) : QrLoginState

    /** 二维码过期或被取消，要重新开始。 */
    @Serializable
    data object Expired : QrLoginState

    @Serializable
    data class Failed(val message: String) : QrLoginState
}
