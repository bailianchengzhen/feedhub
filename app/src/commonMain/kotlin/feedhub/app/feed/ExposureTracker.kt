package feedhub.app.feed

import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.snapshotFlow
import feedhub.app.library.ExposureSurface
import feedhub.app.library.Library
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import feedhub.feed.EXPOSURE_MIN_VISIBLE_FRACTION
import feedhub.feed.EXPOSURE_MIN_VISIBLE_MS
import feedhub.feed.EXPOSURE_TICK_MS
import feedhub.feed.VisibleEntry
import feedhub.feed.VisibleItem
import feedhub.feed.exposureInputs

/**
 * 网格当前的可见情况（Compose 那侧的入口，逻辑在 [feedhub.feed.visibleItems]）。
 *
 * 名字带 `grid` 前缀：`feedhub.feed.visibleItems` 是同一份逻辑的**纯函数**入口（不碰 Compose），
 * 同名同签名在同一个包里会撞（Kotlin 的重载解析会把两边的参数弄混）。
 *
 * **留在宿主**：`LazyStaggeredGridState.layoutInfo` 是框架接口（测量），判定在 Rust。
 */
fun gridVisibleItems(grid: LazyStaggeredGridState, items: List<FeedItem?>): List<VisibleItem> {
    val info = grid.layoutInfo
    val entries = info.visibleItemsInfo.map { VisibleEntry(it.index, it.offset.y, it.size.height) }
    return feedhub.feed.visibleItems(entries, items, info.viewportStartOffset, info.viewportEndOffset)
}

/**
 * 同上，从 [LazyListState] 读（视频详情的"相关推荐"）。行的 key 必须是 [keyPrefix] + 行号。
 * 认不出行号的项（信息、分 P、粘性标签栏）跳过。
 *
 * **留在宿主**：`LazyListState.layoutInfo` 是框架接口（测量），判定在 Rust。
 */
fun listVisibleRelatedItems(list: LazyListState, rows: List<List<FeedItem>>, keyPrefix: String = "related-"): List<VisibleItem> {
    val info = list.layoutInfo
    val entries = info.visibleItemsInfo.mapNotNull { item ->
        val row = (item.key as? String)?.takeIf { it.startsWith(keyPrefix) }?.removePrefix(keyPrefix)?.toIntOrNull()
            ?: return@mapNotNull null
        VisibleEntry(row, item.offset, item.size)
    }
    return feedhub.feed.relatedVisibleItems(entries, rows, info.viewportStartOffset, info.viewportEndOffset)
}

/**
 * 曝光的判定和停留计时：卡片可见面积 ≥ [EXPOSURE_MIN_VISIBLE_FRACTION] 且**连续** ≥ [EXPOSURE_MIN_VISIBLE_MS]
 * 才算一次曝光，从达标那一刻起累计停留，到不再达标（滚出视口 / 被盖住 / 窗口失焦）时结算，写进 [Library.recordExposure]。
 *
 *
 * 这个类只剩**宿主那三件事**：
 * 1. **写资料库**（`Library.recordExposure` / `recordSeenItem`）——那是宿主的 IO；
 * 2. **"这一条是哪一批、平台第几条"**（[origin] 查 `FeedState.originOf`，`seen_item` 用）；
 * 3. `enabled` 是设置开关的**现读 lambda**（用户中途关掉就立刻停）。
 *
 * 纯逻辑、不依赖 Compose：界面每隔 [EXPOSURE_TICK_MS] 调一次 [update]（界面侧的循环见 [ExposureTicker]）。
 *
 * ## 几个取舍（Rust 那边的口径，这里只是接线）
 *
 * - **[enabled] 只表示设置里的开关**（[feedhub.app.AppSettings.recordBehavior]）。关掉时不再跟踪，已经在跟踪的
 *   直接丢掉、不写库——这是"用户不想记了"的语义。
 * - **被全屏详情盖住 / 切页签 / 窗口失焦**都走 `foreground = false`，语义是**结算**（见 [exposureInputs]）。
 * - **曝光过的卡片内容**：结算时除了曝光 / 停留，还会写一条 `Library.recordSeenItem`
 *   （完整条目 + 哪一批 + 平台原始顺序第几条）——**只在有"批"的面**（首页 / 关注；靠 [origin] 查来源）。
 *   同一个 key 只写第一次，18+ 不写。
 *
 * ## 已知局限
 *
 * 用户在右栏看详情时左栏卡片照样算可见（`covered = false`），
 *   左栏的停留时长会虚增。要修得先定义清楚"双栏时左栏算不算在看"（按焦点降权还是停表），这属于产品决定。
 */
