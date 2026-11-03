package feedhub.app.video

import feedhub.player.RuleKind
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalMinimumInteractiveComponentSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.app.settings.Hint
import feedhub.app.settings.SegmentedChoice
import feedhub.app.settings.SwitchRow
import kotlin.math.roundToInt

private enum class PanelTab(val label: String) {
    STYLE("样式"),
    LAYOUT("排布"),
    BLOCK("屏蔽"),
}

/**
 * 弹幕设置面板：播放器上的"弹幕设置"按钮（全屏时也是）和设置页的"弹幕设置"入口打开的是同一个面板。
 * 三页：样式（透明度、字号、字重、描边）、排布（显示区域、行高、海量模式、滚动 / 停留时间、固定像素速度、顶部底部转滚动）、
 * 屏蔽（类型、智能云屏蔽、合并重复、屏蔽词）。选项照 BiliPai 的弹幕设置面板，含义见 [DanmakuSettings]。
 *
 * 每改一下就调 [onChange]（滑块是一档一档的，拖一下最多改十几次），由调用方立刻生效、存起来。
 * 大小由 [modifier] 定（要给一个最大高度，内容放不下就在面板里滚）。[notice] 放在标题下面（播放器用它提示"弹幕现在是关着的"），
 * [preview] 放在标签页上面（设置页没有视频，放一块示意用的小画面）。
 */
@Composable
fun DanmakuSettingsPanel(
    settings: DanmakuSettings,
    onChange: (DanmakuSettings) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    notice: (@Composable () -> Unit)? = null,
    preview: (@Composable () -> Unit)? = null,
) {
    var tab by remember { mutableStateOf(PanelTab.STYLE) }
    Surface(
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLowest,
        shadowElevation = 8.dp,
        modifier = modifier,
    ) {
        Column(Modifier.padding(start = 16.dp, end = 12.dp, top = 8.dp, bottom = 14.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("弹幕设置", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                TextButton(onClick = { onChange(settings.reset()) }, enabled = settings.reset() != settings) { Text("恢复默认") }
                IconButton(onClick = onClose, modifier = Modifier.size(32.dp)) {
                    Icon(Icons.Filled.Close, contentDescription = "关闭", modifier = Modifier.size(20.dp))
                }
            }
            notice?.invoke()
            preview?.invoke()
            SegmentedChoice(PanelTab.entries.map { it to it.label }, tab) { tab = it }
            Column(
                Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()).padding(end = 4.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                when (tab) {
                    PanelTab.STYLE -> StyleTab(settings, onChange)
                    PanelTab.LAYOUT -> LayoutTab(settings, onChange)
                    PanelTab.BLOCK -> BlockTab(settings, onChange)
                }
            }
        }
    }
}

@Composable
private fun StyleTab(s: DanmakuSettings, onChange: (DanmakuSettings) -> Unit) {
    SliderRow("透明度", "${(s.opacity * 100).roundToInt()}%", s.opacity, DanmakuSettings.OPACITY, 0.05f) { onChange(s.copy(opacity = it)) }
    SliderRow("字号", "${(s.fontScale * 100).roundToInt()}%", s.fontScale, DanmakuSettings.FONT_SCALE, 0.1f, "在跟着播放器大小走的字号上再放大缩小") {
        onChange(s.copy(fontScale = it))
    }
    SliderRow("字重", "${s.fontWeight}00", s.fontWeight.toFloat(), 1f..9f, 1f, "数字越大越粗；字体没有那么多档时，会用最接近的") {
        onChange(s.copy(fontWeight = it.roundToInt()))
    }
    SliderRow("描边", if (s.strokeWidth == 0f) "无" else "${oneDecimal(s.strokeWidth)} dp", s.strokeWidth, DanmakuSettings.STROKE_WIDTH, 0.5f) {
        onChange(s.copy(strokeWidth = it))
    }
}

@Composable
private fun LayoutTab(s: DanmakuSettings, onChange: (DanmakuSettings) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("显示区域", style = MaterialTheme.typography.bodyMedium)
        for (row in DanmakuSettings.AREA_OPTIONS.chunked(4)) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (area in row) OptionChip(areaLabel(area), area == s.area, Modifier.weight(1f)) { onChange(s.copy(area = area)) }
            }
        }
        Hint("弹幕最多占画面高度的多少。区域太小时，顶部、底部弹幕会排不下")
    }
    SliderRow("行高", oneDecimal(s.lineHeight), s.lineHeight, DanmakuSettings.LINE_HEIGHT, 0.1f) { onChange(s.copy(lineHeight = it)) }
    SwitchRow("海量模式", "行距压紧，同样的区域多排约三分之一的轨道", s.massive) { onChange(s.copy(massive = it)) }
    SliderRow("滚动时长", seconds(s.scrollSeconds), s.scrollSeconds, DanmakuSettings.SECONDS, 0.5f, "滚动弹幕从出现到消失的时间，越短越快") {
        onChange(s.copy(scrollSeconds = it))
    }
    SwitchRow("固定像素速度", "播放器越宽滚得越久，放大前后每秒走的距离差不多", s.fixedVelocity) { onChange(s.copy(fixedVelocity = it)) }
    SliderRow("顶部 / 底部停留", seconds(s.fixedSeconds), s.fixedSeconds, DanmakuSettings.SECONDS, 0.5f) { onChange(s.copy(fixedSeconds = it)) }
    SwitchRow("顶部 / 底部弹幕改成滚动", "它们不再停在中间，跟着滚动弹幕一起走", s.staticToScroll) { onChange(s.copy(staticToScroll = it)) }
}

