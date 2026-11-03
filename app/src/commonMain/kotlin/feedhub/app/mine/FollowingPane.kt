package feedhub.app.mine

import feedhub.app.LocalPlatforms
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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import feedhub.app.feed.Avatar
import feedhub.core.FollowKind
import feedhub.core.FollowTarget
import feedhub.core.Platform

/**
 * 关注管理：B站关注的 UP 主、贴吧关注的吧（分页，滑到底自动取下一页）。每项有"取关"按钮，
 * **点了先弹确认框，确认后才会调 `setFollowing(target, false)`**；读列表本身是只读的。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FollowingPane(state: MineState, onBack: (() -> Unit)?) {
    // 平台标签 / 空列表的叫法按插件描述（B站"B站 UP 主"/" UP 主"，贴吧用默认的"关注的吧"/"吧"）
    val platforms = LocalPlatforms.current
    val sources = state.followSources
    val current = sources.firstOrNull { it.platform == state.followPlatform } ?: sources.firstOrNull()
    val follow = current?.let { state.following(it) }
    LaunchedEffect(follow) { follow?.ensureLoaded() }

    PaneScaffold(
        "关注管理",
        onBack,
        actions = {
            if (follow != null) {
                if (follow.loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                IconButton(onClick = follow::refresh) { Icon(Icons.Outlined.Refresh, contentDescription = "刷新") }
            }
        },
    ) {
        if (current == null || follow == null) {
            EmptyHint("没有支持关注管理的平台。")
            return@PaneScaffold
        }
        Row(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for (s in sources) {
                FilterChip(
                    selected = s.platform == current.platform,
                    onClick = { state.followPlatform = s.platform },
                    label = { Text(platforms[s.platform]?.texts?.followTab ?: "${s.platform.label} 关注的吧") },
                )
            }
        }
        Text(
            "取关会直接改动你在${current.platform.label}上的关注，点了会先问一次。",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
        follow.actionError?.let { msg ->
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(msg, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
                TextButton(onClick = follow::dismissActionError) { Text("知道了") }
            }
        }
        val listState = rememberLazyListState()
        val nearEnd by remember {
            derivedStateOf {
                val info = listState.layoutInfo
                val last = info.visibleItemsInfo.lastOrNull()?.index ?: return@derivedStateOf false
                info.totalItemsCount > 0 && last >= info.totalItemsCount - 5
            }
        }
        LaunchedEffect(nearEnd, follow.items.size) { if (nearEnd) follow.loadMore() }

        if (follow.items.isEmpty() && !follow.loading) {
            if (follow.error != null) {
                ErrorRetry(follow.error!!, follow::retry)
            } else {
                EmptyHint(if (follow.loaded) "没有关注任何${platforms[current.platform]?.texts?.followNoun ?: "吧"}。" else "")
            }
        } else {
            LazyColumn(
                state = listState,
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 24.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxSize(),
            ) {
                items(follow.items, key = { "${it.kind}:${it.id}" }) { t ->
                    FollowRow(t, busy = t.id in follow.busy, onUnfollow = { follow.askUnfollow(t) })
                }
                item(key = "footer") {
                    Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
                        when {
                            follow.loading -> CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                            follow.error != null -> ErrorRetry(follow.error!!, follow::retry)
                            !follow.hasMore -> Text("共 ${follow.items.size} 个", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
    }

    follow?.pendingUnfollow?.let { t ->
        ConfirmDialog(
            title = "取消关注「${t.name}」？",
            text = "会在${t.platform.label}上取消对${if (t.kind == FollowKind.FORUM) "这个吧" else "这位 UP 主"}的关注。",
            confirmText = "取消关注",
            dismissText = "再想想",
            onConfirm = follow::confirmUnfollow,
            onDismiss = follow::cancelUnfollow,
        )
    }
}

@Composable
private fun ErrorRetry(message: String, onRetry: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(message, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = onRetry) { Text("重试") }
    }
}

@Composable
private fun FollowRow(target: FollowTarget, busy: Boolean, onUnfollow: () -> Unit) {
    Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainerLowest, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Avatar(target.avatar, 40)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                OneLine(target.name)
                target.description?.takeIf { it.isNotBlank() }?.let { OneLine(it, muted = true) }
            }
            Spacer(Modifier.width(8.dp))
            OutlinedButton(onClick = onUnfollow, enabled = !busy) {
                Text(if (busy) "处理中…" else "取关", color = if (busy) Color.Unspecified else MaterialTheme.colorScheme.error)
            }
        }
    }
}
