package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
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
import java.io.File

/**
 * 设置页「翻译（本地）」：**开关 + 自动翻译的平台**。
 *
 * 默认值见 [DEFAULT_TRANSLATION_MODEL] / [DEFAULT_LLAMA_SERVER]（HY-MT1.5-1.8B Q4_K_M + llama.cpp Vulkan 构建）；
 * 换机器时把两个路径改成自己的即可。开关打开后，只有**点亮的平台**的内容才会自动翻：第一次出现外语文字时
 * 才启动 llama-server（约 1.2GB 内存），不会在设置页里预加载；没有任何点亮平台的内容时连引擎都不建
 *
 * 这一页只留开关和平台标签，
 * 外加一个能点进去的入口（[SettingsJump]）。存储键和默认值一个字没动。
 *
 * 这一页的两行文案跟着改：
 * - 总开关的说明改成"详情里那条「翻译」点了才翻 / 点亮平台的内容进详情自动翻"（两句都对，看下面那排标签）；
 * - 那排平台标签的说明分成桌面 / 手机两句：**手机默认一个都不点亮**（点了才翻），
 *   手机上的文案写成"手机上默认点了才翻"，不让用户以为这里没生效。
 */
@Composable
internal fun TranslationSettings(graph: AppGraph, nav: SettingsNav) {
    val s = graph.settings.value
    val available = graph.translation.available

    SettingsCard("翻译（本地）") {
        SwitchRow(
            "打开本地翻译",
            if (graph.desktop) {
                "只翻不是中文的正文 / 简介 / 评论；模型常驻约 1.2GB"
            } else {
                "详情里那条「翻译」点了才翻，不会自己翻；模型常驻约 1.2GB"
            },
            checked = s.translationOn,
            enabled = available,
            onChange = { v -> graph.settings.update { it.copy(translationOn = v) } },
        )
        // 平台标签：样式和交互照「网络 → 走代理的平台」（同一个 PlatformToggleChips）
        SettingBlock(
            "自动翻译的平台",
            if (graph.desktop) {
                "点亮的平台，卡片标题 / 摘要和详情都自动翻；没点亮的平台仍可在正文下面点「翻译」"
            } else {
                "手机默认点了才翻（这里一个都不点亮）；点亮的平台卡片和详情都会自动翻"
            },
        ) {
            PlatformToggleChips(s.translationPlatforms) { v ->
                graph.settings.update { it.copy(translationPlatforms = v) }
            }
        }
        if (!available) Hint(if (graph.desktop) "本地翻译只在桌面版可用。" else "这个安装包没有带翻译模型。")
        Hint("目标语言：简体中文（跟随界面语言；以后加应用语言设置再扩展）")
        SettingsJump(
            "模型和 llama-server 的路径在「翻译（进阶）」里",
            SettingsRoute.Advanced(AdvancedSection.TRANSLATION),
            nav,
        )
    }
}

/**
 *
 * 为什么这些不留在常规页：两个路径是**这台机器上 llama.cpp 装在哪**的问题，普通用户不该被要求填；
 * 状态 / 缓存清理也只有改过路径、或者启动失败时才用得上。
 */
@Composable
internal fun TranslationAdvancedSettings(graph: AppGraph) {
    val s = graph.settings.value
    val status by graph.translation.status.collectAsState()
    val available = graph.translation.available
    val modelExists = File(s.translationModelPath).isFile
    val serverExists = File(s.translationServerPath).isFile
    val statusLabel = when (status) {
        TranslationStatus.STOPPED -> "未启动（第一次翻外语时启动）"
        TranslationStatus.STARTING -> "正在加载模型…"
        TranslationStatus.READY -> "已就绪"
        TranslationStatus.FAILED -> "启动失败，日志在 ~/.cache/feedhub/llama-server.log"
    }

    SettingsCard("翻译引擎") {
        if (!available) Hint("本地翻译只在桌面版可用。")
        Hint(
            when {
                modelExists && serverExists -> "模型和 llama-server 都找到了。"
                !modelExists && !serverExists -> "模型和 llama-server 都没找到，先在下面改路径。"
                !modelExists -> "没找到翻译模型，先在下面改路径。"
                else -> "没找到 llama-server，先在下面改路径。"
            },
        )
        SettingBlock("翻译模型（GGUF）") {
            OutlinedTextField(
                value = s.translationModelPath,
                onValueChange = { v -> graph.settings.update { it.copy(translationModelPath = v) } },
                singleLine = true,
                textStyle = MaterialTheme.typography.bodySmall,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        SettingBlock("llama-server") {
            OutlinedTextField(
                value = s.translationServerPath,
                onValueChange = { v -> graph.settings.update { it.copy(translationServerPath = v) } },
                singleLine = true,
                textStyle = MaterialTheme.typography.bodySmall,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        Hint("状态：$statusLabel")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { graph.translation.clearCache() }) { Text("清除翻译缓存") }
        }
    }
}