@Composable
private fun BlockTab(s: DanmakuSettings, onChange: (DanmakuSettings) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("显示这些弹幕", style = MaterialTheme.typography.bodyMedium)
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            OptionChip("滚动", s.allowScroll, Modifier.weight(1f)) { onChange(s.copy(allowScroll = !s.allowScroll)) }
            OptionChip("顶部", s.allowTop, Modifier.weight(1f)) { onChange(s.copy(allowTop = !s.allowTop)) }
            OptionChip("底部", s.allowBottom, Modifier.weight(1f)) { onChange(s.copy(allowBottom = !s.allowBottom)) }
            OptionChip("彩色", s.allowColorful, Modifier.weight(1f)) { onChange(s.copy(allowColorful = !s.allowColorful)) }
        }
        Hint("点一下屏蔽这一类，再点恢复。高级弹幕本来就不显示")
    }
    SliderRow(
        "智能云屏蔽", if (s.weightLevel == 0) "关" else "${s.weightLevel} 级", s.weightLevel.toFloat(), 0f..10f, 1f,
        "平台给每条弹幕打了权重，低于这个等级的不显示；没有权重的照常显示",
    ) { onChange(s.copy(weightLevel = it.roundToInt())) }
    SwitchRow("合并重复弹幕", "短时间内同样的内容并成一条", s.mergeDuplicates) { onChange(s.copy(mergeDuplicates = it)) }
    if (s.mergeDuplicates) {
        SliderRow("合并窗口", "${s.mergeWindowMs} 毫秒", s.mergeWindowMs.toFloat(), 100f..3000f, 100f, "相邻两条相隔不超过它，就算同一批") {
            onChange(s.copy(mergeWindowMs = it.roundToInt()))
        }
        SliderRow("计数阈值", "${s.mergeThreshold} 条", s.mergeThreshold.toFloat(), 2f..10f, 1f, "一批至少这么多条，合并后才标 x3 这样的数") {
            onChange(s.copy(mergeThreshold = it.roundToInt()))
        }
    }
    BlockWords(s.blockRules) { onChange(s.copy(blockRules = it)) }
}

/** 屏蔽词：输入框（回车或点"添加"）+ 已有的屏蔽词列表（点叉删掉）。正则写得不对的标红，加不进去。 */
@Composable
private fun BlockWords(rules: List<String>, onChange: (List<String>) -> Unit) {
    var text by remember { mutableStateOf("") }
    val kind = BlockRules.kindOf(text)
    val tooLong = text.trim().length > DanmakuSettings.MAX_RULE_LENGTH
    val canAdd = text.isNotBlank() && kind != RuleKind.INVALID && !tooLong
    fun add() {
        if (!canAdd) return
        val rule = text.trim()
        if (rule !in rules && rules.size < DanmakuSettings.MAX_RULES) onChange(rules + rule)
        text = ""
    }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("屏蔽词", style = MaterialTheme.typography.bodyMedium)
        Hint("包含这些词的弹幕不显示，只存在这台机器上。正则写成 regex:第\\d+集 或 /第\\d+集/")
        Row(verticalAlignment = Alignment.Top) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                isError = kind == RuleKind.INVALID || tooLong,
                placeholder = { Text("关键词，或 regex:正则") },
                supportingText = when {
                    kind == RuleKind.INVALID -> ({ Text("这条正则写得不对（或者会匹配所有弹幕），加了也不会起作用") })
                    tooLong -> ({ Text("太长了，最多 ${DanmakuSettings.MAX_RULE_LENGTH} 个字") })
                    kind == RuleKind.REGEX -> ({ Text("按正则匹配，不分大小写") })
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
        if (rules.isEmpty()) {
            Hint("还没有屏蔽词")
        } else {
            val kinds = remember(rules) { rules.map(BlockRules::kindOf) }
            rules.forEachIndexed { i, rule -> RuleRow(rule, kinds[i]) { onChange(rules - rule) } }
        }
    }
}

