package feedhub.app.captcha

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph

/**
 * 酷安评论的人机验证覆盖层。**放在详情页里面**：双栏时只盖住右边的详情区，全屏详情时盖住整屏；
 * 不弹全局对话框、不开系统浏览器（除非内嵌浏览器起不来，用户点「用系统浏览器打开」）。
 *
 * 验证页由平台的 handler 提供：桌面是 [DesktopCaptchaHandler] 在本机 127.0.0.1 起的临时 HTTP 服务；
 * 安卓是 `AndroidCaptchaHandler` 在一个固定的本机地址上由 WebView 拦截直接返回（见 `CaptchaBrowser.android.kt`）。
 * 用户在里面完成易盾验证，页面把 proof 回传，`solve` 返回，评论请求自动重试，覆盖层随之消失。
 *
 * 直接弹窗让人划就行"——不再有介绍文字 / 状态行 / 「开始验证」按钮：
 * 框只比易盾的滑块大一圈，页面一加载就弹出滑块（还是用户本人划，见 `CoolapkCaptchaPage`）。
 */
@Composable
fun CaptchaOverlay(graph: AppGraph) {
    val prompt = graph.captchaPrompt ?: return
    if (!prompt.visible) return
    val url = prompt.url ?: return
    var browserError by remember(url) { mutableStateOf<String?>(null) }
    // 安卓：返回键 = 取消（桌面没有这个键，是空实现）；覆盖层开着时由它吃掉返回键，别把详情页也关了
    CaptchaBackHandler(enabled = true) { graph.captcha?.cancel() }
    // 安卓的验证页由 handler 在固定本机地址上直接返回（WebView 拦截），桌面是 handler 起的本机 HTTP 服务
    val host = graph.captcha as? CaptchaPageHost

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = 0.38f))
            // 吃掉覆盖层上的点击，别让下面的详情页跟着响应
            .clickable(
                interactionSource = remember { MutableInteractionSource() },
                indication = null,
            ) {},
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            modifier = Modifier
                // 手机上躲开状态栏 / 手势条 / 软键盘（桌面上这些 inset 都是 0，布局不变）
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .imePadding()
                .widthIn(max = 380.dp)
                .fillMaxWidth(0.94f)
                .heightIn(max = 520.dp)
                .fillMaxHeight(0.9f),
            shape = RoundedCornerShape(16.dp),
            tonalElevation = 6.dp,
            shadowElevation = 12.dp,
        ) {
            Column(Modifier.fillMaxSize().padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "${prompt.platformLabel} · 人机验证",
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.weight(1f),
                    )
                    IconButton(onClick = { graph.captcha?.cancel() }) {
                        Icon(Icons.Outlined.Close, contentDescription = "取消验证")
                    }
                }
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .padding(end = 8.dp)
                        .clip(RoundedCornerShape(10.dp))
                        .background(MaterialTheme.colorScheme.surfaceVariant),
                ) {
                    CaptchaBrowser(
                        url = url,
                        modifier = Modifier.fillMaxSize(),
                        onError = { browserError = it },
                        host = host,
                    )
                }
                browserError?.let { error ->
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "内置浏览器起不来：$error。可以点下面用系统浏览器完成。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                // 安卓没有这个退路：验证页只存在于我们这个 WebView 里（固定本机地址是拦截出来的，
                // 系统浏览器打不开它），所以按钮只在桌面显示
                if (host == null) {
                    TextButton(
                        onClick = { graph.captcha?.openInSystemBrowser() },
                        modifier = Modifier.align(Alignment.End),
                    ) {
                        Text("用系统浏览器打开", style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        }
    }
}
