package feedhub.app.video

import androidx.compose.runtime.snapshotFlow
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first

/**
 * 排一次延时任务的抽象（只有"排一次 / 取消"两件事）。安卓上就是 `Handler.postDelayed` +
 * `removeCallbacks`；离线测试里是假时钟（见 `PlaybackTickerTest`），不碰真线程、不用真等。
 *
 * **必须**用同一个 [Runnable] 实例去取消：Kotlin 的 `() -> Unit` 每做一次 SAM 转换都是新的对象，
 * 拿 lambda 去 `removeCallbacks` 是删不掉的（[PlaybackTicker] 只负责持有返回值，细节在这里）。
 */
fun interface TickScheduler {
    /** [delayMs] 之后执行 [action]（只排这一次）；返回取消句柄（还没跑就撤掉，跑过了再调用没有副作用）。 */
    fun post(delayMs: Long, action: () -> Unit): () -> Unit
}

/**
 * 播放器状态的周期同步（位置 / 已缓冲 / 时长）：**只有真的在播时才跑**。
 *
 * 暂停、播完、切后台都不停。
 * 真机 Perfetto 里后果很直接：后台 5 分钟主线程 `Choreographer#doFrame` 稳定 4 Hz（= 250 ms 一次），
 * `DefaultExecutor` / `MessageQueue` 上还跟着一片 `monitor contention`——全是这个 250 ms 的
 * "写 `state.positionMs` → 快照状态变更 → 要一帧 → 什么也没画"。
 *
 * 现在：
 * - 真的在播（[shouldRun]）才排下一次；暂停 / 播完 / 切后台（引擎被 `pause()`）/ 释放 → [stop]；
 * - 恢复播放（[sync] 看到 [shouldRun] 为真）再起；
 * - 暂停时用 [refreshNow] 补读一次真实位置（不然进度条会停在最多 250 ms 之前的地方）。
 *
 * 逻辑抽在 commonMain 里是为了能离线测：`PlaybackTickerTest` 用假时钟断言"暂停后不再排期、恢复后再排"。
 */
class PlaybackTicker(
    private val everyMs: Long,
    private val scheduler: TickScheduler,
    /** 该不该继续跑：**真的在播**（不是"想播"），也没播完 / 没释放。 */
    private val shouldRun: () -> Boolean,
    /** 一次同步（读位置 / 已缓冲 / 时长）。 */
    private val onTick: () -> Unit,
) {
    /** 现在有没有排着下一次（测试用）。 */
    var scheduled: Boolean = false
        private set

    private var cancel: (() -> Unit)? = null

    /** 状态可能变了（播放 / 暂停 / 播完 / 缓冲完了 / 释放）：该跑就起，不该跑就停。重复调用没有副作用。 */
    fun sync() {
        if (!shouldRun()) {
            stop()
        } else if (!scheduled) {
            schedule()
        }
    }

    /** 立刻同步一次（暂停、跳转之后用），不改排期状态。 */
    fun refreshNow() {
        onTick()
    }

    /** 停掉（释放播放器 / Activity 销毁时调）；之后 [sync] 还能重新起来。 */
    fun stop() {
        cancel?.invoke()
        cancel = null
        scheduled = false
    }

    private fun schedule() {
        scheduled = true
        cancel = scheduler.post(everyMs) {
            scheduled = false
            if (!shouldRun()) return@post
            onTick()
            if (shouldRun()) schedule()
        }
    }
}

/**
 * "只在真的在播时每 [everyMs] 干一件事"的循环骨架：弹幕加载、每 5 秒存进度都用它。
 *
 * 以前这两处都是 `while (true) { 干活(); delay(...) }`，不看播放状态、也不看前后台：暂停的视频页在后台照样
 * 每秒 / 每 5 秒醒一次（真机 Perfetto 里后台 `DefaultExecutor` ≈5/s、主线程还会被叫醒出空帧）。
 * 现在不在播就 `snapshotFlow` 挂着等（不轮询），继续播再接着干。
 * [playing] 要读 Compose 状态（`state.playing` 之类），变更通知才会到 [snapshotFlow]。
 */
suspend fun everyWhilePlaying(playing: () -> Boolean, everyMs: Long, onTick: suspend () -> Unit) {
    while (true) {
        if (!playing()) {
            snapshotFlow { playing() }.first { it }
            continue
        }
        onTick()
        delay(everyMs)
    }
}
