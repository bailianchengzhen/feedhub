package feedhub.player

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiDanmaku
import uniffi.feedhub_ffi.FfiDanmakuFilter
import uniffi.feedhub_ffi.FfiDanmakuLoader
import uniffi.feedhub_ffi.FfiDanmakuMode
import uniffi.feedhub_ffi.FfiDanmakuScheduler
import uniffi.feedhub_ffi.FfiDanmakuSettings
import uniffi.feedhub_ffi.FfiDanmakuTimeline
import uniffi.feedhub_ffi.FfiRuleKind
import uniffi.feedhub_ffi.danmakuAreaOptions
import uniffi.feedhub_ffi.danmakuBlocked
import uniffi.feedhub_ffi.danmakuFilterSegment
import uniffi.feedhub_ffi.danmakuFixedVelocityFactor
import uniffi.feedhub_ffi.danmakuLayout
import uniffi.feedhub_ffi.danmakuMassiveLineHeight
import uniffi.feedhub_ffi.danmakuMaxRuleLength
import uniffi.feedhub_ffi.danmakuMaxRules
import uniffi.feedhub_ffi.danmakuMerge
import uniffi.feedhub_ffi.danmakuNearestArea
import uniffi.feedhub_ffi.danmakuPlanY
import uniffi.feedhub_ffi.danmakuRuleKind
import uniffi.feedhub_ffi.danmakuScrollMs
import uniffi.feedhub_ffi.danmakuSettingsFilter
import uniffi.feedhub_ffi.danmakuSettingsFromLegacyDensity
import uniffi.feedhub_ffi.danmakuSettingsNormalized
import uniffi.feedhub_ffi.danmakuSettingsReset
import uniffi.feedhub_ffi.plannedX

/**
 * **弹幕层**的 Rust 门面（`rust/crates/feedhub-core/src/player/danmaku/`）。
 *
 * 三段里前两段在 Rust：
 * 1. **进时间线**（[process] / [RustDanmakuTimeline.put]）：过滤 / 合并 / 按时间排，**一段一次**过 FFI；
 * 2. **排轨道**（[RustDanmakuScheduler]）：进时间线时一条一次，**不是每帧**；
 * 3. **画**：宿主的事（Compose `Canvas` / 安卓 `SurfaceView`）；每帧只剩 [x] / [y] 两个纯算术。
 *
 * 同样没有开关：`DanmakuFilter` / `DanmakuTimeline` / `DanmakuLoader` / `DanmakuScheduler` /
 * `DanmakuSettings` 的换算**生产路径都走这一份**，Kotlin 原版只在测试里当对拍参照。
 */
object RustDanmaku {
    /** 过滤一段（`DanmakuFilter.process`）。 */
    fun process(list: List<feedhub.core.Danmaku>, filter: DanmakuFilter): List<feedhub.core.Danmaku> =
        native { danmakuFilterSegment(list.map { it.toFfi() }, filter.toFfi()).map { it.toCore() } }

    /** 合并重复（单独的入口：已经滤好的列表想再合并时用）。 */
    fun merge(
        list: List<feedhub.core.Danmaku>,
        windowMs: Long,
        threshold: Int,
    ): List<feedhub.core.Danmaku> = native {
        danmakuMerge(list.map { it.toFfi() }, windowMs, threshold).map { it.toCore() }
    }

    /** 这条文字会不会被这批屏蔽词挡住（`BlockRules.blocks`）。 */
    fun blocks(text: String, rules: List<String>): Boolean = native { danmakuBlocked(text, rules) }

    /** 一条屏蔽词算哪种。 */
    fun ruleKind(rule: String): RuleKind = native {
        when (danmakuRuleKind(rule)) {
            FfiRuleKind.KEYWORD -> RuleKind.KEYWORD
            FfiRuleKind.REGEX -> RuleKind.REGEX
            FfiRuleKind.INVALID -> RuleKind.INVALID
        }
    }

    // ---------- 设置换算 ----------

    fun layout(settings: DanmakuSettings, viewWidth: Float, viewHeight: Float, density: Float): DanmakuLayout =
        native {
            danmakuLayout(settings.toFfi(), viewWidth, viewHeight, density).let {
                DanmakuLayout(
                    fontPx = it.fontPx,
                    fontWeight = it.fontWeight,
                    laneHeight = it.laneHeight,
                    area = it.area,
                    scrollMs = it.scrollMs,
                    fixedMs = it.fixedMs,
                    reserveTop = it.reserveTop,
                    reserveBottom = it.reserveBottom,
                )
            }
        }

    fun filterOf(settings: DanmakuSettings): DanmakuFilter = native {
        danmakuSettingsFilter(settings.toFfi()).toCore()
    }

