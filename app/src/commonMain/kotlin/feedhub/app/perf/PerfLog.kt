package feedhub.app.perf

/**
 * 性能 / 失败日志的一行。
 *
 * 安卓落到 [android.util.Log]，tag 见实现（`feedhub-perf`，和现有的 `feedhub-proxy` / `feedhub-embed`
 * 是同一套"`adb logcat -s feedhub-xxx`"的用法）；桌面打到 stderr。
 *
 * **只收平台 id / 错误分类 / 毫秒数**：调用方（[PerfHooks.requestFailed]）手里根本没有 URL / 标题 / Cookie。
 */
expect fun perfLogLine(line: String)
