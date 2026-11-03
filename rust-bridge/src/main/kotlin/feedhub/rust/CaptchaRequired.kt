package feedhub.rust

import feedhub.core.Platform
import feedhub.core.SourceException

/**
 * **平台在读接口上要人机验证**（`FfiError::CaptchaRequired`）。目前只有酷安的评论 / 楼中楼会这样：
 * `replyList` 第一页回 `err_request_captcha_v2`，Rust 认出之后**不在那边弹、也不重试**，而是把这个异常
 * 带过边界，由平台自己的 Kotlin 门面（`RustCoolapkSource`）照 Kotlin 原逻辑让**用户本人**验证一次、
 * 再重试同一个请求一次。**不循环、不自动绕，也不改版本号 / 身份参数**（风控红线）。
 *
 * 这些字段就是 Kotlin `CaptchaChallenge` 需要的那三样（+ 服务端原话），由 Rust 从
 * `messageExtra` 里取好（`captchaId` / `captchaField` 的兜底规则和 Kotlin 版一字不差）：
 * - [captchaId]：服务端下发的验证码 id（没下发时是酷安网页那版默认 id）；
 * - [field]：空 = 答案写进 `validate` cookie；非空 = 作为这个查询参数回传；
 * - [serverMessage]：服务端原话（`当前访问需要验证码`），界面可以直接显示。
 *
 * **它不是 [feedhub.core.RiskControlException]**：风控那条是"不许重试"，这条是"用户验证完可以重试一次"。
 * 平台门面接住它之后要么重试、要么翻成风控错（没有 handler / 用户取消 / 验证仍不过）。
 */
class CaptchaRequiredException(
    platform: Platform,
    val captchaId: String,
    val field: String,
    val serverMessage: String,
) : SourceException(platform, "需要人机验证：$serverMessage")
