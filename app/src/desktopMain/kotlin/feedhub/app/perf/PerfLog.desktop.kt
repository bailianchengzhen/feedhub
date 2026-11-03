package feedhub.app.perf

/**
 * [perfLogLine] 的桌面实现：打到 stderr（桌面没有 logcat；`app --record` 之类看控制台时能直接看到）。
 *
 * 桌面本来就不记性能记录（`AppGraph.perf` 是 null），这一行只在失败时出现，不算噪音。
 */
actual fun perfLogLine(line: String) {
    System.err.println("[feedhub-perf] $line")
}
