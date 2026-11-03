package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.types.int
import com.github.ajalt.clikt.parameters.types.path
import feedhub.core.DeviceCodeLogin
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.DeviceCodeLoginState
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.Source
import feedhub.core.UserRef
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import java.io.IOException
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * 扫码登录流程里和终端无关的部分（测试里用假的 [QrLoginSource] 驱动）：
 * 生成二维码 → 画出来（终端 + PNG）→ 按 [QrLogin.pollIntervalMs] 轮询并打印状态变化 → 成功 / 过期 / 失败 / 超时。
 *
 * 风控等 [feedhub.core.SourceException] 直接往外抛，不重试；网络错误连续出现 [maxNetworkErrors] 次才放弃。
 * Ctrl-C 直接结束进程：Cookie 只在 [QrLoginSource.pollQrLogin] 返回 Success 的那一刻才写，所以退出时什么都没改。
 */
class QrLoginRunner(
    private val source: QrLoginSource,
    private val out: (String) -> Unit,
    private val pngFile: Path,
    private val timeoutMs: Long,
    private val terminalQr: Boolean = true,
    private val ansi: Boolean = true,
    private val now: () -> Long = System::currentTimeMillis,
    private val pause: suspend (Long) -> Unit = { delay(it) },
    private val maxNetworkErrors: Int = 3,
) {
    sealed interface Outcome {
        data class Success(val user: UserRef?) : Outcome
        data object Expired : Outcome
        data class Failed(val message: String) : Outcome
        data object TimedOut : Outcome
    }

    suspend fun run(): Outcome {
        val label = source.platform.label
        val qr = source.startQrLogin()
        val matrix = QrRender.encode(qr.content)
        QrRender.toPng(matrix, pngFile)
        out("$label 扫码登录：${qr.hint}")
        if (terminalQr) out(QrRender.toTerminal(matrix, ansi = ansi).trimEnd('\n'))
        out("二维码图片：$pngFile")
        val started = now()
        val deadline = started + timeoutMs
        val expires = qr.expiresAtMs
        out(
            "最多等 ${timeoutMs / 1000} 秒" + (expires?.let { "，二维码约 ${((it - started) / 1000).coerceAtLeast(0)} 秒后过期" } ?: "") +
                "；Ctrl-C 取消（不会改动现有登录态）",
        )

        var last: QrLoginState? = null
        var errors = 0
        while (true) {
            val state = try {
                source.pollQrLogin(qr).also { errors = 0 }
            } catch (e: IOException) {
                if (++errors >= maxNetworkErrors) throw e
                out("⚠ 网络错误（${e.message}），再试一次")
                null
            }
            if (state != null) {
                if (state != last) out(time() + describeState(state))
                last = state
                when (state) {
                    is QrLoginState.Success -> return Outcome.Success(state.user)
                    QrLoginState.Expired -> return Outcome.Expired
                    is QrLoginState.Failed -> return Outcome.Failed(state.message)
                    QrLoginState.Waiting, QrLoginState.Scanned -> Unit
                }
            }
            if (now() >= deadline) return Outcome.TimedOut
            pause(qr.pollIntervalMs)
        }
    }

    private fun describeState(state: QrLoginState): String = when (state) {
        QrLoginState.Waiting -> "等待扫码…"
        QrLoginState.Scanned -> "已扫码，请在手机上确认登录"
        is QrLoginState.Success -> "登录成功"
        QrLoginState.Expired -> "二维码已过期或被取消"
        is QrLoginState.Failed -> "登录失败：${state.message}"
    }

    private fun time() = TIME_FMT.format(Instant.ofEpochMilli(now())) + "  "

    private companion object {
        val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss").withZone(ZoneId.systemDefault())
    }
}

/**
 * 设备码登录（现在只有 YouTube）里和终端无关的部分，测试里用假的 [DeviceCodeLoginSource] 驱动：
 * 取一组设备码 → 打印**短码**和验证网址（`device_code` 是机密，不打印）→ 按平台的间隔轮询，
 * 直到成功 / 过期 / 超时。风控等 [feedhub.core.SourceException] 直接往外抛，不重试。
 *
 * Ctrl-C 直接结束进程：token 只在 [DeviceCodeLoginSource.pollDeviceCodeLogin] 返回 Success 的那一刻才写，
 * 所以退出时什么都没改。
 */
