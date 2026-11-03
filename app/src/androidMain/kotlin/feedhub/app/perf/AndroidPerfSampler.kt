package feedhub.app.perf

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.Build
import android.os.Debug
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.os.SystemClock
import android.net.TrafficStats
import android.view.Choreographer
import android.view.FrameMetrics
import android.view.Window
import android.view.WindowManager
import feedhub.app.video.ActivePlaybackSessions
import java.io.File

/**
 * 安卓的采样器（任务 §3 的那几项）：只负责"把系统给的数拿出来"，换算 / 分位数 / 增量都在 [PerfRecorder]。
 *
 * 每项都**各自 try/catch**：某一项在这台机器上拿不到（厂商没实现 `CURRENT_NOW`、TrafficStats 不支持 uid 统计、
 * 温度接口返回 NaN）只让那一项是 null，不能把整行采样搞没。
 *
 * 监听器（帧指标、亮屏灭屏）只由 [setEnabled] 挂 / 摘——**开关关着时一个回调都不注册**（任务 §3 的硬要求：
 * 记录器自己不能变成唤醒源）。
 *
 * @param context 应用上下文（`applicationContext` 更安全，但取 `getExternalFilesDir` / 注册粘性广播用哪个都行）。
 * @param window 能拿到 Activity 的窗口就传：`addOnFrameMetricsAvailableListener` 和 `display.refreshRate` 要用。
 *   没有（预览 / 测试 / 应用上下文）就退化成"没有帧统计"（`frames` 为 null），别的照常。
 */
