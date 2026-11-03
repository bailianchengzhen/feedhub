package feedhub.app.detail

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.slideInHorizontally
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import feedhub.app.feed.Avatar
import feedhub.app.translate.LocalTranslation
import feedhub.app.translate.TranslationEntryRow
import feedhub.app.translate.TranslationState
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.NativeDetailSource
import feedhub.core.RichText
import feedhub.present.RustPresentPaging
import feedhub.translate.RustTranslate
import uniffi.feedhub_ffi.FfiPresentCommentDisplayItem
import uniffi.feedhub_ffi.FfiPresentPreviewReplyDisplayItem

/**
 * 评论区：标题 + 排序切换（默认 / 热门 / 最新）+ 评论列表 + 底部的"滑到底加载下一页 / 出错重试 / 到底了"。
 * 摊在 LazyColumn 里（图文详情、视频详情共用）。点"共 N 条回复"走 [onOpenReplies]，一般是
 * `{ host.openReplies(it, poster, contentId) }`（[DetailHost] 画楼中楼面板）。
 *
 * @param total 评论总数（标题上显示），不知道就 null。
 */
fun LazyListScope.commentsSection(
    state: CommentsState,
    poster: Poster,
    total: Long?,
    onOpenReplies: (Comment) -> Unit,
    displayBatch: Map<String, FfiPresentCommentDisplayItem>? = null,
    /** 知乎问题版面：评论区放的是回答，标题写"回答 N"（[answersHeaderTitle]）。 */
    answers: Boolean = false,
) {
    item(key = "comments-header") { CommentsHeader(total, state.sort, state::select, state.sortable, answers) }
    commentsList(state, poster, onOpenReplies, displayBatch)
}

/**
 * [commentsSection] 去掉标题的部分：评论列表 + 底部的分页提示。标题要自己摆的时候用（视频详情把标题和排序放进标签栏里）。
 * 底部提示出现在屏幕上就加载下一页，所以只在评论真的显示着的时候才调它。
 */
fun LazyListScope.commentsList(
    state: CommentsState,
    poster: Poster,
    onOpenReplies: (Comment) -> Unit,
    displayBatch: Map<String, FfiPresentCommentDisplayItem>? = null,
) {
    val page = state.page
    items(page.items, key = { "comment-${it.id}" }) { comment ->
        val batch = displayBatch ?: state.displayBatch(poster.id, LocalDetailPlatform.current?.label)
        // 知乎回答楼层：滑到屏幕上才取这条回答的几条评论当预览
        val floor = isAnswerFloor(comment)
        if (floor) LaunchedEffect(comment.id) { state.loadAnswerPreview(comment) }
        Column {
            CommentItem(
                comment = comment,
                poster = poster,
                onOpenReplies = onOpenReplies,
                repliesSupported = state.repliesSupported,
                displayItem = batch[comment.id],
                lazyPreviews = if (floor) state.answerPreviews[comment.id] else null,
            )
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        }
    }
    item(key = "comments-footer") { PagedFooter(page, emptyText = "还没有评论", endText = "没有更多评论了") }
}

@Composable
private fun CommentsHeader(total: Long?, sort: CommentSort, onSort: (CommentSort) -> Unit, sortable: Boolean = true, answers: Boolean = false) {
    Row(Modifier.fillMaxWidth().padding(top = 16.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        val title = RustPresentPaging.commentsHeaderTitle(total)
        Text(
            if (answers) answersHeaderTitle(title) else title,
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.weight(1f),
        )
        if (sortable) CommentSortChips(sort, onSort)
    }
}

/** 评论排序切换：默认 / 热门 / 最新三个胶囊，选中的那个高亮；[compact] 留白小一点（手机竖屏，要和别的东西挤一行）。 */
@Composable
fun CommentSortChips(sort: CommentSort, onSort: (CommentSort) -> Unit, compact: Boolean = false) {
    for ((value, label) in listOf(CommentSort.DEFAULT to "默认", CommentSort.HOT to "热门", CommentSort.NEWEST to "最新")) {
        val selected = sort == value
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            color = if (selected) MaterialTheme.colorScheme.onPrimaryContainer else MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(start = if (compact) 2.dp else 6.dp).clip(RoundedCornerShape(50))
                .background(if (selected) MaterialTheme.colorScheme.primaryContainer else Color.Transparent)
                .pointerHoverIcon(PointerIcon.Hand)
                .clickable { onSort(value) }
                .padding(horizontal = if (compact) 8.dp else 12.dp, vertical = 5.dp),
        )
    }
}

