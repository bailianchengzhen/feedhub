package feedhub.cli

import feedhub.core.CaptchaChallenge
import feedhub.core.CaptchaHandler
import feedhub.core.Platform
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import java.io.PrintStream
import java.util.concurrent.atomic.AtomicInteger

/**
 * 给 agent 用的 JSON 输出的 schema 版本。
 *
 * 每行都带 `schema` / `type`，
 * 载荷（`data`）就是 `FeedItem` / `FeedDetail` / `Comment` 现有的序列化，没有另起一套模型。
 */
const val SCHEMA_VERSION: Int = 1

/** agent 模式一次运行的请求上限。 */
const val DEFAULT_MAX_REQUESTS: Int = 20

/**
 * [Session] 在 clikt context 里的键。**必须显式给键**：`requireObject<T>()` 的默认键是 `"default_object"`，
 * 和 [Env] 那个默认键会撞，两个对象只能留一个（踩过：`requireObject<Session>` 拿到 null）。
 */
const val SESSION_KEY: String = "feedhub.cli.session"

/**
 * 错误码 → 进程退出码。**错误码是稳定的接口**，退出码只在这张表里定一次。
 *
 * `retryable` 表示"过一会儿原样重跑可能成功"：网络 / 冷却算；风控、验证码、没登录、没代理、参数不对都不算
 * （要人先改点什么）。
 */
enum class ErrorCode(val key: String, val exitCode: Int, val retryable: Boolean) {
    /** 参数 / 用法不对，或者 agent 模式下被拒绝的写操作 / 请求上限。退出码 2。 */
    BAD_INPUT("bad_input", 2, false),

    /** 没登录或登录失效。退出码 3。 */
    NOT_LOGGED_IN("not_logged_in", 3, false),

    /** 风控信号（403 / 412 / 429、平台的风控错误码）。已经写冷却，**不要重试**。退出码 4。 */
    RISK_CONTROL("risk_control", 4, false),

    /** 平台要人机验证，而这个进程没法让用户本人做（agent 模式 / 非交互）。退出码 5。 */
    CAPTCHA_REQUIRED("captcha_required", 5, false),

    /** 境外平台没配可用的代理（fail-closed），一个请求都没发。退出码 6。 */
    PROXY_UNAVAILABLE("proxy_unavailable", 6, false),

    /** 这个平台正在风控冷却期，**一个请求都没发**。退出码 7，可重试（等冷却过去）。 */
    COOLDOWN("cooldown", 7, true),

    /** 平台 / 插件没有这个能力（没接搜索、没有原生详情……）。退出码 8。 */
    NOT_SUPPORTED("not_supported", 8, false),

    /** 网络层失败（连不上、超时、DNS……）。退出码 9，可重试。 */
    NETWORK("network", 9, true),

    /** 平台明确回了个错误（接口错误码 / 签名被拒 / 响应解析不了）。退出码 10；重试多半没用。 */
    PLATFORM_ERROR("platform_error", 10, false),

    /** 别的意外（cli 自己的 bug）。退出码 1。 */
    INTERNAL("internal", 1, false),
}

/**
 * cli 里"能说清楚"的错误：错误码 + 一行话 + 哪个平台。
 *
 * 命令里不要直接 `CliktError(...)` 了：`CliktError` 只能表达"用法错误"，到了 `main` 里分不出
 * 没登录 / 风控 / 没代理。抛这个，`main` 统一输出（`--json` 时是一行 JSON 到 stderr）和退出码。
 */
class CliFailure(
    val error: ErrorCode,
    message: String,
    val platform: Platform? = null,
    val retryable: Boolean = error.retryable,
    /** 附加字段（例如风控的 `code`、冷却的 `retryAfterMs`），会合进 stderr 那一行 JSON。 */
    val extra: Map<String, JsonElement> = emptyMap(),
) : Exception(message)

