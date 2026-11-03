package feedhub.app.perf

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.Platform
import feedhub.perf.RustPerf
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.feedhub_ffi.*
import kotlin.coroutines.CoroutineContext

/** 前台采样的间隔（任务：每 10 秒一行）。 */
const val PERF_SAMPLE_INTERVAL_MS: Long = 10_000L

/**
 * 性能记录器：
 *
 * - **开关关着、或者不在前台，一个定时器都没有**（[loop] 只在两者都成立时存在，切后台第一件事就是 cancel）；
 * - 每 [intervalMs] 毫秒采一行 `sample`；开关 / 前后台 / 亮灭屏 / 视频 / 刷新 / 详情 / 评论这些**事件发生时**
 *   各写一行 `event`；进后台时记基线，回前台写一行 `bg_span`；
 * - 所有文件 IO 都在这份 recorder 的 [io] 上下文中做（采样循环本身就在 IO 上跑，事件用 `scope.launch(io)`），
 *   **不在主线程读写文件**；
 * - 只记平台 id、内容类型、页面名、计数、耗时（隐私口径见 [PerfHooks]），没开就什么都不写。
 *
 * 核心状态机、分位数、增量计算与 JSON Lines 组装已完全下沉至 Rust (`feedhub-core::perf::recorder`)。
 *
 * @param sampler 平台采样器；测试给假的（[PerfReading] 想怎么填就怎么填）。
 * @param store 写文件的那份；null = 不写（桌面）。
 * @param scope App 的主线程作用域（[feedhub.app.AppGraph.scope]）。
 * @param threadCpuMs **当前线程**的 CPU 时间（安卓 `SystemClock.currentThreadTimeMillis`），用来量采样自身开销。
 * @param exporter 把记录复制到 adb 能直接拉的位置（安卓 = 系统下载目录）；null = 设置页不显示这个按钮。
 */
