package feedhub.app.video

import androidx.compose.animation.core.Easing
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.PlayInfo
import feedhub.core.ProgressiveStream
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.VideoStream
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sign
import kotlin.math.sqrt

/**
 * 竖屏视频的沉浸播放里**不带界面**的那部分：
 * 什么时候进沉浸、画面怎么摆、上下滑动的队列和判定、音量控件的换算。[ImmersionScreen] 负责画，逻辑都在这里，方便离线测试。
 *
 * 放哪儿：沉浸界面是"视频详情页"的另一种样子，占详情栏那么大（[feedhub.app.SplitPane] 双栏时就只在右栏里，
 * 全屏打开方式 / 手机竖屏才占满窗口）——这一点不用这里管，界面按被给的尺寸排。
 *
 * **上下滑连续刷**：队列 = 来源列表里的视频条目（[ImmersionQueue]），
 * 只预取下一条，到尾部叫来源列表的 `loadMore()`。
 */

/** 拿不到宽高时按 16:9 当横屏：横屏视频不进沉浸界面，宁可保守（照旧用现在的视频详情页）。 */
private const val LandscapeRatio = 16f / 9f

/** 评论面板从右边滑出时的宽度（[immersionCommentsOnSide] 为 true 时占右栏里这么宽，视频往左让位）。 */
val ImmersionCommentsPanelWidth = 360.dp

/** 沉浸界面顶上返回 / "⋯" 那一行占的高度（4dp 边距 + 48dp 按钮，取整）：画面上方的弹幕带从它下面开始。 */
val ImmersionTopRow = 56.dp

/** 画面上方空白至少这么高（约四行弹幕）才把弹幕挪上去；不够就照旧画在画面里。 */
val ImmersionDanmakuMinBand = 96.dp

/** 右栏窄于这个宽度一律从底下升起半屏评论面板（放不下"竖屏视频 + 评论面板"）。 */
val ImmersionSideCommentsMinPane = 700.dp

/** 视频和评论面板之间留一点缝。 */
private val CommentsGap = 8.dp

/**
 * 这条视频的画面宽高比（宽 / 高）。**拿到流之前**用详情里的分 P / 信息流卡片（封面）给的宽高比，
 * **拿到流之后**（引擎报了解码出来的尺寸）以真实分辨率为准。都没有就是 null（按横屏对待）。
 *
 * @param engineWidth / @param engineHeight 播放器报的真实画面尺寸（[PlayerState.videoWidth] / [PlayerState.videoHeight]），
 *   还没播起来就是 0。
 */
fun immersionAspect(
    item: FeedItem,
    video: VideoInfo? = null,
    info: PlayInfo? = null,
    part: VideoPart? = null,
    engineWidth: Int = 0,
    engineHeight: Int = 0,
): Float? =
    sizeRatio(engineWidth, engineHeight)
        ?: info?.let { sizeRatio(streamSize(it.video)) ?: sizeRatio(progressiveSize(it.progressive)) }
        ?: sizeRatio(part?.width, part?.height)
        ?: sizeRatio(video?.parts?.firstOrNull()?.width, video?.parts?.firstOrNull()?.height)
        ?: item.media.filterIsInstance<Media.Video>().firstOrNull()?.let { sizeRatio(it.width, it.height) }

/** 竖屏（高 > 宽）才用沉浸界面；宽高比不知道时不算竖屏。 */
fun isPortraitVideo(aspect: Float?): Boolean = aspect != null && aspect < 1f

/**
 * 横屏（宽 > 高）：沉浸界面里给不给"横屏全屏"按钮就看它。
 *
 * 竖视频（沉浸界面已经按高度撑满，再放大没有意义）、正方形（宽 = 高）、尺寸还不知道（[aspect] 为 null）
 * 一律 false——宁可先不给按钮，也不给一个点下去没变化 / 把竖视频裁掉的按钮。宽高比的来源和
 * [immersionAspect] 一样（引擎真实分辨率 → 流 → 分 P → 卡片封面），所以"拿到流发现其实是横屏"时按钮会自己出现。
 */
