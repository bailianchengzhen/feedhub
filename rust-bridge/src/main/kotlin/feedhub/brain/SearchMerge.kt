package feedhub.brain

import feedhub.core.FeedItem
import feedhub.core.Platform

/**
 * 跨平台搜索的混排。
 *
 * 这里只做"整批过 FFI + 拿下标回列表"。
 * **App 和 cli 共用这一份**，
 * 所以 cli 的 `search all` 和搜索页是同一套规则，不在 cli 里抄一遍。
 *
 * 语义：
 * - 每家第一页只取 k 条进自己的桶，然后**按平台顺序轮转各取一条**拼成一批；
 * - k = max(2, ceil(目标条数 / 平台数))，但不超过这家自己的一页条数（`pageSize`）；
 * - 平台之间不做"相关度比较"——各家的分数不可比，这里只决定摆放顺序；
 * - 同一批里重复的 `FeedItem.key` 只留第一条，不在平台顺序里的平台不收。
 */
object SearchMerge {
    /** 一批的目标条数。 */
    const val TARGET_BATCH: Int = 20

    /**
     * 每家在**一页里**取几条进桶。
     * @param platforms 当前参与的平台数
     * @param pageSize 这家一页给多少条（请求口径；响应可能更少，桶里以实际条数为准）
     */
    fun quota(platforms: Int, pageSize: Int, target: Int = TARGET_BATCH): Int =
        Brain.quota(platforms, pageSize, target)

    /**
     * 轮转：按 [order] 里平台的顺序，一轮每家取一条，直到所有桶都空。
     * 不在 [order] 里的平台不收（保持 `plugins.toml` 的平台顺序）；同一批里重复的 `FeedItem.key` 只留第一条。
     */
    fun roundRobin(buffers: Map<Platform, List<FeedItem>>, order: List<Platform>): List<FeedItem> {
        // 把各家的桶按平台顺序摊平成一批（桶内的先后就是平台返回的先后），顺序 / 去重交给 Rust
        val flat = ArrayList<FeedItem>()
        for (platform in order) {
            val bucket = buffers[platform] ?: continue
            if (bucket.isEmpty()) continue
            flat += bucket
        }
        if (flat.isEmpty()) return emptyList()
        val picked = Brain.roundRobin(order, flat)
        return List(picked.size) { flat[picked[it]] }
    }
}
