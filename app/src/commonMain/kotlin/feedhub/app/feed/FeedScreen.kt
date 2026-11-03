package feedhub.app.feed

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState
import androidx.compose.foundation.lazy.staggeredgrid.LazyVerticalStaggeredGrid
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridCells
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridItemSpan
import androidx.compose.foundation.lazy.staggeredgrid.items
import androidx.compose.foundation.lazy.staggeredgrid.rememberLazyStaggeredGridState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.snapshotFlow
import coil3.SingletonImageLoader
import coil3.compose.LocalPlatformContext
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import feedhub.app.AppForeground
import feedhub.app.detail.DetailDebug
import feedhub.app.fixedLayoutWidth
import feedhub.app.library.ExposureSurface
import feedhub.app.library.Library
import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.feed.EXPOSURE_TICK_MS
import feedhub.feed.ExposureInputs
import feedhub.feed.FeedPreloadTrigger
import feedhub.feed.PreloadPolicy
import feedhub.feed.exposureInputs
import feedhub.feed.visibleItems

/**
 * 首页 / 关注页：瀑布流网格。[columns] 是全宽时的列数，由 `App.kt` 按窗口宽度解析好
 * （设置里「自动」就是算出来的，"固定 N 列"就是那个数；见 `feed/GridColumns.kt` 的 [homeColumns]）。
 * 刷新：（手机上）下拉或右上角按钮；新的一批放在最上面，网格回到顶部看新内容。滑到底自动加载更多。
 * **桌面上没有下拉刷新**：刷新只有右上角按钮。
 *
 * 滚动位置用 rememberSaveable 存，所以放在 `SaveableStateProvider` 里时（App 里每个页签一个）切到别的页签再回来还在原位。
 * [gridSpec] 不为空是双栏：列数和排版宽度由外面定（见 [FeedGridSpec]），不按自己被给的宽度重排；
 * 标题行仍按被给的宽度排，所以刷新按钮会跟着栏的右边缘走。
 *
 * [exposure] 是行为数据的两个入参（[ExposureInputs]，`App.kt` 的 `TabContent` 用 [exposureInputs] 算好传进来）：
 * `enabled` 是设置里"记录行为数据"的开关，`foreground` 是"这个页面真的在被看着"（没被全屏详情盖住 + 窗口有焦点）。
 * 两个都开着时才记曝光 / 停留（见 [ExposureTracker]）：卡片可见面积 ≥ 50% 且连续 ≥ 1 秒
 * 算一次曝光，停留记到滚出视口 / 被盖住 / 失焦为止。纯本地，不发请求。
 * **`foreground=false` 不能并进 `enabled`**：被盖住是"结算"，关开关是"丢弃"，两者语义不同。
 * [surface] 说明这次曝光发生在哪个面（首页 / 关注）。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FeedScreen(
    title: String,
    state: FeedState,
    columns: Int,
    library: Library,
    onOpen: (FeedItem) -> Unit,
    gridSpec: FeedGridSpec? = null,
    exposure: ExposureInputs = ExposureInputs(),
    surface: ExposureSurface = ExposureSurface.HOME,
    /** App 前后台（安卓由 Activity 喂）：后台不预取卡片图（见 [CardImagePrefetch]）。 */
    foreground: AppForeground = AppForeground(),
    /**
     * 搜索入口：null = 不显示（别的页复用 [FeedScreen] 时）。
     * 点开整页搜索页。
     */
    onOpenSearch: (() -> Unit)? = null,
    /**
     * 下拉刷新。
     * `false` 时**连 [PullToRefreshBox] 都不包**：没有下拉手势，也不会出现那个圈，刷新只靠标题行右边的 ↻。
     * 桌面传 `false`（`App.kt` 里 `!graph.desktop`），手机 / 平板照旧。
     */
    pullToRefresh: Boolean = true,
    /**
     * 空状态判定要用的事实：点亮了谁、谁有这个流、内容类型筛掉了谁、登录了没有、这一刷谁失败了。
     * 由 `App.kt` 从 [feedhub.app.AppGraph.feedEmptyInputs] 算好传进来（和 [exposure] 一样只有一处组装）。
     */
    emptyInputs: FeedEmptyInputs = FeedEmptyInputs(),
    /**
     * 空状态里按钮的动作：跳到「我的 → 账号」/ 切到首页 /
     * 打开设置的「信息流」那一页。**界面自己不长导航**，去哪儿由 `App.kt` 接。
     */
    onEmptyAction: (FeedEmptyAction) -> Unit = {},
    /**
     * 点当前页签的次数（`App.kt` 的 [feedhub.app.TabReselectState]）：数一变就滚回顶部
     * 。首次组合时记下当前值，所以切页签回来不会自己回顶。
     */
    reselectSignal: Int = 0,
    /**
     * 这个页签打开着的那一条（[FeedItem.key]），沉浸播放上下切换时也跟着变。
     * **不描边**，也**不在点开的时候滚**；只有列数没变、只是换了一条、而且它完全不在可视区里时才滚一下
     * （见 [feedRevealSide] / [revealMinimal]）。null = 没有打开的详情。
     */
    selectedKey: String? = null,
) {
    LaunchedEffect(state) {
        state.ensureLoaded()
    }
    // 使用挂在 FeedState 上的同一份网格状态：切页签回来列分配还在内存里，不需要整段重测
    val grid = state.gridState
    // 刷新的新一批整批放在最上面：网格是按 key 保持位置的，不主动回顶就看不到新插进来的。
    // 记数也跟着 state 存起来：离开页签期间刷新完了，回来时 handledRefreshedTimes 对不上，网格回顶
    val refreshed = state.refreshedTimes
    LaunchedEffect(state, refreshed) {
        if (refreshed != state.handledRefreshedTimes) {
            state.handledRefreshedTimes = refreshed
            grid.scrollToItem(0)
        }
    }
    // 点当前页签 = 回顶（M3 的惯例）：数变了才动，首次组合时记下当前值（切页签回来不自动回顶）
    var handledReselect by remember { mutableIntStateOf(reselectSignal) }
    LaunchedEffect(reselectSignal) {
        if (reselectSignal != handledReselect) {
            handledReselect = reselectSignal
            grid.scrollToItem(0)
        }
    }
    // 下一页：剩的不到 N 屏（插电 1.5 屏 / 电池 1 屏，至少剩 8 张时）就取，一次一页，取完用户不滑就不再取（见 PreloadPolicy）
    val plugged = LocalPluggedIn.current
    // 触发器跟着这个 FeedState 活（它记着"上次发起的位置"）；DisposableEffect 里释放 Rust 句柄
    val trigger = remember(state) { FeedPreloadTrigger() }
    DisposableEffect(trigger) { onDispose { trigger.close() } }
    LaunchedEffect(state, grid, plugged) {
        snapshotFlow {
            val info = grid.layoutInfo
            val visible = info.visibleItemsInfo.filter { it.key != FEED_FOOTER_KEY }
            // 网格里的卡片数（去掉脚注；屏蔽的作者不在网格里，所以不用 state.items.size）
            FeedFrame(visible.maxOfOrNull { it.index } ?: -1, (info.totalItemsCount - 1).coerceAtLeast(0), visible.size, state.loadingMore || state.refreshing)
        }.collect { f ->
            // 看过当前这批的一半就在后台备下一批
            state.onScrolledTo(f.lastVisible)
            // 两步走：Rust 判"该不该发起"，宿主真的发起成功才让 Rust 记下这次的位置
            val screens = PreloadPolicy.feedScreens(plugged)
            if (trigger.shouldStart(f.lastVisible, f.total, f.perScreen, screens, f.loading)) {
                if (state.loadMore()) trigger.noteStarted(f.lastVisible)
            }
        }
    }

    // 失败提示：标题行只放一个聚合的"N 个平台加载失败"，
    // 点开才是每家的原因 + 单独重试；断网（全部失败都是网络问题）另给一条"没有网络，显示的是之前的内容"。
    val notice = feedNoticeFor(state.errors)
    var showFailures by remember { mutableStateOf(false) }
    // 点卡片 = 进详情：用户看详情的时间正好用来在后台备下一批。
    // 只备信息流的下一批，**详情接口本身仍然一律不预取**（Preload.kt）。
    val openDetail: (FeedItem) -> Unit = { item -> state.onOpenDetail(); onOpen(item) }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.width(12.dp))
            // 搜索入口：所有平台 / 窗口宽度都是这一个放大镜
            if (onOpenSearch != null) {
                IconButton(onClick = onOpenSearch) { Icon(Icons.Outlined.Search, contentDescription = "搜索") }
            }
            if (notice != null) FailureChip(notice.failures) { showFailures = true }
            Spacer(Modifier.weight(1f))
            if (state.refreshing) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
            IconButton(onClick = state::refresh) { Icon(Icons.Outlined.Refresh, contentDescription = "刷新") }
        }
        if (notice != null && notice.networkOnly && state.items.isNotEmpty()) {
            OfflineBanner(onRetry = state::refresh)
        }
        // 网格 + 空状态这一坨两头共用：桌面上不包 [PullToRefreshBox]
        val body: @Composable () -> Unit = {
            Box(Modifier.fillMaxSize()) {
                if (gridSpec != null) {
                    FeedGrid(state, grid, gridSpec.columns, library, openDetail, Modifier.fillMaxSize().fixedLayoutWidth(gridSpec.width), exposure, surface, foreground, selectedKey)
                } else {
                    // 列数由 `App.kt` 按窗口宽度解析好传进来（自动 / 固定 N 列，见 [homeColumns]），这里不再看屏宽分档
                    FeedGrid(state, grid, columns, library, openDetail, Modifier.fillMaxSize(), exposure, surface, foreground, selectedKey)
                }
                // 空状态：原来只是网格脚注里一句"没有内容"贴在顶上，既没有原因也没有出口。
                // 盖在网格上而不是换掉它：网格的滚动位置 / 预取 / 下拉刷新都还在。
                // 这里只画。
                // **刚进来那一帧不说"空"**：第一次刷新还没开始时 [FeedState.refreshing] 还是 false，原来会闪一句
                // "这次没有内容"。现在空白时先分档（[feedBlankFor]）：还没加载过 / 正在刷新 → 现成的转圈，加载完还空 → 空状态。
                when (feedBlankFor(state.items.size, state.refreshing, state.loadedOnce)) {
                    FeedBlank.LOADING -> FeedLoading()
                    FeedBlank.EMPTY -> {
                        // 判定是纯函数，入参没变就不重算（`emptyInputs` 是 data class，值比较）
                        val copy = remember(emptyInputs) { feedEmptyFor(emptyInputs) }
                        FeedEmptyState(copy = copy, onAction = onEmptyAction, onRetry = state::refresh)
                    }
                    // 有内容：网格自己在画，什么都不盖
                    FeedBlank.CONTENT -> Unit
                }
            }
        }
        if (pullToRefresh) {
            PullToRefreshBox(isRefreshing = state.refreshing, onRefresh = state::refresh, modifier = Modifier.fillMaxSize()) { body() }
        } else {
            Box(Modifier.fillMaxSize()) { body() }
        }
    }
    if (showFailures) {
        FailuresDialog(state) { showFailures = false }
    }
}

