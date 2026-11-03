package feedhub.app.mine

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.staggeredgrid.LazyVerticalStaggeredGrid
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridCells
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridItemSpan
import androidx.compose.foundation.lazy.staggeredgrid.items
import androidx.compose.foundation.lazy.staggeredgrid.rememberLazyStaggeredGridState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import feedhub.app.library.ExposureSurface
import feedhub.app.library.HistoryEntry
import feedhub.app.library.HistoryQuery
import feedhub.app.library.Library
import feedhub.app.ui.duration
import feedhub.app.video.isVideoPost
import feedhub.app.feed.ProvideCardTexts
import feedhub.core.FeedItem
import kotlinx.coroutines.delay
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import feedhub.feed.ExposureInputs

private const val PAGE = 200

private val timeFormat = DateTimeFormatter.ofPattern("HH:mm")

/**
 * 历史记录：按日期分组（今天 / 昨天 / 具体日期）的瀑布流卡片，可按平台筛选、按标题搜索、删单条、清空（要确认）。
 * 视频卡片上显示看到哪了。数据多时每次多取一页（滑到底再取），不一次全读出来。
 *
 * [exposure] 是界面算好的行为数据两个入参（[ExposureInputs]），照 `App.kt` 的约定（面 = [ExposureSurface.HISTORY]）。
 */
@Composable
fun HistoryPane(
    state: MineState,
    library: Library,
    onOpen: (FeedItem) -> Unit,
    onBack: (() -> Unit)?,
    exposure: ExposureInputs = ExposureInputs(),
) {
    var limit by remember(state.historyPlatform, state.historySearch) { mutableIntStateOf(PAGE) }
    // 搜索框打字时等 250ms 再查
    var search by remember { mutableStateOf(state.historySearch) }
    LaunchedEffect(search) {
        if (search != state.historySearch) delay(250)
        state.historySearch = search
    }
    val platform = state.historyPlatform
    val query = HistoryQuery(platform, state.historySearch, limit)
    val entries by produceState<List<HistoryEntry>?>(null, query, library.historyRevision) { value = library.history(query) }
    val total by produceState(0, library.historyRevision) { value = library.historyCount() }
    val matching by produceState(0, platform, state.historySearch, library.historyRevision) {
        value = library.historyCount(platform, state.historySearch)
    }
    var confirmClear by remember { mutableStateOf(false) }

    PaneScaffold(
        "历史记录",
        onBack,
        subtitle = "共 $total 条",
        actions = { TextButton(onClick = { confirmClear = true }, enabled = total > 0) { Text("清空", color = if (total > 0) MaterialTheme.colorScheme.error else Color.Unspecified) } },
    ) {
        OutlinedTextField(
            value = search,
            onValueChange = { search = it },
            singleLine = true,
            placeholder = { Text("按标题、作者搜索") },
            leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
            trailingIcon = {
                if (search.isNotEmpty()) IconButton(onClick = { search = "" }) { Icon(Icons.Outlined.Close, contentDescription = "清除搜索") }
            },
            shape = RoundedCornerShape(24.dp),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        )
        PlatformFilter(platform, { state.historyPlatform = it }, Modifier.fillMaxWidth().padding(horizontal = 12.dp))
        val list = entries
        when {
            list == null -> Box(Modifier.fillMaxSize())
            list.isEmpty() -> EmptyHint(
                if (total == 0) "还没有历史记录。打开过的详情会记在这里。" else "没有符合条件的记录。",
            )
            else -> HistoryGrid(list, matching, library, onOpen, exposure, loadMore = { if (list.size >= limit) limit += PAGE })
        }
    }

    if (confirmClear) {
        ConfirmDialog(
            title = "清空历史记录？",
            text = "会删掉全部 $total 条历史记录和视频的播放进度，不能恢复。收藏不受影响。",
            confirmText = "清空",
            onConfirm = { library.clearHistory(); confirmClear = false },
            onDismiss = { confirmClear = false },
        )
    }
}

