package feedhub.app.feed

import feedhub.app.LocalPlatforms
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.PlayCircleOutline
import androidx.compose.material.icons.outlined.Subtitles
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import kotlinx.coroutines.delay
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import feedhub.app.detail.fittedImageBox
import kotlin.math.roundToInt
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import feedhub.app.translate.LocalTranslation
import feedhub.app.translate.TranslationState
import feedhub.app.translate.TranslationStore
import feedhub.app.ui.EmoteText
import feedhub.app.ui.brandColor
import feedhub.app.ui.compactCount
import feedhub.app.ui.duration
import feedhub.app.ui.relativeTime
import feedhub.brain.AdultFilter
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Platform
import feedhub.translate.RustTranslate

private val CardShape = RoundedCornerShape(12.dp)

/**
 * 信息流卡片：视频学 BiliPai（大封面、时长、播放 / 弹幕数），图文学 TiebaLite（标题、摘要、首图、吧名）。
 *
 * **长按 = 底部菜单**：收藏 / 取消收藏、分享、复制链接、
 * 在官方 App 打开、屏蔽作者。触屏上是长按（带一次触感反馈），桌面按住不动也一样（桌面上本来没有长按行为，
 * 拿不到 graph 时（例如纯预览）长按不弹。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun FeedCard(item: FeedItem, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val graph = LocalCardGraph.current
    val haptics = LocalHapticFeedback.current
    var menuOpen by remember(item.key) { mutableStateOf(false) }
    // 菜单关了以后在卡片上飘一句"已复制链接"这种提示（卡片在列表里，拿不到详情页那一层提示）
    var toast by remember(item.key) { mutableStateOf<String?>(null) }
    LaunchedEffect(toast) {
        if (toast != null) {
            delay(2000)
            toast = null
        }
    }
    // 卡片上这张图请求的是哪个地址，记一笔（[CardImageHints]）：点进详情时先用它垫着，原图到了再无感替换
    // 。只记下来，不额外发请求。
    val cardWidthPx = LocalCardWidthPx.current
    val cardHint = remember(item, cardWidthPx) { cardImageHint(item, cardWidthPx) }
    remember(item.key, cardHint) {
        cardHint?.let { CardImageHints.record(item.key, it) }
        true
    }
    // 九宫格 / 多图卡片第 2～9 张：卡片**真的画到屏幕上**时顺手把它们的卡片尺寸缩略图取了
    // 先记进 [CardImageHints] 让详情页认得出，再交给 Coil；后台不取、滚出屏幕就取消（见 GridThumbnails.kt）。
    val windowWidth = windowWidthPx()
    val density = LocalDensity.current.density
    val gridTargets = remember(item, windowWidth, density) {
        // 记一笔"这张卡打算取哪几张"（只给截图工具 `print:gridthumbs` 看）
        gridThumbnailTargets(item, windowWidth, density).also { GridPrefetchTrace.planned(it) }
    }
    val gridHints = remember(gridTargets) { gridTargets.map { it.hint } }
    remember(item.key, gridHints) {
        if (gridHints.isNotEmpty()) CardImageHints.recordGrid(item.key, gridHints)
        true
    }
    if (graph != null) GridThumbnailPrefetch(item.key, gridTargets, graph.foreground)
    Box(modifier) {
        Surface(
            shape = CardShape,
            color = MaterialTheme.colorScheme.surfaceContainerLowest,
            modifier = Modifier
                .clip(CardShape)
                .combinedClickable(
                onClick = onClick,
                onLongClick = if (graph == null) {
                    null
                } else {
                    {
                        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                        menuOpen = true
                    }
                },
            ),
        ) {
            if (item.kind == ItemKind.VIDEO || item.kind == ItemKind.LIVE) VideoCard(item) else PostCard(item)
        }
        toast?.let {
            Surface(
                shape = RoundedCornerShape(50),
                color = MaterialTheme.colorScheme.inverseSurface,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp),
            ) {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.inverseOnSurface,
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 1,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                )
            }
        }
        if (menuOpen && graph != null) {
            CardMenuSheet(
                item = item,
                graph = graph,
                onDismiss = { menuOpen = false },
                onMessage = { toast = it },
            )
        }
    }
}

@Composable
private fun VideoCard(item: FeedItem) {
    val video = item.media.firstOrNull { it is Media.Video } as Media.Video?
    val cover = video?.cover ?: (item.media.firstOrNull { it is Media.Image } as Media.Image?)?.url
    val text = item.title ?: item.summary.orEmpty()
    // **卡片照旧自动翻**。
    // 判定（要不要翻 / 现在画原文还是译文）在 Rust，这里只画。
    val translation = rememberCardTranslation(item, text, null)
    Column {
        Box(Modifier.fillMaxWidth().aspectRatio(videoCardRatio(video)).background(MaterialTheme.colorScheme.surfaceVariant)) {
            if (cover != null) {
                AsyncImage(
                    model = cardImage(cover),
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize(),
                )
            }
            Row(
                Modifier.align(Alignment.BottomStart).fillMaxWidth()
                    .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.6f))))
                    .padding(horizontal = 8.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                item.stats.views?.let { OverlayStat(Icons.Outlined.PlayCircleOutline, cardCount(it)) }
                item.stats.danmaku?.let { OverlayStat(Icons.Outlined.Subtitles, cardCount(it)) }
                // 没有播放数的平台（小红书视频笔记只有点赞）：封面角上放赞
                if (item.stats.views == null) item.stats.likes?.let { OverlayStat(Icons.Outlined.ThumbUp, cardCount(it)) }
                Spacer(Modifier.weight(1f))
                if (item.kind == ItemKind.LIVE) {
                    Text("直播", color = Color.White, fontSize = 11.sp)
                } else {
                    video?.durationSec?.let { Text(cardDuration(it), color = Color.White, fontSize = 11.sp) }
                }
            }
            if (translation.anyTranslated) {
                CardTranslationPill(
                    translation.showOriginal,
                    translation.onToggle,
                    Modifier.align(Alignment.TopEnd).padding(6.dp),
                )
            }
        }
        Column(Modifier.padding(horizontal = 8.dp, vertical = 7.dp)) {
            CardText(item, text, translation, MaterialTheme.typography.bodyMedium, FontWeight.Medium, 2, 2)
            Spacer(Modifier.height(5.dp))
            MetaRow(item)
        }
    }
}

/**
 * 卡片上的一段文字（标题 / 摘要）：现在该画译文就画译文（译文是模型给的中文，里面没有表情代码，
 * 所以走纯 [Text]），否则画原文 + 表情（[EmoteText]）。
 *
 * 哪一份由 [CardTranslation.display] 给（判断在 Rust 的 `showing_translation` / `display`）。
 */