fun isLandscapeVideo(aspect: Float?): Boolean = aspect != null && aspect.isFinite() && aspect > 1f

/** 画面按 [ratio] 适配进 pane 之后的大小：竖屏撑满高、横屏撑满宽，居中（两边 / 上下留出来的地方铺模糊封面）。 */
fun fitVideoSize(paneWidth: Dp, paneHeight: Dp, ratio: Float?): DpSize {
    val r = ratio?.takeIf { it > 0f && it.isFinite() } ?: LandscapeRatio
    if (paneWidth <= 0.dp || paneHeight <= 0.dp) return DpSize(0.dp, 0.dp)
    val byHeight = paneHeight * r
    return if (byHeight <= paneWidth) DpSize(byHeight, paneHeight) else DpSize(paneWidth, paneWidth / r)
}

/**
 * 评论面板在右栏**内部从右边滑出**（视频往左让位），还是从**底下升起**半屏面板。
 * 右栏够宽（能同时放下竖屏视频和 [ImmersionCommentsPanelWidth] 的评论面板）才在右边。
 */
fun immersionCommentsOnSide(paneWidth: Dp, paneHeight: Dp, ratio: Float?): Boolean {
    if (paneWidth < ImmersionSideCommentsMinPane) return false
    val video = fitVideoSize(paneWidth, paneHeight, ratio)
    return paneWidth - ImmersionCommentsPanelWidth >= video.width + CommentsGap
}

// ---------------------------------------------------------------- 上下滑连续刷

/**
 * 上下滑动一次的结果：[Go] = 换到那一条；[End] = 停住（[End.forward] = 是往下滑的那一头；
 * [End.loading] = 已经在叫来源列表加载更多，还没有下一页就是真的到底了，界面提示"已经是最后一条了"）。
 */
sealed interface ImmersionStep {
    data class Go(val item: FeedItem) : ImmersionStep

    data class End(val forward: Boolean, val loading: Boolean) : ImmersionStep
}

/**
 * 上下切换用的队列：**来源列表**（从哪个信息流点进来的）里的视频条目，跳过图文和直播。
 *
 * - 列表**现读**（`list()`），所以信息流 `loadMore()` 之后队列跟着变长；
 * - 当前这条不在来源列表里（收藏 / 历史 / 相关推荐点进来的）时队列只有它自己，[step] 永远返回 [ImmersionStep.End]，
 *   沉浸界面照样画，只是不给上下切；
 * - 只有**往下**滑到尾巴才叫 [loadMore]（往上到头没有"上一页"这回事）。
 *
 * @param loadMore 叫来源列表取下一页，返回"真的发起了"（没有下一页 / 正在取都返回 false）。
 * @param loadingMore 来源列表这一会儿是不是正在取下一页（用来把"正在加载更多"和"已经是最后一条"分开）。
 */
