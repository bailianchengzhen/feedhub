package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties

/** 桌面：`Popup` 自己就有 `onPreviewKeyEvent`，直接转发（和加这个 expect 之前的行为完全一样）。 */
@Composable
internal actual fun PanelPopup(
    positionProvider: PopupPositionProvider,
    onDismissRequest: () -> Unit,
    onPreviewKey: (KeyEvent) -> Boolean,
    content: @Composable () -> Unit,
) {
    Popup(
        popupPositionProvider = positionProvider,
        onDismissRequest = onDismissRequest,
        properties = PopupProperties(focusable = true),
        onPreviewKeyEvent = onPreviewKey,
    ) { content() }
}
