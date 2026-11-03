package feedhub.app.perf

/**
 * 自己的追踪标记：安卓落到 `android.os.Trace`（Perfetto / systrace 能直接看到），
 * 桌面是空实现。
 *
 * 为什么用 `android.os.Trace` 而不是 `androidx.tracing`：这个模块没有 androidx.tracing 依赖；`android.os.Trace` 从 API 18 起就有，异步切片
 * （[perfTraceBeginAsync] / [perfTraceEndAsync]）从 API 29 起有，这台机器（国产 ROM / Android 16）全都能用。
 *
 * **同步切片必须同一个线程 begin / end**（systrace 的约定）；跨线程的（打开播放会话 → 首帧、图片解码）用
 * 异步切片，见 [traceSectionAsync]。
 */
expect fun perfTraceBegin(name: String)

expect fun perfTraceEnd()

/** 异步切片：返回一个 cookie，之后在**任意线程**用同一个名字 + cookie 结束。 */
expect fun perfTraceBeginAsync(name: String): Int

expect fun perfTraceEndAsync(name: String, cookie: Int)

/**
 * 包住一段同步的活，Perfetto 里就能看到"这段时间在干什么"。标记名用短英文（`feed.load.bilibili`）。
 * 没开性能记录也照常打（没接 Perfetto 时 `Trace` 基本是空操作，开销可以忽略；这样"以后拿 Perfetto 录"才现成有用）。
 */
inline fun <T> traceSection(name: String, block: () -> T): T {
    perfTraceBegin(name)
    try {
        return block()
    } finally {
        perfTraceEnd()
    }
}

/** 开始一段跨线程的切片，返回 cookie（给 [traceEndAsync]）；桌面 / 没接上时返回负数，结束调用会忽略。 */
fun traceBeginAsync(name: String): Int = perfTraceBeginAsync(name)

/** 结束 [traceBeginAsync] 开的那一段。 */
fun traceEndAsync(name: String, cookie: Int) = perfTraceEndAsync(name, cookie)

/** 切片名的最大长度（`android.os.Trace` 的硬限制）。 */
internal const val PERF_TRACE_NAME_MAX = 127

/** 标记名截到系统允许的长度（超出的部分 Android 会直接截断，这里自己截，行为可测）。 */
internal fun traceName(name: String): String = if (name.length <= PERF_TRACE_NAME_MAX) name else name.take(PERF_TRACE_NAME_MAX)
