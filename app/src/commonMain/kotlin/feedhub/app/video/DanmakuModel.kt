package feedhub.app.video

import feedhub.core.Danmaku
import feedhub.core.DanmakuMode
import feedhub.player.DanmakuFilter
import feedhub.player.DanmakuLayout
import feedhub.player.DanmakuPlan
import feedhub.player.PlannedDanmaku
import feedhub.player.RustDanmakuLoader
import feedhub.player.RustDanmakuScheduler
import feedhub.player.RustDanmakuTimeline
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * 弹幕的数据和排布。
 *
 * 这里只剩宿主必须做的两件事：
 * 1. **量文字宽度**（Compose `TextMeasurer` / `android.graphics.Paint`——那是渲染，见 [DanmakuTimeline.setMeasurer]）；
 * 2. **每帧算 x / y**（纯算术，不过 FFI，见 [ActiveDanmaku.x] / [DanmakuScheduler.y]）。
 *
 * 界面部分见 [DanmakuLayer]。流程：[DanmakuLoader] 按播放进度一段一段取弹幕放进 [DanmakuTimeline]；
 * [DanmakuScheduler] 把到点的弹幕分到轨道上（不重叠），再算出每一帧每条弹幕的位置。
 *
 * 三个类的**跨调用状态都在 Rust**（各带一把 `Mutex`），Kotlin 这一侧只多存一份"id → 弹幕"（文字 / 颜色
 * 要给画的那一侧用）和"每段量过的宽度"，那份不参与判定。
 */

/** 弹幕的默认值（真值在 Rust，见 `feedhub.player.DanmakuDefaults`）。 */
typealias DanmakuDefaults = feedhub.player.DanmakuDefaults

/** 排布要用的那几项设置（已换算成像素，Rust 给；见 `feedhub.player.DanmakuLayout`）。 */
typealias DanmakuLayout = feedhub.player.DanmakuLayout

/**
 * 已取回的弹幕，按时间排好、按段存（Rust `Timeline` 的门面）。
 *
 * 弹幕进来（[put]）时就按 [filter] 处理好——类型 / 权重 / 屏蔽词过滤、合并重复——之后只是按时间取，
 * 不再判断。过滤规则变了（[setFilter]）对每一段重新处理一遍。
 *
 * **文字宽度**：宿主量好之后由 [setMeasurer] 装进这条时间线，`put` / `setFilter` 之后自动按段交过去
 * （一段一次）。排布表里因此只放 id + 排布结果，不把每条弹幕的字符串再过一次边界。
 */