class DeviceCodeLoginRunner(
    private val source: DeviceCodeLoginSource,
    private val out: (String) -> Unit,
    private val timeoutMs: Long,
    private val now: () -> Long = System::currentTimeMillis,
    private val pause: suspend (Long) -> Unit = { delay(it) },
) {
    sealed interface Outcome {
        data class Success(val user: UserRef?) : Outcome
        data object Expired : Outcome
        data class Failed(val message: String) : Outcome
        data object TimedOut : Outcome
    }

    suspend fun run(): Outcome {
        val code: DeviceCodeLogin = source.startDeviceCodeLogin()
        out("${source.platform.label} 设备码登录：${code.hint}")
        out("  在 ${code.verificationUrl} 输入短码：${code.userCode}")
        val started = now()
        val deadline = started + timeoutMs
        out(
            "最多等 ${timeoutMs / 1000} 秒" +
                (code.expiresAtMs?.let { "，短码约 ${((it - started) / 1000).coerceAtLeast(0)} 秒后过期" } ?: "") +
                "；Ctrl-C 取消（不会改动现有登录态）",
        )
        while (true) {
            when (val state = source.pollDeviceCodeLogin(code)) {
                DeviceCodeLoginState.Waiting -> Unit
                is DeviceCodeLoginState.Success -> return Outcome.Success(state.user)
                DeviceCodeLoginState.Expired -> return Outcome.Expired
                is DeviceCodeLoginState.Failed -> return Outcome.Failed(state.message)
            }
            if (now() >= deadline) return Outcome.TimedOut
            pause(code.pollIntervalMs)
        }
    }
}

class LoginCmd : CliktCommand(name = "login") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台：bili / zhihu / tieba / heybox（酷安没有扫码，用 cookie import；yt 是设备码）")
    private val timeout by option("--timeout", help = "最多等多少秒").int().default(600)
    private val png by option("--png", help = "二维码 PNG 存到哪里（默认 build/login-qr-<平台>.png）").path()
    private val noTerminal by option("--no-terminal", help = "不在终端里画二维码，只存 PNG").flag()
    private val endpoint by option("--endpoint", help = "设备码端点（只有 yt 用）：tv（默认）或 google")

    override fun help(context: Context) = """
        登录。B站 / 知乎 / 贴吧 / 小黑盒是扫码（生成二维码，用对应平台的手机 App 扫并确认）；
        YouTube 是设备码（打印一组短码，去 google.com/device 输入并确认）。
        成功之前不会改动现有的登录态；Ctrl-C 退出也什么都不改。
    """.trimIndent()

    override fun run() = runBlocking {
        // 登录会写 Cookie / token：agent 模式下一律拒绝
        session.rejectInAgentMode("登录")
        val p = platformOf(platform)
        val source = env.source(p)
        if (source is DeviceCodeLoginSource) {
            deviceCode(source)
            return@runBlocking
        }
        val qrSource = source as? QrLoginSource
            ?: throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}没有扫码登录" + installed[p]?.login?.noQrHint?.let { "，$it" }.orEmpty())
        val runner = QrLoginRunner(
            source = qrSource,
            out = { echo(it) },
            pngFile = png ?: Paths.get("build", "login-qr-${p.key}.png"),
            timeoutMs = timeout * 1000L,
            terminalQr = !noTerminal,
        )
        val outcome = try {
            runner.run()
        } catch (e: Exception) {
            throw CliktError(describe(e))
        }
        when (outcome) {
            is QrLoginRunner.Outcome.Success -> reportSuccess(qrSource, outcome.user)
            QrLoginRunner.Outcome.Expired -> throw CliktError("二维码已过期或被取消，现有登录态没有改动。重新执行 feedhub login ${p.key}")
            is QrLoginRunner.Outcome.Failed -> throw CliktError("${outcome.message}（现有登录态没有改动）")
            QrLoginRunner.Outcome.TimedOut -> throw CliktError("等了 $timeout 秒没人扫码，现有登录态没有改动。重新执行 feedhub login ${p.key}")
        }
    }

    private suspend fun deviceCode(source: DeviceCodeLoginSource) {
        // --endpoint 只有 YouTube 认；写了就存下来，下次不写也用同一个
        endpoint?.let { env.ctx.store.put("youtube.device_code_endpoint", it) }
        val runner = DeviceCodeLoginRunner(source, out = { echo(it) }, timeoutMs = timeout * 1000L)
        val outcome = try {
            runner.run()
        } catch (e: Exception) {
            throw CliktError(describe(e))
        }
        when (outcome) {
            is DeviceCodeLoginRunner.Outcome.Success -> reportSuccess(source, outcome.user)
            DeviceCodeLoginRunner.Outcome.Expired ->
                throw CliktError("短码已过期，现有登录态没有改动。重新执行 feedhub login ${source.platform.key}")
            is DeviceCodeLoginRunner.Outcome.Failed -> throw CliktError("${outcome.message}（现有登录态没有改动）")
            DeviceCodeLoginRunner.Outcome.TimedOut ->
                throw CliktError("等了 $timeout 秒没人输码，现有登录态没有改动。重新执行 feedhub login ${source.platform.key}")
        }
    }

    private suspend fun reportSuccess(source: Source, user: UserRef?) {
        val me = user ?: try {
            source.whoami()
        } catch (e: Exception) {
            echo("登录成功，但查询账号出错：${describe(e)}")
            null
        }
        echo("${source.platform.label}  " + (me?.let { "已登录  ${it.name}（${it.id}）" } ?: "登录成功，账号信息暂时拿不到，可用 feedhub whoami ${source.platform.key} 再查"))
        val missing = source.login.requiredCookies.filter { env.ctx.cookies.find(source.login.cookieUrl, it).isNullOrEmpty() }
        if (missing.isNotEmpty()) echo("⚠ 缺少 Cookie：${missing.joinToString()}，部分功能可能用不了", err = true)
    }
}
