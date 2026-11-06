package feedhub.app.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Article
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.FilterAlt
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.Palette
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material.icons.outlined.Translate
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import coil3.SingletonImageLoader
import coil3.compose.LocalPlatformContext
import feedhub.app.AppGraph
import feedhub.app.ContentType
import feedhub.app.DefaultQuality
import feedhub.app.DetailMode
import feedhub.app.feed.FeedOrder
import feedhub.app.HomeColumnsMode
import feedhub.app.LocalHomeColumns
import feedhub.app.MAX_HOME_COLUMNS
import feedhub.app.MAX_PER_PLATFORM_CHOICES
import feedhub.app.MIN_HOME_COLUMNS
import feedhub.app.ui.DarkMode
import feedhub.app.ui.PlatformBackHandler
import feedhub.app.video.DanmakuPreview
import feedhub.app.video.DanmakuSettings
import feedhub.app.video.DanmakuSettingsPanel
import feedhub.app.video.areaLabel
import feedhub.core.FeedKind
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 设置页：**分组列表 → 子页**。
 *
 * - 手机竖屏：一页分组列表（[SettingsMenu]），点一项进子页；子页顶部是标题栏 + 返回箭头，系统返回也回列表
 *   （再往外是"设置 → 首页"，在 `App.kt` 那层）。
 * - 平板横屏 / 桌面（宽度 ≥ [SettingsWideBreakpoint]）：左边分组列表、右边子页内容（双栏），列表常驻。
 *
 * 只是从"一页放完"改成"一项一页"：
 * 信息流 / 内容过滤 / 详情页 / 外观 / 播放与弹幕 / 翻译（本地）/ 网络与代理 / 跨设备同步 / 存储 / 关于，
 * 列表最后一项是**进阶设置**（原来页面底部那行）。
 *
 * 层级是"进阶子页 → 进阶列表 → 设置首页"。手机竖屏同样是一屏一页；宽屏左边那栏在进到进阶之后
 * 换成**进阶的列表**（顶上多一行「‹ 设置」回一级），右边还是子页内容——复用同一个列表行 [SettingsMenuRow]、
 * 同一个子页外壳 [SettingsPage]，不另写一套。
 * **每个设置项的行为和存储都没变，只是挪了位置**（[SettingsSectionContent] / [AdvancedSectionContent] 里还是原来那几个
 * [FeedSettings] / [ContentFilterSettings] / … 组合，一个字段都没改）。
 *
 * 层级状态放在 [SettingsNav]（由 `App` 持有）：切到别的页签再回来，还停在原来那一页。
 *
 * @param nav 当前停在哪一页（null = 设置首页；宽屏没选过就是最后一次看的那一组）。
 * @param initialAdvanced 截图 / debug 工具用来直接打开「进阶设置」的**分组列表**（平时都是点了才进）。
 * @param initialAdvancedPage 截图 / debug 工具用来直接打开某个进阶子页（[AdvancedSection.key]；空 = 列表）。
 * @param initialSection 截图工具用来直接打开某个一级分组（[SettingsSection] 的名字，例如 `NETWORK`）。
 * @param initialDialog 截图工具用来直接打开进阶页里的某个弹框（import / subscribe，都在「代理（进阶）」里）。
 */
