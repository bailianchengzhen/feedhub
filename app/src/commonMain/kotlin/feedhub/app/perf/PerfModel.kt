package feedhub.app.perf

/**
 * 一次采样的**原始读数**（各平台实现填，见 [PerfSampler]）。这里一个数都不换算：
 * 增量、百分比、分位数都在 [PerfRecorder] 里算（好离线测），平台侧只负责"把系统给的数拿出来"。
 *
 * 拿不到 / 这台机器不支持的一律 null（**不编 0**）——`0` 和"没有这个数"在分析脚本里是两回事。
 */
data class PerfReading(
    val battery: BatteryReading? = null,
    /** 本进程的 utime + stime（clock tick，1 tick = 10ms，见 [PERF_CLOCK_TICKS_PER_SEC]）。 */
    val cpuTicks: Long? = null,
    /** `/proc/self/task/<tid>` 里每个线程的 utime + stime。 */
    val threads: List<ProcTicks> = emptyList(),
    /** 上次读之后新交的帧（读一次就清空）。 */
    val frames: FrameReading? = null,
    /** 本进程 uid 的累计收发字节（`TrafficStats`）。 */
    val network: NetworkReading? = null,
    val thermal: ThermalReading? = null,
    /** Java 堆已用字节。 */
    val heapUsed: Long? = null,
    val gc: GcReading? = null,
    /** 现在活着的播放会话（0~n 个）。 */
    val playback: List<PlaybackReading> = emptyList(),
    /** 屏幕亮着吗。 */
    val screenOn: Boolean? = null,
)

/**
 * 电池。
 *
 * **单位（任务要求查清楚、原值照存）**：
 * - [currentUa] = `BatteryManager.getIntProperty(BATTERY_PROPERTY_CURRENT_NOW)` 的**原值**，按 Android 文档是
 *   **微安（µA）**：正值 = 电流从充电器**进**电池，负值 = 电池在**放电**。有些机型的驱动按毫安报（社区里
 *   Pixel / 三星都出现过），所以日志里同时写 [currentUnit]（这台机器按文档填 "uA"），**不在这里换算成 mA**；
 *   分析脚本只做展示，发现量级明显不对（例如 -3000 而不是 -3000000）就按毫安提示。
 * - [chargeUah] = `CHARGE_COUNTER`，Android 文档是**微安时（µAh）**，部分机型上常见的是 µAh 量级的整数。
 * - [level] 百分比 0~100；[voltageMv] 毫伏（`ACTION_BATTERY_CHANGED` 的 `EXTRA_VOLTAGE`）。
 */
data class BatteryReading(
    val currentUa: Long? = null,
    val currentUnit: String = "uA",
    val chargeUah: Long? = null,
    val level: Int? = null,
    val voltageMv: Int? = null,
    /** `charging` / `discharging` / `full` / `not_charging` / `unknown`。 */
    val status: String? = null,
    /** `ac` / `usb` / `wireless` / `dock` / `unplugged` / `unknown`。 */
    val plugged: String? = null,
)

data class NetworkReading(val rxBytes: Long, val txBytes: Long)

/**
 * 温度。
 *
 * [status] = `PowerManager.currentThermalStatus`（0 = none，1 = light，2 = moderate，3 = severe，4 = critical，
 * 5 = emergency，6 = shutdown；数值越大越热）。
 * [headroom] = `getThermalHeadroom(10)`：0~1，**越接近 1 越接近降频**（1 = 已经在节流）；
 * 文档要求同一个 forecast 秒数不要一秒内调多次，我们 10 秒一次，安全。
 */
data class ThermalReading(val status: Int? = null, val headroom: Float? = null)

/** ART 的 GC 账（`Debug.getRuntimeStat`）：累计次数 / 累计耗时（毫秒），两者都是累计值，日志里存增量。 */
data class GcReading(val count: Long? = null, val timeMs: Long? = null)

/** 一次读帧的窗口：每帧耗时（纳秒）+ 当时的刷新率（判断"超过截止时间"用）。 */
data class FrameReading(val durationsNs: List<Long>, val refreshHz: Float)

/** 一个播放会话的现状（从 `ActivePlaybackSessions` 读，只读，不改播放器）。 */
data class PlaybackReading(
    val platform: String,
    val playing: Boolean,
    /** 硬解 = true / 软解 = false / 还不知道 = null。 */
    val hardware: Boolean? = null,
    /** 平台自己的清晰度代号（B站 qn：80 = 1080P、64 = 720P…），原值。 */
    val qn: Int? = null,
    /** 这一路流的高度（像素），能拿到就记，脚本里更好读。 */
    val height: Int? = null,
)

/**
 * 平台采样器：安卓实现见 `androidMain/.../perf/AndroidPerfSampler.kt`；桌面不给（[feedhub.app.AppGraph.perf] 为 null，
 * 什么都不记）。测试用假实现（`desktopTest/.../perf/PerfRecorderTest.kt`）。
 */
interface PerfSampler {
    /** 读一次（可能有点慢：读 /proc、取粘性广播，所以调用方保证不在主线程）。 */
    fun read(): PerfReading

    /** 开关开了才挂监听（帧指标 / 亮屏灭屏广播）。 */
    fun setEnabled(enabled: Boolean) {}

    /** 下一次真的画出帧时回调（毫秒 = 从调用到现在）；用来量"回前台第一帧"。 */
    fun awaitNextFrame(onFrame: (Long) -> Unit) {}

    /** 亮屏 / 灭屏的接收方（安卓实现动态注册 `ACTION_SCREEN_ON/OFF`）；null = 摘掉。 */
    fun setScreenListener(listener: ((Boolean) -> Unit)?) {}
}
