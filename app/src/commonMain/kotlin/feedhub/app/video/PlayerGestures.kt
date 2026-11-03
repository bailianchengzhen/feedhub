package feedhub.app.video

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.BrightnessHigh
import androidx.compose.material.icons.filled.FastForward
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.abs
import kotlin.math.roundToInt
import kotlinx.coroutines.delay

/**
 * 常规详情页播放器的手势：
 *
 * - **单击** = 显示 / 隐藏控件条（以前是暂停，用户要叫出控件得先把视频停了）；
 * - **双击** = 播放 / 暂停（不再兼任"放大"，放大只走控件条按钮和转屏）；
 * - **长按** = 临时 3 倍速，松手恢复原倍速；长按时**往上滑**（超过 [PlayerGestureMath.LOCK_UP_DP]）锁定 3 倍速，松手保持；
 *   锁定中**长按** = 临时恢复原倍速，松手回到 3 倍速；锁定中长按**上滑** = 取消锁定；
 * - **横向拖** = 调进度（带时间气泡，松手才真的跳）；
 * - **左侧上下滑** = 亮度（只改这个窗口，不动系统亮度设置；退出播放器还原）；
 * - **右侧上下滑** = 音量（安卓上就是系统媒体音量，见 `Media3PlayerEngine`）。
 *
 * 一次手势只做一件事：方向定了（见 [PlayerGestureMath.kind]）就不再换。每一项都在画面中间给浮层提示（[PlayerHintOverlay]）。
 * 沉浸界面（竖屏视频）是 `controls = false`，**"点一下 = 暂停"是用户以前定的，不变**。
 */
internal object PlayerGestureMath {
    /** 长按加速的倍速。 */
    const val LONG_PRESS_SPEED = 3f

    /** 长按后上滑超过这么多（dp）就锁定倍速。 */
    const val LOCK_UP_DP = 36f

    /** 横向位移要超过竖向的这么多倍才算"在拖进度"，否则算上下滑（亮度 / 音量）。 */
    const val SEEK_RATIO = 1.2f

    /** 拖动的三个方向。 */
    enum class Kind { Seek, Brightness, Volume }

    /** 亮度 / 音量的上下滑：手指往上 = 变大。 */
    fun levelAfter(start: Float, dy: Float, height: Float): Float =
        (start - dy / height.coerceAtLeast(1f)).coerceIn(0f, 1f)

    fun percent(level: Float): Int = (level.coerceIn(0f, 1f) * 100).roundToInt()

    /**
     * 这个拖动在干什么：**先看方向**——横向位移明显大（> 竖向 [SEEK_RATIO] 倍）就是拖进度，不管手指落在哪；
     * 否则是上下滑，按落点分区：左 30% 亮度 / 右 30% 音量 / 中间不做（中上滑留给页面滚动 / 以后的别的功能）。
     * 位移还不到 [slopPx] 就返回 null（还没定）。
     *
     * [verticalLevels] = false 时**竖向拖动一律不认领**（返回 null，也不 consume）：沉浸界面里上下滑是
     * "上一条 / 下一条"，亮度和音量在那边走右侧按钮列的滑块。横向拖进度照旧。
     */
    fun kind(downX: Float, width: Float, dx: Float, dy: Float, slopPx: Float, verticalLevels: Boolean = true): Kind? {
        val ax = abs(dx)
        val ay = abs(dy)
        if (ax < slopPx && ay < slopPx) return null
        if (ax > ay * SEEK_RATIO) return Kind.Seek
        if (!verticalLevels) return null
        val x = downX / width.coerceAtLeast(1f)
        return when {
            x < 0.3f -> Kind.Brightness
            x > 0.7f -> Kind.Volume
            else -> null
        }
    }

    /** 拖进度：整条宽度 = 整个时长（BiliPai 口径），起点是按下时的位置。 */
    fun seekTo(startMs: Long, dx: Float, width: Float, durationMs: Long): Long {
        if (durationMs <= 0) return startMs.coerceAtLeast(0)
        val delta = (dx / width.coerceAtLeast(1f) * durationMs).toLong()
        return (startMs + delta).coerceIn(0, durationMs)
    }

    /** 时间气泡里的文字：目标时间 + 相对现在差多少。 */
    fun seekLabel(targetMs: Long, startMs: Long, durationMs: Long): String {
        val delta = targetMs - startMs
        val sign = if (delta < 0) "-" else "+"
        return "${clock(targetMs)} / ${clock(durationMs)}  $sign${clock(abs(delta))}"
    }
}

/** 手势中间浮层显示什么。 */
internal sealed interface PlayerHint {
    /** 拖进度：目标时间、总时长、相对拖动起点的差值。 */
    data class Seek(val targetMs: Long, val startMs: Long, val durationMs: Long) : PlayerHint