@Composable
internal fun SettingsScreen(
    graph: AppGraph,
    nav: SettingsNav,
    initialAdvanced: Boolean = false,
    initialAdvancedPage: String? = null,
    initialSection: String? = null,
    initialDialog: String? = null,
    initialSyncPreview: feedhub.app.sync.SyncPreviewDebug? = null,
    initialPairing: feedhub.app.sync.SyncPairingDebug? = null,
    /** 点当前页签的次数（`App.kt` 的 `TabReselectState`）：数一变，分组列表回顶。 */
    reselectSignal: Int = 0,
) {
    // 截图工具 / debug 启动参数：一进来就摆在「进阶设置」（有子页 key 就直接摆那一页）。
    // 只摆一次（rememberSaveable）：用户返回、或者切到别的页签再回来，都不会又自己弹回进阶页。
    var initialOpened by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(initialAdvanced, initialAdvancedPage, initialSection, initialDialog) {
        if (initialOpened) return@LaunchedEffect
        // 认不出的 key 就当没给（入口那边 StartScreen / Screenshot 会先校验并报错，这里别崩）
        val page = initialAdvancedPage?.let { AdvancedSection.of(it) }
        val section = initialSection?.let { key ->
            SettingsSection.entries.firstOrNull { it.name.equals(key.trim(), ignoreCase = true) }
        }
        if (initialAdvanced || page != null || section != null || initialDialog != null) {
            initialOpened = true
            if (nav.route == null) {
                nav.open(
                    page?.let { SettingsRoute.Advanced(it) }
                        ?: section?.let { SettingsRoute.Section(it) }
                        ?: SettingsRoute.AdvancedList,
                )
            }
        }
    }
    // Esc：和系统返回同一个层级（子页 → 进阶列表 → 设置首页）。桌面没有返回键，Esc 是唯一的"往回"；
    // 在设置首页按 Esc 不消费（交给外层，和以前一样什么都不做）。焦点在设置页里（点过任何地方都还在），
    // 所以能一直收到——和详情页的做法一样（见 DetailPane 的 onKeyEvent）。
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    BoxWithConstraints(
        Modifier.fillMaxSize()
            .focusRequester(focus)
            .focusable()
            .onKeyEvent { e ->
                if (e.type == KeyEventType.KeyDown && e.key == Key.Escape) nav.back() else false
            },
    ) {
        val wide = maxWidth >= SettingsWideBreakpoint
        // 系统返回 = 子页回上一层。宽屏一级分组的右边
        // 没有"上一层"（列表一直在），不拦，交给 `App.kt` 那层"设置 → 首页"；但宽屏进到**进阶**之后左边换成的是
        // 进阶列表（一级列表被盖住），所以那一层要拦（见 settingsSubBackEnabled）。
        // 子页这一层注册在 `App` 之后，所以先收到。
        PlatformBackHandler(
            enabled = settingsSubBackEnabled(nav.route != null, wide, advancedLayer = nav.level == SettingsMenuLevel.ADVANCED),
        ) { nav.back() }
        // 分组列表的滚动位置：从子页回来、切页签再回来都还在原位（"点当前页签回顶"也用它，见 `App.kt`）
        val menuScroll = rememberScrollState()
        var handledReselect by remember { mutableIntStateOf(reselectSignal) }
        LaunchedEffect(reselectSignal) {
            if (reselectSignal != handledReselect) {
                handledReselect = reselectSignal
                menuScroll.animateScrollTo(0)
            }
        }
        if (wide) {
            Row(Modifier.fillMaxSize()) {
                Column(Modifier.width(300.dp).fillMaxHeight().verticalScroll(menuScroll)) {
                    // 左边栏画哪一层列表：进阶子页 / 进阶列表时换成进阶那一层（顶上多一行「‹ 设置」）
                    when (nav.level) {
                        SettingsMenuLevel.TOP -> SettingsMenu(graph, nav, selected = nav.shown)
                        SettingsMenuLevel.ADVANCED -> AdvancedMenu(
                            graph, nav, selected = nav.shown,
                            showTitle = true,
                            onBack = { nav.reset() },
                        )
                    }
                }
                VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
                    SettingsPage(graph, nav.shown, nav, showBack = false, initialDialog, initialSyncPreview, initialPairing)
                }
            }
        } else {
            val route = nav.route
            if (route == null) {
                Column(Modifier.fillMaxSize().verticalScroll(menuScroll)) {
                    SettingsMenu(graph, nav, selected = null)
                }
            } else {
                SettingsPage(graph, route, nav, showBack = true, initialDialog, initialSyncPreview, initialPairing)
            }
        }
    }
}

/**
 * 设置子页要不要拦系统返回（纯逻辑，`NavBackTest` 离线测）。
 *
 * - 窄屏单栏：任何一页开着都有"上一层"（一级子页 → 分组列表；进阶子页 → 进阶列表），拦；
 * - 宽屏：一级分组的右边没有"上一层"（左边列表一直在），交给外层（页签 → 首页）——**和以前一样**；
 *   但**进到进阶那一层之后例外**：左边这时换成的是进阶列表，一级列表被盖住了，
 *   所以"进阶子页 → 进阶列表 → 设置首页"这条也得靠返回键走（见 [SettingsNav.level]）。
 */
internal fun settingsSubBackEnabled(subOpen: Boolean, wide: Boolean, advancedLayer: Boolean = false): Boolean =
    subOpen && (!wide || advancedLayer)

/** 宽屏双栏的分界宽度（和「我的」的 `MineWideBreakpoint` 一样）。 */
val SettingsWideBreakpoint = 720.dp

/**
 * 设置页的一级分组：**一项一页**。图标只给分组列表用（子页里不重复画标题图标）。
 * 顺序就是列表里的顺序。
 */
