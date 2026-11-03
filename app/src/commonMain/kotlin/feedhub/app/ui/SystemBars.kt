package feedhub.app.ui

import androidx.compose.runtime.Composable

/**
 * 系统栏（状态栏 / 导航栏）里的图标明暗。
 *
 * 问题：`enableEdgeToEdge()` 只看**系统**的夜间模式决定状态栏图标是黑还是白，而 App 自己的深色模式是设置里选的
 * （跟随系统 / 浅色 / 深色）。系统浅色 + App 深色时，状态栏图标还是黑的，压在深色背景上看不见。
 *
 * 所以在 [FeedhubTheme] 里按 App 当前算出的是不是深色，把两边的图标改过来：深色 → 浅色图标，浅色 → 深色图标。
 * 安卓走 `WindowInsetsControllerCompat`；桌面没有系统栏，空实现（不影响任何行为）。
 */
@Composable
expect fun SystemBarIcons(dark: Boolean)