    /** 亮度 / 音量：0~100。 */
    data class Level(val which: Kind, val percent: Int) : PlayerHint

    /** 长按倍速：现在几倍、是否锁定了、要不要带一句说明（"上滑锁定" / "已解除"）。 */
    data class Speed(val rate: Float, val locked: Boolean, val note: String? = null) : PlayerHint

    /** 只有一句话的提示（"已解除倍速锁定"这种状态变化，不是正在进行的倍速）。 */
    data class Notice(val text: String) : PlayerHint
}

/** 亮度 / 音量两种，浮层里只为了挑图标和文字。 */
internal enum class Kind { Brightness, Volume }

/**
 * 触屏上的手势检测：三个 detector 叠在一个 Box 上（tap / 普通拖动 / 长按后拖动），谁先认领这次触摸谁赢，
 * 另一个看到事件被消费就自己收手——这是 Compose 里同时要"单击、双击、长按、拖动"的标准做法。
 *
 * 长按倍速：[onLongPressStart] 按下、
 * [onLongPressSwipeUp] 按着往上滑过 [PlayerGestureMath.LOCK_UP_DP]（一次长按只报一次）、[onLongPressEnd] 松手 / 被取消。
 */
internal fun Modifier.playerTouchGestures(
    durationMs: () -> Long,
    positionMs: () -> Long,
    onTap: () -> Unit,
    onDoubleTap: () -> Unit,
    onLongPressStart: () -> Unit,
    onLongPressSwipeUp: () -> Unit,
    onLongPressEnd: () -> Unit,
    isSpeedHeld: () -> Boolean,
    onSeek: (Long) -> Unit,
    onSeekPreview: (PlayerHint.Seek) -> Unit,
    volumePercent: () -> Int,
    onVolume: (Int) -> Unit,
    brightness: WindowBrightness,
    onLevel: (PlayerHint.Level) -> Unit,
    density: Float,
    /** 竖向拖动是不是亮度（左）/ 音量（右）。沉浸界面里传 false：上下滑留给"上一条 / 下一条"。 */
    verticalLevels: Boolean = true,
): Modifier {
    val lockUpPx = PlayerGestureMath.LOCK_UP_DP * density
    return this
        .pointerInputUnit(onTap, onDoubleTap) {
            detectTapGestures(onTap = { onTap() }, onDoubleTap = { onDoubleTap() })
        }
        // 长按：用 afterLongPress 的拖动，手指一松就是 onDragEnd
        .pointerInputUnit(Unit) {
            var upTravel = 0f
            var swiped = false
            detectDragGesturesAfterLongPress(
                onDragStart = {
                    upTravel = 0f
                    swiped = false
                    onLongPressStart()
                },
                onDrag = { change, drag ->
                    change.consume()
                    upTravel += drag.y
                    if (!swiped && upTravel <= -lockUpPx) {
                        swiped = true
                        onLongPressSwipeUp()
                    }
                },
                onDragEnd = { onLongPressEnd() },
                onDragCancel = { onLongPressEnd() },
            )
        }
        // 普通拖动：横向进度 / 左侧亮度 / 右侧音量。这里自己写一圈而不直接用 detectDragGestures：
        // 那个一过触摸阈值就 consume，会把正在进行的"长按加速"顶掉（长按那边看到事件被消费就取消，倍速弹回去）。
        // 自己写就能先问 [isSpeedHeld]，长按在进行就把这一整场让给它。
        .pointerInputUnit(Unit) {
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false)
                var started = false
                var kind: PlayerGestureMath.Kind? = null
                var startValue = 0f
                var startPos = 0L
                var seekTarget = 0L
                var total = Offset.Zero
                while (true) {
                    val event = awaitPointerEvent()
                    val change = event.changes.firstOrNull { it.id == down.id } ?: break
                    if (!change.pressed) {
                        if (started && kind == PlayerGestureMath.Kind.Seek) onSeek(seekTarget)
                        break
                    }
                    if (!started) {
                        if (isSpeedHeld()) break // 长按加速在跑：这一场不抢（上滑锁定倍速归长按）
                        total = change.position - down.position
                        if (total.getDistance() <= viewConfiguration.touchSlop) continue
                        started = true
                        startPos = positionMs()
                        seekTarget = startPos
                        startValue = when {
                            down.position.x < size.width * 0.3f -> brightness.current() ?: 0.5f
                            down.position.x > size.width * 0.7f -> volumePercent() / 100f
                            else -> 0f
                        }
                        kind = PlayerGestureMath.kind(down.position.x, size.width.toFloat(), total.x, total.y, viewConfiguration.touchSlop, verticalLevels)
                    } else {
                        total += change.positionChange()
                    }
                    val k = kind ?: continue
                    change.consume()
                    when (k) {
                        PlayerGestureMath.Kind.Seek -> {
                            seekTarget = PlayerGestureMath.seekTo(startPos, total.x, size.width.toFloat(), durationMs())
                            onSeekPreview(PlayerHint.Seek(seekTarget, startPos, durationMs()))
                        }
                        PlayerGestureMath.Kind.Brightness -> {
                            val level = PlayerGestureMath.levelAfter(startValue, total.y, size.height.toFloat())
                            brightness.set(level)
                            onLevel(PlayerHint.Level(Kind.Brightness, PlayerGestureMath.percent(level)))
                        }
                        PlayerGestureMath.Kind.Volume -> {
                            val level = PlayerGestureMath.levelAfter(startValue, total.y, size.height.toFloat())
                            onVolume(PlayerGestureMath.percent(level))
                            onLevel(PlayerHint.Level(Kind.Volume, PlayerGestureMath.percent(level)))
                        }
                    }
                }
            }
        }
}

