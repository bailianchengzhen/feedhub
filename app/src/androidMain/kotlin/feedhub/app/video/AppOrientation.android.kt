package feedhub.app.video

import android.app.Activity
import android.content.pm.ActivityInfo

/**
 * 整个 App 的屏幕方向只在这一处算。
 *
 * 两样东西叠起来：
 * - **基准方向**：手机（最短边 < 600dp）且设置里「自动旋转」关着（默认）→ 锁竖屏；
 *   其余（手机开了自动旋转、平板）→ `UNSPECIFIED`，跟系统的自动旋转走；
 * - **横屏视频全屏**：盖在基准方向上，`SENSOR_LANDSCAPE`，退出全屏就回到基准方向。
 *
 * 原来全屏那边是"进来时记下 `requestedOrientation`、退出时还原"，基准方向改了（比如开着播放器去改设置）
 * 会还原成旧值；现在两边都只报自己那一半，最终值每次重算。只在主线程调。
 */
internal object AppOrientation {
    private var base = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    private var landscapeFullscreen = false

    fun setBase(activity: Activity, autoRotate: Boolean, smallestWidthDp: Int) {
        base = baseOrientation(autoRotate, smallestWidthDp)
        apply(activity)
    }

    fun setLandscapeFullscreen(activity: Activity, on: Boolean) {
        landscapeFullscreen = on
        apply(activity)
    }

    private fun apply(activity: Activity) {
        if (activity.isFinishing || activity.isDestroyed) return
        val want = if (landscapeFullscreen) ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE else base
        if (activity.requestedOrientation != want) activity.requestedOrientation = want
    }
}

/** 基准方向（纯函数）：手机且没开自动旋转 → 竖屏；别的都跟系统。 */
internal fun baseOrientation(autoRotate: Boolean, smallestWidthDp: Int): Int =
    if (!autoRotate && smallestWidthDp in 1 until PHONE_MAX_SMALLEST_WIDTH_DP) {
        ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
    } else {
        ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    }

/** 最短边小于它算手机（和界面那条 600dp 的宽窄分界一致，见 `WideLayoutMinWidth`）。 */
private const val PHONE_MAX_SMALLEST_WIDTH_DP = 600