class DanmakuTimeline(
    val segmentMs: Long,
    filter: DanmakuFilter = DanmakuFilter.NONE,
) {
    private val inner = RustDanmakuTimeline(segmentMs, filter)
    private val lock = Any()
    private val contentRev = AtomicInteger(0)

    /** 时间线内容或测宽修订号（供 [DanmakuScheduler] 无 FFI 判断是否需要重新同步）。 */
    internal val contentRevision: Int get() = contentRev.get()

    /** 当前过滤规则（宿主判断"要不要 setFilter"用）。 */
    @Volatile
    var filter: DanmakuFilter = filter
        private set

    /** 过滤规则变过几次；[DanmakuScheduler] 看它变了就重新对齐。 */
    val version: Int get() = inner.version()

    /** id → 弹幕（画的那一侧按 id 取文字 / 颜色，跨线程无锁安全读）。 */
    private val texts = ConcurrentHashMap<ULong, Danmaku>()

    /** 这一段的**完整**弹幕列表（按段内下标排好，量宽度的顺序要它）。 */
    private val segmentTexts = ConcurrentHashMap<Int, List<Danmaku>>()

    /** 量文字宽度的口子（界面层给；没有就按 0，排布当"宽度 0"处理，不会崩）。 */
    @Volatile
    private var measurer: ((Danmaku) -> Float)? = null

    /** 量过的宽度（按 id；改规则之后整份清掉重量）。 */
    private val measuredWidths = ConcurrentHashMap<ULong, Float>()

    /** 装一个"量文字宽度"的口子（界面层在创建时给一次）。 */
    fun setMeasurer(measure: (Danmaku) -> Float) {
        synchronized(lock) {
            measurer = measure
            // 已经取回来的段补量一次（先创建的播放器再装量具时不会漏）
            for (segment in segmentTexts.keys.toList()) measureSegmentLocked(segment)
            contentRev.incrementAndGet()
        }
    }

    /** 字号或字重变化后，重新测量已加载的所有段。 */
    internal fun remeasureAll() {
        synchronized(lock) {
            if (measurer == null || segmentTexts.isEmpty()) return
            for (segment in segmentTexts.keys.toList()) measureSegmentLocked(segment)
            contentRev.incrementAndGet()
        }
    }

    /** 按 id 取那条弹幕（画的那一侧用）。 */
    fun danmaku(id: ULong): Danmaku? = texts[id]

    fun has(segment: Int): Boolean = segmentTexts.containsKey(segment) || inner.has(segment)

    /** 放进一段：处理好的列表由 Rust 交回，这里记下文字并按段量宽度。 */
    fun put(segment: Int, list: List<Danmaku>) {
        synchronized(lock) {
            val processed = inner.put(segment, list)
            val prevSize = segmentTexts[segment]?.size ?: 0
            segmentTexts[segment] = processed
            for ((index, d) in processed.withIndex()) {
                texts[danmakuId(segment, index)] = d
            }
            for (index in processed.size until prevSize) {
                val staleId = danmakuId(segment, index)
                texts.remove(staleId)
                measuredWidths.remove(staleId)
            }
            measureSegmentLocked(segment)
            contentRev.incrementAndGet()
        }
    }

    /** 换过滤规则。和现在的一样就什么都不做，返回 false；否则对已取回的每一段重新处理，返回 true（要重排）。 */
    fun setFilter(next: DanmakuFilter): Boolean {
        if (next == filter) return false
        synchronized(lock) {
            if (next == filter) return false
            filter = next
            // Rust 把**重新处理过的每一段**交回来：文字表 / 宽度全部按新内容重建
            val changed = inner.setFilter(next) ?: return false
            val nextTexts = HashMap<ULong, Danmaku>()
            val nextSegments = HashMap<Int, List<Danmaku>>(changed.size)
            for ((segment, list) in changed) {
                nextSegments[segment] = list
                for ((index, d) in list.withIndex()) nextTexts[danmakuId(segment, index)] = d
            }
            texts.clear()
            texts.putAll(nextTexts)
            measuredWidths.clear()
            segmentTexts.clear()
            segmentTexts.putAll(nextSegments)
            for ((segment, _) in changed) {
                measureSegmentLocked(segment)
            }
            contentRev.incrementAndGet()
            return true
        }
    }

    /** 时间 [ms] 落在第几段（从 1 开始，和 B站接口一致）。 */
    fun segmentOf(ms: Long): Int = inner.segmentOf(ms)

    val loadedCount: Int get() = inner.loadedCount()

    /** 出现时间在 ([fromExclusive], [toInclusive]] 里的弹幕 id。 */
    fun rangeIds(fromExclusive: Long, toInclusive: Long): List<ULong> =
        inner.rangeIds(fromExclusive, toInclusive)

    /** 出现时间在 ([fromExclusive], [toInclusive]] 里的弹幕（检查工具 / 测试 / 调试用）。 */
    fun range(fromExclusive: Long, toInclusive: Long): List<Danmaku> =
        rangeIds(fromExclusive, toInclusive).mapNotNull { texts[it] }

    /** 出现时间严格大于 [afterMs] 的最近一条的时刻（排布器定"下一次什么时候来问"用）。 */
    fun nextTimeAfter(afterMs: Long): Long? = inner.nextTimeAfter(afterMs)

    /** 给排布器用的底层对象。 */
    internal fun rust(): RustDanmakuTimeline = inner

    /**
     * 重量某一段的宽度：把 `segmentTexts[segment]` 整段量一遍、按段交过去。
     *
     * 为什么整段重量而不是"只量新的"：Rust 的 `set_widths` 是按段覆盖的，段内下标必须和 `put` 交回的
     * 那份列表对齐。过滤规则变过之后同一个 id 也可能指向另一条，重量最省心（量具那边有 LRU 缓存）。
     */
    internal fun measureSegment(segment: Int) {
        synchronized(lock) {
            measureSegmentLocked(segment)
            contentRev.incrementAndGet()
        }
    }

    private fun measureSegmentLocked(segment: Int) {
        val list = segmentTexts[segment] ?: return
        val measure = measurer ?: return
        val widths = list.map { d -> measure(d) }
        for ((index, w) in widths.withIndex()) {
            measuredWidths[danmakuId(segment, index)] = w
        }
        inner.setWidths(segment, widths)
    }

    internal fun widthOf(id: ULong): Float = measuredWidths[id] ?: 0f

    companion object {
        /** 和 Rust `danmaku_id` 同一个口径：`(段号 << 32) | 段内下标`。 */
        fun danmakuId(segment: Int, index: Int): ULong = (segment.toULong() shl 32) or index.toULong()

        /** 和 Rust `split_id` 同一个口径。 */
        fun splitId(id: ULong): Pair<Int, Int> = (id shr 32).toInt() to (id and 0xFFFF_FFFFuL).toInt()

        /** 探段号时的上限（6 分钟一段的话够 100 小时）。 */
        private const val MAX_SEGMENTS = 1000
    }
}