/** 标题行那个聚合提示：只显示"几家失败"，细节点开看。 */
@Composable
private fun FailureChip(count: Int, onClick: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(
        shape = RoundedCornerShape(50),
        color = colors.errorContainer,
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Row(
            Modifier.padding(start = 8.dp, end = 10.dp, top = 3.dp, bottom = 3.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.ErrorOutline, contentDescription = null, tint = colors.onErrorContainer, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(4.dp))
            Text("$count 个平台加载失败", style = MaterialTheme.typography.labelSmall, color = colors.onErrorContainer, maxLines = 1)
        }
    }
}

/**
 * 断网提示：屏幕上这批是**之前**的内容，得说清楚，不然用户会以为刷新成功了。
 * 只在"失败的全是网络问题、而且屏幕上还有内容"时出现（没有内容时走 [FeedEmptyState]）。
 */
@Composable
private fun OfflineBanner(onRetry: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(color = colors.surfaceContainerHigh, modifier = Modifier.fillMaxWidth()) {
        Row(
            Modifier.padding(start = 16.dp, end = 8.dp, top = 2.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.CloudOff, contentDescription = null, tint = colors.onSurfaceVariant, modifier = Modifier.size(16.dp))
            Spacer(Modifier.width(8.dp))
            Text(
                "没有网络，显示的是之前的内容",
                style = MaterialTheme.typography.bodySmall,
                color = colors.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onRetry) { Text("重试") }
        }
    }
}

