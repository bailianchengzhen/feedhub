package feedhub.source.bilibili

import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.Span
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/**
 * B站文字 → [Span] / [Block] 的公共小工具：评论、视频简介、动态都用。
 * 做法是"先找出要替换的片段（表情、@、链接），再把剩下的当普通文字"，所以和 B站各接口的结构无关。
 */
internal object BiliText {
    /** 文本里一个要换成特殊 [Span] 的片段。[once] 为 true 时只换第一处（评论关键词链接的 `match_once`）。 */
    class Token(val text: String, val span: Span, val once: Boolean = false)

    private class Match(val start: Int, val end: Int, val span: Span)

    /** `\r\n`、单独的 `\r` 都当 `\n`。评论里真有只用 `\r` 换行的。 */
    fun normalize(text: String): String = text.replace("\r\n", "\n").replace('\r', '\n')

    /** 按 [tokens] 把 [text] 切成 Span。重叠时先出现的、长的优先。 */
    fun spans(text: String, tokens: List<Token>): List<Span> {
        val matches = mutableListOf<Match>()
        for (t in tokens) {
            if (t.text.isEmpty()) continue
            var from = 0
            while (true) {
                val i = text.indexOf(t.text, from)
                if (i < 0) break
                matches += Match(i, i + t.text.length, t.span)
                if (t.once) break
                from = i + t.text.length
            }
        }
        matches.sortWith(compareBy<Match> { it.start }.thenByDescending { it.end - it.start })
        val out = mutableListOf<Span>()
        var pos = 0
        for (m in matches) {
            if (m.start < pos) continue
            if (m.start > pos) out += Span.Text(text.substring(pos, m.start))
            out += m.span
            pos = m.end
        }
        if (pos < text.length) out += Span.Text(text.substring(pos))
        return out
    }

    /** 一串 Span 按 `\n` 分段；只有空白的段丢掉。 */
    fun paragraphs(spans: List<Span>): List<Block.Paragraph> {
        val out = mutableListOf<Block.Paragraph>()
        var cur = mutableListOf<Span>()
        fun flush() {
            if (cur.any { it !is Span.Text || it.text.isNotBlank() }) out += Block.Paragraph(cur)
            cur = mutableListOf()
        }
        for (s in spans) {
            if (s is Span.Text && '\n' in s.text) {
                s.text.split('\n').forEachIndexed { i, piece ->
                    if (i > 0) flush()
                    if (piece.isNotEmpty()) cur += s.copy(text = piece)
                }
            } else {
                cur += s
            }
        }
        flush()
        return out
    }

    private val URL = Regex("""https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+""")
    private val BV = Regex("""BV1[1-9A-HJ-NP-Za-km-z]{9}""")

    /** 简介里的网址和 BV 号自动变成链接（B站简介是纯文本，网页端是前端识别的）。 */
    fun linkTokens(text: String): List<Token> {
        val out = mutableListOf<Token>()
        for (m in URL.findAll(text)) {
            val url = m.value.trimEnd('.', ',', ';', ':', '!', '?', ')', ']', '\'')
            out += Token(url, Span.Link(url, url, BiliLinks.target(url)))
        }
        for (m in BV.findAll(text)) {
            val url = BiliParser.videoUrl(m.value)
            out += Token(m.value, Span.Link(m.value, url, DetailTarget.Native(m.value)))
        }
        return out.distinctBy { it.text }
    }
}

/** 站内链接 → 能原生打开的目标：视频页（BV / av）、图文页 `/opus/ID`、动态页 `t.bilibili.com/ID`。 */
internal object BiliLinks {
    private val VIDEO_PATH = Regex("""/video/(BV1[1-9A-HJ-NP-Za-km-z]{9}|av(\d+))""", RegexOption.IGNORE_CASE)
    private val OPUS_PATH = Regex("""/opus/(\d+)""")
    private val DYNAMIC_PATH = Regex("""/(\d{10,})""")

    fun target(url: String): DetailTarget? {
        val u = url.toHttpUrlOrNull() ?: return null
        val host = u.host
        if (host == "t.bilibili.com" || host == "m.bilibili.com" && u.encodedPath.matches(DYNAMIC_PATH)) {
            return DYNAMIC_PATH.matchEntire(u.encodedPath)?.let { DetailTarget.Native(BiliIds.dynamic(it.groupValues[1])) }
        }
        if (host != "www.bilibili.com" && host != "bilibili.com" && host != "m.bilibili.com") return null
        OPUS_PATH.find(u.encodedPath)?.let { return DetailTarget.Native(BiliIds.dynamic(it.groupValues[1])) }
        VIDEO_PATH.find(u.encodedPath)?.let { m ->
            val bv = m.groupValues[2].takeIf { it.isNotEmpty() }?.toLongOrNull()?.let(BvAv::toBvid)
                ?: m.groupValues[1].takeIf { BvAv.isBvid(it) }
            return bv?.let { DetailTarget.Native(it) }
        }
        return null
    }
}
