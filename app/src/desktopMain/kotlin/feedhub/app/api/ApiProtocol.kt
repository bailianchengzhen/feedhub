package feedhub.app.api

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.NotLoggedInException
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.UnsupportedFeedException
import feedhub.rust.RustAppApi
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * 桌面 App 对外接口的数据模型。
 *
 * 每行都带 `schema` / `type`，`data` 就是现有的 `FeedItem` / `FeedDetail` / `Comment` 序列化。
 * 这里再写一遍（而不是 import `:cli`）是故意的：`cli` 是仓库里的开发工具，**App 不依赖它**
 */

/**
 * 输出格式的版本。
 *
 * **值和错误码表都在 Rust**：这是 agent 按
 * `error` 分支的稳定接口，只能有一份（`api_error_codes` / `api_classify`）。Kotlin 这边只保留 enum
 * 的**名字**（代码里到处按它分支），字段值全部从 Rust 读。
 */
val SCHEMA_VERSION: Int get() = RustAppApi.schemaVersion

/**
 * 错误码 → HTTP 状态码。**错误码是稳定的接口**。
 *
 * 和 cli 那张表一样，只是多了两种"服务器层面"的情况（cli 是每次一个进程，遇不到）：
 * BUSY（同时在跑的调用太多）和 INTERNAL 的 500。**没有 `cooldown`**：接口没有自己的冷却，
 * 风控由 App 自己按它的规矩处理（遇到风控就 `risk_control`，不重试）。
 *
 * **码 / 状态码 / retryable 三个字段的值从 Rust 读**（[RustAppApi.errorCodes]），enum 只是名字。
 */
enum class ApiError {
    /** 参数不对（缺参数、平台不认识、值不合法）。400。 */
    BAD_INPUT,

    /** 没登录 / 登录失效。401。 */
    NOT_LOGGED_IN,

    /** 风控信号（403 / 412 / 429、平台的风控错误码）。**别重试**。403。 */
    RISK_CONTROL,

    /** 平台要人机验证，而接口这一侧不能替用户做（验证只能用户本人在 App 里做）。409。 */
    CAPTCHA_REQUIRED,

    /** 境外平台没配可用代理（fail-closed），**一个请求都没发**。503。 */
    PROXY_UNAVAILABLE,

    /** 平台 / 插件没这个能力（没接搜索、没有原生详情、没装这个插件），或者这个路径不存在。404。 */
    NOT_SUPPORTED,

    /** **这条内容 / 这张图不存在**（`/image` 用）：`idx` 越界、这条内容没有图、评论里没有这张图、或者这条是 18+。404。 */
    NOT_FOUND,

    /** 网络层失败（连不上 / 超时 / DNS）。504，可重试。 */
    NETWORK,

    /** 平台明确回了错误（接口错误码 / 签名被拒 / 响应解析不了）。502，多半是接口变了。 */
    PLATFORM_ERROR,

    /** 同时在跑的调用到了上限（`ApiServerLimits.MAX_CONCURRENCY`）。503，等会儿再来。 */
    BUSY,

    /** 别的意外（App 的 bug）。500。 */
    INTERNAL,
    ;

    /** 稳定的字符串（`"risk_control"` …）；Rust 的错误码表里按 enum 名找。 */
    val code: String get() = RustAppApi.codeOf(name).code

    /** HTTP 状态码（粗分类）。 */
    val httpStatus: Int get() = RustAppApi.codeOf(name).httpStatus

    /** agent 能不能重试。 */
    val retryable: Boolean get() = RustAppApi.codeOf(name).retryable

    companion object {
        fun ofKey(key: String): ApiError? = entries.firstOrNull { it.code == key }

        /** Rust 给的那个错误码 → 对应的 enum（`code` 字符串是同一个）。 */
        internal fun ofCode(code: RustAppApi.ErrorCode): ApiError =
            entries.firstOrNull { it.code == code.code }
                ?: throw IllegalStateException("Rust 给了个 Kotlin 不认识的错误码：${code.code}")
    }
}

/**
 * 接口里"能说清楚"的失败：错误码 + 一行话 + 哪个平台。
 *
 * 一行 JSON 是 `{"schema":1,"type":"error","error":"risk_control","platform":"xhs","message":"…",
 * "retryable":false}`（和 cli 的 stderr 那一行**同一个形状**），HTTP 状态码按 [ApiError.httpStatus]。
 */
