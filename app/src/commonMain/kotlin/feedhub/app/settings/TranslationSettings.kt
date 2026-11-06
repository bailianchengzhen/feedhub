package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.translate.TranslationStatus

/**
 * 设置页「翻译（本地）」：**开关 + 自动翻译的平台**。
 *
 * 双端统一采用进程内轻量离线翻译引擎（Project Bergamot / Firefox Translations `enzh`）：
 * - 内置模型约 48 MB，开箱离线可用，仅在首次遇到英文内容时按需加载（运行期约 180 MB，空闲或退后台自动释放）；
 * - 当前仅支持 **英文 → 简体中文**，非英文外语（如日文、韩文）不会误触发自动翻译或显示翻译入口；
 * - 手机默认所有平台不点亮（像 X 一样在详情里点「翻译」才翻），桌面默认点亮境外平台自动翻。
 */
@Composable
internal fun TranslationSettings(graph: AppGraph, nav: SettingsNav) {
    val s = graph.settings.value
    val available = graph.translation.available

    SettingsCard("翻译（本地）") {
        SwitchRow(
            "打开本地翻译",
            if (graph.desktop) {
                "将英文正文、摘要与评论离线译为简体中文；模型约 48 MB，空闲自动释放内存"
            } else {
                "详情里那条「翻译」点了才将英文译为简体中文；模型约 48 MB，退后台自动释放内存"
            },
            checked = s.translationOn,
            enabled = available,
            onChange = { v -> graph.settings.update { it.copy(translationOn = v) } },
        )
        SettingBlock(
            "自动翻译的平台",
            if (graph.desktop) {
                "点亮的平台，英文卡片标题 / 摘要和详情都自动翻；没点亮的平台仍可在英文正文下面点「翻译」"
            } else {
                "手机默认点了才翻（这里一个都不点亮）；点亮的平台英文卡片和详情都会自动翻"
            },
        ) {
            PlatformToggleChips(s.translationPlatforms) { v ->
                graph.settings.update { it.copy(translationPlatforms = v) }
            }
        }
        if (!available) Hint("这个安装包没有带本地翻译引擎与模型。")
        Hint("语言范围：英文 → 简体中文（Firefox Bergamot 离线引擎，非英文内容暂不触发翻译）")
        SettingsJump(
            if (graph.desktop) {
                "引擎状态、缓存清理与自定义模型目录在「翻译（进阶）」里"
            } else {
                "引擎状态与缓存清理在「翻译（进阶）」里"
            },
            SettingsRoute.Advanced(AdvancedSection.TRANSLATION),
            nav,
        )
    }
}

/**
 * 「翻译（进阶）」：进程内 Bergamot 翻译引擎的运行状态、缓存清理及桌面端可选自定义模型目录。
 */
@Composable
internal fun TranslationAdvancedSettings(graph: AppGraph) {
    val s = graph.settings.value
    val status by graph.translation.status.collectAsState()
    val available = graph.translation.available
    val runtimeSummary = graph.translation.describeRuntime()
        ?: if (available) {
            "内置英译中离线模型已就绪（约 48 MB）"
        } else {
            "这个安装包没有带本地翻译引擎与模型。"
        }
    val statusLabel = when (status) {
        TranslationStatus.STOPPED -> "未加载（首次翻译英文时按需加载，平时零内存占用）"
        TranslationStatus.STARTING -> "正在加载翻译模型…"
        TranslationStatus.READY -> if (graph.desktop) {
            "已就绪（内存占用约 180 MB，空闲 5 分钟自动释放）"
        } else {
            "已就绪（内存占用约 180 MB，退后台或空闲 2 分钟自动释放）"
        }
        TranslationStatus.FAILED -> "加载失败，请检查模型文件是否完整"
    }

    SettingsCard("翻译引擎（Bergamot）") {
        Hint(runtimeSummary)
        if (graph.desktop) {
            SettingBlock(
                "自定义模型目录（可选）",
                "留空使用内置英译中模型；也可填写包含 model.*.bin、词表 .spm 与 lex.*.bin 的 Bergamot 模型目录",
            ) {
                OutlinedTextField(
                    value = s.translationModelPath,
                    onValueChange = { v -> graph.settings.update { it.copy(translationModelPath = v) } },
                    placeholder = { Text("留空使用内置英译中模型", style = MaterialTheme.typography.bodySmall) },
                    singleLine = true,
                    textStyle = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }
        Hint("状态：$statusLabel")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { graph.translation.clearCache() }) { Text("清除翻译缓存") }
        }
    }
}
