package feedhub.source.tieba

import feedhub.core.ApiException
import feedhub.core.NotLoggedInException
import feedhub.core.RiskControlException
import feedhub.core.SignatureException

/**
 * 贴吧平台错误码 → 异常的归类。规则**只有这一份**：Rust 平台对象带回来的错误码、搜索 / 关注管理、
 * 对拍用的 Kotlin 参照实现都走它。
 *
 * 贴吧没有公开的错误码表。确定的只有一个：
 * - 110001"未知错误"：sign 不对→ [SignatureException]。
 * 其余按 error_msg 的关键词归类：
 * - 未登录：消息里含"未登录""重新登录"、"登录…失效"、"BDUSS" 的；
 * - 风控：消息里含"频繁""验证""安全""风险""异常""封禁"的；
 * - 签名：消息里含 "sign" / "签名" 的。
 * 风控一律抛 [RiskControlException]，不重试。
 */
internal object TiebaErrors {
    fun mapError(code: Int, msg: String): Exception = when {
        isNotLoggedIn(msg) -> NotLoggedInException(TIEBA, "未登录或登录已失效（$code $msg）")
        RISK_WORDS.any { it in msg } -> RiskControlException(TIEBA, code.toString(), msg)
        code == 110001 || "sign" in msg.lowercase() || "签名" in msg -> SignatureException(TIEBA, "$code $msg")
        else -> ApiException(TIEBA, code.toString(), msg)
    }

    fun isNotLoggedIn(msg: String): Boolean =
        "未登录" in msg || "重新登录" in msg || ("登录" in msg && "失效" in msg) || "bduss" in msg.lowercase()

    val RISK_WORDS = listOf("频繁", "验证", "安全", "风险", "异常", "封禁")
}
