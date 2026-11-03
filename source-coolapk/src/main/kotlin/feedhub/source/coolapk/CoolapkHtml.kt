package feedhub.source.coolapk

import feedhub.core.Block
import feedhub.core.Emotes
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.UserRef
import feedhub.core.toHttps
import java.net.URLDecoder
import java.net.URLEncoder

/**
 * 酷安正文 HTML → [RichText] / 纯文本。纯函数。
 *
 * 酷安的 message 不是完整 HTML，而是"纯文本 + 少量标签"：换行就是 `\n`，标签只有这几种：
 * - 话题 `<a class="feed-link-tag" href="/t/小米18?type=0">#小米18#</a>`
 * - @人 `<a class="feed-link-uname" href="/u/用户名或uid">@用户名</a>`
 * - 外链 `<a class="feed-link-url" href="https://...">网页链接</a>`
 * - 信息流里被截断的正文末尾 `<a href="">查看更多</a>`，详情里的 `<!--break-->`
 * - 偶尔有 `<br/>`
 *
 * 表情是 `[受虐滑稽]`、`#(笑眼)` 这样的文字代码，官方没有公开的图片地址，图片随应用带（见 [CoolapkEmoji]）：
 * 正文文字里认得的代码切成 [Span.Emote]（`url` 是 `feedhub-emoji:coolapk/<文件>`，由 app 的图片加载器取），不认得的方括号文字照旧是文字。
 * [toPlainText] 不变（信息流卡片的摘要用），表情还是它的文字代码。
 * 正文、评论、楼中楼都走这里（`CoolapkParser.comment` 也是 [toBlocks]）；链接文字里的 `[xxx]` 不动，它是链接的一部分。
 */
internal object CoolapkHtml {
    private val P = COOLAPK
    const val WEB = "https://www.coolapk.com"

    private val TOKEN = Regex(
        "<!--.*?-->|<a\\b([^>]*)>(.*?)</a\\s*>|<br\\s*/?>|<img\\b([^>]*)>|<[^>]*>|[^<]+|<",
        setOf(RegexOption.IGNORE_CASE, RegexOption.DOT_MATCHES_ALL),
    )
    private val HREF = Regex("href\\s*=\\s*(\"([^\"]*)\"|'([^']*)')", RegexOption.IGNORE_CASE)
    private val SRC = Regex("src\\s*=\\s*(\"([^\"]*)\"|'([^']*)')", RegexOption.IGNORE_CASE)
    private val CLASS = Regex("class\\s*=\\s*(\"([^\"]*)\"|'([^']*)')", RegexOption.IGNORE_CASE)
    private val TAG = Regex("<[^>]*>")

    /** 解析成段落（按 `\n` / `<br>` 分段，空段落丢掉）。正文里的 `<img>` 变成图片块。 */
    fun toBlocks(html: String): List<Block> {
        val out = mutableListOf<Block>()
        var spans = mutableListOf<Span>()
        fun flush() {
            normalize(spans)?.let { out += Block.Paragraph(it) }
            spans = mutableListOf()
        }
        fun text(t: String) {
            val lines = t.split('\n')
            lines.forEachIndexed { i, line ->
                if (i > 0) flush()
                if (line.isNotEmpty()) spans += CoolapkEmoji.spans(line)
            }
        }
        for (m in TOKEN.findAll(html)) {
            val tok = m.value
            when {
                tok.startsWith("<!--") -> Unit
                m.groups[2] != null -> link(m.groupValues[1], m.groupValues[2])?.let { spans += it }
                tok.length > 1 && tok.startsWith("<br", ignoreCase = true) -> flush()
                tok.startsWith("<img", ignoreCase = true) -> {
                    val src = attr(SRC, m.groupValues[3])
                    if (!src.isNullOrBlank()) {
                        flush()
                        out += image(src)
                    }
                }
                tok.startsWith("<") && tok.length > 1 -> Unit // 其他标签去掉，保留里面的文字
                else -> text(decodeEntities(tok))
            }
        }
        flush()
        return out
    }

    fun toRichText(html: String): RichText = RichText(toBlocks(html))

    /** 纯文本（摘要用）：段落之间换行，链接只留文字，"查看更多"去掉。表情还是文字代码。 */
    fun toPlainText(html: String): String = toPlain(html).text

    /** [toPlainText] 的结果，加上里面认得的表情：文字代码（`[受虐滑稽]`、`#(笑眼)`，原样）→ 随应用带的图片地址，见 [CoolapkEmoji]。 */
    class Plain(val text: String, val emotes: Map<String, String>)

