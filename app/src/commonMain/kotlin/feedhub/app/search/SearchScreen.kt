package feedhub.app.search

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.foundation.lazy.staggeredgrid.LazyVerticalStaggeredGrid
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridCells
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridItemSpan
import androidx.compose.foundation.lazy.staggeredgrid.items
import androidx.compose.foundation.lazy.staggeredgrid.rememberLazyStaggeredGridState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.app.feed.FeedCard
import feedhub.app.feed.ProvideCardTexts
import feedhub.app.feed.LocalCardWidthPx
import feedhub.app.ui.brandColor
import feedhub.core.FeedItem
import feedhub.core.Platform
import kotlinx.coroutines.delay

/**
 * 跨平台搜索页。
 *
 * 入口：
 * 桌面和手机都是**首页标题行的一个放大镜按钮**，点开整页搜索页（首页的滚动位置 / 内容都还在，
 * 关掉就回到原位置）。桌面另有快捷键：`Ctrl+F` / `/` 打开、`Esc` 关（见 `App.kt` 的 `appKeyAction`）。
 *
 * 布局只有一套（手机那套）：顶上返回箭头 + 输入框 + 一排平台标签，进来自动聚焦输入框、回车搜；
 * 结果网格的列数按窗口宽度算（[columns]，宽窗口多列），只有这一处是"桌面 / 手机不同"。
 *
 * 内容：搜索框 + 一排平台标签（默认 = 这次参与搜索的平台，也就是首页点亮的那些）+ 瀑布流结果。
 * **平台标签就是筛选**：只有亮着的几家会被请求、
 * 也只有它们的结果会留在列表里；点灭立刻把这一家拿掉（不再请求），点亮立刻用当前关键词补搜这一家。
 * 至少留一家亮着（最后一家点不灭，点它会提示）。**这个选择只在这次搜索页里有效**，不写进设置，
 * 关掉搜索页（[SearchState.close]）就回到全亮。每张卡片自带平台标签（[FeedCard] 的 `PlatformTag`：平台色 + 平台名），
 * 一批一批追加、已上屏的位置不插东西。
 *
 * 排序只有"相关"，所以没有排序控件；不做搜索历史。
 */
