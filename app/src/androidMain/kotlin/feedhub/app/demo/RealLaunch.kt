package feedhub.app.demo

import feedhub.app.AppGraph
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.NativeDetailSource
import feedhub.core.Platform
import feedhub.app.DetailItem

/**
 * 真实数据里按 debug 的启动参数取一条完整的 [FeedItem]（`screen=detail` / `screen=video` 用）。
 *
 * - 给了 [id]：走这个平台的详情接口（和点开卡片是同一条路）；
 * - 没给 [id]：在这个平台的**推荐流**里挑一条视频（[portrait] 为 true 时优先挑竖屏的，用来截沉浸界面），
 *   挑不到就退第一条。
 *
 * **会真的发请求**（推荐流 1 个 + 详情 1 个），所以只在 debug 启动参数这条路用；平台不认识 / 不支持
 * 原生详情时返回 null（界面显示"取不到"）。
 */
suspend fun androidRealItem(graph: AppGraph, platform: Platform, id: String? = null, portrait: Boolean = false): FeedItem? {
    val source = graph.source(platform)
    if (!id.isNullOrBlank()) {
        val native = source as? NativeDetailSource ?: return null
        return native.detail(id).item.copy(detail = DetailTarget.Native(id))
    }
    val page = source.feed(FeedKind.RECOMMEND, null)
    val videos = page.items.filter { it.kind == ItemKind.VIDEO }
    val picked = (if (portrait) videos.firstOrNull { it.height() > it.width() } else null)
        ?: videos.firstOrNull()
        ?: page.items.firstOrNull()
    return picked
}

/** 这条内容的视频尺寸（竖屏判断用）；卡片 / 详情都没给尺寸时按 0 算。 */
private fun FeedItem.width(): Int = (media.firstOrNull() as? feedhub.core.Media.Video)?.width ?: 0
private fun FeedItem.height(): Int = (media.firstOrNull() as? feedhub.core.Media.Video)?.height ?: 0
