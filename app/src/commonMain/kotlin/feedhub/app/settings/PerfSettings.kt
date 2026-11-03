package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.perf.PerfDebug
import feedhub.app.perf.PerfStatus
import kotlinx.coroutines.launch

/**
 * 设置 →「进阶设置」→「性能记录」。
 *
 * 普通用户用不上（编排行分析耗电 / 流畅度用），所以放在进阶设置里，**默认关**。打开后 App 自己每 10 秒写一行
 * JSON Lines 到外置应用目录；这里显示目录和当前大小，另有「清空记录」「导出到下载目录」。
 *
 * 只存本机（[feedhub.app.AppSettings.perfLog] 在设置里，跨设备同步只传收藏 / 历史，不带设置）。
 * 桌面版没有记录器（[AppGraph.perf] 为 null），卡片会说明"这个构建不记录"。
 */
@Composable
internal fun PerfLogSettings(graph: AppGraph) {
    val s = graph.settings.value
    val perf = graph.perf
    var status by remember { mutableStateOf<PerfStatus?>(null) }
    var refresh by remember { mutableIntStateOf(0) }
    var message by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    LaunchedEffect(refresh, s.perfLog) {
        status = perf?.status() ?: PerfDebug.statusOverride ?: PerfStatus.Unsupported
    }
    SettingsCard("性能记录") {
        SwitchRow(
            "记录性能日志",
            if (s.perfLog) "开着：前台每 10 秒一行，进后台就停" else "关着（默认）",
            s.perfLog,
        ) { v ->
            graph.settings.update { it.copy(perfLog = v) }
            message = null
            refresh++
        }
        // 设计规范：说明只占一行，细节收进右边的「?」
        Hint(
            "只记平台 id、类型、页面名、计数和耗时。",
            detail = "不记标题、正文、URL、用户名、Cookie / token。只存在本机，不进跨设备同步。",
        )
        if (s.perfLog) {
            val st = status
            SettingBlock(
                "记录文件",
                hint = "一天一个文件，只留最近 7 天",
                detail = "安卓写在 /sdcard/Android/data/feedhub.app/files/perf/，插线后 adb pull 这个目录。" +
                    "部分系统的 release 包会拒（Android/data 的 shell 访问限制），点「导出到下载目录」，" +
                    "再从 /sdcard/Download/feedhub-perf/ 拉。",
            ) {
                if (st?.supported == true) {
                    Text(
                        st.dirPath.orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                Hint(
                    when {
                        st == null -> "正在计算…"
                        !st.supported -> "这个构建不写记录（桌面版不记录；安卓版才有文件）"
                        else -> "现在 ${st.files} 个文件、${formatBytes(st.bytes)}"
                    },
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(
                    enabled = (st?.files ?: 0) > 0 && perf?.exporter != null && !busy,
                    onClick = {
                        val export = perf?.exporter ?: return@OutlinedButton
                        busy = true
                        scope.launch {
                            message = export()
                            busy = false
                            refresh++
                        }
                    },
                ) { Text(if (busy) "导出中…" else "导出到下载目录") }
                OutlinedButton(
                    enabled = (st?.files ?: 0) > 0 && !busy,
                    onClick = {
                        scope.launch {
                            val n = perf?.clear() ?: 0
                            message = if (n > 0) "清掉了 $n 个文件" else "没有可清的文件"
                            refresh++
                        }
                    },
                ) { Text("清空记录") }
                Spacer(Modifier.width(4.dp))
                message?.let { Hint(it) }
            }
        }
    }
}