@Composable
private fun HistoryGrid(
    entries: List<HistoryEntry>,
    matching: Int,
    library: Library,
    onOpen: (FeedItem) -> Unit,
    exposure: ExposureInputs,
    loadMore: () -> Unit,
) {
    val zone = remember { ZoneId.systemDefault() }
    val today = LocalDate.now(zone)
    val groups = remember(entries) { entries.groupBy { Instant.ofEpochMilli(it.visitedAt).atZone(zone).toLocalDate() } }
    val grid = rememberLazyStaggeredGridState()
    // 曝光：网格的下标里日期分组头也占一项，所以按同样的顺序铺一张"下标 → 条目"的表（分组头是 null，会被跳过）
    val flat = remember(groups) {
        buildList<FeedItem?> {
            for ((_, day) in groups) {
                add(null)
                day.forEach { add(it.item) }
            }
        }
    }
    TrackExposures(grid, flat, ExposureSurface.HISTORY, library, exposure)
    val nearEnd by remember {
        derivedStateOf {
            val info = grid.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()?.index ?: return@derivedStateOf false
            info.totalItemsCount > 0 && last >= info.totalItemsCount - 8
        }
    }
    LaunchedEffect(nearEnd, entries.size) { if (nearEnd && entries.size < matching) loadMore() }
    // 卡片自己的元信息（时间 / 计数 / 时长）一屏算一次（见 CardTexts.kt）；"看于 …" 那行是每条的浏览时刻，
    // 和分组标题的 [dayLabel] 一样数量少，逐条算（分组标题一屏几个，卡片下面的进度是 1～2 处）
    ProvideCardTexts(flat.filterNotNull()) {
    LazyVerticalStaggeredGrid(
        columns = StaggeredGridCells.Adaptive(190.dp),
        state = grid,
        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 24.dp),
        verticalItemSpacing = 10.dp,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        modifier = Modifier.fillMaxSize(),
    ) {
        for ((date, day) in groups) {
            item(key = "day-$date", span = StaggeredGridItemSpan.FullLine) {
                Text(
                    dayLabel(date, today),
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 4.dp, top = 10.dp, bottom = 2.dp),
                )
            }
            items(day, key = { it.item.key }) { e -> HistoryCard(e, library, onOpen, Modifier.animateItem()) }
        }
    }
    }
}

@Composable
private fun HistoryCard(e: HistoryEntry, library: Library, onOpen: (FeedItem) -> Unit, modifier: Modifier) {
    // 进度优先取资料库里最新的（正在看的视频会更新），列表里的是查询时的；
    // 只在内存快照里看，命中不了就用列表里的值，不要在主线程逐条查库。
    val progress = library.progressCached(e.item.key) ?: e.progressMs
    ActionCard(
        item = e.item,
        onOpen = { onOpen(e.item) },
        footer = "看于 ${timeFormat.format(Instant.ofEpochMilli(e.visitedAt).atZone(ZoneId.systemDefault()))}",
        modifier = modifier,
        overlay = { if (e.item.isVideoPost() && progress != null) ProgressOverlay(progress, e.durationMs) },
        action = { SmallIconButton(onClick = { library.removeHistory(e.item.key) }, description = "删除这条记录", icon = Icons.Outlined.Close) },
    )
}

/** 视频封面上的"看到 12:34 / 45:00"和封面底边的进度条。封面是卡片宽度的 10/16 高。 */
@Composable
private fun BoxScope.ProgressOverlay(positionMs: Long, durationMs: Long?) {
    val fraction = if (durationMs != null && durationMs > 0) (positionMs.toFloat() / durationMs).coerceIn(0f, 1f) else null
    val done = fraction != null && fraction >= 0.97f
    val text = when {
        positionMs <= 0 -> null
        done -> "已看完"
        durationMs != null && durationMs > 0 -> "看到 ${duration((positionMs / 1000).toInt())} / ${duration((durationMs / 1000).toInt())}"
        else -> "看到 ${duration((positionMs / 1000).toInt())}"
    }
    if (text != null) {
        Text(
            text,
            color = Color.White,
            fontSize = 11.sp,
            modifier = Modifier.align(Alignment.TopStart).padding(6.dp)
                .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(4.dp))
                .padding(horizontal = 5.dp, vertical = 2.dp),
        )
    }
    if (fraction != null) {
        BoxWithConstraints(Modifier.matchParentSize()) {
            LinearProgressIndicator(
                progress = { fraction },
                modifier = Modifier.fillMaxWidth().height(3.dp).offset(y = maxWidth * 10f / 16f - 3.dp),
                color = MaterialTheme.colorScheme.primary,
                trackColor = Color.Black.copy(alpha = 0.35f),
            )
        }
    }
}
