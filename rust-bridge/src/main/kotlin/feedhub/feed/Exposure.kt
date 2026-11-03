package feedhub.feed

import feedhub.core.FeedItem
import feedhub.core.Platform

/** 卡片要有多少比例落在视口里才算"看到了"（与 Rust `feed::exposure::EXPOSURE_MIN_VISIBLE_FRACTION` 同值）。 */
const val EXPOSURE_MIN_VISIBLE_FRACTION: Float = 0.5f

/** 要连续可见多久（毫秒）才算一次曝光（与 Rust `feed::exposure::EXPOSURE_MIN_VISIBLE_MS` 同值）。 */
const val EXPOSURE_MIN_VISIBLE_MS: Long = 1_000L

/** 兼容保留的采样间隔常量（界面侧已改为 `snapshotFlow` 事件驱动，静止 0 唤醒）。 */
const val EXPOSURE_TICK_MS: Long = 500L

/** 这一刻屏幕上的一条内容：[visibleFraction] 是它落在视口里的高度比例（0~1）。 */
data class VisibleItem(val item: FeedItem, val visibleFraction: Float)

/**
 * 一条卡片落在视口里的高度比例（0~1）。
 *
 * 瀑布流每列高度不一样，所以按**卡片自己的位置和尺寸**算，不能按行算。卡片宽度就是所在列的宽度、
 * 横向完整可见，所以高度比例就是面积比例。[itemTop] / [viewportTop] / [viewportBottom] 在同一个坐标空间
 * （列表的滚动坐标）。与 `feed::exposure::visible_fraction` 逐行同构，纯 Kotlin 计算免逐帧过 FFI。
 */
fun visibleFraction(itemTop: Int, itemHeight: Int, viewportTop: Int, viewportBottom: Int): Float {
    if (itemHeight <= 0 || viewportBottom <= viewportTop) return 0f
    val top = if (itemTop > viewportTop) itemTop else viewportTop
    val bottom = if (itemTop + itemHeight < viewportBottom) itemTop + itemHeight else viewportBottom
    val visible = bottom - top
    if (visible <= 0) return 0f
    val fraction = visible.toFloat() / itemHeight.toFloat()
    return if (fraction > 1f) 1f else fraction
}

/** 一个可见项在网格里的位置：下标（用来找回 [FeedItem]）、顶边、高度。抽出来是为了能脱离 Compose 测。 */
data class VisibleEntry(val index: Int, val top: Int, val height: Int)

/**
 * 从"哪些下标在屏幕上、各自在哪、多高"算出可见的卡片。
 * 下标对不上内容的（网格末尾那个整行的 footer、历史页的日期分组头）直接跳过。
 * 只遍历屏幕可见项（O(visible)），不再把整张 `items` 列表过一遍 FFI（修复 E7）。
 */
fun visibleItems(
    entries: List<VisibleEntry>,
    items: List<FeedItem?>,
    viewportTop: Int,
    viewportBottom: Int,
): List<VisibleItem> {
    if (entries.isEmpty()) return emptyList()
    val out = ArrayList<VisibleItem>(entries.size)
    for (entry in entries) {
        if (entry.index !in items.indices) continue
        val item = items[entry.index] ?: continue
        val fraction = visibleFraction(entry.top, entry.height, viewportTop, viewportBottom)
        if (fraction > 0f) {
            out += VisibleItem(item, fraction)
        }
    }
    return out
}

/**
 * **行式**网格里现在可见的卡片：`LazyColumn` 的一项里塞一整行卡片（视频详情的"相关推荐"），
 * 整行的可见比例当作行内每张卡片的可见比例（卡片横向完整可见）。[entries] 的 [VisibleEntry.index] 是**行号**。
 */
fun relatedVisibleItems(
    entries: List<VisibleEntry>,
    rows: List<List<FeedItem>>,
    viewportTop: Int,
    viewportBottom: Int,
): List<VisibleItem> {
    if (entries.isEmpty()) return emptyList()
    val out = ArrayList<VisibleItem>(entries.size * 2)
    for (entry in entries) {
        if (entry.index !in rows.indices) continue
        val fraction = visibleFraction(entry.top, entry.height, viewportTop, viewportBottom)
        if (fraction > 0f) {
            for (card in rows[entry.index]) {
                out += VisibleItem(card, fraction)
            }
        }
    }
    return out
}

