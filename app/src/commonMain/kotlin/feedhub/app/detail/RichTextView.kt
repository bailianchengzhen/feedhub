package feedhub.app.detail

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.text.appendInlineContent
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.isSpecified
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.isSpecified
import androidx.compose.ui.unit.sp
import feedhub.app.ui.emoteInlineContent
import feedhub.app.ui.withColorEmoji
import feedhub.app.ui.withFailedAsText
import feedhub.app.video.InlineVideo
import feedhub.core.Block
import feedhub.core.RichText
import feedhub.core.Span

/** 正文里一张图最高多高（超出的封顶高度、宽度缩窄，右下角给「长图」角标；不裁图，见 [bodyImageHeight]）。 */
internal val MaxImageHeight = 640.dp

/** 宫格格子的圆角。 */
private val GridCorner = RoundedCornerShape(6.dp)

/** 正文用的字号和行高（比默认的行距松一点，中文长文好读）。 */
@Composable
fun richBodyStyle(): TextStyle = MaterialTheme.typography.bodyLarge.copy(lineHeight = 26.sp)

/**
 * 富文本渲染：段落、小标题、引用、代码块、分割线、图片、卡片、视频（[Block.Video]，原地播放，见 [InlineVideo]）；行内的粗体 / 斜体、链接、@人、表情。
 * 视频简介、评论内容、图文正文都用它。一个 [Column]，块比较多的长文正文用 [richTextItems] 摊到 LazyColumn 里。
 *
 * - 图片：设置里"默认显示原图"开着用 `original`，否则 `url`；按宽高比占位（没给宽高就先 4:3，加载完按实际比例）；
 *   点击打开大图查看层（同一个 [text] 里的图可以左右切换）。**连续的几张图收成宫格**：
 *   2–4 张两列、≥5 张三列、最多 9 格（第 9 格叠 "+N"），点哪张从哪张开始看；1 张还是整宽大图。
 *   太长 / 太高的图（按宽度排会超过 [MaxImageHeight]）**高度封顶、宽度按比例缩窄，整张完整显示**，
 *   右下角给「长图」角标 + 渐变提示点开看全。
 * - 链接：站内（有 `target`）点了在详情里打开，站外的复制链接（桌面版不跳外部浏览器）。
 * - 点击这些靠 [LocalDetailHost]，不在 [DetailPane] 里时只渲染不响应。
 *
 * @param prefix 接在第一个段落最前面的一小段（楼中楼的"回复 xxx："、预览里的"名字："）；第一块不是段落就单独成行。
 * @param maxImageWidth 图片最宽多少（评论里的图不用撑满）。
 * @param imageMode 图片怎么排（见 [RichImageMode]）：正文 / 评论 / 楼中楼预览框三种，预览框是一行小方图。
 */
@Composable
fun RichTextView(
    text: RichText,
    modifier: Modifier = Modifier,
    style: TextStyle = richBodyStyle(),
    prefix: AnnotatedString? = null,
    maxImageWidth: Dp = Dp.Unspecified,
    blockSpacing: Dp = 10.dp,
    imageMode: RichImageMode = RichImageMode.BODY,
) {
    val blocks = remember(text) { text.blocks.filterNot { it.isBlank() } }
    val ctx = remember(text) { RichContext(blocks.filterIsInstance<Block.Image>()) }
    val units = remember(blocks) { richUnits(blocks) }
    val prefixInline = prefix != null && blocks.firstOrNull().let { it is Block.Paragraph || it is Block.Quote }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(blockSpacing)) {
        if (prefix != null && !prefixInline) Text(prefix, style = style)
        units.forEach { unit ->
            RichUnitView(unit, ctx, style, if (unit.firstIndex == 0 && prefixInline) prefix else null, maxImageWidth, imageMode)
        }
    }
}

/**
 * 长文正文：每个渲染单元一个 LazyColumn 条目，滑到才画（图片也才开始加载）。连续的图片是一组（宫格）。
 * 样式在可组合函数里取好传进来：[richBodyStyle]。
 */
fun LazyListScope.richTextItems(text: RichText, keyPrefix: String, style: TextStyle, blockSpacing: Dp = 12.dp) {
    val blocks = text.blocks.filterNot { it.isBlank() }
    val ctx = RichContext(blocks.filterIsInstance<Block.Image>())
    val units = richUnits(blocks)
    itemsIndexed(units, key = { _, unit -> "$keyPrefix${unit.firstIndex}" }) { _, unit ->
        Box(Modifier.padding(bottom = blockSpacing)) { RichUnitView(unit, ctx, style, null, Dp.Unspecified) }
    }
}