internal class AndroidPerfSampler(
    context: Context,
    window: Window? = null,
) : PerfSampler {
    private val app = context.applicationContext
    private val battery: BatteryManager? = app.getSystemService(Context.BATTERY_SERVICE) as? BatteryManager
    private val power: PowerManager? = app.getSystemService(Context.POWER_SERVICE) as? PowerManager

    @Volatile
    private var window: Window? = window

    @Volatile
    private var enabled: Boolean = false

    // ---- 帧 ----
    private val frameLock = Any()
    private val frameNanos = ArrayList<Long>()

    @Volatile
    private var refreshHz: Float = 60f
    private var framesThread: HandlerThread? = null
    private var framesHandler: Handler? = null
    private var framesListener: Window.OnFrameMetricsAvailableListener? = null

    // ---- 亮屏 / 灭屏 ----
    @Volatile
    private var screenListener: ((Boolean) -> Unit)? = null
    private var screenRegistered = false
    private val screenReceiver = object : BroadcastReceiver() {
        override fun onReceive(c: Context?, intent: Intent?) {
            val lit = intent?.action == Intent.ACTION_SCREEN_ON
            screenOn = lit
            screenListener?.invoke(lit)
        }
    }

    /** 最近一次知道的屏幕状态（灭屏广播之后采到的行也要写对）。 */
    @Volatile
    private var screenOn: Boolean = true

    override fun setEnabled(enabled: Boolean) {
        this.enabled = enabled
        if (enabled) {
            attachFrames()
            registerScreen()
            refreshHz = currentRefreshHz()
            screenOn = runCatching { power?.isInteractive ?: true }.getOrDefault(true)
        } else {
            detachFrames()
            unregisterScreen()
        }
    }

    /** 绑定 / 切换当前 Activity 的窗口（不长期持有已销毁窗口，A4）。 */
    fun setWindow(newWindow: Window?) {
        if (this.window === newWindow) return
        detachFrames()
        this.window = newWindow
        if (enabled && newWindow != null) {
            attachFrames()
            refreshHz = currentRefreshHz()
        }
    }

    fun clearWindow(oldWindow: Window?) {
        if (this.window === oldWindow) {
            setWindow(null)
        }
    }

    override fun setScreenListener(listener: ((Boolean) -> Unit)?) {
        screenListener = listener
    }

    override fun awaitNextFrame(onFrame: (Long) -> Unit) {
        val started = SystemClock.elapsedRealtime()
        // Choreographer 只在自己线程的 Looper 上工作：统一 post 到主线程
        Handler(Looper.getMainLooper()).post {
            runCatching {
                Choreographer.getInstance().postFrameCallback {
                    onFrame((SystemClock.elapsedRealtime() - started).coerceAtLeast(0))
                }
            }
        }
    }

    override fun read(): PerfReading {
        // 帧：先把这一段攒下的取走（取一次就清空），再顺手更新刷新率
        val frames = synchronized(frameLock) {
            val copy = ArrayList(frameNanos)
            frameNanos.clear()
            copy
        }
        val hz = currentRefreshHz()
        refreshHz = hz
        return PerfReading(
            battery = readBattery(),
            cpuTicks = readProcTicks("/proc/self/stat")?.totalTicks,
            threads = readThreads(),
            frames = if (window == null) null else FrameReading(frames, hz),
            network = readNetwork(),
            thermal = readThermal(),
            heapUsed = runCatching { Runtime.getRuntime().let { it.totalMemory() - it.freeMemory() } }.getOrNull(),
            gc = readGc(),
            playback = readPlayback(),
            screenOn = screenOn,
        )
    }

    /**
     * 现在的刷新率（`>0`；拿不到按 60）。`Window.getDisplay()` 在 compileSdk 37 上已经取不到，改用
     * `Context.getDisplay()`（API 30+）/ `WindowManager.defaultDisplay`（26~29），和 `WindowFrameRateSink` 同一套做法。
     */
    @Suppress("DEPRECATION")
    private fun currentRefreshHz(): Float = runCatching {
        val ctx = window?.context ?: app
        val display = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            ctx.display
        } else {
            (ctx.getSystemService(Context.WINDOW_SERVICE) as? WindowManager)?.defaultDisplay
        }
        display?.refreshRate?.takeIf { it > 0f } ?: 60f
    }.getOrDefault(60f)

    // ---------------------------------------------------------------------
    // 电池
    // ---------------------------------------------------------------------

    private fun readBattery(): BatteryReading? {
        val intent = runCatching {
            app.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        }.getOrNull()
        val current = battery?.let { bm ->
            runCatching { bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CURRENT_NOW) }.getOrNull()
        }?.takeIf { it != Int.MIN_VALUE }?.toLong()
        val charge = battery?.let { bm ->
            runCatching { bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CHARGE_COUNTER) }.getOrNull()
        }?.takeIf { it != Int.MIN_VALUE }?.toLong()
        val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)?.takeIf { it >= 0 }
        val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, -1)?.takeIf { it > 0 }
        val voltage = intent?.getIntExtra(BatteryManager.EXTRA_VOLTAGE, -1)?.takeIf { it > 0 }
        if (intent == null && current == null && charge == null) return null
        return BatteryReading(
            currentUa = current,
            chargeUah = charge,
            level = if (level != null && scale != null) level * 100 / scale else null,
            voltageMv = voltage,
            status = when (intent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1)) {
                BatteryManager.BATTERY_STATUS_CHARGING -> "charging"
                BatteryManager.BATTERY_STATUS_DISCHARGING -> "discharging"
                BatteryManager.BATTERY_STATUS_FULL -> "full"
                BatteryManager.BATTERY_STATUS_NOT_CHARGING -> "not_charging"
                BatteryManager.BATTERY_STATUS_UNKNOWN -> "unknown"
                else -> null
            },
            plugged = when (intent?.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1)) {
                BatteryManager.BATTERY_PLUGGED_AC -> "ac"
                BatteryManager.BATTERY_PLUGGED_USB -> "usb"
                BatteryManager.BATTERY_PLUGGED_WIRELESS -> "wireless"
                4 -> "dock" // BATTERY_PLUGGED_DOCK 是 API 33 的常量；数值沿用 4
                0 -> "unplugged"
                else -> "unknown"
            },
        )
    }

    // ---------------------------------------------------------------------
    // /proc
    // ---------------------------------------------------------------------

    private fun readProcTicks(path: String): ProcTicks? = runCatching {
        if (path == "/proc/self/stat") {
            feedhub.perf.RustPerf.readProcSelfStat()?.let {
                ProcTicks(it.pid, it.name, it.utimeTicks, it.stimeTicks)
            } ?: parseProcStat(File(path).readText())
        } else {
            parseProcStat(File(path).readText())
        }
    }.getOrNull()

    private fun readThreads(): List<ProcTicks> {
        val fromRust = runCatching { feedhub.perf.RustPerf.readProcSelfThreads() }.getOrNull()
        if (!fromRust.isNullOrEmpty()) {
            return fromRust.map { ProcTicks(it.pid, it.name, it.utimeTicks, it.stimeTicks) }
        }
        val dirs = runCatching { File("/proc/self/task").listFiles() }.getOrNull() ?: return emptyList()
        val out = ArrayList<ProcTicks>(dirs.size)
        for (d in dirs) {
            val t = readProcTicks("${d.path}/stat") ?: continue
            out += t
        }
        return out
    }

    // ---------------------------------------------------------------------
    // 网络 / 温度 / GC
    // ---------------------------------------------------------------------

    private fun readNetwork(): NetworkReading? = runCatching {
        val uid = Process.myUid()
        val rx = TrafficStats.getUidRxBytes(uid)
        val tx = TrafficStats.getUidTxBytes(uid)
        // TrafficStats.UNSUPPORTED = -1：这台机器不统计（有些厂商 / 老内核）
        if (rx < 0 || tx < 0) null else NetworkReading(rx, tx)
    }.getOrNull()

    private fun readThermal(): ThermalReading? {
        val status = if (Build.VERSION.SDK_INT >= 29) {
            runCatching { power?.currentThermalStatus }.getOrNull()
        } else {
            null
        }
        // getThermalHeadroom 有调用频率限制（同一个 forecast 一秒最多一次）；我们 10 秒一次
        val headroom = if (Build.VERSION.SDK_INT >= 30) {
            runCatching { power?.getThermalHeadroom(10) }.getOrNull()?.takeIf { !it.isNaN() }
        } else {
            null
        }
        if (status == null && headroom == null) return null
        return ThermalReading(status = status, headroom = headroom)
    }

    private fun readGc(): GcReading? = runCatching {
        val count = Debug.getRuntimeStat("art.gc.gc-count")?.toLongOrNull()
        val time = Debug.getRuntimeStat("art.gc.gc-time")?.toLongOrNull()
        if (count == null && time == null) null else GcReading(count, time)
    }.getOrNull()

    // ---------------------------------------------------------------------
    // 播放（只读会话，不改播放器）
    // ---------------------------------------------------------------------

    private fun readPlayback(): List<PlaybackReading> = runCatching {
        ActivePlaybackSessions.alive().map { s ->
            PlaybackReading(
                platform = s.platform.key,
                playing = s.state.playing,
                hardware = s.state.hardwareDecoding,
                qn = s.choice?.quality,
                height = s.choice?.video?.height,
            )
        }
    }.getOrDefault(emptyList())

    // ---------------------------------------------------------------------
    // 帧指标监听（只在开关开时挂）
    // ---------------------------------------------------------------------

    private fun attachFrames() {
        val w = window ?: return
        if (framesListener != null) return
        runCatching {
            val thread = HandlerThread("feedhub-perf-frames").apply { start() }
            val handler = Handler(thread.looper)
            val listener = Window.OnFrameMetricsAvailableListener { _, metrics, _ ->
                val ns = metrics.getMetric(FrameMetrics.TOTAL_DURATION)
                if (ns <= 0) return@OnFrameMetricsAvailableListener
                synchronized(frameLock) {
                    // 10 秒最多 1200 帧（120Hz）；给一倍余量，多了就丢（说明根本没在读，别把内存撑爆）
                    if (frameNanos.size < 4000) frameNanos.add(ns)
                }
            }
            w.addOnFrameMetricsAvailableListener(listener, handler)
            framesThread = thread
            framesHandler = handler
            framesListener = listener
        }.onFailure {
            framesListener = null
            framesHandler = null
            framesThread?.quitSafely()
            framesThread = null
        }
    }

    private fun detachFrames() {
        val listener = framesListener ?: return
        val w = window
        if (w != null) {
            runCatching { w.removeOnFrameMetricsAvailableListener(listener) }
        }
        framesListener = null
        framesHandler = null
        framesThread?.quitSafely()
        framesThread = null
        synchronized(frameLock) { frameNanos.clear() }
    }

    // ---------------------------------------------------------------------
    // 亮屏 / 灭屏（动态注册，只在开关开时）
    // ---------------------------------------------------------------------

    private fun registerScreen() {
        if (screenRegistered) return
        runCatching {
            val filter = IntentFilter().apply {
                addAction(Intent.ACTION_SCREEN_ON)
                addAction(Intent.ACTION_SCREEN_OFF)
            }
            app.registerReceiver(screenReceiver, filter)
            screenRegistered = true
        }
    }

    private fun unregisterScreen() {
        if (!screenRegistered) return
        runCatching { app.unregisterReceiver(screenReceiver) }
        screenRegistered = false
    }
}
