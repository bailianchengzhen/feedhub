package feedhub.app.detail

import feedhub.app.LocalPlatforms
import feedhub.core.plugin.PlatformCatalog
import feedhub.core.plugin.PlatformTexts
import feedhub.plugins.InstalledPlugins
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.MonetizationOn
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import feedhub.app.AppGraph
import feedhub.app.feed.Avatar
import feedhub.app.feed.CardImageHint
import feedhub.app.feed.PlatformTag
import feedhub.app.feed.describe
import feedhub.app.translate.LocalTranslation
import feedhub.app.translate.TranslationEntryRow
import feedhub.app.translate.TranslationState
import feedhub.app.ui.EmoteText
import feedhub.app.ui.compactCount
import feedhub.brain.AdultFilter
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.FollowKind
import feedhub.core.FollowSource
import feedhub.core.FollowState
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.NativeDetailSource
import feedhub.core.Platform
import feedhub.core.Stats
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 图文详情（帖子、回答、文章、动态）：顶栏 + 一个 LazyColumn（头部、正文、互动数据、评论区），参考 TiebaLite。
 * 列表打开时先用信息流里已有的标题 / 作者画头部，详情回来后换成完整的；详情和评论各自加载、各自出错。
 *
 * 要在 [DetailPane] 里用（返回、屏蔽、收藏、大图、楼中楼都靠它提供的 [DetailHost]）。
 */
