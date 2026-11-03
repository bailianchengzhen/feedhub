package feedhub.app.display

import android.app.Activity
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.Choreographer

/**
 * 安卓上把刷新率请求落到系统。
 *
 * 做法：`WindowManager.LayoutParams.preferredDisplayModeId` 选**当前分辨率下刷新率最高**的显示模式
 * （`Display.supportedModes` 里挑），撤回时置 0（= 没有偏好、交给系统）。minSdk 26，
 * 这两个 API 都是 23 起的，所以不用按 API 级别分支。
 *
 * 说明：
 * - 只挑**和当前模式同一分辨率**的 mode：换分辨率会改画面大小，不是"锁刷新率"该做的事；
 *   万一同一分辨率里没有别的模式，才退回全体里刷新率最高的那个。
 * - 上限是面板支持的最高刷新率（`supportedModes` 里最大的那个）。国产 ROM 还会按
 *   `refresh_rate_config.xml` 的名单再夹一次，这不是 App 能改的——系统设置里把刷新率选成"高"才拿得到 120
 * - 原来的 `View.setRequestedFrameRate` / `Surface.setFrameRate` / `preferredRefreshRate` 那几条通道，
 *   以及"滑动要高刷""视频按片源帧率""静止撤回"的状态，
 * - 所有调用都 post 到主线程；Activity 已经在销毁就什么都不做。
 */
class WindowFrameRateSink(initialActivity: Activity? = null) : FrameRateSink {
    private val main = Handler(Looper.getMainLooper())

    @Volatile
    private var activity: Activity? = initialActivity

    /** 最近一次"要 / 不要最高刷新率"（只在主线程读写）。用来判掉"排着的帧回调已经过期"的情况。 */
    private var wanted = false

    /** 绑定当前前台 Activity（`AppGraph` 提升到 `Application` 层后按 Activity 生命周期挂接，不泄漏旧实例，A4）。 */
    fun attach(activity: Activity) {
        onMain {
            this.activity = activity
            if (wanted) {
                Choreographer.getInstance().postFrameCallback {
                    if (wanted && this.activity === activity) applyOnMain(activity, true)
                }
            }
        }
    }

    /** 解绑正在销毁的 Activity。 */
    fun detach(activity: Activity) {
        onMain {
            if (this.activity === activity) {
                applyOnMain(activity, false)
                this.activity = null
            }
        }
    }

    override fun apply(max: Boolean) {
        main.post {
            wanted = max
            val act = activity ?: return@post
            if (!max) {
                // 退到后台：**立刻**撤回（后台没有画面要看，晚撤就等于白占着高刷）
                applyOnMain(act, false)
                return@post
            }
            // 回前台：等"下一帧"之后再改窗口属性。
            //
            // 改 `preferredDisplayModeId` 会让 WindowManager 立刻走一次 relayout（真机 Perfetto 里回前台
            // 那一帧里有 `relayoutWindow#...params=true`，8~10 ms）。以前在 onStart 里同步设，这个 relayout
            // 就落在"回前台的第一帧"里；排到下一帧再设，第一帧只管把画面画出来。
            //
            // 只是改成下一帧生效。
            Choreographer.getInstance().postFrameCallback {
                val cur = activity
                if (wanted && cur != null) applyOnMain(cur, true)
            }
        }
    }

    private fun onMain(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) block() else main.post(block)
    }

    private fun applyOnMain(act: Activity, max: Boolean) {
        if (act.isFinishing || act.isDestroyed) return
        val window = act.window ?: return
        val modeId = if (max) highestRefreshModeId(act) else 0
        val attrs = window.attributes
        if (attrs.preferredDisplayModeId == modeId) return
        attrs.preferredDisplayModeId = modeId
        window.attributes = attrs
    }

    /** 当前分辨率下刷新率最高的显示模式；查不到就 0（不硬报一个不存在的 mode）。 */
    @Suppress("DEPRECATION")
    private fun highestRefreshModeId(act: Activity): Int {
        val display = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            act.display
        } else {
            act.windowManager?.defaultDisplay
        } ?: return 0
        val current = display.mode ?: return 0
        val modes = display.supportedModes ?: return 0
        val sameSize = modes.filter {
            it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight
        }
        val pool = sameSize.ifEmpty { modes.toList() }
        return pool.maxByOrNull { it.refreshRate }?.modeId?.takeIf { it > 0 } ?: 0
    }
}