class ImmersionQueue(
    private val list: () -> List<FeedItem>,
    val current: FeedItem,
    private val loadMore: () -> Boolean = { false },
    private val loadingMore: () -> Boolean = { false },
) {
    /**
     * 来源列表里的视频条目（跳过图文 / 直播）。**按列表实例 + 长度缓存**：来源列表（`FeedState.items`）换内容时
     * 整个换成新实例；长度也比一下，原地追加的可变列表（用例里有）也认得出来。
     *
     * 每条都跨一次 FFI 调 `isVideoPost`（JNA 建结构体还要走一遍调用栈），一次重组读十来遍，
     * 每刷一条主线程连着几帧 40～90ms——"竖屏刷视频卡"的大头就是它。只在主线程读（组合 / 手势回调），不加锁。
     */
    private fun videos(): List<FeedItem> {
        val source = list()
        if (source !== cachedSource || source.size != cachedSize) {
            cachedVideos = source.filter { it.isVideoPost() && it.kind != ItemKind.LIVE }
            cachedSource = source
            cachedSize = source.size
        }
        return cachedVideos
    }

    private var cachedSource: List<FeedItem>? = null
    private var cachedSize = -1
    private var cachedVideos: List<FeedItem> = emptyList()

    /** 当前这条在不在来源列表里（收藏 / 历史 / 相关推荐点进来的不在 → 不给切，也不去叫加载更多）。 */
    val inSource: Boolean get() = videos().any { it.key == current.key }

    /** 队列里的条目（跳过图文 / 直播）；当前这条不在里面就退化成只有它自己。 */
    val items: List<FeedItem>
        get() {
            val v = videos()
            return if (v.any { it.key == current.key }) v else listOf(current)
        }

    /** 当前这条在队列里的位置（不在来源列表里是 -1）。 */
    val index: Int get() = if (inSource) items.indexOfFirst { it.key == current.key } else -1

    /** 队列里还有没有上一条 / 下一条（不触发加载更多）。 */
    val hasPrevious: Boolean get() = index > 0
    val hasNext: Boolean get() = index >= 0 && index < items.size - 1

    val next: FeedItem? get() = if (hasNext) items.getOrNull(index + 1) else null
    val previous: FeedItem? get() = if (hasPrevious) items.getOrNull(index - 1) else null

    /**
     * 往下（[delta] = +1）/ 往上（-1）走一步。走到来源列表的尾巴时叫一次 [loadMore]：
     * 发起了 / 正在取就是 [ImmersionStep.End]（loading = true，界面说"正在加载更多"），否则停在最后一条。
     */
    fun step(delta: Int): ImmersionStep {
        if (delta == 0) return ImmersionStep.End(forward = false, loading = false)
        val forward = delta > 0
        // 不在来源列表里（收藏 / 历史 / 相关推荐点进来的）：不给切，也不去叫加载更多
        if (!inSource) return ImmersionStep.End(forward, loading = false)
        val all = items
        val i = all.indexOfFirst { it.key == current.key }
        if (i < 0) return ImmersionStep.End(forward, loading = false)
        all.getOrNull(i + delta)?.let { return ImmersionStep.Go(it) }
        if (!forward) return ImmersionStep.End(forward = false, loading = false)
        return ImmersionStep.End(forward = true, loading = loadMore() || loadingMore())
    }
}

/**
 * 从来源列表里点开一条时，换到下一条**横屏**视频还留在沉浸界面里吗。
 * 只有"这个详情会话已经在沉浸界面里切过条目"才留；从信息流直接点开一条横屏视频照旧是普通视频详情页。
 */
fun immersionStays(portrait: Boolean, switchedInImmersion: Boolean): Boolean = portrait || switchedInImmersion

/** 一次纯位移上下滑要滑过这么多（dp）才算切换（兜底阈值，[immersionSettleStep] 结合速度与屏高比例判）。 */
val ImmersionSwipeThreshold = 56.dp

/** 快速轻拂（Fling）翻页要求同方向至少走过这么多位移（避免手指轻点抬起瞬间的微小速度抖动误翻）。 */
val ImmersionFlingMinDistance = 20.dp

/** 快速轻拂（Fling）翻页的纵向速度门槛（dp/s）：对齐主流短视频大拇指随手一拨即可切条的轻快手感。 */
val ImmersionFlingVelocity = 300.dp

/** 慢速拖动（松手速度不够 Fling）时，需拖过视口高度的这个比例才翻页，否则平滑吸回当前页。 */
const val ImmersionSlowDragFraction = 0.18f

/** 慢速拖动翻页阈值的上限（高屏手机 / 平板不必拖过半尺远）。 */
val ImmersionSlowDragMax = 140.dp

/**
 * 整屏分页（沉浸刷视频 / 大图查看器）松手后的单调减速缓出曲线 `1 - (1 - u)^3`：
 * 起点斜率 `f'(0) = 3` 直接承接手指松手时的运动趋势（绝不像 `FastOutSlowInEasing` 那样先刹停再加速），终点平滑归零无回弹过冲。
 */
