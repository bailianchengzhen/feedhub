package feedhub.app.ui

import androidx.compose.runtime.Composable

/**
 * 系统的"返回"：安卓上就是返回键 / 返回手势（`androidx.activity.compose.BackHandler`），
 * 桌面没有这个东西（[enabled] 忽略，什么都不做）——桌面的返回是 `Main.kt` 里点详情顶栏的返回箭头或 Esc。
 *
 * 为什么要按平台给：`BackHandler` 是 androidx.activity 的 API，桌面上根本没有。详情页用它把返回键接到
 * "关大图 → 关楼中楼 → 返回上一层 → 关详情"，否则在安卓上按返回会直接退出 App。
 */
@Composable
expect fun PlatformBackHandler(enabled: Boolean = true, onBack: () -> Unit)
