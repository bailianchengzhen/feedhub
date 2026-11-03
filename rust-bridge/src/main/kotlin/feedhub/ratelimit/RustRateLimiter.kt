package feedhub.ratelimit

import feedhub.core.RequestPriority
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.CrossProcessRateLimit as FfiCrossProcess
import uniffi.feedhub_ffi.CooldownStore as FfiCooldownStore
import uniffi.feedhub_ffi.FfiCooldownCheck
import uniffi.feedhub_ffi.FfiCookieException
import uniffi.feedhub_ffi.FfiPlatformInterval
import uniffi.feedhub_ffi.FfiQueueSnapshot
import uniffi.feedhub_ffi.FfiRequestPriority
import uniffi.feedhub_ffi.RateLimitQueue as FfiRateLimitQueue
import uniffi.feedhub_ffi.cooldownStoreNew
import uniffi.feedhub_ffi.crossProcessDefaultInterval
import uniffi.feedhub_ffi.crossProcessDefaultIntervals
import uniffi.feedhub_ffi.crossProcessRateLimitNew
import uniffi.feedhub_ffi.crossProcessRateLimitNewWithIntervals
import uniffi.feedhub_ffi.rateLimitQueueNew

/**
 * **限速 / 冷却的 Kotlin 门面**。
 *
 * 决策全在 Rust（`crates/feedhub-core/src/ratelimit.rs`）：谁先发、下一张票什么时候能发、取消之后谁
 * 往前挪、跨进程那个文件里记的是几、冷却到没到。Kotlin 这边只有三件薄活：
 * 1. 把 OkHttp 请求上的 tag（[RequestPriority] / [RateLimitWait]）翻成 Rust 的参数；
 * 2. **睡**（`Thread.sleep`，每 100ms 醒一次问一次，和改动前的 `pollMs` 一样）；
 * 3. 看 `Call.isCanceled()`（取消的请求要退出队列、不发出去）。
 *
 * 见 `core/Http.kt` 的 `MinIntervalInterceptor`（那边是这个门面的唯一使用者）。
 */

/** 一个平台的排队器（Rust `RateLimitQueue` 的句柄）。 */
class RustRateLimitQueue private constructor(
    private val queue: FfiRateLimitQueue,
    private val nowMs: () -> Long,
) {
    /** 一张票（一次调用里的一排队）。`close()` = 取消（已经放行的票是空操作）。 */
    inner class Ticket internal constructor(internal val handle: ULong) : AutoCloseable {
        /** 已经放行了（`tryGrant` 返回过非 null）。 */
        internal var granted: Boolean = false

        /** 取消（没发出去的票让出时间槽）。[atMs] 默认取这个队列的时钟（生产里就是 `nowMs()`）。 */
        fun close(atMs: Long = nowMs()) {
            if (granted) return
            queue.release(handle, atMs)
        }

        override fun close() = close(nowMs())
    }

    fun enqueue(priority: RequestPriority, nowMs: Long = this.nowMs()): Ticket =
        Ticket(queue.enqueue(priority.toFfi(), nowMs))

    /** 队头到点就放行；返回在队列里等了多久（毫秒）。还没轮到返回 null（票留在队列里）。 */
    fun tryGrant(ticket: Ticket, nowMs: Long = this.nowMs()): Long? {
        val waited = queue.tryGrant(ticket.handle, nowMs)
        if (waited != null) ticket.granted = true
        return waited
    }

    /** 还要等多久（毫秒）：非队头给的是"按现在的排法"的预计时刻。 */
    fun waitMs(ticket: Ticket, nowMs: Long = this.nowMs()): Long = queue.waitMs(ticket.handle, nowMs)

    /** 队头还要等多久 + 队列长度（宿主每 100ms 问一次这个就够）。 */
    fun snapshot(nowMs: Long = this.nowMs()): QueueSnapshot {
        val s: FfiQueueSnapshot = queue.snapshot(nowMs)
        return QueueSnapshot(s.headWaitMs, s.size.toInt())
    }

    fun size(): Int = queue.size().toInt()

    /** 这张票按现在的排队"该在什么时刻发出去"。 */
    fun scheduledAt(ticket: Ticket): Long? = queue.scheduledAt(ticket.handle)

    /** 这张票自己的抖动。 */
    fun jitterMs(ticket: Ticket): Long? = queue.jitterMs(ticket.handle)

    companion object {
        /**
         * 建一个（每个平台一个实例，和改动前一样）。
         *
         * `randomSeed` **由调用方注入**：生产里给 `System.nanoTime()`（抖动只要"每次运行不一样"），
         * 测试里给固定值（两边抖动量逐票相同）。
         */
        fun create(
            minIntervalMs: Long,
            jitterMs: Long = 0,
            backgroundGraceMs: Long = DEFAULT_BACKGROUND_GRACE_MS,
            randomSeed: Long = System.nanoTime(),
            nowMs: () -> Long = System::currentTimeMillis,
        ): RustRateLimitQueue {
            RustNative.ensureLoaded()
            return RustRateLimitQueue(
                rateLimitQueueNew(minIntervalMs, jitterMs, backgroundGraceMs, randomSeed),
                nowMs,
            )
        }

        /** 等待切片：被取消的请求最多拖这么久才让出时间槽（和改动前的 `pollMs` 一样）。 */
        const val DEFAULT_POLL_MS: Long = 100L

        /**
         * 后台请求的"保护期"：排队超过 5 秒的后台请求，新来的前台请求排在它后面。
         * 前台请求本来就是用户点出来的、一次两三个；这样后台最多被一串点击推到约"保护期 + 几个槽"。
         */
        const val DEFAULT_BACKGROUND_GRACE_MS: Long = 5_000L
    }
}