/** 同一段富文本里的图片，查看层里左右切换用。 */
@Immutable
internal class RichContext(val images: List<Block.Image>)

internal fun Block.isBlank(): Boolean = when (this) {
    is Block.Paragraph -> spans.all { it !is Span.Emote && it.text.isBlank() }
    is Block.Heading -> spans.all { it !is Span.Emote && it.text.isBlank() }
    is Block.Quote -> spans.all { it !is Span.Emote && it.text.isBlank() }
    is Block.Code -> text.isBlank()
    else -> false
}

@Composable
internal fun RichUnitView(
    unit: RichUnit,
    ctx: RichContext,
    style: TextStyle,
    prefix: AnnotatedString?,
    maxImageWidth: Dp,
    imageMode: RichImageMode = RichImageMode.BODY,
) {
    // 楼中楼预览框里一行小方图：单张和多张都走这条路（宫格会把评论区撑成好几行）
    val images = if (unit.blocks.firstOrNull() is Block.Image) unit.blocks.filterIsInstance<Block.Image>() else emptyList()
    when {
        imageMode == RichImageMode.PREVIEW && images.isNotEmpty() -> CommentThumbRow(images, ctx)
        unit.isGrid -> RichImageGrid(unit, ctx, maxImageWidth, imageMode)
        else -> RichBlockView(unit.blocks.first(), ctx, style, prefix, maxImageWidth, imageMode)
    }
}

@Composable
internal fun RichBlockView(
    block: Block,
    ctx: RichContext,
    style: TextStyle,
    prefix: AnnotatedString?,
    maxImageWidth: Dp,
    imageMode: RichImageMode = RichImageMode.BODY,
) {
    when (block) {
        is Block.Paragraph -> RichSpans(block.spans, style, prefix = prefix)
        is Block.Heading -> RichSpans(
            block.spans,
            MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold, lineHeight = 28.sp, fontSize = style.fontSize * 1.1f),
            modifier = Modifier.padding(top = 6.dp),
        )
        is Block.Quote -> Row(
            Modifier.fillMaxWidth().height(IntrinsicSize.Min).clip(RoundedCornerShape(6.dp)).background(MaterialTheme.colorScheme.surfaceContainer),
        ) {
            Box(Modifier.width(3.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outline))
            RichSpans(
                block.spans,
                style.copy(color = MaterialTheme.colorScheme.onSurfaceVariant),
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
                prefix = prefix,
            )
        }
        is Block.Code -> CodeBlock(block)
        is Block.Image ->
            if (imageMode == RichImageMode.PREVIEW) CommentThumbRow(listOf(block), ctx) else RichImage(block, ctx, maxImageWidth, imageMode)
        is Block.Card -> RichCard(block)
        is Block.Video -> InlineVideo(block, maxWidth = maxImageWidth)
        Block.Divider -> HorizontalDivider(Modifier.padding(vertical = 4.dp), color = MaterialTheme.colorScheme.outlineVariant)
    }
}

/** 一段行内内容：文字 + 链接 + @人 + 表情（画成行内小图，图片加载失败的改回文字代码）。 */
@Composable
private fun RichSpans(spans: List<Span>, style: TextStyle, modifier: Modifier = Modifier, prefix: AnnotatedString? = null) {
    val host = LocalDetailHost.current
    val primary = MaterialTheme.colorScheme.primary
    var failed by remember(spans) { mutableStateOf(emptySet<String>()) }
    val shown = remember(spans, failed) { spans.withFailedAsText(failed) }
    val rich = remember(shown, prefix, primary, host) {
        buildRichString(
            shown,
            linkStyle = SpanStyle(color = primary),
            mentionStyle = SpanStyle(color = primary, fontWeight = FontWeight.Medium),
            prefix = prefix,
            onLink = { host?.openLink(it.url, it.target) },
        )
    }
    val inline = remember(rich) { emoteInlineContent(rich.emotes, onError = { url -> failed = failed + url }) }
    // Unicode emoji 用彩色字体：桌面上 `⚡️ ❤️ ☺️ ⚠️` 这类
    // "要 U+FE0F 才彩色"的字符否则会被回退成黑白字形。安卓那边返回 null，什么都不变。
    val shownText = remember(rich) { rich.text.withColorEmoji() }
    Text(shownText, style = style, inlineContent = inline, modifier = modifier)
}

