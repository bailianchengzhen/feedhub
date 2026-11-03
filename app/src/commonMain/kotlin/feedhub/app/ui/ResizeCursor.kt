package feedhub.app.ui

import androidx.compose.ui.input.pointer.PointerIcon

/**
 * 拖动分隔线时鼠标悬停的指针（左右调整大小的 ↔）。Compose 公共部分只有默认 / 手 / 文本 / 十字这几种，
 * 所以按平台给：桌面用 AWT 的 E_RESIZE_CURSOR；触屏平台没有鼠标指针，用默认的。
 */
expect val HorizontalResizePointer: PointerIcon