/** 队头状态。 */
data class QueueSnapshot(val headWaitMs: Long, val size: Int)

internal fun RequestPriority.toFfi(): FfiRequestPriority = when (this) {
    RequestPriority.INTERACTIVE -> FfiRequestPriority.INTERACTIVE
    RequestPriority.BACKGROUND -> FfiRequestPriority.BACKGROUND
}

/**
 * **跨进程限速**（cli 的 `~/.cache/feedhub/cli-rate/`）：判定在 Rust，拿锁 / 睡 / 写文件在 cli。
 *
 * 文件格式一个字都没变：`<平台>.rate` 里就是一个十进制毫秒时间戳，`<平台>.rate.lock` 是空门闩。
 */
class RustCrossProcessRateLimit private constructor(private val inner: FfiCrossProcess) {
    fun intervalFor(platformKey: String): Long = inner.intervalFor(platformKey)

    fun lastRequestAt(platformKey: String): Long? = inner.lastRequestAt(platformKey)

    /** 还要等多久（毫秒，0 = 现在就能发）。**不写文件**。 */
    fun waitMs(platformKey: String, nowMs: Long): Long = inner.waitMs(platformKey, nowMs)

    /** 记下这次请求的时刻（原子替换）。调用方要先拿锁、而且在 [waitMs] 为 0 之后调。 */
    fun commit(platformKey: String, nowMs: Long) {
        try {
            inner.commit(platformKey, nowMs)
        } catch (e: FfiCookieException.Storage) {
            throw java.io.IOException("写 ${platformKey}.rate 失败：${e.detail}", e)
        }
    }

    companion object {
        fun create(dir: java.nio.file.Path): RustCrossProcessRateLimit {
            RustNative.ensureLoaded()
            return RustCrossProcessRateLimit(crossProcessRateLimitNew(dir.toAbsolutePath().toString()))
        }

        /**
         * 换过间隔表的版本（**测试 / 探针用**：真实间隔 1.5～3 秒，测试等不起）。
         *
         * [intervals] 是"覆盖"：表里没写到的平台照旧走默认那张；[defaultIntervalMs] ≤ 0 时也用默认值
         * （Rust 那边不接受"零间隔"）。
         */
        fun create(
            dir: java.nio.file.Path,
            intervals: Map<String, Long>,
            defaultIntervalMs: Long,
        ): RustCrossProcessRateLimit {
            RustNative.ensureLoaded()
            val overrides = intervals.map { FfiPlatformInterval(it.key, it.value) }
            return RustCrossProcessRateLimit(
                crossProcessRateLimitNewWithIntervals(
                    dir.toAbsolutePath().toString(),
                    overrides,
                    defaultIntervalMs,
                ),
            )
        }

        /** 没有配在表里的平台用的默认间隔（毫秒）。 */
        val DEFAULT_INTERVAL_MS: Long get() = crossProcessDefaultInterval()

        /** 按平台的最小间隔。 */
        val DEFAULT_INTERVALS_MS: Map<String, Long> by lazy {
            crossProcessDefaultIntervals().associate { (it).platform to it.intervalMs }
        }
    }
}

/** 一条冷却记录。 */
data class CooldownEntry(val platform: String, val until: Long, val at: Long, val reason: String)

/** [RustCooldownStore.check] 的结论。 */
sealed interface CooldownCheck {
    data object Clear : CooldownCheck

    data class Cooling(val until: Long, val retryAfterMs: Long, val reason: String) : CooldownCheck
}

/**
 * **风控冷却**（cli 的 `~/.cache/feedhub/cooldown/`）：格式和判定都在 Rust。
 *
 * **App 不受影响**：App 是用户本人在用，这里只是 cli 自己的闸。
 */
class RustCooldownStore private constructor(private val inner: FfiCooldownStore) {
    /** 冷却多久（毫秒）。 */
    val ttlMs: Long get() = inner.ttlMs()

    /** 正在冷却就返回还差多久；过期了就顺手删掉（和 Kotlin `check` 一样）。 */
    fun check(platformKey: String, nowMs: Long): CooldownCheck = when (val r = inner.check(platformKey, nowMs)) {
        is FfiCooldownCheck.Clear -> CooldownCheck.Clear
        is FfiCooldownCheck.Cooling -> CooldownCheck.Cooling(r.until, r.retryAfterMs, r.reason)
    }

    /** 写一笔冷却（覆盖这个平台原来的标记）。失败只该打一行 stderr，不能盖住原来那个错误。 */
    fun mark(platformKey: String, reason: String, nowMs: Long) {
        try {
            inner.mark(platformKey, reason, nowMs)
        } catch (e: FfiCookieException.Storage) {
            System.err.println("feedhub：写 $platformKey 的冷却标记失败（不影响这次的错误）：${e.detail}")
        }
    }

    /** 现在在冷却的平台，按 `until` 从早到晚。 */
    fun list(nowMs: Long): List<CooldownEntry> =
        inner.list(nowMs).map { CooldownEntry(it.platform, it.until, it.at, it.reason) }

    /** 清掉一个平台的冷却。清掉了（文件本来在）返回 true。 */
    fun clear(platformKey: String, nowMs: Long): Boolean = inner.clear(platformKey, nowMs)

    companion object {
        /** 冷却 30 分钟（值在 Rust：`feedhub_core::ratelimit::CooldownStore::DEFAULT_TTL_MS`）。 */
        const val DEFAULT_TTL_MS: Long = 30 * 60 * 1000L

        fun create(dir: java.nio.file.Path): RustCooldownStore {
            RustNative.ensureLoaded()
            return RustCooldownStore(cooldownStoreNew(dir.toAbsolutePath().toString()))
        }
    }
}