internal enum class SettingsSection(val title: String, val summary: String, val icon: ImageVector) {
    FEED("信息流", "内容类型、平台、列数", Icons.Outlined.Tune),
    CONTENT("内容过滤", "屏蔽词、外出模式", Icons.Outlined.FilterAlt),
    DETAIL("详情页", "打开方式、默认显示原图", Icons.Outlined.Article),
    APPEARANCE("外观", "深色模式、界面缩放", Icons.Outlined.Palette),
    VIDEO("播放与弹幕", "默认清晰度、弹幕", Icons.Outlined.Movie),
    TRANSLATION("翻译（本地）", "自动翻译的开关与平台", Icons.Outlined.Translate),
    NETWORK("网络与代理", "代理、按平台指定", Icons.Outlined.Language),
    SYNC("跨设备同步", "已配对设备、导出与导入", Icons.Outlined.Sync),
    STORAGE("存储", "历史保留天数、图片缓存", Icons.Outlined.Storage),
    ABOUT("关于", "版本、开源许可", Icons.Outlined.Info),
}

/**
 * 设置页现在停在哪一页：
 * 一级分组子页、进阶设置的分组列表、进阶设置下的一个子页。
 */
internal sealed interface SettingsRoute {
    data class Section(val section: SettingsSection) : SettingsRoute

    /** 「进阶设置」的分组列表（第二层）。 */
    data object AdvancedList : SettingsRoute

    /** 「进阶设置」下的一个子页（第三层）。 */
    data class Advanced(val section: AdvancedSection) : SettingsRoute
}

/** 宽屏左边那一栏画哪一层列表（进阶设置自己也是"列表 → 子页"，所以左栏有两层）。 */
internal enum class SettingsMenuLevel { TOP, ADVANCED }

/**
 * 设置页的层级：[route] 为 null = 设置首页（分组列表）。
 *
 * 为什么放在 `App` 里（而不是 `SettingsScreen` 内部的 `remember`）：① 切到别的页签再切回来还停在原来那一页；
 * ② "点当前页签 = 回上一层"和系统返回都要在 `App` 那层看到这个状态。
 *
 * 宽屏（平板横屏 / 桌面）列表常驻，没选过分组时右边先摆**最后一次看过的**那一组（默认信息流，见 [shown]）；
 * 窄屏 [route] 为 null 时画的就是列表本身。进阶设置那一层同理：宽屏进到进阶之后左边换成进阶的列表
 * （[level]），右边摆最后一次看过的进阶子页（默认「代理（进阶）」）。
 */
internal class SettingsNav {
    var route: SettingsRoute? by mutableStateOf(null)
        private set

    /** 宽屏右边兜底显示哪一组（最后一次打开过的一级分组）。 */
    private var lastSection: SettingsSection = SettingsSection.FEED

    /** 宽屏右边兜底显示哪一个进阶子页（最后一次打开过的）。 */
    private var lastAdvanced: AdvancedSection = AdvancedSection.PROXY

    /** 宽屏右边真正画哪一页：进阶列表那一层就摆最后一次看过的进阶子页，别让右栏空着。 */
    val shown: SettingsRoute
        get() = when (val r = route) {
            null -> SettingsRoute.Section(lastSection)
            SettingsRoute.AdvancedList -> SettingsRoute.Advanced(lastAdvanced)
            else -> r
        }

    /** 宽屏左边画哪一层列表。 */
    val level: SettingsMenuLevel
        get() = when (route) {
            is SettingsRoute.Advanced, SettingsRoute.AdvancedList -> SettingsMenuLevel.ADVANCED
            else -> SettingsMenuLevel.TOP
        }

    fun open(route: SettingsRoute) {
        when (route) {
            is SettingsRoute.Section -> lastSection = route.section
            is SettingsRoute.Advanced -> lastAdvanced = route.section
            SettingsRoute.AdvancedList -> Unit
        }
        this.route = route
    }

    /**
     * 往上一层：进阶子页 → 进阶列表 → 设置首页；一级分组子页 → 设置首页。
     * **已经在设置首页时返回 false**（这次返回不归设置页管，交给外层：页签 → 首页）。
     * 宽屏也走同一条路（右边退回到最后一次看的那一组 / 那一页，别让右栏空着）；要不要拦返回键由
     * [settingsSubBackEnabled] 按"窄屏 / 宽屏的进阶那一层"决定。
     */
    fun back(): Boolean = when (route) {
        null -> false
        SettingsRoute.AdvancedList -> { route = null; true }
        is SettingsRoute.Advanced -> { route = SettingsRoute.AdvancedList; true }
        is SettingsRoute.Section -> { route = null; true }
    }

    /** 一步回到设置首页（"点当前页签 = 回这一页的根部"、宽屏左栏的「‹ 设置」）。已经在首页返回 false。 */
    fun reset(): Boolean {
        val was = route != null
        route = null
        return was
    }
}