@Composable
fun SearchScreen(
    state: SearchState,
    onBack: () -> Unit,
    onOpen: (FeedItem) -> Unit,
    columns: Int,
    /**
     * 搜索页上有没有一条详情盖在上面：它盖着时这条链的"返回"归详情自己，
     * 关掉之后要把键盘焦点还给输入框（否则 Esc / 回车都收不到）。
     */
    detailOpen: Boolean = false,
    modifier: Modifier = Modifier,
) {
    var text by remember(state) { mutableStateOf(state.query) }
    // 刚打开（还没搜过）：摆出这次能搜的平台（只画 chips，不发请求）
    LaunchedEffect(state) { state.preparePlatforms() }
    // 最后一家点不灭：点了一下给个提示（2 秒后自己消失，不占地方）
    var blocked by remember(state) { mutableStateOf(false) }
    LaunchedEffect(blocked) {
        if (blocked) {
            delay(2_000)
            blocked = false
        }
    }
    val focus = remember { FocusRequester() }
    // 打开就聚焦、弹键盘（桌面上是光标在框里，可以直接打字）；盖在上面的详情关掉之后也要拿回来
    LaunchedEffect(detailOpen) {
        if (!detailOpen) runCatching { focus.requestFocus() }
    }
    val grid = rememberLazyStaggeredGridState()
    val current = rememberUpdatedState(state)
    // 系统返回键：先退出搜索页（首页的位置 / 内容都不动）。宽屏也要拦。
    feedhub.app.ui.PlatformBackHandler(enabled = true) { onBack() }

    // 滑到底自动取下一批（和首页同一套"还剩几屏"的简单版：快到底就取）
    LaunchedEffect(grid, state) {
        snapshotFlow {
            val info = grid.layoutInfo
            val last = info.visibleItemsInfo.maxOfOrNull { it.index } ?: -1
            last to info.totalItemsCount
        }.collect { (last, total) ->
            if (total > 0 && last >= total - 6) {
                val s = current.value
                if (!s.loading && !s.loadingMore && !s.exhausted) s.loadMore()
            }
        }
    }

    Column(modifier.fillMaxSize()) {
        // 顶上就一套：返回箭头 + 输入框（桌面也一样，见类注释）
        Row(
            Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 6.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回") }
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                modifier = Modifier.weight(1f).focusRequester(focus),
                singleLine = true,
                // 不再列这次会搜哪些平台——下面那排平台标签就是（还能点）
                placeholder = { Text("搜索") },
                leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = { if (text.isNotBlank()) state.search(text) }),
                trailingIcon = {
                    TextButton(onClick = { if (text.isNotBlank()) state.search(text) }) { Text("搜索") }
                },
            )
        }
        // 平台标签 = **筛选**：只有亮着的会请求、也只有它们的结果在列表里。点灭 / 点亮立刻按当前关键词重搜那一部分
        // （亮着的几家已经拿到的结果保留、不重发；新点亮的那家补一页）。
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // 平台多时横向可滚（手机上 5 家刚好差一点，6 家就放不下了；桌面窗口窄的时候一样）
            Row(
                Modifier.weight(1f).horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                for (p in state.platforms) {
                    PlatformChip(p, on = p in state.enabled) {
                        // 最后一家不让点灭（至少留一家亮着）
                        if (!state.toggle(p)) blocked = true
                    }
                }
            }
            Spacer(Modifier.width(8.dp))
            ProgressText(state)
        }
        if (blocked) {
            Text(
                "至少留一家亮着",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
            )
        }
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            for ((p, msg) in state.errors) {
                Text(
                    "${p.label}：$msg",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.error,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.background(MaterialTheme.colorScheme.errorContainer.copy(alpha = 0.5f), androidx.compose.foundation.shape.RoundedCornerShape(6.dp))
                        .padding(horizontal = 6.dp, vertical = 2.dp),
                )
            }
        }
        val shown = remember(state.items, state.enabled) { state.items.filter { it.platform in state.enabled } }
        SearchGrid(shown, state, grid, columns, onOpen, Modifier.fillMaxSize().weight(1f))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PlatformChip(platform: Platform, on: Boolean, onClick: () -> Unit) {
    FilterChip(
        selected = on,
        onClick = onClick,
        label = { Text(platform.label) },
        leadingIcon = {
            Box(Modifier.size(8.dp).background(platform.brandColor(), CircleShape))
        },
    )
}

@Composable
private fun ProgressText(state: SearchState) {
    val total = state.enabled.size
    when {
        state.loading -> Row(verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(6.dp))
            Text("搜索中…", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        total > 0 -> Text(
            "已到 ${state.answered} / $total 家",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SearchGrid(
    items: List<FeedItem>,
    state: SearchState,
    grid: androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState,
    columns: Int,
    onOpen: (FeedItem) -> Unit,
    modifier: Modifier,
) {
    var gridWidthPx by remember { mutableIntStateOf(0) }
    val density = LocalDensity.current
    val cardWidthPx: Int = if (gridWidthPx <= 0) 0 else with(density) {
        ((gridWidthPx - (24.dp.toPx() + 10.dp.toPx() * (columns - 1))) / columns).coerceAtLeast(1f).toInt()
    }
    CompositionLocalProvider(LocalCardWidthPx provides cardWidthPx) {
        // 一屏卡片的文案一次算好（见 CardTexts.kt）
        ProvideCardTexts(items) {
        LazyVerticalStaggeredGrid(
            columns = StaggeredGridCells.Fixed(columns),
            state = grid,
            contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 24.dp),
            verticalItemSpacing = 10.dp,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            modifier = modifier.onSizeChanged { gridWidthPx = it.width },
        ) {
            items(items, key = { it.key }) { item ->
                FeedCard(item, onClick = { onOpen(item) })
            }
            item(span = StaggeredGridItemSpan.FullLine) {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                    when {
                        state.loadingMore -> CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        state.loading -> Unit
                        items.isEmpty() && state.query.isNotEmpty() && state.errors.isEmpty() -> Text("没有搜到内容", color = MaterialTheme.colorScheme.onSurfaceVariant)
                        state.exhausted && items.isNotEmpty() -> Text("已经到底了", color = MaterialTheme.colorScheme.onSurfaceVariant)
                        else -> Unit
                    }
                }
            }
        }
        }
    }
}
