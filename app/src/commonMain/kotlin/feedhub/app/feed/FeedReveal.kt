package feedhub.app.feed

/**
 * 打开的详情对应的那张卡片，在信息流里要不要滚、往哪边滚。
 *
 * 规则：**完全在可视区里（哪怕只露一角）就一动不动**；完全看不到才滚，而且只滚到最近的那条边
 * （"刚好露出来"），不把它顶到顶端、也不换页。点开一张卡片时它本来就在眼前，所以列表一动不动；只有详情里用快捷键 /
 * 按钮切到屏幕外的上一条 / 下一条（沉浸播放上下切、双栏里换一条）才会走到 [ABOVE] / [BELOW]。
 *
 * 纯逻辑，`FeedRevealTest` 离线测（不建 Compose 场景）。
 */
internal enum class FeedRevealSide {
    /** 已经在可视区里（露多少都算）：一点都不动。 */
    NONE,

    /** 完全看不到，而且在这张之前（上面）：往回滚，让它只见**底边**。 */
    ABOVE,

    /** 完全看不到，而且在这张之后（下面）：往前滚，让它只见**顶边**。 */
    BELOW,
}

/**
 * 见 [FeedRevealSide]。[visibleIndexes] 是网格当前**排到屏幕上**的那些下标
 * （`LazyStaggeredGridState.layoutInfo.visibleItemsInfo.map { it.index }`）。
 */
internal fun feedRevealSide(selectedIndex: Int, visibleIndexes: List<Int>): FeedRevealSide = when {
    selectedIndex < 0 -> FeedRevealSide.NONE
    visibleIndexes.isEmpty() -> FeedRevealSide.NONE // 还没排版（刚组合出来 / 列数刚变）：这一帧不动
    selectedIndex in visibleIndexes -> FeedRevealSide.NONE
    selectedIndex < visibleIndexes.min() -> FeedRevealSide.ABOVE
    else -> FeedRevealSide.BELOW
}

/**
 * [androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState.scrollToItem] 已经把这条的**开头**顶到可视区开头，
 * 之后还要再挪多少像素才"只见一条边"（正数 = 往后滚、内容往上走）。返回值直接给 `scrollBy`。
 *
 * - [FeedRevealSide.ABOVE]：它的底边挪到可视区顶边下面 [marginPx] 处 → 往前滚 `itemTop + itemHeight - margin`；
 * - [FeedRevealSide.BELOW]：它的顶边挪到可视区底边上面 [marginPx] 处 → 往回滚 `viewport - margin - itemTop`。
 *
 * [itemTopPx] 是这条相对可视区顶部的 y（`scrollToItem` 之后一般是 0；拿到的值不精确时这个公式仍然对）。
 */
internal fun revealAdjustPx(
    side: FeedRevealSide,
    itemTopPx: Int,
    itemHeightPx: Int,
    viewportHeightPx: Int,
    marginPx: Int,
): Int = when (side) {
    FeedRevealSide.NONE -> 0
    // 至少挪 1 像素：留白比卡片还大时也别"滚了 0"（那种情况本来就该看见它了）
    FeedRevealSide.ABOVE -> (itemTopPx + itemHeightPx - marginPx).coerceAtLeast(1)
    FeedRevealSide.BELOW -> (itemTopPx - (viewportHeightPx - marginPx)).coerceAtMost(-1)
}