@Composable
fun PostDetailScreen(graph: AppGraph, item: FeedItem, id: String, onClose: () -> Unit, modifier: Modifier = Modifier) {
    val source = graph.sourceOrNull(item.platform) as? NativeDetailSource
    if (source == null) {
        Column(modifier.fillMaxSize()) {
            DetailTopBar(graph, item, onClose)
            // 收藏 / 历史里的老条目，平台的插件已经拆掉了：条目照样显示，详情打不开
            CenterMessage(
                if (item.platform !in graph.platforms) "${item.platform.label}的插件没装，打不开详情"
                else "${item.platform.label}还没有原生详情",
            )
        }
        return
    }
    val scope = rememberCoroutineScope()
    // 缓存键用**条目**的 key（`平台:id`）：详情 id 里带的 `@来源@token` 每次打开都会变（小红书 / 抖音），
    // 拿它当键第二次点进去就命中不了
    val cacheKey = remember(item.key) { DetailCache.itemKey(item) }
    val state = remember(source, id, cacheKey) { PostDetailState(source, id, scope, platforms = graph.platforms, cacheKey = cacheKey) }
    LaunchedEffect(state) { state.load() }
    val host = LocalDetailHost.current
    // 截图工具（holdbody=1）把这一屏按"详情还在加载"的样子画，好截"先垫缩略图"那一屏（正常永远是真实状态）
    val detail = if (DetailDebug.holdBodyLoading) null else state.detail
    // 平台在详情接口里才标的 18+（YouTube 年龄限制）：和从信息流直接打开同一条规则，只画一行、不显示正文 / 评论
    if (detail != null && detailHiddenAsAdult(detail.item)) {
        AdultHiddenDetail(onClose, modifier.fillMaxSize())
        return
    }
    // 详情正文**已经加载出来**：用"标题 + 正文开头"重算这条的向量（覆盖）。
    // 只在这一屏真的拿到正文之后调，不为了向量多发一个请求；正文只用来算向量，不存库。
    LaunchedEffect(detail) {
        val loaded = detail ?: return@LaunchedEffect
        graph.onDetailLoaded(loaded.item, loaded.body.textOnly())
    }
    val shown = detail?.item ?: item
    // 知乎问题版面：上面是问题、评论区放回答；"题主"是提问者（详情回来之前不知道是谁，不标）
    val questionView = isQuestionView(item)
    // 问题版面的详情还没回来：卡片上那条**回答**的时间 / 话题 / 摘要 / 配图都不垫。只留标题（卡片标题本来就是问题标题，前后不变），
    // 下面摆骨架；回答区也等详情回来再画（回答列表顺带等置顶那条记下来再取，少一次补取请求）。
    val pendingQuestion = questionView && detail == null
    val poster = if (questionView) {
        Poster(detail?.item?.author?.id, "题主")
    } else {
        Poster(shown.author?.id ?: item.author?.id, posterLabel(item, graph.platforms))
    }
    // **翻译**：入口在正文下面那一行（不再有顶栏按钮 / 「⋯」里那一项）。
    // 桌面上「点亮平台的自动翻」照旧：进详情就自动翻并直接显示译文；手机 / 没点亮的平台点了才翻。
    val translation = LocalTranslation.current
    val language = shown.language ?: item.language
    // 整条判定：标题 + 摘要 + 正文一起看（平台给了 language 就直接用平台的）
    val detectContext = remember(shown.title, shown.summary, item.summary, detail?.body) {
        listOfNotNull(shown.title, shown.summary, item.summary, detail?.body?.textOnly())
            .filter { it.isNotBlank() }.distinct().joinToString("\n")
    }
    val autoTranslate = remember(detectContext, language, item.platform, translation?.enabled, translation?.target) {
        translation?.enabled == true &&
            translation.shouldAutoTranslate(detectContext, language, item.platform)
    }
    // 正文那一行入口（整篇一个，学 X）：自动翻也用它显示「显示原文 / 显示译文」和失败重试。
    // **在 LazyColumn 外面算**——入口是正文的"总开关"，列表里的块要根据它决定画原文还是译文。
    // 正文还没到（`detail == null`）时没有入口，垫着的摘要就是普通文字。
    val bodyTranslation = detail?.let {
        rememberRichTextTranslation(it.body, autoTranslate, language)
    }
    // 标题单独一段文字：正文那一行入口点「翻译」/「显示原文」时，标题跟着同一份开关走
    // （手机上标题和正文一起翻，桌面上自动翻也一样）。标题本身不是外语就不翻（返回原标题）。
    val translatedTitle = shown.title?.takeIf { it.isNotBlank() }?.let { title ->
        rememberTitleTranslation(title, language, autoTranslate, bodyTranslation?.entry)
    }
    // 翻译入口的截图钩子
    SideEffect {
        DetailDebug.translateEntry = bodyTranslation?.entry?.let { entry ->
            { entry.onStart to entry.onToggle }
        }
        DetailDebug.translatePhase = {
            val e = bodyTranslation?.entry
            if (e == null) "没有翻译入口" else "入口=${e.phase} 语言=${e.language} 显示原文=${e.showOriginal} 失败=${e.failure}"
        }
    }
    SideEffect {
        DetailDebug.comments = { state.comments.page.items }
        DetailDebug.poster = poster
        DetailDebug.contentId = id
        DetailDebug.bodyImages = { detail?.body?.blocks?.filterIsInstance<feedhub.core.Block.Image>().orEmpty().map { ViewerImage(it.original ?: it.url, it.url, it.caption) } }
    }

    // 详情列表的滚动位置也喂给顶栏：窄屏时"滚下去淡入平台名"
    val listState = rememberLazyListState()
    // 时间线：这一帧详情页里现在是什么。正常界面没人读它。
    // 里面**现读** `state` 的 snapshot 状态（不能用外面那几个 local val——探针是由截图工具在别的时机调的，
    // 捕获进来的会是第一次组合时的旧值，量到的时间线就是错的）。
    SideEffect {
        DetailDebug.timelineProbe = {
            val loaded = state.detail
            val phase = when {
                loaded != null -> "正文到了（${loaded.body.blocks.size} 块）"
                state.error != null -> "正文失败"
                else -> "正文没到（只有垫着的摘要 / 卡片图）"
            }
            val title = if (loaded != null && loaded.item.title != item.title) "标题换了" else "标题=卡片那个"
            "$phase $title 评论=${state.comments.page.items.size} ${DetailDebug.imageStatus()} 首条=${listState.firstVisibleItemIndex}"
        }
    }
    val fadeDistancePx = with(LocalDensity.current) { 96.dp.toPx() }
    Column(modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
        DetailTopBar(
            graph, item, onClose, loaded = detail?.item,
            titleAlpha = {
                if (listState.firstVisibleItemIndex > 0) 1f
                else (listState.firstVisibleItemScrollOffset / fadeDistancePx).coerceIn(0f, 1f)
            },
        )
        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
            val side: Dp = maxOf(DetailSidePadding, (maxWidth - DetailContentMaxWidth) / 2)
            val bodyStyle = richBodyStyle()
            // 桌面上正文 / 评论的文字要能选中复制
            SelectionContainer {
                // 滚动 / 惯性中要高刷，停下来撤回
                LazyColumn(
                    state = listState,
                    contentPadding = PaddingValues(start = side, end = side, top = 8.dp, bottom = 32.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    item(key = "header") {
                        PostHeader(
                            if (pendingQuestion) item.copy(publishedAt = null, reason = null, community = null) else shown,
                            detail?.follow, source,
                            // 标题也跟着「显示原文 / 显示译文」走：正文那一行入口翻好了才换标题，
                            // 没翻 / 点了「显示原文」就是原标题
                            translatedTitle = translatedTitle,
                            showOriginal = bodyTranslation?.entry?.showOriginal == true,
                            // 点头像 / 名字 = 在官方 App 打开作者主页（退化：浏览器 → 复制主页链接），见 DetailHost.openAuthor
                            onAuthorClick = shown.author?.let { author -> { host?.openAuthor(author) } },
                            // 问题版面拿不到提问者（匿名 / 没给）就不画头像那一行；详情回来之前也不画卡片上的答主
                            hideMissingAuthor = questionView,
                            authorOverride = if (pendingQuestion) null else shown.author,
                            reserveMeta = questionView,
                        )
                    }
                    when {
                        detail != null -> {
                            // 正文：长文仍然按块懒加载（只翻滑到的块）。
                            // **译文就地替换**：`translated` 由下面那一行入口决定。
                            translatedRichTextItems(detail.body, "body", bodyStyle, translated = bodyTranslation?.showingTranslation == true)
                            // 正文**下方**那一行「翻译」入口。
                            // 中文内容不出现；点了才翻（手机上不自动翻），翻好就地显示译文 + 灰字那一行。
                            item(key = "translate") { TranslationEntryRow(bodyTranslation?.entry) }
                            // 转发的原内容被标成 18+ 就整块不显示
                            AdultFilter.visibleRepost(shown)?.let { orig -> item(key = "repost") { RepostBox(orig, Modifier.padding(bottom = 12.dp)) } }
                        }
                        state.error != null -> item(key = "error") {
                            // 一句话 + 「重试」，按钮单独一行摆
                            Box(Modifier.fillMaxWidth().padding(vertical = 24.dp), contentAlignment = Alignment.Center) {
                                FailureBlock(state.error.orEmpty(), onRetry = state::load)
                            }
                        }
                        pendingQuestion -> item(key = "loading") { QuestionSkeleton() }
                        else -> {
                            // 详情还在加载：先把信息流里的摘要垫上，再把**卡片上已经下好的那张图**摆出来
                            // ：不转圈，进详情就有东西看。
                            // 没有再退回
                            // 缩略图的真实比例。以前只按缩略图比例摆，正文到了图会变高 / 变矮，下面的东西跟着跳。
                            item(key = "loading") {
                                Column(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                                    item.summary?.takeIf { it.isNotBlank() && it != item.title }?.let {
                                        // 摘要垫着的那一段也带表情（和卡片摘要同一套），否则详情回来后表情才"出现"
                                        EmoteText(
                                            it,
                                            emotes = remember(it, item.emotes) { Emotes.used(item.emotes, it) },
                                            style = bodyStyle,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                            maxLines = 6,
                                            overflow = TextOverflow.Ellipsis,
                                        )
                                    }
                                    LoadingCardImage(
                                        LocalDetailThumbnails.current.cardImage,
                                        expectedImage = item.media.isNotEmpty(),
                                        declaredRatio = cardMediaRatio(item),
                                    )
                                }
                            }
                        }
                    }
                    // 问题版面：计数是问题的（浏览 / 回答数），不和卡片上那条回答的赞同 / 评论混
                    if (!questionView) {
                        item(key = "stats") { StatsRow(shown.stats.mergeWith(item.stats)) }
                    } else if (detail != null) {
                        item(key = "stats") { StatsRow(shown.stats, commentsLabel = "回答") }
                    }
                    if (!pendingQuestion) {
                        commentsSection(
                            state.comments,
                            poster,
                            total = if (questionView) detail?.item?.stats?.comments else shown.stats.comments ?: item.stats.comments,
                            onOpenReplies = { host?.openReplies(it, poster, id) },
                            answers = questionView,
                        )
                    }
                }
            }
        }
    }
}

