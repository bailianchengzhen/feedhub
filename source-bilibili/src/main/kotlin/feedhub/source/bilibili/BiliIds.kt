package feedhub.source.bilibili

/**
 * [feedhub.core.DetailTarget.Native.id] 的格式：
 * - 视频：`BV1xxxxxxxxx`（和 [feedhub.core.FeedItem.id] 一样，推荐流、关注流、动态流里的视频都是它）
 * - 动态 / 图文 / 专栏（opus）：`dyn:<动态 id_str>`，例如 `dyn:1258983140737679377`。
 *   信息流里这类条目的 [feedhub.core.FeedItem.id] 是不带前缀的 `id_str`，只有 `detail` 里的 Native id 带前缀。
 *   专栏（cv）在 B站新版里也是一条动态（`DYNAMIC_TYPE_ARTICLE`），评论区类型 12，所以同样用 `dyn:`。
 * 为了方便命令行，纯数字也当动态 id 收。
 */
internal sealed interface BiliId {
    data class Video(val bvid: String) : BiliId
    data class Dynamic(val id: String) : BiliId
}

internal object BiliIds {
    private const val DYN = "dyn:"

    fun dynamic(idStr: String) = DYN + idStr

    fun parse(id: String): BiliId? = when {
        BvAv.isBvid(id) -> BiliId.Video(id)
        id.startsWith(DYN) -> id.removePrefix(DYN).takeIf { it.isNotEmpty() && it.all(Char::isDigit) }?.let { BiliId.Dynamic(it) }
        id.isNotEmpty() && id.all(Char::isDigit) -> BiliId.Dynamic(id)
        else -> null
    }
}
