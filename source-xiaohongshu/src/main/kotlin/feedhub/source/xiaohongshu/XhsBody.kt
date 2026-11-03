package feedhub.source.xiaohongshu

import feedhub.core.Block
import feedhub.core.Span
import feedhub.core.UserRef

/**
 * 笔记正文和评论文字的富文本化。小红书的文字是纯文本加几种记号：
 * - 话题：`#日漫[话题]#`（笔记的 `tag_list` 里有话题 id，没有就退回 `#日漫` 文字）；
 * - @人：`@昵称`（`at_user_list` / `at_users` 里有这个人才认，否则就是普通文字）；
 * - 表情：`[笑哭R]`、`[doge]`（表见 [XhsEmoteTable]，表里没有的保持文字）。
 */
internal object XhsBody {
    /** 方括号里没有空白、不超过 12 个字的记号，可能是表情代码。有这样的记号才去取表情表。 */
    val EMOTE_TOKEN = Regex("""\[[^\[\]\s]{1,12}]""")

    private val TOPIC = Regex("""#([^#\[\]\n]+?)\[话题]#""")

    /** 话题页的网页地址。 */
    fun topicUrl(id: String) = "https://www.xiaohongshu.com/page/topics/$id"

    /** 纯文本：`#日漫[话题]#` 变成 `#日漫`，别的原样（表情代码、@昵称都留着）。信息流摘要和分享用。 */
    fun plain(text: String): String = TOPIC.replace(text) { "#" + it.groupValues[1] }

    /**
     * 把一段文字（可以有换行）切成段落块。每一行一个段落，空行丢掉。
     * @param topics 话题名 → 话题 id
     * @param mentions 笔记 / 评论里 @ 到的人
     */
    fun blocks(text: String, emotes: Map<String, String>, topics: Map<String, String>, mentions: List<UserRef>): List<Block> =
        text.split('\n').map { it.trimEnd('\r') }.filter { it.isNotBlank() }
            .map { Block.Paragraph(spans(it, emotes, topics, mentions)) }

    fun spans(line: String, emotes: Map<String, String>, topics: Map<String, String>, mentions: List<UserRef>): List<Span> {
        val named = mentions.filter { it.name.isNotEmpty() }.distinctBy { it.name }.sortedByDescending { it.name.length }
        val pattern = buildString {
            append("""#(?<topic>[^#\[\]\n]+?)\[话题]#""")
            // 没有 @ 的人时用一个永远匹配不上的分支占住 mention 这个组名
            if (named.isNotEmpty()) append("|(?<mention>@(?:").append(named.joinToString("|") { Regex.escape(it.name) }).append("))")
            else append("|(?<mention>(?!x)x)")
            append("""|(?<emote>\[[^\[\]\s]{1,12}])""")
        }
        val out = mutableListOf<Span>()
        val buf = StringBuilder()
        fun flush() {
            if (buf.isNotEmpty()) out += Span.Text(buf.toString()).also { buf.setLength(0) }
        }
        var last = 0
        for (m in Regex(pattern).findAll(line)) {
            buf.append(line, last, m.range.first)
            last = m.range.last + 1
            val topic = m.groups["topic"]?.value
            val mention = m.groups["mention"]?.value
            val emote = m.groups["emote"]?.value
            when {
                topic != null -> {
                    flush()
                    val id = topics[topic]
                    out += if (id != null) Span.Link("#$topic", topicUrl(id)) else Span.Text("#$topic")
                }
                mention != null -> {
                    val user = named.first { "@" + it.name == mention }
                    flush()
                    out += Span.Mention(mention, user)
                }
                emote != null -> {
                    val url = emotes[emote]
                    if (url != null) {
                        flush()
                        out += Span.Emote(emote, url)
                    } else {
                        buf.append(emote)
                    }
                }
            }
        }
        buf.append(line, last, line.length)
        flush()
        // 连续的文字合并，免得被没认出来的记号切碎
        return out.fold(mutableListOf<Span>()) { acc, s ->
            val prev = acc.lastOrNull()
            if (prev is Span.Text && s is Span.Text && !prev.bold && !s.bold) acc[acc.size - 1] = Span.Text(prev.text + s.text) else acc += s
            acc
        }
    }
}