class ApiFailure(
    val error: ApiError,
    message: String,
    val platform: Platform? = null,
    val retryable: Boolean = error.retryable,
    /** 附加字段（例如风控的 `code`），合进那一行 JSON。 */
    val extra: Map<String, JsonElement> = emptyMap(),
) : Exception(message)

/** 一行 JSON（`schema` / `type` / `data` / 附加字段），字段顺序和 cli 一样。 */
internal fun jsonLine(type: String, data: JsonElement? = null, extra: Map<String, JsonElement> = emptyMap()): String =
    buildJsonObject {
        put("schema", JsonPrimitive(SCHEMA_VERSION))
        put("type", JsonPrimitive(type))
        if (data != null) put("data", data)
        extra.forEach { (k, v) -> put(k, v) }
    }.toString()

/**
 * 错误那一行（和上面 [ApiFailure] 的形状一致）。
 *
 * [requests] = **这次调用自己发起了几个平台请求**（`feedhub.core.RequestScope`）。失败的那次也要算：
 * 实测里两条 502 各发了 1 个请求，但当时的错误行里没有这个字段，
 * agent 没法核预算。现在错误行和 `done` 行都带它，`null` 只留给"根本不在调用里"的内部路径。
 */
internal fun ApiFailure.toJsonLine(requests: Int? = null): String {
    val extraJson = if (extra.isEmpty()) {
        null
    } else {
        FeedJson.encodeToString(kotlinx.serialization.json.JsonObject.serializer(), kotlinx.serialization.json.JsonObject(extra))
    }
    return RustAppApi.renderErrorLine(
        code = error.code,
        platform = platform?.key,
        message = message.orEmpty(),
        retryable = retryable,
        extraJson = extraJson,
        requests = requests,
    )
}

internal fun itemJson(item: FeedItem): JsonElement = FeedJson.encodeToJsonElement(FeedItem.serializer(), item)

internal fun commentJson(comment: Comment): JsonElement = FeedJson.encodeToJsonElement(Comment.serializer(), comment)

internal fun jsonArrayOfStrings(values: Iterable<String>): JsonElement = buildJsonArray { values.forEach { add(it) } }

/**
 * 把异常归类成 [ApiFailure]：和 cli 的 `Throwable.toCliFailure()` 同一套映射（同一个错误码表），
 * 只是没有 clikt 那几种用法错误、也没有 cli 自己的冷却。
 */
/**
 * 把异常归类成 [ApiFailure]：**"异常 → 种类"在这里，"种类 → 错误码"在 Rust**
 * （[RustAppApi.classify]，原 `Throwable.toApiFailure()` 的那张表）。
 *
 * 这样"签名错 / 接口错 / 解析错都归 `platform_error`"这种口径只有一份，而 Kotlin 只负责认出异常类型。
 */
internal fun Throwable.toApiFailure(): ApiFailure {
    val kind = when (this) {
        is ApiFailure -> return this
        // 要人机验证：接口这一侧不替用户做，回 `captcha_required`（409），文档里承诺的那个码
        is RiskControlException ->
            if (code.equals("captcha", ignoreCase = true)) RustAppApi.FailureKind.CAPTCHA_REQUIRED
            else RustAppApi.FailureKind.RISK_CONTROL
        is NotLoggedInException -> RustAppApi.FailureKind.NOT_LOGGED_IN
        is UnsupportedFeedException -> RustAppApi.FailureKind.NOT_SUPPORTED
        is SignatureException, is ApiException, is ParseException -> RustAppApi.FailureKind.PLATFORM_ERROR
        is java.io.IOException -> RustAppApi.FailureKind.NETWORK
        else -> RustAppApi.FailureKind.INTERNAL
    }
    val code = ApiError.ofCode(RustAppApi.classify(kind))
    val message = when (this) {
        is RiskControlException -> readableMessage()
        is NotLoggedInException, is UnsupportedFeedException, is SignatureException, is ApiException, is ParseException ->
            readableMessage()
        is java.io.IOException -> "网络错误：${readableMessage()}"
        else -> "${this::class.simpleName}: ${readableMessage()}"
    }
    // 风控的附加字段：平台给的风控码（`code`）原样带上，agent 能据此判断是哪一类
    val extra = (this as? RiskControlException)?.code
        ?.let { mapOf("code" to JsonPrimitive(it)) }
        ?: emptyMap()
    return ApiFailure(code, message, (this as? feedhub.core.SourceException)?.platform, extra = extra)
}


private fun Throwable.readableMessage(): String = message ?: this::class.simpleName.orEmpty()