/** 一次运行的会话：输出模式、agent 模式、请求预算、冷却 / 限速。 */
class Session(
    var json: Boolean = false,
    var agent: Boolean = false,
    /** 每次运行的请求上限（**只在 agent 模式生效**）；只能往低调。 */
    var maxRequests: Int = DEFAULT_MAX_REQUESTS,
    val out: PrintStream = System.out,
    val err: PrintStream = System.err,
    /** 冷却 / 限速状态的目录（默认 `~/.cache/feedhub/...`；测试给临时目录）。 */
    cooldownDir: java.nio.file.Path = CliPaths.cooldownDir,
    rateDir: java.nio.file.Path = CliPaths.rateDir,
) {
    /** 这次运行已经发出去的请求数（包括各平台内部的中间请求）。 */
    val requests = AtomicInteger()

    /** 交互式验证码处理器记下的"哪个平台要验证"：Source 抛回来时据此报 `captcha_required`。 */
    val captchaNeeded: MutableSet<Platform> = java.util.Collections.synchronizedSet(mutableSetOf())

    /** 风控冷却（`~/.cache/feedhub/cooldown/`，App 不受影响）。 */
    val cooldown: CooldownStore = CooldownStore(cooldownDir)

    /** 跨进程限速（`~/.cache/feedhub/cli-rate/`）。 */
    val rateLimit: CrossProcessRateLimit = CrossProcessRateLimit(rateDir)

    /**
     * 合并一次命令自己声明的开关（`--json` / `--agent` / `--max-requests` 既能在子命令前、
     * 也能在子命令后写）。`--max-requests` **调不高**：比默认值大就按默认值算。
     */
    fun configure(jsonFlag: Boolean, agentFlag: Boolean, maxRequestsFlag: Int?) {
        json = json || jsonFlag
        agent = agent || agentFlag
        val raw = maxRequestsFlag ?: maxRequests
        maxRequests = when {
            raw < 1 -> throw CliFailure(ErrorCode.BAD_INPUT, "--max-requests 要 ≥ 1（收到 $raw）")
            raw > DEFAULT_MAX_REQUESTS -> DEFAULT_MAX_REQUESTS
            else -> raw
        }
    }

    /** agent 模式下拒绝写操作 / 危险命令：一律 `bad_input`，一个请求都不发。 */
    fun rejectInAgentMode(action: String) {
        if (!agent) return
        throw CliFailure(
            ErrorCode.BAD_INPUT,
            "agent 模式（FEEDHUB_AGENT=1 或 --agent）不允许 $action：写操作和 raw 会绕过限速 / 冷却 / 预算，" +
                "需要时由用户本人在不带 agent 模式的情况下执行。",
        )
    }

    // ---------------------------------------------------------------- 输出

    /** 一行 JSON 记录（JSON Lines）。[extra] 的字段和 `type` / `data` 同级。 */
    fun emit(type: String, data: JsonElement? = null, extra: Map<String, JsonElement> = emptyMap()) {
        out.println(jsonLine(type, data, extra))
        out.flush()
    }

    /** 给人看的输出：`--json` 时整行丢掉（不污染 stdout 的 JSON Lines）。 */
    fun say(text: String = "") {
        if (json) return
        out.println(text)
    }

    /** 给人看的提示（`--json` 时丢掉）；写到 stderr，别混进结果。 */
    fun note(text: String) {
        if (json) return
        err.println(text)
    }

    /** stderr 的一行错误（`--json` 时是 JSON；人类模式是那一行字）。 */
    fun reportError(f: CliFailure) {
        if (json) {
            val fields = buildMap {
                put("error", JsonPrimitive(f.error.key))
                f.platform?.let { put("platform", JsonPrimitive(it.key)) }
                put("message", JsonPrimitive(f.message))
                put("retryable", JsonPrimitive(f.retryable))
                putAll(f.extra)
            }
            err.println(
                buildJsonObject {
                    put("schema", JsonPrimitive(SCHEMA_VERSION))
                    put("type", JsonPrimitive("error"))
                    fields.forEach { (k, v) -> put(k, v) }
                }.toString(),
            )
        } else {
            err.println("错误：${f.message}")
        }
        err.flush()
    }

    /**
     * 平台级失败（`search all` / `whoami` 这种一次问好几家的）：记一行错误、写冷却，然后继续问下一家。
     * 返回错误码，调用方据此决定最后整个命令的退出码。
     */
    fun reportPlatformFailure(t: Throwable): ErrorCode {
        val f = classify(t)
        // 风控 / 验证码给这个平台写冷却：冷却期内一个请求都不发（App 不受影响）
        val platform = f.platform
        if (platform != null && (f.error == ErrorCode.RISK_CONTROL || f.error == ErrorCode.CAPTCHA_REQUIRED)) {
            cooldown.mark(platform, f.error.key)
        }
        reportError(f)
        return f.error
    }

    /**
     * 把异常归类好：风控错误 + 这次运行里验证码处理器记下过这个平台 → `captcha_required`
     * （用户本人才能做验证，agent 模式里不该假装是普通风控）。
     */
    fun classify(t: Throwable): CliFailure {
        val f = t.toCliFailure()
        val p = f.platform
        return if (f.error == ErrorCode.RISK_CONTROL && p != null && p in captchaNeeded) {
            CliFailure(ErrorCode.CAPTCHA_REQUIRED, f.message.orEmpty(), p, f.retryable, f.extra)
        } else {
            f
        }
    }
}