/**
 * 评论区里"楼主"标签的文字：回答 / 文章是"作者"；视频用插件描述的 `texts.videoPosterLabel`（B站"UP主"、知乎"作者"），
 * 别的用 `texts.posterLabel`（默认"楼主"，小红书"作者"）。
 */
fun posterLabel(item: FeedItem, platforms: PlatformCatalog = InstalledPlugins.catalog): String {
    if (item.kind == ItemKind.ANSWER || item.kind == ItemKind.ARTICLE) return "作者"
    val texts = platforms[item.platform]?.texts ?: PlatformTexts()
    return if (item.kind == ItemKind.VIDEO) texts.videoPosterLabel ?: texts.posterLabel else texts.posterLabel
}

/** 详情里没给的计数用信息流里的补上。 */
private fun Stats.mergeWith(other: Stats) = Stats(
    views = views ?: other.views,
    likes = likes ?: other.likes,
    comments = comments ?: other.comments,
    shares = shares ?: other.shares,
    danmaku = danmaku ?: other.danmaku,
    favorites = favorites ?: other.favorites,
    coins = coins ?: other.coins,
)

@Composable
private fun CenterMessage(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/**
 * 详情接口还没回来时垫在最前面的那张图：**卡片上已经下好的缩略图**（[hint]），
 * 按它的真实比例把位置占好——不给原图、也不额外发请求。
 * 详情回来后这一段被真正的正文替换，正文里同一张图也用同一张缩略图垫着（[RichImage]），所以不会闪。
 *
 * 不是按缩略图自己的比例——
 * 平台在卡片 `media` 里给了宽高时（[declaredRatio]）正文图块用的就是同一个宽高比，所以垫的这块和换上去的
 * 那块**一样高**，下面的互动数据 / 评论区不会跟着跳。缩略图的比例只在平台没给宽高时才用（那时候正文也要等图
 * 到了才知道比例，两边至少是同一个起点）。
 *
 * 卡片上没下过图（[hint] 为 null）但这条内容本来有图的（[expectedImage]）：只画一块和背景接近的占位块
 * （"转圈换成占位块"，不转圈）；连图都没有的纯文字帖就什么都不画。
 */
@Composable
private fun LoadingCardImage(hint: CardImageHint?, expectedImage: Boolean, declaredRatio: Float? = null) {
    if (hint == null) {
        if (!expectedImage) return
        Box(
            Modifier.fillMaxWidth().aspectRatio(declaredRatio?.coerceIn(MinBodyImageRatio, MaxBodyImageRatio) ?: (16f / 10f))
                .clip(RoundedCornerShape(8.dp))
                .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f)),
        )
        return
    }
    // 平台给了宽高就用它（和正文图块一致）；没给才退回卡片上那张缩略图的真实比例
    val ratio = detailImageRatio(declaredRatio, hint)
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        // 和正文里那张图同一套排法：垫着的时候也是最终尺寸，
        // 详情回来换成正文图时**位置和裁法都一样**，所以不跳。
        val box = bodyImageHeight(maxWidth.value, ratio, MaxImageHeight.value)
        val width = box.widthFor(maxWidth.value, ratio).dp
        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            Box(Modifier.width(width).height(box.height.dp)) {
                DetailPhoto(
                    url = hint.thumbnail,
                    contentDescription = null,
                    contentScale = ContentScale.Fit,
                    alignment = Alignment.Center,
                    modifier = Modifier.fillMaxSize().clip(RoundedCornerShape(8.dp)),
                )
                if (box.cropped) LongImageHint(Modifier.align(Alignment.BottomCenter))
            }
        }
    }
}

