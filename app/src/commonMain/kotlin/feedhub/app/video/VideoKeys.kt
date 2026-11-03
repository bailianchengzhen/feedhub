package feedhub.app.video

import androidx.compose.ui.input.key.Key

/** ↑ / ↓ 调音量的步长（和常规播放器一致）。 */
const val VolumeKeyStep = 5

/**
 * 视频详情页的键盘动作。**沉浸界面和常规详情页共用同一套**：
 * - 常规详情页：↑ / ↓ 调音量；
 * - 沉浸界面（[immersion] = true）：↑ / ↓ = **上一条 / 下一条**，音量在那边走 **Shift + ↑↓**（界面把 shift 按下时
 *   按 [immersion] = false 再问一次，于是拿到的还是调音量）。
 *
 * 界面自己处理 Esc（退出放大 / 关评论面板）和有状态的那些分支，其余的键问这里要动作；
 * 返回 null = 这个键不归视频页管，往上交给详情页（[feedhub.app.detail.DetailPane]）。
 */
sealed interface VideoKeyAction {
    data object TogglePlay : VideoKeyAction

    /** 快退 / 快进 [deltaMs]（负数 = 快退）。 */
    data class Seek(val deltaMs: Long) : VideoKeyAction

    /** 调音量 [delta]（百分数，正 = 大声）。 */
    data class Volume(val delta: Int) : VideoKeyAction

    data object ToggleMute : VideoKeyAction

    /** 倍速快一档 / 慢一档。 */
    data class Speed(val delta: Int) : VideoKeyAction

    data object ToggleZoom : VideoKeyAction

    /** 沉浸界面里换上一条（-1）/ 下一条（+1）。常规详情页没有这个动作。 */
    data class Step(val delta: Int) : VideoKeyAction
}

/**
 * 按键 → 动作。PageUp / PageDown 只在沉浸界面里是上一条 / 下一条，常规详情页返回 null（和以前一样）。
 */
fun videoKeyAction(key: Key, immersion: Boolean = false): VideoKeyAction? = when (key) {
    Key.Spacebar -> VideoKeyAction.TogglePlay
    Key.DirectionLeft -> VideoKeyAction.Seek(-5000)
    Key.DirectionRight -> VideoKeyAction.Seek(5000)
    Key.DirectionUp -> if (immersion) VideoKeyAction.Step(-1) else VideoKeyAction.Volume(VolumeKeyStep)
    Key.DirectionDown -> if (immersion) VideoKeyAction.Step(1) else VideoKeyAction.Volume(-VolumeKeyStep)
    Key.PageUp -> if (immersion) VideoKeyAction.Step(-1) else null
    Key.PageDown -> if (immersion) VideoKeyAction.Step(1) else null
    Key.M -> VideoKeyAction.ToggleMute
    Key.LeftBracket -> VideoKeyAction.Speed(-1)
    Key.RightBracket -> VideoKeyAction.Speed(1)
    Key.F -> VideoKeyAction.ToggleZoom
    else -> null
}