/** 分组列表（手机竖屏的整页 / 宽屏的左边一栏）。[selected] 是宽屏右边正在显示的那一页。 */
@Composable
private fun SettingsMenu(graph: AppGraph, nav: SettingsNav, selected: SettingsRoute?) {
    Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("设置", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(start = 4.dp, top = 4.dp, bottom = 4.dp))
        for (section in SettingsSection.entries.filter { it != SettingsSection.TRANSLATION || graph.showTranslation }) {
            val route = SettingsRoute.Section(section)
            SettingsMenuRow(section.title, section.summary, section.icon, selected == route) { nav.open(route) }
        }
        Spacer(Modifier.size(4.dp))
        SettingsMenuRow(
            "进阶设置",
            advancedSummary(graph.showBuiltinProxy, showTranslation = graph.showTranslation),
            Icons.Outlined.Build,
            // 宽屏在进阶里时左边已经是进阶的列表，这里就不该再高亮（两个列表不会同时出现）
            selected is SettingsRoute.Advanced || selected == SettingsRoute.AdvancedList,
        ) { nav.open(SettingsRoute.AdvancedList) }
        Spacer(Modifier.size(12.dp))
    }
}

/**
 * 「进阶设置」的分组列表。**复用一级列表的行** [SettingsMenuRow]，和设置首页长得一样。
 *
 * - 手机竖屏：这是 `nav.route == AdvancedList` 时的整页，标题栏（带返回箭头）由 [SettingsPage] 画。
 * - 宽屏：这是左边那一栏（[showTitle] = true），顶上多一行「‹ 设置」回一级列表（那一栏这时不显示一级列表了）。
 *
 * [selected] 是右边正在显示的那一页（宽屏高亮用；窄屏传 null）。
 */
@Composable
private fun AdvancedMenu(
    graph: AppGraph,
    nav: SettingsNav,
    selected: SettingsRoute?,
    showTitle: Boolean,
    onBack: (() -> Unit)? = null,
) {
    Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (showTitle) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "进阶设置",
                    style = MaterialTheme.typography.titleLarge,
                    modifier = Modifier.padding(start = 4.dp, top = 4.dp, bottom = 4.dp),
                )
                Spacer(Modifier.weight(1f))
                if (onBack != null) TextButton(onClick = onBack) { Text("‹ 设置") }
            }
        }
        // 「对外接口（agent）」只有桌面组装有；「翻译（进阶）」只有带翻译功能的构建才列（基础版不摆无效项）
        for (section in AdvancedSection.entries.filter {
            (it != AdvancedSection.API || graph.appApi != null) &&
                (it != AdvancedSection.TRANSLATION || graph.showTranslation)
        }) {
            val route = SettingsRoute.Advanced(section)
            val summary = if (section == AdvancedSection.BEHAVIOR && !graph.showEmbedding) {
                "曝光与停留统计、清除"
            } else {
                section.summary
            }
            SettingsMenuRow(section.title, summary, section.icon, selected == route) { nav.open(route) }
        }
        Hint(
            if (graph.showBuiltinProxy) "普通用户用不上的东西都放在这一层：内置代理核心、指定代理明细，以及一些偏调试的开关。"
            else "普通用户用不上的东西都放在这一层：指定代理的明细，以及一些偏调试的开关（这台设备上没有内置代理核心这一档）。",
        )
        Spacer(Modifier.size(12.dp))
    }
}

/**
 * 「进阶设置」那一行的一句话说明（有内置核心 / 没有内置核心两种）。
 *
 * 一行放不下就省略（[SettingsMenuRow] 只给一行），所以挑最能说明"里面有什么"的几样。
 */
internal fun advancedSummary(showBuiltinCore: Boolean, showTranslation: Boolean = true): String =
    (if (showBuiltinCore) "内置代理核心、指定代理明细、" else "指定代理明细、") +
        if (showTranslation) "首页排序、翻译路径、行为数据" else "首页排序、行为数据"

