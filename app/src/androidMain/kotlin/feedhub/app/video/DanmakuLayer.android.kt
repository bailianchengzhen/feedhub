package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.viewinterop.AndroidView

/**
 * 安卓的弹幕层：**自己一块 `SurfaceView` + 自己的画线程**。
 *
 * 这里只做三件事：
 * 1. 把"现在该不该动、倍速、设置、暂停在哪"这几个值**在界面线程**读出来（都是 Compose 状态；播放中它们不变，
 *    所以弹幕动的时候这个 composable 不重组、主窗口不重画）；
 * 2. 建 / 释放 [DanmakuSurfaceView]（画线程、Surface、Choreographer 都在它里面）；
 * 3. 值变了就 [DanmakuSurfaceView.update] 推给画线程（它自己 post 到画线程，不在这里画）。
 *
 * **第一帧之前不建图层**（`hasFrame`）：那时播放器上垫着封面（`PlayerView` 的 `poster`），SurfaceView 会把窗口上
 * 那块"挖掉"，封面就看不见了。第一帧出来（有真实画面了）再建，正好也是拿到片源分辨率、能算准弹幕矩形的时候。
 */
@Composable
actual fun DanmakuLayer(
    timeline: DanmakuTimeline,
    state: PlayerState,
    settings: DanmakuSettings,
    modifier: Modifier,
    onSurface: Boolean,
) {
    // 设置页那块预览（DanmakuPreview）底下是 Compose 画的渐变底，不是视频：这层 SurfaceView 在窗口**下面**，
    // 用在这儿会把渐变挖掉、透出桌面（见 DanmakuLayer 的 onSurface 注释），所以还是走 Compose 那一份。
    if (!onSurface || DanmakuRenderDebug.forceCompose) {
        ComposeDanmakuLayer(timeline, state, settings, modifier)
        return
    }
    val density = LocalDensity.current
    val visible = LocalWindowVisible.current
    // 下面这几个读的都是"播放状态变了才变"的状态；播放中一直是同一批值，不会因为弹幕在动而重组。
    val playing = state.playing
    val buffering = state.buffering
    val speed = state.speed
    val hasFrame = state.hasFrame
    val running = playing && !buffering && visible
    // 暂停 / 缓冲时才读位置（跳转要重画一帧）；播放中不读，免得每 250ms 的位置报告把这一层重组一次。
    val pausedAtMs = if (running) 0L else state.positionMs
    if (!hasFrame) return
    AndroidView(
        modifier = modifier,
        factory = { ctx -> DanmakuSurfaceView(ctx, timeline, state, density.density) },
        onRelease = { view -> view.release() },
        update = { view ->
            view.update(
                running = running,
                speed = speed,
                settings = settings,
                density = density.density,
                pausedAtMs = pausedAtMs,
            )
        },
    )
}

/**
 * 调试开关（**只有 debug 的 `DebugStartActivity` 会写它**，release 里没人调、恒为 false）：
 * `true` = 安卓也用改之前那份 Compose 实现，用来在模拟器上同一份代码里量 `dumpsys gfxinfo` 的"改前 / 改后"
 * 。不是给用户的功能开关。
 */
object DanmakuRenderDebug {
    @Volatile
    var forceCompose: Boolean = false
}
