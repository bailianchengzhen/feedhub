package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * 桌面的弹幕层：**还是原来那份 Compose 实现**（[ComposeDanmakuLayer]，`Canvas` + `withFrameNanos`）。
 *
 * 桌面本来就没有这块开销，而且 GStreamer / VLCJ 是把帧画进 Compose 图片里的，弹幕贴上去最省事）。
 */
@Composable
actual fun DanmakuLayer(
    timeline: DanmakuTimeline,
    state: PlayerState,
    settings: DanmakuSettings,
    modifier: Modifier,
    onSurface: Boolean,
) {
    ComposeDanmakuLayer(timeline, state, settings, modifier)
}
