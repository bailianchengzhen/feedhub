package feedhub.source.coolapk

import feedhub.core.CaptchaChallenge
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.SourceContext
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.net.URLEncoder

/**
 * 酷安人机验证的**共用件**：三种失败文案 + `validate` cookie 怎么写。
 *
 * 验证码本身**只由用户本人完成**（`ctx.captcha` 交给 App / cli 的界面），这里不解、不绕、不循环；
 * 版本号 / 身份参数也一个字不改（风控红线）。
 *
 * 为什么单独一个文件：同一条流程有两份实现——参照实现 [CoolapkSource]（Kotlin 拼请求、自己处理验证码）
 * 和线上实现 [RustCoolapkSource]（Rust 认出要验证、Kotlin 门面接住）。文案和 cookie 规则**只能有一份**，
 * 否则两条路的异常和请求字节会悄悄漂移（`CoolapkRequestEquivalenceTest` 会抓，但没必要让它抓）。
 */
internal object CoolapkCaptcha {
    /** 服务端没下发 `captchaId` 时的兜底（coolapk-desktop 的默认酷安 id，和 Kotlin 原来那份一样）。 */
    const val DEFAULT_ID = "414e5c9b866a03db03f860a1a9101672"

    /** 登录态 / `validate` cookie 挂的"身份 URL"（和 [CoolapkSource] 里那份一样）。 */
    private const val COOKIE_URL = "https://api2.coolapk.com/"
    private const val COOKIE_DOMAIN = "coolapk.com"

    /**
     * `validate` cookie 存多久：**让服务端说了算，我们不要自己先丢**。
     *
     * 服务端提前失效时，下一个评论请求会再回 `err_request_captcha_v2`，照常弹一次验证、再写一份新的；
     * 所以留长一点没有坏处（多带一个服务端不认的 cookie，最坏就是再弹一次，和没有它一样）。反过来，
     * 自己设一个比服务端短的上限，等于**每隔这么久白弹一次验证**。
     *
     * 是拍的、没有在线证据；
     */
    private const val VALIDATE_TTL_MS = 30L * 24 * 60 * 60 * 1000

    /** 交给 `ctx.captcha` 的那份挑战（`captchaId` / `field` / 服务端原话都由 Rust 或 Kotlin 解析好）。 */
    fun challenge(platform: Platform, captchaId: String, field: String, message: String) =
        CaptchaChallenge(platform, captchaId, field, message)

    /** 没有 handler：说清楚为什么读不了评论（不退回 `hotReplyList`、不静默给半份）。 */
    fun noHandler(platform: Platform, message: String) = RiskControlException(
        platform,
        "captcha",
        "酷安要求人机验证（$message），但这次没有可用的验证方式",
    )

    /** 用户取消了验证：说清楚"再点评论会再弹一次"，不自动重试。 */
    fun cancelled(platform: Platform, message: String) = RiskControlException(
        platform,
        "captcha",
        "已取消酷安人机验证（$message）；重试评论会再弹一次",
    )

    /** 验证过了、服务端还是要验证：只重试了一次，不循环。 */
    fun stillRequired(platform: Platform, message: String) = RiskControlException(
        platform,
        "captcha",
        "酷安仍然要求人机验证（$message）；这次验证没有放行，请重试",
    )

    /**
     * 把验证结果写成 `.coolapk.com` 的 `validate` cookie（URL 编码，和 ZaiLinn/c001apk 的做法一致）。
     * 挑战响应同时下的 `SESSID` 已经在共用 Cookie 罐里，重试会带上同一个会话。
     */
    fun storeValidate(ctx: SourceContext, proof: String) {
        val value = URLEncoder.encode(proof, Charsets.UTF_8.name())
        val cookie = Cookie.Builder()
            .name("validate")
            .value(value)
            .domain(COOKIE_DOMAIN)
            .path("/")
            .secure()
            .expiresAt(System.currentTimeMillis() + VALIDATE_TTL_MS)
            .build()
        ctx.cookies.put(COOKIE_URL.toHttpUrl(), listOf(cookie))
    }
}
