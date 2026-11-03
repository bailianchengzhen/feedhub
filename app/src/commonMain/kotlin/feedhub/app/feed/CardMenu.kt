package feedhub.app.feed

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import feedhub.app.AppGraph
import feedhub.app.detail.ExternalOpenResult
import feedhub.app.detail.externalOpenOfItem
import feedhub.app.detail.performExternalOpen
import feedhub.app.detail.shareText
import feedhub.app.detail.shareTitle
import feedhub.core.FeedItem

/**
 * 卡片长按菜单用的 [AppGraph]。
 *
 * 为什么默认值要读 [AppGraph.current]：卡片在首页 / 关注 / 收藏 / 历史四处画，那四处的调用点当前不动
 * （分属不同批次的文件），所以没法一路把 `graph` 传进 [FeedCard]。默认值只是兜底——测试 / 截图想换一份，
 * 正常用 `CompositionLocalProvider(LocalCardGraph provides …)` 覆盖即可。
 */
val LocalCardGraph = compositionLocalOf<AppGraph?> { AppGraph.current }

/** 长按菜单里的一项（顺序 = [cardMenuItems] 里的顺序）。 */
enum class CardMenuAction {
    /** 收藏 / 取消收藏（文案跟着当前状态走）。 */
    FAVORITE,

    /** 分享：安卓弹系统分享面板，桌面复制"标题 + 链接"。 */
    SHARE,

    /** 只复制链接到剪贴板。 */
    COPY_LINK,

    /** 在官方 App / 浏览器打开这条内容（安卓才有，见 [externalOpenOfItem]）。 */
    OPEN_EXTERNAL,

    /** 屏蔽作者（以后不再出现他的内容）。 */
    BLOCK_AUTHOR,
}

/**
 * 菜单里放哪几项。**纯函数**，好让"安卓 / 桌面、有没有作者、有没有链接"这些组合被离线测试盯住：
 * - 收藏 / 分享：一直有；
 * - 复制链接：有网页地址才有；
 * - 在官方 App / 浏览器打开：安卓（有系统界面）才有——桌面这一项没有意义，点开也是复制；
 * - 屏蔽作者：有作者才有。
 */
internal fun cardMenuItems(
    systemUi: Boolean,
    hasUrl: Boolean,
    hasExternal: Boolean,
    hasAuthor: Boolean,
): List<CardMenuAction> = buildList {
    add(CardMenuAction.FAVORITE)
    add(CardMenuAction.SHARE)
    if (hasUrl) add(CardMenuAction.COPY_LINK)
    if (systemUi && hasExternal) add(CardMenuAction.OPEN_EXTERNAL)
    if (hasAuthor) add(CardMenuAction.BLOCK_AUTHOR)
}

/** 收藏那一项的文案（跟着当前状态走）。 */
internal fun favoriteLabel(favorite: Boolean): String = if (favorite) "取消收藏" else "收藏"

/**
 * 卡片的长按菜单：底部弹出的一层（点空白 / 返回键关掉）。动作和详情页顶栏 / ⋯ 对齐——
 * 收藏、分享、复制链接、在官方 App 打开、屏蔽作者，全都是本地操作或交给系统，**不发任何平台请求**
 * （收藏 / 屏蔽写本地资料库，和详情顶栏走同一个 [feedhub.app.library.Library]）。
 *
 * @param onMessage 需要在卡片上飘一句提示时调（"已复制链接" / "已收藏" 之类）。
 */
@Composable
fun CardMenuSheet(
    item: FeedItem,
    graph: AppGraph,
    onDismiss: () -> Unit,
    onMessage: (String) -> Unit,
) {
    val clipboard = LocalClipboardManager.current
    val favorite = graph.library.isFavorite(item.key)
    val url = item.url
    val author = item.author
    val actions = graph.systemActions
    val openExternal = if (actions.available) externalOpenOfItem(item, graph.platforms) else null
    val items = cardMenuItems(
        systemUi = actions.available,
        hasUrl = url != null,
        hasExternal = openExternal != null,
        hasAuthor = author != null,
    )
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(Modifier.fillMaxSize()) {
            // 点外面关掉（不透明度 32%，和楼中楼面板一样）
            Box(
                Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.32f))
                    .clickable(indication = null, interactionSource = null, onClick = onDismiss),
            )
            Surface(
                modifier = Modifier.align(Alignment.BottomCenter).fillMaxWidth(),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
                shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
                tonalElevation = 3.dp,
            ) {
                Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
                    // 抓手（触屏上"这是能往下拖走的一层"的暗示）
                    Box(
                        Modifier.align(Alignment.CenterHorizontally).padding(top = 8.dp, bottom = 6.dp)
                            .size(width = 32.dp, height = 4.dp)
                            .background(MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(50)),
                    )
                    cardMenuTitle(item)?.let {
                        Text(
                            it,
                            style = MaterialTheme.typography.titleSmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                        )
                    }
                    Column(verticalArrangement = Arrangement.Top) {
                        for (action in items) {
                            when (action) {
                                CardMenuAction.FAVORITE -> MenuRow(
                                    icon = if (favorite) Icons.Filled.Star else Icons.Outlined.StarBorder,
                                    label = favoriteLabel(favorite),
                                ) {
                                    if (favorite) {
                                        graph.library.removeFavorite(item.key)
                                        onMessage("已取消收藏")
                                    } else {
                                        graph.library.addFavorite(item)
                                        onMessage("已收藏")
                                    }
                                    onDismiss()
                                }
                                CardMenuAction.SHARE -> MenuRow(Icons.Outlined.Share, "分享") {
                                    onDismiss()
                                    if (url == null) {
                                        onMessage("这条内容没有链接可以分享")
                                    } else {
                                        val text = shareText(shareTitle(item), url)
                                        if (!actions.share(text)) onMessage("已复制标题和链接")
                                    }
                                }
                                CardMenuAction.COPY_LINK -> MenuRow(Icons.Outlined.ContentCopy, "复制链接") {
                                    onDismiss()
                                    if (url != null) {
                                        clipboard.setText(AnnotatedString(url))
                                        onMessage("已复制链接")
                                    }
                                }
                                CardMenuAction.OPEN_EXTERNAL -> MenuRow(Icons.AutoMirrored.Outlined.OpenInNew, openExternal?.label.orEmpty()) {
                                    onDismiss()
                                    val open = openExternal ?: return@MenuRow
                                    val result = performExternalOpen(actions, open) { text ->
                                        clipboard.setText(AnnotatedString(text))
                                        onMessage("已复制链接")
                                    }
                                    if (result == ExternalOpenResult.FAILED) onMessage("这条内容没有可打开的链接")
                                }
                                CardMenuAction.BLOCK_AUTHOR -> MenuRow(Icons.Outlined.Block, "屏蔽作者") {
                                    onDismiss()
                                    if (author != null) {
                                        graph.library.block(author)
                                        onMessage("不再显示 ${author.name}")
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** 菜单顶上那一行：标题（没有就用摘要的第一行），让用户确认点的是哪张卡。 */
private fun cardMenuTitle(item: FeedItem): String? =
    item.title?.takeIf { it.isNotBlank() }
        ?: item.summary?.lineSequence()?.firstOrNull { it.isNotBlank() }?.trim()?.take(60)

@Composable
private fun MenuRow(icon: ImageVector, label: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).heightIn(min = 52.dp).padding(horizontal = 20.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(22.dp))
        Spacer(Modifier.width(16.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge)
    }
}
