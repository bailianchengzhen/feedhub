package feedhub.app.feed

import androidx.compose.runtime.Immutable
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import feedhub.app.HomeColumnsMode
import feedhub.app.MAX_HOME_COLUMNS
import feedhub.app.MIN_HOME_COLUMNS
import kotlin.math.floor

/** 信息流网格左右内边距之和（[FeedScreen] 的 contentPadding 左右各 12dp）。 */
val FeedGridPadding = 24.dp

/** 双栏时右栏（详情）最窄多宽：拖分隔线时不让详情挤得比这还窄（窗口太窄放不下两头时让给左栏的"至少一张卡片"）。 */
val MinDetailWidth = 480.dp

/** 网格 → 双栏的过渡时长（毫秒）。左栏收窄、详情滑入、卡片滑到新位置都用这个，三样同时开始同时结束。 */
const val PaneAnimMillis = 280

/** 判"够不够放下 n 张"时的容差，单位是一张卡片宽：整数倍的宽度不会因为边距 / 浮点误差被向下取整成少一列。 */
private const val FitEpsilon = 0.05f

/**
 * 「自动」列数用的理想卡片宽度。见 [autoColumns] 里这个数的来历。
 */
val IdealCardWidth = 310.dp

/**
 * **自动**模式下的全宽列数：
 * `列数 = floor((窗口宽度 - 网格内边距) / [IdealCardWidth])`，夹在 [MIN_HOME_COLUMNS]～[MAX_HOME_COLUMNS]。
 *
 * [windowWidth] 传的是**窗口**宽度（不是减掉导航栏之后的信息流栏宽）。
 * 减掉 80dp 导航栏之后再套同一个公式，**没有任何理想宽度能同时满足这两个参考**：
 * 横屏 (1476-24)/W < 5 要 W > 290.4，竖屏 (892-24)/W ≥ 3 要 W ≤ 289.3，刚好差 1dp 多。
 * 按窗口宽度算时交集是 306.6 < W ≤ 316.3，取中间的 [IdealCardWidth] = 310dp：
 * 1557 → floor(1533/310) = 4；973 → floor(949/310) = 3；1280 → floor(1256/310) = 4；420 → 1（被下限抬到 2）。
 *
 * **下限是 2 不是 1**：公式在 420dp 上算出来是 1 列，比原来（窄屏恒 2 列）差，也和参考里的"手机 2 列"不符，
 * 所以窄到只有一列时仍按 2 列排。卡片的实际宽度不由这个数直接决定：见 [twoPaneColumns] / [fullCardWidth]，
 * 那里用的是信息流栏真正的宽度。
 */
fun autoColumns(windowWidth: Dp): Int =
    floor((windowWidth - FeedGridPadding) / IdealCardWidth).toInt().coerceIn(MIN_HOME_COLUMNS, MAX_HOME_COLUMNS)

/**
 * 首页 / 关注全宽时的列数：设置是「自动」就按 [autoColumns] 算（[windowWidth] 传窗口宽度），
 * 是「固定 N 列」就用设置里那个数（防御性地夹回 [MIN_HOME_COLUMNS]～[MAX_HOME_COLUMNS]）。
 */
fun homeColumns(windowWidth: Dp, mode: HomeColumnsMode, fixed: Int): Int =
    if (mode == HomeColumnsMode.AUTO) autoColumns(windowWidth) else fixed.coerceIn(MIN_HOME_COLUMNS, MAX_HOME_COLUMNS)

/**
 * 信息流网格的排法：[columns] 列，并且按 [width] 这么宽来排。
 * 双栏切换的动画里栏的可见宽度在变，网格不能跟着每帧重排，所以由外面（[feedhub.app.SplitPane]）定好列数和排版宽度给它。
 */
@Immutable
class FeedGridSpec(val columns: Int, val width: Dp)

/**
 * 一张全宽卡片的宽度：内容区 [fullWidth]（去掉导航栏和网格内边距）除以全宽时的列数 [columns]。
 * 间距不扣，按"栏宽比"算（[FeedGrid] 的 horizontalArrangement 留 10dp 间距，这一项不进公式）。
 * [columns] 是**已经解析过**的列数（`App.kt` 里用 [homeColumns] 按窗口宽度算好）。
 */
private fun fullCardWidth(fullWidth: Dp, columns: Int): Dp =
    ((fullWidth - FeedGridPadding) / columns).coerceAtLeast(1.dp)

/**
 * 左栏至少有这么宽，就至少排 2 列：和手机竖屏一样，
 * 400dp 上下的一栏排两列卡片是正常的；不然平板竖屏（973dp，全宽 3 列、卡片约 289dp）开双栏时
 * 左栏一半宽只放得下 1 张"全宽那么宽"的卡片，整栏一张大卡片很空。
 */
val TwoColumnFeedWidth = 360.dp

/**
 * 双栏时左栏（信息流）的列数 = 左栏宽度 [feedWidth] 能放下几张"全宽时那么宽的卡片"，向下取整；
 * 左栏不窄于 [TwoColumnFeedWidth] 时至少 2 列，再窄至少 1 列。
 * [fullWidth] 是没有详情时内容区的宽度，[columns] 是全宽时的列数（自动模式已经按窗口宽度算好）。
 * 例：4 列的全宽网格，左栏正好占一半就是 2 列（加了 [FitEpsilon] 的容差，不会因为误差变成 1 列）。
 */
fun twoPaneColumns(feedWidth: Dp, fullWidth: Dp, columns: Int): Int =
    floor(feedWidth / fullCardWidth(fullWidth, columns) + FitEpsilon).toInt()
        .coerceAtLeast(if (feedWidth >= TwoColumnFeedWidth) 2 else 1)

/** 左栏最窄 = 一列：一张全宽卡片的宽度加上网格内边距。 */
fun minFeedWidth(fullWidth: Dp, columns: Int): Dp = fullCardWidth(fullWidth, columns) + FeedGridPadding

/**
 * 拖分隔线时把左栏宽度夹在合理范围：不窄于 [minFeed]（一张卡片），右栏不窄于 [minDetail]。
 * 窗口窄到两头都放不下时先保住左栏的最小宽度（详情会比 [minDetail] 窄一点），但左栏最宽不超过 [total]。
 */
fun clampFeedWidth(feedWidth: Dp, total: Dp, minFeed: Dp, minDetail: Dp = MinDetailWidth): Dp {
    val maxFeed = total - minDetail
    return if (maxFeed <= minFeed) minFeed.coerceAtMost(total) else feedWidth.coerceIn(minFeed, maxFeed)
}