@Composable
private fun CardText(
    item: FeedItem,
    text: String,
    translation: CardTranslation,
    style: androidx.compose.ui.text.TextStyle,
    fontWeight: FontWeight? = null,
    maxLines: Int = Int.MAX_VALUE,
    minLines: Int = 1,
    color: Color = Color.Unspecified,
) {
    val shown = translation.display(text)
    if (shown != text) {
        Text(
            shown,
            style = style,
            fontWeight = fontWeight,
            color = color,
            maxLines = maxLines,
            minLines = minLines,
            overflow = TextOverflow.Ellipsis,
        )
    } else {
        EmoteText(
            text,
            item.emotes,
            style = style,
            fontWeight = fontWeight,
            color = color,
            maxLines = maxLines,
            minLines = minLines,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * 视频卡片封面的宽高比：平台给了封面尺寸（小红书）就按它画，竖屏视频是竖的，限制在 3:4 ~ 16:10 之间；
 * 没给（B站、知乎……）一律 16:10。
 */
internal fun videoCardRatio(video: Media.Video?): Float {
    val w = video?.width
    val h = video?.height
    return if (w != null && h != null && w > 0 && h > 0) (w.toFloat() / h).coerceIn(0.75f, 1.6f) else 1.6f
}

@Composable
private fun OverlayStat(icon: ImageVector, text: String) {
    Icon(icon, contentDescription = null, tint = Color.White, modifier = Modifier.size(13.dp))
    Spacer(Modifier.width(2.dp))
    Text(text, color = Color.White, fontSize = 11.sp)
    Spacer(Modifier.width(8.dp))
}

@Composable
private fun PostCard(item: FeedItem) {
    // **卡片照旧自动翻**：标题和摘要各翻各的，
    // 有译文就在作者那一行右边摆原来那个「原文 / 译文」小胶囊。判定在 Rust，这里只画。
    val summary = item.summary?.takeIf { it.isNotBlank() && it != item.title }
    val translation = rememberCardTranslation(item, item.title, summary)
    val images = postImages(item)
    Column(Modifier.padding(PostCardPadding)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            // Reddit 照官方首页的样子"版块在前"：头像位置画版块图标、名字写 r/版块，作者挪到下面 MetaRow
            // 。别的平台照旧是作者头像 + 作者名
            val communityFirst = communityFirst(item)
            // 没有作者就不画空头像圈，那一行只剩平台标签
            if (communityFirst || item.author != null) {
                Avatar(if (communityFirst) item.communityIcon else item.author?.avatar, 20)
                Spacer(Modifier.width(6.dp))
            }
            Text(
                (if (communityFirst) item.community else item.author?.name) ?: "",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            PlatformTag(item.platform)
            if (translation.anyTranslated) {
                CardTranslationPill(
                    translation.showOriginal,
                    translation.onToggle,
                    Modifier.padding(start = 4.dp),
                )
            }
        }
        Spacer(Modifier.height(6.dp))
        item.title?.let {
            CardText(item, it, translation, MaterialTheme.typography.titleSmall, maxLines = 2)
            Spacer(Modifier.height(3.dp))
        }
        summary?.let {
            val color = if (item.title != null) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface
            CardText(
                item,
                it,
                translation,
                MaterialTheme.typography.bodySmall,
                color = color,
                maxLines = if (item.title != null) 3 else 5,
            )
        }
        images.firstOrNull()?.let { (url, w, h) ->
            Spacer(Modifier.height(8.dp))
            // 卡片上的单图也按它自己的比例**完整显示**：以前比例被夹进 0.75~1.78 再 Crop，长图截下半截、宽图截左右，而且卡片上没有「长图」角标。
            // 现在用和详情正文同一个 [fittedImageBox]：宽高先按平台给的宽高 / 卡片缩略图占好，图完整画进去，
            // 太高的封顶 [CardMaxImageHeight] 并按比例缩窄（点开详情看全）。多图（九宫格）的卡片照旧只画第一张 + "N图"角标。
            BoxWithConstraints(Modifier.fillMaxWidth()) {
                val ratio = postImageRatio(w, h)
                val container = maxWidth.value
                val box = fittedImageBox(container, ratio, CardMaxImageHeight.value)
                // 封顶高度时宽度按比例缩窄、居中摆；其余情况宽度撑满（等比例的框里 Fit 就是正好铺满）
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                    Box(Modifier.width(box.widthFor(container, ratio).dp).height(box.height.dp)) {
                        AsyncImage(
                            model = cardImage(url),
                            contentDescription = null,
                            contentScale = ContentScale.Fit,
                            modifier = Modifier.fillMaxSize().clip(RoundedCornerShape(8.dp))
                                .background(MaterialTheme.colorScheme.surfaceVariant),
                        )
                        if (images.size > 1) {
                            Text(
                                "${images.size}图",
                                color = Color.White,
                                fontSize = 11.sp,
                                modifier = Modifier.align(Alignment.BottomEnd).padding(6.dp)
                                    .background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(4.dp))
                                    .padding(horizontal = 5.dp, vertical = 1.dp),
                            )
                        }
                    }
                }
            }
        }
        // 转发的原内容自己有 adult 标记：被标成 18+ 的连"@某某：标题"这一行也不显示
        AdultFilter.visibleRepost(item)?.let { orig ->
            Spacer(Modifier.height(8.dp))
            EmoteText(
                listOfNotNull(orig.author?.name?.let { "@$it：" }, orig.title ?: orig.summary).joinToString(""),
                orig.emotes, // 转发的原内容有自己的表情表
                style = MaterialTheme.typography.bodySmall,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainer, RoundedCornerShape(8.dp))
                    .padding(8.dp),
            )
        }
        Spacer(Modifier.height(8.dp))
        MetaRow(item, showPlatform = false)
    }
}

