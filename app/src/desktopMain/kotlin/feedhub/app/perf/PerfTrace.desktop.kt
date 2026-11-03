package feedhub.app.perf

/**
 * 桌面的追踪标记：**空实现**（任务 §8 "桌面空实现，不用做"）。
 *
 * 桌面没有 Perfetto / systrace 这套东西，`AppGraph.perf` 也是 null（桌面不记录），这里只要保证公共代码
 * 里的 `traceSection` 能编译、行为不变。
 */
actual fun perfTraceBegin(name: String) = Unit

actual fun perfTraceEnd() = Unit

actual fun perfTraceBeginAsync(name: String): Int = -1

actual fun perfTraceEndAsync(name: String, cookie: Int) = Unit
