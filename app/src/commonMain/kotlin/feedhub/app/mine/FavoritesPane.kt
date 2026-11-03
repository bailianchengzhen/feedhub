package feedhub.app.mine

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.staggeredgrid.LazyVerticalStaggeredGrid
import androidx.compose.foundation.lazy.staggeredgrid.StaggeredGridCells
import androidx.compose.foundation.lazy.staggeredgrid.items
import androidx.compose.foundation.lazy.staggeredgrid.rememberLazyStaggeredGridState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import feedhub.app.ContentType
import feedhub.app.library.ExposureSurface
import feedhub.app.library.Favorite
import feedhub.app.library.FavoriteFolder
import feedhub.app.library.FavoriteQuery
import feedhub.app.library.FolderFilter
import feedhub.app.feed.ProvideCardTexts
import feedhub.app.library.Library
import feedhub.app.ui.relativeTime
import feedhub.core.FeedItem
import feedhub.feed.ExposureInputs

private sealed interface FolderDialog {
    data object Create : FolderDialog
    data class Rename(val folder: FavoriteFolder) : FolderDialog
    data class Delete(val folder: FavoriteFolder) : FolderDialog
}

/**
 * 收藏：收藏夹条 + 平台 / 类型筛选 + 瀑布流卡片（复用 [feedhub.app.feed.FeedCard]）。
 * 卡片右上角的"⋯"：取消收藏、移到别的收藏夹。点卡片打开详情。
 *
 * [exposure] 是界面算好的行为数据两个入参（[ExposureInputs]），照 `App.kt` 的约定（面 = [ExposureSurface.FAVORITES]）。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FavoritesPane(
    state: MineState,
    library: Library,
    onOpen: (FeedItem) -> Unit,
    onBack: (() -> Unit)?,
    exposure: ExposureInputs = ExposureInputs(),
) {
    val folders = library.folders
    // 当前选中的收藏夹被删了（或不存在）就回到"全部"
    val folderFilter = state.favoriteFolder.takeIf { f -> f !is FolderFilter.Folder || folders.any { it.id == f.id } } ?: FolderFilter.All
    var dialog by remember { mutableStateOf<FolderDialog?>(null) }
    val query = FavoriteQuery(folderFilter, state.favoritePlatform, state.favoriteType)
    val favorites by produceState<List<Favorite>?>(null, query, library.favoritesRevision) { value = library.favorites(query) }

    PaneScaffold("收藏", onBack, subtitle = "共 ${library.favoriteCount()} 条") {
        // 收藏夹条：横向滚动，最后是"新建"
        Row(
            Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            FolderChip("全部", library.favoriteCount(FolderFilter.All), folderFilter == FolderFilter.All) { state.favoriteFolder = FolderFilter.All }
            FolderChip("默认收藏夹", library.favoriteCount(FolderFilter.Default), folderFilter == FolderFilter.Default) {
                state.favoriteFolder = FolderFilter.Default
            }
            for (f in folders) {
                val sel = (folderFilter as? FolderFilter.Folder)?.id == f.id
                FolderChip(f.name, library.favoriteCount(FolderFilter.Folder(f.id)), sel) { state.favoriteFolder = FolderFilter.Folder(f.id) }
            }
            FilterChip(
                selected = false,
                onClick = { dialog = FolderDialog.Create },
                label = { Text("新建收藏夹") },
                leadingIcon = { Icon(Icons.Outlined.Add, contentDescription = null, modifier = Modifier.width(16.dp)) },
            )
        }
        // 选中了自己建的收藏夹时：重命名 / 删除
        val selected = (folderFilter as? FolderFilter.Folder)?.let { sel -> folders.firstOrNull { it.id == sel.id } }
        if (selected != null) {
            Row(Modifier.padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("收藏夹「${selected.name}」", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                TextButton(onClick = { dialog = FolderDialog.Rename(selected) }) { Text("重命名") }
                TextButton(onClick = { dialog = FolderDialog.Delete(selected) }) { Text("删除", color = MaterialTheme.colorScheme.error) }
            }
        }
        // 筛选：平台 + 类型（一行，窄屏可横向滚动）
        PlatformFilter(state.favoritePlatform, { state.favoritePlatform = it }, Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
            Spacer(Modifier.width(8.dp))
            for ((t, label) in listOf(ContentType.MIXED to "全部类型", ContentType.VIDEO to "视频", ContentType.TEXT to "图文")) {
                FilterChip(selected = state.favoriteType == t, onClick = { state.favoriteType = t }, label = { Text(label) })
            }
        }
        val list = favorites
        when {
            list == null -> Box(Modifier.fillMaxSize())
            list.isEmpty() -> EmptyHint(
                if (library.favoriteCount() == 0) "还没有收藏。在详情页顶栏点收藏，就会出现在这里。" else "没有符合筛选条件的收藏。",
            )
            else -> {
                val grid = rememberLazyStaggeredGridState()
                val items = list.map { it.item }
                // 曝光：收藏页的卡片也算看到过（面 = favorites），判定 / 结算复用同一个 tracker
                TrackExposures(grid, items, ExposureSurface.FAVORITES, library, exposure)
                // 卡片自己的元信息（时间 / 计数 / 时长）一屏算一次（见 CardTexts.kt）；卡片下面那行
                // "收藏于 …" 是每条的收藏时刻，另算（一屏几条，逐条过边界）
                ProvideCardTexts(items) {
                    LazyVerticalStaggeredGrid(
                        columns = StaggeredGridCells.Adaptive(190.dp),
                        state = grid,
                        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 24.dp),
                        verticalItemSpacing = 10.dp,
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) {
                        items(list, key = { it.item.key }) { fav ->
                            FavoriteCard(fav, folders, library, onOpen, Modifier.animateItem())
                        }
                    }
                }
            }
        }
    }

    when (val d = dialog) {
        null -> Unit
        FolderDialog.Create -> NameDialog(
            title = "新建收藏夹",
            initial = "",
            confirmText = "新建",
            validate = { n -> if (folders.any { it.name == n }) "已经有同名的收藏夹" else null },
            onConfirm = { n -> state.favoriteFolder = FolderFilter.Folder(library.createFolder(n).id); dialog = null },
            onDismiss = { dialog = null },
        )
        is FolderDialog.Rename -> NameDialog(
            title = "重命名收藏夹",
            initial = d.folder.name,
            confirmText = "确定",
            validate = { n -> if (folders.any { it.id != d.folder.id && it.name == n }) "已经有同名的收藏夹" else null },
            onConfirm = { n -> library.renameFolder(d.folder.id, n); dialog = null },
            onDismiss = { dialog = null },
        )
        is FolderDialog.Delete -> DeleteFolderDialog(
            d.folder,
            library.favoriteCount(FolderFilter.Folder(d.folder.id)),
            onMove = { library.deleteFolder(d.folder.id, deleteItems = false); dialog = null },
            onDeleteAll = { library.deleteFolder(d.folder.id, deleteItems = true); dialog = null },
            onDismiss = { dialog = null },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FolderChip(name: String, count: Int, selected: Boolean, onClick: () -> Unit) {
    FilterChip(selected = selected, onClick = onClick, label = { Text("$name $count") })
}

@Composable
private fun FavoriteCard(fav: Favorite, folders: List<FavoriteFolder>, library: Library, onOpen: (FeedItem) -> Unit, modifier: Modifier) {
    var menu by remember { mutableStateOf(false) }
    ActionCard(
        item = fav.item,
        onOpen = { onOpen(fav.item) },
        footer = "收藏于 ${relativeTime(fav.addedAt / 1000)}",
        modifier = modifier,
        action = {
            SmallIconButton(onClick = { menu = true }, description = "更多", icon = Icons.Outlined.MoreVert)
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text("取消收藏", color = MaterialTheme.colorScheme.error) },
                    onClick = { menu = false; library.removeFavorite(fav.item.key) },
                )
                HorizontalDivider()
                if (fav.folder != null) {
                    DropdownMenuItem(text = { Text("移到：默认收藏夹") }, onClick = { menu = false; library.addFavorite(fav.item, null) })
                }
                for (f in folders) {
                    if (f.id == fav.folder) continue
                    DropdownMenuItem(text = { Text("移到：${f.name}") }, onClick = { menu = false; library.addFavorite(fav.item, f.id) })
                }
            }
        },
    )
}

@Composable
private fun DeleteFolderDialog(folder: FavoriteFolder, count: Int, onMove: () -> Unit, onDeleteAll: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("删除收藏夹「${folder.name}」？") },
        text = {
            Text(
                if (count == 0) "这个收藏夹是空的。" else "里面有 $count 条收藏：可以移回默认收藏夹，也可以连收藏一起删掉（不能恢复）。",
            )
        },
        confirmButton = {
            Column(horizontalAlignment = Alignment.End) {
                TextButton(onClick = onMove) { Text(if (count == 0) "删除" else "删除收藏夹，收藏移回默认") }
                if (count > 0) TextButton(onClick = onDeleteAll) { Text("连收藏一起删除", color = MaterialTheme.colorScheme.error) }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}
