package feedhub.core

import feedhub.ratelimit.RustRateLimitQueue
import okhttp3.Call
import okhttp3.Interceptor
import okhttp3.Response
import java.io.IOException

/**
 * 同一个客户端两次请求之间至少隔 [minIntervalMs]，再加 0~[jitterMs] 毫秒的随机抖动（风控严的平台用，间隔不要太整齐）。
 * 每个平台一个实例，防止刷太快触发风控。
 * 在 OkHttp 的工作线程里 sleep，不占调用方线程。
 *
 * **排队、优先级、取消**：所有请求排成一条队，
 * 以前是"进来就往后占一个时间槽再睡"，现在多了三件事：
 * - **取消的请求不发、还让出时间槽**：边睡边看 `chain.call().isCanceled()`（每 [pollMs] 醒一次），
 *   被取消就退出队列、后面的往前挪。以前取消不打断 `Thread.sleep`，睡醒照样发出去——白占一个风控名额，
 *   还把用户真正要的请求推得更晚（连点几条详情就"完全不动"）；
 * - **前台优先**：请求上打了 [RequestPriority.INTERACTIVE] 的（用户正等着的详情 / 评论 / 播放）插到后台请求前面；
 *   后台请求（刷新 / 预取 / 表情表）不会因此被饿死——等超过 [backgroundGraceMs] 的后台请求不再被新来的前台请求插队；
 * - **记账**：请求 tag 里带 [RateLimitWait] 的，把"睡了多久"累加进去（性能记录里的 `queue_ms`，见 [trackRateLimitWait]）。
 *
 * **最小间隔一毫秒都没有缩短**：任意两个真正发出去的请求之间仍然 ≥ `minIntervalMs` + 抖动；
 * 优先级只改"谁先谁后"，不改总速率，也没有引入新的并发（放行一个才轮到下一个）。
 *
 * [nowMs] / [sleepMs] 只在测试里换掉（离线测试要断言"这次请求要求等多久"，不想真睡）；
 * 生产里就是 `System.currentTimeMillis` + `Thread.sleep`。[pollMs] ≤ 0 表示不切片（一觉睡到点），
 * 只在拿不到 `Call`（测试里的假 chain）或者老测试里用。
 */
class MinIntervalInterceptor(
    private val minIntervalMs: Long,
    private val jitterMs: Long = 0,
    /**
     * 抖动用的随机源。
     * 这个参数只剩"兼容老调用点 + 测试里给固定种子"的意义——
     * 它的 `nextLong()` 被当作 Rust 那边的种子（同一个种子两边摇出同一串抖动，
     * `RateLimitQueueParityTest` 拿它逐票对拍）。
     */
    random: java.util.Random = java.util.Random(),
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val sleepMs: (Long) -> Unit = { Thread.sleep(it) },
    /** 等待时每一小段睡多久：睡醒一次看一眼取消 / 队列有没有变（0 = 一觉睡到点）。 */
    private val pollMs: Long = DEFAULT_POLL_MS,
    /** 后台请求等了这么久（毫秒）之后，新来的前台请求不再插到它前面（防止后台被饿死）。 */
    private val backgroundGraceMs: Long = DEFAULT_BACKGROUND_GRACE_MS,
) : Interceptor {
    /**
     * **排队决策在 Rust**（`feedhub-core/src/ratelimit.rs` 的 `RateLimitQueue`）：谁先发、下一张票
     * 什么时候能发、取消之后谁往前挪，全在那边。这里只做三件事：把 tag 翻成参数、**睡**、
     * 看 `Call` 取消（和 Kotlin 原版逐条对齐，`MinIntervalInterceptorTest` / `RateLimitQueueParityTest` 守着）。
     */
    private val queue = RustRateLimitQueue.create(
        minIntervalMs = minIntervalMs,
        jitterMs = jitterMs,
        backgroundGraceMs = backgroundGraceMs,
        randomSeed = random.nextLong(),
        nowMs = nowMs,
    )

    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        val priority = request.tag(RequestPriority::class.java) ?: RequestPriority.BACKGROUND
        val wait = request.tag(RateLimitWait::class.java)
        // 拿不到 Call（测试里的假 chain）就查不了取消：只能一觉睡到点，行为和加这段之前一样
        val call = runCatching { chain.call() }.getOrNull()
        val ticket = queue.enqueue(priority)
        try {
            while (true) {
                if (call?.isCanceled() == true) throw IOException(CANCELED)
                val waitedMs = queue.tryGrant(ticket)
                if (waitedMs != null) {
                    wait?.add(waitedMs)
                    return chain.proceed(request)
                }
                sleep(queue.waitMs(ticket), call)
            }
        } catch (e: Throwable) {
            // 没发出去的请求把时间槽让出来（已经放行的票是空操作）
            ticket.close()
            throw e
        }
    }

    /** 睡一小段（[pollMs]）：睡醒后由调用方重新看一眼取消 / 队列。 */
    private fun sleep(remainingMs: Long, call: Call?) {
        if (remainingMs <= 0) return
        val chunk = if (call == null || pollMs <= 0) remainingMs else minOf(remainingMs, pollMs)
        try {
            sleepMs(chunk)
        } catch (e: InterruptedException) {
            // OkHttp 的线程池会复用这条线程，别把中断标记留给下一个任务
            Thread.interrupted()
            throw IOException(CANCELED, e)
        }
    }

    companion object {
        /** 等待切片：被取消的请求最多拖这么久才让出时间槽（100ms 的排队唤醒，只在真的有请求排队时发生）。 */
        const val DEFAULT_POLL_MS: Long = RustRateLimitQueue.DEFAULT_POLL_MS

        /**
         * 后台请求的"保护期"：排队超过 5 秒的后台请求，新来的前台请求排在它后面。
         * 前台请求本来就是用户点出来的、一次两三个；这样后台最多被一串点击推到约"保护期 + 几个槽"，
         * 不会出现"后台请求永远排在最后"。
         */
        const val DEFAULT_BACKGROUND_GRACE_MS: Long = RustRateLimitQueue.DEFAULT_BACKGROUND_GRACE_MS

        /** 和 OkHttp 自己的取消文案一致（`Call.await()` 那条路上会作为 IOException 抛出去）。 */
        private const val CANCELED = "Canceled"
    }
}

