package feedhub.app.detail

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import feedhub.app.ui.CardEmoteSize
import feedhub.app.ui.EmoteText
import feedhub.core.Emotes
import feedhub.core.FeedItem

/**
 * 详情页里**标题**那一行的表情。
 *
 * 以前详情标题是裸 `Text(...)`：平台表情代码（B站 `[doge]`、小黑盒 `[超人]`、小红书 `[笑哭R]`、酷安 `[受虐滑稽]`…）
 * 在卡片上是图、点进详情就变回字面文字；Unicode emoji 也只有系统字体那条路。现在统一走卡片同一套
 * [EmoteText]（行内表情图 + Unicode emoji 用彩色字体，见 [feedhub.app.ui.EmojiFont]）。
 *
 * 表情表用的是"信息流里的 + 详情接口给的"合并表，并且**只留标题里真有的**代码：详情接口有时不带 emotes，信息流那一份才是有的。
 */
internal fun titleEmotes(shown: FeedItem?, fallback: FeedItem? = null): Map<String, String> {
    val title = shown?.title ?: fallback?.title ?: return emptyMap()
    val table = LinkedHashMap<String, String>()
    for (src in listOfNotNull(fallback, shown)) table.putAll(src.emotes)
    return Emotes.used(table, title)
}

/**
 * 详情页标题（图文详情的头部、视频详情的"信息"块、转发原文的标题都走它）。
 *
 * @param text 已经定好要显示的文字（可能是译文；译文里没有表情代码，会原样当文字画）
 * @param emotes 表情代码 → 图片地址，一般来自 [titleEmotes]
 */
@Composable
internal fun DetailTitleText(
    text: String,
    emotes: Map<String, String>,
    style: TextStyle,
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    fontWeight: FontWeight? = null,
    maxLines: Int = Int.MAX_VALUE,
    overflow: TextOverflow = TextOverflow.Clip,
) {
    // 标题字号比卡片大：表情按字号的 1.3 倍放（22sp 标题 → 28.6sp，放得进 30sp 的行高），
    // 和卡片摘要同一个比例，所以还是 [CardEmoteSize]。
    EmoteText(
        text = text,
        emotes = remember(text, emotes) { emotes.filterKeys { it in text } },
        modifier = modifier,
        color = color,
        fontWeight = fontWeight,
        overflow = overflow,
        maxLines = maxLines,
        style = style,
        emoteSize = CardEmoteSize,
    )
}
