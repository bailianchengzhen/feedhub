package feedhub.app.mine

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.ui.PlatformBackHandler
import feedhub.app.ui.brandColor
import feedhub.core.FeedItem
import feedhub.feed.ExposureInputs
import feedhub.feed.exposureInputs

/**
 * "我的"：宽屏（平板横屏 / 桌面）左栏是账号和入口、右栏显示选中项；窄屏是一列入口，点进去全屏（左上角返回）。
 * 选中的入口和筛选条件在 [feedhub.app.mine.MineState]（挂在 AppGraph 上），打开详情再返回时还在。
 * [onOpen] 是 App 里"打开详情"的回调，收藏和历史的卡片用。
 *
 * [exposure] 是行为数据（曝光）的两个入参（[ExposureInputs]），照 `App.kt` 的约定（`exposureInputs` 算好传进来）：
 * `enabled` 是设置里的开关（关掉 = 丢弃），`foreground` 是"这个页签真的在被看着"（被全屏详情盖住 / 失焦 = 结算）。
 */
@Composable
fun MineScreen(
    graph: AppGraph,
    onOpen: (FeedItem) -> Unit,
    exposure: ExposureInputs = ExposureInputs(),
    /** 点当前页签的次数（`App.kt` 的 [feedhub.app.TabReselectState]）：数一变，入口列表回顶。 */
    reselectSignal: Int = 0,
) {
    val state = graph.mine
    LaunchedEffect(Unit) { state.accounts.ensureChecked() }
    // 入口列表的滚动位置：宽 / 窄两支共用一份（"点当前页签回顶"用它）
    val menuScroll = rememberScrollState()
    var handledReselect by remember { mutableIntStateOf(reselectSignal) }
    LaunchedEffect(reselectSignal) {
        if (reselectSignal != handledReselect) {
            handledReselect = reselectSignal
            menuScroll.animateScrollTo(0)
        }
    }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val wide = maxWidth >= MineWideBreakpoint
        if (wide) {
            val selected = state.section ?: MineSection.ACCOUNTS
            Row(Modifier.fillMaxSize()) {
                Column(Modifier.width(300.dp).fillMaxHeight().verticalScroll(menuScroll)) {
                    Text("我的", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp))
                    MineMenu(graph, selected, wide = true) { state.section = it }
                }
                VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Box(Modifier.weight(1f).fillMaxHeight()) { MinePane(graph, selected, onOpen, onBack = null, exposure) }
            }
        } else {
            val selected = state.section
            if (selected == null) {
                Column(Modifier.fillMaxSize().verticalScroll(menuScroll)) {
                    Text("我的", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp))
                    MineMenu(graph, null, wide = false) { state.section = it }
                }
            } else {
                // 系统返回 = 回「我的」入口列表（不是退出 App）：和页面上那个左上返回箭头同一条路
                PlatformBackHandler(enabled = mineSectionBackEnabled(selected != null, wide)) { state.section = null }
                MinePane(graph, selected, onOpen, onBack = { state.section = null }, exposure)
            }
        }
    }
}

/**
 * 「我的」子页要不要拦系统返回（纯逻辑，`NavBackTest` 离线测）：**只有窄屏的单栏子页**有"上一层"
 * （回到入口列表）；宽屏是左边入口 + 右边内容，列表一直在，返回交给外层（页签 → 首页）。
 */
internal fun mineSectionBackEnabled(sectionOpen: Boolean, wide: Boolean): Boolean = sectionOpen && !wide

@Composable
private fun MinePane(
    graph: AppGraph,
    section: MineSection,
    onOpen: (FeedItem) -> Unit,
    onBack: (() -> Unit)?,
    exposure: ExposureInputs,
) {
    val state = graph.mine
    when (section) {
        MineSection.ACCOUNTS -> AccountsPane(graph, state.accounts, onBack)
        MineSection.FAVORITES -> FavoritesPane(state, graph.library, onOpen, onBack, exposure)
        MineSection.HISTORY -> HistoryPane(state, graph.library, onOpen, onBack, exposure)
        MineSection.FOLLOWING -> FollowingPane(state, onBack)
        MineSection.BLOCKED -> BlockedPane(graph.library, onBack)
    }
}

/** 账号小结（五个平台各一行）+ 入口列表。 */
@Composable
private fun MineMenu(graph: AppGraph, selected: MineSection?, wide: Boolean, onSelect: (MineSection) -> Unit) {
    val state = graph.mine
    val library = graph.library
    Column(Modifier.padding(horizontal = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        AccountSummary(state.accounts, selected == MineSection.ACCOUNTS) { onSelect(MineSection.ACCOUNTS) }
        val historyCount by produceState(0, library.historyRevision) { value = library.historyCount() }
        for (s in MineSection.entries) {
            if (s == MineSection.ACCOUNTS) continue
            val subtitle = when (s) {
                MineSection.FAVORITES -> "${library.favoriteCount()} 条"
                MineSection.HISTORY -> "$historyCount 条"
                MineSection.FOLLOWING -> state.followSources.joinToString(" · ") { it.platform.label }
                MineSection.BLOCKED -> "${library.blockedUsers.size} 人"
                MineSection.ACCOUNTS -> ""
            }
            MenuRow(s, subtitle, selected == s) { onSelect(s) }
        }
        Spacer(Modifier.size(12.dp))
    }
}

@Composable
private fun AccountSummary(accounts: AccountsState, selected: Boolean, onClick: () -> Unit) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = if (selected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainerLowest,
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    ) {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Text("账号", style = MaterialTheme.typography.titleSmall)
            for (p in accounts.platforms) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(8.dp).background(p.brandColor(), CircleShape))
                    Spacer(Modifier.width(8.dp))
                    Text(p.label, style = MaterialTheme.typography.bodySmall, modifier = Modifier.width(48.dp))
                    when (val st = accounts.status[p]) {
                        null, AccountStatus.Checking -> CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                        is AccountStatus.LoggedIn -> OneLine(st.user.name, Modifier.weight(1f))
                        is AccountStatus.LoggedOut ->
                            Text(if (st.expired) "登录已失效" else "未登录", style = MaterialTheme.typography.bodySmall, color = if (st.expired) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
                        is AccountStatus.Failed -> Text("检查失败", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
    }
}

@Composable
private fun MenuRow(section: MineSection, subtitle: String, selected: Boolean, onClick: () -> Unit) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = if (selected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainerLowest,
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    ) {
        Row(Modifier.padding(horizontal = 14.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(section.icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.width(12.dp))
            Text(section.label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
            Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