/** `Modifier.pointerInput(keys)` 的短写法（这个文件里用得很多）。 */
private fun Modifier.pointerInputUnit(vararg keys: Any?, block: suspend PointerInputScope.() -> Unit): Modifier =
    this.then(Modifier.pointerInput(*keys, block = block))

/**
 * 手势浮层：拖进度是"目标时间 + 差值"的气泡，亮度 / 音量是图标 + 百分比 + 一条细进度（这两种在画面中间），
 * 长按倍速是"3 倍速 · 上滑锁定"这类文字。只在手势进行中和刚结束的一小会儿出现。
 *
 * 倍速和状态提示（[PlayerHint.Speed] / [PlayerHint.Notice]）是画面**顶部居中的一颗小胶囊**
 */
@Composable
internal fun PlayerHintOverlay(hint: PlayerHint?, modifier: Modifier = Modifier) {
    if (hint == null) return
    if (hint is PlayerHint.Speed || hint is PlayerHint.Notice) {
        TopPill(hint, modifier)
        return
    }
    Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier
                .background(Color.Black.copy(alpha = 0.62f), RoundedCornerShape(10.dp))
                .padding(horizontal = 16.dp, vertical = 10.dp),
        ) {
            when (hint) {
                is PlayerHint.Seek -> {
                    Text(
                        PlayerGestureMath.seekLabel(hint.targetMs, hint.startMs, hint.durationMs),
                        color = Color.White,
                        fontSize = 15.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
                is PlayerHint.Level -> {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            when (hint.which) {
                                Kind.Brightness -> Icons.Filled.BrightnessHigh
                                Kind.Volume -> Icons.AutoMirrored.Filled.VolumeUp
                            },
                            contentDescription = null,
                            tint = Color.White,
                            modifier = Modifier.size(18.dp),
                        )
                        Text(" ${hint.percent}%", color = Color.White, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                    }
                    LevelBar(hint.percent)
                }
                // 倍速 / 状态提示在上面的 TopPill 里
                is PlayerHint.Speed, is PlayerHint.Notice -> Unit
            }
        }
    }
}

/** 顶部小胶囊：一行，倍速 + 说明（说明字小一号、淡一点）。 */
@Composable
private fun TopPill(hint: PlayerHint, modifier: Modifier) {
    Box(modifier.fillMaxSize().padding(top = 14.dp), contentAlignment = Alignment.TopCenter) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(50))
                .padding(horizontal = 12.dp, vertical = 5.dp),
        ) {
            when (hint) {
                is PlayerHint.Speed -> {
                    if (hint.rate > 1f) Icon(Icons.Filled.FastForward, contentDescription = null, tint = HintAccent, modifier = Modifier.size(15.dp))
                    Text(
                        " ${speedText(hint.rate)}${if (hint.locked) " · 已锁定" else ""}",
                        color = Color.White,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    hint.note?.let { Text("  $it", color = Color.White.copy(alpha = 0.75f), fontSize = 11.sp) }
                }
                is PlayerHint.Notice -> Text(hint.text, color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                else -> Unit
            }
        }
    }
}

private fun speedText(rate: Float): String = if (rate == rate.toInt().toFloat()) "${rate.toInt()} 倍速" else "${rate}x"

@Composable
private fun LevelBar(percent: Int) {
    Box(
        Modifier.padding(top = 6.dp).width(120.dp).height(3.dp)
            .background(Color.White.copy(alpha = 0.3f), RoundedCornerShape(2.dp)),
    ) {
        Box(Modifier.fillMaxWidth(percent.coerceIn(0, 100) / 100f).height(3.dp).background(Color.White, RoundedCornerShape(2.dp)))
    }
}

private val HintAccent = Color(0xFFFB7299)

/** 浮层在手势结束后再停留这么久（拖动中一直在，不闪）。 */
internal const val HINT_LINGER_MS = 900L
