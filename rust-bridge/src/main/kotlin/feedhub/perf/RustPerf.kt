package feedhub.perf

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.*

/**
 * 性能记录的 Rust 门面。
 * 纯逻辑、分位数、增量计算、文件轮转淘汰、JSON Lines 组装全在 Rust。
 */
object RustPerf {
    init {
        RustNative.ensureLoaded()
    }

    fun ensureLoaded() {
        RustNative.ensureLoaded()
    }

    // ---------------------------------------------------------------------
    // /proc
    // ---------------------------------------------------------------------

    fun parseProcStat(text: String): FfiPerfProcTicks? {
        ensureLoaded()
        return perfParseProcStat(text)
    }

    fun topThreadDeltas(
        previous: Map<Int, FfiPerfProcTicks>,
        current: List<FfiPerfProcTicks>,
        limit: Int = 8,
        ticksPerSecond: Long = 100L,
    ): List<FfiPerfThreadCpu> {
        ensureLoaded()
        return perfTopThreadDeltas(previous.values.toList(), current, limit.toUInt(), ticksPerSecond)
    }

    fun ticksToMs(ticks: Long, ticksPerSecond: Long = 100L): Long {
        ensureLoaded()
        return perfTicksToMs(ticks, ticksPerSecond)
    }

    fun deltaOrNull(before: Long?, now: Long?): Long? {
        ensureLoaded()
        return perfDeltaOrNull(before, now)
    }

    fun signedDeltaOrNull(before: Long?, now: Long?): Long? {
        ensureLoaded()
        return perfSignedDeltaOrNull(before, now)
    }

    fun readProcSelfStat(): FfiPerfProcTicks? {
        ensureLoaded()
        return perfReadProcSelfStat()
    }

    fun readProcSelfThreads(): List<FfiPerfProcTicks> {
        ensureLoaded()
        return perfReadProcSelfThreads()
    }

    // ---------------------------------------------------------------------
    // 帧统计分位数
    // ---------------------------------------------------------------------

    fun frameStats(durationsNs: List<Long>, refreshHz: Float = 60f): FfiPerfFrameStats? {
        ensureLoaded()
        return perfFrameStats(durationsNs, refreshHz)
    }

    fun round3(v: Double): Double {
        ensureLoaded()
        return perfRound3(v)
    }

    // ---------------------------------------------------------------------
    // 请求计量与文案
    // ---------------------------------------------------------------------

    fun timeoutMessage(timeoutMs: Long): String {
        ensureLoaded()
        return perfTimeoutMessage(timeoutMs)
    }

    fun requestFailedLogLine(
        kind: String,
        platform: String,
        reason: String,
        ms: Long,
        queueMs: Long? = null,
    ): String {
        ensureLoaded()
        return perfRequestFailedLogLine(kind, platform, reason, ms, queueMs)
    }

    // ---------------------------------------------------------------------
    // 文件存储核心
    // ---------------------------------------------------------------------

    fun createFileStore(keepDays: Int): FfiPerfFileStore {
        ensureLoaded()
        return FfiPerfFileStore(keepDays.toUInt())
    }

    // ---------------------------------------------------------------------
    // 采样与事件状态机
    // ---------------------------------------------------------------------

    fun createRecorderCore(ticksPerSecond: Long = 100L): FfiPerfRecorderCore {
        ensureLoaded()
        return FfiPerfRecorderCore(ticksPerSecond)
    }

    fun buildEventMark(nowMs: Long, screen: String, name: String): String {
        ensureLoaded()
        return perfBuildEventMark(nowMs, screen, name)
    }

    fun buildEventFeedRefresh(
        nowMs: Long,
        screen: String,
        kind: String,
        start: Boolean,
        platforms: List<String>,
        count: Int?,
        dMs: Long?,
    ): String {
        ensureLoaded()
        return perfBuildEventFeedRefresh(nowMs, screen, kind, start, platforms, count, dMs)
    }

    fun buildEventDetailLoaded(
        nowMs: Long,
        screen: String,
        platform: String,
        kind: String,
        dMs: Long,
        queueMs: Long?,
    ): String {
        ensureLoaded()
        return perfBuildEventDetailLoaded(nowMs, screen, platform, kind, dMs, queueMs)
    }

    fun buildEventCommentsLoaded(
        nowMs: Long,
        screen: String,
        platform: String,
        count: Int,
        dMs: Long,
        queueMs: Long?,
    ): String {
        ensureLoaded()
        return perfBuildEventCommentsLoaded(nowMs, screen, platform, count, dMs, queueMs)
    }

    fun buildEventRequestFailed(
        nowMs: Long,
        screen: String,
        kind: String,
        platform: String,
        reason: String,
        dMs: Long,
        queueMs: Long?,
    ): String {
        ensureLoaded()
        return perfBuildEventRequestFailed(nowMs, screen, kind, platform, reason, dMs, queueMs)
    }

    fun buildEventEmbedBatch(
        nowMs: Long,
        screen: String,
        backend: String,
        count: Int,
        dMs: Long,
        model: String,
        len: Int,
    ): String {
        ensureLoaded()
        return perfBuildEventEmbedBatch(nowMs, screen, backend, count, dMs, model, len)
    }

    fun buildEventCoverWindow(
        nowMs: Long,
        screen: String,
        count: Int,
        visionMs: Long,
        textMs: Long,
        totalMs: Long,
        compiled: Int,
    ): String {
        ensureLoaded()
        return perfBuildEventCoverWindow(nowMs, screen, count, visionMs, textMs, totalMs, compiled)
    }

    fun buildEventVideo(
        nowMs: Long,
        screen: String,
        platform: String,
        action: String,
        playing: Boolean,
        hardware: Boolean?,
        qn: Int?,
        height: Int?,
        dMs: Long?,
    ): String {
        ensureLoaded()
        return perfBuildEventVideo(nowMs, screen, platform, action, playing, hardware, qn, height, dMs)
    }

    fun buildEventFirstFrame(nowMs: Long, screen: String, dMs: Long): String {
        ensureLoaded()
        return perfBuildEventFirstFrame(nowMs, screen, dMs)
    }

    fun buildEventScreen(nowMs: Long, screen: String, lit: Boolean): String {
        ensureLoaded()
        return perfBuildEventScreen(nowMs, screen, lit)
    }
}
