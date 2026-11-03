package feedhub.app.mine

import feedhub.app.LocalPlatforms
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.app.feed.FeedCard
import feedhub.app.library.ExposureSurface
import feedhub.app.library.Library
import feedhub.app.ui.brandColor
import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.feed.ExposureInputs
import feedhub.feed.exposureInputs
import feedhub.app.feed.ExposureTracker
import feedhub.app.feed.ExposureTicker
import feedhub.app.feed.gridVisibleItems

/**
 * "我的"里一个入口的页面外框：顶上一行（窄屏有返回箭头）+ 标题 + 右边的操作，下面是内容。
 * 标题行的样式和首页 / 关注页一致。
 */
@Composable
fun PaneScaffold(
    title: String,
    onBack: (() -> Unit)?,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    actions: @Composable RowScope.() -> Unit = {},
    content: @Composable ColumnScope.() -> Unit,
) {
    // imePadding：历史记录的搜索框在页面顶部，键盘弹出时让下面的内容缩上去（输入框本身不会被挡住）；
    // 别的子页没有输入框，IME 为 0 时等于没加。桌面没有 IME。
    Column(modifier.fillMaxSize().imePadding()) {
        Row(
            Modifier.fillMaxWidth().padding(start = if (onBack == null) 16.dp else 4.dp, end = 8.dp, top = 8.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (onBack != null) {
                IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回") }
            }
            Text(title, style = MaterialTheme.typography.titleLarge)
            if (subtitle != null) {
                Spacer(Modifier.width(10.dp))
                Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Spacer(Modifier.weight(1f))
            actions()
        }
        content()
    }
}

/**
 * 平台筛选：全部 + 各平台，点一下切换；一行横向滚动（窄屏不换行占地方）。[trailing] 接在后面（收藏页放类型筛选）。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PlatformFilter(
    selected: Platform?,
    onSelect: (Platform?) -> Unit,
    modifier: Modifier = Modifier,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    Row(modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        FilterChip(selected = selected == null, onClick = { onSelect(null) }, label = { Text("全部平台") })
        for (p in LocalPlatforms.current.platforms) {
            FilterChip(
                selected = selected == p,
                onClick = { onSelect(if (selected == p) null else p) },
                label = { Text(p.label) },
                leadingIcon = { Box(Modifier.size(8.dp).background(p.brandColor(), CircleShape)) },
            )
        }
        trailing()
    }
}

@Composable
fun EmptyHint(text: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
        Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
    }
}

/** 确认框。[destructive] 时确认按钮用红色（删除、取关、清空这类）。 */
@Composable
fun ConfirmDialog(
    title: String,
    text: String,
    confirmText: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    dismissText: String = "取消",
    destructive: Boolean = true,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(confirmText, color = if (destructive) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(dismissText) } },
    )
}

/**
 * 「从 Chrome 导入」的确认框（账号页和「同步到手机」里的导入按钮共用，文案不各写一份）。
 * 导入会替换本机这个平台的 Cookie，所以必须先确认。
 */
@Composable
fun ImportConfirmDialog(platformLabel: String, browserName: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    ConfirmDialog(
        title = "从 $browserName 导入${platformLabel}登录信息？",
        text = "会用 $browserName 里${platformLabel}的 Cookie 替换本机保存的。$browserName 里要先登录好${platformLabel}。",
        confirmText = "导入",
        onConfirm = onConfirm,
        onDismiss = onDismiss,
    )
}

/** 输入一个名字的对话框（新建 / 重命名收藏夹）。[validate] 返回错误说明，null = 可以。 */
@Composable
fun NameDialog(
    title: String,
    initial: String,
    confirmText: String,
    validate: (String) -> String?,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by remember { mutableStateOf(initial) }
    val error = if (name.isBlank()) null else validate(name.trim())
    val ok = name.isNotBlank() && error == null
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it.take(30) },
                singleLine = true,
                isError = error != null,
                supportingText = error?.let { { Text(it) } },
                label = { Text("收藏夹名字") },
            )
        },
        confirmButton = { TextButton(onClick = { onConfirm(name.trim()) }, enabled = ok) { Text(confirmText) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

/**
 * 卡片 + 卡片下面一小行（[footer] 是左边的说明，右边是 [action] 小按钮：收藏页的"⋯"、历史页的"×"）。
 * 按钮常驻、不靠长按 / 右键，触屏也能用；放在卡片外面，不挡卡片上的平台标签。[overlay] 画在卡片上（例如播放进度）。
 */
@Composable
fun ActionCard(
    item: FeedItem,
    onOpen: () -> Unit,
    footer: String,
    modifier: Modifier = Modifier,
    overlay: @Composable BoxScope.() -> Unit = {},
    action: @Composable BoxScope.() -> Unit,
) {
    Column(modifier) {
        Box {
            FeedCard(item, onClick = onOpen, modifier = Modifier.fillMaxWidth())
            overlay()
        }
        Row(Modifier.fillMaxWidth().padding(start = 6.dp, top = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                footer,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Box { action() }
        }
    }
}

/** 卡片下面一行右边的小图标按钮（32dp，比默认的 48dp 紧凑）。 */
@Composable
fun SmallIconButton(onClick: () -> Unit, description: String, icon: ImageVector) {
    Box(
        Modifier.size(32.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = description, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
    }
}

/** 列表行里的一行文字：单行省略。 */
@Composable
fun OneLine(text: String, modifier: Modifier = Modifier, muted: Boolean = false) {
    Text(
        text,
        style = if (muted) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
        color = if (muted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}

/** 宽屏（平板横屏、桌面）左右两栏，窄屏一列的分界宽度。 */
val MineWideBreakpoint = 720.dp

/**
 * "我的"里的瀑布流卡片也记曝光（收藏页 / 历史页，面由 [surface] 定）：和首页 / 关注页同一套判定
 * （[ExposureTracker]）——卡片可见面积 ≥ 50% 且连续 ≥ 1 秒算一次，滚出视口 / 被全屏详情盖住 / 窗口失焦时结算。
 * 纯本地，不发请求。
 *
 * [items] 必须和网格里的**下标一一对应**：历史页的日期分组头也占一个下标，调用方要在那张表里补 `null`。
 * [exposure] 是界面算好的两个入参（[ExposureInputs]，`App.kt` 的 `exposureInputs`）：`enabled` 是设置里的开关
 * （关掉 = 丢弃），`foreground` 是"这个页签真的在被看着"（被全屏详情盖住 / 失焦 = 结算）。两者语义不同。
 */
@Composable
fun TrackExposures(
    grid: LazyStaggeredGridState,
    items: List<FeedItem?>,
    surface: ExposureSurface,
    library: Library,
    exposure: ExposureInputs,
) {
    val inputs = rememberUpdatedState(exposure)
    val current = rememberUpdatedState(items)
    val tracker = remember(library, surface) { ExposureTracker(library, surface, enabled = { inputs.value.enabled }) }
    ExposureTicker(tracker, foreground = inputs.value.foreground) { gridVisibleItems(grid, current.value) }
}