val PagerCubicOutEasing = Easing { fraction ->
    val inv = 1f - fraction.coerceIn(0f, 1f)
    1f - inv * inv * inv
}

/**
 * 整屏分页松手动画时长（毫秒）：按剩余路程比例与松手速度动态缩放——
 * 剩余路程越短收尾越快，快速轻拂更干脆利落，慢拖松手平顺滑入。
 */
fun pagerSettleDurationMs(
    distancePx: Float,
    velocityPxPerSec: Float,
    paneSizePx: Float,
    minMs: Int = 95,
    maxMs: Int = 230,
): Int {
    val size = paneSizePx.coerceAtLeast(1f)
    val dist = abs(distancePx)
    if (dist <= 0.5f) return 0
    val distFraction = (dist / size).coerceIn(0f, 1f)
    val baseMs = minMs + (maxMs - minMs) * sqrt(distFraction)
    val speedScreensPerSec = (abs(velocityPxPerSec) / size).coerceIn(0f, 4f)
    val speedFactor = 1f - 0.09f * speedScreensPerSec
    return (baseMs * speedFactor).roundToInt().coerceIn(minMs, maxMs)
}

/** 竖向翻页的方向锁比例：过触摸阈值时 `|dy| >= |dx| * ImmersionAxisLockRatio` 才认领竖向翻页，横向/斜滑整场忽略。 */
const val ImmersionAxisLockRatio = 1.15f

/** 滚轮转多少格算一次切换（桌面滚一格是 1.0）。 */
const val ImmersionWheelThreshold = 1f

/**
 * 手指竖着拖了 [dyPx]（往下为正）之后该切哪一条：-1 = 上一条、+1 = 下一条、0 = 不切（没滑够）。
 * 纯函数，界面和用例共用。
 */
fun immersionSwipeStep(dyPx: Float, thresholdPx: Float): Int = when {
    dyPx <= -thresholdPx -> 1  // 手指往上滑 = 看下一条
    dyPx >= thresholdPx -> -1  // 手指往下滑 = 看上一条
    else -> 0
}

/**
 * 整屏分页器松手结算：结合净位移 [dyPx]（往下为正）与松手纵向速度 [velocityYPxPerSec]（往下为正）判定：
 * 1. **反向轻拂取消**：例如往下拉了一截又快速往上甩回去，速度超过 [flingVelocityPxPerSec] 则取消切换（返回 0）；
 * 2. **同向快速轻拂（Fling）**：同向位移达到 [minFlingDistancePx] 且同向速度达到 [flingVelocityPxPerSec] 即翻页；
 * 3. **慢速拖动**：没有达到轻拂速度时，按视口高度的 [ImmersionSlowDragFraction]（受 [maxSlowDragPx] 封顶，下限 `2.5 * minFlingDistancePx`）判定。
 */
fun immersionSettleStep(
    dyPx: Float,
    velocityYPxPerSec: Float,
    paneHeightPx: Float,
    minFlingDistancePx: Float,
    flingVelocityPxPerSec: Float,
    maxSlowDragPx: Float = Float.POSITIVE_INFINITY,
): Int {
    if (paneHeightPx <= 0f) return immersionSwipeStep(dyPx, minFlingDistancePx)
    // 反向甩回 = 用户反悔取消切换
    if (dyPx < 0f && velocityYPxPerSec >= flingVelocityPxPerSec) return 0
    if (dyPx > 0f && velocityYPxPerSec <= -flingVelocityPxPerSec) return 0
    // 同向快速轻拂
    if (dyPx <= -minFlingDistancePx && velocityYPxPerSec <= -flingVelocityPxPerSec) return 1
    if (dyPx >= minFlingDistancePx && velocityYPxPerSec >= flingVelocityPxPerSec) return -1
    // 慢速拖动：过视口比例（有上限封顶）才翻页
    val slowThreshold = min(paneHeightPx * ImmersionSlowDragFraction, maxSlowDragPx)
        .coerceAtLeast(minFlingDistancePx * 2.5f)
    return immersionSwipeStep(dyPx, slowThreshold)
}