class PerfRecorder(
    private val sampler: PerfSampler?,
    val store: PerfFileStore?,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
    private val now: () -> Long = System::currentTimeMillis,
    private val threadCpuMs: () -> Long = { System.nanoTime() / 1_000_000 },
    private val intervalMs: Long = PERF_SAMPLE_INTERVAL_MS,
    /** 导出到下载目录（安卓实现见 `androidMain/.../perf/AndroidPerfExport.kt`）；返回给人看的一句话。 */
    val exporter: (suspend () -> String)? = null,
) {
    /** 开关本身（Compose 状态：设置页那一行要跟着变）。方法和属性同名会撞 JVM 签名，所以状态放这里。 */
    private var recording by mutableStateOf(false)

    /** 开关（只读；改它用 [setEnabled]）。 */
    val enabled: Boolean get() = recording

    /** 前台 / 后台（由 `AppGraph.onForeground` 喂）。 */
    private var foreground = true

    /** 当前页面名（由 `App` 的导航状态喂，见 [PerfScreen]）。 */
    @Volatile
    var screen: String = "首页"
        private set

    /** 同 [sample] 的快照，给设置页看"最近写了没有"（不是 Compose 状态，别在组合里直接读）。 */
    @Volatile
    var samplesWritten: Long = 0
        private set

    private var loop: Job? = null

    /** Rust 状态机核心。 */
    private val core: FfiPerfRecorderCore by lazy {
        RustPerf.createRecorderCore(PERF_CLOCK_TICKS_PER_SEC)
    }

    // ---------------------------------------------------------------------
    // 开关 / 前后台
    // ---------------------------------------------------------------------

    /**
     * 开关变了：**挂 / 摘监听、起 / 停定时器、写一行 start / stop**。只在主线程调（`AppGraph` 的 snapshotFlow）。
     */
    fun setEnabled(on: Boolean) {
        if (recording == on) return
        recording = on
        if (on) {
            sampler?.setEnabled(true)
            sampler?.setScreenListener { lit -> eventScreen(lit) }
            // 基线读数去 IO 上做（读 /proc、取粘性广播都算文件 / 系统调用，不能占主线程）
            scope.launch(io) {
                val reading = readSafe()
                val line = core.onEnabled(reading.toFfi(), now(), screen)
                if (!enabled) core.onDisabled(0L, "")
                store?.prune()
                writeNow(line)
            }
            restartLoop()
            // 开着的这一瞬间就在前台：顺手量一次"下一帧"（冷启动 / 刚打开开关时的首帧）
            if (foreground) sampler?.awaitNextFrame { ms -> eventFirstFrame(ms) }
        } else {
            stopLoop()
            sampler?.setEnabled(false)
            sampler?.setScreenListener(null)
            core.onDisabled(0L, "")
            // 同一份 IO 协程里写 stop 再关文件，别让 close 抢在写前面
            scope.launch(io) {
                val line = core.onDisabled(now(), screen)
                writeNow(line)
                store?.close()
            }
        }
    }

    /**
     * 前后台变了（`MainActivity.onStart/onStop` → `AppGraph.onForeground`）。
     *
     * 进后台：**马上**停定时器、记基线（读数在 IO 上）、写一行 `event background`。
     * 回前台：算 `bg_span`（后台时长、本进程 CPU、电量计数、网络字节的变化）、量一次首帧、重新起定时器。
     */
    fun setForeground(active: Boolean) {
        if (foreground == active) return
        foreground = active
        if (!enabled) return
        if (active) {
            sampler?.awaitNextFrame { ms -> eventFirstFrame(ms) }
            scope.launch(io) { finishBackgroundSpan() }
            restartLoop()
        } else {
            stopLoop()
            scope.launch(io) { beginBackgroundSpan() }
        }
    }

    /** 页面变了（就存起来，下一行 sample 带上；不单独写事件，免得切页签时刷屏）。 */
    fun setScreen(name: String) {
        screen = name
    }

    /**
     * 采一行（**只在 IO 上跑**：采样循环、测试直接调）。开关关着就什么都不做。
     * 返回写成功没有（没开 / 写失败都是 false）。
     */
    fun sampleOnce(): Boolean {
        if (!enabled) return false
        val started = threadCpuMs()
        val reading = readSafe()
        val cost = (threadCpuMs() - started).coerceAtLeast(0)
        val line = core.sampleOnce(reading.toFfi(), cost, now(), screen)
        val ok = writeNow(line)
        if (ok) samplesWritten++
        return ok
    }

    /** 记录器自己的状态（目录 / 文件数 / 字节）；[PerfRecorder.store] 为 null 就是"不支持"。 */
    suspend fun status(): PerfStatus = withContext(io) { store?.status() ?: PerfStatus.Unsupported }

    /** 清空记录，返回删了几个文件。 */
    suspend fun clear(): Int = withContext(io) { store?.clear() ?: 0 }

    /** 关掉打开着的文件、停掉定时器。 */
    fun close() {
        stopLoop()
        runCatching { store?.close() }
    }

    private fun restartLoop() {
        if (!enabled || !foreground) return
        if (loop?.isActive == true) return
        loop = scope.launch {
            while (isActive) {
                delay(intervalMs)
                withContext(io) { sampleOnce() }
            }
        }
    }

    private fun stopLoop() {
        loop?.cancel()
        loop = null
    }

    // ---------------------------------------------------------------------
    // 事件
    // ---------------------------------------------------------------------

    /** 信息流刷新：开始 / 结束各一行（结束带耗时、条数、哪几家平台）。 */
    fun eventFeedRefresh(kind: FeedKind, start: Boolean, platforms: List<Platform>, count: Int? = null, dMs: Long? = null) {
        if (!enabled) return
        val kindStr = if (kind == FeedKind.RECOMMEND) "home" else "following"
        writeSoon(RustPerf.buildEventFeedRefresh(now(), screen, kindStr, start, platforms.map { it.key }, count, dMs))
    }

    /**
     * 详情页正文到了：平台 + 内容类型 + 从发起请求到正文回来的耗时。
     * [queueMs] 是这次请求在平台限速器里排队的时长（null = 这个平台 / 这条路径没接排队记账），
     * 下次看日志就能分清"慢在排队还是慢在网络"。
     */
    fun eventDetailLoaded(platform: Platform, kind: ItemKind, dMs: Long, queueMs: Long? = null) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventDetailLoaded(now(), screen, platform.key, kind.name.lowercase(), dMs, queueMs))
    }

    /** 评论第一页回来了（平台 + 条数 + 耗时 + 排队）。 */
    fun eventCommentsLoaded(platform: Platform, count: Int, dMs: Long, queueMs: Long? = null) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventCommentsLoaded(now(), screen, platform.key, count, dMs, queueMs))
    }

    /**
     * 详情 / 评论 / 楼中楼 / 播放**失败**了：
     * `name` 是 `<kind>_failed`（`detail_failed` / `comments_failed` / `play_failed`），
     * 字段是错误类型（[PerfHooks.errorReason]：`RiskControl` / `NotLoggedIn` / `Network` / `Timeout` / `Cancelled` …）、
     * 耗时、排队时长。
     *
     * 以前失败一个记录都没有——"详情 / 视频完全不动"那几次事后完全无从查起（性能记录、logcat 都空的）。
     */
    fun eventRequestFailed(kind: String, platform: Platform, reason: String, dMs: Long, queueMs: Long? = null) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventRequestFailed(now(), screen, kind, platform.key, reason, dMs, queueMs))
    }

    /**
     * 本地向量算完了一批：**后端**（`cpu` / `npu`）、条数、耗时、模型标识。
     *
     * **不记标题 / 正文 / URL**（和别的打点一个口径），[model] 只是模型的名字。
     */
    /**
     * 一批本地向量算完。[len] 是这一批用的**固定长度图**（token 数；0 = CPU / 不知道），
     */
    fun eventEmbedBatch(backend: String, count: Int, dMs: Long, model: String, len: Int = 0) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventEmbedBatch(now(), screen, backend, count, dMs, model, len))
    }

    /**
     * 一个**封面窗口**算完了：条数、视觉那一段 / 文本+图那一段各多少毫秒、
     * 合计、这个窗口**现编了几张图**（0 = 都是 ctx 加载，2 = 两张都编了）。
     *
     * 为什么单独一行 `cover_window` 而不复用 `embed_batch`：封面是"两张图串行 + 切会话"的结构，
     * 一段一段的耗时才是要看的（视觉 32 张 vs 文本 4 批 vs 切换），混进 `embed_batch` 就分不出来了。
     * 一样**不记标题 / 正文 / URL**。
     */
    fun eventCoverWindow(count: Int, visionMs: Long, textMs: Long, totalMs: Long, compiled: Int) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventCoverWindow(now(), screen, count, visionMs, textMs, totalMs, compiled))
    }

    /** 视频开始 / 暂停 / 播完 / 首帧。 */
    fun eventVideo(
        platform: Platform,
        action: String,
        playing: Boolean,
        hardware: Boolean?,
        qn: Int?,
        height: Int?,
        dMs: Long? = null,
    ) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventVideo(now(), screen, platform.key, action, playing, hardware, qn, height, dMs))
    }

    /** 回到前台后的第一帧（毫秒，从 `onStart` 到真的画出帧）。 */
    fun eventFirstFrame(dMs: Long) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventFirstFrame(now(), screen, dMs))
    }

    /** 亮屏 / 灭屏。 */
    fun eventScreen(lit: Boolean) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventScreen(now(), screen, lit))
    }

    /** 一行不带字段的事件（start / stop / background）。 */
    fun eventMark(name: String) {
        if (!enabled) return
        writeSoon(RustPerf.buildEventMark(now(), screen, name))
    }

    private fun beginBackgroundSpan() {
        val reading = readSafe()
        val line = core.beginBackground(reading.toFfi(), now(), screen)
        writeNow(line)
    }

    private fun finishBackgroundSpan() {
        val end = readSafe()
        val line = core.finishBackground(end.toFfi(), now()) ?: return
        writeNow(line)
    }

    // ---------------------------------------------------------------------
    // 写行
    // ---------------------------------------------------------------------

    /** 已经在 IO 线程上（采样循环 / 后台基线）用的同步写。 */
    private fun writeNow(line: String): Boolean {
        val s = store ?: return false
        return s.append(line)
    }

    /** 主线程上发生的事件：扔到 IO 去写（**不在主线程做文件 IO**）。 */
    private fun writeSoon(line: String) {
        val s = store ?: return
        scope.launch(io) { s.append(line) }
    }

    private fun readSafe(): PerfReading = runCatching { sampler?.read() }.getOrNull() ?: PerfReading()
}

internal fun PerfReading.toFfi(): FfiPerfReading = FfiPerfReading(
    battery = battery?.let { b ->
        FfiPerfBatteryReading(
            currentUa = b.currentUa,
            currentUnit = b.currentUnit,
            chargeUah = b.chargeUah,
            level = b.level,
            voltageMv = b.voltageMv,
            status = b.status,
            plugged = b.plugged,
        )
    },
    cpuTicks = cpuTicks,
    threads = threads.map { FfiPerfProcTicks(it.pid, it.name, it.utimeTicks, it.stimeTicks) },
    frames = frames?.let { FfiPerfFrameReading(it.durationsNs, it.refreshHz) },
    network = network?.let { FfiPerfNetworkReading(it.rxBytes, it.txBytes) },
    thermal = thermal?.let { FfiPerfThermalReading(it.status, it.headroom) },
    heapUsed = heapUsed,
    gc = gc?.let { FfiPerfGcReading(it.count, it.timeMs) },
    playback = playback.map { p ->
        FfiPerfPlaybackReading(p.platform, p.playing, p.hardware, p.qn, p.height)
    },
    screenOn = screenOn,
)

