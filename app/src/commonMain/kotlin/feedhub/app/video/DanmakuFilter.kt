package feedhub.app.video

import feedhub.core.Danmaku
import feedhub.player.DanmakuFilter as RustFilter
import feedhub.player.RuleKind
import feedhub.player.RustDanmaku

/**
 * 弹幕进时间线时做的过滤和合并。
 *
 * 顺序照 BiliPai：丢掉不能显示的（高级弹幕等 `OTHER`、空文字）→ 按类型 / 颜色 / 权重 / 屏蔽词过滤 →
 * 顶部底部转滚动 → 按时间排序 → 合并重复。**一段一次过 FFI**（不是一条一次）。
 *
 * 这个类型是 Rust `Filter` 的 Kotlin 面（住在 `:rust-bridge` 的 `feedhub.player`）：`stream` 那边要它当
 * `DanmakuSettings.filter` 的返回类型，`app` 这边只 import 一个 typealias。原版在
 * `app/src/desktopTest/.../video/legacy/DanmakuFilter.kt` 当对拍参照。
 */
typealias DanmakuFilter = RustFilter

/**
 * 编译好的屏蔽词（Rust `BlockRules` 的门面）：一条屏蔽词是关键词，或者正则（`regex:第\d+集` /
 * `re:第\d+集` / `/第\d+集/`，照 BiliPai）。
 *
 * 正则只在**编译时**编译一次；匹配用 `is_match`（非锚定、不分大小写）；编译不了、或者会匹配空文字的规则
 * 当成无效（那会屏蔽所有弹幕）。这些判定都在 Rust。
 */
class BlockRules private constructor(
    /** 编译前的原文（Rust 那边解析成关键词 / 正则；这里留着是为了"和现在的一样就不重新编译"）。 */
    val rules: List<String>,
) {
    /**
     * [text] 要不要被屏蔽。
     *
     * **判定在 Rust**（`BlockRules::blocks`）：关键词不分大小写地包含、正则非锚定匹配。这里只做一次
     * "一条弹幕"的过滤调用（`process` 会连"空文字"一起判掉，所以顺带覆盖了 Kotlin 原来那条 `text.isBlank()`）。
     */
    fun blocks(text: String): Boolean = RustDanmaku.blocks(text, rules)

    /** 一条都编译不出来（不屏蔽任何弹幕）。 */
    val isEmpty: Boolean get() = rules.isEmpty()

    companion object {
        /** 不屏蔽任何弹幕。 */
        val NONE = BlockRules(emptyList())

        /**
         * 编译一批屏蔽词。
         *
         * 判定（关键词 / 正则 / 无效）和去空白都在 Rust；这里只把"一条都编译不出来"的那些（空串、纯空白、
         * 写坏的正则、会匹配空文字的正则）**按同样的口径**滤掉——`isEmpty` 是生产路径会读的（设置页
         * 显示"屏蔽词 N 条"），所以不能只留原文。
         */
        fun compile(rules: List<String>): BlockRules = BlockRules(
            rules.filter { it.isNotBlank() && kindOf(it) != RuleKind.INVALID },
        )

        /** [text] 要不要被这批屏蔽词挡住（不用先编译：[rules] 直接给原文）。 */
        fun blocks(rules: List<String>, text: String): Boolean = RustDanmaku.blocks(text, rules)

        /** 一条屏蔽词算哪种（设置面板里给用户看的提示用）。 */
        fun kindOf(rule: String): RuleKind = RustDanmaku.ruleKind(rule)

        /** 一条屏蔽词算哪种，按原文（`DanmakuPanel` 的列表用）。 */
        fun kindsOf(rules: List<String>): List<RuleKind> = rules.map(::kindOf)
    }
}
