package feedhub.app.video

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties

/**
 * 倍速的**小**列表。
 *
 * 原来三处（详情播放器 / 卡片内联播放器 / 沉浸界面）都是 Material 的 `DropdownMenu`：一档 48dp，
 * 七八档在横屏全屏里几乎和屏幕一样高，看着就是正中间一大块。这里一档 32dp、窄条半透明底，
 * **贴在倍速按钮的正上方**（上面放不下才放到下面），当前档用强调色标出来。
 */
@Composable
internal fun SpeedPopup(expanded: Boolean, current: Float, onDismiss: () -> Unit, onPick: (Float) -> Unit) {
    if (!expanded) return
    val density = LocalDensity.current
    val gap = with(density) { 6.dp.roundToPx() }
    val edge = with(density) { 8.dp.roundToPx() }
    Popup(
        popupPositionProvider = AboveAnchor(gap, edge),
        onDismissRequest = onDismiss,
        properties = PopupProperties(focusable = true),
    ) {
        Column(
            Modifier.width(76.dp).clip(RoundedCornerShape(8.dp)).background(Color.Black.copy(alpha = 0.78f))
                .padding(vertical = 4.dp),
        ) {
            for (s in PlaybackSpeed.STEPS.reversed()) {
                val selected = s == current
                Box(
                    Modifier.fillMaxWidth().height(32.dp).clickable { onPick(s) },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        PlaybackSpeed.label(s),
                        color = if (selected) Accent else Color.White,
                        fontSize = 13.sp,
                        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                    )
                }
            }
        }
    }
}

private val Accent = Color(0xFFFB7299)

/** 水平对准按钮中线（不出窗口两边）；竖直放在按钮上方 [gap] 处，上面放不下就放到按钮下方。 */
private class AboveAnchor(private val gap: Int, private val edge: Int) : PopupPositionProvider {
    override fun calculatePosition(
        anchorBounds: IntRect,
        windowSize: IntSize,
        layoutDirection: LayoutDirection,
        popupContentSize: IntSize,
    ): IntOffset {
        val maxX = (windowSize.width - popupContentSize.width - edge).coerceAtLeast(edge)
        val x = (anchorBounds.center.x - popupContentSize.width / 2).coerceIn(edge, maxX)
        val above = anchorBounds.top - gap - popupContentSize.height
        val y = if (above >= edge) {
            above
        } else {
            val maxY = (windowSize.height - popupContentSize.height - edge).coerceAtLeast(edge)
            (anchorBounds.bottom + gap).coerceIn(edge, maxY)
        }
        return IntOffset(x, y)
    }
}
