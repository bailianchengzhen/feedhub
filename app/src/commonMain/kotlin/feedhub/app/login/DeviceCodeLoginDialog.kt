package feedhub.app.login

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import feedhub.app.AppForeground
import feedhub.app.feed.describe
import feedhub.core.DeviceCodeLogin
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.DeviceCodeLoginState
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * 设备码登录的弹框（OAuth 2.0 device flow，现在只有 YouTube）。
 *
 * 流程：[startDeviceCodeLogin] 拿到短码 → 显示 `user_code` 和验证地址（可点开 / 可复制）→
 * 按服务端给的间隔轮询，直到成功 / 过期 / 失败。**用户本人在另一台设备上输入短码并确认**，
 * 我们不打开账号、不填任何东西。
 *
 * `device_code` 是机密（换 token 用）：**不显示、不打印、不进日志**；界面上只有 `user_code`（本来就是给人看的）
 * 和验证地址。token 由 Source 在成功后自己写进 `ctx.store`。
 *
 * 关掉弹框就停止轮询（协程随 composition 取消），成功前不动现有登录态。
 * 切到浏览器输码时（App 在后台）不轮询，回来接着问，偶尔网络不通不判死，见 [pollLogin]。
 */
@Composable
fun DeviceCodeLoginDialog(
    platformLabel: String,
    source: DeviceCodeLoginSource,
    onDismiss: () -> Unit,
    onSuccess: () -> Unit,
    foreground: AppForeground? = null,
) {
    val gate = remember(foreground) { foreground?.let { ForegroundGate.of(it) } ?: ForegroundGate.Always }
    var attempt by remember { mutableIntStateOf(0) }
    var phase by remember { mutableStateOf<PairingPhase>(PairingPhase.Loading) }
    LaunchedEffect(attempt) {
        phase = PairingPhase.Loading
        try {
            val login = withContext(Dispatchers.IO) { source.startDeviceCodeLogin() }
            phase = PairingPhase.Showing(login, DeviceCodeLoginState.Waiting)
            val end = pollLogin(
                intervalMs = login.pollIntervalMs.coerceAtLeast(1000L),
                expiresAtMs = login.expiresAtMs,
                gate = gate,
                expired = DeviceCodeLoginState.Expired,
                poll = { withContext(Dispatchers.IO) { source.pollDeviceCodeLogin(login) } },
                isFinal = { it != DeviceCodeLoginState.Waiting },
                onState = { state, trouble ->
                    val shown = (phase as? PairingPhase.Showing)?.state ?: DeviceCodeLoginState.Waiting
                    phase = PairingPhase.Showing(login, state ?: shown, notice = if (trouble) NETWORK_RETRY_HINT else null)
                },
            )
            phase = PairingPhase.Showing(login, end)
            if (end is DeviceCodeLoginState.Success) {
                delay(600)
                onSuccess()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            phase = PairingPhase.Error(describe(e))
        }
    }

    Dialog(onDismissRequest = onDismiss) {
        Surface(shape = RoundedCornerShape(20.dp), color = MaterialTheme.colorScheme.surfaceContainerLowest) {
            Column(
                Modifier.padding(22.dp).width(320.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                DeviceCodeLoginContent(
                    platformLabel = platformLabel,
                    phase = phase,
                    onRetry = { attempt++ },
                    onDismiss = onDismiss,
                )
            }
        }
    }
}

/** 弹框里的阶段。 */
internal sealed interface PairingPhase {
    data object Loading : PairingPhase
    data class Showing(val login: DeviceCodeLogin, val state: DeviceCodeLoginState, val notice: String? = null) : PairingPhase
    data class Error(val message: String) : PairingPhase
}

/**
 * 弹框的内容（拆出来是为了截图工具能在没有 `Dialog` 窗口的情况下画一遍：`screen=devicelogin`）。
 * 只读 [DeviceCodeLogin] 的 `userCode` / `verificationUrl` / `hint`，**永远不碰 `deviceCode`**。
 */
@Composable
internal fun DeviceCodeLoginContent(
    platformLabel: String,
    phase: PairingPhase,
    onRetry: () -> Unit,
    onDismiss: () -> Unit,
) {
    Text("$platformLabel 设备码登录", style = MaterialTheme.typography.titleMedium)
    when (phase) {
        PairingPhase.Loading -> {
            CircularProgressIndicator()
            Text("正在向平台要一组短码…", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = onDismiss) { Text("关闭") }
        }
        is PairingPhase.Error -> {
            Text(phase.message, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = onRetry) { Text("重试") }
                TextButton(onClick = onDismiss) { Text("关闭") }
            }
        }
        is PairingPhase.Showing -> {
            val login = phase.login
            val state = phase.state
            val clipboard = LocalClipboardManager.current
            val uriHandler = LocalUriHandler.current
            var copied by remember(login.userCode) { mutableStateOf(false) }
            LaunchedEffect(copied) {
                if (copied) {
                    delay(1500)
                    copied = false
                }
            }

            Text(
                "在另一台设备上打开验证页，输入下面的短码并确认。",
                style = MaterialTheme.typography.bodyMedium,
                textAlign = TextAlign.Center,
            )
            Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Text(
                    login.userCode,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
                    style = MaterialTheme.typography.headlineMedium.copy(
                        fontFamily = FontFamily.Monospace,
                        letterSpacing = 2.sp,
                    ),
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = {
                    clipboard.setText(AnnotatedString(login.userCode))
                    copied = true
                }) { Text(if (copied) "已复制" else "复制短码") }
                TextButton(onClick = { uriHandler.openUri(login.verificationUrl) }) { Text("打开验证页") }
            }
            Text(
                login.verificationUrl,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                textAlign = TextAlign.Center,
            )
            val (line, color) = when (state) {
                DeviceCodeLoginState.Waiting ->
                    "等你输入…" to MaterialTheme.colorScheme.onSurfaceVariant
                DeviceCodeLoginState.Expired ->
                    "短码过期了，换一组再来" to MaterialTheme.colorScheme.error
                is DeviceCodeLoginState.Success ->
                    "登录成功" + (state.user?.name?.let { "：$it" } ?: "") to MaterialTheme.colorScheme.primary
                is DeviceCodeLoginState.Failed ->
                    state.message to MaterialTheme.colorScheme.error
            }
            Text(phase.notice ?: line, color = color, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
            Text(
                login.hint,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (state == DeviceCodeLoginState.Expired || state is DeviceCodeLoginState.Failed) {
                    TextButton(onClick = onRetry) { Text("换一组短码") }
                }
                TextButton(onClick = onDismiss) { Text("关闭") }
            }
        }
    }
}