    fun normalized(settings: DanmakuSettings): DanmakuSettings = native {
        danmakuSettingsNormalized(settings.toFfi()).toCore()
    }

    fun reset(settings: DanmakuSettings): DanmakuSettings = native {
        danmakuSettingsReset(settings.toFfi()).toCore()
    }

    fun fromLegacyDensity(density: Int): DanmakuSettings = native {
        danmakuSettingsFromLegacyDensity(density).toCore()
    }

    fun nearestArea(value: Float): Float = native { danmakuNearestArea(value) }

    fun areaOptions(): List<Float> = native { danmakuAreaOptions() }

    fun maxRules(): Int = native { danmakuMaxRules().toInt() }

    fun maxRuleLength(): Int = native { danmakuMaxRuleLength().toInt() }

    fun massiveLineHeight(lineHeight: Float): Float = native { danmakuMassiveLineHeight(lineHeight) }

    fun scrollMs(speed: Float, baseMs: Long): Long = native { danmakuScrollMs(speed, baseMs) }

    fun fixedVelocityFactor(widthDp: Float): Float = native { danmakuFixedVelocityFactor(widthDp) }

    // ---------- 每帧的两个纯算术 ----------

    /**
     * 滚动弹幕此刻左边缘的 x（顶部 / 底部居中）。
     *
     * **每帧、每条调一次**：真的过一次 FFI 太贵（屏上几十条 × 每秒 60~120 帧），所以生产路径用的是
     * Kotlin 里同样的一行（`ActiveDanmaku.x`）——两边口径由 `DanmakuDualRunTest` 盯着。这个入口留给
     * 检查工具和对拍。
     */
    fun x(item: PlannedDanmaku, nowMs: Long, viewWidth: Float): Float = native {
        plannedX(item.toFfi(), nowMs, viewWidth)
    }

    /** 弹幕上边缘的 y。 */
    fun y(plan: DanmakuPlan, index: Int): Float? = native { danmakuPlanY(plan.toFfi(), index.toUInt()) }

    internal inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("弹幕层：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
    }

    // ---------- 映射（只有这一份） ----------

    internal fun feedhub.core.Danmaku.toFfi(): FfiDanmaku = FfiDanmaku(
        id = id,
        timeMs = timeMs,
        mode = when (mode) {
            feedhub.core.DanmakuMode.SCROLL -> FfiDanmakuMode.SCROLL
            feedhub.core.DanmakuMode.TOP -> FfiDanmakuMode.TOP
            feedhub.core.DanmakuMode.BOTTOM -> FfiDanmakuMode.BOTTOM
            feedhub.core.DanmakuMode.OTHER -> FfiDanmakuMode.OTHER
        },
        text = text,
        color = color,
        fontSize = fontSize,
        weight = weight,
    )

    internal fun FfiDanmaku.toCore(): feedhub.core.Danmaku = feedhub.core.Danmaku(
        id = id,
        timeMs = timeMs,
        mode = when (mode) {
            FfiDanmakuMode.SCROLL -> feedhub.core.DanmakuMode.SCROLL
            FfiDanmakuMode.TOP -> feedhub.core.DanmakuMode.TOP
            FfiDanmakuMode.BOTTOM -> feedhub.core.DanmakuMode.BOTTOM
            FfiDanmakuMode.OTHER -> feedhub.core.DanmakuMode.OTHER
        },
        text = text,
        color = color,
        fontSize = fontSize,
        weight = weight,
    )

    internal fun DanmakuFilter.toFfi(): FfiDanmakuFilter = FfiDanmakuFilter(
        allowScroll = allowScroll,
        allowTop = allowTop,
        allowBottom = allowBottom,
        allowColorful = allowColorful,
        weightLevel = weightLevel,
        rules = rules,
        staticToScroll = staticToScroll,
        merge = merge,
        mergeWindowMs = mergeWindowMs,
        mergeThreshold = mergeThreshold,
    )

    internal fun FfiDanmakuFilter.toCore(): DanmakuFilter = DanmakuFilter(
        allowScroll = allowScroll,
        allowTop = allowTop,
        allowBottom = allowBottom,
        allowColorful = allowColorful,
        weightLevel = weightLevel,
        rules = rules,
        staticToScroll = staticToScroll,
        merge = merge,
        mergeWindowMs = mergeWindowMs,
        mergeThreshold = mergeThreshold,
    )