/**
 * 卡片 media 里那第一张图声明的宽高比（宽 / 高）；没有 / 不合法就是 null。
 *
 * 用它是因为**正文图块按声明宽高占位**（[detailImageRatio]）：占位块要和换上去的那块一样高，
 * 才不会有"先矮后高"的跳动。
 */
internal fun cardMediaRatio(item: FeedItem): Float? {
    val (w, h) = when (val media = item.media.firstOrNull()) {
        is Media.Image -> (media.width ?: 0) to (media.height ?: 0)
        is Media.Video -> (media.width ?: 0) to (media.height ?: 0)
        else -> return null
    }
    if (w <= 0 || h <= 0) return null
    return w.toFloat() / h
}

/** 头部：作者头像 / 名字/ 时间 / 吧名或话题、关注按钮，然后是标题。 */
@Composable
private fun PostHeader(
    item: FeedItem,
    follow: FollowState?,
    source: NativeDetailSource,
    translatedTitle: String? = null,
    showOriginal: Boolean = false,
    /** 点头像 / 名字：在官方 App 打开作者主页（`DetailHost.openAuthor`）；null = 不可点。 */
    onAuthorClick: (() -> Unit)? = null,
    /** 没有作者就不画头像那一行（知乎问题版面：提问者拿不到就不显示），默认画"匿名"。 */
    hideMissingAuthor: Boolean = false,
    authorOverride: feedhub.core.UserRef? = item.author,
    /** 时间那一行没字时也占着一行高度（问题版面：加载中先空着，回来后标题不往下跳）。 */
    reserveMeta: Boolean = false,
) {
    // 提问者那一行是详情回来才有的：高度变化走动画，不是一下子把标题顶下去
    Column(
        Modifier.fillMaxWidth().padding(bottom = 12.dp).then(if (reserveMeta) Modifier.animateContentSize() else Modifier),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        val meta = listOfNotNull(item.publishedAt?.let { fullTime(it, item.publishedAtPrecision) }, item.reason).joinToString(" · ")
        if (hideMissingAuthor && authorOverride == null) {
            // 没有作者那一行：时间 / 计数这行字还是留着
            if (meta.isNotEmpty() || reserveMeta) Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        } else Row(verticalAlignment = Alignment.CenterVertically) {
            // 头像 + 名字整块可点（只是这一个人，不是整行：右边的关注按钮有自己的点击）
            Row(
                Modifier.weight(1f).then(
                    if (onAuthorClick != null) Modifier.pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onAuthorClick) else Modifier,
                ),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Avatar(authorOverride?.avatar, 40)
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        authorOverride?.name ?: "匿名",
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (meta.isNotEmpty()) Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            if (follow != null && source is FollowSource) {
                Spacer(Modifier.width(8.dp))
                FollowButton(follow, source)
            }
        }
        item.title?.takeIf { it.isNotBlank() }?.let {
            // 标题也走表情渲染：
            // 平台表情代码画成行内小图，Unicode emoji 用彩色字体，和正文 / 评论 / 卡片同一套。
            // 译文里没有表情代码，`emotes` 会过滤成空表，原样当文字画。
            val shownTitle = if (!showOriginal && translatedTitle != null) translatedTitle else it
            DetailTitleText(
                text = shownTitle,
                emotes = titleEmotes(item),
                style = MaterialTheme.typography.headlineSmall.copy(fontSize = 22.sp, lineHeight = 30.sp),
                fontWeight = FontWeight.SemiBold,
            )
        }
        item.community?.takeIf { it.isNotBlank() && it != item.title }?.let { community ->
            val label = communityLabel(community, LocalPlatforms.current[item.platform]?.texts)
            Text(
                label,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.clip(RoundedCornerShape(50)).background(MaterialTheme.colorScheme.primaryContainer)
                    .padding(horizontal = 10.dp, vertical = 3.dp),
            )
        }
    }
}