/**
 * 还没有内容、也还不能说"空"时的样子：**现成的转圈**，居中。
 *
 * 用的就是 App 里已有的加载样式（和标题行的刷新转圈、网格脚注的"加载更多"同一个 `CircularProgressIndicator`），
 * 这里不多长东西。
 */
@Composable
private fun FeedLoading() {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp)
    }
}

/**
 * 空白页：说清**为什么**空，并且给一个真能做的动作。
 * 文案和按钮全在 [feedEmptyFor]（纯函数，`FeedEmptyTest` 离线测），这里只负责画：图标 + 标题 + 一句人话 + 最多两个按钮。
 * 原来是"还没有内容 → 先到「我的 → 账号」登录至少一个平台"，只点亮小黑盒 / 抖音看「关注」时那句话是错的。
 */
@Composable
private fun FeedEmptyState(copy: FeedEmptyCopy, onAction: (FeedEmptyAction) -> Unit, onRetry: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Box(
        Modifier.fillMaxSize().background(colors.background).padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(
                if (copy.reason == FeedEmptyReason.OFFLINE) Icons.Outlined.CloudOff else Icons.Outlined.Inbox,
                contentDescription = null,
                tint = colors.onSurfaceVariant,
                modifier = Modifier.size(40.dp),
            )
            Text(copy.headline, style = MaterialTheme.typography.titleMedium)
            Text(copy.body, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, textAlign = TextAlign.Center)
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                for ((i, button) in copy.buttons.withIndex()) {
                    if (i == 0) {
                        Button(onClick = { onAction(button.action) }) { Text(button.label) }
                    } else {
                        OutlinedButton(onClick = { onAction(button.action) }) { Text(button.label) }
                    }
                }
                if (copy.retry) OutlinedButton(onClick = onRetry) { Text("重试") }
            }
        }
    }
}

