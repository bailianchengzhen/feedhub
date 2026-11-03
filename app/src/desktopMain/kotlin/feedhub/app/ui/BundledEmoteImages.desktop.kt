package feedhub.app.ui

import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import org.jetbrains.skia.Image as SkiaImage

/**
 * 桌面：用 Skia 解这段字节（和 Coil 桌面侧同一个解码器），再包成 Compose 的 [ImageBitmap]。
 * 表情都是小 PNG（44~96 px），同步解一张在微秒到毫秒级（见 `BundledEmoteImagesTest` 的耗时）。
 */
actual fun decodeBundledEmote(bytes: ByteArray): ImageBitmap? = try {
    SkiaImage.makeFromEncoded(bytes).toComposeImageBitmap()
} catch (e: Exception) {
    null
}