    internal fun DanmakuSettings.toFfi(): FfiDanmakuSettings = FfiDanmakuSettings(
        opacity = opacity,
        fontScale = fontScale,
        fontWeight = fontWeight,
        strokeWidth = strokeWidth,
        area = area,
        lineHeight = lineHeight,
        massive = massive,
        scrollSeconds = scrollSeconds,
        fixedVelocity = fixedVelocity,
        fixedSeconds = fixedSeconds,
        staticToScroll = staticToScroll,
        allowScroll = allowScroll,
        allowTop = allowTop,
        allowBottom = allowBottom,
        allowColorful = allowColorful,
        weightLevel = weightLevel,
        mergeDuplicates = mergeDuplicates,
        mergeWindowMs = mergeWindowMs,
        mergeThreshold = mergeThreshold,
        blockRules = blockRules,
    )

    internal fun FfiDanmakuSettings.toCore(): DanmakuSettings = DanmakuSettings(
        opacity = opacity,
        fontScale = fontScale,
        fontWeight = fontWeight,
        strokeWidth = strokeWidth,
        area = area,
        lineHeight = lineHeight,
        massive = massive,
        scrollSeconds = scrollSeconds,
        fixedVelocity = fixedVelocity,
        fixedSeconds = fixedSeconds,
        staticToScroll = staticToScroll,
        allowScroll = allowScroll,
        allowTop = allowTop,
        allowBottom = allowBottom,
        allowColorful = allowColorful,
        weightLevel = weightLevel,
        mergeDuplicates = mergeDuplicates,
        mergeWindowMs = mergeWindowMs,
        mergeThreshold = mergeThreshold,
        blockRules = blockRules,
    )
}

internal fun FfiDanmakuMode.toCore(): feedhub.core.DanmakuMode = when (this) {
    FfiDanmakuMode.SCROLL -> feedhub.core.DanmakuMode.SCROLL
    FfiDanmakuMode.TOP -> feedhub.core.DanmakuMode.TOP
    FfiDanmakuMode.BOTTOM -> feedhub.core.DanmakuMode.BOTTOM
    FfiDanmakuMode.OTHER -> feedhub.core.DanmakuMode.OTHER
}

internal fun feedhub.core.DanmakuMode.toFfi(): FfiDanmakuMode = when (this) {
    feedhub.core.DanmakuMode.SCROLL -> FfiDanmakuMode.SCROLL
    feedhub.core.DanmakuMode.TOP -> FfiDanmakuMode.TOP
    feedhub.core.DanmakuMode.BOTTOM -> FfiDanmakuMode.BOTTOM
    feedhub.core.DanmakuMode.OTHER -> FfiDanmakuMode.OTHER
}

internal fun uniffi.feedhub_ffi.FfiPlannedDanmaku.toCore(): PlannedDanmaku = PlannedDanmaku(
    id = id,
    mode = mode.toCore(),
    lane = lane,
    startMs = startMs,
    durationMs = durationMs,
    width = width,
    viewWidth = viewWidth,
)

internal fun PlannedDanmaku.toFfi(): uniffi.feedhub_ffi.FfiPlannedDanmaku =
    uniffi.feedhub_ffi.FfiPlannedDanmaku(
        id = id,
        mode = mode.toFfi(),
        lane = lane,
        startMs = startMs,
        durationMs = durationMs,
        width = width,
        viewWidth = viewWidth,
    )

internal fun uniffi.feedhub_ffi.FfiDanmakuPlan.toCore(): DanmakuPlan = DanmakuPlan(
    items = items.map { it.toCore() },
    viewWidth = viewWidth,
    viewHeight = viewHeight,
    laneHeight = laneHeight,
    usableLanes = usableLanes,
    scrollLanes = scrollLanes,
    topFixedLanes = topFixedLanes,
    bottomFixedLanes = bottomFixedLanes,
    nextMs = nextMs,
)

internal fun DanmakuPlan.toFfi(): uniffi.feedhub_ffi.FfiDanmakuPlan =
    uniffi.feedhub_ffi.FfiDanmakuPlan(
        items = items.map { it.toFfi() },
        viewWidth = viewWidth,
        viewHeight = viewHeight,
        laneHeight = laneHeight,
        usableLanes = usableLanes,
        scrollLanes = scrollLanes,
        topFixedLanes = topFixedLanes,
        bottomFixedLanes = bottomFixedLanes,
        nextMs = nextMs,
    )

/**
 * **弹幕时间线**（Rust `FfiDanmakuTimeline` 的 Kotlin 面）。
 *
 * 宿主线程和安卓的弹幕画线程都会碰它 → 所有方法 `@Synchronized`（Rust 那边也有自己的 `Mutex`，
 * 这里加锁是为了"put 之后立刻 setWidths"这种两步操作不被插队）。
 */
