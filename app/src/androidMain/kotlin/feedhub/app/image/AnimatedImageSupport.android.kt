package feedhub.app.image

import coil3.ComponentRegistry

/**
 * 安卓上的动图解码：**这一阶段先什么都不装**（占位实现）。
 *
 * 桌面是自己写的 Skia 按帧解码器（`coil-gif` 只有安卓版，桌面上 Coil 用 Skia 只出第一帧）。安卓这边
 * 第二阶段直接接 `coil-gif` 的 `AnimatedImageDecoder` / `GifDecoder`，
 * 在那之前：GIF / 动态 WebP 在安卓上只画第一帧，静态图（JPG / PNG / 静态 WebP）不受影响。
 */
actual fun ComponentRegistry.Builder.addAnimatedImageDecoder(): ComponentRegistry.Builder = this