/**
 * 分页列表的最后一行：加载中转圈、出错一行字 + 重试、没有内容 / 到底了的提示。
 * 它出现在屏幕上（滑到底）就自动加载下一页；出错之后不自动重试，等点"重试"。
 */
@Composable
fun PagedFooter(page: PagedState<*>, emptyText: String, endText: String, modifier: Modifier = Modifier) {
    LaunchedEffect(page, page.items.size, page.error) {
        if (page.error == null) page.loadMore()
    }
    Box(modifier.fillMaxWidth().padding(vertical = 16.dp), contentAlignment = Alignment.Center) {
        val error = page.error
        when {
            // 出错：一句人话 + 「重试」上下两行摆
            error != null -> FailureBlock(error, onRetry = page::loadMore)
            page.loading -> CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            page.ended && page.items.isEmpty() -> Text(emptyText, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            page.ended -> Text(endText, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            else -> Spacer(Modifier.height(20.dp))
        }
    }
}

/**
 * 一条评论：头像、名字（和正文作者同一个人时加 [Poster.label] 标签）、置顶标签、楼层号、时间、IP 属地、点赞数、
 * 内容（[RichTextView]）、楼中楼预览（"名字：内容"几行）和"共 N 条回复"。
 *
 * 展示字段支持从一整页批量预计算结果 [displayItem] 传入（重组时 0 次跨边界 FFI 调用）。
 */
@Composable
fun CommentItem(
    comment: Comment,
    poster: Poster,
    modifier: Modifier = Modifier,
    onOpenReplies: ((Comment) -> Unit)? = null,
    repliesSupported: Boolean = true,
    replyToRootAuthorId: String? = null,
    compact: Boolean = false,
    /** 评论内容自动翻成中文；楼中楼回复先不做，[RepliesContent] 传 false。 */
    translate: Boolean = true,
    displayItem: FfiPresentCommentDisplayItem? = null,
    /** 楼中楼预览不在评论里、是后来取的（知乎回答楼层，[CommentsState.loadAnswerPreview]）；null = 用评论自带的。 */
    lazyPreviews: List<Comment>? = null,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val platform = LocalDetailPlatform.current
    val display = displayItem ?: remember(comment.id, poster, replyToRootAuthorId, repliesSupported, platform?.label) {
        RustPresentPaging.prepareCommentItem(
            comment = comment,
            posterId = poster.id,
            replyToRootAuthorId = replyToRootAuthorId,
            onOpenRepliesAllowed = repliesSupported,
            platformLabel = platform?.label,
        )
    }
    val isPoster = display.isPoster

    val translationStore = LocalTranslation.current
    val autoTranslate = remember(comment.id, comment.content, platform, translationStore?.enabled, translationStore?.target) {
        translate && translationStore?.shouldAutoTranslate(comment.content.textOnly(), comment.language, platform) == true
    }
    val contentTranslation = rememberRichTextTranslation(comment.content, autoTranslate, comment.language)
    val replyPrefix = display.replyToName?.let { replyName ->
        buildAnnotatedString {
            append("回复 ")
            withStyle(SpanStyle(color = MaterialTheme.colorScheme.primary)) { append("@$replyName") }
            append("：")
        }
    }
    Row(modifier.fillMaxWidth().padding(vertical = if (compact) 8.dp else 12.dp)) {
        Avatar(comment.author?.avatar, if (compact) 28 else 36)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(
                        display.authorName,
                        style = MaterialTheme.typography.labelLarge,
                        fontWeight = FontWeight.Medium,
                        color = if (isPoster) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (isPoster) Tag(poster.label, MaterialTheme.colorScheme.primary)
                    if (comment.pinned) Tag("置顶", Color(0xFFE8863B))
                }
                display.likesText?.let { countText ->
                    Icon(Icons.Outlined.ThumbUp, contentDescription = null, tint = muted, modifier = Modifier.size(14.dp))
                    Spacer(Modifier.width(3.dp))
                    Text(countText, style = MaterialTheme.typography.labelSmall, color = muted)
                }
            }
            val meta = display.metaText
            if (meta.isNotEmpty()) Text(meta, style = MaterialTheme.typography.labelSmall, color = muted)
            // 知乎回答楼层：长回答先折叠，点「展开全文」（滑出屏幕再回来还是展开着）
            val folded = remember(comment.id, contentTranslation.text) {
                if (isAnswerFloor(comment)) foldedAnswer(contentTranslation.text) else null
            }
            var expanded by androidx.compose.runtime.saveable.rememberSaveable(comment.id) { androidx.compose.runtime.mutableStateOf(false) }
            if (!comment.content.isEmpty() || replyPrefix != null) {
                RichTextView(
                    if (folded != null && !expanded) folded else contentTranslation.text,
                    modifier = Modifier.padding(top = 4.dp),
                    style = MaterialTheme.typography.bodyMedium.copy(lineHeight = 22.sp),
                    prefix = replyPrefix,
                    maxImageWidth = 240.dp,
                    blockSpacing = 6.dp,
                    imageMode = RichImageMode.COMMENT,
                )
            }
            if (folded != null) {
                Text(
                    if (expanded) "收起" else "展开全文",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(top = 4.dp).pointerHoverIcon(PointerIcon.Hand).clickable { expanded = !expanded },
                )
            }
            TranslationEntryRow(contentTranslation.entry, compact = compact)
            if (onOpenReplies != null) RepliesPreview(comment, poster, onOpenReplies.takeIf { repliesSupported }, display, lazyPreviews)
        }
    }
}

