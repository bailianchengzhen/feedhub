package feedhub.core

/**
 * **fail-closed**：这个平台要走代理，但这次没有可用的代理（设置里是「不用」/ 地址没填全 / 内置核心没在跑）
 * ——**一个字节都没发出去**。
 *
 * 是 [java.io.IOException] 的子类（现有约定：网络错误不包装成 [SourceException]），但**类型要留住**：
 * cli 靠它给 `proxy_unavailable`（退出码 6）；Rust 平台层靠它把"代理不可用"和"连不上 / 超时"
 * 分开带过 FFI 边界（`feedhub-ffi` 的 `FfiHostError::ProxyUnavailable`）。
 */
open class ProxyUnavailableException(message: String) : java.io.IOException(message)

/** Source 抛出的所有业务错误的基类。网络层错误（IOException）不包装，原样抛。 */
open class SourceException(
    val platform: Platform,
    message: String,
    cause: Throwable? = null,
) : Exception("[${platform.label}] $message", cause)

/** 没登录或登录已失效，需要重新登录。 */
class NotLoggedInException(platform: Platform, message: String = "未登录或登录已失效") :
    SourceException(platform, message)

/** 被风控拦了：验证码、请求被拦截、账号异常等。不要自动重试。 */
class RiskControlException(platform: Platform, val code: String?, message: String) :
    SourceException(platform, "风控：$message" + (code?.let { "（$it）" } ?: ""))

/** 签名被拒，多半是算法或版本号过期了。 */
class SignatureException(platform: Platform, message: String) :
    SourceException(platform, "签名被拒：$message")

/** 平台返回了其他错误码。 */
class ApiException(platform: Platform, val code: String, message: String) :
    SourceException(platform, "接口错误 $code：$message")

/** 响应结构和预期不符，多半是接口改了。 */
class ParseException(platform: Platform, message: String, cause: Throwable? = null) :
    SourceException(platform, "解析失败：$message", cause)

class UnsupportedFeedException(platform: Platform, kind: FeedKind) :
    SourceException(platform, "不支持${kind.label}流")
