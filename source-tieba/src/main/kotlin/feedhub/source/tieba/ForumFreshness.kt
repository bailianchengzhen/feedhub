package feedhub.source.tieba

import feedhub.core.FeedItem

/**
 * 关注流刷新时，哪些吧的第一页该重新抓。纯函数。
 *
 * 关注的吧有几十个，活跃度差很多，每次刷新都把所有吧的第一页抓一遍
 * （约 50 个请求，限速下 30 秒）不划算。所以只抓"很可能有新帖"的吧：
 *
 *     预计新帖数 = 每天新帖数 × 距上次抓取的天数 >= 1
 *
 * 同时设一个最长不刷新时间 [MAX_STALENESS_MS]，再冷清的吧也不会超过它没看过；
 * 再设一个最短间隔 [MIN_GAP_MS]，连着下拉刷新时不会反复打同一个吧。
 *
 * "每天新帖数"取两个估计里大的那个：
 * - 吧列表接口的 `day_thread_num`（今日新帖数；每天从零开始，所以凌晨偏低）；
 * - 上次抓第一页时从帖子时间算出来的（[observedPerDay]），不受零点影响，但只反映上次抓取时的速度。
 * 估计偏高只会多抓几个吧，偏低由 [MAX_STALENESS_MS] 兜底。
 */
internal object ForumFreshness {
    const val DAY_MS = 24 * 3_600_000.0
    const val MAX_STALENESS_MS = 6 * 3_600_000L
    const val MIN_GAP_MS = 20_000L

    fun ratePerDay(f: ForumState): Double = maxOf(f.dayThreads.toDouble(), f.perDay ?: 0.0)

    /** 这个吧现在该不该重抓第一页。 */
    fun isDue(f: ForumState, now: Long): Boolean {
        if (f.isClosed(now)) return false
        if (f.headFetchedAt <= 0L || f.items.isEmpty() && !f.exhausted) return true
        val elapsed = now - f.headFetchedAt
        if (elapsed < MIN_GAP_MS) return false
        if (elapsed >= MAX_STALENESS_MS) return true
        return ratePerDay(f) * elapsed / DAY_MS >= 1.0
    }

    /** 该重抓第一页的吧，活跃的在前（先抓活跃的，万一中途被风控拦下也已经拿到了最要紧的）。 */
    fun due(states: Collection<ForumState>, now: Long): List<Long> =
        states.filter { isDue(it, now) }.sortedByDescending { ratePerDay(it) }.map { it.id }

    /**
     * 从一页帖子（新的在前）估每天新帖数：(条数 - 1) / 最新到最旧的时间跨度。
     * 少于 5 条或者跨度为 0（同一秒发的）时估不出来，返回 null。
     */
    fun observedPerDay(newestFirst: List<FeedItem>): Double? {
        if (newestFirst.size < 5) return null
        val span = (newestFirst.first().publishedAt ?: return null) - (newestFirst.last().publishedAt ?: return null)
        if (span <= 0) return null
        return (newestFirst.size - 1) * 86_400.0 / span
    }
}
