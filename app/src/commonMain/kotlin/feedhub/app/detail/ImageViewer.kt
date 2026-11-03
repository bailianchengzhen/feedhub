package feedhub.app.detail

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.math.abs

/** 大图查看层的状态：这一组图和当前是第几张。 */
@Stable
class ImageViewerState(val images: List<ViewerImage>, index: Int) {
    var index: Int by mutableIntStateOf(index)

    fun next() {
        if (index < images.lastIndex) index++
    }

    fun prev() {
        if (index > 0) index--
    }
}

private const val MaxScale = 8f

/** 大图查看层的背景：纯黑。 */
private val ViewerBackground = Color(0xFF000000)

/** 底部菜单的底色（查看层永远是黑底，菜单也跟着用深色，不跟 App 的深色 / 浅色设置走）。 */
private val SheetBackground = Color(0xFF1E2126)

private val WholeWindow = object : PopupPositionProvider {
    override fun calculatePosition(anchorBounds: IntRect, windowSize: IntSize, layoutDirection: LayoutDirection, popupContentSize: IntSize) =
        IntOffset.Zero
}

/**
 * 覆盖整个窗口的大图查看层（用 Popup，双栏时也盖住左边的信息流）。
 *
 * 桌面（保持原样）：滚轮缩放（以鼠标位置为中心）、放大后拖动平移、双击在 1 倍和 2.5 倍之间切换、
 * 左右方向键 / 两侧按钮切换同一篇里的图；Esc、右上角的关闭按钮或在 1 倍时点一下空白处关闭。
 *
 * 触屏：双指缩放；**缩放到 1× 时**左右滑换图（跟手，松手按过没过 1/5 屏决定翻不翻），
 * 往下拖关闭；长按弹出底部菜单「保存到相册 / 分享图片 / 复制图片链接」（保存 / 分享只有安卓有，桌面只有复制链接）。
 * 背景纯黑，左上角「3 / 12」。
 */
@Composable
fun ImageViewer(state: ImageViewerState, onClose: () -> Unit) {
    // 性能记录：页面名临时盖成"图片查看器"（关掉就恢复成下面那一页，见 PerfScreen）
    DisposableEffect(Unit) {
        feedhub.app.perf.PerfHooks.overlay("图片查看器")
        onDispose { feedhub.app.perf.PerfHooks.overlay(null) }
    }
    Popup(popupPositionProvider = WholeWindow, onDismissRequest = onClose, properties = PopupProperties(focusable = true)) {
        ViewerContent(state, onClose)
    }
}

