package feedhub.app.video

import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull

/**
 * 窗口现在是不是看得见。桌面：窗口最小化时 false（`Main.kt` 按 WindowState 给）；
 * 安卓暂时一直 true。看不见时：视频照常解码、出声，但**不画帧**（[FrameSurface] 只存最新一帧，看得见了补画一次），
 * 弹幕 / 进度条的循环也停下。
 */
val LocalWindowVisible = compositionLocalOf { true }

/**
 * 进度条多久读一次播放位置。原来是播放中固定 100 ms（行内视频的细进度线是每帧），每次都让整个窗口重画一帧；
 * 现在按"一次大约走 [STEP_PX] 像素"算：一条 [trackPx] 宽的进度条，[durationMs] 长的视频，
 * 间隔 = 时长 × 步长 / 宽度，夹在 [minMs]～[MAX_MS] 之间——短视频照样顺滑，长视频不再每 100 ms 重画一次。
 * 暂停时 [PAUSED_MS]（拖动 / 跳转由状态变化直接触发重画，不靠这个）；不知道时长时按 [minMs]。
 */
fun progressTickMs(playing: Boolean, durationMs: Long, trackPx: Int = DEFAULT_TRACK_PX, minMs: Long = MIN_MS): Long {
    if (!playing) return PAUSED_MS
    if (durationMs <= 0 || trackPx <= 0) return minMs
    return (durationMs * STEP_PX / trackPx).coerceIn(minMs, MAX_MS)
}

/** 播放器 / 沉浸界面那条进度条按多宽算（4K 屏上全宽也就这么多）：宁可算宽，间隔偏短、更顺。 */
const val DEFAULT_TRACK_PX = 1600
const val STEP_PX = 2
const val MIN_MS = 100L
const val MAX_MS = 500L
const val PAUSED_MS = 400L

/**
 * 进度条的一次更新：先等 [tickMs]，播放中有画面时再等下一帧视频（[PlayerState.videoFrames]），然后在**那一帧**里调 [update]——
 * 进度条跟视频同一次重画，不单独多画一帧（原来 30 fps 的片子窗口每秒画 ≈52 帧，多出来的就是进度条自己的定时器）。
 * 等不到视频帧（缓冲、暂停、行内封面还盖着）最多等 [VIDEO_FRAME_WAIT_MS]，然后照样自己画。
 */
suspend fun awaitProgressFrame(state: PlayerState, tickMs: Long, update: (nowMs: Long) -> Unit) {
    delay(tickMs)
    if (state.playing && state.hasFrame) {
        val seen = state.videoFrames
        withTimeoutOrNull(VIDEO_FRAME_WAIT_MS) { snapshotFlow { state.videoFrames }.first { it != seen } }
    }
    // 写在帧回调里：这一帧的重组 / 重画就带上它（写在回调外面要等到下一帧）
    withFrameNanos { update(state.nowMs()) }
}

/**
 * 进度条的一轮等待：**播放中**跟视频帧刷一次（[awaitProgressFrame]）；
 * **暂停 / 播完就不循环**——挂起等真正会改变画面的状态（继续播放、跳转、换了视频、时长 / 缓冲位置出来），
 * 变了才画一帧。
 *
 * 以前暂停时也按 [PAUSED_MS]（400 ms）一直转，转一次就要一帧：真机 Perfetto 里视频页在后台时
 * 主线程仍在被这个循环叫醒（`Choreographer#scheduleVsyncLocked`），白白出帧。
 * 暂停时**没有任何定时器**，`snapshotFlow` 只在快照状态真的变了才醒（不轮询）。
 */
suspend fun awaitProgressUpdate(
    state: PlayerState,
    trackPx: Int = DEFAULT_TRACK_PX,
    minMs: Long = MIN_MS,
    update: (nowMs: Long) -> Unit,
) {
    if (state.playing) {
        awaitProgressFrame(state, progressTickMs(true, state.durationMs, trackPx, minMs), update)
        return
    }
    val seen = ProgressWait(state.playing, state.positionMs, state.durationMs, state.bufferedMs)
    snapshotFlow { ProgressWait(state.playing, state.positionMs, state.durationMs, state.bufferedMs) }
        .first { it != seen }
    withFrameNanos { update(state.nowMs()) }
}

/** 暂停时"等什么变了"：播放意图、位置、时长、已缓冲。都**不是**定时器，是快照状态。 */
private data class ProgressWait(val playing: Boolean, val positionMs: Long, val durationMs: Long, val bufferedMs: Long)

const val VIDEO_FRAME_WAIT_MS = 200L
