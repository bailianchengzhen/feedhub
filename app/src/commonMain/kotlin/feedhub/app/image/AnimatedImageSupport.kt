package feedhub.app.image

import coil3.ComponentRegistry

/**
 * 让 Coil 的 ImageLoader 能解动图（GIF / 动态 WebP）并让它动起来，在 `ImageLoader.Builder.components { }` 里调用。
 * 动图在所有平台都要会动。
 *
 * - 桌面：Coil 的 `coil-gif` 只有安卓版，桌面上 Coil 用 Skia 解码只出第一帧，所以自己写了解码器（用 Skia 的 `Codec` 按帧解），见 desktopMain 的 `AnimatedImageDecoder`。
 * - 安卓：以后直接用 `coil-gif`（`AnimatedImageDecoder` / `GifDecoder`），现在还没有安卓目标。
 */
expect fun ComponentRegistry.Builder.addAnimatedImageDecoder(): ComponentRegistry.Builder
