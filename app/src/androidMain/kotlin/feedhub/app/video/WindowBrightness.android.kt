package feedhub.app.video

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.provider.Settings
import android.view.WindowManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext

/**
 * 安卓上"本窗口亮度"：写 `Window.attributes.screenBrightness`（0f~1f），**不改系统亮度设置**
 * （`Settings.System.SCREEN_BRIGHTNESS` 只读一下当起点），退出播放器时还原成 `BRIGHTNESS_OVERRIDE_NONE`。
 *
 * 用的是 Compose 的 `LocalContext`（在 `ComponentActivity.setContent` 里就是 Activity，外面包了几层就剥到 Activity），
 * 拿不到 Activity（不该发生）就退回 [NoWindowBrightness]，手势里的亮度那一项就不起作用。
 */
@Composable
actual fun rememberWindowBrightness(): WindowBrightness {
    val context = LocalContext.current
    val activity = remember(context) { context.findActivity() }
    val impl = remember(activity) { activity?.let(::AndroidWindowBrightness) }
    DisposableEffect(impl) { onDispose { impl?.restore() } }
    return impl ?: NoWindowBrightness
}

/** 触屏：安卓上就是（手机 / 平板）；手势只在这里开，桌面的鼠标行为保持不变。 */
actual val isTouchDevice: Boolean = true

private class AndroidWindowBrightness(private val activity: Activity) : WindowBrightness {
    /** 进来时的窗口亮度（-1 = 交回系统），退出时还原它。 */
    private val original: Float = activity.window?.attributes?.screenBrightness ?: -1f
    private var last: Float? = null

    override fun current(): Float? {
        last?.let { return it }
        val window = activity.window ?: return null
        val own = window.attributes.screenBrightness
        // -1（BRIGHTNESS_OVERRIDE_NONE）= 现在跟着系统走，起点取系统的那个值；0 太暗，夹到 0.05
        if (own >= 0f) return own.coerceIn(0.05f, 1f)
        val system = runCatching {
            Settings.System.getInt(activity.contentResolver, Settings.System.SCREEN_BRIGHTNESS)
        }.getOrNull() ?: return 0.5f
        return (system / 255f).coerceIn(0.05f, 1f)
    }

    override fun set(level: Float) {
        val v = level.coerceIn(0.05f, 1f)
        last = v
        val window = activity.window ?: return
        // 已经销毁 / 正在销毁就别再碰它
        if (activity.isFinishing || activity.isDestroyed) return
        val attrs: WindowManager.LayoutParams = window.attributes
        attrs.screenBrightness = v
        window.attributes = attrs
    }

    fun restore() {
        if (last == null) return
        if (activity.isFinishing || activity.isDestroyed) return
        val window = activity.window ?: return
        val attrs: WindowManager.LayoutParams = window.attributes
        attrs.screenBrightness = original
        window.attributes = attrs
        last = null
    }
}

/** 从 Context 往上剥到 Activity；不是 Activity 包着的（application context）就 null。 */
private fun Context.findActivity(): Activity? {
    var c: Context? = this
    while (c is ContextWrapper) {
        if (c is Activity) return c
        c = c.baseContext
    }
    return c as? Activity
}