/**
 * 行为数据的接线结果：就是 [ExposureTracker] 要的两个入参（构造时的 `enabled` 和 [ExposureTracker.update] 的 `foreground`）。
 */
data class ExposureInputs(val enabled: Boolean = true, val foreground: Boolean = true)

/**
 * 把界面那三个原始输入折成 [ExposureInputs]——**B2 的接线就这一处**。
 * [recordBehavior] 原样进 `enabled`；[covered] / [windowFocused] 都进 `foreground = false`，也就是**结算**，不是**丢弃**。
 */
fun exposureInputs(recordBehavior: Boolean, covered: Boolean, windowFocused: Boolean): ExposureInputs =
    ExposureInputs(enabled = recordBehavior, foreground = !covered && windowFocused)

/** 一次结算要写的那几行（宿主拿去写资料库）。 */
data class Settled(val key: String, val platform: Platform, val startedAtMs: Long, val dwellMs: Long, val lastSeenAtMs: Long)

/**
 * 曝光的判定和停留计时（与 Rust `feed::exposure::ExposureTracker` 完全同构，免滚动路径细粒度 FFI）。
 *
 * 卡片可见面积 ≥ [EXPOSURE_MIN_VISIBLE_FRACTION] 且**连续** ≥ [EXPOSURE_MIN_VISIBLE_MS] 才算一次曝光，
 * 从达标那一刻起累计停留，到不再达标（滚出视口 / 被盖住 / 窗口失焦）时结算。
 */
class ExposureEngine : AutoCloseable {
    private class Tracked(
        val key: String,
        val platform: Platform,
        val startedAtMs: Long,
        var lastAtMs: Long,
        var dwellMs: Long,
    )

    private val tracked = LinkedHashMap<String, Tracked>()

    /**
     * 更新当前可见的卡片集合，返回当前要写库的几行。
     *
     * [enabled] = false 时**丢弃**（用户不想记了，已经在跟踪的直接丢掉、不写库）；
     * [foreground] = false 时**结算**（正在看的这一段够门槛就写）。
     */
    fun update(visible: List<VisibleItem>, enabled: Boolean, foreground: Boolean, nowMs: Long): List<Settled> {
        if (!enabled) {
            tracked.clear()
            return emptyList()
        }
        val active = LinkedHashMap<String, Platform>(visible.size)
        if (foreground) {
            for (v in visible) {
                if (v.visibleFraction < EXPOSURE_MIN_VISIBLE_FRACTION) continue
                active.putIfAbsent(v.item.key, v.item.platform)
            }
        }
        val settled = ArrayList<Settled>()
        val iter = tracked.entries.iterator()
        while (iter.hasNext()) {
            val entry = iter.next().value
            val elapsed = nowMs - entry.lastAtMs
            if (elapsed > 0) entry.dwellMs += elapsed
            entry.lastAtMs = nowMs
            if (entry.key !in active) {
                iter.remove()
                if (nowMs - entry.startedAtMs >= EXPOSURE_MIN_VISIBLE_MS) {
                    settled += Settled(
                        key = entry.key,
                        platform = entry.platform,
                        startedAtMs = entry.startedAtMs,
                        dwellMs = entry.dwellMs,
                        lastSeenAtMs = nowMs,
                    )
                }
            }
        }
        for ((key, platform) in active) {
            if (key !in tracked) {
                tracked[key] = Tracked(
                    key = key,
                    platform = platform,
                    startedAtMs = nowMs,
                    lastAtMs = nowMs,
                    dwellMs = 0L,
                )
            }
        }
        return settled
    }

    /** 界面离开组合（切页签、关窗口）时调：把还在跟踪的按当前时刻结算。 */
    fun settleAll(nowMs: Long): List<Settled> {
        if (tracked.isEmpty()) return emptyList()
        val settled = ArrayList<Settled>(tracked.size)
        for (entry in tracked.values) {
            val elapsed = nowMs - entry.lastAtMs
            val dwell = entry.dwellMs + if (elapsed > 0) elapsed else 0L
            if (nowMs - entry.startedAtMs >= EXPOSURE_MIN_VISIBLE_MS) {
                settled += Settled(
                    key = entry.key,
                    platform = entry.platform,
                    startedAtMs = entry.startedAtMs,
                    dwellMs = dwell,
                    lastSeenAtMs = nowMs,
                )
            }
        }
        tracked.clear()
        return settled
    }

    fun trackedCount(): Int = tracked.size

    override fun close() {
        tracked.clear()
    }
}