/** [buildRichString] 的结果：带样式 / 链接的文字，和里面表情的占位 id → 图片地址。 */
internal class RichString(val text: AnnotatedString, val emotes: Map<String, String>)

/**
 * 把一串行内内容拼成 [AnnotatedString]：粗体 / 斜体用样式，链接用 [LinkAnnotation.Clickable]（点击走 [onLink]），
 * @人 用 [mentionStyle]，表情在文字里留一个行内占位（id → 图片地址放在 [RichString.emotes]，图片加载不出来时显示它的文字代码）。
 */
internal fun buildRichString(
    spans: List<Span>,
    linkStyle: SpanStyle,
    mentionStyle: SpanStyle,
    prefix: AnnotatedString? = null,
    onLink: (Span.Link) -> Unit,
): RichString {
    val emotes = LinkedHashMap<String, String>()
    val text = AnnotatedString.Builder().apply {
        if (prefix != null) append(prefix)
        for (span in spans) {
            when (span) {
                is Span.Text ->
                    if (span.bold || span.italic) {
                        withStyle(
                            SpanStyle(
                                fontWeight = if (span.bold) FontWeight.Bold else null,
                                fontStyle = if (span.italic) FontStyle.Italic else null,
                            ),
                        ) { append(span.text) }
                    } else {
                        append(span.text)
                    }
                is Span.Link -> withLink(
                    LinkAnnotation.Clickable(
                        tag = span.url,
                        styles = TextLinkStyles(style = linkStyle),
                        linkInteractionListener = { onLink(span) },
                    ),
                ) { append(span.text) }
                is Span.Mention -> withStyle(mentionStyle) { append(span.text) }
                is Span.Emote -> {
                    val id = emotes.entries.firstOrNull { it.value == span.url }?.key ?: "emote${emotes.size}".also { emotes[it] = span.url }
                    appendInlineContent(id, span.text)
                }
            }
        }
    }.toAnnotatedString()
    return RichString(text, emotes)
}

@Composable
private fun CodeBlock(block: Block.Code) {
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(10.dp)) {
        block.language?.takeIf { it.isNotBlank() }?.let {
            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(
            block.text,
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
            lineHeight = 19.sp,
            softWrap = false,
            modifier = Modifier.horizontalScroll(rememberScrollState()),
        )
    }
}

/** 点开大图查看层：从 [block] 这一张开始（同一个富文本里的图都能左右切换）。 */
private fun openViewer(host: DetailHost?, ctx: RichContext, block: Block.Image, thumbs: DetailThumbnails) {
    if (host == null) return
    val index = ctx.images.indexOfFirst { it === block }.coerceAtLeast(0)
    // 大图也是先垫着：卡片上下好的缩略图最清楚，没有就用正文里已经加载过的那张（旧行为）
    host.openImages(
        ctx.images.map { img ->
            ViewerImage(img.original ?: img.url, thumb = thumbs.hint(img)?.thumbnail ?: img.url, caption = img.caption)
        },
        index,
    )
}

