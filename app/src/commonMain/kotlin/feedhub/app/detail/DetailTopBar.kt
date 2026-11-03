package feedhub.app.detail

import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.SystemActions
import feedhub.app.feed.PlatformTag
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Platform

/**
 * 详情页顶栏：左边返回，右边收藏、分享、屏蔽作者；窄屏（宽 < 600dp）时屏蔽收进"⋯"菜单。
 * 图文详情和视频详情共用，要在 [DetailPane] 里（动作靠 [LocalDetailHost]）。
 *
 * 窄屏滚下去淡入的是**平台名**，不是作者名。
 *
 * - 收藏 / 取消收藏：本地收藏（[feedhub.app.library.Library]）。
 * - 分享：标题 + 官方链接（去掉跟踪参数，见 [cleanShareUrl]）。**安卓弹系统分享面板**（[AppGraph.systemActions]
 *   的 `share`），桌面 / 没有系统界面时复制到剪贴板、底部提示"已复制"。
 * - 屏蔽作者：加入本地屏蔽列表，然后关掉整个详情。
 * - "在 {平台} 打开"：安卓才有（[SystemActions.available]），深链格式写在插件描述里。
 *
 * @param loaded 详情接口拿到的更完整的条目（收藏时存它，列表里的缩略图更全）；还没加载好就用 [item]。
 *
 */
