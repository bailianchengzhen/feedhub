package feedhub.source.reddit

import feedhub.core.Block
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.UserRef

/**
 * Reddit 的 markdown → 我们的富文本（[RichText]）。
 *
 * Reddit 的正文（`selftext`）和评论（`body`）都是 markdown 源文，没有 HTML；仓库里没有现成的 markdown 处理
 * （`core/HtmlRichText` 是给真 HTML 的），所以这里自己写一个小转换器，只做 Reddit 上真会出现的子集：
 *
 * - 标题 `#`~`######` → [Block.Heading]；引用 `>` → [Block.Quote]；围栏代码 ``` → [Block.Code]；
 * - 列表项（`-` / `*` / `+` / `1.`）→ 带 "• " / "1. " 前缀的 [Block.Paragraph]；
 *   表格逐行降级成文字，分隔行丢掉；`---` 这类分割线 → [Block.Divider]；
 * - 单独一段只有一张图（`![alt](url)`）→ [Block.Image]，尺寸 / 原图 markdown 里没有就是 null；
 * - 行内：`[文字](链接)` → [Span.Link]、`**粗**` / `*斜*` / `_斜_` → [Span.Text] 的 bold / italic、
 *   `` `代码` `` / `~~删除~~` → 普通文字（模型里没有对应样式）、`<http://...>` 和裸 URL → [Span.Link]、
 *   `u/某人` → [Span.Mention]、`r/版块` → 指向版块的 [Span.Link]；
 * - 反斜杠转义、`&amp;` 之类的 HTML 实体按 markdown 规则还原。
 *
 * 不做的（Reddit 上少见，降级成纯文字）：嵌套列表缩进、表格对齐、脚注、上下标、代码高亮、
 * 段落里混着的行内图片（降级成链接文字）。
 *
 * 纯函数、不发网络，`RedditMarkdownTest` 直接喂字符串。
 */
internal object RedditMarkdown {

    fun toBlocks(markdown: String?): List<Block> {
        val text = markdown?.replace("\r\n", "\n")?.replace('\r', '\n') ?: return emptyList()
        if (text.isBlank()) return emptyList()
        val lines = text.split('\n')
        val blocks = mutableListOf<Block>()
        var i = 0
        while (i < lines.size) {
            val line = lines[i]
            val trimmed = line.trim()
            val heading = HEADING.matchEntire(trimmed)
            val listItem = LIST_ITEM.matchEntire(line)
            when {
                trimmed.isEmpty() -> i++

                // 围栏代码块（``` 或 ~~~），语言写在开头围栏后面
                trimmed.startsWith("```") || trimmed.startsWith("~~~") -> {
                    val fence = trimmed.take(3)
                    val language = trimmed.removePrefix(fence).trim().takeIf { it.isNotEmpty() }
                    val body = mutableListOf<String>()
                    i++
                    while (i < lines.size && !lines[i].trim().startsWith(fence)) {
                        body += lines[i]
                        i++
                    }
                    if (i < lines.size) i++ // 吃掉收尾围栏
                    blocks += Block.Code(body.joinToString("\n"), language)
                }

                heading != null -> {
                    blocks += Block.Heading(inline(heading.groupValues[2]))
                    i++
                }

                trimmed.startsWith(">") -> {
                    val quoted = mutableListOf<String>()
                    while (i < lines.size && lines[i].trim().startsWith(">")) {
                        quoted += lines[i].trim().removePrefix(">").removePrefix(" ")
                        i++
                    }
                    blocks += Block.Quote(inline(quoted.joinToString("\n")))
                }

                HR.matches(trimmed) -> {
                    blocks += Block.Divider
                    i++
                }

                // 表格：分隔行（|---|---|）丢掉，其余按 " | " 拼成一行文字
                trimmed.startsWith("|") && trimmed.endsWith("|") -> {
                    val rows = mutableListOf<String>()
                    while (i < lines.size && lines[i].trim().startsWith("|") && lines[i].trim().endsWith("|")) {
                        val row = lines[i].trim()
                        if (!TABLE_SEP.matches(row)) rows += row.trim('|').split('|').joinToString(" | ") { it.trim() }
                        i++
                    }
                    if (rows.isNotEmpty()) blocks += Block.Paragraph(inline(rows.joinToString("\n")))
                }

                listItem != null -> {
                    val ordered = listItem.groupValues[3]
                    val prefix = if (ordered.isNotEmpty()) "$ordered " else "• "
                    blocks += Block.Paragraph(listOf(Span.Text(prefix)) + inline(listItem.groupValues[4]))
                    i++
                }

                // 整段就是一张图
                IMAGE_ONLY.matchEntire(trimmed) != null -> {
                    val m = IMAGE_ONLY.matchEntire(trimmed)!!
                    blocks += Block.Image(url = m.groupValues[2], caption = m.groupValues[1].takeIf { it.isNotBlank() })
                    i++
                }

                else -> {
                    val para = mutableListOf<String>()
                    while (i < lines.size) {
                        val t = lines[i].trim()
                        if (t.isEmpty() || t.startsWith("```") || t.startsWith("~~~") || t.startsWith(">") ||
                            HEADING.matches(t) || HR.matches(t) || LIST_ITEM.matches(lines[i]) || IMAGE_ONLY.matches(t) ||
                            (t.startsWith("|") && t.endsWith("|"))
                        ) {
                            break
                        }
                        para += t
                        i++
                    }
                    if (para.isNotEmpty()) blocks += Block.Paragraph(inline(para.joinToString("\n")))
                }
            }
        }
        return blocks
    }

