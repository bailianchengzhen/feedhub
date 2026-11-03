package feedhub.app.video

import androidx.compose.runtime.Composable

/**
 * 播放器手势调的"亮度"：**只改本窗口**，不动系统的亮度设置，离开播放器就还原。
 *
 * 安卓把 `Window.attributes.screenBrightness` 设成 0f~1f（`BRIGHTNESS_OVERRIDE_NONE` = -1 就是交回系统）；
 * 桌面不接手势（[isTouchDevice] 为 false），拿到的永远是 [NoWindowBrightness]。
 */
interface WindowBrightness {
    /** 这个窗口现在的亮度（0f~1f）；不知道 / 没接系统就是 null（手势从中间开始）。 */
    fun current(): Float?

    /** 把本窗口亮度设成 [level]（0f~1f）。 */
    fun set(level: Float)
}

/** 不接系统（桌面 / 没拿到 Activity）：读不到、设了也没效果。 */
object NoWindowBrightness : WindowBrightness {
    override fun current(): Float? = null
    override fun set(level: Float) {}
}

/**
 * 造一个本窗口亮度控制器：[level] 只在第一次拖动时读，之后由手势自己算；退出组合（离开播放器）时把窗口还原成系统亮度。
 */
@Composable
expect fun rememberWindowBrightness(): WindowBrightness

/**
 * 这台设备是触屏吗：播放器手势（单击显隐控件、双击暂停、长按 3 倍速、拖进度 / 亮度 / 音量）**只加在触屏上**，
 * 桌面保持原来的鼠标 / 键盘行为（点画面暂停、双击放大、Esc）。
 */
expect val isTouchDevice: Boolean
