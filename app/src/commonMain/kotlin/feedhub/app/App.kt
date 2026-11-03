package feedhub.app

import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.People
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.movableContentOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalWindowInfo
import feedhub.app.ui.PlatformBackHandler
import coil3.SingletonImageLoader
import coil3.compose.LocalPlatformContext
import coil3.compose.setSingletonImageLoaderFactory
import feedhub.app.detail.DetailNavigator
import feedhub.app.detail.DetailPane
import feedhub.app.detail.LocalDetailNavigator
import feedhub.app.feed.FeedEmptyAction
import feedhub.app.feed.FeedGridSpec
import feedhub.app.feed.FeedScreen
import feedhub.app.feed.LocalPluggedIn
import feedhub.app.feed.homeColumns
import feedhub.app.image.appImageLoader
import feedhub.app.library.ExposureSurface
import feedhub.app.login.WebLoginScreen
import feedhub.app.mine.MineScreen
import feedhub.app.mine.MineSection
import feedhub.app.search.SearchScreen
import feedhub.app.settings.SettingsNav
import feedhub.app.settings.SettingsRoute
import feedhub.app.settings.SettingsScreen
import feedhub.app.settings.SettingsSection
import feedhub.app.translate.LocalTranslation
import feedhub.app.ui.FeedhubTheme
import feedhub.app.video.PlayPrefetcher
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.VideoSource
import feedhub.feed.exposureInputs
import feedhub.feed.PreloadPolicy

/** [movableContentOf] 的参数打包（它只到五个参数的重载，我们有六个）。 */
private data class TabContentArgs(
    val tab: Tab,
    val grid: FeedGridSpec?,
    val covered: Boolean,
    val advanced: Boolean,
    val advancedPage: String?,
    val settingsSection: String?,
    val dialog: String?,
    val syncPreview: feedhub.app.sync.SyncPreviewDebug?,
    val pairingDebug: feedhub.app.sync.SyncPairingDebug? = null,
    /** 点当前页签的次数（"列表回顶"，见 [TabReselectState]）。 */
    val reselect: Int = 0,
    /**
     * 这个页签打开着的那一条（沉浸播放上下切换时也跟着变）。
     * 见 `feed/FeedReveal.kt`）。
     * **全屏详情盖住信息流时是 null**：
     * 盖着的网格不跟着滚，关掉详情回到原来的位置，瀑布流不重排。
     */
    val selectedKey: String? = null,
)

enum class Tab(val label: String, val icon: ImageVector) {
    HOME("首页", Icons.Outlined.Home),
    // 「关注」用两个人（People / 订阅）：爱心在安卓语境里是"收藏"，而收藏是真的有的功能（我的 → 收藏），
    // 两个含义撞在一起。
    FOLLOWING("关注", Icons.Outlined.People),
    MINE("我的", Icons.Outlined.Person),
    SETTINGS("设置", Icons.Outlined.Settings),
}

/**
 * 全宽时的首页 / 关注列数（设置是「自动」的话已经按**窗口**宽度解析过，见 [feedhub.app.feed.homeColumns]）。
 * 由 [App] 在页签内容外面提供，首页 / 关注网格和设置页那句"现在是 N 列"读的是**同一个值**：
 * 窗口大小一变（比如平板转屏）两边一起跟着变，不会出现设置页说 3 列、首页排 4 列。
 * 默认值是老默认的 4 列，只给没有 [App] 的场景兜底（现在没有这种场景）。
 */
val LocalHomeColumns = compositionLocalOf { LEGACY_DEFAULT_HOME_COLUMNS }

/**
 * 整个 App：主题、图片加载、导航（宽屏左侧竖排图标栏，窄屏底部导航栏）、详情页的摆放。
 *
 * 详情页：宽屏且设置是"双栏"时，左边收成信息流列、右边是详情（[SplitPane]，有过渡动画，
 * 中间的分隔线可以拖）；设置是"全屏"或窄屏时，详情盖住整个界面（连导航栏一起）。
 *
 * 切页签保留状态：每个页签各自打开着的详情记在 [open] 里，切走再切回来还开着；页签里界面的状态（网格滚动位置等）靠
 * [rememberSaveableStateHolder]：页签离开组合时 `rememberSaveable` 的东西存进去，回来时取出。全屏详情是**盖在**原界面上的，
 * 不是换掉它，所以关掉详情回到网格时滚动位置还在。离开组合的详情（切到别的页签）里的视频会随之释放、停止播放，回来时重新加载，从保存的进度接着播。
 *
 * 系统返回：页签这一层只有"回首页"（[tabBackEnabled]），
 * 「我的」子页 / 设置子页 / 进阶设置 / 详情这些更靠内的层各自在自己的组合里注册，返回到最外层才退出 App。
 */
