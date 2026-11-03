package feedhub.app.detail

import androidx.compose.runtime.Composable

/**
 * 大图查看器里「保存到相册 / 分享图片」的平台实现。
 *
 * 桌面两样都不支持（桌面版保持原样：箭头 / 键盘 / 滚轮），只有「复制图片链接」；
 * 安卓：保存走 MediaStore 直写 `Pictures/feedhub`（Android 10+ 不需要存储权限），分享走 `ACTION_SEND`。
 *
 * 返回的字符串是**给用户看的**一句提示（成功 / 为什么没成功），查看器在自己的右下角弹出来。
 */
interface ImageViewerActions {
    /** 能不能保存到系统相册。 */
    val canSaveToAlbum: Boolean

    /** 能不能调系统分享面板。 */
    val canShare: Boolean

    /** 把 [url] 这张图保存到相册。 */
    suspend fun saveToAlbum(url: String, fileName: String): String

    /** 用系统分享面板分享 [url] 这张图；空串 = 面板已经打开、不用再提示。 */
    suspend fun share(url: String, caption: String?): String
}

/** 当前平台的图片导出能力（桌面是全否的空实现，见 desktopMain 的 actual）。 */
@Composable
expect fun rememberImageViewerActions(): ImageViewerActions

/** 保存 / 分享文件名里的本地时间戳 `yyyyMMdd_HHmmss`（commonMain 里没有 java.time，两端各给一份 actual）。 */
expect fun fileStamp(): String

/** 保存 / 分享时用的文件名：`<说明或 feedhub>_<时间戳>_<序号>.<扩展名>`，说明里的不能进文件名的字符换成 `-`。 */
internal fun imageFileName(caption: String?, index: Int, stamp: String, extension: String): String {
    val base = caption?.let { sanitizeFileName(it) }?.takeIf { it.isNotBlank() } ?: "feedhub"
    val seq = (index + 1).toString().padStart(2, '0')
    return "${base}_${stamp}_$seq.$extension"
}

/** 只留字母 / 数字 / 汉字 / `-_`，其余换成 `-`；连着的 `-` 合成一个，太长截断（文件名在各家文件系统上都不能太长）。 */
internal fun sanitizeFileName(text: String): String {
    val sb = StringBuilder()
    for (ch in text.trim()) {
        when {
            ch.isLetterOrDigit() -> sb.append(ch)
            ch == '-' || ch == '_' -> sb.append(ch)
            else -> if (sb.isNotEmpty() && sb.last() != '-') sb.append('-')
        }
        if (sb.length >= 24) break
    }
    return sb.toString().trim('-', '_', ' ')
}

/**
 * 从文件头猜图片扩展名（保存到相册 / 分享时决定 MIME 和文件名用，不然相册里全是 `.jpg` 但内容是 PNG）。
 * 认 JPG / PNG / GIF / WebP，别的按 jpg 处理（绝大多数平台给的就是 jpg）。
 */
internal fun imageExtension(bytes: ByteArray): String {
    fun at(i: Int): Int = if (i < bytes.size) bytes[i].toInt() and 0xFF else -1
    return when {
        at(0) == 0xFF && at(1) == 0xD8 && at(2) == 0xFF -> "jpg"
        at(0) == 0x89 && at(1) == 0x50 && at(2) == 0x4E && at(3) == 0x47 -> "png"
        at(0) == 0x47 && at(1) == 0x49 && at(2) == 0x46 -> "gif"
        at(0) == 0x52 && at(1) == 0x49 && at(2) == 0x46 && at(3) == 0x46 &&
            at(8) == 0x57 && at(9) == 0x45 && at(10) == 0x42 && at(11) == 0x50 -> "webp"
        else -> "jpg"
    }
}

/** 扩展名 → MIME（相册和分享面板都认这个）。 */
internal fun imageMimeType(extension: String): String = when (extension.lowercase()) {
    "png" -> "image/png"
    "gif" -> "image/gif"
    "webp" -> "image/webp"
    else -> "image/jpeg"
}