    /** 同 [toPlainText]，另外返回文字里的表情（信息流卡片把它们画成图片，见 [feedhub.core.FeedItem.emotes]）。 */
    fun toPlain(html: String): Plain {
        val paragraphs = toBlocks(html).filterIsInstance<Block.Paragraph>()
        return Plain(
            paragraphs.joinToString("\n") { p -> p.spans.joinToString("") { it.text } }.trim(),
            Emotes.of(paragraphs.flatMap { it.spans }),
        )
    }

    /** 图片块；地址里带尺寸（`..._866@1280x2772.jpg`）时顺便填上宽高。 */
    fun image(url: String): Block.Image {
        val (w, h) = imageSize(url)
        return Block.Image(url.toHttps(), w, h)
    }

    private val SIZE = Regex("@(\\d+)x(\\d+)(?=\\.[A-Za-z]+(?:[?#].*)?$)")

    fun imageSize(url: String): Pair<Int?, Int?> {
        val m = SIZE.find(url) ?: return null to null
        return m.groupValues[1].toIntOrNull() to m.groupValues[2].toIntOrNull()
    }

    fun userUrl(uid: String) = "$WEB/u/$uid"

    // ---------- 内部 ----------

    private fun link(attrs: String, innerHtml: String): Span? {
        val text = decodeEntities(innerHtml.replace(TAG, "")).replace('\n', ' ')
        val href = attr(HREF, attrs)?.trim().orEmpty()
        // 信息流里截断正文后面的"查看更多"，href 是空的
        if (href.isEmpty()) return null
        if (text.isBlank()) return null
        val cls = attr(CLASS, attrs).orEmpty()
        val userPath = Regex("^(?:https?://(?:www\\.)?coolapk\\.com)?/u/([^/?#]+)").find(href)?.groupValues?.get(1)
        if (userPath != null || "feed-link-uname" in cls) {
            val target = userPath?.let { URLDecoder.decode(it, Charsets.UTF_8) } ?: text.removePrefix("@")
            return if (target.all(Char::isDigit) && target.isNotEmpty()) {
                Span.Mention(text, UserRef(P, target, text.removePrefix("@"), url = userUrl(target)))
            } else {
                // 很多 @ 链接只给用户名、不给 uid，拿不到 uid 就退化成普通链接（酷安网页 /u/ 后面也接受用户名）
                Span.Link(text, "$WEB/u/" + URLEncoder.encode(target, Charsets.UTF_8).replace("+", "%20"))
            }
        }
        val url = absolute(href)
        // B站视频链接 → 用我们自己的 B站播放器打开（B站 Source 的原生详情）
        return Span.Link(text, url, CoolapkVideo.bilibiliTarget(url))
    }

    private fun absolute(href: String): String = when {
        href.startsWith("//") -> "https:$href"
        href.startsWith("/") -> WEB + href
        else -> href
    }

    private fun attr(re: Regex, attrs: String): String? {
        val m = re.find(attrs) ?: return null
        return decodeEntities(m.groups[2]?.value ?: m.groups[3]?.value ?: return null)
    }

    /** 合并相邻文字、去掉首尾空白；全是空白就返回 null。 */
    private fun normalize(spans: List<Span>): List<Span>? {
        val merged = mutableListOf<Span>()
        for (s in spans) {
            val last = merged.lastOrNull()
            if (s is Span.Text && last is Span.Text) merged[merged.size - 1] = Span.Text(last.text + s.text) else merged += s
        }
        (merged.firstOrNull() as? Span.Text)?.let { merged[0] = Span.Text(it.text.trimStart()) }
        (merged.lastOrNull() as? Span.Text)?.let { merged[merged.size - 1] = Span.Text(it.text.trimEnd()) }
        merged.removeAll { it is Span.Text && it.text.isEmpty() }
        if (merged.all { it is Span.Text && it.text.isBlank() }) return null
        return merged
    }

    private val ENTITY = Regex("&(#\\d+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);")
    private val NAMED = mapOf("amp" to "&", "lt" to "<", "gt" to ">", "quot" to "\"", "apos" to "'", "nbsp" to " ")

    fun decodeEntities(s: String): String = if ('&' !in s) s else ENTITY.replace(s) { m ->
        val e = m.groupValues[1]
        when {
            e.startsWith("#x") || e.startsWith("#X") -> e.substring(2).toIntOrNull(16)?.let { String(Character.toChars(it)) }
            e.startsWith("#") -> e.substring(1).toIntOrNull()?.let { String(Character.toChars(it)) }
            else -> NAMED[e]
        } ?: m.value
    }
}