/**
 * "平台加载失败"的明细：每家一行（平台 + 原因 + 单独重试）。
 * 原来是标题行里并排几行红字，现在只在点了那个聚合提示之后出现。
 */
@Composable
private fun FailuresDialog(state: FeedState, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("平台加载失败") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                for ((p, msg) in state.errors) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(p.label, style = MaterialTheme.typography.bodyMedium)
                            Text(msg, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        if (state.retrying == p) {
                            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                        } else {
                            TextButton(
                                onClick = { state.retryPlatform(p) },
                                enabled = !state.refreshing && !state.loadingMore && state.retrying == null,
                            ) { Text("重试") }
                        }
                    }
                }
                if (state.errors.isEmpty()) {
                    Text(
                        if (state.retrying != null) "正在重试…" else "都好了，这次没失败的平台。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("关闭") } },
    )
}

/**
 * 标题行该显示什么（纯逻辑，`FeedNoticeTest` 离线测）：
 * 没有失败 = null；[networkOnly] = 所有失败原因都是"网络错误"（[NETWORK_ERROR_TEXT]），
 * 界面据此说"没有网络，显示的是之前的内容"。
 */
internal data class FeedNotice(val failures: Int, val networkOnly: Boolean)

internal fun feedNoticeFor(errors: Map<Platform, String>): FeedNotice? {
    if (errors.isEmpty()) return null
    return FeedNotice(failures = errors.size, networkOnly = errors.values.all { it == NETWORK_ERROR_TEXT })
}

private val GridSidePadding = 12.dp
private val GridSpacing = 10.dp

/** 完全不在可视区里的那张卡被"露出来"时留的边：只露一条边，不是把它顶到顶端。 */
private val RevealMargin = 12.dp

/** 把选中卡片挪到边上时，一次最多滚半个屏（见 [revealMinimal]）；下面是单步下限和最多几步。 */
private const val RevealMinStepPx = 200
private const val RevealMaxSteps = 24