@Composable
fun App(
    graph: AppGraph,
    initialTab: Tab = Tab.HOME,
    initialDetail: FeedItem? = null,
    /** 截图工具用：直接打开设置页的「进阶设置」分组列表（[SettingsScreen] 的 initialAdvanced）。 */
    initialAdvancedSettings: Boolean = false,
    /** 截图工具用：直接打开某个进阶子页（`AdvancedSection.key`，见 [SettingsScreen] 的 initialAdvancedPage）。 */
    initialAdvancedPage: String? = null,
    /** 截图工具用：直接打开某个一级分组（`SettingsSection` 的名字，见 [SettingsScreen] 的 initialSection）。 */
    initialSettingsSection: String? = null,
    /** 截图工具用：直接打开进阶页里的某个弹框（import / subscribe，都在「代理（进阶）」里）。 */
    initialSettingsDialog: String? = null,
    /** 截图工具用：一打开就搜这个词（配 `query=` 的演示数据看搜索页）。 */
    initialSearch: String? = null,
    /** 截图工具用：直接摆出设置页「跨设备同步」的导入预览弹框（平时只有点了导入才会出现）。 */
    initialSyncPreview: feedhub.app.sync.SyncPreviewDebug? = null,
    /** 截图工具用：直接摆出"配对新设备"的二维码 / 校验码（不监听端口、不发请求）。 */
    initialPairing: feedhub.app.sync.SyncPairingDebug? = null,
) {
    val settings = graph.settings.value
    // 图片加载器（Coil）的配置在 `feedhub.app.image.appImageLoader` 里只有一份：
    // 界面和对外接口的 `/image` 用的必须是**同一个单例**（同一份磁盘缓存 / 代理），见那个函数的注释。
    setSingletonImageLoaderFactory { context -> appImageLoader(context, graph) }
    // 图片内存缓存：插电放大、用电池回到默认（PreloadPolicy；Coil 的 maxSize 可以随时改，改小会马上裁）
    val plugged = LocalPluggedIn.current
    val imageContext = LocalPlatformContext.current
    LaunchedEffect(plugged) {
        SingletonImageLoader.get(imageContext).memoryCache?.let { it.maxSize = PreloadPolicy.memoryCacheBytes(plugged, it.initialMaxSize) }
    }
    FeedhubTheme(settings.darkMode) {
      CompositionLocalProvider(
        LocalPlatforms provides graph.platforms,
        LocalTranslation provides graph.translation,
      ) {
        var tab by remember { mutableStateOf(initialTab) }
        // 系统返回：
        // **只有在「首页」按返回才是退出**。关注 / 我的 / 设置按返回先回首页；各页自己的子页
        // （我的子页、设置子页、进阶设置、详情、搜索……）在它们里面另注册一层，BackHandler 后注册的先收到，
        // 所以这里是最外层。放在页签内容之前：子页那一层一定在它后面注册（见 MineScreen / SettingsScreen）。
        PlatformBackHandler(enabled = tabBackEnabled(tab)) { tab = Tab.HOME }
        val open = remember { mutableStateMapOf<Tab, FeedItem>().apply { initialDetail?.let { put(initialTab, it) } } }
        // 沉浸播放：来源列表 + 只预取下一条（换页签重新来）。
        // 收藏 / 历史 / 设置 / 搜索里点开的详情没有来源列表，navigator 就是 null（沉浸界面照样画，只是不给切）。
        val prefetcher = remember(tab) {
            PlayPrefetcher(
                { p -> graph.sourceOrNull(p) as? VideoSource },
                graph.scope,
                canPrefetch = { graph.foreground.isForeground },
            )
        }
        DisposableEffect(prefetcher) { onDispose { prefetcher.cancel() } }
        val navigator = remember(tab, prefetcher, open) {
            val feed = when (tab) {
                Tab.HOME -> graph.home
                Tab.FOLLOWING -> graph.following
                else -> null
            }
            feed?.let {
                DetailNavigator(
                    items = { it.items },
                    onSelect = { item -> open[tab] = item },
                    loadMore = { it.loadMore() },
                    loadingMore = { it.loadingMore },
                    prefetcher = prefetcher,
                )
            }
        }
        // 性能探针用：从外面"点开"一条（和点首页卡片走同一条路）
        DisposableEffect(Unit) {
            AppDebug.open = { item -> open[Tab.HOME] = item }
            onDispose { AppDebug.open = null }
        }
        val saveable = rememberSaveableStateHolder()
        // 设置页的层级（分组列表 / 子页 / 进阶设置）。放在这里：切页签再回来还停在原来那一页，
        // 系统返回和"点设置页签"也都要看到它（见 SettingsScreen 的 KDoc）。
        val settingsNav = remember { SettingsNav() }
        // 点当前页签 = 回到这一页的顶部。
        // 每次点一下给那个页签计数 +1，首页 / 关注 / 我的 / 设置各自看自己那个数。
        val reselect = remember { TabReselectState() }
        // 搜索页：盖在当前页签上的整页搜索；关掉它首页 / 关注的滚动位置和内容都不动。
        // 桌面另有 Ctrl+F / `/` 打开、Esc 关。
        // **搜索页有自己的"打开的那条"**（[SearchLayers]）：从搜索结果点开的详情只盖在搜索页上，
        // 页签（首页右栏）那条详情一个字段都不碰。
        val search = remember { SearchLayers(open = initialSearch != null) }
        LaunchedEffect(initialSearch) { if (initialSearch != null) graph.search.search(initialSearch) }
        val closeSearch = { search.close(); graph.search.close(); Unit }
        // 页签内容用 movableContentOf：窗口在宽 / 窄布局之间变（换了摆放的位置）时状态不丢；换页签靠 SaveableStateProvider 存取。
        // 参数打包成一个（movableContentOf 的重载参数个数有限）
        val tabContent = remember {
            // [covered]：全屏详情是**盖在**页签内容上的（下面那个 Surface），页签其实还组合着。信息流要据此停掉
            // 曝光 / 停留的计时，否则卡片其实看不见、却还在记（双栏时详情在旁边，不算盖住）。
            movableContentOf { args: TabContentArgs ->
                saveable.SaveableStateProvider(args.tab.name) {
                    // 从信息流点开一条 = 新的沉浸会话（"已经在沉浸里切过"要清掉）。navigator 按页签换，
                    // 所以在这里现读 CompositionLocal，而不是记住创建 movable 内容时的那一份。
                    val nav = LocalDetailNavigator.current
                    TabContent(
                        graph, args.tab, args.grid, args.covered,
                        onOpen = { item -> nav?.resetSwitched(); open[args.tab] = item },
                        // 首页 / 关注空状态的出口：
                        // 去登录 →「我的 → 账号」；去推荐看看 → 首页；去点亮平台 / 改内容类型 → 设置「信息流」。
                        onEmptyAction = { action ->
                            when (action) {
                                FeedEmptyAction.GO_LOGIN -> {
                                    graph.mine.section = MineSection.ACCOUNTS
                                    tab = Tab.MINE
                                }
                                FeedEmptyAction.GO_RECOMMEND -> tab = Tab.HOME
                                FeedEmptyAction.GO_PLATFORMS, FeedEmptyAction.GO_CONTENT_TYPE -> {
                                    settingsNav.open(SettingsRoute.Section(SettingsSection.FEED))
                                    tab = Tab.SETTINGS
                                }
                            }
                        },
                        reselectSignal = args.reselect,
                        initialAdvancedSettings = args.advanced,
                        initialAdvancedPage = args.advancedPage,
                        initialSettingsSection = args.settingsSection,
                        initialSettingsDialog = args.dialog,
                        onOpenSearch = { search.open() },
                        initialSyncPreview = args.syncPreview,
                        initialPairing = args.pairingDebug,
                        settingsNav = settingsNav,
                        selectedKey = args.selectedKey,
                    )
                }
            }
        }
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            // 沉浸播放要的"来源列表 + 换条目"给整棵界面（双栏右栏 / 全屏详情 / 首页关注都读得到）
            CompositionLocalProvider(LocalDetailNavigator provides navigator) {
            // 桌面快捷键：Ctrl+F / `/` 打开搜索页、Esc 逐层退（详情 → 搜索页）。
            // 判定在 `appKeyAction`（纯函数，`ScreensTest` 离线测）；这里只管把焦点摆在对的地方：
            // 信息流 / 搜索页这一层自己拿着，详情和设置那些里层会自己 [FocusRequester.requestFocus]。
            val rootFocus = remember { FocusRequester() }
            BoxWithConstraints(Modifier.fillMaxSize()) {
                // 宽屏布局 = 宽 ≥ 600dp **且** 高 ≥ 480dp：手机横屏（915×412dp）
                // 宽度过了、高度没过，所以它仍然是手机——底部导航 + 单页，详情全屏打开，不切左侧图标栏 / 双栏。
                // 判定只有 [isWideLayout] 一处，表格在 `LayoutTest` 里。
                val wide = isWideLayout(maxWidth, maxHeight)
                // 全宽时的首页列数：设置里选「自动」就按**窗口**宽度算（见 feed.GridColumns.autoColumns），
                // 选「固定 N 列」就用那个数。窗口大小一变这里跟着重算，设置页的"现在是 N 列"读的也是它。
                val columns = homeColumns(maxWidth, settings.homeColumnsMode, settings.homeColumns)
                // 盖在页签内容上的几层（搜索页 / 页签自己的详情 / 搜索页自己的详情）怎么摆：纯函数，[ScreenLayers]。
                // 搜索页不再把双栏压成单栏。
                val layers = ScreenLayers(wide, settings.detailMode, search.isOpen, open[tab], search.detail)
                val twoPane = layers.twoPane
                val detail = open[tab]
                // 性能记录：当前页面名（首页 / 关注 / 我的 / 设置 / 详情-图文 / 详情-视频 / 搜索；
                // 图片查看器和沉浸界面由各自的组合再盖一层，见 PerfScreen）
                val perfScreen = feedhub.app.perf.perfScreenName(tab, layers.perfDetail, layers.searchOpen)
                LaunchedEffect(perfScreen) { feedhub.app.perf.PerfHooks.screen(perfScreen) }
                val covered = layers.feedCovered
                // 关掉详情：下一个详情会话重新按宽高比判定进不进沉浸界面（见 DetailNavigator.switched）。
                // 搜索页盖着时关的是**搜索页自己那条**，页签那条（`open[tab]`）不动。
                val onClose = {
                    if (layers.searchOpen) search.closeDetail() else { navigator?.resetSwitched(); open.remove(tab) }
                    Unit
                }
                // 搜索页盖着首页时，右栏那条在播的视频先停：它还在组合里（关搜索页不重开、进度不丢），
                // 但声音不该跟着出来——尤其是搜索里再点开一条视频的时候（那会两路声音一起响）。
                // 用的是前后台同一条路（[AppGraph.pausePlayback]）：关掉搜索页时之前自己在播的从原位置续播，
                // 用户自己暂停过的不动。
                LaunchedEffect(layers.searchOpen) {
                    if (layers.searchOpen) graph.pausePlayback() else graph.resumePlayback()
                }
                // 快捷键的焦点：只在"信息流 / 搜索页"这一层抢焦点。详情（右栏 / 全屏）、设置、我的自己会拿，
                // 这里插一脚会把它们的 Esc 抢没。
                val rootWantsFocus = !layers.searchOpen && layers.feedDetailFullScreen == null &&
                    (tab == Tab.HOME || tab == Tab.FOLLOWING)
                LaunchedEffect(graph.desktop, rootWantsFocus) {
                    if (graph.desktop && rootWantsFocus) runCatching { rootFocus.requestFocus() }
                }
                // 页签点击：换页签 / 点当前页签回顶（子页则回上一层）。桌面上也这样（同一个 Compose 界面，
                // 侧栏点当前项回顶是 M3 的惯例，不影响原来的任何状态）。
                val onTabClick: (Tab) -> Unit = { t ->
                    if (t != tab) {
                        tab = t
                    } else {
                        when (tabReselectAction(t, graph.mine.section != null, settingsNav.route != null, wide)) {
                            TabReselectAction.CLOSE_MINE_SECTION -> graph.mine.section = null
                            // 一步回到设置首页（"根部"）：进阶子页 / 进阶列表都不该一次只退一层
                            TabReselectAction.CLOSE_SETTINGS_SUB -> settingsNav.reset()
                            TabReselectAction.SCROLL_TOP -> reselect.ping(t)
                        }
                    }
                }
                Box(
                    Modifier.fillMaxSize()
                        .focusRequester(rootFocus)
                        .focusable()
                        // 打开搜索页：Ctrl+F / `/`（先于里层，输入框里也不会被抢走——只在首页 / 关注生效）
                        .onPreviewKeyEvent { e ->
                            if (!graph.desktop) return@onPreviewKeyEvent false
                            when (
                                appKeyAction(
                                    key = e.key,
                                    down = e.type == KeyEventType.KeyDown,
                                    ctrl = e.isCtrlPressed,
                                    searchOpen = layers.searchOpen,
                                    fullDetailOpen = layers.topDetail != null,
                                    // 搜索入口只放在首页 / 关注（设置页里有输入框，`/` 不能抢）
                                    feedTab = tab == Tab.HOME || tab == Tab.FOLLOWING,
                                )
                            ) {
                                AppKeyAction.OPEN_SEARCH -> { search.open(); true }
                                else -> false
                            }
                        }
                        // Esc 兜底：只有里层都没消费才轮到这里（详情 / 设置子页有自己要关的东西时先它们关）。
                        // 为什么要兜底：点卡片开详情时焦点停在被点的卡片上，详情收不到 Esc（见 `escapeAction`）。
                        .onKeyEvent { e ->
                            if (!graph.desktop || e.type != KeyEventType.KeyDown) return@onKeyEvent false
                            val detailOpen = if (layers.searchOpen) layers.searchDetail != null else layers.feedDetail != null
                            when (escapeAction(layers.searchOpen, detailOpen)) {
                                AppKeyAction.CLOSE_DETAIL -> { onClose(); true }
                                AppKeyAction.CLOSE_SEARCH -> { closeSearch(); true }
                                else -> false
                            }
                        },
                ) {
                    val topDetail = layers.topDetail
                    val webLogin = graph.webLogin
                    val webPlatform = webLogin?.platform
                    val webLoginOpen = webLogin != null && webPlatform != null
                    val tabLayerCovered = layers.searchOpen || topDetail != null || webLoginOpen
                    val searchLayerCovered = topDetail != null || webLoginOpen
                    FreezeWhenCovered(covered = tabLayerCovered, modifier = Modifier.fillMaxSize()) {
                        if (wide) {
                            // 宽屏这一支没有 Scaffold（那一支自己会躲系统栏），所以这里显式躲一下：
                            // 安卓平板横屏时状态栏 / 手势条才不压住导航栏和信息流（桌面的 insets 是 0，等于没加）。
                            Row(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
                                NavigationRail(containerColor = MaterialTheme.colorScheme.surfaceContainerLow) {
                                    for (t in Tab.entries) {
                                        NavigationRailItem(
                                            selected = tab == t,
                                            onClick = { onTabClick(t) },
                                            icon = { Icon(t.icon, contentDescription = null) },
                                            label = { Text(t.label) },
                                        )
                                    }
                                }
                                VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                                SplitPane(
                                    detail = detail.takeIf { twoPane },
                                    stateKey = tab,
                                    columns = columns,
                                    ratio = settings.detailSplit,
                                    onRatioChange = { r -> graph.settings.update { it.copy(detailSplit = r) } },
                                    modifier = Modifier.weight(1f).fillMaxHeight(),
                                    detailContent = { item ->
                                        // 详情在双栏的右栏里：告诉它（[LocalDetailInRightPane]），全屏放大时就不转屏
                                        // （转屏会把详情挪进另一处组合、放大态丢掉，见 requestsLandscapeFullscreen）
                                        CompositionLocalProvider(LocalDetailInRightPane provides true) {
                                            DetailPane(graph, item, onClose, Modifier.fillMaxSize())
                                        }
                                    },
                                ) { grid ->
                                    CompositionLocalProvider(LocalHomeColumns provides columns) {
                                        tabContent(TabContentArgs(tab, grid, covered, initialAdvancedSettings, initialAdvancedPage, initialSettingsSection, initialSettingsDialog, initialSyncPreview, initialPairing, reselect.count(tab), detail?.key.takeUnless { covered }))
                                    }
                                }
                            }
                        } else {
                            Scaffold(
                                bottomBar = {
                                    NavigationBar {
                                        for (t in Tab.entries) {
                                            NavigationBarItem(
                                                selected = tab == t,
                                                onClick = { onTabClick(t) },
                                                icon = { Icon(t.icon, contentDescription = null) },
                                                label = { Text(t.label) },
                                            )
                                        }
                                    }
                                },
                            ) { padding ->
                                Box(Modifier.padding(padding)) {
                                    CompositionLocalProvider(LocalHomeColumns provides columns) {
                                        tabContent(TabContentArgs(tab, null, covered, initialAdvancedSettings, initialAdvancedPage, initialSettingsSection, initialSettingsDialog, initialSyncPreview, initialPairing, reselect.count(tab), detail?.key.takeUnless { covered }))
                                    }
                                }
                            }
                        }
                    }
                    // 搜索页：整页盖在页签内容上（不换掉它，关掉就回到原位置；也不刷新首页）。
                    // 搜索页盖在上面，
                    // 关掉搜索页回到一模一样的状态（不跳位置、不重开、播放进度不丢）。
                    if (layers.searchOpen) {
                        FreezeWhenCovered(covered = searchLayerCovered, modifier = Modifier.fillMaxSize()) {
                            Surface(
                                color = MaterialTheme.colorScheme.background,
                                modifier = Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing),
                            ) {
                                SearchScreen(
                                    state = graph.search,
                                    onBack = closeSearch,
                                    // 从搜索结果点开的那条进**搜索页自己的槽**（不碰 `open[tab]`，见 [SearchLayers]）
                                    onOpen = { item -> search.openDetail(item) },
                                    columns = columns,
                                    detailOpen = layers.searchDetail != null,
                                )
                            }
                        }
                    }
                    // 最上面那条详情：搜索页开着 = 搜索页自己的那条（盖在搜索页上）；否则是页签的那条全屏详情。
                    // 双栏右栏那条不在这里（[ScreenLayers.topDetail] 已经把它排除掉了）。
                    if (topDetail != null) {
                        // 全屏详情盖在原界面上（Surface 吃掉底下的点击），不把原界面换掉：关掉后网格的滚动位置还在
                        // safeDrawing：安卓是 edge-to-edge（enableEdgeToEdge），不躲开会画到状态栏 / 手势条底下
                        // （桌面的 insets 全是 0，等于没加）。
                        Surface(
                            color = MaterialTheme.colorScheme.background,
                            modifier = Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing),
                        ) {
                            // 搜索里点开的详情**没有来源列表**（沉浸播放的上下切换不给它，和收藏 / 历史同一条规矩），
                            // 所以这里把 navigator 置空；页签自己的那条详情照旧拿首页 / 关注那份。
                            if (layers.searchOpen) {
                                CompositionLocalProvider(LocalDetailNavigator provides null) {
                                    DetailPane(graph, topDetail, onClose, Modifier.fillMaxSize())
                                }
                            } else {
                                DetailPane(graph, topDetail, onClose, Modifier.fillMaxSize())
                            }
                        }
                    }
                    // 网页登录（安卓的系统 WebView）：整屏覆盖层，盖在详情之上。
                    // 为什么不用 `Dialog`：Dialog 是另一个窗口，Android 15+ 边到边模式下软键盘不会把它的 Compose
                    // 布局缩上去（密码框 / 验证码被挡），返回键也被 Dialog 自己的 dismiss 抢走。放在这里以后
                    // imePadding / BackHandler / insets 都跟主窗口同一条路。桌面没有 webLogin（null），不会进这一支。
                    if (webLoginOpen) {
                        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
                            WebLoginScreen(
                                platform = webPlatform!!,
                                login = graph.mine.accounts.loginSpec(webPlatform),
                                proxy = graph.webLoginProxy(webPlatform),
                                onDone = { cookies ->
                                    graph.mine.accounts.webLoginDone(webPlatform, cookies)
                                    webLogin!!.cancel()
                                },
                                onCancel = { webLogin!!.cancel() },
                            )
                        }
                    }
                }
            }
            }
        }
      }
    }
}

