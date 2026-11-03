package feedhub.app.perf

/**
 * 只给截图 / 调试用的口子（跟 `AppDebug` 一个用途）：
 *
 * 桌面组装没有 `PerfRecorder`（[feedhub.app.AppGraph.perf] 为 null），设置页那张卡片在桌面上就只会显示
 * "这个构建不记录"。截图要看"记录开着、显示路径和大小"的样子，用 `perf=1` / `perfsize=` 塞一份假状态进来。
 * 只在截图工具里写，App 里没人写它，所以真实行为不受影响。
 */
object PerfDebug {
    /** 非 null 时设置页显示它（代替真实的目录状态）。 */
    @Volatile
    var statusOverride: PerfStatus? = null
}
