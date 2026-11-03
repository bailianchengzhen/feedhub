package feedhub.brain

import feedhub.core.FeedItem
import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog

/**
 * 跨平台搜索的**规则**。
 *
 * 这里只放纯逻辑（挑哪些平台、每家取几条、哪些条目能上屏），没有任何 UI / 状态：
 * App 的 `SearchState` 负责超时、并发、点灭标签这些交互；cli 负责串行请求和输出。
 * 规则本身只写一份，两边跑出来的是同一个集合（`SearchPlanTest` 守着）。
 *
 * 而 `:core` 在依赖顺序上在 `:rust-bridge` 下面
 * （桥接层依赖 core，不能倒过来），所以这条规则跟着"首页大脑"的门面一起放。
 *
 * 口径：**只搜「首页点亮且支持搜索」的平台**；
 * 每家一次搜索最多 [PAGE_BUDGET] 页；18+ / 内容过滤（屏蔽词 / 外出模式）在**进列表之前**过。
 */
object SearchPlan {
    /** 每家一次搜索最多翻几页。 */
    const val PAGE_BUDGET: Int = 3

    /** cli 是串行的，用不到，但两边记的是同一个数。 */
    const val READY_PLATFORMS: Int = 3

    /**
     * 这次能搜哪些平台：按 [PlatformCatalog]（= `plugins.toml`）的顺序，
     * 只留「首页点亮（[enabled]）」+「描述里声明了 `capabilities.search`」+「这一端能用（[available]）」的。
     *
     * 和 `AppGraph.searchPlatforms()` / `searchSources()` 是同一条规则；
     * [available] 在 App 里是"装了、这一端跑得了、没被标成暂不可用"，cli 里是同一份（桌面端全可用）。
     */
    fun platforms(
        catalog: PlatformCatalog,
        enabled: Set<Platform>,
        available: Set<Platform>,
    ): List<Platform> = catalog.plugins
        .filter { it.platform in enabled && it.capabilities.search != null && it.platform in available }
        .map { it.platform }

    /** 每家在一页里取几条进桶（转发 [SearchMerge.quota]，调用方少 import 一个对象）。 */
    fun quota(platforms: Int, pageSize: Int): Int = SearchMerge.quota(platforms, pageSize)

    /**
     * 进列表之前过一遍：平台标成 18+ 的整条去掉（[AdultFilter]），再按内容过滤第一层去掉
     * （[ContentFilter]：屏蔽词一直生效；外出模式关键词 / 社区只在开着时生效）。
     * 和 `FeedState.kept` / `SearchState.kept` 是同一条规则（顺序也一致：先 18+，再内容过滤），
     * 现在两边是**同一个 Rust 函数**（`brain_filter_shown`，一次一批）。
     */
    fun keep(items: List<FeedItem>, rules: ContentFilter.Rules): List<FeedItem> {
        if (items.isEmpty()) return items
        return FeedFilter.shown(items, rules).items
    }
}