/** "版块在前"的卡片（插件描述的 `texts.communityFirst`，Reddit）：上面一行是版块图标 + r/版块，下面一行是作者。 */
@Composable
private fun communityFirst(item: FeedItem): Boolean =
    item.community != null && LocalPlatforms.current[item.platform]?.texts?.communityFirst == true

/** 底下一行：（平台）· 吧名 / 作者 · 时间 · 赞 · 评论。 */
@Composable
private fun MetaRow(item: FeedItem, showPlatform: Boolean = true) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        if (showPlatform) PlatformTag(item.platform)
        val left = if (showPlatform || communityFirst(item)) item.author?.name else item.community
        Text(
            listOfNotNull(left, cardTime(item)).joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = muted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (!showPlatform) {
            item.stats.likes?.takeIf { it > 0 }?.let { SmallStat(Icons.Outlined.ThumbUp, it) }
            item.stats.comments?.takeIf { it > 0 }?.let { SmallStat(Icons.Outlined.ChatBubbleOutline, it) }
        }
    }
}

@Composable
private fun SmallStat(icon: ImageVector, n: Long) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null, tint = muted, modifier = Modifier.size(12.dp))
        Spacer(Modifier.width(2.dp))
        Text(cardCount(n), style = MaterialTheme.typography.labelSmall, color = muted)
    }
}