@Composable
private fun FeedGrid(
    state: FeedState,
    grid: LazyStaggeredGridState,
    columns: Int,
    library: Library,
    onOpen: (FeedItem) -> Unit,
    modifier: Modifier,
    exposure: ExposureInputs,
    surface: ExposureSurface,
    foreground: AppForeground,
    selectedKey: String? = null,
) {
    // 屏蔽的作者不显示（原条目还留在 state 里，解除屏蔽后又能看到）。
    // 用 derivedStateOf 缓存结果：只在 items 或屏蔽列表变化时重算，loadingMore / errors 之类的父级重组不再整表过滤。
    val shown by remember(state, library) {
        derivedStateOf { state.items.filter { item -> item.author?.let(library::isBlocked) != true } }
    }

    // 行为数据（曝光 / 停留）：判定和计时都在 [ExposureTracker] 里，这里只按 [EXPOSURE_TICK_MS] 喂"现在可见的卡片"。
    // rememberUpdatedState：跟踪循环只起一次，列表变了 / 这两个入参变了都读得到最新值，不用重启循环。
    val current = rememberUpdatedState(exposure)
    val currentShown = rememberUpdatedState(shown)
    // 曝光结算时顺带记"曝光过的卡片内容"：只有首页 / 关注有"哪一批、平台第几条"，
    // 详情 / 收藏 / 历史那些面不记（`FeedState.originOf` 查不到就不写）。
    val tracker = remember(library, surface, state) {
        ExposureTracker(
            library,
            surface,
            enabled = { current.value.enabled },
            origin = { state.originOf(it) },
            feedKind = when (surface) {
                ExposureSurface.HOME, ExposureSurface.FOLLOWING -> state.kind
                else -> null
            },
        )
    }
    // 窗口失焦、被全屏详情盖住（`App.kt` 的 covered）：都是 [ExposureInputs.foreground] = false → 结算，不是丢弃
    ExposureTicker(tracker, foreground = current.value.foreground) { gridVisibleItems(grid, currentShown.value) }

    // 截图工具（`print:feed`）看左栏滚到哪、选中谁
    SideEffect {
        DetailDebug.feedGridInfo = {
            "左栏 firstVisible=${grid.firstVisibleItemIndex} offset=${grid.firstVisibleItemScrollOffset} 选中=$selectedKey 条目=${currentShown.value.size}"
        }
    }
    // 瀑布流按"哪一列最短放哪一列"排，只有从上往下一张张排过才知道每张卡片在哪一列。列数一变（双栏开 / 关、窗口变宽窄）
    // 或者从保存的位置恢复，它只从当前这张开始重新排，上面的卡片在哪一列不知道，往回滚时边滚边猜、猜不齐就整片重排，
    // 卡片在列之间跳来跳去。所以这时先回到顶上、再一大截一大截往下滚回原来那张：中间每张卡片都排过一次，列就定了。
    // 滚动都是当场重新测量（不等下一帧），整个过程在同一帧里做完，看不到它在滚。
    //
    // 打开的详情是列表里的哪一条（[selectedKey]，双栏时左栏跟着选中 / 沉浸播放换下一条；全屏详情盖着时是 null）。
    // 而且只挪到最近的边上**。
    // 点开一张卡片时它本来就在眼前，列表要一动不动；只有详情里用快捷键 / 按钮切到屏幕外的上一条 / 下一条才滚。
    // **双栏刚开 / 关（列数同时变了）这一下不跟着滚**：那一下要的正是"列表原位不动"，重排是双栏自己的布局；
    // 只有列数没变、只是换了一条（上面那两种切法）才走 [FeedRevealSide]。
    val density = LocalDensity.current
    LaunchedEffect(state, grid, columns, selectedKey) {
        val prevColumns = state.lastLaidOutColumns
        val columnsChanged = prevColumns != 0 && prevColumns != columns
        state.lastLaidOutColumns = columns
        val selected = selectedKey?.let { key -> currentShown.value.indexOfFirst { it.key == key } } ?: -1
        if (selected >= 0) {
            if (columnsChanged) return@LaunchedEffect
            val side = feedRevealSide(selected, grid.layoutInfo.visibleItemsInfo.map { it.index })
            if (side == FeedRevealSide.NONE) return@LaunchedEffect
            grid.revealMinimal(selected, side, with(density) { RevealMargin.roundToPx() })
            return@LaunchedEffect
        }
        // 只在列数真的变化时才从顶部重建瀑布流列分配；切页签复用同一个 state.gridState，列分配不丢
        if (!columnsChanged) return@LaunchedEffect
        val index = grid.firstVisibleItemIndex
        if (index == 0) return@LaunchedEffect
        val offset = grid.firstVisibleItemScrollOffset
        grid.scrollToItem(0)
        val step = (grid.layoutInfo.viewportSize.height * 4).coerceAtLeast(4000).toFloat()
        while (grid.firstVisibleItemIndex < index && grid.canScrollForward) grid.scrollBy(step)
        grid.scrollToItem(index, offset)
    }
    // 卡片的像素宽度（缩略图按它要，见 CardThumbnails）：网格宽减掉左右内边距和列间距，均分
    var gridWidthPx by remember { mutableIntStateOf(0) }
    val cardWidthPx = if (gridWidthPx <= 0) 0 else with(density) {
        ((gridWidthPx - (GridSidePadding * 2).roundToPx() - GridSpacing.roundToPx() * (columns - 1)) / columns).coerceAtLeast(1)
    }
    // 一屏卡片的文案（时间 / 计数 / 时长）在列表这一层一次算好，重组时卡片只查表（见 CardTexts.kt）
    CompositionLocalProvider(LocalCardWidthPx provides cardWidthPx) {
        ProvideCardTexts(shown) {
        CardImagePrefetch(grid, shown, SingletonImageLoader.get(LocalPlatformContext.current), foreground)
        PrefetchedImages(grid, state.prefetched, SingletonImageLoader.get(LocalPlatformContext.current), foreground)
        LazyVerticalStaggeredGrid(
            columns = StaggeredGridCells.Fixed(columns),
            state = grid,
            contentPadding = PaddingValues(start = GridSidePadding, end = GridSidePadding, top = 4.dp, bottom = 24.dp),
            verticalItemSpacing = GridSpacing,
            horizontalArrangement = Arrangement.spacedBy(GridSpacing),
            modifier = modifier.onSizeChanged { gridWidthPx = it.width },
        ) {
            items(shown, key = { it.key }) { item ->
                // 换列数 / 刷新插入时卡片滑到新位置：时长和曲线跟双栏的过渡一致（默认的弹簧要晃半秒多，比详情滑入慢一截）
                FeedCard(
                    item,
                    onClick = { onOpen(item) },
                    modifier = Modifier.animateItem(placementSpec = tween(PaneAnimMillis, easing = FastOutSlowInEasing)),
                )
            }
            item(span = StaggeredGridItemSpan.FullLine, key = FEED_FOOTER_KEY) {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                    when {
                        state.loadingMore -> CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        // 空列表由 [FeedEmptyState] 居中画（带「去登录」/「重试」），脚注不再重复一句"没有内容"
                        else -> Unit
                    }
                }
            }
        }
        }
    }
}