/**
 * 一个页签的内容。[grid] 是双栏时信息流栏的网格排法（只有首页 / 关注用，别的页签按它给的宽度排，免得过渡动画里每帧重排）。
 * [covered]：这个页签被全屏详情盖住了（内容还组合着）。
 *
 * 行为数据的两个入参在这里算**一次**（[exposureInputs]，B2 的接线唯一一处）：设置里的开关 → `enabled`
 * （关掉 = 丢弃），[covered] / 窗口焦点 → `foreground`（被盖住 = 结算）。首页 / 关注 / 收藏 / 历史都吃这一份，
 * 所以"被盖住"不会在某一处被并进 `enabled`。
 */
@Composable
private fun TabContent(
    graph: AppGraph,
    tab: Tab,
    grid: FeedGridSpec?,
    covered: Boolean,
    onOpen: (FeedItem) -> Unit,
    /**
     * 首页 / 关注空状态里那些按钮的动作：去登录 / 去推荐看看 / 去点亮平台 / 去改内容类型。
     * 由 [App] 接（页签、设置页、「我的 → 账号」都在外面）。
     */
    onEmptyAction: (FeedEmptyAction) -> Unit = {},
    /** 点当前页签的次数（+1 = 回顶；见 [TabReselectState]）。 */
    reselectSignal: Int = 0,
    initialAdvancedSettings: Boolean = false,
    initialAdvancedPage: String? = null,
    initialSettingsSection: String? = null,
    initialSettingsDialog: String? = null,
    onOpenSearch: () -> Unit = {},
    initialSyncPreview: feedhub.app.sync.SyncPreviewDebug? = null,
    initialPairing: feedhub.app.sync.SyncPairingDebug? = null,
    /** 设置页的层级（分组列表 / 子页）。由 [App] 持有，切页签再回来还在原来那一页。 */
    settingsNav: SettingsNav,
    /** 这个页签打开着的那一条：信息流里跟着选中并滚到它（双栏左栏 / 沉浸播放上下切换）。 */
    selectedKey: String? = null,
) {
    // 全宽时的列数：由 [App] 按窗口宽度解析好放在 [LocalHomeColumns] 里（设置是「自动」时它才是算出来的数）
    val columns = LocalHomeColumns.current
    val exposure = exposureInputs(
        recordBehavior = graph.settings.value.recordBehavior,
        covered = covered,
        windowFocused = LocalWindowInfo.current.isWindowFocused,
    )
    when (tab) {
        Tab.HOME -> FeedScreen(
            "首页", graph.home, columns, graph.library, onOpen, grid, exposure, ExposureSurface.HOME, graph.foreground,
            onOpenSearch = onOpenSearch,
            // 桌面不要下拉刷新：刷新只留标题行右边的 ↻；手机 / 平板照旧
            pullToRefresh = !graph.desktop,
            // 空状态的判定事实只有这一处组装（不构造 Source，见 AppGraph.feedEmptyInputs）
            emptyInputs = graph.feedEmptyInputs(FeedKind.RECOMMEND, graph.home.errors, graph.home.notLoggedIn),
            onEmptyAction = onEmptyAction,
            reselectSignal = reselectSignal,
            selectedKey = selectedKey,
        )
        // 搜索入口只放首页；关注页标题行照旧
        Tab.FOLLOWING -> FeedScreen(
            "关注", graph.following, columns, graph.library, onOpen, grid, exposure, ExposureSurface.FOLLOWING, graph.foreground,
            emptyInputs = graph.feedEmptyInputs(FeedKind.FOLLOWING, graph.following.errors, graph.following.notLoggedIn),
            onEmptyAction = onEmptyAction,
            reselectSignal = reselectSignal,
            selectedKey = selectedKey,
            pullToRefresh = !graph.desktop,
        )
        Tab.MINE -> Box(if (grid != null) Modifier.fixedLayoutWidth(grid.width) else Modifier) {
            MineScreen(graph, onOpen, exposure, reselectSignal)
        }
        Tab.SETTINGS -> SettingsScreen(
            graph, settingsNav, initialAdvancedSettings, initialAdvancedPage, initialSettingsSection,
            initialSettingsDialog, initialSyncPreview, initialPairing,
            reselectSignal = reselectSignal,
        )
    }
}

