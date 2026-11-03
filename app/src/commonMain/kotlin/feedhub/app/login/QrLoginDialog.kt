package feedhub.app.login

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import feedhub.app.AppForeground
import feedhub.app.feed.describe
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/** 弹框里的阶段。 */
private sealed interface Phase {
    data object Loading : Phase
    data class Showing(val login: QrLogin, val state: QrLoginState, val notice: String? = null) : Phase
    data class Error(val message: String) : Phase
}

/**
 * 扫码登录弹框：拿二维码 → 画出来 → 按平台建议的间隔轮询，直到成功 / 过期 / 出错。
 * 成功时 Source 已经把新的 Cookie 存好了，这里只通知 [onSuccess]；过期或出错可以"刷新二维码"重来，不自动重试。
 * 关掉弹框就停止轮询，原来的登录态不受影响（成功前 Source 不碰 Cookie）。
 * 切到平台 App 扫码时（本机在后台）不轮询，回来接着问，偶尔网络不通不判死，见 [pollLogin]。
 */
@Composable
fun QrLoginDialog(
    platformLabel: String,
    source: QrLoginSource,
    onDismiss: () -> Unit,
    onSuccess: () -> Unit,
    foreground: AppForeground? = null,
) {
    val gate = remember(foreground) { foreground?.let { ForegroundGate.of(it) } ?: ForegroundGate.Always }
    var attempt by remember { mutableIntStateOf(0) }
    var phase by remember { mutableStateOf<Phase>(Phase.Loading) }
    LaunchedEffect(attempt) {
        phase = Phase.Loading
        try {
            val login = withContext(Dispatchers.IO) { source.startQrLogin() }
            phase = Phase.Showing(login, QrLoginState.Waiting)
            val end = pollLogin(
                intervalMs = login.pollIntervalMs,
                expiresAtMs = login.expiresAtMs,
                gate = gate,
                expired = QrLoginState.Expired,
                poll = { withContext(Dispatchers.IO) { source.pollQrLogin(login) } },
                isFinal = { it != QrLoginState.Waiting && it != QrLoginState.Scanned },
                onState = { state, trouble ->
                    val shown = (phase as? Phase.Showing)?.state ?: QrLoginState.Waiting
                    phase = Phase.Showing(login, state ?: shown, notice = if (trouble) NETWORK_RETRY_HINT else null)
                },
            )
            phase = Phase.Showing(login, end)
            if (end is QrLoginState.Success) {
                delay(600)
                onSuccess()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            phase = Phase.Error(describe(e))
        }
    }

    Dialog(onDismissRequest = onDismiss) {
        Surface(shape = RoundedCornerShape(20.dp), color = MaterialTheme.colorScheme.surfaceContainerLowest) {
            Column(
                Modifier.padding(24.dp).width(300.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                Text("扫码登录$platformLabel", style = MaterialTheme.typography.titleMedium)
                Box(Modifier.size(232.dp), contentAlignment = Alignment.Center) {
                    when (val p = phase) {
                        Phase.Loading -> CircularProgressIndicator()
                        is Phase.Showing -> QrCode(p.login.content, 232.dp, dimmed = p.state != QrLoginState.Waiting)
                        is Phase.Error -> Text(p.message, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                    }
                }
                val (line, color) = when (val p = phase) {
                    Phase.Loading -> "正在获取二维码…" to MaterialTheme.colorScheme.onSurfaceVariant
                    is Phase.Error -> "获取二维码失败" to MaterialTheme.colorScheme.error
                    is Phase.Showing -> if (p.notice != null) p.notice to MaterialTheme.colorScheme.onSurfaceVariant else when (val s = p.state) {
                        QrLoginState.Waiting -> p.login.hint to MaterialTheme.colorScheme.onSurfaceVariant
                        QrLoginState.Scanned -> "已扫码，请在手机上确认" to MaterialTheme.colorScheme.primary
                        is QrLoginState.Success -> "登录成功" + (s.user?.name?.let { "：$it" } ?: "") to MaterialTheme.colorScheme.primary
                        QrLoginState.Expired -> "二维码已过期" to MaterialTheme.colorScheme.error
                        is QrLoginState.Failed -> s.message to MaterialTheme.colorScheme.error
                    }
                }
                Text(line, color = color, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val showing = phase as? Phase.Showing
                    val canRetry = phase is Phase.Error ||
                        showing?.state == QrLoginState.Expired || showing?.state is QrLoginState.Failed
                    if (canRetry) TextButton(onClick = { attempt++ }) { Text("刷新二维码") }
                    TextButton(onClick = onDismiss) { Text("取消") }
                }
            }
        }
    }
}

/** 画二维码：永远白底黑码（深色模式下也是，手机才扫得出来）。扫过之后变淡，提示"已扫码"。 */
@Composable
internal fun QrCode(content: String, size: Dp, dimmed: Boolean) {
    val matrix = remember(content) { encodeQr(content) }
    Canvas(Modifier.size(size).background(Color.White, RoundedCornerShape(8.dp)).padding(10.dp)) {
        val cell = this.size.width / matrix.width
        val ink = if (dimmed) Color(0xFFBDBDBD) else Color.Black
        for (y in 0 until matrix.height) for (x in 0 until matrix.width) {
            if (matrix[x, y]) drawRect(ink, Offset(x * cell, y * cell), Size(cell + 0.5f, cell + 0.5f))
        }
    }
}

internal fun encodeQr(content: String): BitMatrix = QRCodeWriter().encode(
    content,
    BarcodeFormat.QR_CODE,
    0,
    0,
    mapOf(EncodeHintType.MARGIN to 0, EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M, EncodeHintType.CHARACTER_SET to "UTF-8"),
)
