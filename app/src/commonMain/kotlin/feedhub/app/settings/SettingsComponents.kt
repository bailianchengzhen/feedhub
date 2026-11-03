package feedhub.app.settings

import feedhub.app.LocalPlatforms
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.HelpOutline
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
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
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import feedhub.app.ui.brandColor
import feedhub.core.Platform

/** 设置页的一张卡片：标题 + 若干设置项（项之间留空）。 */
@Composable
fun SettingsCard(title: String, modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Surface(
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLowest,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
            content()
        }
    }
}

/**
 * 一个设置项：上面是名称（和一行说明），下面是控件。
 *
 * [detail] 不为空时，[hint] 只占**一行**（超出省略），细节收进右边那个「?」。
 */
@Composable
fun SettingBlock(label: String, hint: String? = null, detail: String? = null, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Column {
            Text(label, style = MaterialTheme.typography.bodyMedium)
            if (hint != null) Hint(hint, detail = detail)
        }
        content()
    }
}

/** 同一个设置项，但卡片够宽时名称在左、控件在右，省一行高度。 */
@Composable
fun SettingInline(label: String, hint: String? = null, detail: String? = null, controlWidth: Dp = 250.dp, content: @Composable () -> Unit) {
    BoxWithConstraints {
        if (maxWidth >= 420.dp) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(label, style = MaterialTheme.typography.bodyMedium)
                    if (hint != null) Hint(hint, detail = detail)
                }
                Spacer(Modifier.width(12.dp))
                Box(Modifier.width(controlWidth)) { content() }
            }
        } else {
            SettingBlock(label, hint, detail, content)
        }
    }
}

/**
 * 设置项下面那行小字。
 *
 * [detail] 不为空时只显示**一行**（超出省略），右边一个「?」点开看全文。
 * 为空时照旧按需折行（短句用不着弹框）。[text] 本身已经是一句完整的话，[detail] 里放"为什么 / 边界情况"。
 */
@Composable
fun Hint(text: String, modifier: Modifier = Modifier, detail: String? = null) {
    val style = MaterialTheme.typography.bodySmall
    val color = MaterialTheme.colorScheme.onSurfaceVariant
    if (detail.isNullOrBlank()) {
        Text(text, style = style, color = color, modifier = modifier)
        return
    }
    var open by remember { mutableStateOf(false) }
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        Text(text, style = style, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
        IconButton(onClick = { open = true }, modifier = Modifier.size(28.dp)) {
            Icon(Icons.Outlined.HelpOutline, contentDescription = "说明", modifier = Modifier.size(16.dp), tint = color)
        }
    }
    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text("说明") },
            text = { Text(detail, style = MaterialTheme.typography.bodyMedium) },
            confirmButton = { TextButton(onClick = { open = false }) { Text("知道了") } },
        )
    }
}

/**
 * 子页的标题栏：窄屏带返回箭头，宽屏不需要（分组列表一直在左边）。
 * 样式和信息流 / 「我的」的标题行一致。
 */
@Composable
fun SubPageHeader(title: String, onBack: (() -> Unit)?, modifier: Modifier = Modifier, actions: @Composable RowScope.() -> Unit = {}) {
    Row(
        modifier.fillMaxWidth().padding(start = if (onBack == null) 16.dp else 4.dp, end = 8.dp, top = 8.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (onBack != null) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回") }
        }
        Text(title, style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.weight(1f))
        actions()
    }
}

/**
 * 一行"去某处改"的入口：点了**直接跳到那个子页**。
 *
 * 为什么要有它：有几个设置项被挪进了「进阶设置」，「常规」那一页上不能什么都不说（用户会以为设置丢了）；
 * 留一句话 + 一个能点的入口，比"去进阶设置里找"省事。样式压得很矮（一行、主题色 + `›`），
 * 不抢设置项的注意力。
 */
@Composable
internal fun SettingsJump(text: String, target: SettingsRoute, nav: SettingsNav, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth().clickable { nav.open(target) }.padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.weight(1f),
        )
        Text("›", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
    }
}

/** 开关项：名称（和说明）在左，开关在右。 */
@Composable
fun SwitchRow(label: String, hint: String? = null, checked: Boolean, onChange: (Boolean) -> Unit) {
    SwitchRow(label, hint, checked, enabled = true, onChange = onChange)
}

/** 开关项，多一个 [enabled] 控制开关本身（例如安卓没有本地翻译）。 */
@Composable
fun SwitchRow(
    label: String,
    hint: String? = null,
    checked: Boolean,
    enabled: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyMedium)
            if (hint != null) Hint(hint)
        }
        Spacer(Modifier.width(12.dp))
        Switch(checked = checked, onCheckedChange = onChange, enabled = enabled)
    }
}

/** 分段按钮：几个选项里选一个。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun <T> SegmentedChoice(options: List<Pair<T, String>>, selected: T, enabled: Boolean = true, onSelect: (T) -> Unit) {
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        options.forEachIndexed { i, (value, label) ->
            SegmentedButton(
                selected = value == selected,
                onClick = { onSelect(value) },
                shape = SegmentedButtonDefaults.itemShape(i, options.size),
                enabled = enabled,
                label = { Text(label, maxLines = 1) },
            )
        }
    }
}

/**
 * 平台标签行：点一下切换。[supported] 为 false 的平台是灰的、点不动（例如小黑盒没有关注流，
 * 或者安卓上的 YouTube：插件描述的 targets 里没有安卓）。[enabled] 为 false 的平台也是灰的（暂时用不了，附带一句原因）。
 * 至少要留一个能用的平台选中，最后一个取消选中会被忽略。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PlatformChips(
    selected: Set<Platform>,
    supported: (Platform) -> Boolean,
    enabled: (Platform) -> Boolean = { true },
    onChange: (Set<Platform>) -> Unit,
) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for (p in LocalPlatforms.current.platforms) {
            val can = enabled(p)
            val ok = can && supported(p)
            PlatformChip(p, on = ok && p in selected, enabled = ok) {
                val next = if (p in selected) selected - p else selected + p
                if (next.any { enabled(it) && supported(it) }) onChange(next)
            }
        }
    }
}

/**
 * 和 [PlatformChips] 同一样式的平台标签行，但**允许一个都不选**（也允许全不选）。
 * 设置页「翻译 → 自动翻译的平台」用它。
 * 国内七家默认不点亮，境外平台（X、YouTube、Reddit）默认点亮。
 * [enabled] 为 false 的平台灰掉（这台机器上暂时用不了的平台，点亮了也没用）。
 */
@Composable
fun PlatformToggleChips(selected: Set<Platform>, enabled: (Platform) -> Boolean = { true }, onChange: (Set<Platform>) -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for (p in LocalPlatforms.current.platforms) {
            val can = enabled(p)
            PlatformChip(p, on = can && p in selected, enabled = can) {
                onChange(if (p in selected) selected - p else selected + p)
            }
        }
    }
}

/** 一排里的一张平台标签：平台名 + 平台色点（[PlatformChips] 和 [PlatformToggleChips] 共用这一种样式）。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PlatformChip(platform: Platform, on: Boolean, enabled: Boolean, onClick: () -> Unit) {
    FilterChip(
        selected = on,
        enabled = enabled,
        onClick = onClick,
        label = { Text(platform.label) },
        leadingIcon = {
            Box(
                Modifier
                    .size(8.dp)
                    .background(if (enabled) platform.brandColor() else MaterialTheme.colorScheme.outlineVariant, CircleShape),
            )
        },
    )
}
