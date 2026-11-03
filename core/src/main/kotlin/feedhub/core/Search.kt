package feedhub.core

/**
 * 平台侧搜索。**可选能力**，照 [QrLoginSource] / [DeviceCodeLoginSource] 的写法：
 * 插件描述里 `capabilities.search` 非空才算支持，主程序按它分支、不按平台 id 分支
 *
 * 约定：
 * - 只读，不做"搜索历史上报"之类的写操作；搜索结果本身也不入库（点进详情自然记历史）。
 * - 页码 / offset / search_id 全塞进 [Cursor]（不透明字符串），平台自己解释。
 * - 风控信号（403 / 412 / 429 / 验证码 / 平台的错误码 / 空 body）抛 [RiskControlException]，
 *   **不重试、不换参数**。
 * - 每页条数由 [SearchCapability.pageSize] 说明（请求口径），调用方按**响应实际条数**算配额。
 * - 只返回内容类条目（视频 / 文章 / 回答 / 帖子 / 动态）。
 */
interface SearchSource : Source {
    /** 这个平台的搜索能力。必须和描述里的 `capabilities.search` 是同一份（[feedhub.core.plugin.PluginContract] 会查）。 */
    val search: SearchCapability

    /**
     * 搜一页。[cursor] 为 null 是第一次搜索；翻页把它原样还给平台。
     * 返回的 [Page.next] 为 null 表示这个平台没有下一页了。
     */
    suspend fun search(request: SearchRequest, cursor: Cursor? = null): Page<FeedItem>
}

/**
 * 一次搜索的输入。加筛选（时间范围 / 时长 / 吧内）时往这里加**带默认值**的字段，不改接口签名。
 */
data class SearchRequest(
    /** 搜索词（用户输入，原样交给平台）。 */
    val text: String,
    /**
     * 搜什么。平台没有的类型不进 [SearchCapability.kinds]。
     * 平台把 [SearchKind.ALL] 解释成"它的内容类结果"（B站是视频，知乎是回答 / 文章 / 视频，贴吧是主题帖，酷安是动态）。
     */
    val kind: SearchKind = SearchKind.ALL,
    val sort: SearchSort = SearchSort.RELEVANCE,
    /**
     * 圈子名（只有贴吧用）：给了就是**吧内搜索**（`kw` = 这个吧名），不给是**全吧搜索**。
     * cli 可以用 `--community` 指定吧名来验吧内搜索。
     */
    val community: String? = null,
)

/**
 * 搜索结果的种类。
 * 聚合层也不会请求；真要做是另一件事（新的结果模型 + 不进收藏 / 历史）。
 */
enum class SearchKind { ALL, VIDEO, POST, USER, TOPIC }

/**
 * 排序。平台没有的档不进 [SearchCapability.sorts]；主程序把选项收窄到"大家都支持的"。
 */
enum class SearchSort { RELEVANCE, LATEST, HOT }

/**
 * 搜索能力（插件描述和 [SearchSource.search] 共用同一份，不写两遍）。
 *
 * @param kinds 支持搜哪些类型。
 * @param sorts 支持哪些排序档（[SearchSort.RELEVANCE] 必须有）。
 * @param pageSize 这个平台一页给多少条（请求口径：B站 30 / 知乎 20 / 贴吧 30 / 酷安 20 / YouTube 20）。
 *   聚合层按它算配额，也按**响应实际条数**校正。
 * @param needsLogin 不登录就搜不了。
 * @param heavy 重接口：一次搜索要打多个请求（小红书首搜可能 4 个）→ 混排时排在后面、默认可以关掉。
 */
data class SearchCapability(
    val kinds: Set<SearchKind> = setOf(SearchKind.ALL),
    val sorts: Set<SearchSort> = setOf(SearchSort.RELEVANCE),
    val pageSize: Int = 20,
    val needsLogin: Boolean = false,
    val heavy: Boolean = false,
)