@Composable
private fun RuleRow(rule: String, kind: RuleKind, onRemove: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).background(colors.surfaceContainer).padding(start = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (kind != RuleKind.KEYWORD) {
            Text(
                if (kind == RuleKind.REGEX) "正则" else "无效",
                style = MaterialTheme.typography.labelSmall,
                color = if (kind == RuleKind.REGEX) colors.primary else colors.error,
                modifier = Modifier.padding(end = 8.dp),
            )
        }
        Text(
            rule,
            style = MaterialTheme.typography.bodyMedium,
            color = if (kind == RuleKind.INVALID) colors.error else colors.onSurface,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        IconButton(onClick = onRemove, modifier = Modifier.size(32.dp)) {
            Icon(Icons.Filled.Close, contentDescription = "删除", modifier = Modifier.size(16.dp), tint = colors.onSurfaceVariant)
        }
    }
}

/** 一行滑块：名称在左、当前值在右，下面是一档一档的滑块（[step] 是一档多大）。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SliderRow(
    label: String,
    valueText: String,
    value: Float,
    range: ClosedFloatingPointRange<Float>,
    step: Float,
    hint: String? = null,
    onChange: (Float) -> Unit,
) {
    Column {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            Text(valueText, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
        }
        // 面板里滑块多，去掉最小点击区域（48dp），不然一页放不下几个
        val colors = SliderDefaults.colors(inactiveTrackColor = MaterialTheme.colorScheme.surfaceVariant)
        CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 0.dp) {
            Slider(
                value = value.coerceIn(range),
                onValueChange = { onChange(snapToStep(it, range.start, step)) },
                valueRange = range,
                steps = ((range.endInclusive - range.start) / step).roundToInt() - 1,
                modifier = Modifier.fillMaxWidth().height(28.dp),
                colors = colors,
                // 刻度点一排，太碎，只留滑块
                track = { SliderDefaults.Track(it, colors = colors, drawTick = { _, _ -> }, drawStopIndicator = null) },
            )
        }
        if (hint != null) Hint(hint)
    }
}

/** 一个可以点选的小块；[selected] 的高亮。 */
@Composable
private fun OptionChip(label: String, selected: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val shape = RoundedCornerShape(8.dp)
    Box(
        modifier.height(32.dp).clip(shape)
            .background(if (selected) colors.primaryContainer else colors.surfaceContainer)
            .border(1.5.dp, if (selected) colors.primary else Color.Transparent, shape)
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            color = if (selected) colors.onPrimaryContainer else colors.onSurfaceVariant,
        )
    }
}

/** 把滑块给的值对齐到 [start] 起、每格 [step] 的刻度上（去掉 0.35000002 这样的浮点误差）。 */
internal fun snapToStep(value: Float, start: Float, step: Float): Float {
    val n = ((value - start) / step).roundToInt()
    return ((start + n * step) * 1000f).roundToInt() / 1000f
}

/** 显示区域的叫法：1/8、1/4、3/8、1/2、5/8、3/4、7/8、全屏。 */
internal fun areaLabel(area: Float): String = when (val eighths = (area * 8).roundToInt()) {
    8 -> "全屏"
    2 -> "1/4"
    4 -> "1/2"
    6 -> "3/4"
    else -> "$eighths/8"
}

private fun oneDecimal(v: Float): String = ((v * 10).roundToInt() / 10f).toString()

/** 3.5 → "3.5 秒"，8.0 → "8 秒"。 */
private fun seconds(v: Float): String = if (v == v.toInt().toFloat()) "${v.toInt()} 秒" else "${oneDecimal(v)} 秒"