class ExposureTracker(
    private val library: Library,
    /** 这次曝光发生在哪个面（首页 / 关注 / 详情 / 收藏 / 历史 / 相关推荐），见 [ExposureSurface]。 */
    private val surface: ExposureSurface = ExposureSurface.HOME,
    internal val enabled: () -> Boolean,
    private val clock: () -> Long = System::currentTimeMillis,
    /**
     * 首页 / 关注页：这一条是**哪一批、在它平台的原始顺序里排第几**（`FeedState.originOf`）。
     * null / 查不到 = 这个面没有"批"的概念（详情 / 收藏 / 历史 / 相关推荐），**不写** `seen_item`。
     */
    private val origin: ((String) -> ItemOrigin?)? = null,
    /** 写 `seen_item` 时记的流（首页 [FeedKind.RECOMMEND] / 关注 [FeedKind.FOLLOWING]）；null = 不写（见 [origin]）。 */
    private val feedKind: FeedKind? = null,
) : AutoCloseable {
    private val engine = feedhub.feed.ExposureEngine()

    /**
     * 正在跟踪的那些条目（结算时按 key 找回整条：Rust 只回 key / 平台）。
     *
     * **必须跨 [update] 累积**：结算发生在"这一条不再可见"的那一轮，那一轮的 [visible] 里已经没有它了
     * （最常见的就是滚出视口、或者切后台时喂进来的空列表），拿当前这轮去查一定是空。
     * 结算过的 key 立刻删掉，所以大小和 Rust 那边的跟踪表一样，只跟"屏幕上同时可见几条"有关。
     */
    private val seen = LinkedHashMap<String, FeedItem>()

    fun update(visible: List<VisibleItem>, foreground: Boolean) {
        val active = enabled()
        if (!active) {
            // 用户关掉了：Rust 那边会清空跟踪表、不写库
            engine.update(emptyList(), enabled = false, foreground = foreground, nowMs = clock())
            seen.clear()
            return
        }
        for (v in visible) seen[v.item.key] = v.item
        val settled = engine.update(visible, active, foreground, clock())
        settle(settled)
    }

    /** 界面离开组合（切页签、关窗口）时调：把还在跟踪的按当前时刻结算，别把这最后一次停留丢了。 */
    fun settleAll() {
        if (!enabled()) {
            return
        }
        settle(engine.settleAll(clock()))
        seen.clear()
    }

    override fun close() {
        seen.clear()
        engine.close()
    }

    /**
     * 结算：**曝光 / 停留照旧**写进 `Library.recordExposure`；再在**同一个时机**写"曝光过的卡片内容"
     * （`Library.recordSeenItem`）——只有信息流（首页 / 关注）的卡片知道自己是哪一批、平台第几条。
     */
    private fun settle(settled: List<feedhub.feed.Settled>) {
        for (row in settled) {
            library.recordExposure(
                row.key,
                row.platform,
                row.startedAtMs,
                row.dwellMs,
                surface = surface,
                lastSeenAt = row.lastSeenAtMs,
            )
            val item = seen.remove(row.key) ?: continue
            val kind = feedKind ?: continue
            val who = origin?.invoke(row.key) ?: continue
            if (item.platform != row.platform) continue
            library.recordSeenItem(item, kind, row.startedAtMs, who.batchId, who.platformRank)
        }
    }
}

private class QualifyingFrame(
    val enabled: Boolean,
    val foreground: Boolean,
    val keys: List<String>,
    val items: List<VisibleItem>,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is QualifyingFrame) return false
        return enabled == other.enabled && foreground == other.foreground && keys == other.keys
    }

    override fun hashCode(): Int {
        var result = enabled.hashCode()
        result = 31 * result + foreground.hashCode()
        result = 31 * result + keys.hashCode()
        return result
    }
}

/**
 * 界面侧的曝光跟踪（修复 E7：事件驱动，静止阅读时 **0 定时器唤醒**）：
 * 只在 `enabled`、`foreground` 或"可见比例 ≥ 50% 的卡片集合"真的变化时才通知 [tracker]，离开组合时结算并释放。
 */
@Composable
fun ExposureTicker(tracker: ExposureTracker, foreground: Boolean, visible: () -> List<VisibleItem>) {
    val current = rememberUpdatedState(visible)
    val fg = rememberUpdatedState(foreground)
    LaunchedEffect(tracker) {
        snapshotFlow {
            val isEnabled = tracker.enabled()
            val isFg = fg.value
            if (!isEnabled || !isFg) {
                QualifyingFrame(isEnabled, isFg, emptyList(), emptyList())
            } else {
                val raw = current.value()
                val seenKeys = ArrayList<String>(raw.size)
                val qualifying = ArrayList<VisibleItem>(raw.size)
                for (v in raw) {
                    if (v.visibleFraction < EXPOSURE_MIN_VISIBLE_FRACTION) continue
                    if (v.item.key in seenKeys) continue
                    seenKeys += v.item.key
                    qualifying += v
                }
                QualifyingFrame(true, true, seenKeys, qualifying)
            }
        }.distinctUntilChanged().collect { frame ->
            tracker.update(frame.items, frame.foreground)
        }
    }
    // 离开组合（切页签、关窗口、换了一条内容）：把还在跟踪的这段停留结算掉并释放句柄
    DisposableEffect(tracker) {
        onDispose {
            tracker.settleAll()
            tracker.close()
        }
    }
}

/**
 * 曝光循环的骨架：**后台 / 被盖住时挂起等回前台**，不再每 500 ms 空转一次。
 *
 * 语义没变：[onTick] 在 [foreground] 为 false 时收到的也是 `false`——那边是"结算"，不是丢弃。
 * 所以失焦 / 后台时**先喂一次 false 结算**，然后一直挂着等 `foreground` 变回 true 再开始计时；
 * [snapshotFlow] 只在状态真的变了才醒，不是轮询。
 *
 * **留在宿主**：它是定时器 + 协程（`delay` / `snapshotFlow`），
 */
suspend fun exposureLoop(foreground: () -> Boolean, tickMs: Long, onTick: (foreground: Boolean) -> Unit) {
    while (true) {
        val fg = foreground()
        onTick(fg)
        if (!fg) {
            // 后台：挂起，等真的回到前台（不占计时器、不唤醒）
            snapshotFlow { foreground() }.first { it }
            continue
        }
        delay(tickMs)
    }
}