/** 分组列表里的一行：图标 + 名称 + 一行摘要 + `›`。 */
@Composable
private fun SettingsMenuRow(title: String, summary: String, icon: ImageVector, selected: Boolean, onClick: () -> Unit) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = if (selected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainerLowest,
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    ) {
        Row(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(20.dp))
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.bodyLarge)
                Text(
                    summary,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text("›", style = MaterialTheme.typography.titleLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * 一个子页：标题栏（窄屏带返回箭头）+ 内容。
 * 内容是**分组自己的卡片**，和拆之前一模一样（[SettingsSectionContent] / [AdvancedSectionContent]），
 * 只是不在一页里挤着了。
 */
@Composable
private fun SettingsPage(
    graph: AppGraph,
    route: SettingsRoute,
    nav: SettingsNav,
    showBack: Boolean,
    initialDialog: String? = null,
    initialSyncPreview: feedhub.app.sync.SyncPreviewDebug? = null,
    initialPairing: feedhub.app.sync.SyncPairingDebug? = null,
) {
    val title = when (route) {
        is SettingsRoute.Section -> route.section.title
        SettingsRoute.AdvancedList -> "进阶设置"
        is SettingsRoute.Advanced -> route.section.title
    }
    Column(Modifier.fillMaxSize()) {
        SubPageHeader(title, onBack = if (showBack) ({ nav.back() }) else null)
        // key(route)：换一页就换一份滚动位置（从信息流翻到网络，不该继承上一页滚到一半的位置）
        key(route) {
            when (route) {
                // 窄屏的「进阶设置」列表页：只有一级列表才走到这里（宽屏画在左栏，右边永远是子页）
                SettingsRoute.AdvancedList -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    AdvancedMenu(graph, nav, selected = null, showTitle = false)
                }
                is SettingsRoute.Advanced -> Column(
                    // imePadding：软键盘弹出时把滚动区域缩上去，聚焦的输入框（地址 / 订阅链接 / 模型路径…）
                    // 才能被"滚进可见区"。桌面没有 IME，等于没加。
                    Modifier.fillMaxSize().imePadding().verticalScroll(rememberScrollState())
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    // 桌面窗口很宽时不让一张卡片横着铺满整个屏幕（和原来"两栏卡片"的观感一致）
                    Column(Modifier.widthIn(max = 820.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        AdvancedSectionContent(graph, route.section, initialDialog)
                    }
                }
                is SettingsRoute.Section -> Column(
                    // imePadding：同上（屏蔽词那些输入框）
                    Modifier.fillMaxSize().imePadding().verticalScroll(rememberScrollState())
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Column(Modifier.widthIn(max = 820.dp).fillMaxWidth()) {
                        SettingsSectionContent(graph, route.section, nav, initialSyncPreview, initialPairing)
                    }
                }
            }
        }
    }
}

/** 一个分组的内容（拆子页之前 `SettingsGroupCard` 的那个 `when`，一一对应）。 */
@Composable
private fun SettingsSectionContent(
    graph: AppGraph,
    section: SettingsSection,
    nav: SettingsNav,
    initialSyncPreview: feedhub.app.sync.SyncPreviewDebug? = null,
    initialPairing: feedhub.app.sync.SyncPairingDebug? = null,
) {
    when (section) {
        SettingsSection.FEED -> FeedSettings(graph, nav)
        SettingsSection.CONTENT -> ContentFilterSettings(graph)
        SettingsSection.DETAIL -> DetailSettings(graph)
        SettingsSection.APPEARANCE -> AppearanceSettings(graph)
        SettingsSection.VIDEO -> VideoSettings(graph)
        SettingsSection.TRANSLATION -> TranslationSettings(graph, nav)
        SettingsSection.NETWORK -> NetworkSettings(graph, nav)
        SettingsSection.SYNC -> SyncSettings(graph, initialSyncPreview, initialPairing)
        SettingsSection.STORAGE -> StorageSettings(graph)
        SettingsSection.ABOUT -> AboutSection()
    }
}

@Composable
private fun FeedSettings(graph: AppGraph, nav: SettingsNav) {
    val s = graph.settings.value
    val update = graph.settings::update
    val platformCount = graph.platforms.platforms.size
    SettingsCard("信息流") {
        SettingBlock("内容类型") {
            SegmentedChoice(
                listOf(ContentType.MIXED to "混合", ContentType.VIDEO to "只看视频", ContentType.TEXT to "只看图文"),
                s.contentType,
            ) { v -> update { it.copy(contentType = v) } }
        }
        SettingBlock("首页平台", "点一下切换，已选 ${s.homePlatforms.size} / $platformCount 家") {
            PlatformChips(s.homePlatforms, { graph.supports(it, FeedKind.RECOMMEND) }, { graph.available(it) }) { v ->
                update { it.copy(homePlatforms = v) }
            }
        }
        SettingBlock("关注平台", "点一下切换，已选 ${s.followingPlatforms.size} / $platformCount 家") {
            PlatformChips(s.followingPlatforms, { graph.supports(it, FeedKind.FOLLOWING) }, { graph.available(it) }) { v ->
                update { it.copy(followingPlatforms = v) }
            }
        }
        // 没有关注流的平台（现在只有小黑盒）：按各平台 Source 实际支持的流列出，不写死；
        // 这台机器上暂时用不了的平台单列一句原因，别让人以为是选了没生效。
        // 这两句只在"真有这种情况"时才长，所以收进「?」里，主界面只留一行（设计对照 G2）。
        val noFollowing = graph.platforms.platforms.filter { graph.available(it) && FeedKind.FOLLOWING !in graph.source(it).feeds }
        val missing = graph.unavailable
        val details = buildList {
            if (noFollowing.isNotEmpty()) add(noFollowing.joinToString("、") { it.label } + "没有关注流。")
            if (missing.isNotEmpty()) add(missing.entries.joinToString("；") { "${it.key.label}：${it.value}" } + "。")
        }.joinToString("")
        Hint(
            "改了内容类型或平台后，首页和关注页会自动刷新一次。",
            detail = details.ifBlank { null },
        )
        SettingBlock("首页列数", "自动：按窗口宽度算；也可以固定几列") {
            HomeColumnsChoice(
                mode = s.homeColumnsMode,
                fixed = s.homeColumns,
                nowColumns = LocalHomeColumns.current,
                onMode = { m -> update { it.copy(homeColumnsMode = m) } },
                onFixed = { n -> update { it.copy(homeColumnsMode = HomeColumnsMode.FIXED, homeColumns = n) } },
            )
        }
        // 「每平台一批最多」「首页排序」
        // 挪到了「信息流（进阶）」。这里留一行能点进去的入口，别让人以为设置项丢了。
        SettingsJump(
            "首页排序、每平台一批最多在「信息流（进阶）」里",
            SettingsRoute.Advanced(AdvancedSection.FEED),
            nav,
        )
    }
}

/**
 * 「每平台一批最多」的选项行：**8 · 12 · 15 · 不限**，样式和首页列数那一排一样。
 * 只显示数字，末尾一句小字说明现在选的是什么（0 = 不限）。
 */
@Composable
internal fun PerPlatformCapChoice(value: Int, onChange: (Int) -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for (n in MAX_PER_PLATFORM_CHOICES) {
            OptionChip(if (n == 0) "不限" else "$n", value == n) { onChange(n) }
        }
        Hint(
            if (value > 0) "现在是每个平台最多 $value 条" else "现在是不限（每家一页全进来）",
            Modifier.align(Alignment.CenterVertically).padding(start = 4.dp),
        )
    }
}

/**
 * 首页列数的选项行：**自动 · 2 · 3 · 4 · 5 · 6**。
 * 选「自动」时这行末尾多一句小字"现在是 N 列"（[nowColumns] 是 `App.kt` 按当前窗口宽度算好的同一个值，
 * 窗口大小一变它就变）。选项和平台标签同一个样式（[FilterChip]），窄屏排不下会自动折到下一行。
 */
@Composable
internal fun HomeColumnsChoice(
    mode: HomeColumnsMode,
    fixed: Int,
    nowColumns: Int,
    onMode: (HomeColumnsMode) -> Unit,
    onFixed: (Int) -> Unit,
) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        OptionChip("自动", mode == HomeColumnsMode.AUTO) { onMode(HomeColumnsMode.AUTO) }
        for (n in MIN_HOME_COLUMNS..MAX_HOME_COLUMNS) {
            OptionChip("$n", mode == HomeColumnsMode.FIXED && fixed == n) { onFixed(n) }
        }
        if (mode == HomeColumnsMode.AUTO) {
            Hint("现在是 $nowColumns 列", Modifier.align(Alignment.CenterVertically).padding(start = 4.dp))
        }
    }
}

