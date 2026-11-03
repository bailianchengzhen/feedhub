package feedhub.app.video

import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Box
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties

/**
 * 安卓：`Popup` 没有 `onPreviewKeyEvent` 参数，所以面板外面套一层自己能聚焦的 `Box`，
 * 键盘事件（外接键盘的 Esc）先在它这里过一遍；触屏上点面板外面 / 返回键仍然由 `PopupProperties` 关掉。
 */
@Composable
internal actual fun PanelPopup(
    positionProvider: PopupPositionProvider,
    onDismissRequest: () -> Unit,
    onPreviewKey: (KeyEvent) -> Boolean,
    content: @Composable () -> Unit,
) {
    val focus = remember { FocusRequester() }
    Popup(
        popupPositionProvider = positionProvider,
        onDismissRequest = onDismissRequest,
        properties = PopupProperties(focusable = true),
    ) {
        Box(Modifier.focusRequester(focus).focusable().onPreviewKeyEvent(onPreviewKey)) { content() }
        LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    }
}