/** 滚轮累计了 [acc] 格之后该切哪一条（[ImmersionWheelThreshold] 一格、向下滚 = 下一条）。 */
fun immersionWheelStep(acc: Float): Int = when {
    acc >= ImmersionWheelThreshold -> 1
    acc <= -ImmersionWheelThreshold -> -1
    else -> 0
}

/**
 * 整屏跟手的位移换算：
 * - [hasNeighbor] = true（那个方向有上一条 / 下一条）：1:1 跟着手指走满整屏 `[-paneHeightPx, +paneHeightPx]`，
 *   相邻那条的预览紧贴着边缘 1:1 推入；
 * - [hasNeighbor] = false（已经是第一条往下拉 / 最后一条往上拉）：给阻尼回弹（最多 15% 屏高），提示到头了。
 */
fun immersionDragOffset(dyPx: Float, paneHeightPx: Float, hasNeighbor: Boolean = true): Float {
    if (paneHeightPx <= 0f) return 0f
    return if (hasNeighbor) {
        dyPx.coerceIn(-paneHeightPx, paneHeightPx)
    } else {
        (dyPx * 0.18f).coerceIn(-paneHeightPx * 0.15f, paneHeightPx * 0.15f)
    }
}

/**
 * 沉浸界面**一场触摸**里"上下翻页到底算不算"的账本（[ImmersionScreen] 的上下滑用它；纯逻辑，离线测得到）。
 *
 * 按**整场触摸**记账：
 * 1. 内层控件消费了**按着状态**的移动（底部进度条横向拖动、音量条、展开文案滚动、评论抽屉）或长按倍速正按着 →
 *    [owned] = true，位移清零、松手返回 0；
 * 2. 给了 [slopPx] > 0 时，先攒位移到过触摸阈值再按 [ImmersionAxisLockRatio] 锁定主轴：
 *    竖向为主才锁进翻页（[locked] = true），横向 / 斜滑为主则标成 [owned] = true 整场忽略，绝不因横滑夹带纵向偏移误切视频。
 */
class ImmersionSwipeTracker(private val slopPx: Float = 0f) {
    /** 跟手的位移（像素，往下为正）。归了别人的时候一直是 0。 */
    var offset: Float = 0f
        private set

    /** 这一场触摸是不是已经归别人（内层控件消费 / 长按倍速 / 起手判为横向手势）。 */
    var owned: Boolean = false
        private set

    /** 这一场触摸是否已经锁定为竖向翻页拖动。 */
    var locked: Boolean = false
        private set

    private var rawDx: Float = 0f
    private var rawDy: Float = 0f

    /**
     * 喂一个"手指按着"的移动事件，返回 true = [offset] 变了（外面要跟着更新跟手位移）。
     *
     * @param deltaY 这一下竖着走了多少（往下为正）
     * @param consumed 这一下有没有被内层控件消费
     * @param speedHeld 长按倍速是不是正按着
     * @param deltaX 这一下横着走了多少（往右为正；用于过阈值时的横竖主轴锁定）
     */
    fun move(deltaY: Float, consumed: Boolean, speedHeld: Boolean, deltaX: Float = 0f): Boolean {
        if (!owned && (speedHeld || consumed)) {
            owned = true
            locked = false
            offset = 0f
            return true // 位移吐回去（外面画回原位）
        }
        if (owned) return false
        if (!locked) {
            if (slopPx <= 0f) {
                locked = true
                offset += deltaY
                return true
            }
            rawDx += deltaX
            rawDy += deltaY
            if (hypot(rawDx, rawDy) <= slopPx) return false
            if (abs(rawDy) >= abs(rawDx) * ImmersionAxisLockRatio) {
                locked = true
                // 扣掉阈值起步，画面跟手第一帧不跳变
                offset = rawDy - sign(rawDy) * min(abs(rawDy), slopPx)
                return true
            } else {
                owned = true
                locked = false
                offset = 0f
                return false
            }
        }
        offset += deltaY
        return true
    }