/** 一个选项标签（首页列数、每平台最多）：和 [PlatformChips] / 界面缩放的「自动」同一个样式（选中的带一个勾）。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun OptionChip(label: String, selected: Boolean, onClick: () -> Unit) {
    FilterChip(
        selected = selected,
        onClick = onClick,
        label = { Text(label) },
        leadingIcon = if (selected) ({ Icon(Icons.Outlined.Check, contentDescription = null, modifier = Modifier.size(16.dp)) }) else null,
    )
}

@Composable
private fun DetailSettings(graph: AppGraph) {
    val s = graph.settings.value
    val update = graph.settings::update
    SettingsCard("详情页") {
        SettingInline("打开方式", "平板横屏和桌面可选；手机总是全屏") {
            SegmentedChoice(
                listOf(DetailMode.TWO_PANE to "双栏", DetailMode.FULLSCREEN to "全屏"),
                s.detailMode,
            ) { v -> update { it.copy(detailMode = v) } }
        }
        SwitchRow(
            "默认显示原图",
            "关掉则先显示缩略图，点开再看原图",
            s.originalImages,
        ) { v -> update { it.copy(originalImages = v) } }
    }
}

@Composable
private fun AppearanceSettings(graph: AppGraph) {
    val s = graph.settings.value
    SettingsCard("外观") {
        SettingInline("深色模式", controlWidth = 340.dp) {
            SegmentedChoice(
                listOf(DarkMode.SYSTEM to "跟随系统", DarkMode.LIGHT to "浅色", DarkMode.DARK to "深色"),
                s.darkMode,
            ) { v -> graph.settings.update { it.copy(darkMode = v) } }
        }
        if (!graph.desktop) {
            SwitchRow(
                "自动旋转",
                "关着时手机上锁竖屏（横屏视频全屏照样转横屏）；平板始终跟随系统",
                s.autoRotate,
            ) { v -> graph.settings.update { it.copy(autoRotate = v) } }
        }
        if (graph.desktop) {
            val system = graph.systemUiScale
            SettingBlock(
                "界面缩放",
                if (system != null) "自动：跟系统的显示缩放（现在是 ${(system * 100).roundToInt()}%）" else "自动：跟系统的显示缩放",
            ) {
                UiScaleControl(s.uiScale, system ?: 1f) { v -> graph.settings.update { it.copy(uiScale = v) } }
            }
        }
    }
}

@Composable
private fun VideoSettings(graph: AppGraph) {
    val s = graph.settings.value
    val update = graph.settings::update
    SettingsCard("视频") {
        SettingBlock("默认清晰度") {
            SegmentedChoice(
                listOf(DefaultQuality.HIGHEST to "自动最高", DefaultQuality.P1080 to "1080P", DefaultQuality.P720 to "720P"),
                s.quality,
            ) { v -> update { it.copy(quality = v) } }
        }
        SwitchRow("弹幕默认开启", "播放器里的弹幕按钮只管这一次", s.danmakuOn) { v -> update { it.copy(danmakuOn = v) } }
        DanmakuSettingsEntry(s.danmaku) { v -> update { it.copy(danmaku = v) } }
    }
}

/** 弹幕设置入口：一行摘要 + "设置"按钮，点开是和播放器上同一个面板（顶上多一块示意画面，因为这里没有视频）。 */
@Composable
private fun DanmakuSettingsEntry(settings: DanmakuSettings, onChange: (DanmakuSettings) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text("弹幕设置", style = MaterialTheme.typography.bodyMedium)
            Hint(danmakuSummary(settings))
        }
        Spacer(Modifier.width(12.dp))
        OutlinedButton(onClick = { open = true }) { Text("设置") }
    }
    if (open) {
        Dialog(onDismissRequest = { open = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            DanmakuSettingsPanel(
                settings = settings,
                onChange = onChange,
                onClose = { open = false },
                modifier = Modifier.padding(16.dp).widthIn(max = 420.dp).heightIn(max = 680.dp),
                preview = { DanmakuPreview(settings, Modifier.fillMaxWidth().aspectRatio(16f / 9f)) },
            )
        }
    }
}

