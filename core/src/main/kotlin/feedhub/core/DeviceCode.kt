package feedhub.core

import kotlinx.serialization.Serializable

/**
 * 设备码登录。
 * 现在只有 YouTube 用它（TVHTML5 的电视设备码）：用户在一台设备上拿到 `user_code`，去
 * [DeviceCodeLogin.verificationUrl] 输入，我们在另一头按 [DeviceCodeLogin.pollIntervalMs] 轮询换 token。
 *
 * 和 [QrLoginSource] 的分工：扫码是"拿二维码"，设备码是"拿短码 + 一个网址"；两者都是"用户本人在别处完成"。
 *
 * 约定（照 [QrLoginSource]）：
 * - **成功之前不动现有的登录态**；成功那一刻 Source 自己把 token 写进 `ctx.store`。
 * - 轮询接口的 `authorization_pending`（还没输码）→ [DeviceCodeLoginState.Waiting]，
 *   `slow_down` → [DeviceCodeLoginState.Waiting]（Source 自己把间隔调大），不要自动重试网络错误。
 * - 短码过期 → [DeviceCodeLoginState.Expired]，用户取消 → [DeviceCodeLoginState.Failed]。
 */
interface DeviceCodeLoginSource : Source {
    /** 取一组设备码。返回的 [DeviceCodeLogin.deviceCode] 是机密，别打印。 */
    suspend fun startDeviceCodeLogin(): DeviceCodeLogin

    /** 轮询一次换 token；换到就存起来并返回 [DeviceCodeLoginState.Success]。 */
    suspend fun pollDeviceCodeLogin(login: DeviceCodeLogin): DeviceCodeLoginState

    /** 退出登录：删掉本机存的 token，回到匿名。 */
    fun logout()

    /**
     * App 启动 / 回到前台时调：短命的 access token 过期了就先在后台换好，用户点开内容时不用再等这一跳。
     * 没过期就什么都不发；失败不抛（真用到时还会再换）。
     */
    suspend fun keepSessionFresh() {}
}

@Serializable
data class DeviceCodeLogin(
    val platform: Platform,
    /** 服务端给的 device_code，轮询时回传。**机密**：不要打印 / 落日志。 */
    val deviceCode: String,
    /** 给用户看的短码（`ABCD-1234`）。可以打印。 */
    val userCode: String,
    /** 用户在哪个网址输入短码。 */
    val verificationUrl: String,
    /** 建议轮询间隔（毫秒）。 */
    val pollIntervalMs: Long = 5000,
    /** 短码什么时候过期（Unix 毫秒）。 */
    val expiresAtMs: Long? = null,
    /** 给用户的提示。 */
    val hint: String,
)

@Serializable
sealed interface DeviceCodeLoginState {
    /** 还没输码（`authorization_pending`）。 */
    @Serializable
    data object Waiting : DeviceCodeLoginState

    /** 登录成功，token 已经存好；[user] 是新账号（拿不到就 null，前端再调 whoami）。 */
    @Serializable
    data class Success(val user: UserRef? = null) : DeviceCodeLoginState

    /** 短码过期，要重新取一组。 */
    @Serializable
    data object Expired : DeviceCodeLoginState

    @Serializable
    data class Failed(val message: String) : DeviceCodeLoginState
}
