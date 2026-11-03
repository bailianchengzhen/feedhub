package feedhub.app.captcha

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.awt.awtEventOrNull
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.nativeKeyCode
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.key.utf16CodePoint
import androidx.compose.ui.input.pointer.PointerButton
import androidx.compose.ui.input.pointer.PointerEvent
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.isAltPressed
import androidx.compose.ui.input.pointer.isCtrlPressed
import androidx.compose.ui.input.pointer.isMetaPressed
import androidx.compose.ui.input.pointer.isPrimaryPressed
import androidx.compose.ui.input.pointer.isShiftPressed
import androidx.compose.ui.input.pointer.isSecondaryPressed
import androidx.compose.ui.input.pointer.isTertiaryPressed
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import java.awt.event.InputEvent
import java.awt.event.KeyEvent
import java.awt.event.MouseEvent
import java.awt.event.MouseWheelEvent
import kotlin.math.abs
import kotlin.math.roundToInt
import kotlin.math.sign

/**
 * 桌面端的验证码浏览器：**CEF 跑在一个用完就退的子进程里**（[CaptchaHelperSession]），
 * 这里只是一块 Compose 画布——子进程离屏渲染的帧传回来画在这里，鼠标 / 滚轮 / 键盘转发过去，光标形状照子进程说的换。
 * 外面的覆盖层（[CaptchaOverlay]，双栏只盖详情区、全屏盖整屏）没变；App 进程里不再加载 CEF。
 *
 * 坐标：画布是设备像素，CEF 用 CSS 像素（= px / density），设备像素比 = density，所以帧和画布 1:1、
 * 鼠标坐标 = 指针位置 / density，滑块拖到哪就是哪。
 *
 * 离开组合（验证成功 / 取消 / 超时，覆盖层消失）时 [CaptchaHelperSession.close]：子进程体面地关 CEF，超时按 PID 杀。
 * 子进程起不来 / 中途没了 → [onError]，覆盖层显示「用系统浏览器打开」的退路（文案不变）。
 */
@OptIn(ExperimentalComposeUiApi::class)
@Composable
actual fun CaptchaBrowser(
    url: String,
    modifier: Modifier,
    onError: (String) -> Unit,
    // 桌面不需要：页面在本机 HTTP 服务上，浏览器正常请求
    host: CaptchaPageHost?,
) {
    val session = remember(url) { CaptchaHelperSession(url) }
    DisposableEffect(session) {
        onDispose { session.close() }
    }
    val error = session.error
    LaunchedEffect(error) { error?.let(onError) }
    val density = LocalDensity.current.density
    val focus = remember(session) { FocusRequester() }
    val pointer = remember(session) { PointerForwarder(session) }

    Box(
        modifier
            .background(Color(0xFFF5F5F5))
            .pointerHoverIcon(PointerIcon(java.awt.Cursor.getPredefinedCursor(session.cursor.coerceIn(0, 13))))
            .focusRequester(focus)
            .onFocusChanged { session.focus(it.isFocused) }
            .onKeyEvent { forwardKey(session, it) }
            .focusable()
            .pointerInput(session) {
                awaitPointerEventScope {
                    while (true) {
                        val ev = awaitPointerEvent()
                        if (ev.type == PointerEventType.Press) focus.requestFocus()
                        pointer.forward(ev, density)
                        ev.changes.forEach { it.consume() }
                    }
                }
            },
    ) {
        Canvas(
            Modifier.fillMaxSize().onSizeChanged { session.resize(it.width, it.height, density) },
        ) {
            @Suppress("UNUSED_EXPRESSION") session.tick // 读一下：新帧来了才重画
            session.withImage { img ->
                drawIntoCanvas { c -> c.nativeCanvas.drawImage(img, 0f, 0f) }
            }
        }
        if (!session.hasFrame) {
            Text(
                error?.let { "内置浏览器起不来：$it" } ?: session.status,
                textAlign = TextAlign.Center,
                color = Color(0xFF444444),
                modifier = Modifier.align(Alignment.Center).padding(16.dp),
            )
        }
    }
}

/** Compose 的指针事件 → AWT 风格的鼠标事件（JCEF 收的就是 AWT 事件），坐标换成 CSS 像素。 */
internal class PointerForwarder(private val session: CaptchaHelperSession) {
    private var lastPressAt = 0L
    private var lastPressX = Int.MIN_VALUE
    private var lastPressY = Int.MIN_VALUE
    private var lastButton = 0
    private var clicks = 0

