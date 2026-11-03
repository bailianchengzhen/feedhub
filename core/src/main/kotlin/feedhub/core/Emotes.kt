package feedhub.core

/**
 * 信息流卡片上的表情（[FeedItem.emotes]）：纯文本里的文字代码 → 图片地址。
 * 各 Source 用 [used] / [of] 把自己的表情来源缩成"这两段文字里真有的那几个"；前端用 [split] 把文字切成文字和表情。
 */
object Emotes {
    /** 从 [table]（文字代码 → 图片地址，可以是整张表）里挑出 [texts] 里真的出现过的代码。 */
    fun used(table: Map<String, String>, vararg texts: String?): Map<String, String> {
        if (table.isEmpty()) return emptyMap()
        val present = texts.filterNotNull().filter { it.isNotEmpty() }
        if (present.isEmpty()) return emptyMap()
        val out = LinkedHashMap<String, String>()
        for ((code, url) in table) {
            if (code.isNotEmpty() && url.isNotEmpty() && present.any { code in it }) out[code] = url
        }
        return out
    }

    /** 一串 [Span] 里的表情：`Span.Emote.text` → `url`。 */
    fun of(spans: Iterable<Span>): Map<String, String> {
        val out = LinkedHashMap<String, String>()
        for (s in spans) if (s is Span.Emote) out.putIfAbsent(s.text, s.url)
        return out
    }

    /**
     * 把 [text] 按 [emotes] 切成 [Span.Text] 和 [Span.Emote]（`text` 是原文里的代码）。
     * 同一位置上多个代码都对得上时取最长的；表里没有的 `[xxx]` 照旧留在文字里。没有表情就是一个 [Span.Text]，空串是空列表。
     */
    fun split(text: String, emotes: Map<String, String>): List<Span> {
        if (text.isEmpty()) return emptyList()
        val codes = emotes.keys.filter { it.isNotEmpty() }
        if (codes.isEmpty()) return listOf(Span.Text(text))
        val re = Regex(codes.sortedByDescending { it.length }.joinToString("|") { Regex.escape(it) })
        val out = mutableListOf<Span>()
        var last = 0
        for (m in re.findAll(text)) {
            if (m.range.first > last) out += Span.Text(text.substring(last, m.range.first))
            out += Span.Emote(m.value, emotes.getValue(m.value))
            last = m.range.last + 1
        }
        if (out.isEmpty()) return listOf(Span.Text(text))
        if (last < text.length) out += Span.Text(text.substring(last))
        return out
    }
}
