package feedhub.brain

import feedhub.core.FeedItem

/**
 * 内容过滤的**第一层**：关键词 + 社区黑名单。
 *
 * 不打码、不折叠**。两份名单：
 * - **屏蔽词**（[Rules.blockWords]）：一直生效。标题 / 摘要 / 转发原文命中就整条不进首页 / 关注；
 * - **外出模式名单**（[Rules.outingKeywords] 关键词 + [Rules.outingCommunities] 社区）：
 *   **只在外出模式开着时**（[Rules.outingMode]）生效；关掉之后"我们自己判出来的"那部分照常显示，
 *   平台标成 18+ 的那部分不受影响（那是 [AdultFilter]，一直隐藏，见 `FeedItem.adult`）。
 *
 * 把 18+ 一起判掉）。
 * 口径（全半角 / 大小写 / 空白压缩 / 去重 / 长度上限）只该有一份。
 * 归一化是**用户动作**（打开设置页、读一次存档、点一次"添加"），不是每次重组的路径；
 * 设置页那条路用 [normalizeMany] 一次一批，见 `ContentFilterSettings`。
 *
 * 匹配规则：不分大小写、**全半角当同一个字**（`Ａ` = `A`、`：` = `:`、全角空格 = 空格），
 * 连续空白压成一个；关键词是**子串**匹配（"原神"能命中"原神新版本"）；**社区是全名相等**（不分大小写 /
 * 全半角），不做子串匹配，免得"原神"把"原神内鬼吧"也一起藏了。
 * **繁体 / 简体先不做**：要对一份字表（或者平台包），这一版没有，
 *
 * 只在信息流聚合层用（`feedhub.app.feed.FeedState`）；评论区、收藏 / 历史还没接，
 */
object ContentFilter {
    /** 一份名单最多几条、一条最多几个字（和设置页的输入限制同一个数；读存档时也按它收）。 */
    const val MAX_RULES: Int = 200
    const val MAX_RULE_LENGTH: Int = 40

    /**
     * 这次过滤用的规则。字段和设置里那四个一一对应（`AppSettings.blockWords` / `outingMode` /
     * `outingKeywords` / `outingCommunities`），每一批**现读**，
     * 所以设置页改了立刻对下一批生效，不用重建 `FeedItem` 或者刷新整页。
     */
    data class Rules(
        /** 屏蔽词，一直生效。 */
        val blockWords: List<String> = emptyList(),
        /** 外出模式开着没有。 */
        val outingMode: Boolean = false,
        /** 外出模式的关键词，只在外出模式开着时生效。 */
        val outingKeywords: List<String> = emptyList(),
        /** 外出模式的社区（贴吧吧名、Reddit `r/…`、知乎问题标题、酷安话题、抖音话题……），只在外出模式开着时生效。 */
        val outingCommunities: List<String> = emptyList(),
    ) {
        /**
         * 现在这条规则会不会藏东西。**空名单 = 什么都不用做**：调用方据此走"原样返回"的快路径
         * （外出模式关着时，它那两份名单再长也不算数）。
         */
        val isEmpty: Boolean
            get() = blockWords.isEmpty() && (!outingMode || (outingKeywords.isEmpty() && outingCommunities.isEmpty()))
    }

    /** 为什么被藏掉（只给日志 / 调试用；界面上一律"整条隐藏"，不告诉用户是哪条规则）。 */
    enum class Reason(val label: String) {
        BLOCK_WORD("屏蔽词"),
        OUTING_KEYWORD("外出模式关键词"),
        OUTING_COMMUNITY("外出模式社区"),
    }

    /** 命中的那条规则（[rule] 是用户写的原文，方便在调试日志里对是哪一条）。 */
    data class Hit(val reason: Reason, val rule: String)

    /**
     * 这条要不要整条藏掉：命中就返回是哪条规则，没有就是 null。
     *
     * 顺序：屏蔽词（一直生效）→ 外出模式关键词 → 外出模式社区。**外出模式关着时后两项完全不看**
     * （[Rules.outingMode]），所以"关掉外出模式后我们自己判出来的那部分照常显示"。
     *
     * **不看 `FeedItem.adult`**：平台标成 18+ 的一直由 [AdultFilter] 藏，和这里无关。
     * 单条入口（详情页之类）用它；成批的地方用 [visible] / [FeedFilter.shown]（一次一批、不逐条过 FFI）。
     */
    fun hidden(item: FeedItem, rules: Rules): Hit? = Brain.hidden(item, rules)

    /** 这一批里按规则能显示的那些；规则为空时**原样返回**（不复制、不过滤）。 */
    fun visible(items: List<FeedItem>, rules: Rules): List<FeedItem> {
        if (rules.isEmpty || items.isEmpty()) return items
        val shown = Brain.visibleRules(items, rules)
        return List(shown.kept.size) { items[shown.kept[it]] }
    }

    /**
     * 把一条关键词 / 社区名归一成用来比较的样子：去首尾空白、全角转半角、小写、连续空白压成一个。
     * 也用在读设置时给列表去重（`Cat` 和 `cat` 算同一条）。
     *
     * **实现在 Rust**：
     * 原来两边各有一份、靠 `BrainParityTest` 的随机对拍盯着，现在只有 Rust 那一份。
     * 一次一条的入口；一批的用 [normalizeMany]。
     */
    fun normalize(text: String): String = Brain.normalize(text)

    /**
     * 一批文本的归一化（[normalize] 的批量版）：**一次 FFI**，顺序和输入一一对应。
     *
     * 界面上调用它的时候别在重组里逐条调 [normalize]（`WordListEditor` 一次要比输入框 + 200 条已有条目）：
     * 用 `remember(text, words) { ... }` 包住、一次算完。
     */
    fun normalizeMany(texts: List<String>): List<String> = Brain.normalizeMany(texts)

    /**
     * 读存档 / 设置页加一条时把名单收拾干净：去空白、丢掉空的和过长的、按 [normalize] 出来的样子去重、
     * 最多 [MAX_RULES] 条。**保留用户写的原文**（不把 `Ａ` 改成 `A`），只是比较时忽略这些差别。
     *
     * **实现在 Rust**（`brain::filter::normalize_rules`）：`trim()` 是 Kotlin 的真口径
     * （`Char.isWhitespace()` 含 NBSP 类三个字符）、长度按 **UTF-16 码元**算（一个 emoji 算 2），
     * 和原来的 Kotlin 版逐字节一致（`BrainParityTest` 的 500 组随机对拍盯着）。
     */
    fun normalizeRules(words: List<String>): List<String> = Brain.normalizeRules(words)
}