/**
 * 卡片上的三样文案：**一屏一次算好的那份**（[LocalCardTexts]）优先，没有就过单条入口。
 *
 * 为什么这样写：这三样在卡片里一处都不止一个（元信息行 / 角标 / 点赞），逐条过 FFI 一屏就是几百次；
 * 列表那边用 [ProvideCardTexts] 一屏算一次，这里只剩查表。
 */
@Composable
private fun cardTime(item: FeedItem): String? =
    LocalCardTexts.current?.time(item) ?: item.publishedAt?.let { relativeTime(it, item.publishedAtPrecision) }

@Composable
private fun cardCount(n: Long): String = LocalCardTexts.current?.count(n) ?: compactCount(n)

@Composable
private fun cardDuration(sec: Int): String = LocalCardTexts.current?.duration(sec) ?: duration(sec)

@Composable
fun PlatformTag(p: Platform) {
    val c = p.brandColor()
    Text(
        p.label,
        color = c,
        fontSize = 10.sp,
        fontWeight = FontWeight.Medium,
        modifier = Modifier.background(c.copy(alpha = 0.14f), RoundedCornerShape(4.dp)).padding(horizontal = 4.dp, vertical = 1.dp),
    )
}

@Composable
fun Avatar(url: String?, sizeDp: Int) {
    val m = Modifier.size(sizeDp.dp).clip(CircleShape).background(MaterialTheme.colorScheme.surfaceVariant)
    val px = with(LocalDensity.current) { sizeDp.dp.roundToPx() }
    if (url == null) Box(m) else AsyncImage(model = thumbnail(null, url, px), contentDescription = null, contentScale = ContentScale.Crop, modifier = m)
}

/**
 * 卡片上的「原文 / 译文」小胶囊。
 *
 * 显示「原文」= 当前看的是译文，按了切回原文；显示「译文」= 当前看的是原文，按了切回译文。
 */
@Composable
private fun CardTranslationPill(showOriginal: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Text(
        if (showOriginal) "译文" else "原文",
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = modifier
            .clip(RoundedCornerShape(50))
            .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.92f))
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 3.dp),
    )
}

/**
 * 一张卡片的翻译状态（[rememberCardTranslation] 算出来的）。
 *
 * **判断全在 Rust**（`should_auto_translate` 要不要翻、`display` 现在画哪一份）；这里只是持有一份
 * "原文 → 现在该画的文字"的查表 + "用户点了显示原文"那一格界面状态。
 */
