package feedhub.core

import kotlinx.coroutines.currentCoroutineContext
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext
import kotlinx.coroutines.ThreadContextElement

/**
 * **"这一次调用自己发起了几个 HTTP 请求"**。
 *
 * 桌面 App 的对外接口以前是"进程内计数器前后取差值"，
 * 用户 App 同时在刷信息流时那个数会偏大，agent 的请求预算就核不准。现在改成**按调用**：
 * 接口每处理一个请求就建一个 [RequestScope] 放进协程上下文，这次调用里发出的 HTTP 请求记在它身上；
 * App 自己取流的请求不在这个上下文里，于是一个都不算进来（并发两个接口调用也各记各的）。
 *
 * 两处记账（互不重叠，同一个请求不会被记两次）：
 * 1. **异步发送**（`Call.await()` / 各平台的 `awaitBytes` / `fetchBytes`）：发起调用时在**协程上下文**里记一笔
 *    （[recordHttpCall]）——OkHttp 的拦截器在别的线程上跑，看不到协程上下文，所以只能在发起处记；
 * 2. **同步发送**（`Call.execute()`，例如 Rust 宿主回调那条路）：`SourceContext.onRequest` 的拦截器
 *    在**同一条线程**上跑，用 [currentThread] 取当前调用的 scope 记一笔（AppGraph 里挂的那一下）。
 *
 * [ThreadContextElement] 那部分是为了让第 2 条在 `withContext(Dispatchers.IO)` 之后也成立：
 * 协程在别的线程上恢复时把 scope 放进线程局部变量，恢复原来的值（不会漏到别的协程上）。
 */
class RequestScope : ThreadContextElement<RequestScope?>, AbstractCoroutineContextElement(Key) {
    companion object Key : CoroutineContext.Key<RequestScope> {
        private val onThread = ThreadLocal<RequestScope?>()

        /**
         * 当前**这条线程**正在服务的调用（没有就是 null）。
         * 只该在 HTTP 拦截器这种"同步回调"里读：异步回调跑在 OkHttp 自己的线程上，那里本来就没有值。
         */
        fun currentThread(): RequestScope? = onThread.get()
    }

    private val count = AtomicInteger()

    /** 这次调用到目前为止发起了几个 HTTP 请求。 */
    val requests: Int get() = count.get()

    /** 记一笔（计数只增不减，几路读它都是同一个数）。 */
    fun record() {
        count.incrementAndGet()
    }

    override fun updateThreadContext(context: CoroutineContext): RequestScope? {
        val previous = onThread.get()
        onThread.set(this)
        return previous
    }

    override fun restoreThreadContext(context: CoroutineContext, oldState: RequestScope?) {
        if (oldState == null) onThread.remove() else onThread.set(oldState)
    }
}

/**
 * 在**发起**一次 HTTP 调用之前记一笔（异步发送的记账点，见 [RequestScope]）。
 *
 * 当前协程不在任何 [RequestScope] 里（App 自己刷信息流、cli、测试）就什么都不做——
 * 那些请求不该算到 agent 的预算上。
 */
suspend fun recordHttpCall() {
    currentCoroutineContext()[RequestScope]?.record()
}
