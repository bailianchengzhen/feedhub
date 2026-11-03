package feedhub.app.ui

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

/**
 * 安卓：把状态栏 / 导航栏图标改成跟 App 的深色设置一致（H1）。
 * `MainActivity` 只调了一次 `enableEdgeToEdge()`（按**系统**夜间模式），这里每次主题变化再改一次，跟随设置。
 */
@Composable
actual fun SystemBarIcons(dark: Boolean) {
    val view = LocalView.current
    val window = remember(view) { view.context.findActivity()?.window }
    SideEffect {
        if (window != null) {
            WindowCompat.getInsetsController(window, view).apply {
                isAppearanceLightStatusBars = !dark
                isAppearanceLightNavigationBars = !dark
            }
        }
    }
}

/** 从 Compose 的 View 的 Context 往上找 Activity（外面可能套了 ContextThemeWrapper）。 */
private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
