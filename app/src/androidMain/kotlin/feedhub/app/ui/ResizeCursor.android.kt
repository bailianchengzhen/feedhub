package feedhub.app.ui

import androidx.compose.ui.input.pointer.PointerIcon

/**
 * 触屏上没有鼠标指针：双栏中间那条分隔线也是手指拖的（`App.kt` 的 [SplitPane]），所以用默认指针。
 * 桌面版用 AWT 的左右调整大小的 ↔（desktopMain 的同名文件）。
 */
actual val HorizontalResizePointer: PointerIcon = PointerIcon.Default