internal class CardTranslation(
    /** 至少有一段真翻好了（这才是胶囊出现的条件，和 4c7ac4e8 时一样）。 */
    val anyTranslated: Boolean,
    /** true = 用户点了「原文」，整张卡片显示原文。 */
    val showOriginal: Boolean,
    /** 点胶囊：原文 ⇄ 译文（**不重新翻**，译文早就在 [TranslationStore] 的缓存里）。 */
    val onToggle: () -> Unit,
    /** 原文 → 现在该画的那一份（没译文 / 点了显示原文的不在这里面）。 */
    private val displayed: Map<String, String>,
) {
    /** 这段原文现在该画哪一份。 */
    fun display(original: String): String = displayed[original] ?: original
}

/**
 * 组合里算一张卡片（首页 / 关注 / 收藏 / 历史 / 搜索都用同一个 [FeedCard]）的翻译。
 *
 * **和 4c7ac4e8 时的行为一模一样**：只自动翻**点亮的平台**（`shouldAutoTranslate` 先看平台、再判语言），
 * 判定单位是整条（标题 + 摘要一起交给语言识别）。桌面上点亮的平台就是境外那四家，所以首页里外语卡片
 * 会自己变成译文；**手机上「自动翻译的平台」默认全灭**，所以手机卡片默认自然不翻——用户自己在设置里
 * 点亮了某家之后，手机卡片也照这条逻辑翻（没有另写手机特例）。
 *
 * 翻译本身仍然走 `LaunchedEffect` + `store.ensure`（缓存命中时立刻是译文；离开组合就取消排队）。
 */
@Composable
private fun rememberCardTranslation(item: FeedItem, title: String?, summary: String?): CardTranslation {
    val store = LocalTranslation.current
    if (store == null) return NoCardTranslation
    // `sources` 要 remember 住：每次重组新建一个 list 会让 `LaunchedEffect` 的 key 每帧都变，
    // 自动翻那条路上会一直重新排队。
    val sources = remember(item.key, title, summary) { cardTranslateSources(title, summary) }
    // 整条判定：标题 + 摘要一起交给语言识别（和 X 的 lang 元数据），不逐块猜。
    val autoTranslate = remember(item.key, item.language, item.platform, store.enabled, store.target) {
        cardAutoTranslate(store, item, sources)
    }
    var localShowOriginal by remember(item.key) { mutableStateOf(false) }
    // 截图工具 `play=cardtranslate` 用（正常界面永远是 null）
    val showOriginal = CardTranslationDebug.showOriginalOverride ?: localShowOriginal
    // 只自动翻点亮的平台：没点亮的一个请求都不发（判定在 Rust，见 shouldAutoTranslate）
    LaunchedEffect(sources, autoTranslate, store.enabled, store.target) {
        if (autoTranslate) sources.forEach { store.ensure(it) }
    }
    val translated = if (autoTranslate) cardTranslated(store, sources) else emptyMap()
    val anyTranslated = translated.isNotEmpty()
    // 现在画哪一份由 Rust 决定（`display`：点了显示原文 / 没译文都回原文）；每帧只查表、不再过 FFI
    val displayed = remember(translated, showOriginal) { cardDisplayed(translated, showOriginal) }
    SideEffect { CardTranslationDebug.record(item.key, anyTranslated, showOriginal) }
    return CardTranslation(
        anyTranslated = anyTranslated,
        showOriginal = showOriginal,
        onToggle = {
            localShowOriginal = !localShowOriginal
            CardTranslationDebug.showOriginalOverride?.let { CardTranslationDebug.showOriginalOverride = !it }
        },
        displayed = displayed,
    )
}

/**
 * 一张卡片要翻哪几段文字：标题、摘要（空 / 和标题重复的不算）。
 *
 * 和 4c7ac4e8 时一样**分开翻**（一个标题一个请求），但**整条判定**：两段拼起来交给语言识别。
 */
internal fun cardTranslateSources(title: String?, summary: String?): List<String> =
    listOfNotNull(title, summary?.takeIf { it.isNotBlank() && it != title })