/**
 * 页签这一层要不要拦系统返回（纯逻辑，`NavBackTest` 离线测）。
 *
 * 安卓惯例：非起始目的地按返回回起始目的地；**只有首页按返回才退出 App**。
 * 桌面上没有系统返回（[PlatformBackHandler] 是空实现），这个返回值不影响桌面。
 */
internal fun tabBackEnabled(tab: Tab): Boolean = tab != Tab.HOME

/** 点**当前**页签该做什么。 */
internal enum class TabReselectAction {
    /** 列表页：滚回顶部。 */
    SCROLL_TOP,

    /** 「我的」窄屏子页：回入口列表。 */
    CLOSE_MINE_SECTION,

    /** 设置子页（含进阶设置）：回分组列表。 */
    CLOSE_SETTINGS_SUB,
}

/**
 * 点当前页签的决定（纯逻辑，`TabReselectTest` 离线测）。
 *
 * - 「我的」窄屏子页 → 回入口列表（宽屏是左列表 + 右内容，列表一直在，就按"回顶"处理）；
 * - 设置子页 → 回分组列表；
 * - 其它（首页 / 关注 / 已经在列表上）→ 列表回顶。
 */
internal fun tabReselectAction(tab: Tab, mineSectionOpen: Boolean, settingsSubOpen: Boolean, wide: Boolean): TabReselectAction = when {
    tab == Tab.MINE && !wide && mineSectionOpen -> TabReselectAction.CLOSE_MINE_SECTION
    tab == Tab.SETTINGS && settingsSubOpen -> TabReselectAction.CLOSE_SETTINGS_SUB
    else -> TabReselectAction.SCROLL_TOP
}