/**
 * 楼中楼预览：灰底框里最多几条"名字：内容"，底下"共 N 条回复"，整块点开看全部。
 */
@Composable
private fun RepliesPreview(
    comment: Comment,
    poster: Poster,
    onOpenReplies: ((Comment) -> Unit)?,
    display: FfiPresentCommentDisplayItem? = null,
    lazyPreviews: List<Comment>? = null,
) {
    val lazy = comment.previewReplies.isEmpty() && lazyPreviews != null
    val previews = if (lazy) lazyPreviews.orEmpty().take(ANSWER_PREVIEW_COMMENTS) else comment.previewReplies.take(3)
    val platform = LocalDetailPlatform.current
    val hasPreviews = display?.hasPreviews ?: (previews.isNotEmpty() || (comment.replyCount ?: 0L) > 0L)
    if (!hasPreviews) return

    val translationStore = LocalTranslation.current
    val autoReplies = remember(comment.id, previews, platform, translationStore?.enabled, translationStore?.target) {
        previews.map { reply -> translationStore?.shouldAutoTranslate(reply.content.textOnly(), reply.language, platform) == true }
    }
    val sources = remember(comment.id, previews, autoReplies) {
        previews.filterIndexed { i, _ -> autoReplies.getOrElse(i) { false } }
            .flatMap { reply -> RustTranslate.translatableTexts(reply.content.blocks) }.distinct()
    }
    LaunchedEffect(comment.id, previews, autoReplies, translationStore?.enabled, translationStore?.target) {
        sources.forEach { translationStore?.ensure(it) }
    }
    val showOriginal = false
    fun translated(content: RichText): RichText {
        val plan = RustTranslate.planBlocks(content.blocks)
        val texts = plan.map { src -> src?.let { translationStore?.state(it) as? TranslationState.Done }?.text }
        if (texts.all { it == null }) return content
        return RichText(RustTranslate.applyBlocks(content.blocks, texts))
    }
    val more = ((if (lazy) null else display?.moreText) ?: RustPresentPaging.repliesMoreText(
        comment.replyCount,
        previews.size,
        onOpenReplies != null,
        platform?.label,
    ))?.let { if (isAnswerFloor(comment)) it.replace("回复", "评论") else it }
    Column(
        Modifier.padding(top = 8.dp).fillMaxWidth().clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .then(if (onOpenReplies != null) Modifier.pointerHoverIcon(PointerIcon.Hand).clickable { onOpenReplies(comment) } else Modifier)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        for ((index, reply) in previews.withIndex()) {
            val previewDisplay = if (lazy) null else display?.previews?.getOrNull(index)
            RichTextView(
                if (autoReplies.getOrElse(index) { false } && !showOriginal) translated(reply.content) else reply.content,
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp, lineHeight = 19.sp),
                prefix = previewPrefix(reply, poster, MaterialTheme.colorScheme.primary, MaterialTheme.colorScheme.onSurfaceVariant, previewDisplay),
                maxImageWidth = 120.dp,
                blockSpacing = 4.dp,
                imageMode = RichImageMode.PREVIEW,
            )
        }
        more?.let {
            val color = if (onOpenReplies != null) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant
            Text(it, style = MaterialTheme.typography.labelMedium, color = color)
        }
    }
}