/**
 * 按播放进度取弹幕：当前这段，和快播完这段时的下一段（提前取，翻段时不断档）。
 *
 * **决策在 Rust**（`Loader`：试几次、隔多久重试、什么时候算到头 / 超出视频长度），这里只剩"取回来塞进
 * 时间线"这一件事。
 */
class DanmakuLoader(
    private val timeline: DanmakuTimeline,
    private val durationMs: Long?,
    private val clock: () -> Long = System::currentTimeMillis,
    private val fetch: suspend (segment: Int) -> List<Danmaku>,
) {
    private val inner = RustDanmakuLoader(timeline.segmentMs, durationMs)

    /**
     * 取 [positionMs] 这一刻需要的段；要取的取完（或失败）才返回。
     * 返回当前**真的取成功**的段号（检查工具 / 测试用）。
     */
    suspend fun ensure(positionMs: Long): List<Int> {
        val wanted = inner.ensure(positionMs, loadedSegments(), clock())
        val done = ArrayList<Int>(wanted.size)
        for (segment in wanted) {
            try {
                val items = fetch(segment)
                withContext(Dispatchers.Default) {
                    timeline.put(segment, items)
                }
                done += segment
            } catch (e: CancellationException) {
                inner.complete(segment)
                throw e
            } catch (_: Exception) {
                // 留给下次重试（次数上限和间隔在 Rust）
            } finally {
                inner.complete(segment)
            }
        }
        return done
    }

    /** 已经有哪几段（给 Rust 的"已有的不再取"用）。 */
    private fun loadedSegments(): Set<Int> {
        val last = durationMs?.takeIf { it > 0 }?.let { timeline.segmentOf(it - 1) } ?: 1_000
        return (1..last).filter { timeline.has(it) }.toSet()
    }

    /** 这一段问过几次（检查工具用）。 */
    fun tries(segment: Int): Int = inner.tries(segment)

    /** 现在有几段在飞。 */
    fun inFlight(): Int = inner.inFlight()

    companion object {
        /** 取失败的段最多试几次（真值在 Rust `danmaku::MAX_TRIES`）。 */
        const val MAX_TRIES = 3

        /** 取失败之后隔多久再试（毫秒，真值在 Rust `danmaku::RETRY_AFTER_MS`）。 */
        const val RETRY_AFTER_MS = 4000L
    }
}