@Composable
private fun RichImage(block: Block.Image, ctx: RichContext, contentMaxWidth: Dp, imageMode: RichImageMode = RichImageMode.BODY) {
    val host = LocalDetailHost.current
    // 先用卡片上已经下好的缩略图垫着（[DetailThumbnails]），原图到了淡入替换
    val thumbs = LocalDetailThumbnails.current
    val hint = thumbs.hint(block)
    val plan = detailImagePlan(block.url, block.original, host?.originalImages == true, hint)
    val density = LocalDensity.current
    // 评论里的图只有 maxImageWidth 宽，没必要下原图：能按宽度缩的平台换缩略图（大图查看层仍用原图）。
    // 正文（详情页）照旧用平台给的地址，见 [RichImageMode]
    val widthPx = with(density) { (contentMaxWidth.takeIf { it.isSpecified } ?: 240.dp).toPx().toInt() }
    val shownUrl = if (imageMode == RichImageMode.COMMENT) commentImageUrl(plan.url, widthPx) else plan.url
    val declared = block.declaredRatio()
    // 尺寸先占好：有宽高用宽高，没有就用卡片上那张缩略图的真实比例，都没有才 4:3
    var ratio by remember(plan.url) { mutableStateOf(detailImageRatio(declared, hint)) }
    val onClick = { openViewer(host, ctx, block, thumbs) }
    Column(Modifier.then(if (contentMaxWidth.isSpecified) Modifier.widthIn(max = contentMaxWidth) else Modifier)) {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            // 单张图按它自己的比例完整显示：不裁（`ContentScale.Fit`），
            // 宽高先按声明的 / 卡片缩略图的真实比例占好，图到了不跳。
            // 只有"比 MaxImageHeight 还高"的才封顶高度、宽度按比例缩窄（整张仍然完整，只是变窄）+ 右下角「长图」角标。
            val box = bodyImageHeight(maxWidth.value, ratio, MaxImageHeight.value)
            val height = box.height.dp
            // 封顶高度时宽度按比例缩窄、居中摆；其余情况宽度撑满（等比例的框里 Fit 就是正好铺满）
            val width = box.widthFor(maxWidth.value, ratio).dp
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                Box(Modifier.width(width).height(height)) {
                    DetailPhoto(
                        url = shownUrl,
                        placeholder = plan.placeholder,
                        contentDescription = block.caption,
                        contentScale = ContentScale.Fit,
                        alignment = Alignment.Center,
                        onLoadedSize = { size ->
                            if (declared == null && size.isSpecified && size.height > 0f) ratio = size.width / size.height
                        },
                        modifier = Modifier.fillMaxSize()
                            .clip(RoundedCornerShape(8.dp))
                            .pointerHoverIcon(PointerIcon.Hand)
                            .clickable(onClick = onClick),
                    )
                    if (box.cropped) LongImageHint(Modifier.align(Alignment.BottomCenter))
                }
            }
        }
        block.caption?.takeIf { it.isNotBlank() }?.let {
            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 4.dp))
        }
    }
}

/**
 * 「长图」标记：图比正文里能舒服显示的最大高度还高时，
 * 右下角一个角标 + 底部渐变，提示"这张很长、点开看全图"。
 *
 * 以前这条是配着"底部裁掉下半截"用的；所以现在**图本身完整显示**
 * （高度封顶 + 宽度按比例缩窄，见 [bodyImageHeight]），角标只是提示"可以点开看更大"。文案没改（用户认这个）。
 */
@Composable
internal fun LongImageHint(modifier: Modifier) {
    Box(modifier.fillMaxWidth().height(72.dp).background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.55f))))) {
        Text(
            "长图",
            style = MaterialTheme.typography.labelSmall,
            color = Color.White,
            modifier = Modifier.align(Alignment.BottomEnd).padding(8.dp)
                .background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(50))
                .padding(horizontal = 8.dp, vertical = 2.dp),
        )
    }
}

/**
 * 正文里连续的几张图 → 宫格：2–4 张两列、≥5 张三列，最多 9 格（第 9 格叠 "+N"）。
 * 格子是正方形（Crop），点哪张就从哪张开始进大图查看层；组里的图不加说明文字（说明在查看器里显示）。
 */
@Composable
private fun RichImageGrid(unit: RichUnit, ctx: RichContext, maxWidth: Dp, imageMode: RichImageMode = RichImageMode.BODY) {
    val host = LocalDetailHost.current
    val thumbs = LocalDetailThumbnails.current
    val images = unit.blocks.filterIsInstance<Block.Image>()
    if (images.isEmpty()) return
    if (images.size == 1) {
        RichImage(images.first(), ctx, maxWidth, imageMode)
        return
    }
    val columns = imageGridColumns(images.size)
    val cells = imageGridCells(images.size)
    val more = imageGridMore(images.size)
    val rows = (cells + columns - 1) / columns
    Column(
        modifier = Modifier.then(if (maxWidth.isSpecified) Modifier.widthIn(max = maxWidth) else Modifier).fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(GridGap),
    ) {
        var drawn = 0
        repeat(rows) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(GridGap)) {
                repeat(columns) {
                    if (drawn < cells) {
                        val image = images[drawn]
                        val badge = if (drawn == cells - 1) more else 0
                        GridCell(
                            block = image,
                            more = badge,
                            modifier = Modifier.weight(1f),
                            imageMode = imageMode,
                            cellWidthPx = maxWidth.takeIf { it.isSpecified }?.let { w ->
                                with(LocalDensity.current) { (w / columns).toPx().toInt() }
                            },
                            onClick = { openViewer(host, ctx, image, thumbs) },
                        )
                        drawn++
                    } else {
                        // 最后一行不满：补空位，格子宽度才和其它行一样
                        Spacer(Modifier.weight(1f))
                    }
                }
            }
        }
    }
}

