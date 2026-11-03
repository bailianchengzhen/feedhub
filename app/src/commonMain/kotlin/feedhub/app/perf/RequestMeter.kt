package feedhub.app.perf

import feedhub.core.Platform
import feedhub.core.RateLimitWait
import feedhub.core.trackRateLimitWait
import java.net.SocketTimeoutException
import kotlin.coroutines.CoroutineContext
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/**
 * 用户正等着的一次平台请求（性能记录里的 `<key>` 成功事件 / `<key>_failed` 失败事件）。
 *
 * 只有这四种：详情首屏、评论第一页、楼中楼、播放地址。信息流刷新有自己的事件（`feed_refresh`）。
 */
enum class PerfRequest(val key: String) {
    DETAIL("detail"),
    COMMENTS("comments"),
    REPLIES("replies"),
    PLAY("play"),
}

/**
 * 详情 / 评论 / 楼中楼 / 播放这类"用户正等着"的请求的**总超时**。
 *
 * 为什么要有：这几个调用以前没有任何总超时——限速器里排队的时间不算在网络超时里，关注流那种一刷新十几个作者
 * 的队列能把用户的详情请求推到几十秒以后（`MinIntervalInterceptor` 现在有前台优先级 + 取消让位，
 * 这里是最后一道保险）。超时按网络错误抛（[SocketTimeoutException] 是 `IOException`），界面显示"网络错误"+重试。
 *
 * 30 秒的取法：最坏情况下详情前面还压着一个刚发出的请求（1.5～2.3 秒）＋ 一次请求自己的网络耗时（读超时 30 秒
 * 是平台客户端的值，这里要比"排队 + 一次正常请求"宽，但不能宽到让人觉得"完全不动"）。
 */
const val INTERACTIVE_REQUEST_TIMEOUT_MS: Long = 30_000L

/** 一次请求的结果 + 记账（耗时 / 限速器排队）。 */
class Metered<T>(val value: T, val dMs: Long, val queueMs: Long?)

/**
 * 包一次"用户正等着"的平台请求，顺手把三件事做了：
 *
 * 1. **排队记账**：用 [RateLimitWait] 包住调用（平台层把它挂到请求 tag 上，限速器填"睡了多久"），
 *    结果里的 [Metered.queueMs] 就是性能记录里的 `queue_ms`——下次看日志能分清慢在排队还是慢在网络；
 * 2. **总超时**：见 [INTERACTIVE_REQUEST_TIMEOUT_MS]，超时按网络错误抛；
 * 3. **失败也记**：失败（含超时、取消）写一条 `<key>_failed` 事件 + 一行 logcat（[PerfHooks.requestFailed]），
 *    然后把异常原样抛出去（界面 / 调用方的错误处理一个字都不用改）。
 *
 * 成功不在这里打点：各调用点写的事件字段不一样（详情要内容类型、评论要条数），由调用方拿 [Metered] 里的值自己写。
 */
suspend fun <T> meteredRequest(
    kind: PerfRequest,
    platform: Platform,
    io: CoroutineContext,
    timeoutMs: Long = INTERACTIVE_REQUEST_TIMEOUT_MS,
    block: suspend () -> T,
): Metered<T> {
    val startedAt = PerfHooks.nowMs()
    val wait = RateLimitWait()
    return try {
        val value = withContext(io) {
            trackRateLimitWait(wait) { withTimeout(timeoutMs) { block() } }
        }
        Metered(
            value = value,
            dMs = (PerfHooks.nowMs() - startedAt).coerceAtLeast(0),
            queueMs = wait.takeIf { it.seen }?.totalMs,
        )
    } catch (e: Throwable) {
        val dMs = (PerfHooks.nowMs() - startedAt).coerceAtLeast(0)
        val queueMs = wait.takeIf { it.seen }?.totalMs
        // 超时转成网络错误：`describe` / `friendlyError` 都把它当断网那一档（界面照常给"重试"）
        val error: Throwable = if (e is TimeoutCancellationException) {
            SocketTimeoutException(feedhub.perf.RustPerf.timeoutMessage(timeoutMs))
        } else {
            e
        }
        PerfHooks.requestFailed(kind, platform, error, dMs, queueMs)
        throw error
    }
}