/**
 * 每个页签被"点当前页签"了几次：页面拿到的数一变就回顶（[FeedScreen] / [MineScreen] / [SettingsScreen]）。
 * 用计数而不是布尔："回顶"是一次性动作，计数天然不会在重组时丢掉，也不会让两个页面互相看见。
 */
internal class TabReselectState {
    private val counts = mutableStateMapOf<Tab, Int>()

    fun count(tab: Tab): Int = counts[tab] ?: 0

    fun ping(tab: Tab) {
        counts[tab] = count(tab) + 1
    }
}

/**
 * 被整屏覆盖层（详情 / 搜索 / 网页登录）完全盖住时，保留下层组合与滚动状态，但把子树标成未摆放（`isPlaced = false`）：
 * Compose 不会对未摆放子树做测量、摆放、绘制和点击命中，看视频时底下的信息流不再重排或重画。
 */
@Composable
private fun FreezeWhenCovered(
    covered: Boolean,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    Layout(content = content, modifier = modifier) { measurables, constraints ->
        if (covered) {
            layout(constraints.minWidth, constraints.minHeight) {}
        } else {
            val placeables = measurables.map { it.measure(constraints) }
            val w = placeables.maxOfOrNull { it.width } ?: constraints.minWidth
            val h = placeables.maxOfOrNull { it.height } ?: constraints.minHeight
            layout(w, h) {
                for (p in placeables) {
                    p.placeRelative(0, 0)
                }
            }
        }
    }
}

/** 开发工具的口子（性能探针）：[open] 在 App 组合着的时候能用，等于在首页点开这一条。 */
internal object AppDebug {
    @Volatile var open: ((FeedItem) -> Unit)? = null
}
