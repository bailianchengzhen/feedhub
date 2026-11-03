package feedhub.app.video

import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** 视频详情评论区的两个标签（参考 BiliPai 平板版的"评论 | 相关推荐"）。 */
enum class VideoTab(val label: String) {
    COMMENTS("评论"),
    RELATED("相关推荐"),
}

/** 播放器最高占详情高度的多少：16:10 的窗口里也要给标题、UP 主、简介和标签栏留出位置。 */
private const val PLAYER_MAX_HEIGHT_FRACTION = 0.56f

/** 播放器区域的高度：整宽 16:9，但不超过详情高度的 [PLAYER_MAX_HEIGHT_FRACTION]（宽了上下不会占满，两侧留黑边）。 */
fun playerHeight(width: Dp, height: Dp): Dp = minOf(height * PLAYER_MAX_HEIGHT_FRACTION, width * 9f / 16f)

/** 相关推荐每张卡片至少这么宽，由此定列数。 */
private val RelatedCardMinWidth = 240.dp

/** 相关推荐的列数：窄屏 2 列，随宽度增加到最多 4 列（[width] 是列表可用宽度）。 */
fun relatedColumns(width: Dp): Int = (width / RelatedCardMinWidth).toInt().coerceIn(2, 4)

/** 分 P 卡片的间距、最小宽度。 */
val PartGap = 8.dp
private val PartMinWidth = 150.dp

/** 分 P 网格的列数：每格至少 [PartMinWidth] 宽，至少 1 列（[width] 是可用宽度）。 */
fun partColumns(width: Dp): Int = ((width + PartGap) / (PartMinWidth + PartGap)).toInt().coerceAtLeast(1)

/** 分 P 折叠时露出几个：窄屏摆两行，列数多（≥ 6，一行就很长了）的宽屏只摆一行，免得把评论标签挤到屏幕底下。 */
fun partsCollapsed(columns: Int): Int = columns * if (columns >= 6) 1 else 2

/**
 * 标签栏是不是已经钉在列表顶上：标签栏所在的条目（下标 [headerIndex]）滚出了屏幕顶端，
 * 即第一个可见条目排在它后面，或者就是它自己但已经往上滚了一截。
 * 这时候换标签要回到标签栏处，免得新标签停在中间。
 */
fun tabsPinned(firstVisibleIndex: Int, firstVisibleOffset: Int, headerIndex: Int): Boolean =
    firstVisibleIndex > headerIndex || (firstVisibleIndex == headerIndex && firstVisibleOffset > 0)