/** 预览里一条回复开头的"名字（楼主）[ 回复 xxx]："。 */
internal fun previewPrefix(
    reply: Comment,
    poster: Poster,
    highlight: Color,
    muted: Color,
    display: FfiPresentPreviewReplyDisplayItem? = null,
): AnnotatedString = buildAnnotatedString {
    val authorName = display?.authorName ?: (reply.author?.name ?: "匿名")
    val isPoster = display?.isPoster ?: (poster.id != null && reply.author?.id == poster.id)
    val replyToName = display?.replyToName ?: reply.replyTo?.name

    withStyle(SpanStyle(color = highlight, fontWeight = FontWeight.Medium)) { append(authorName) }
    if (isPoster) withStyle(SpanStyle(color = highlight)) { append("（${poster.label}）") }
    if (replyToName != null) {
        withStyle(SpanStyle(color = muted)) { append(" 回复 ") }
        withStyle(SpanStyle(color = highlight)) { append(replyToName) }
    }
    append("：")
}

@Composable
private fun Tag(text: String, color: Color) {
    Text(
        text,
        color = color,
        fontSize = 10.sp,
        fontWeight = FontWeight.Medium,
        modifier = Modifier.background(color.copy(alpha = 0.14f), RoundedCornerShape(4.dp)).padding(horizontal = 4.dp, vertical = 1.dp),
    )
}

/**
 * 楼中楼面板：上面是被点开的那条评论，下面是全部回复（分页，滑到底加载）。
 */
@Composable
fun RepliesPanel(request: RepliesRequest, host: DetailHost, wide: Boolean, onClose: () -> Unit, modifier: Modifier = Modifier) {
    val state = remember { MutableTransitionState(false).apply { targetState = true } }
    BoxWithConstraints(modifier.fillMaxSize()) {
        if (wide) {
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.32f)).clickable(indication = null, interactionSource = null, onClick = onClose))
        }
        AnimatedVisibility(
            visibleState = state,
            enter = slideInHorizontally { it } + fadeIn(),
            modifier = Modifier.align(Alignment.CenterEnd),
        ) {
            Surface(
                modifier = Modifier.fillMaxHeight().then(if (wide) Modifier.width(minOf(520.dp, maxWidth * 0.9f)) else Modifier.fillMaxWidth()),
                color = MaterialTheme.colorScheme.background,
                shadowElevation = 8.dp,
            ) {
                RepliesContent(request, host, wide, onClose)
            }
        }
    }
}

@Composable
private fun RepliesContent(request: RepliesRequest, host: DetailHost, wide: Boolean, onClose: () -> Unit) {
    val scope = rememberCoroutineScope()
    val source = host.graph.sourceOrNull(host.platform) as? NativeDetailSource
    val page = remember(request) { source?.takeIf { repliesSupported(it.platform, host.graph.platforms) }?.let { repliesState(it, request.contentId, request.comment.id, scope) } }
    val root = request.comment
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().height(52.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onClose) {
                Icon(if (wide) Icons.Outlined.Close else Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = if (wide) "关闭" else "返回")
            }
            Text(if (isAnswerFloor(root)) "评论" else "回复", style = MaterialTheme.typography.titleMedium)
            root.replyCount?.takeIf { it > 0 }?.let {
                Text(" $it", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        val displayBatch = remember(page?.items, root, request.poster) {
            page?.items?.let { items ->
                RustPresentPaging.prepareCommentBatch(
                    comments = items,
                    posterId = request.poster.id,
                    replyToRootAuthorId = root.author?.id,
                    onOpenRepliesAllowed = false,
                    platformLabel = host.platform.label,
                ).associateBy { it.id }
            }.orEmpty()
        }
        LazyColumn(Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp)) {
            item(key = "root") { CommentItem(root, poster = request.poster) }
            item(key = "all-label") {
                Text(
                    if (isAnswerFloor(root)) "全部评论" else "全部回复",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainer).padding(horizontal = 12.dp, vertical = 6.dp),
                )
            }
            if (page == null) {
                val who = host.platform.label
                item {
                    Text(
                        RustPresentPaging.repliesUnsupportedText(who, source != null),
                        Modifier.fillMaxWidth().padding(16.dp),
                        textAlign = TextAlign.Center,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                items(page.items, key = { "reply-${it.id}" }) { reply ->
                    Column {
                        CommentItem(
                            comment = reply,
                            poster = request.poster,
                            compact = true,
                            replyToRootAuthorId = root.author?.id,
                            displayItem = displayBatch[reply.id],
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                item(key = "footer") { PagedFooter(page, emptyText = "还没有回复", endText = "没有更多回复了") }
            }
        }
    }
}
