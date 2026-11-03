package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.brain.ContentFilter

/**
 * 设置页的「内容过滤」卡片：屏蔽词 + 外出模式。
 *
 * 不打码、不折叠**；屏蔽词一直生效，外出模式那两份名单只在外出模式开着时生效；
 * 关掉外出模式后"我们自己判出来的"那部分照常显示。平台标成 18+ 的（[feedhub.brain.AdultFilter]）
 * **不是这一页管的**——它一直隐藏，没开关也没名单。
 *
 * 这一版只做第一层（关键词 + 社区）：图片小模型、小语言模型的接口
 * （[feedhub.core.ContentJudge]）只定义没实现，所以卡片里也没有第二、三层的开关。
 * 默认两份名单都是空的。
 */
@Composable
internal fun ContentFilterSettings(graph: AppGraph) {
    val s = graph.settings.value
    val update = graph.settings::update
    SettingsCard("内容过滤") {
        SettingBlock(
            "屏蔽词",
            "命中标题 / 摘要 / 转发原文就整条隐藏",
            detail = "标题、摘要、转发原文里含这些词的整条不进首页 / 关注；一直生效（空名单不做任何过滤）。",
        ) {
            WordListEditor(
                words = s.blockWords,
                placeholder = "要屏蔽的词",
                emptyHint = "还没有屏蔽词（空名单不做任何过滤）",
            ) { v -> update { it.copy(blockWords = v) } }
        }
        SwitchRow(
            "外出模式",
            if (s.outingMode) "开着：下面两份名单命中的整条隐藏"
            else "关着：两份名单不起作用（18+ 照样隐藏）",
            s.outingMode,
        ) { v -> update { it.copy(outingMode = v) } }
        SettingBlock(
            "外出模式关键词",
            "只在外出模式开着时生效",
            detail = "命中标题 / 摘要 / 转发原文就整条隐藏；关掉外出模式后这些关键词不起作用。",
        ) {
            WordListEditor(
                words = s.outingKeywords,
                placeholder = "外出时不想看到的词",
                emptyHint = "还没有外出模式关键词（空名单不按关键词过滤）",
            ) { v -> update { it.copy(outingKeywords = v) } }
        }
        SettingBlock(
            "外出模式社区",
            "贴吧吧名、Reddit 版块、知乎问题、话题、社区",
            detail = "贴吧的吧名、Reddit 版块（r/…）、知乎问题、酷安 / 抖音话题、小黑盒社区；" +
                "只在外出模式开着时生效，命中就整条隐藏。",
        ) {
            WordListEditor(
                words = s.outingCommunities,
                placeholder = "社区名，例如 原神 或 r/China_irl",
                emptyHint = "还没有社区（空名单不按社区过滤）",
            ) { v -> update { it.copy(outingCommunities = v) } }
        }
        // 说明压成一行，口径细节收进「?」
        Hint(
            "关键词按子串、社区按全名；改完首页 / 关注会自动刷新一次",
            detail = "匹配不分大小写和全半角：关键词按子串、社区按全名（“原神”不会命中“原神内鬼吧”）。" +
                "繁体 / 简体这一版还分不开。改了名单后首页 / 关注会自动刷新一次。",
        )
    }
}

/**
 * 一份名单的编辑器：输入框 + 「添加」+ 已有条目的标签（点 ✕ 删掉）。
 *
 * 和弹幕的屏蔽词编辑器同一个路子（[feedhub.app.video.DanmakuSettingsPanel] 里的那个），但这里只有关键词、
 * 没有正则：内容过滤的关键词是给标题 / 摘要用的，正则在这个场景容易误伤。限制和读存档时一样：
 * 每条最多 [ContentFilter.MAX_RULE_LENGTH] 个字、最多 [ContentFilter.MAX_RULES] 条、不重复（忽略大小写 / 全半角）。
 */
@Composable
private fun WordListEditor(words: List<String>, placeholder: String, emptyHint: String, onChange: (List<String>) -> Unit) {
    var text by remember { mutableStateOf("") }
    val tooLong = text.trim().length > ContentFilter.MAX_RULE_LENGTH
    val full = words.size >= ContentFilter.MAX_RULES
    // 归一化在 Rust：输入框 + 已有条目**一次 FFI 算完**（最多 201 条），`remember` 包住——每次重组不重算，
    // 只在敲字 / 名单变化时算一次。逐条调 `normalize` 的话这里每次重组要过 200 次边界。
    val (normalized, duplicate) = remember(text, words) {
        val all = ContentFilter.normalizeMany(listOf(text) + words)
        val entered = all.firstOrNull().orEmpty()
        entered to (entered.isNotEmpty() && all.drop(1).any { it == entered })
    }
    val canAdd = text.isNotBlank() && !tooLong && !full && !duplicate
    fun add() {
        if (!canAdd) return
        onChange(ContentFilter.normalizeRules(words + text.trim()))
        text = ""
    }
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Row(verticalAlignment = Alignment.Top) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                isError = tooLong || full,
                placeholder = { Text(placeholder) },
                supportingText = when {
                    tooLong -> ({ Text("太长了，最多 ${ContentFilter.MAX_RULE_LENGTH} 个字") })
                    full -> ({ Text("已经 ${ContentFilter.MAX_RULES} 条了，删掉一些再加") })
                    duplicate -> ({ Text("名单里已经有了") })
                    else -> null
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { add() }),
                textStyle = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(4.dp))
            TextButton(onClick = ::add, enabled = canAdd, modifier = Modifier.padding(top = 6.dp)) { Text("添加") }
        }
        if (words.isEmpty()) {
            Hint(emptyHint)
        } else {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for (word in words) WordChip(word) { onChange(words - word) }
            }
            Hint("已加 ${words.size} 条" + if (full) "（满了）" else "")
        }
    }
}

/** 名单里的一条：文字 + 一个 ✕（和弹幕屏蔽词每条一行的样子不同，这里排成能折行的标签，省高度）。 */
@Composable
private fun WordChip(word: String, onRemove: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(shape = RoundedCornerShape(8.dp), color = colors.surfaceContainer) {
        Row(
            Modifier.padding(start = 10.dp, end = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                word,
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            IconButton(onClick = onRemove, modifier = Modifier.size(28.dp)) {
                Icon(Icons.Filled.Close, contentDescription = "删掉「$word」", modifier = Modifier.size(14.dp), tint = colors.onSurfaceVariant)
            }
        }
    }
}
