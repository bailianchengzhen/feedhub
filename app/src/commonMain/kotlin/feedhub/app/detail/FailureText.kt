package feedhub.app.detail

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.core.NotLoggedInException
import feedhub.core.RiskControlException
import feedhub.present.RustPresent
import java.io.IOException
import uniffi.feedhub_ffi.FfiApiFailureKind

/**
 * 出错时界面上那一句**人话**。
 *
 * 只有这几种：没网、没登录、被风控拦了、这个平台看不了、加载失败。技术细节走日志，不进界面。
 * 平台的 `SourceException.message` 里本来就带 `[小红书]` 这种前缀和参数名，是给日志用的，不往界面上摆。
 *
 * "哪一类对应哪个字"在 Rust**（`present::errors::friendly_error`），
 * 这里只做"宿主异常类型 → 种类"那一次 `when`（`IOException` / `NotLoggedInException` 是**宿主侧**的东西）。
 * 种类复用的是对外接口那套 `FailureKind`（`appapi::classify`），所以接口错误码和界面文案不会各说各的。
 */
fun friendlyError(e: Throwable): String = RustPresent.friendlyError(
    when (e) {
        is NotLoggedInException -> FfiApiFailureKind.NOT_LOGGED_IN
        is RiskControlException -> FfiApiFailureKind.RISK_CONTROL
        is IOException -> FfiApiFailureKind.NETWORK
        is UnsupportedOperationException -> FfiApiFailureKind.NOT_SUPPORTED
        else -> FfiApiFailureKind.INTERNAL
    },
    e.message,
)

/**
 * 出错占位块：一句话 + 「重试」按钮，**按钮永远看得见**（两行摆，不像以前那样被一段长错误挤出屏幕）。
 *
 * @param hint 按钮上面再说一句的地方（例如"平台限制"这种补充）；null 就不显示。
 */
@Composable
fun FailureBlock(
    message: String,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
    hint: String? = null,
) {
    Column(
        modifier.fillMaxWidth().padding(horizontal = 4.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Surface(color = MaterialTheme.colorScheme.surfaceContainer, shape = RoundedCornerShape(10.dp)) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 10.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    message,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                    // 再长也不会把"重试"挤走：文案自己折行 / 省略
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
                if (hint != null) {
                    Text(
                        hint,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        TextButton(onClick = onRetry, modifier = Modifier.heightIn(min = 48.dp)) { Text("重试") }
    }
}
