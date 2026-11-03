package feedhub.app.perf

/**
 * 性能日志的 logcat tag。和 `feedhub-proxy` / `feedhub-embed` 同一套用法：
 * `adb logcat -s feedhub-perf`（release 包也能用，`Log.i` 不会被去掉）。
 */
internal const val PERF_LOG_TAG = "feedhub-perf"

/** [perfLogLine] 的安卓实现：一行 `Log.i`（不带任何内容 / 凭据，见公共声明）。 */
actual fun perfLogLine(line: String) {
    android.util.Log.i(PERF_LOG_TAG, line)
}
