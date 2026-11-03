package feedhub.brain

import feedhub.core.FeedItem

/**
 * 信息流 / 搜索**取回一页之后的那一次过滤**：平台标的 18+ 整条去掉，再按内容过滤第一层
 * （屏蔽词一直生效；外出模式关键词 / 社区只在 `ContentFilter.Rules.outingMode` 开着时生效）。
 *
 * `FeedState.kept` / `SearchState.kept` 原来在 Kotlin 里逐条 `AdultFilter.hidden(it) || ContentFilter.hidden(it, rules)`，
 * 不逐条跨 FFI。判断口径和以前一模一样（18+ 先挡、命中的算藏），[hidden] 只用于"藏掉几条"的日志
 * （**只记数量**，日志里不出现标题、也不出现用户设的关键词）。
 */
object FeedFilter {
    /** 一批过滤的结果：能上屏的条目（顺序不变）+ 藏掉几条。 */
    class Shown(val items: List<FeedItem>, val hidden: Int)

    /**
     * 一批里能上屏的那些。
     *
     * 规则为空时**也要过一遍**（18+ 那条一直生效）；只是"空列表"这种没有内容的情况直接返回，不过 FFI。
     */
    fun shown(items: List<FeedItem>, rules: ContentFilter.Rules): Shown {
        if (items.isEmpty()) return Shown(items, 0)
        val shown = Brain.shown(items, rules)
        if (shown.hidden == 0) return Shown(items, 0)
        return Shown(List(shown.kept.size) { items[shown.kept[it]] }, shown.hidden)
    }
}
