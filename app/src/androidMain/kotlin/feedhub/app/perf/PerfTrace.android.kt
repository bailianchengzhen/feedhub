package feedhub.app.perf

import android.os.Build
import android.os.Trace
import java.util.concurrent.atomic.AtomicInteger

/** 安卓的追踪标记：`android.os.Trace`（Perfetto / systrace 直接能看到）。 */
actual fun perfTraceBegin(name: String) {
    Trace.beginSection(traceName(name))
}

actual fun perfTraceEnd() {
    Trace.endSection()
}

private val asyncCookie = AtomicInteger(1)

actual fun perfTraceBeginAsync(name: String): Int {
    // 异步切片 API 29 起才有；更老的系统上退化成"没有这一段"（不是错）
    if (Build.VERSION.SDK_INT < 29) return -1
    val cookie = asyncCookie.getAndIncrement()
    Trace.beginAsyncSection(traceName(name), cookie)
    return cookie
}

actual fun perfTraceEndAsync(name: String, cookie: Int) {
    if (Build.VERSION.SDK_INT < 29 || cookie < 0) return
    Trace.endAsyncSection(traceName(name), cookie)
}