/** 屏幕上的一条弹幕（Rust 的排布结果 + 文字）。 */
class ActiveDanmaku(
    /** 文字 / 颜色：宿主从时间线里按 id 取好了。 */
    val danmaku: Danmaku,
    /** 排布结果（Rust 给的）。 */
    val planned: PlannedDanmaku,
) {
    val mode: DanmakuMode get() = planned.mode
    val lane: Int get() = planned.lane
    val startMs: Long get() = planned.startMs
    val durationMs: Long get() = planned.durationMs

    /** 文字宽度（像素）。 */
    val width: Float get() = planned.width

    /** 排布时视图宽度（像素），滚动弹幕的速度由它定。 */
    val viewWidth: Float get() = planned.viewWidth
    val endMs: Long get() = planned.endMs
    val speed: Float get() = planned.speed

    /**
     * 滚动弹幕此刻左边缘的 x；顶部 / 底部弹幕居中。
     *
     * **每帧每条都调**：不过 FFI，就是 [PlannedDanmaku.x] 那一行（和 Rust 的 `Planned::x` 同一个公式，
     * 由 `DanmakuDualRunTest` 盯着）。
     */
    fun x(nowMs: Long, viewWidth: Float): Float = planned.x(nowMs, viewWidth)
}

/**
 * 把到点的弹幕分到轨道上、算每一帧该画哪些、画在哪。
 *
 * **排布在 Rust**（`Scheduler`）：轨道占用的判据（前一条尾巴进没进屏幕、会不会被追上）、顶部 / 底部分区、
 * 显示区域换算成轨道数、跳转 / 改设置 / 换倍速时的重新对齐——全在那边。
 *
 * 这一侧只负责：
 * 1. 在"有新的弹幕要出现 / 跳转 / 改设置 / 换倍速"时叫一次 Rust（[advance] / [reset]），**不是每帧**；
 * 2. 每帧把 id 还原成 [ActiveDanmaku]（文字从时间线里取）。
 *
 * 所有方法在同一个线程调用（界面线程）；安卓的画线程只读**已经发布出去的那一份**（[active]）。
 */