/** 摘要："透明度 85% · 显示区域 1/2 · 屏蔽词 3 条"。 */
internal fun danmakuSummary(s: DanmakuSettings): String = buildList {
    add("透明度 ${(s.opacity * 100).roundToInt()}%")
    add("显示区域 ${areaLabel(s.area)}")
    if (s.weightLevel > 0) add("云屏蔽 ${s.weightLevel} 级")
    if (s.blockRules.isNotEmpty()) add("屏蔽词 ${s.blockRules.size} 条")
}.joinToString(" · ")

private val retentionPresets = listOf(7, 30, 90, 180, 365)

private fun retentionLabel(days: Int) = if (days == 365) "1 年" else "$days 天"

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun StorageSettings(graph: AppGraph) {
    val s = graph.settings.value
    val scope = rememberCoroutineScope()
    var pruned by remember { mutableStateOf<Int?>(null) }
    fun applyRetention() {
        scope.launch { pruned = graph.applyHistoryRetention() }
    }
    SettingsCard("存储") {
        SwitchRow(
            "历史记录只保留最近 N 天",
            if (s.historyLimitOn) "超过 ${s.historyDays} 天的记录会在设置改动后和每次启动时清除" else "关着：一直保留",
            s.historyLimitOn,
        ) { v ->
            graph.settings.update { it.copy(historyLimitOn = v) }
            if (v) applyRetention()
        }
        if (s.historyLimitOn) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for (d in (retentionPresets + s.historyDays).distinct().sorted()) {
                    FilterChip(
                        selected = d == s.historyDays,
                        onClick = {
                            graph.settings.update { it.copy(historyDays = d) }
                            applyRetention()
                        },
                        label = { Text(retentionLabel(d)) },
                    )
                }
            }
            pruned?.takeIf { it > 0 }?.let { Hint("刚清理了 $it 条") }
        }
        ImageCacheRow()
    }
}

