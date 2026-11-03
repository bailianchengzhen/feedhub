package feedhub.app.perf

import feedhub.perf.RustPerf

/**
 * 帧耗时统计（纯函数，离线可测）：这 10 秒的总帧数、超过截止时间的帧数、p50 / p90 / p99 / max。
 *
 * 数据来自安卓的 `Window.addOnFrameMetricsAvailableListener`（`FrameMetrics.TOTAL_DURATION`，单位纳秒）。
 * 为什么不用 androidx 的 `JankStats`：它要加 `androidx.metrics:metrics-performance` 依赖，而且报的是"卡了多久、
 * 是不是掉帧"的事件，我们这里要的是**每 10 秒一份的分位数**，自己收 `TOTAL_DURATION` 一次排序就够了；
 * 监听器挂在后台 Handler 上，主线程只在系统交帧时被抄一份数字。
 *
 * 核心算法已由 Rust (`feedhub-core::perf::frame_stats`) 实现，此处委托给 Rust。
 */
data class FrameStats(
    /** 这一段里交了多少帧。 */
    val frames: Int,
    /** 超过截止时间（一个刷新周期）的帧数。 */
    val janky: Int,
    /** 毫秒。 */
    val p50: Double,
    val p90: Double,
    val p99: Double,
    val max: Double,
    /** 判定卡顿用的截止时间（毫秒）= 1 秒 / 刷新率。 */
    val deadlineMs: Double,
) {
    /** 卡顿率 0~1。 */
    val jankRate: Double get() = if (frames == 0) 0.0 else janky.toDouble() / frames
}

/** 兜底截止时间（毫秒）：不知道刷新率时按 60Hz 算。 */
const val PERF_DEFAULT_DEADLINE_MS: Double = 1000.0 / 60.0

/**
 * 按帧耗时算统计。耗时 <= 0 的帧（系统偶尔报 0）丢掉，不算进总帧数——它们没有参考价值，还会把 p50 拉到 0。
 *
 * @param durationsNs 每帧的 `TOTAL_DURATION`（纳秒），顺序不重要
 * @param refreshHz 屏幕刷新率（10 秒里可能变过，取采样那一刻的值；<= 0 按 60）
 */
fun frameStats(durationsNs: List<Long>, refreshHz: Float = 60f): FrameStats? {
    val ffi = RustPerf.frameStats(durationsNs, refreshHz) ?: return null
    return FrameStats(
        frames = ffi.frames,
        janky = ffi.janky,
        p50 = ffi.p50,
        p90 = ffi.p90,
        p99 = ffi.p99,
        max = ffi.max,
        deadlineMs = ffi.deadlineMs,
    )
}

/** 保留三位小数（毫秒），日志小一点。 */
internal fun round3(v: Double): Double = RustPerf.round3(v)