/** 一行 JSON 的拼装（`schema` / `type` / `data` / 附加字段）。 */
fun jsonLine(type: String, data: JsonElement? = null, extra: Map<String, JsonElement> = emptyMap()): String =
    buildJsonObject {
        put("schema", JsonPrimitive(SCHEMA_VERSION))
        put("type", JsonPrimitive(type))
        if (data != null) put("data", data)
        extra.forEach { (k, v) -> put(k, v) }
    }.toString()

fun jsonArray(values: Iterable<String>): JsonElement = buildJsonArray { values.forEach { add(it) } }

/** 一次运行的请求预算（agent 模式）。发请求前 [take]；超了直接 `bad_input`，不再发。 */
class RequestBudget(private val limit: Int) {
    private val used = AtomicInteger()

    fun take() {
        val n = used.incrementAndGet()
        if (n > limit) {
            throw CliFailure(
                ErrorCode.BAD_INPUT,
                "本次运行的请求上限是 $limit 个（agent 模式），已经用完了——剩下的一个请求都没发。" +
                    "需要更多就分几次跑，或者用 --max-requests 往低调之前的命令。",
            )
        }
    }

    val usedCount: Int get() = used.get()
}

/**
 * agent 模式 / 非交互时的验证码处理器：**不能替用户做验证**（红线），也不弹终端等输入。
 * 记下"这个平台要验证"，由 Source 抛回风控错误，`main` 再映射成 `captcha_required`。
 */
class NonInteractiveCaptchaHandler(private val session: Session) : CaptchaHandler {
    override suspend fun solve(challenge: CaptchaChallenge): String? {
        session.captchaNeeded += challenge.platform
        return null
    }
}

/** `main` 里的统一映射：任何异常都能变成"错误码 + 一行话"。 */
fun Throwable.toCliFailure(): CliFailure = when (this) {
    is CliFailure -> this
    is feedhub.core.RiskControlException -> CliFailure(ErrorCode.RISK_CONTROL, messageForHuman(), platform, extra = codeField())
    is feedhub.core.NotLoggedInException -> CliFailure(ErrorCode.NOT_LOGGED_IN, messageForHuman(), platform)
    is feedhub.core.UnsupportedFeedException -> CliFailure(ErrorCode.NOT_SUPPORTED, messageForHuman(), platform)
    is ProxyUnavailableException -> CliFailure(ErrorCode.PROXY_UNAVAILABLE, messageForHuman())
    is CliUsageError -> CliFailure(ErrorCode.BAD_INPUT, messageForHuman())
    is feedhub.core.SignatureException,
    is feedhub.core.ApiException,
    is feedhub.core.ParseException,
    -> CliFailure(ErrorCode.PLATFORM_ERROR, messageForHuman(), this.platform)
    is com.github.ajalt.clikt.core.CliktError -> CliFailure(ErrorCode.BAD_INPUT, messageForHuman())
    is java.io.IOException -> CliFailure(ErrorCode.NETWORK, "网络错误：${message ?: this::class.simpleName}")
    else -> CliFailure(ErrorCode.INTERNAL, "${this::class.simpleName}: ${message}")
}

private fun Throwable.platform(): Platform? = (this as? feedhub.core.SourceException)?.platform

private fun feedhub.core.RiskControlException.codeField(): Map<String, JsonElement> =
    code?.let { mapOf("code" to JsonPrimitive(it)) } ?: emptyMap()

/** `SourceException` 的 `message` 已经带了 `[平台] ` 前缀；人看的时候保留，JSON 里 platform 字段已经单独给了。 */
private fun Throwable.messageForHuman(): String = message ?: this::class.simpleName.orEmpty()
