package feedhub.app.image

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executor
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ThreadFactory
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/**
 * 所有动图共用的时钟：**只在有动图被画的时候才跑**。
 *
 * - 动图的 [AnimatedImage.draw] 向这里报到（[onDraw]），报到的图进入"活跃"集合，时钟线程随之启动；
 * - 时钟线程睡到最近一张图该换帧的时刻，叫醒后让每张活跃的图推进播放位置、安排后台解下一帧（[AnimatedImage.step]），
 *   换帧时那张图改自己的快照状态，Compose 只重画画了它的那一层；
 * - 一张图换了帧之后界面一直没重画它（滚出了列表、详情关了、切了页签、窗口被遮住），过一会儿它就被当作离屏：放掉解出来的帧，
 *   从活跃集合里去掉；集合空了时钟线程就退出，之后一点 CPU 都不占；再有动图被画时重新启动。
 *
 * 帧的解码在单独的小线程池里（最多 2 个线程），界面线程只管画解好的帧。
 */
internal object AnimationClock {
    /** [AnimatedImage.step] 的返回值：不用再跟踪这张图了。 */
    const val STOP = -1L

    /** 时间来源（纳秒，单调）。测试和截图工具的虚拟时间会换掉它。 */
    @Volatile
    var timeSource: () -> Long = System::nanoTime

    /** false 时不启动时钟线程，由调用方手动 [tick]（测试用）。 */
    @Volatile
    var autoRun = true

    fun now(): Long = timeSource()

    private val active: MutableSet<AnimatedImage> = ConcurrentHashMap.newKeySet()
    private val lock = Object()
    private var thread: Thread? = null
    private var woken = false

    /** 现在有几张动图在被跟踪（测试用）。 */
    val activeCount: Int get() = active.size

    /** 时钟线程现在是不是在运行（测试用）。 */
    val running: Boolean get() = synchronized(lock) { thread != null }

    private val decodeExecutor: Executor = ThreadPoolExecutor(
        0, 2, 20, TimeUnit.SECONDS, LinkedBlockingQueue(),
        ThreadFactory { r -> Thread(r, "feedhub-anim-decode").apply { isDaemon = true; priority = Thread.NORM_PRIORITY - 1 } },
    )

    fun executeDecode(task: Runnable) = decodeExecutor.execute(task)

    /** 动图被画了一次（界面线程）。 */
    fun onDraw(image: AnimatedImage) {
        image.lastDrawNs = now()
        if (image.finished || image.broken) return
        if (active.add(image) && autoRun) wake(start = true)
    }

    /** 叫醒时钟线程（解完一帧、有新的动图被画了）；[start] 为真且线程没在跑就启动它。 */
    fun wake(start: Boolean = false) {
        synchronized(lock) {
            woken = true
            if (thread == null && start && active.isNotEmpty()) {
                thread = Thread(::run, "feedhub-anim-clock").apply { isDaemon = true; start() }
            }
            lock.notifyAll()
        }
    }

    private fun run() {
        try {
            while (true) {
                val waitMs = tick(now(), inline = false)
                synchronized(lock) {
                    if (active.isEmpty()) {
                        thread = null
                        return
                    }
                    if (!woken) lock.wait(waitMs.coerceIn(1L, MAX_SLEEP_MS))
                    woken = false
                }
            }
        } finally {
            synchronized(lock) { if (thread === Thread.currentThread()) thread = null }
        }
    }

    /**
     * 让每张活跃的图推进到 [now]，返回离下一次最早该叫醒的时刻还有几毫秒（没有活跃的图返回 [MAX_SLEEP_MS]）。
     * 时钟线程每转一圈调一次；[inline] 为真时在调用线程上同步解帧（测试 / 截图工具）。
     */
    fun tick(now: Long, inline: Boolean): Long {
        var next = Long.MAX_VALUE
        for (image in active) {
            val wait = image.step(now, inline)
            if (wait == STOP) {
                active.remove(image)
                // 它被判成不用跟踪的同时界面线程又画了它：别丢
                if (!image.finished && !image.broken && image.lastDrawNs >= now) {
                    active.add(image)
                    next = minOf(next, 1L)
                }
            } else {
                next = minOf(next, wait)
            }
        }
        return if (next == Long.MAX_VALUE) MAX_SLEEP_MS else next
    }

    /** 清掉所有跟踪的图、停掉时钟线程并等它退出（测试用）。 */
    fun reset() {
        val t: Thread?
        synchronized(lock) {
            active.clear()
            t = thread
            woken = true
            lock.notifyAll()
        }
        t?.join(2000)
        timeSource = System::nanoTime
        autoRun = true
    }

    private const val MAX_SLEEP_MS = 1000L
}
