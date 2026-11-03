package feedhub.cli

import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.TimeText
import java.time.ZoneId

private fun kindLabel(k: ItemKind) = when (k) {
    ItemKind.VIDEO -> "视频"
    ItemKind.ARTICLE -> "文章"
    ItemKind.ANSWER -> "回答"
    ItemKind.QUESTION -> "问题"
    ItemKind.POST -> "帖子"
    ItemKind.MOMENT -> "动态"
    ItemKind.LIVE -> "直播"
    ItemKind.OTHER -> "其他"
}

private fun count(n: Long?): String? = n?.let {
    when {
        it >= 100_000_000 -> "%.1f亿".format(it / 1e8)
        it >= 10_000 -> "%.1f万".format(it / 1e4)
        else -> it.toString()
    }
}

private fun oneLine(s: String?, max: Int): String? =
    s?.replace(Regex("\\s+"), " ")?.trim()?.let { if (it.length > max) it.take(max) + "…" else it }?.ifEmpty { null }

/** 把一条渲染成几行人看的文本。 */
fun formatItem(item: FeedItem, index: Int? = null, indent: String = ""): String = buildString {
    val head = listOfNotNull(index?.let { "#$it" }, "[${item.platform.label}·${kindLabel(item.kind)}]").joinToString(" ")
    val title = oneLine(item.title, 70) ?: oneLine(item.summary, 70) ?: "（无标题）"
    append(indent).append(head).append(' ').append(title).append('\n')

    val meta = listOfNotNull(
        item.author?.name,
        item.community?.let { "@$it" },
        // 平台只给到天 / 月的（YouTube TV tile）按那一档截断，不显示假钟点（见 core.TimeText）
        item.publishedAt?.let { TimeText.absolute(it, item.publishedAtPrecision, zone = ZoneId.of("Asia/Shanghai")) },
        (item.media.firstOrNull() as? Media.Video)?.durationSec?.let { "%d:%02d".format(it / 60, it % 60) },
        count(item.stats.views)?.let { if (item.kind == ItemKind.VIDEO || item.kind == ItemKind.LIVE) "播放 $it" else "浏览 $it" },
        count(item.stats.danmaku)?.let { "弹幕 $it" },
        count(item.stats.likes)?.let { "赞 $it" },
        count(item.stats.comments)?.let { "评论 $it" },
        item.reason,
    )
    if (meta.isNotEmpty()) append(indent).append("    ").append(meta.joinToString(" · ")).append('\n')

    if (item.title != null) oneLine(item.summary, 90)?.let { append(indent).append("    ").append(it).append('\n') }
    val images = item.media.count { it is Media.Image }
    if (images > 0) append(indent).append("    [图片 ×$images]\n")
    item.url?.let { append(indent).append("    ").append(it).append('\n') }
    item.repost?.let { append(indent).append("    ↳ 转发：\n").append(formatItem(it, null, "$indent      ")) }
}
