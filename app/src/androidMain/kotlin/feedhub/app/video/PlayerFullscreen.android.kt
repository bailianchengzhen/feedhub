package feedhub.app.video

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

/**
 * 安卓的全屏效果（E1）：隐藏系统栏（边缘滑动临时唤出）+ 横屏视频转横屏。
 *
 * 为什么放在 commonMain 的 `PlayerView` 这一侧而不是 `MainActivity`：放大是详情页里的一个状态
 * （`DetailHost.playerZoomed`），进 / 出全屏只是它的副作用；这样桌面（空实现）和安卓共用一份调用点。
 *
 * 注意：
 * - **只改这个窗口**的 insets 控制，不碰系统的旋转锁定设置（`SENSOR_LANDSCAPE` 只是这一个 Activity 的方向偏好）；
 * - 方向由 [AppOrientation] 统一算：这里只报"横屏视频在全屏"这一半，退出全屏 / 离开播放器（组合销毁）就撤回，
 *   回到基准方向（手机默认竖屏，见设置「自动旋转」），不留"整个 App 被锁在横屏"的后遗症；
 * - 两个 Activity 的清单都带 `configChanges=orientation|screenSize|screenLayout|smallestScreenSize`，
 *   所以转屏不会重建界面，播放不中断。
 */
@Composable
actual fun PlayerFullscreenEffect(active: Boolean, landscape: Boolean) {
    val context = LocalContext.current
    val view = LocalView.current
    val activity = remember(context) { context.findActivity() } ?: return
    val window = activity.window

    // 系统栏：全屏时藏起来，退出时显示回来（离开播放器也显示回来，免得下次进详情还是藏着的）
    DisposableEffect(active, view) {
        val controller = window?.let { WindowCompat.getInsetsController(it, view) }
        if (active) {
            controller?.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            controller?.hide(WindowInsetsCompat.Type.systemBars())
        } else {
            controller?.show(WindowInsetsCompat.Type.systemBars())
        }
        onDispose {
            if (active) controller?.show(WindowInsetsCompat.Type.systemBars())
        }
    }

    // 方向：横屏视频进全屏 → 横屏；否则（竖屏视频 / 退出全屏）回到基准方向
    LaunchedEffect(active, landscape, activity) {
        AppOrientation.setLandscapeFullscreen(activity, active && landscape)
    }
    DisposableEffect(activity) {
        onDispose { AppOrientation.setLandscapeFullscreen(activity, false) }
    }
}

/** 从 Context 往上剥到 Activity（application context 拿不到就什么都不做）。 */
private fun Context.findActivity(): Activity? {
    var c: Context? = this
    while (c is ContextWrapper) {
        if (c is Activity) return c
        c = c.baseContext
    }
    return c as? Activity
}
