package feedhub.app.perf

import feedhub.perf.RustPerf
import uniffi.feedhub_ffi.FfiPerfProcTicks

/**
 * `/proc` 里 CPU 时间的解析（纯函数，离线可测）：`/proc/self/stat` 和 `/proc/self/task/<tid>/stat` 是同一个格式。
 *
 * 字段 14 = utime、15 = stime（单位是 clock tick，Linux 用户态 `USER_HZ` 固定 100，所以 1 tick = 10ms；
 * 安卓沿用同一个值，见 [PERF_CLOCK_TICKS_PER_SEC]）。第 2 个字段 `comm` 是**带括号**的进程 / 线程名，里面可能有
 * 空格甚至括号（例如 `(Web Content)`），所以按"第一个 `(` 到最后一个 `)`"取名字，后面的字段按空白切。
 *
 * 核心解析已由 Rust (`feedhub-core::perf::proc`) 实现，此处委托给 Rust。
 */
data class ProcTicks(
    /** 第 1 个字段：进程 / 线程 id。 */
    val pid: Int,
    /** 第 2 个字段（括号里的名字）。 */
    val name: String,
    val utimeTicks: Long,
    val stimeTicks: Long,
) {
    val totalTicks: Long get() = utimeTicks + stimeTicks
}

/** Linux 用户态 clock tick 的频率：固定 100（1 tick = 10ms）。 */
const val PERF_CLOCK_TICKS_PER_SEC: Long = 100L

/**
 * 解析一行 `/proc/.../stat`；格式不对返回 null（进程刚退出、文件读了一半都算）。
 *
 * @param text 文件全文（可能带换行）。
 */
fun parseProcStat(text: String): ProcTicks? {
    val ffi = RustPerf.parseProcStat(text) ?: return null
    return ProcTicks(ffi.pid, ffi.name, ffi.utimeTicks, ffi.stimeTicks)
}

/** 一个线程在两次采样之间用掉的 CPU（毫秒）。 */
data class ThreadCpu(val name: String, val ms: Long)

/**
 * 按线程算两次采样之间的增量，取最大的 [limit] 个（`/proc/self/task/<tid>` 是**线程**，名字会重复，所以按 tid 配对）。
 *
 * @param previous 上一次的 `tid -> ticks`
 * @param current 这一次读到的所有线程
 * @param limit 最多几个（任务要求 8）
 */
fun topThreadDeltas(
    previous: Map<Int, ProcTicks>,
    current: List<ProcTicks>,
    limit: Int = 8,
    ticksPerSecond: Long = PERF_CLOCK_TICKS_PER_SEC,
): List<ThreadCpu> {
    val prevFfi = previous.mapValues { (_, v) -> FfiPerfProcTicks(v.pid, v.name, v.utimeTicks, v.stimeTicks) }
    val curFfi = current.map { FfiPerfProcTicks(it.pid, it.name, it.utimeTicks, it.stimeTicks) }
    return RustPerf.topThreadDeltas(prevFfi, curFfi, limit, ticksPerSecond).map {
        ThreadCpu(it.name, it.ms)
    }
}

/** tick 差 → 毫秒（向下取整；1 tick 在 100Hz 下就是 10ms）。 */
fun ticksToMs(ticks: Long, ticksPerSecond: Long = PERF_CLOCK_TICKS_PER_SEC): Long =
    RustPerf.ticksToMs(ticks, ticksPerSecond)

/**
 * 非负增量：计数器被重置（进程重启、系统清账）时返回 null，而不是一个负的假数。
 * 拿不到（null）也返回 null。
 */
fun deltaOrNull(before: Long?, now: Long?): Long? =
    RustPerf.deltaOrNull(before, now)

/**
 * 有符号增量：**允许多出来的负值**。只给本来就该有正负的计数器用（现在只有电池 `CHARGE_COUNTER`：
 * 放电时它在减少，回前台算 `bg_span` 要的就是这个负数）。
 * 数量累加器（CPU tick / 网络字节 / GC 次数）继续用 [deltaOrNull]：那些变小了只可能是计数器被重置。
 */
fun signedDeltaOrNull(before: Long?, now: Long?): Long? =
    RustPerf.signedDeltaOrNull(before, now)