/** 问题版面的加载占位：几条灰条（问题描述 + 统计行的位置），不垫卡片上那条回答的内容。 */
@Composable
private fun QuestionSkeleton() {
    val bar = MaterialTheme.colorScheme.surfaceVariant
    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        for (fraction in listOf(1f, 0.92f, 0.6f)) {
            Box(Modifier.fillMaxWidth(fraction).height(14.dp).clip(RoundedCornerShape(4.dp)).background(bar))
        }
    }
}

/**
 * "关注吧 / 已关注吧"（B站是"关注 / 已关注"）。**写操作**：只在用户点的时候调 [FollowSource.setFollowing]，
 * 请求期间按钮禁用，失败了提示一行字，不重试。
 */
@Composable
private fun FollowButton(follow: FollowState, source: FollowSource) {
    val host = LocalDetailHost.current
    val scope = rememberCoroutineScope()
    var following by remember(follow) { mutableStateOf(follow.following == true) }
    var busy by remember(follow) { mutableStateOf(false) }
    val followLabel = if (follow.target.kind == FollowKind.FORUM) "关注吧" else "关注"
    val onClick = {
        val want = !following
        busy = true
        scope.launch {
            try {
                withContext(Dispatchers.IO) { source.setFollowing(follow.target, want) }
                following = want
                host?.showToast(if (want) "已关注 ${follow.target.name}" else "已取消关注 ${follow.target.name}")
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                host?.showToast(describe(e))
            } finally {
                busy = false
            }
        }
        Unit
    }
    if (following) {
        OutlinedButton(onClick = onClick, enabled = !busy, contentPadding = PaddingValues(horizontal = 14.dp, vertical = 0.dp), modifier = Modifier.height(32.dp)) {
            Text(if (follow.target.kind == FollowKind.FORUM) "已关注吧" else "已关注", style = MaterialTheme.typography.labelLarge)
        }
    } else {
        Button(
            onClick = onClick,
            enabled = !busy,
            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 0.dp),
            colors = ButtonDefaults.buttonColors(),
            modifier = Modifier.height(32.dp),
        ) { Text(followLabel, style = MaterialTheme.typography.labelLarge) }
    }
}