@Composable
private fun ViewerContent(state: ImageViewerState, onClose: () -> Unit) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val actions = rememberImageViewerActions()
    val clipboard = LocalClipboardManager.current
    val scope = rememberCoroutineScope()
    var scale by remember(state.index) { mutableFloatStateOf(1f) }
    var offset by remember(state.index) { mutableStateOf(Offset.Zero) }
    // 触屏跟手的位移（1× 时左右滑 / 下拉）
    var drag by remember(state.index) { mutableStateOf(Offset.Zero) }
    var box by remember { mutableStateOf(IntSize.Zero) }
    var menu by remember { mutableStateOf(false) }
    // 截图工具用：把安卓那一套菜单项都画出来（桌面真机上长按不会弹菜单，保持原样）
    var menuPreview by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var lastPressWasTouch by remember { mutableStateOf(false) }
    val image = state.images[state.index]

    // 截图工具（Screenshot 的 open=image-menu）：直接把底部菜单摆出来
    LaunchedEffect(state) {
        if (DetailDebug.pendingImageMenu) {
            DetailDebug.pendingImageMenu = false
            menuPreview = DetailDebug.pendingImageMenuPreview
            DetailDebug.pendingImageMenuPreview = false
            menu = true
        }
    }

    LaunchedEffect(message) {
        if (message != null) {
            delay(2400)
            message = null
        }
    }

    fun clamp(s: Float, o: Offset): Offset {
        if (s <= 1.01f) return Offset.Zero
        val maxX = box.width * (s - 1f) / 2f
        val maxY = box.height * (s - 1f) / 2f
        return Offset(o.x.coerceIn(-maxX, maxX), o.y.coerceIn(-maxY, maxY))
    }

    /** 以窗口里 [anchor] 这一点为中心缩放：缩放前它下面的图像点，缩放后还在原处。 */
    fun zoomAt(factor: Float, anchor: Offset) {
        val next = (scale * factor).coerceIn(1f, MaxScale)
        val center = Offset(box.width / 2f, box.height / 2f)
        val ratio = next / scale
        offset = clamp(next, (anchor - center) - (anchor - center - offset) * ratio)
        scale = next
    }

    fun toast(text: String) {
        message = text
    }

    /**
     * 触屏手势松手后把跟手位移收回去 / 甩出去，再做 [after]（翻页、关闭）。
     * 手势本身跑在 `AwaitPointerEventScope`（受限的挂起作用域）里，动画不能在里头跑，所以扔到 [scope] 上。
     */
    fun animateDragTo(target: Offset, after: () -> Unit = {}) {
        scope.launch {
            val from = drag
            if (from != target) {
                animate(0f, 1f, animationSpec = tween(150, easing = LinearEasing)) { t, _ ->
                    drag = from + (target - from) * t
                }
            }
            after()
        }
    }

    Box(
        Modifier.fillMaxSize().background(ViewerBackground).onSizeChanged { box = it }
            .focusRequester(focus).focusable()
            .onKeyEvent { e ->
                if (e.type != KeyEventType.KeyDown) return@onKeyEvent false
                if (menu) {
                    // 底部菜单开着时，Esc / 返回先关菜单
                    if (e.key == Key.Escape) { menu = false; menuPreview = false; true } else false
                } else when (e.key) {
                    Key.Escape -> { onClose(); true }
                    Key.DirectionLeft, Key.PageUp -> { state.prev(); true }
                    Key.DirectionRight, Key.PageDown -> { state.next(); true }
                    Key.Plus, Key.Equals, Key.NumPadAdd -> { zoomAt(1.25f, Offset(box.width / 2f, box.height / 2f)); true }
                    Key.Minus, Key.NumPadSubtract -> { zoomAt(1 / 1.25f, Offset(box.width / 2f, box.height / 2f)); true }
                    else -> false
                }
            }
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent()
                        if (event.type == PointerEventType.Scroll) {
                            val change = event.changes.first()
                            val dy = change.scrollDelta.y
                            if (dy != 0f) {
                                zoomAt(if (dy < 0) 1.2f else 1 / 1.2f, change.position)
                                change.consume()
                            }
                        }
                    }
                }
            }
            // 缩放 / 平移 / 触屏翻页 / 下拉关闭：一个手势循环里按"几根手指 + 现在几倍"分工
            .pointerInput(state.index) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    lastPressWasTouch = down.type != PointerType.Mouse
                    val touch = lastPressWasTouch
                    var lastPinch = 0f
                    var pressed: List<PointerInputChange>
                    do {
                        val event = awaitPointerEvent()
                        pressed = event.changes.filter { it.pressed }
                        when {
                            pressed.size >= 2 -> {
                                // 双指缩放（从 1× 也能捏开）
                                val d = (pressed[1].position - pressed[0].position).getDistance()
                                if (lastPinch > 0.5f && d > 0.5f) {
                                    zoomAt(d / lastPinch, (pressed[0].position + pressed[1].position) / 2f)
                                }
                                lastPinch = d
                                pressed.forEach { it.consume() }
                            }
                            pressed.size == 1 -> {
                                val change = pressed.first()
                                val delta = change.positionChange()
                                if (scale > 1.01f) {
                                    // 放大之后：拖动平移
                                    if (delta != Offset.Zero) {
                                        offset = clamp(scale, offset + delta)
                                        change.consume()
                                    }
                                } else if (touch && delta != Offset.Zero) {
                                    // 1× 且在触屏上：左右滑 = 换图，往下拖 = 关闭（跟手）
                                    drag += delta
                                    change.consume()
                                }
                                // 鼠标在 1× 时拖动：和以前一样什么都不做（桌面行为不变）
                            }
                        }
                    } while (pressed.isNotEmpty())

                    // 松手：按拖了多远决定翻页 / 关闭 / 弹回去
                    if (touch && scale <= 1.01f && drag != Offset.Zero) {
                        val w = box.width.toFloat()
                        val h = box.height.toFloat()
                        val dx = drag.x
                        val dy = drag.y
                        when {
                            dy > h * 0.15f && dy > abs(dx) -> animateDragTo(Offset(0f, h)) { onClose() }
                            dx < -w * 0.2f && state.index < state.images.lastIndex ->
                                animateDragTo(Offset(-w, 0f)) { state.next(); drag = Offset.Zero }
                            dx > w * 0.2f && state.index > 0 ->
                                animateDragTo(Offset(w, 0f)) { state.prev(); drag = Offset.Zero }
                            else -> animateDragTo(Offset.Zero)
                        }
                    }
                }
            }
            .pointerInput(Unit) {
                detectTapGestures(
                    onDoubleTap = { p -> if (scale > 1.01f) { scale = 1f; offset = Offset.Zero } else zoomAt(2.5f, p) },
                    onTap = { if (scale <= 1.01f && !menu) onClose() },
                    onLongPress = {
                        // 长按 = 底部菜单。只在手指上（鼠标长按不改桌面行为）。
                        if (lastPressWasTouch) menu = true
                    },
                )
            },
    ) {
        CircularProgressIndicator(Modifier.align(Alignment.Center).size(28.dp), color = Color.White.copy(alpha = 0.7f), strokeWidth = 2.dp)
        val layer = Modifier.fillMaxSize().graphicsLayer(
            scaleX = scale,
            scaleY = scale,
            translationX = offset.x + drag.x,
            translationY = offset.y + drag.y,
        )
        // 原图出来之前先垫着正文里已经加载好的那张
        if (image.thumb != null && image.thumb != image.url) {
            AsyncImage(model = image.thumb, contentDescription = null, contentScale = ContentScale.Fit, modifier = layer)
        }
        AsyncImage(model = image.url, contentDescription = image.caption, contentScale = ContentScale.Fit, modifier = layer)

        if (state.images.size > 1) {
            Text(
                "${state.index + 1} / ${state.images.size}",
                color = Color.White,
                modifier = Modifier.align(Alignment.TopStart).padding(16.dp)
                    .background(Color.Black.copy(alpha = 0.45f), CircleShape).padding(horizontal = 12.dp, vertical = 4.dp),
            )
            if (state.index > 0) ViewerArrow(Modifier.align(Alignment.CenterStart).padding(8.dp), left = true, onClick = state::prev)
            if (state.index < state.images.lastIndex) ViewerArrow(Modifier.align(Alignment.CenterEnd).padding(8.dp), left = false, onClick = state::next)
        }
        IconButton(
            onClick = onClose,
            modifier = Modifier.align(Alignment.TopEnd).padding(12.dp).background(Color.Black.copy(alpha = 0.45f), CircleShape),
        ) { Icon(Icons.Outlined.Close, contentDescription = "关闭", tint = Color.White) }

        message?.let {
            Text(
                it,
                color = Color.White,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = if (menu) 220.dp else 40.dp)
                    .background(Color.Black.copy(alpha = 0.7f), CircleShape).padding(horizontal = 16.dp, vertical = 8.dp),
            )
        } ?: run {
            image.caption?.takeIf { it.isNotBlank() }?.let {
                Text(
                    it,
                    color = Color.White,
                    modifier = Modifier.align(Alignment.BottomCenter).padding(24.dp)
                        .background(Color.Black.copy(alpha = 0.45f), CircleShape).padding(horizontal = 14.dp, vertical = 6.dp),
                )
            }
        }

        if (menu) {
            ViewerMenu(
                canSave = actions.canSaveToAlbum || menuPreview,
                canShare = actions.canShare || menuPreview,
                busy = busy,
                onDismiss = { menu = false; menuPreview = false },
                onCopyLink = {
                    clipboard.setText(AnnotatedString(image.url))
                    menu = false; menuPreview = false
                    toast("已复制图片链接")
                },
                onSave = {
                    menu = false; menuPreview = false
                    if (!busy) {
                        busy = true
                        toast("正在保存…")
                        val name = imageFileName(image.caption, state.index, fileStamp(), "jpg")
                        scope.launch {
                            val text = runCatching { actions.saveToAlbum(image.url, name) }
                                .getOrElse { "保存失败：${it.message ?: it::class.simpleName}" }
                            busy = false
                            toast(text)
                        }
                    }
                },
                onShare = {
                    menu = false; menuPreview = false
                    if (!busy) {
                        busy = true
                        scope.launch {
                            val text = runCatching { actions.share(image.url, image.caption) }
                                .getOrElse { "分享失败：${it.message ?: it::class.simpleName}" }
                            busy = false
                            if (text.isNotEmpty()) toast(text)
                        }
                    }
                },
            )
        }
    }
}

