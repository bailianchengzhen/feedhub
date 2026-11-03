package feedhub.app.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.api.APP_API_SKILL_DIR
import feedhub.app.api.APP_API_SKILL_FILE
import feedhub.app.api.AppApiStatus
import kotlinx.coroutines.flow.MutableStateFlow

/**
 * 设置 →「进阶设置」→「对外接口（agent）」。
 *
 * 再配一份说明书（[APP_API_SKILL_FILE]），agent 读了说明书自己 `curl --unix-socket`。
 * **不是独立程序**：这一页只有一个开关（默认关）、现在在不在听、socket 在哪、说明书的路径、被调过几次。
 * App 关着 / 开关关着，就**根本没有这个接口**（不自动拉起 App、不弹通知）。
 *
 * 只显示"计数和时间"，不记任何调用内容（参数、关键词、条目都不记）。
 */
@Composable
internal fun AppApiSettings(graph: AppGraph) {
    val s = graph.settings.value
    // 截图工具（api=on）给的是不 bind socket 的假接口（dev/FakeAppApi）
    val status by (graph.appApi?.status ?: MutableStateFlow(AppApiStatus())).collectAsState()

    SettingsCard("对外接口（agent）") {
        SwitchRow(
            label = "打开对外接口",
            hint = if (s.appApi) "开着：App 在本机 unix socket 上提供只读 HTTP" else "关着（默认）",
            checked = s.appApi,
        ) { v -> graph.settings.update { it.copy(appApi = v) } }
        Hint(
            "给 agent 读 skill 用：搜各平台内容、看详情和评论、查本地收藏 / 历史。",
            detail = "接口是桌面 App 自己的一部分：没有独立程序、没有自己的状态，请求全部走 App 里的同一套平台 / " +
                "Cookie / 设置 / 资料库。只读：没有登录、关注、任何写操作。App 关着、或者这个开关关着，这个接口就根本不存在" +
                "（不会自动拉起 App，也不弹通知）。",
        )
        SettingBlock(
            "现在的状态",
            hint = "App 关掉之后，这个文件就没了",
            detail = "socket 只放在本机文件系统上（不监听 TCP 端口：TCP 本机任何进程、包括浏览器页面都能碰，" +
                "unix socket 只有本用户能连）。权限：socket 0600、目录 0700。",
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // 状态点：绿 = 在听，灰 = 没在听（和「代理（进阶）」那张卡片同一个画法）
                Box(
                    Modifier.size(8.dp).background(
                        if (s.appApi && status.listening) Color(0xFF2E7D32) else Color(0xFF9E9E9E),
                        RoundedCornerShape(4.dp),
                    ),
                )
                Text(statusTitle(s.appApi, status), style = MaterialTheme.typography.bodyMedium)
            }
            status.socketPath?.takeIf { it.isNotBlank() }?.let { path ->
                Text(
                    path,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
            if (s.appApi && !status.listening) {
                Hint("开关开着，但没在听：${status.error ?: "socket 建不起来（看 App 的日志）"}")
            }
        }
        SettingBlock(
            "skill（给 agent 的说明书）",
            hint = "agent 读这个文件就知道怎么调",
            detail = "仓库里是 skill/feedhub/SKILL.md，deb 把它装到下面的路径。里面写了怎么确认接口在、" +
                "每个接口的 curl 例子、输出格式、错误码。",
        ) {
            Text(
                APP_API_SKILL_DIR,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
            )
            // 一行能直接粘的调用例子（socket 路径跟着实际的走）
            status.socketPath?.takeIf { it.isNotBlank() }?.let { path ->
                Text(
                    "curl --unix-socket $path http://feedhub/capabilities",
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
        SettingBlock("调用统计", "只记次数和时间，不记内容") {
            val last = status.lastCallAt?.let { "最近一次 ${formatTime(it)}" } ?: "还没被调过"
            Text("调过 ${status.calls} 次 · $last", style = MaterialTheme.typography.bodyMedium)
            status.lastError?.let { Hint("最近一次失败：$it") }
        }
    }
}

private fun statusTitle(enabled: Boolean, status: AppApiStatus): String = when {
    !enabled -> "关（没有这个接口）"
    status.listening -> "在听"
    else -> "开关开着，没在听"
}