/** 这张卡片要不要**自动**翻：整条（标题 + 摘要）判定，只翻点亮的平台（规则在 Rust 的 `should_auto_translate`）。 */
internal fun cardAutoTranslate(store: TranslationStore, item: FeedItem, sources: List<String>): Boolean =
    store.shouldAutoTranslate(sources.joinToString("\n"), item.language, item.platform)

/** 这几段文字里已经翻好的（原文 → 译文）。 */
internal fun cardTranslated(store: TranslationStore, sources: List<String>): Map<String, String> =
    sources.mapNotNull { text -> (store.state(text) as? TranslationState.Done)?.let { text to it.text } }.toMap()

/**
 * 现在每段原文该画哪一份（判断在 Rust 的 `translate::entry::display`：点了「原文」、没译文、译文和原文一样，
 * 三种情况都回原文）。每段只算一次，界面每帧只查表。
 */
internal fun cardDisplayed(translated: Map<String, String>, showOriginal: Boolean): Map<String, String> =
    translated.keys.associateWith { RustTranslate.display(it, translated[it], showOriginal) }

/** 没有本地翻译的组装（安卓早期 / 测试 / 截图）：卡片上什么都不翻，也不摆胶囊。 */
private val NoCardTranslation = CardTranslation(
    anyTranslated = false,
    showOriginal = false,
    onToggle = {},
    displayed = emptyMap(),
)

/**
 * 截图工具用的钩子（正常界面不读）：
 * - [showOriginalOverride] 非 null 时，**所有卡片**都按它显示原文 / 译文——截图点不准那个小胶囊
 *   （它贴在标题行最右边，位置随作者名长短变），所以和 `DetailDebug` 那套一样直接改状态；
 * - [toggle] 就是截图工具里"点一下那个胶囊"（`play=cardtranslate`）；
 * - [record] / [dump] 让 `print:cards` 把"这一帧哪些卡片翻好了、现在画的是哪一份"打出来。
 */
object CardTranslationDebug {
    /**
     * 非 null 时所有卡片按它显示原文 / 译文。
     *
     * 用 Compose 的 state（不是普通 `@Volatile var`）：截图工具改完这一格要**触发重组**才看得到新的一帧。
     */
    var showOriginalOverride: Boolean? by mutableStateOf(null)

    private val states = java.util.concurrent.ConcurrentHashMap<String, String>()

    /** 切一下显示原文 / 译文（截图工具用；第一次调用时把当前状态当作"显示译文"）。 */
    fun toggle() {
        showOriginalOverride = showOriginalOverride != true
    }

    internal fun record(key: String, translated: Boolean, showOriginal: Boolean) {
        if (!translated) {
            states.remove(key)
        } else {
            states[key] = if (showOriginal) "原文" else "译文"
        }
    }

    /** 这一帧卡片翻译的样子（`key=译文/原文`，按 key 排序）；`print:cards` 用。 */
    fun dump(): String =
        states.entries.sortedBy { it.key }.joinToString(" ") { "${it.key}=${it.value}" }.ifEmpty { "没有翻好的卡片" }
}

/**
 * 卡片上的图用缩略图：
 * 按卡片的实际像素宽度（[LocalCardWidthPx]，信息流网格量好给下来）向能缩放的 CDN 要一张不比卡片窄的图，规则见 [CardThumbnails]。
 */
@Composable
private fun cardImage(url: String): String {
    val widthPx = LocalCardWidthPx.current.takeIf { it > 0 } ?: with(LocalDensity.current) { FallbackCardWidth.roundToPx() }
    return remember(url, widthPx) { cardImageUrl(url, widthPx) }
}

/** [cardImage] 的纯函数版（预取用同一个地址，内存缓存才对得上）。 */
internal fun cardImageUrl(url: String, cardWidthPx: Int): String = CardThumbnails.of(url, CardThumbnails.bucket(cardWidthPx))

/** 卡片的像素宽度（信息流网格按列宽算好给下来）；0 = 不知道（收藏 / 历史等别处的卡片），按 [FallbackCardWidth] 算。 */
val LocalCardWidthPx = staticCompositionLocalOf { 0 }

private val FallbackCardWidth = 360.dp

