package feedhub.source.coolapk

import feedhub.core.BundledEmote
import feedhub.core.Span

/**
 * 酷安表情。正文和评论里表情是文字代码，官方没有公开图片地址，所以图片随插件带（本模块的资源 `feedhub-emoji/coolapk/`，
 * 图是酷安自己的表情图），这里只知道"有哪些代码、对应哪个文件"，不碰图片本身；
 * 地址是 [BundledEmote] 的 `feedhub-emoji:coolapk/<文件>`，由 app 的图片加载器按类路径资源取。
 *
 * 代码有两种写法（按 `(\[\S*?\]|#\(\S*?\))` 切）：
 * - `[受虐滑稽]`：方括号，390 个（含 `[OK]`，它和 `[ok]` 是同一张图）；
 * - `#(笑眼)`：圆括号前面带 `#`，93 个；表里的键是 `(笑眼)`。不带 `#` 的 `(笑眼)` 就是普通文字（"(85mm)"、"(1)" 这种常见）。
 * 不在表里的方括号文字（`[链接]`、`[1]`、`[INJECT]`）不是表情，照旧是文字。
 */
object CoolapkEmoji {
    const val PACK = "coolapk"

    /** 认得的代码（`[xxx]` / `(xxx)`）→ 图片文件 id，不带扩展名；文件是资源 `feedhub-emoji/coolapk/<id>.png`。 */
    val table: Map<String, String> get() = COOLAPK_EMOJI_TABLE

    /** 资源文件名（`e012.png`）。 */
    fun fileName(id: String) = "$id.png"

    /** 方括号写法：不含 `[` `]` 和空白；圆括号写法：前面要有 `#`，不含 `(` `)` 和空白。 */
    private val CODE = Regex("\\[[^\\[\\]\\s]+]|#\\([^()\\s]+\\)")

    /** 一个文字代码（`[xxx]` 或 `#(xxx)`）→ 图片地址；不认得返回 null。 */
    fun url(code: String): String? {
        val key = if (code.startsWith("#(")) code.substring(1) else code
        val id = COOLAPK_EMOJI_TABLE[key] ?: return null
        return BundledEmote.url(PACK, fileName(id))
    }

    /**
     * 一段纯文字（已经解过实体）切成 [Span.Text] 和 [Span.Emote]。认得的代码变成 [Span.Emote]（`text` 是正文里的原文，包括 `#`），
     * 其余原样留在文字里。没有表情就是一个 [Span.Text]；空串是空列表。
     */
    fun spans(text: String): List<Span> {
        if (text.isEmpty()) return emptyList()
        var out: MutableList<Span>? = null
        var last = 0
        for (m in CODE.findAll(text)) {
            val url = url(m.value) ?: continue
            if (out == null) out = mutableListOf()
            if (m.range.first > last) out += Span.Text(text.substring(last, m.range.first))
            out += Span.Emote(m.value, url)
            last = m.range.last + 1
        }
        if (out == null) return listOf(Span.Text(text))
        if (last < text.length) out += Span.Text(text.substring(last))
        return out
    }
}
