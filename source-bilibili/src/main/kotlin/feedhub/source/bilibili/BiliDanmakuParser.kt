package feedhub.source.bilibili

import feedhub.core.Danmaku
import feedhub.core.DanmakuMode
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.source.bilibili.proto.DanmakuElem
import feedhub.source.bilibili.proto.DmSegMobileReply

/** 弹幕分段的 protobuf（[DmSegMobileReply]）→ [Danmaku]。 */
internal object BiliDanmakuParser {
    fun segment(bytes: ByteArray): List<Danmaku> {
        if (bytes.isEmpty()) return emptyList()
        val reply = try {
            DmSegMobileReply.ADAPTER.decode(bytes)
        } catch (e: Exception) {
            throw ParseException(BILIBILI, "弹幕分段不是合法的 protobuf（${bytes.size} 字节）", e)
        }
        return reply.elems.mapNotNull(::danmaku)
    }

    /** mode 1 / 2 / 3 滚动，4 底部，5 顶部，其余（6 逆向、7 高级、8 代码、9 BAS）归 OTHER。 */
    fun mode(mode: Int): DanmakuMode = when (mode) {
        1, 2, 3 -> DanmakuMode.SCROLL
        4 -> DanmakuMode.BOTTOM
        5 -> DanmakuMode.TOP
        else -> DanmakuMode.OTHER
    }

    private fun danmaku(e: DanmakuElem): Danmaku? {
        if (e.content.isBlank()) return null
        return Danmaku(
            id = e.id_str.ifEmpty { e.id.toString() },
            timeMs = e.progress.toLong().coerceAtLeast(0),
            mode = mode(e.mode),
            text = e.content,
            color = e.color and 0xFFFFFF,
            fontSize = e.fontsize.takeIf { it > 0 } ?: 25,
            weight = e.weight.takeIf { it > 0 },
        )
    }
}