    /**
     * 松手：这一场该按多少位移结算。归了别人或还没过阈值时一定是 0——**松手也不补触发翻页**。
     */
    fun settle(): Float = if (locked && !owned) offset else 0f

    /** 这一场要不要换条目（无速度简版；带速度的整屏结算见 [immersionSettleStep]）。 */
    fun step(thresholdPx: Float): Int = immersionSwipeStep(settle(), thresholdPx)
}

/** 沉浸界面画面中央的半透明播放键什么时候画：没报错、没在加载或缓冲、当前不在播，且已经出过画面或已播完。 */
fun immersionPausedIndicatorVisible(
    playing: Boolean,
    buffering: Boolean,
    loading: Boolean,
    hasFrame: Boolean,
    ended: Boolean,
    hasError: Boolean,
): Boolean = !hasError && !loading && !buffering && !playing && (hasFrame || ended)

/** 沉浸底部进度条热区里的拖动归谁：横滑 = 调进度，竖滑 = 放行给外层上下切视频（避免大拇指从屏幕底部往上划时误拉进度条）。 */
enum class ImmersionProgressAxis { HorizontalScrub, VerticalPassThrough }

fun immersionProgressGestureAxis(dx: Float, dy: Float, slopPx: Float): ImmersionProgressAxis? {
    if (hypot(dx, dy) <= slopPx) return null
    return if (abs(dx) >= abs(dy)) ImmersionProgressAxis.HorizontalScrub else ImmersionProgressAxis.VerticalPassThrough
}

/** 半屏评论抽屉松手时要不要收起：往下甩（速度够）或往下拖过面板高度约 24% 就收起；反悔往上甩则保持展开。 */
fun commentSheetShouldDismiss(
    dragPx: Float,
    sheetHeightPx: Float,
    velocityYPxPerSec: Float,
    flingVelocityPxPerSec: Float,
): Boolean {
    if (sheetHeightPx <= 0f || dragPx <= 0f) return false
    if (velocityYPxPerSec <= -flingVelocityPxPerSec) return false
    if (velocityYPxPerSec >= flingVelocityPxPerSec && dragPx >= sheetHeightPx * 0.05f) return true
    return dragPx >= sheetHeightPx * 0.24f
}

// ---------------------------------------------------------------- 音量

/** 竖向滑块的高度（[volumeFromSliderY] 的 y 相对它算）。 */
val ImmersionVolumeSliderHeight = 160.dp

/** 竖向滑块上一次按下 / 拖动的位置换算成音量：**上大下小**（0 = 最下面）。 */
fun volumeFromSliderY(y: Float, heightPx: Int): Int {
    if (heightPx <= 0) return 0
    val fromTop = (y / heightPx).coerceIn(0f, 1f)
    return ((1f - fromTop) * 100f).toInt().coerceIn(0, 100)
}

/** 屏幕中间短暂显示的那行字（调音量时）：静音 / 0 就写"静音"，否则写音量值。 */
fun volumeOverlayText(percent: Int, muted: Boolean): String =
    if (muted || percent <= 0) "静音" else "音量 ${percent.coerceIn(0, 100)}%"

// ---------------------------------------------------------------- 宽高比的小工具

private fun sizeRatio(width: Int?, height: Int?): Float? =
    if (width != null && height != null && width > 0 && height > 0) width.toFloat() / height else null

/** 都是正数才算一份能用的尺寸。 */
private fun positiveSize(width: Int?, height: Int?): Pair<Int, Int>? =
    if (width != null && height != null && width > 0 && height > 0) width to height else null

private fun streamSize(streams: List<VideoStream>): Pair<Int, Int>? =
    streams.firstNotNullOfOrNull { positiveSize(it.width, it.height) }

private fun progressiveSize(streams: List<ProgressiveStream>): Pair<Int, Int>? =
    streams.firstNotNullOfOrNull { positiveSize(it.width, it.height) }

private fun sizeRatio(size: Pair<Int, Int>?): Float? = size?.let { sizeRatio(it.first, it.second) }