/**
 * 卡片上那张图：地址（原始的，还没过 [cardImage]）、宽高比、是不是缩在内边距里（图文卡片的图在 10dp 内边距里，视频卡片的封面占满卡片宽）。
 * 和 [VideoCard] / [PostCard] 的取法一致，预加载（[CardImagePrefetch]）按它算出和卡片一样的请求尺寸。没有图就 null。
 */
internal data class CardImageSpec(val url: String, val ratio: Float, val inset: Boolean)

internal fun cardImageSpec(item: FeedItem): CardImageSpec? {
    if (item.kind == ItemKind.VIDEO || item.kind == ItemKind.LIVE) {
        val video = item.media.firstOrNull { it is Media.Video } as Media.Video?
        val cover = video?.cover ?: (item.media.firstOrNull { it is Media.Image } as Media.Image?)?.url ?: return null
        return CardImageSpec(cover, videoCardRatio(video), inset = false)
    }
    val (url, w, h) = postImages(item).firstOrNull() ?: return null
    return CardImageSpec(url, postImageRatio(w, h), inset = true)
}

/**
 * 卡片上那张图**实际请求多大**（像素）：和 [PostCard] 画出来的框一模一样（[fittedImageBox]：单图完整显示、
 * 太高就封顶高度并按比例缩窄），预取（[CardImagePrefetch] / [PrefetchedImages]）用同一个算法，
 * Coil 的内存缓存才对得上（差一个像素都会再解一次）。
 *
 * @param containerWidthPx 图能占多宽（卡片宽；图文卡片要减掉左右内边距）
 * @param maxHeightPx [CardMaxImageHeight] 换成的像素（封顶高度）
 * @return (宽, 高)；容器宽 <= 0 时返回 null（不取）
 */
internal fun cardImageRequestSize(containerWidthPx: Int, ratio: Float, maxHeightPx: Float): Pair<Int, Int>? {
    if (containerWidthPx <= 0) return null
    val box = fittedImageBox(containerWidthPx.toFloat(), ratio, maxHeightPx)
    val w = box.widthFor(containerWidthPx.toFloat(), ratio)
    val h = box.height
    if (w <= 0f || h <= 0f) return null
    return w.roundToInt().coerceAtLeast(1) to h.roundToInt().coerceAtLeast(1)
}

internal fun postImages(item: FeedItem): List<Triple<String, Int?, Int?>> = item.media.mapNotNull {
    when (it) {
        is Media.Image -> Triple(it.url, it.width, it.height)
        is Media.Video -> it.cover?.let { c -> Triple(c, null, null) }
    }
}

/**
 * 卡片上单张图的宽高比（宽 / 高）。
 *
 * 比例和图对不上就裁（长图截下半截、
 * 宽图截左右）。现在图片按**自己的比例**完整显示（[fittedImageBox]），这个值只用来看"要占多高"，
 * 所以只夹一个很宽的范围（1:3 ~ 3:1），免得极端比例把卡片本身撑成一屏高。
 */
internal fun postImageRatio(w: Int?, h: Int?): Float =
    if (w != null && h != null && w > 0 && h > 0) (w.toFloat() / h).coerceIn(1f / 3f, 3f) else 4f / 3f

/** 图文卡片的内边距（[PostCard] 的 `padding(10.dp)`）。 */
internal val PostCardPadding = 10.dp

/**
 * 图文卡片上**单张图**最高多高：比正文的 640dp 矮一半。
 *
 * 卡片是瀑布流里的一格，一张 1:3 的竖图按卡片宽度排就会占满一屏；封顶之后按比例缩窄、完整显示，
 * 点进详情还是完整的（详情那边封顶 [feedhub.app.detail.MaxImageHeight]）。
 */
internal val CardMaxImageHeight = 420.dp

/**
 * 头像这类小图的缩略图地址：B站 hdslb 支持 `@{宽}w.webp`，其他平台先用原地址。卡片上的大图走 [CardThumbnails]。
 */
fun thumbnail(p: Platform?, url: String, widthPx: Int): String = when {
    "hdslb.com/" in url && '@' !in url.substringAfterLast('/') -> "$url@${widthPx}w.webp"
    else -> url
}
