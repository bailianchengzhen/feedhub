package feedhub.core

import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.withContext
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext

/**
 * 一个请求的优先级。
 *
 * 打在请求上：`Request.Builder().tag(RequestPriority::class.java, RequestPriority.INTERACTIVE)`。
 * **没有 tag 就是 [BACKGROUND]**——没改过的平台行为一个字不变（只有一道先来后到的队）。
 *
 * - [INTERACTIVE]：用户正盯着结果等的那几个请求（详情正文、评论、楼中楼、播放地址）。排到后台请求前面，
 *   因为它们每个都占一个 [MinIntervalInterceptor] 的时间槽，晚一个槽就是用户多等 1.5～2.3 秒；
 * - [BACKGROUND]：刷新 / 关注流的作者页 / 沉浸预取 / 表情表这种"顺手取的"，让人等一等没关系。
 */
enum class RequestPriority {
    INTERACTIVE,
    BACKGROUND,
}

/**
 * 一次平台调用在限速器里**排队**等了多久的记账（性能记录里的 `queue_ms`）。
 *
 * 用法：调用方建一个，用 [trackRateLimitWait] 包住这次调用，平台层会把它挂到请求的 tag 上，
 * [MinIntervalInterceptor] 放行时把"睡了多久"累加进来；调用返回（或失败）后读 [totalMs]。
 * **它是线程安全的**：累加发生在 OkHttp 的工作线程上，读发生在调用方线程上。
 *
 * 没有接 [trackRateLimitWait] 的调用（cli、测试、别的平台）不会带这个 tag，限速器也就不记账。
 */
class RateLimitWait {
    private val lock = Any()
    private var total = 0L
    private var everSeen = false

    /** 这次调用在限速器里一共等了多久（毫秒；一次调用可能有好几个请求，加起来）。 */
    val totalMs: Long get() = synchronized(lock) { total }

    /**
     * 有请求真的等过（哪怕只等了 0 毫秒也算"限速器见过它"）。区分"没接记账"和"等了 0"：
     * 性能记录里只写 [seen] 为 true 的 [totalMs]。
     */
    val seen: Boolean get() = synchronized(lock) { everSeen }

    /**
     * 限速器放行时把"睡了多久"累加进来。
     *
     * 跨模块可见性得放开
     * （`internal` 在别的模块里看不到）。它不是给业务代码用的：**只有限速器调**。
     */
    fun add(ms: Long) {
        synchronized(lock) {
            total += ms.coerceAtLeast(0)
            everSeen = true
        }
    }
}

/** [trackRateLimitWait] 的上下文元素（只在本进程里往下传，不跨进程）。 */
private class RateLimitWaitElement(val wait: RateLimitWait) : AbstractCoroutineContextElement(Key) {
    companion object Key : CoroutineContext.Key<RateLimitWaitElement>
}

/** [backgroundRequestScope] 的上下文元素。 */
private class BackgroundRequestElement : AbstractCoroutineContextElement(Key) {
    companion object Key : CoroutineContext.Key<BackgroundRequestElement>
}

/**
 * 这一段代码里发出的平台请求，在限速器里排的队都记到 [wait] 上（见 [RateLimitWait]）。
 * 平台层用 [currentRateLimitWait] 把它挂到请求 tag 上；不接的平台照旧。
 */
suspend fun <T> trackRateLimitWait(wait: RateLimitWait, block: suspend () -> T): T =
    withContext(currentCoroutineContext() + RateLimitWaitElement(wait)) { block() }

/** 现在这段代码有没有要求记账（平台层拼请求时问）。 */
suspend fun currentRateLimitWait(): RateLimitWait? = currentCoroutineContext()[RateLimitWaitElement]?.wait

/**
 * 这一段代码里发出的平台请求一律按 [RequestPriority.BACKGROUND] 排队——**覆盖**平台层给的默认优先级。
 *
 * 现在只有沉浸播放的预取用它（`PlayPrefetcher` 提前取下一条的详情和播放地址）：用户还没滑到那一条，
 * 它不该把用户眼前这条的请求挤到后面去。
 */
suspend fun <T> backgroundRequestScope(block: suspend () -> T): T =
    withContext(currentCoroutineContext() + BackgroundRequestElement()) { block() }

/** 现在是不是在 [backgroundRequestScope] 里（平台层拼请求时问）。 */
suspend fun inBackgroundRequestScope(): Boolean = currentCoroutineContext()[BackgroundRequestElement] != null