/** 图片缓存：显示占用，清理按钮。用 Coil 的磁盘缓存对象读大小、清空（它自己管着缓存目录，直接删文件会和它的索引打架）。 */
@Composable
private fun ImageCacheRow() {
    val context = LocalPlatformContext.current
    val loader = remember(context) { SingletonImageLoader.get(context) }
    val cache = loader.diskCache
    var size by remember { mutableStateOf<Long?>(null) }
    var refresh by remember { mutableIntStateOf(0) }
    LaunchedEffect(refresh) { size = withContext(Dispatchers.IO) { cache?.size } }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text("图片缓存", style = MaterialTheme.typography.bodyMedium)
            Hint(
                when {
                    cache == null -> "没有启用磁盘缓存"
                    size == null -> "正在计算…"
                    else -> "占用 ${formatBytes(size!!)}"
                },
            )
        }
        OutlinedButton(
            onClick = {
                loader.memoryCache?.clear()
                cache?.clear()
                refresh++
            },
            enabled = cache != null && (size ?: 0) > 0,
        ) { Text("清理") }
    }
}

fun formatBytes(n: Long): String = when {
    n >= 1L shl 30 -> "%.1f GB".format(n / (1024.0 * 1024 * 1024))
    n >= 1L shl 20 -> "%.1f MB".format(n / (1024.0 * 1024))
    n >= 1L shl 10 -> "%.0f KB".format(n / 1024.0)
    else -> "$n B"
}

@Composable
private fun AboutSection() {
    var showLicenses by remember { mutableStateOf(false) }
    SettingsCard("关于") {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text("${AppInfo.NAME} ${AppInfo.VERSION}", style = MaterialTheme.typography.bodyMedium)
            Hint(AppInfo.DESCRIPTION)
        }
        Column {
            Text(
                if (showLicenses) "开源许可 ▴" else "开源许可 ▾",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.clickable { showLicenses = !showLicenses }.padding(vertical = 4.dp),
            )
            if (showLicenses) {
                Column(Modifier.fillMaxWidth().padding(top = 4.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    for (l in AppInfo.licenses) {
                        Row {
                            Text(l.name, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                            Text(l.license, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
    }
}

/**
 * 界面缩放：「自动」开关 + 滑轨 + 百分比输入框。[value] 是设置里的值（0 = 自动），[system] 是系统的倍数。
 * 拖滑轨时只改显示的数字，松手才生效（不然界面在鼠标底下一直变大变小）；输入框按回车或失去焦点时生效。
 */
@Composable
private fun UiScaleControl(value: Float, system: Float, onChange: (Float) -> Unit) {
    val auto = value <= 0f
    val current = if (auto) system else value
    var dragging by remember(current) { mutableStateOf(current) }
    var text by remember(current) { mutableStateOf((current * 100).roundToInt().toString()) }
    // 输入框刚显示时也会收到一次"没有焦点"的回调，只有真的获得过焦点再失去才算输完
    var focused by remember { mutableStateOf(false) }
    fun commitText() {
        val pct = text.trim().removeSuffix("%").toIntOrNull()
        val shown = (current * 100).roundToInt()
        if (pct == null || pct == shown) {
            text = shown.toString()
            return
        }
        val v = pct.coerceIn(SCALE_MIN_PCT, SCALE_MAX_PCT) / 100f
        text = (v * 100).roundToInt().toString()
        onChange(v)
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        FilterChip(
            selected = auto,
            onClick = { onChange(if (auto) current else 0f) },
            label = { Text("自动") },
            leadingIcon = if (auto) ({ Icon(Icons.Outlined.Check, contentDescription = null, modifier = Modifier.size(16.dp)) }) else null,
        )
        Spacer(Modifier.width(12.dp))
        Slider(
            value = dragging.coerceIn(SCALE_MIN_PCT / 100f, SCALE_MAX_PCT / 100f),
            onValueChange = { dragging = (it * 20).roundToInt() / 20f; text = (dragging * 100).roundToInt().toString() },
            onValueChangeFinished = { if (dragging != current) onChange(dragging) },
            valueRange = SCALE_MIN_PCT / 100f..SCALE_MAX_PCT / 100f,
            modifier = Modifier.weight(1f),
        )
        Spacer(Modifier.width(12.dp))
        OutlinedTextField(
            value = text,
            onValueChange = { t -> text = t.filter { it.isDigit() }.take(3) },
            singleLine = true,
            suffix = { Text("%") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { commitText() }),
            textStyle = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.width(96.dp).onFocusChanged {
                if (focused && !it.isFocused) commitText()
                focused = it.isFocused
            },
        )
    }
}

private const val SCALE_MIN_PCT = 75
private const val SCALE_MAX_PCT = 300
