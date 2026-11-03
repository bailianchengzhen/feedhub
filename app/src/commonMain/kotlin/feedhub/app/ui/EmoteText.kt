package feedhub.app.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.Placeholder
import androidx.compose.ui.text.PlaceholderVerticalAlign
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import coil3.compose.AsyncImage
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import coil3.request.crossfade
import feedhub.app.detail.buildRichString
import feedhub.core.BundledEmote
import feedhub.core.Emotes
import feedhub.core.Span

/** 行内表情占的位置：正文（[feedhub.app.detail.RichTextView]）按字号的 1.5 倍。 */
val RichEmoteSize: TextUnit = 1.5.em

/**
 * 信息流卡片里表情占的位置：比正文小一点，卡片字号小、行高紧（bodySmall 12sp / 16sp 行高，1.5 倍会撑高那一行、
 * 行距不齐），1.3 倍的图在 12sp 下是 15.6sp，放得进 16sp 的行高，14sp / 20sp 的标题更宽松。
 */
val CardEmoteSize: TextUnit = 1.3.em

/**
 * 行内表情的 inline content：每个占位 id 一张图（[images]：占位 id → 图片地址），大小 [size] × [size]，和文字垂直居中。
 * 正文（[feedhub.app.detail.RichTextView]）和信息流卡片（[EmoteText]）共用。图片是网络地址或随应用带的 `feedhub-emoji:` 地址
 * （[BundledEmoteImages]），GIF / 动态 WebP 照样会动（解码器在图片加载器里）。
 * 加载失败时调 [onError]（参数是图片地址），调用方可以据此把这个表情改回文字代码。
 */
fun emoteInlineContent(
    images: Map<String, String>,
    size: TextUnit = RichEmoteSize,
    onError: (url: String) -> Unit = {},
): Map<String, InlineTextContent> = images.mapValues { (_, url) ->
    InlineTextContent(Placeholder(size, size, PlaceholderVerticalAlign.TextCenter)) {
        EmoteInlineImage(url, onError)
    }
}

/**
 * 行内的一张表情图。占位大小在排版时就已经定死（[InlineTextContent] 的 [Placeholder]），所以图什么时候到都不会跳行。
 *
 * - 随插件带的（`feedhub-emoji:`，[BundledEmoteImages]）：**同步**读资源 + 同步解码，第一帧就有图，不走图片加载器；
 *   同一个表情第二次出现直接拿缓存里的那张，不再读、不再解。
 * - 网络的：交给图片加载器（它自己管磁盘 / 内存缓存，同一个地址第二次是内存缓存、不发请求）。
 *   **关掉淡入**（`crossfade`）：表情是行内小图，淡入看起来就是"先空一下再慢慢出来"。
 */
@Composable
private fun EmoteInlineImage(url: String, onError: (String) -> Unit) {
    val bundled = remember(url) { BundledEmoteImages.get(url) }
    if (bundled != null) {
        Image(
            bitmap = bundled,
            contentDescription = null,
            contentScale = ContentScale.Fit,
            modifier = Modifier.fillMaxWidth().fillMaxHeight(),
        )
        return
    }
    if (BundledEmote.parse(url) != null) {
        // 是随应用带的地址但读不到 / 解不开（插件拆了、资源缺了）：按加载失败处理，改回文字代码
        LaunchedEffect(url) { onError(url) }
        return
    }
    val context = LocalPlatformContext.current
    val request = remember(url, context) {
        ImageRequest.Builder(context).data(url).crossfade(false).build()
    }
    AsyncImage(
        model = request,
        contentDescription = null,
        contentScale = ContentScale.Fit,
        onError = { onError(url) },
        modifier = Modifier.fillMaxWidth().fillMaxHeight(),
    )
}

/** 图片地址在 [failed] 里（加载失败）的表情改回它的文字代码，其余原样。 */
internal fun List<Span>.withFailedAsText(failed: Set<String>): List<Span> =
    if (failed.isEmpty()) this else map { if (it is Span.Emote && it.url in failed) Span.Text(it.text) else it }

/**
 * 纯文字（没有平台表情代码）的那条路径也把 **Unicode emoji** 换成彩色字体。
 * 这一端不需要兜底时返回 null，调用方照旧用 [String]。
 */
private fun plainOrEmoji(text: String): AnnotatedString? = colorEmojiText(text)

/**
 * 带表情的纯文字（信息流卡片的标题 / 摘要 / 转发原文）：[emotes]（文字代码 → 图片地址，即 [feedhub.core.FeedItem.emotes]）里认得的代码画成行内小图，
 * 其余照旧是文字。**[text] 不变**：表情只是显示，复制 / 分享 / 屏蔽词用的还是文字代码。
 *
 * - 图片加载失败的表情改回它的文字代码（这个表情在这段文字里重新排版，不是留一块空白）。
 * - 有行数限制（[maxLines]）时行内的图和字一样参与截断：最后一行放不下的部分连同图一起被省略号替掉，不会把截断弄乱；
 *   图和字一样大，不会撑高某一行。
 * - 没有表情（[emotes] 为空，或文字里一个都没有）就是普通的 [Text]。
 */
@Composable
fun EmoteText(
    text: String,
    emotes: Map<String, String>,
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    fontWeight: FontWeight? = null,
    overflow: TextOverflow = TextOverflow.Clip,
    maxLines: Int = Int.MAX_VALUE,
    minLines: Int = 1,
    style: TextStyle = LocalTextStyle.current,
    emoteSize: TextUnit = CardEmoteSize,
) {
    val present = remember(text, emotes) { emotes.filterKeys { it in text } }
    if (present.isEmpty()) {
        // 没有平台表情代码：仍然是普通 Text，只是把 Unicode emoji 那几段换成彩色字体（桌面才有兜底字体族）
        val emoji = remember(text) { plainOrEmoji(text) }
        if (emoji == null) {
            Text(text, modifier, color = color, fontWeight = fontWeight, overflow = overflow, maxLines = maxLines, minLines = minLines, style = style)
        } else {
            Text(emoji, modifier, color = color, fontWeight = fontWeight, overflow = overflow, maxLines = maxLines, minLines = minLines, style = style)
        }
        return
    }
    // 加载失败的图片地址：这些表情不再占行内位置，改回文字代码
    var failed by remember(present) { mutableStateOf(emptySet<String>()) }
    val rich = remember(text, present, failed) {
        buildRichString(Emotes.split(text, present).withFailedAsText(failed), linkStyle = SpanStyle(), mentionStyle = SpanStyle(), onLink = {})
    }
    val inline = remember(rich, emoteSize) { emoteInlineContent(rich.emotes, emoteSize) { url -> failed = failed + url } }
    // Unicode emoji（平台表情之外的）用彩色字体；有行内表情时不带字重，免得给位图字体合成加粗
    val shown = remember(rich) { rich.text.withColorEmoji() }
    Text(
        shown, modifier, color = color, fontWeight = fontWeight, overflow = overflow, maxLines = maxLines, minLines = minLines,
        inlineContent = inline, style = style,
    )
}