/**
 * 长按弹出的底部菜单（不是 `ModalBottomSheet`：查看器本身在一个 Popup 里，再套一层 Dialog 在安卓上返回键 /
 * 键盘 insets 都不好处理；这里就是一个画在查看层里的面板，点遮罩关掉）。
 */
@Composable
private fun ViewerMenu(
    canSave: Boolean,
    canShare: Boolean,
    busy: Boolean,
    onDismiss: () -> Unit,
    onCopyLink: () -> Unit,
    onSave: () -> Unit,
    onShare: () -> Unit,
) {
    Box(Modifier.fillMaxSize()) {
        // 遮罩：点一下关掉菜单（不关查看器）
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.45f)).clickable(onClick = onDismiss))
        Column(
            Modifier.align(Alignment.BottomCenter).fillMaxWidth()
                .background(SheetBackground, RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp))
                .padding(vertical = 8.dp),
        ) {
            Box(Modifier.align(Alignment.CenterHorizontally).padding(top = 4.dp, bottom = 8.dp).size(width = 36.dp, height = 4.dp)
                .background(Color.White.copy(alpha = 0.3f), CircleShape))
            if (canSave) MenuRow(Icons.Outlined.Download, if (busy) "正在保存…" else "保存到相册", enabled = !busy, onClick = onSave)
            if (canShare) MenuRow(Icons.Outlined.Share, "分享图片", enabled = !busy, onClick = onShare)
            MenuRow(Icons.Outlined.Link, "复制图片链接", enabled = !busy, onClick = onCopyLink)
            MenuRow(Icons.Outlined.Close, "取消", enabled = true, onClick = onDismiss)
            Box(Modifier.height(6.dp))
        }
    }
}

@Composable
private fun MenuRow(icon: ImageVector, label: String, enabled: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onClick).padding(horizontal = 22.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Color.White.copy(alpha = if (enabled) 0.9f else 0.4f))
        Text(label, color = Color.White.copy(alpha = if (enabled) 1f else 0.4f))
    }
}

@Composable
private fun ViewerArrow(modifier: Modifier, left: Boolean, onClick: () -> Unit) {
    IconButton(onClick = onClick, modifier = modifier.background(Color.Black.copy(alpha = 0.45f), CircleShape)) {
        Icon(
            if (left) Icons.AutoMirrored.Outlined.KeyboardArrowLeft else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
            contentDescription = if (left) "上一张" else "下一张",
            tint = Color.White,
        )
    }
}
