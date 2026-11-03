package feedhub.brain

import feedhub.core.FeedItem

/**
 * 成人内容（平台标的 18+ / 敏感内容）的统一过滤。
 *
 * 直接不出现。
 * 判断只看 `FeedItem.adult`（平台自己的标记），不是我们猜的。
 *
 * 这是唯一一处判断"要不要藏"的地方，所有把条目展示给用户的入口都必须过它，
 * 界面拿到的 `items` 里就没有
 *   被标的条目（所以它们也不会被记曝光 / 停留）；
 * - 收藏 / 历史：`feedhub.app.library` 在查询里过滤（SQLite 用 `is_adult` 列，内存实现用 [hidden]）；
 * - 详情页直接打开（从评论里的链接、从通知点进来）：[hidden] 为真就只画一行 [NOTICE]，不加载正文和媒体，
 *   也不记历史 / 曝光；
 * - 转发的原内容 `FeedItem.repost` 自己带 `adult`，卡片和详情页展示它之前用 [visibleRepost] 过一遍。
 *
 * 这里只剩"读平台标的那个布尔"（[hidden]）和两个展示辅助（[visible] / [visibleRepost]）：
 * **判断规则本身在 Rust**（`brain::filter` 的 `blocked` / `shown`，和信息流那条链同一份），
 * 不会有第二套"18+ 要不要藏"的逻辑漂出去。
 */
object AdultFilter {
    /** 详情页直接打开一条被标成 18+ 的内容时显示的那一行。 */
    const val NOTICE: String = "这条内容被标为 18+，按设置已隐藏"

    /** 这条要不要整条藏掉。 */
    fun hidden(item: FeedItem): Boolean = item.adult

    /** 展示给用户之前过一遍：被平台标成 18+ 的整条去掉。 */
    fun visible(items: List<FeedItem>): List<FeedItem> = items.filterNot(::hidden)

    /**
     * 转发的原内容：`FeedItem.repost` 有自己的 `adult`，被标了就返回 null（连"@某某：标题"那一行也不显示）。
     * 没有转发也返回 null。
     */
    fun visibleRepost(item: FeedItem): FeedItem? = item.repost?.takeUnless(::hidden)
}
