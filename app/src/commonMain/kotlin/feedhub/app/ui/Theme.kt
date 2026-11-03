package feedhub.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import feedhub.app.LocalPlatforms
import feedhub.core.Platform

enum class DarkMode { SYSTEM, LIGHT, DARK }

private val Light = lightColorScheme(
    primary = Color(0xFF3B6EA8),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFD6E3F7),
    onPrimaryContainer = Color(0xFF0E2A4A),
    background = Color(0xFFF6F7F9),
    surface = Color(0xFFF6F7F9),
    surfaceContainerLowest = Color.White,
    surfaceContainerLow = Color(0xFFFFFFFF),
    surfaceContainer = Color(0xFFEFF1F4),
    surfaceContainerHigh = Color(0xFFE8EBEF),
    surfaceVariant = Color(0xFFE3E6EB),
    onSurfaceVariant = Color(0xFF5C626B),
    outlineVariant = Color(0xFFD9DDE3),
)

private val Dark = darkColorScheme(
    primary = Color(0xFF9CC2F0),
    onPrimary = Color(0xFF0E2A4A),
    primaryContainer = Color(0xFF26456B),
    onPrimaryContainer = Color(0xFFD6E3F7),
    background = Color(0xFF121417),
    surface = Color(0xFF121417),
    // 卡片用的 surfaceContainerLowest 必须**比背景亮**：深色下层级是
    // 背景 #121417 < 卡片 #171A1E < surfaceContainerLow #1B1E22 < surfaceContainer #202328。
    // 原来写的是 #0D0F11（比背景更暗），卡片看起来像挖下去的坑。
    surfaceContainerLowest = Color(0xFF171A1E),
    surfaceContainerLow = Color(0xFF1B1E22),
    surfaceContainer = Color(0xFF202328),
    surfaceContainerHigh = Color(0xFF2A2E33),
    surfaceVariant = Color(0xFF2E3238),
    onSurfaceVariant = Color(0xFFA9AFB8),
    outlineVariant = Color(0xFF353A41),
)

@Composable
fun FeedhubTheme(darkMode: DarkMode = DarkMode.SYSTEM, content: @Composable () -> Unit) {
    val dark = when (darkMode) {
        DarkMode.SYSTEM -> isSystemInDarkTheme()
        DarkMode.LIGHT -> false
        DarkMode.DARK -> true
    }
    // 系统栏图标的明暗跟着 App 的深色设置（不是系统的夜间模式），见 [SystemBarIcons]（H1）
    SystemBarIcons(dark)
    MaterialTheme(colorScheme = if (dark) Dark else Light, content = content)
}

/** 平台标签的颜色：各平台的品牌色（插件描述里的 `color`）；没装的插件（老收藏 / 历史里留下的）是灰的。 */
@Composable
fun Platform.brandColor(): Color = LocalPlatforms.current[this]?.let { Color(it.color) } ?: UNKNOWN_PLATFORM_COLOR

/** 没装的插件的平台色：灰。 */
val UNKNOWN_PLATFORM_COLOR = Color(0xFF9E9E9E)
