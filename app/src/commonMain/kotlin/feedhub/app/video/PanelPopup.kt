package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.window.PopupPositionProvider

/**
 * 播放器里的浮层（现在只有弹幕设置面板用）：位置由 [positionProvider] 给，点外面 / 返回键关掉，
 * 键盘事件先过 [onPreviewKey]（面板开着时 Esc 关掉它——只靠 `dismissOnBackPress`，面板里没有聚焦的控件时收不到）。
 *
 * 为什么要按平台给：桌面（Skiko）的 `androidx.compose.ui.window.Popup` 自带 `onPreviewKeyEvent` 参数，
 * 安卓的 `Popup` 没有（只有 positionProvider / onDismissRequest / properties / content），
 * 直接在 commonMain 里用那个参数安卓编不过。两边行为对齐，见各自的实际实现。
 */
@Composable
internal expect fun PanelPopup(
    positionProvider: PopupPositionProvider,
    onDismissRequest: () -> Unit,
    onPreviewKey: (KeyEvent) -> Boolean,
    content: @Composable () -> Unit,
)