    @OptIn(ExperimentalComposeUiApi::class)
    fun forward(ev: PointerEvent, density: Float) {
        val change = ev.changes.firstOrNull() ?: return
        val x = (change.position.x / density).roundToInt()
        val y = (change.position.y / density).roundToInt()
        val awt = ev.awtEventOrNull
        val mods = modifiers(ev)
        when (ev.type) {
            PointerEventType.Press -> {
                val button = awtButton(ev.button)
                val now = System.currentTimeMillis()
                clicks = awt?.clickCount?.takeIf { it > 0 }
                    ?: if (button == lastButton && now - lastPressAt < 500 && abs(x - lastPressX) <= 4 && abs(y - lastPressY) <= 4) clicks + 1 else 1
                lastPressAt = now; lastPressX = x; lastPressY = y; lastButton = button
                session.mouse(MouseEvent.MOUSE_PRESSED, x, y, mods or buttonMask(button), button, clicks)
            }
            PointerEventType.Release -> {
                val button = awtButton(ev.button)
                session.mouse(MouseEvent.MOUSE_RELEASED, x, y, mods, button, clicks.coerceAtLeast(1))
            }
            PointerEventType.Move -> {
                val dragging = mods and (InputEvent.BUTTON1_DOWN_MASK or InputEvent.BUTTON2_DOWN_MASK or InputEvent.BUTTON3_DOWN_MASK) != 0
                session.mouse(if (dragging) MouseEvent.MOUSE_DRAGGED else MouseEvent.MOUSE_MOVED, x, y, mods, 0, 0)
            }
            PointerEventType.Enter -> session.mouse(MouseEvent.MOUSE_ENTERED, x, y, mods, 0, 0)
            PointerEventType.Exit -> session.mouse(MouseEvent.MOUSE_EXITED, x, y, mods, 0, 0)
            PointerEventType.Scroll -> {
                val wheel = awt as? MouseWheelEvent
                val delta = change.scrollDelta
                // 横向滚动在 AWT 里是 Shift + 滚轮
                val horizontal = wheel == null && abs(delta.x) > abs(delta.y)
                val precise = wheel?.preciseWheelRotation ?: (if (horizontal) delta.x else delta.y).toDouble()
                val rotation = wheel?.wheelRotation ?: precise.sign.toInt()
                session.wheel(x, y, if (horizontal) mods or InputEvent.SHIFT_DOWN_MASK else mods, rotation, precise)
            }
        }
    }

    private fun modifiers(ev: PointerEvent): Int {
        var m = 0
        val k = ev.keyboardModifiers
        if (k.isShiftPressed) m = m or InputEvent.SHIFT_DOWN_MASK
        if (k.isCtrlPressed) m = m or InputEvent.CTRL_DOWN_MASK
        if (k.isAltPressed) m = m or InputEvent.ALT_DOWN_MASK
        if (k.isMetaPressed) m = m or InputEvent.META_DOWN_MASK
        if (ev.buttons.isPrimaryPressed) m = m or InputEvent.BUTTON1_DOWN_MASK
        if (ev.buttons.isTertiaryPressed) m = m or InputEvent.BUTTON2_DOWN_MASK
        if (ev.buttons.isSecondaryPressed) m = m or InputEvent.BUTTON3_DOWN_MASK
        return m
    }

    private fun awtButton(b: PointerButton?): Int = when (b) {
        PointerButton.Secondary -> MouseEvent.BUTTON3
        PointerButton.Tertiary -> MouseEvent.BUTTON2
        else -> MouseEvent.BUTTON1
    }

    private fun buttonMask(button: Int): Int = when (button) {
        MouseEvent.BUTTON3 -> InputEvent.BUTTON3_DOWN_MASK
        MouseEvent.BUTTON2 -> InputEvent.BUTTON2_DOWN_MASK
        else -> InputEvent.BUTTON1_DOWN_MASK
    }
}

/**
 * 键盘照 AWT 原样转发（按下 / 抬起 / 字符），JCEF 自己就是收 AWT 的这三种事件。Compose 桌面版会把 AWT 的 KEY_TYPED 也交给
 * `onKeyEvent`，所以字符不用自己合成。没有原生事件的（测试 / 截图工具造的）按 Compose 的键码补。
 * 焦点在验证码上时**所有键都吃掉**（和原来 SwingPanel 一样，App 的快捷键不响应）。输入法组字没转发（验证码是滑块 / 点选）。
 */
private fun forwardKey(session: CaptchaHelperSession, e: androidx.compose.ui.input.key.KeyEvent): Boolean {
    val awt = e.awtEventOrNull
    if (awt != null) {
        session.key(awt.id, awt.keyCode, awt.keyChar, awt.modifiersEx, awt.keyLocation)
        return true
    }
    val code = e.key.nativeKeyCode
    when (e.type) {
        KeyEventType.KeyDown -> {
            session.key(KeyEvent.KEY_PRESSED, code, KeyEvent.CHAR_UNDEFINED, 0, KeyEvent.KEY_LOCATION_STANDARD)
            val cp = e.utf16CodePoint
            if (cp in 1..0xFFFF) session.key(KeyEvent.KEY_TYPED, KeyEvent.VK_UNDEFINED, cp.toChar(), 0, KeyEvent.KEY_LOCATION_UNKNOWN)
        }
        KeyEventType.KeyUp -> session.key(KeyEvent.KEY_RELEASED, code, KeyEvent.CHAR_UNDEFINED, 0, KeyEvent.KEY_LOCATION_STANDARD)
        else -> {}
    }
    return true
}