/**
 * 只把第 [index] 张挪到可视区边上：[side] 说它现在在可视区外面哪一边，[marginPx] 是露出来的那条边的宽。
 *
 * 为什么不直接 `scrollToItem(index)`：瀑布流只量过屏幕上的那些卡片，下面（上面）那些没有尺寸；`scrollToItem` 跳过中间
 * 一大段直接落在那张上，再往回挪时中间那些卡片的列是"猜"的，位置会飘。
 * 这里改成**一截一截往它那边滚**（每截半个屏，卡片按顺序量过去，位置是准的），等它露出来再按量到的位置挪到边上；
 * 一截不超过半个屏，所以不会把整张卡片跳过。
 */
private suspend fun LazyStaggeredGridState.revealMinimal(index: Int, side: FeedRevealSide, marginPx: Int) {
    if (side == FeedRevealSide.NONE) return
    repeat(RevealMaxSteps) {
        val info = layoutInfo
        val item = info.visibleItemsInfo.firstOrNull { it.index == index }
        if (item != null) {
            val adjust = revealAdjustPx(side, item.offset.y, item.size.height, info.viewportSize.height, marginPx)
            if (adjust != 0) scrollBy(adjust.toFloat())
            return
        }
        val step = (info.viewportSize.height / 2).coerceAtLeast(RevealMinStepPx)
        val moved = when (side) {
            FeedRevealSide.BELOW -> if (canScrollForward) scrollBy(step.toFloat()) else 0f
            FeedRevealSide.ABOVE -> if (canScrollBackward) scrollBy(-step.toFloat()) else 0f
            FeedRevealSide.NONE -> 0f
        }
        // 已经到头了（到顶 / 到底）：这张卡片到不了可视区，就停在这儿，不硬滚
        if (moved == 0f) return
    }
}