@Composable
fun DetailTopBar(
    graph: AppGraph,
    item: FeedItem,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    loaded: FeedItem? = null,
    /**
     * 窄屏顶栏滚下去淡入的那一行文字的不透明度：0 = 还没滚动（不显示），1 = 完全显示。
     * 做成 lambda 是为了让滚动时只重组顶栏这一小块。
     *
     * 文字是**平台名**。
     *
     * 视频详情传 `{ 1f }`：一进来就显示平台名，不跟滚动；
     * 图文详情照旧滚动淡入（列表的 `firstVisibleItemScrollOffset`，96dp 内 0 → 1）。
     */
    titleAlpha: () -> Float = { 0f },
) {
    val host = LocalDetailHost.current
    val effective = remember(item, loaded) { item.mergedWithDetail(loaded) }
    val favorite = graph.library.isFavorite(item.key)
    val url = effective.url ?: item.url
    val author = effective.author
    val block = {
        if (author != null) {
            graph.library.block(author)
            host?.closeAll() ?: onBack()
        }
    }
    // 「在 官方 App 打开」只在有系统界面的那一端出现（安卓）；深链来自插件描述，没有深链就退化成网页。
    val openExternal = if (graph.systemActions.available) externalOpenOfItem(effective, graph.platforms) else null
    val share = {
        if (url != null) {
            // 安卓：系统分享面板（ACTION_SEND）；桌面 / 没有系统界面时照旧复制
            val text = shareText(shareTitle(effective), url)
            if (!graph.systemActions.share(text)) host?.copy(text, "已复制标题和链接")
        }
    }

    Column(modifier.fillMaxWidth()) {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val narrow = maxWidth < 600.dp
            val menuActions = detailMenuActions(
                narrow = narrow,
                systemUi = graph.systemActions.available,
                hasExternal = openExternal != null,
                hasUrl = url != null,
                hasAuthor = author != null,
            )
            val copyLink = { if (url != null) host?.copy(cleanShareUrl(url), "已复制链接") }
            Row(Modifier.fillMaxWidth().height(52.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "返回") }
                if (narrow) {
                    // 窄屏：只留返回 / 平台名（滚下去淡入）/ 收藏 / 分享 / ⋯。平台标签和"视频""帖子"让位给平台名
                    Text(
                        topBarPlatformName(item, graph.platforms),
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f).padding(start = 4.dp).alpha(titleAlpha().coerceIn(0f, 1f)),
                    )
                } else {
                    PlatformTag(item.platform)
                    Spacer(Modifier.width(8.dp))
                    Text(kindLabel(item, graph.platforms), style = MaterialTheme.typography.titleSmall)
                    Spacer(Modifier.weight(1f))
                }
                IconButton(onClick = {
                    if (favorite) {
                        graph.library.removeFavorite(item.key)
                        host?.showToast("已取消收藏")
                    } else {
                        graph.library.addFavorite(effective)
                        host?.showToast("已收藏")
                    }
                }) {
                    if (favorite) {
                        Icon(Icons.Filled.Star, contentDescription = "取消收藏", tint = MaterialTheme.colorScheme.primary)
                    } else {
                        Icon(Icons.Outlined.StarBorder, contentDescription = "收藏")
                    }
                }
                IconButton(enabled = url != null, onClick = share) { Icon(Icons.Outlined.Share, contentDescription = "分享") }
                if (menuActions.isEmpty()) {
                    // 宽屏桌面：屏蔽作者还是常驻图标，「⋯」不出现（桌面行为不变）
                    if (author != null) IconButton(onClick = block) { Icon(Icons.Outlined.Block, contentDescription = "屏蔽作者") }
                } else {
                    var menu by remember { mutableStateOf(false) }
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, contentDescription = "更多") }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            for (action in menuActions) {
                                when (action) {
                                    DetailMenuAction.OPEN_EXTERNAL -> DropdownMenuItem(
                                        text = { Text(openExternal?.label.orEmpty()) },
                                        leadingIcon = { Icon(Icons.AutoMirrored.Outlined.OpenInNew, contentDescription = null) },
                                        onClick = {
                                            menu = false
                                            val open = openExternal ?: return@DropdownMenuItem
                                            val result = performExternalOpen(graph.systemActions, open) { text -> host?.copy(text, "已复制链接") }
                                            if (result == ExternalOpenResult.FAILED) host?.showToast("这条内容没有可打开的链接")
                                        },
                                    )
                                    DetailMenuAction.COPY_LINK -> DropdownMenuItem(
                                        text = { Text("复制链接") },
                                        leadingIcon = { Icon(Icons.Outlined.ContentCopy, contentDescription = null) },
                                        onClick = { menu = false; copyLink() },
                                    )
                                    DetailMenuAction.BLOCK_AUTHOR -> DropdownMenuItem(
                                        text = { Text("屏蔽作者") },
                                        leadingIcon = { Icon(Icons.Outlined.Block, contentDescription = null) },
                                        onClick = { menu = false; block() },
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

/** 详情顶栏「⋯」菜单里的一项（顺序 = [detailMenuActions] 里的顺序）。 */
enum class DetailMenuAction {
    /** 在官方 App / 浏览器打开这条内容（安卓；地址见 [externalOpenOfItem]）。 */
    OPEN_EXTERNAL,

    /** 复制链接（只有窄屏收进「⋯」；宽屏桌面保持原样，连"⋯"都没有）。 */
    COPY_LINK,

    /** 屏蔽作者（窄屏才收进「⋯」，宽屏是常驻图标）。 */
    BLOCK_AUTHOR,
}

/**
 * 「⋯」里放哪几项。**纯函数**，好让"窄屏 / 宽屏、安卓 / 桌面"这几种组合被离线测试盯住：
 * - 宽屏桌面：空表（屏蔽是常驻图标，桌面行为不变）；
 * - 宽屏安卓：只有"在 xx 打开"；
 * - 窄屏：再加屏蔽作者。
 */
internal fun detailMenuActions(
    narrow: Boolean,
    systemUi: Boolean,
    hasExternal: Boolean,
    hasUrl: Boolean,
    hasAuthor: Boolean,
): List<DetailMenuAction> = buildList {
    if (systemUi && hasExternal) add(DetailMenuAction.OPEN_EXTERNAL)
    // 窄屏：顶栏只留 返回 / 标题 / 收藏 / 分享 / ⋯，其余全进菜单
    if (narrow) {
        if (hasUrl) add(DetailMenuAction.COPY_LINK)
        if (hasAuthor) add(DetailMenuAction.BLOCK_AUTHOR)
    }
}

/** 分享文字里的标题：有标题用标题，没有（短动态）用正文开头一行。 */
internal fun shareTitle(item: FeedItem): String? =
    item.title?.takeIf { it.isNotBlank() }
        ?: item.summary?.lineSequence()?.firstOrNull { it.isNotBlank() }?.trim()?.let { if (it.length > 60) it.take(60) + "…" else it }

/**
 * 窄屏顶栏滚下去淡入的那一行：**平台名**（插件描述里的显示名，`贴吧` / `B站` 这种）。
 *
 * 显示平台名就好"。
 * 所有平台、图文详情和视频详情都用同一个写法，宽屏照旧显示平台标签 + 内容类型（那边不动）。
 */
internal fun topBarPlatformName(item: FeedItem, platforms: PlatformCatalog = InstalledPlugins.catalog): String =
    platforms[item.platform]?.label ?: item.platform.label

/** 顶栏上的内容类型文字。动态类按插件描述的 `texts.momentLabel`（知乎叫"想法"）。 */
internal fun kindLabel(item: FeedItem, platforms: PlatformCatalog = InstalledPlugins.catalog): String = when (item.kind) {
    ItemKind.VIDEO -> "视频"
    ItemKind.ARTICLE -> "文章"
    ItemKind.ANSWER -> "回答"
    ItemKind.QUESTION -> "提问"
    ItemKind.POST -> "帖子"
    ItemKind.MOMENT -> platforms[item.platform]?.texts?.momentLabel ?: "动态"
    ItemKind.LIVE -> "直播"
    ItemKind.OTHER -> "详情"
}

/**
 * 收藏用的条目：[this] 是打开时的条目（key 以它为准，历史、收藏、屏蔽都是它），其余字段用详情里更全的 [loaded]（没取到就是 null，用 [this]）。
 * 卡片表情（[FeedItem.emotes]）：详情里的条目没有就用信息流里的，收藏夹里的卡片也要显示表情图；只留文字里真有的。
 */
internal fun FeedItem.mergedWithDetail(loaded: FeedItem?): FeedItem =
    loaded?.copy(id = id, detail = detail, emotes = Emotes.used(emotes + loaded.emotes, loaded.title, loaded.summary)) ?: this