class DanmakuScheduler(
    private val timeline: DanmakuTimeline,
    /** 弹幕文字的宽度（像素），由界面层测量（有缓存）。装进时间线（一段一次）。 */
    measure: (Danmaku) -> Float,
) {
    private val inner = RustDanmakuScheduler()
    private var layout: DanmakuLayout? = null

    /** 倍速（界面每帧从 [PlayerState.speed] 更新）：决定新出现的滚动弹幕走多快。值未变不过 FFI。 */
    var playbackSpeed = 1f
        set(value) {
            if (field == value) return
            field = value
            inner.setPlaybackSpeed(value)
        }

    var viewWidth = 0f
        private set
    var viewHeight = 0f
        private set
    var laneHeight = 0f
        private set

    /** 弹幕区域一共几条轨道（滚动 + 顶部 + 底部）。 */
    var usableLanes = 0
        private set

    /** 滚动弹幕用的轨道数；它们在画面上的位置从第 [topFixedLanes] 条轨道开始。 */
    var scrollLanes = 0
        private set

    /** 顶部、底部弹幕各自的轨道数。 */
    var topFixedLanes = 0
        private set
    var bottomFixedLanes = 0
        private set

    /** 现在屏上的这些（最近一次 [advance] / [reset] 的结果）。 */
    private var active: List<ActiveDanmaku> = emptyList()

    /** 最近一次排布的那份（每帧读它的 y 口径）。 */
    private var plan: DanmakuPlan = DanmakuPlan.EMPTY

    /** 有没有配置过（没配过时 [advance] 什么都不做）。 */
    private var configured = false

    /** 上一次向 Rust 同步排布的位置（`null` = 还没排过）。 */
    private var lastSyncMs: Long? = null

    /** 上一帧传入的位置（用来识别暂停拖动或跳转）。 */
    private var lastSeenMs: Long? = null

    /** 上一次向 Rust 同步时看到的时间线修订号。 */
    private var seenTimelineRevision: Int = -1

    init {
        timeline.setMeasurer(measure)
    }

    /**
     * 视图尺寸或排布变了就重新排（轨道数量变了，旧的排布作废）；都没变什么也不做。
     * 返回 `true` = 真的变了（宿主据此重画一帧）。
     */
    fun configure(width: Float, height: Float, layout: DanmakuLayout): Boolean {
        if (configured && width == viewWidth && height == viewHeight && layout == this.layout) return false
        val prevLayout = this.layout
        this.layout = layout
        if (prevLayout == null || prevLayout.fontPx != layout.fontPx || prevLayout.fontWeight != layout.fontWeight) {
            timeline.remeasureAll()
        }
        inner.configure(width, height, layout)
        configured = true
        // 从当前进度重新对齐（轨道数变了，已经排上的全作废）
        val anchor = lastSeenMs ?: lastSyncMs ?: 0L
        republish(inner.reset(anchor, timeline.rust()), anchor)
        return true
    }

    /**
     * 播放到了 [nowMs]：把到点的弹幕排上，去掉已经出屏的，返回此刻该画的。
     *
     * **只在"有新的弹幕要出现 / 出屏 / 跳转 / 时间线更新"时过 FFI**（按 [DanmakuPlan.nextMs] 判断），
     * 其余帧直接复用缓存好的 [active]，每帧只做纯 Kotlin 的 x / y 插值。
     * 空档期每隔 [MAX_SILENT_SPAN_MS]（小于 [DanmakuDefaults.JUMP_MS]）同步一次 `last_ms`，避免 Rust 误判成跳转。
     */
    fun advance(nowMs: Long): List<ActiveDanmaku> {
        if (!configured) return emptyList()
        val syncMs = lastSyncMs
        val prevSeen = lastSeenMs
        val curRev = timeline.contentRevision
        val jumped = prevSeen == null || nowMs < prevSeen || nowMs - prevSeen > DanmakuDefaults.JUMP_MS
        val due = plan.nextMs?.let { nowMs >= it } ?: false
        val keepAlive = syncMs == null || nowMs < syncMs || nowMs - syncMs >= MAX_SILENT_SPAN_MS
        if (!jumped && !due && !keepAlive && curRev == seenTimelineRevision) {
            lastSeenMs = nowMs
            return active
        }
        // Rust 按它自己的 last_ms（上一次同步）判跳转；在两次同步之间往回拖不一定越过它，这里显式对齐
        if (jumped) {
            republish(inner.reset(nowMs, timeline.rust()), nowMs)
            return active
        }
        republish(inner.advance(nowMs, timeline.rust()), nowMs)
        return active
    }

    /** 对齐到 [positionMs]：清空，再把它之前一个周期内出现的弹幕按顺序排一遍。 */
    fun reset(positionMs: Long) {
        if (!configured) return
        republish(inner.reset(positionMs, timeline.rust()), positionMs)
    }

    /** 下一次该来找排布器是什么时候（`null` = 后面什么都没有了）。 */
    fun nextMs(nowMs: Long): Long? {
        if (!configured) return null
        val syncMs = lastSyncMs
        val next = plan.nextMs
        if (
            syncMs != null &&
            seenTimelineRevision == timeline.contentRevision &&
            nowMs >= syncMs &&
            (next == null || nowMs < next)
        ) {
            return next
        }
        return inner.nextMs(nowMs, timeline.rust())
    }

    /** 弹幕上边缘的 y。 */
    fun y(a: ActiveDanmaku): Float = plan.y(a.planned)

    /** 现在屏上有几条。 */
    val activeCount: Int get() = active.size

    /** 把 Rust 给的排布表还原成 [ActiveDanmaku]（文字从时间线里取；找不到的跳过）。 */
    private fun republish(next: DanmakuPlan, atMs: Long) {
        plan = next
        lastSyncMs = atMs
        lastSeenMs = atMs
        seenTimelineRevision = timeline.contentRevision
        viewWidth = next.viewWidth
        viewHeight = next.viewHeight
        laneHeight = next.laneHeight
        usableLanes = next.usableLanes
        scrollLanes = next.scrollLanes
        topFixedLanes = next.topFixedLanes
        bottomFixedLanes = next.bottomFixedLanes
        active = next.items.mapNotNull { item ->
            timeline.danmaku(item.id)?.let { ActiveDanmaku(it, item) }
        }
    }

    private companion object {
        /** 空档期同步间隔上限（严小于 Rust `JUMP_MS = 1500`，防止正常播放被误判为 seek 跳转）。 */
        const val MAX_SILENT_SPAN_MS = 1_000L
    }
}