    /** 摘要用的纯文字：markdown 记号去掉，多行压成一行；图片不写 "[图片]"（卡片会单独画出来）。 */
    fun toPlain(markdown: String?): String =
        RichText(toBlocks(markdown)).textOnly().replace(Regex("\\s+"), " ").trim()

    // ---------- 行内 ----------

    private fun inline(text: String): List<Span> {
        val out = mutableListOf<Span>()
        val plain = StringBuilder()
        var i = 0

        fun flush() {
            if (plain.isNotEmpty()) {
                out += Span.Text(plain.toString())
                plain.clear()
            }
        }

        while (i < text.length) {
            val span = matchInline(text, i)
            if (span != null) {
                flush()
                out += span.first
                i += span.second
                continue
            }
            // 反斜杠转义：\* 就是字面量 *
            if (text[i] == '\\' && i + 1 < text.length && text[i + 1] in ESCAPABLE) {
                plain.append(text[i + 1])
                i += 2
                continue
            }
            plain.append(text[i])
            i++
        }
        flush()
        return decodeEntities(out)
    }

    /** 在 [text] 的 [at] 位置匹配一个行内元素；返回（Span，吃掉的字符数）。 */
    private fun matchInline(text: String, at: Int): Pair<Span, Int>? {
        val prev = if (at == 0) ' ' else text[at - 1]
        // find(text, at) 不复制子串；range.first == at 才算"从这个位置开始"
        fun Regex.at(): MatchResult? = find(text, at)?.takeIf { it.range.first == at }

        IMAGE.at()?.let {
            return Span.Link(it.groupValues[1].ifBlank { "图片" }, it.groupValues[2]) to it.value.length
        }
        LINK.at()?.let {
            return Span.Link(it.groupValues[1], it.groupValues[2]) to it.value.length
        }
        AUTOLINK.at()?.let {
            return Span.Link(it.groupValues[1], it.groupValues[1]) to it.value.length
        }
        BOLD.at()?.let {
            return Span.Text(unescape(it.groupValues[1]), bold = true) to it.value.length
        }
        ITALIC_STAR.at()?.let {
            return Span.Text(unescape(it.groupValues[1]), italic = true) to it.value.length
        }
        ITALIC_UNDERSCORE.at()?.let {
            return Span.Text(unescape(it.groupValues[1]), italic = true) to it.value.length
        }
        STRIKE.at()?.let {
            return Span.Text(unescape(it.groupValues[1])) to it.value.length
        }
        INLINE_CODE.at()?.let {
            return Span.Text(it.groupValues[1]) to it.value.length
        }
        // u/某人 / r/版块：只在词首认（前面是空白或括号），不然 "you/foo" 会被切开
        if (!prev.isLetterOrDigit() && prev != '_') {
            MENTION.at()?.let {
                val name = it.groupValues[1]
                return Span.Mention("/u/$name", UserRef(PLATFORM, name, name, url = "https://www.reddit.com/user/$name")) to it.value.length
            }
            SUBREDDIT.at()?.let {
                val name = it.groupValues[1]
                return Span.Link("/r/$name", "https://www.reddit.com/r/$name") to it.value.length
            }
        }
        BARE_URL.at()?.let {
            val url = it.value.trimEnd('.', ',', ';', ':', '!', '?', ')', ']')
            return Span.Link(url, url) to url.length
        }
        return null
    }

