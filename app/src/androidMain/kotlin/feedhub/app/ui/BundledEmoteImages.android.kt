package feedhub.app.ui

import android.graphics.BitmapFactory
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap

/**
 * 安卓：用 `BitmapFactory` 解这段字节（表情都是小 PNG，44~96 px），包成 Compose 的 [ImageBitmap]。
 * 同步解一张在零点几毫秒级。
 */
actual fun decodeBundledEmote(bytes: ByteArray): ImageBitmap? = try {
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
} catch (e: Exception) {
    null
}
