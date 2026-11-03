package feedhub.core

/**
 * 平台在**读**接口上要人机验证时给出的信息。
 *
 * 这是给上层（App / cli）的接口：哪个平台、用哪个验证码 id、验证结果怎么回传。
 * Source 自己不实现验证码，也不解验证码——用户本人在官方验证码页面里完成，Source 只负责
 * 把拿到的 proof 写回请求并在同一会话里重试一次。
 */
data class CaptchaChallenge(
    val platform: Platform,
    /** 服务端下发的验证码 id（酷安是网易易盾的 NEC captchaId）。 */
    val captchaId: String,
    /**
     * 空字符串 = proof 放在 `validate` cookie 里（酷安 `replyList` 这种 GET 接口）；
     * 非空 = 作为这个查询 / 表单参数回传（写接口常见）。
     */
    val field: String,
    /** 服务端原话，界面可以直接显示。 */
    val message: String,
) {
    /** 酷安 / 易盾的 proof 形状：`NEC:<captchaId 前 8 位>:<validate>`。 */
    val proofPrefix: String get() = "NEC:${captchaId.take(8)}:"
}

/**
 * 人去完成验证码这一步的入口。App 用内嵌 WebView / 系统浏览器实现，cli 用终端提示实现；
 * 用户取消就返回 null（Source 会抛一条说得清的 [RiskControlException]，不重试、不循环）。
 *
 * **实现方不要把 proof 打进日志或错误信息**：它和登录凭据一样是短时凭据。
 */
interface CaptchaHandler {
    suspend fun solve(challenge: CaptchaChallenge): String?

    /** 用户点了「取消」（或关掉了验证界面）：让 [solve] 尽快返回 null。cli 没有这个动作，默认空实现。 */
    fun cancel() {}

    /**
     * 内嵌浏览器起不来 / 下载失败时的退路：用系统浏览器打开同一个验证页（页面仍会把结果回传给同一个
     * 本机服务）。默认没有退路，只提示错误。
     */
    fun openInSystemBrowser() {}
}
