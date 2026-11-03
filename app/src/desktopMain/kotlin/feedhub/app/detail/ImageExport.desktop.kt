package feedhub.app.detail

import androidx.compose.runtime.Composable
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

/** 桌面：不支持保存到相册 / 分享（桌面版的大图查看器保持原样：箭头 / 键盘 / 滚轮 + 复制链接）。 */
@Composable
actual fun rememberImageViewerActions(): ImageViewerActions = DesktopImageViewerActions

private object DesktopImageViewerActions : ImageViewerActions {
    override val canSaveToAlbum = false
    override val canShare = false

    override suspend fun saveToAlbum(url: String, fileName: String): String = "桌面版不支持保存到相册"

    override suspend fun share(url: String, caption: String?): String = "桌面版不支持分享图片"
}

/** 桌面的时间戳（文件名用）。 */
actual fun fileStamp(): String = LocalDateTime.now().format(DateTimeFormatter.ofPattern("yyyyMMdd_HHmmss"))