@Composable
private fun GridCell(
    block: Block.Image,
    more: Int,
    modifier: Modifier,
    onClick: () -> Unit,
    imageMode: RichImageMode = RichImageMode.BODY,
    cellWidthPx: Int? = null,
) {
    // 九宫格格子：卡片上下过的第一张（如果对得上）先垫着；格子的地址和单图一样（[Block.Image.url]）
    val thumbs = LocalDetailThumbnails.current
    val hint = thumbs.hint(block)
    val density = LocalDensity.current
    val px = cellWidthPx ?: with(density) { 120.dp.toPx().toInt() }
    Box(modifier.aspectRatio(1f).clip(GridCorner)) {
        DetailPhoto(
            url = if (imageMode == RichImageMode.COMMENT) commentImageUrl(block.url, px) else block.url,
            // 卡片上那张缩略图本身就是这个 URL 时不用再垫一层
            placeholder = hint?.thumbnail?.takeIf { it != block.url },
            contentDescription = block.caption,
            contentScale = ContentScale.Crop,
            modifier = Modifier.fillMaxSize().pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onClick),
        )
        if (more > 0) {
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)).clickable(onClick = onClick), contentAlignment = Alignment.Center) {
                Text("+$more", color = Color.White, style = MaterialTheme.typography.titleMedium)
            }
        }
    }
}

/**
 * 楼中楼预览框里的图片（[RichImageMode.PREVIEW]）：**一行小方图**，边长 [CommentThumbSize]、最多 [CommentThumbMax] 张，
 * 多出来的第 4 张叠「+N」（和宫格第 9 格一个意思）。
 *
 * 为什么要另画一个而不是沿用宫格：预览框在评论区里，一张图按 120dp 宽、两三列排下来会把一屏评论挤成一两条；
 * 一行 56dp 的小方图够认出是什么、又不占地方。尺寸是定死的（不按比例、不裁切高度），所以图到之前就占好了，
 * 不会跳。点图看大图（[openViewer]，和正文同一条路，大图用原图 / `original`），点框的其它地方才开楼中楼。
 */
@Composable
private fun CommentThumbRow(images: List<Block.Image>, ctx: RichContext) {
    if (images.isEmpty()) return
    val host = LocalDetailHost.current
    val thumbs = LocalDetailThumbnails.current
    val px = with(LocalDensity.current) { CommentThumbSize.roundToPx() }
    val cells = commentThumbCells(images.size)
    val more = commentThumbMore(images.size)
    Row(horizontalArrangement = Arrangement.spacedBy(GridGap)) {
        for (i in 0 until cells) {
            val image = images[i]
            val onClick = { openViewer(host, ctx, image, thumbs) }
            Box(Modifier.size(CommentThumbSize).clip(GridCorner)) {
                DetailPhoto(
                    url = commentImageUrl(image.url, px),
                    // 卡片上下过同一张就先垫着（本地缓存命中，不额外发请求）
                    placeholder = thumbs.hint(image)?.thumbnail?.takeIf { it != image.url },
                    contentDescription = image.caption,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize().pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onClick),
                )
                if (i == cells - 1 && more > 0) {
                    Box(
                        Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)).clickable(onClick = onClick),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text("+$more", color = Color.White, style = MaterialTheme.typography.labelLarge)
                    }
                }
            }
        }
    }
}

@Composable
private fun RichCard(block: Block.Card) {
    val host = LocalDetailHost.current
    val onClick = host?.takeIf { block.target != null || block.url != null }?.let { h -> { h.openTarget(block.target, block.url) } }
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).background(MaterialTheme.colorScheme.surfaceContainer)
            .then(if (onClick != null) Modifier.pointerHoverIcon(PointerIcon.Hand).clickable(onClick = onClick) else Modifier)
            .padding(8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        block.cover?.let {
            DetailPhoto(
                url = it,
                placeholder = LocalDetailThumbnails.current.hint(it)?.thumbnail,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.size(width = 96.dp, height = 64.dp).clip(RoundedCornerShape(6.dp)),
            )
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(block.title, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium, maxLines = 2, overflow = TextOverflow.Ellipsis)
            block.subtitle?.takeIf { it.isNotBlank() }?.let {
                Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        if (onClick != null) {
            Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