    /** markdown 里 `\*` 这样的转义。 */
    private fun unescape(s: String): String = s.replace(Regex("\\\\([\\\\`*_{}\\[\\]()#+\\-.!~|>])"), "$1")

    /**
     * HTML 实体：Reddit 的 markdown 源文里偶尔带 `&amp;`、`&gt;`（`raw_json=1` 时 `&` 也可能写成 `&amp;amp;`）。
     * 数字实体（`&#x2F;`）也还原。[Span] 的 text 只改文字，链接地址不动。
     */
    private fun decodeEntities(spans: List<Span>): List<Span> = spans.map { s ->
        val text = NAMED_ENTITIES.entries.fold(s.text) { acc, (k, v) -> acc.replace(k, v) }
            .replace(NUMERIC_ENTITY) { m ->
                val code = m.groupValues[1]
                val cp = if (code.startsWith("x") || code.startsWith("X")) {
                    code.drop(1).toIntOrNull(16)
                } else {
                    code.toIntOrNull()
                }
                if (cp == null || cp <= 0 || cp > 0x10FFFF || cp in 0xD800..0xDFFF) "\uFFFD" else String(Character.toChars(cp))
            }
        when (s) {
            is Span.Text -> s.copy(text = text)
            is Span.Link -> s.copy(text = text)
            is Span.Emote -> s.copy(text = text)
            is Span.Mention -> s.copy(text = text)
        }
    }

    private val PLATFORM = REDDIT

    private val NUMERIC_ENTITY = Regex("&#(x?[0-9A-Fa-f]+);")
    private val NAMED_ENTITIES = mapOf(
        "&lt;" to "<", "&gt;" to ">", "&quot;" to "\"", "&#39;" to "'", "&nbsp;" to " ",
        "&mdash;" to "—", "&ndash;" to "–", "&hellip;" to "…", "&amp;" to "&",
    )

    private val HEADING = Regex("^(#{1,6})\\s+(.*)$")
    private val HR = Regex("^(-{3,}|\\*{3,}|_{3,})$")
    private val TABLE_SEP = Regex("^\\|[\\s:|-]+\\|$")
    private val LIST_ITEM = Regex("^(\\s*)(?:([-*+])|(\\d+[.)]))\\s+(.*)$")
    private val IMAGE_ONLY = Regex("^!\\[([^\\]]*)\\]\\(([^)\\s]+)(?:\\s+\"[^\"]*\")?\\)$")
    private val IMAGE = Regex("!\\[([^\\]]*)\\]\\(([^)\\s]+)(?:\\s+\"[^\"]*\")?\\)")
    private val LINK = Regex("\\[([^\\]]+)\\]\\(([^)\\s]+)(?:\\s+\"[^\"]*\")?\\)")
    private val AUTOLINK = Regex("<(https?://[^>\\s]+)>")
    private val BOLD = Regex("\\*\\*(.+?)\\*\\*")
    private val ITALIC_STAR = Regex("\\*([^*\\n]+?)\\*")
    private val ITALIC_UNDERSCORE = Regex("(?<![A-Za-z0-9_])_([^_\\n]+?)_(?![A-Za-z0-9_])")
    private val STRIKE = Regex("~~(.+?)~~")
    private val INLINE_CODE = Regex("`([^`\\n]+?)`")
    private val MENTION = Regex("/?u/([A-Za-z0-9_-]{1,32})")
    private val SUBREDDIT = Regex("/?r/([A-Za-z0-9_]{1,32})")
    private val BARE_URL = Regex("https?://[^\\s<>\\[\\]()]+")
    private val ESCAPABLE = "\\`*_{}[]()#+-.!~|>".toSet()
}