/** 转发的原内容（B站转发动态等）：一个带作者、文字和缩略图的框，点了打开原内容。 */
@Composable
private fun RepostBox(orig: FeedItem, modifier: Modifier = Modifier) {
    val host = LocalDetailHost.current
    val images = orig.media.mapNotNull {
        when (it) {
            is Media.Image -> it.url
            is Media.Video -> it.cover
        }
    }.take(3)
    Column(
        modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).background(MaterialTheme.colorScheme.surfaceContainer)
            .pointerHoverIcon(PointerIcon.Hand)
            .clickable { host?.openItem(orig) }
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Avatar(orig.author?.avatar, 20)
            Text("@${orig.author?.name ?: "原作者"}", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            PlatformTag(orig.platform)
        }
        // 转发原文的标题 / 摘要在卡片上是走 EmoteText 的（feed/Cards.kt:341），这里跟齐
        orig.title?.takeIf { it.isNotBlank() }?.let {
            DetailTitleText(it, titleEmotes(orig), MaterialTheme.typography.titleSmall, maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
        orig.summary?.takeIf { it.isNotBlank() && it != orig.title }?.let {
            EmoteText(
                it,
                emotes = remember(it, orig.emotes) { Emotes.used(orig.emotes, it) },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 6,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (images.isNotEmpty()) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (url in images) {
                    DetailPhoto(
                        url = url,
                        placeholder = LocalDetailThumbnails.current.hint(url)?.thumbnail,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.size(88.dp).clip(RoundedCornerShape(6.dp)),
                    )
                }
            }
        }
    }
}

/** 互动数据：浏览 / 赞 / 评论 / 收藏 / 分享 / 投币，没有的不显示。 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun StatsRow(stats: Stats, commentsLabel: String = "评论") {
    val entries = listOfNotNull(
        stats.views?.let { Triple(Icons.Outlined.Visibility, it, "浏览") },
        stats.likes?.let { Triple(Icons.Outlined.ThumbUp, it, "赞") },
        stats.comments?.let { Triple(Icons.Outlined.ChatBubbleOutline, it, commentsLabel) },
        stats.favorites?.let { Triple(Icons.Outlined.StarBorder, it, "收藏") },
        stats.shares?.let { Triple(Icons.Outlined.Share, it, "分享") },
        stats.coins?.let { Triple(Icons.Outlined.MonetizationOn, it, "投币") },
    )
    if (entries.isEmpty()) return
    Column(Modifier.fillMaxWidth().padding(top = 4.dp)) {
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        FlowRow(
            Modifier.fillMaxWidth().padding(vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(18.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            for ((icon, n, label) in entries) StatChip(icon, compactCount(n), label)
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

@Composable
private fun StatChip(icon: ImageVector, value: String, label: String) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = label, tint = muted, modifier = Modifier.size(16.dp))
        Spacer(Modifier.width(4.dp))
        Text(value, style = MaterialTheme.typography.labelMedium, color = muted)
    }
}

/** 圈子名加上平台的后缀（贴吧：`原神` → `原神吧`；本来就以它结尾的不再加），见插件描述的 `texts.communitySuffix`。 */
fun communityLabel(community: String, texts: PlatformTexts?): String {
    val suffix = texts?.communitySuffix.orEmpty()
    return if (suffix.isEmpty() || community.endsWith(suffix)) community else community + suffix
}