class RustDanmakuTimeline internal constructor(internal val inner: FfiDanmakuTimeline) {
    constructor(segmentMs: Long, filter: DanmakuFilter) : this(
        // 构造 UniFFI 对象也会碰 `.so`：走 `native` 才会先 ensureLoaded（不然先构造后加载会抛
        // UnsatisfiedLinkError，而且绕过了那条"说得清的错误"）
        RustDanmaku.native { FfiDanmakuTimeline(segmentMs, RustDanmaku.run { filter.toFfi() }) },
    )

    val segmentMs: Long get() = inner.segmentMs()

    /** 放进一段，返回**处理好的**列表（宿主拿它当文字表）。 */
    @Synchronized
    fun put(segment: Int, list: List<feedhub.core.Danmaku>): List<feedhub.core.Danmaku> =
        RustDanmaku.run { inner.put(segment, list.map { it.toFfi() }).map { it.toCore() } }

    /** 塞进这一段每条的文字宽度（像素），顺序和 [put] 交回的列表一致。 */
    @Synchronized
    fun setWidths(segment: Int, widths: List<Float>) = inner.setWidths(segment, widths)

    /** 换过滤规则：`null` = 没变；否则是**重新处理过的每一段**（重建文字表 / 重新量宽度用）。 */
    @Synchronized
    fun setFilter(filter: DanmakuFilter): List<Pair<Int, List<feedhub.core.Danmaku>>>? =
        RustDanmaku.run {
            inner.setFilter(filter.toFfi())?.map { segment ->
                segment.segment to segment.items.map { it.toCore() }
            }
        }

    @Synchronized
    fun has(segment: Int): Boolean = inner.has(segment)

    @Synchronized
    fun segmentOf(ms: Long): Int = inner.segmentOf(ms)

    @Synchronized
    fun loadedCount(): Int = inner.loadedCount().toInt()

    @Synchronized
    fun version(): Int = inner.version().toInt()

    @Synchronized
    fun rangeIds(fromExclusive: Long, toInclusive: Long): List<ULong> =
        inner.rangeIds(fromExclusive, toInclusive)

    @Synchronized
    fun nextTimeAfter(afterMs: Long): Long? = inner.nextTimeAfter(afterMs)
}

/** **弹幕装载策略**（Rust `FfiDanmakuLoader` 的 Kotlin 面）。 */
class RustDanmakuLoader(segmentMs: Long, durationMs: Long?) {
    private val inner = RustDanmaku.native { FfiDanmakuLoader(segmentMs, durationMs) }

    @Synchronized
    fun ensure(positionMs: Long, loaded: Set<Int>, nowMs: Long): List<Int> =
        inner.ensure(positionMs, loaded.toList(), nowMs)

    @Synchronized
    fun complete(segment: Int) = inner.complete(segment)

    @Synchronized
    fun tries(segment: Int): Int = inner.tries(segment).toInt()

    @Synchronized
    fun inFlight(): Int = inner.inFlight().toInt()
}

/**
 * **弹幕排布器**（Rust `FfiDanmakuScheduler` 的 Kotlin 面）。
 *
 * 调 [advance] / [reset] 的时机是**有新的弹幕要出现**时（[DanmakuPlan.nextMs] 给的时刻），外加跳转 / 改设置 /
 * 换倍速——**不是每帧**。
 */
class RustDanmakuScheduler {
    private val inner = RustDanmaku.native { FfiDanmakuScheduler() }

    /** 视图尺寸 / 排布变了就重算轨道数；返回 true = 真的变了（要重新 [reset]）。 */
    @Synchronized
    fun configure(viewWidth: Float, viewHeight: Float, layout: DanmakuLayout): Boolean =
        inner.configure(
            viewWidth,
            viewHeight,
            uniffi.feedhub_ffi.FfiDanmakuLayout(
                fontPx = layout.fontPx,
                fontWeight = layout.fontWeight,
                laneHeight = layout.laneHeight,
                area = layout.area,
                scrollMs = layout.scrollMs,
                fixedMs = layout.fixedMs,
                reserveTop = layout.reserveTop,
                reserveBottom = layout.reserveBottom,
            ),
        )

    /** 倍速：只影响**新排**的滚动弹幕。 */
    @Synchronized
    fun setPlaybackSpeed(speed: Float) = inner.setSpeed(speed)

    @Synchronized
    fun reset(positionMs: Long, timeline: RustDanmakuTimeline): DanmakuPlan =
        inner.reset(positionMs, timeline.inner).toCore()

    @Synchronized
    fun advance(nowMs: Long, timeline: RustDanmakuTimeline): DanmakuPlan =
        inner.advance(nowMs, timeline.inner).toCore()

    @Synchronized
    fun nextMs(nowMs: Long, timeline: RustDanmakuTimeline): Long? =
        inner.nextMs(nowMs, timeline.inner)

    @Synchronized
    fun activeCount(): Int = inner.activeCount().toInt()
}
