package feedhub.app.detail

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.FileProvider
import coil3.SingletonImageLoader
import coil3.request.ImageRequest
import coil3.request.SuccessResult
import coil3.toBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.File
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

/**
 * 安卓的「保存到相册 / 分享图片」。
 *
 * **保存走的是查看器里已经加载过的那张原图**，不再下一次：先到 Coil 的磁盘缓存里取原始字节（就是原图本身，
 * 不重新编码、不改画质），缓存里没有原始字节时（极少数：图片只命中内存缓存）才让 Coil 解一遍再编码成 PNG。
 *
 * 保存用 `MediaStore.Images` 直写 `Pictures/feedhub`，**Android 10 起不需要存储权限**（`IS_PENDING` 写完再转正，
 * 别的 App 不会扫到半张图）；Android 9 及以下写相册要么申请 `WRITE_EXTERNAL_STORAGE` 要么走别的路，这里不申请，
 * 直接告诉用户改用「分享图片」。
 */
@Composable
actual fun rememberImageViewerActions(): ImageViewerActions {
    val context = LocalContext.current
    return remember(context) { AndroidImageViewerActions(context.applicationContext) }
}

private val StampFormat = DateTimeFormatter.ofPattern("yyyyMMdd_HHmmss")

/** 安卓的时间戳（文件名用）。 */
actual fun fileStamp(): String = LocalDateTime.now().format(StampFormat)

private class AndroidImageViewerActions(private val context: Context) : ImageViewerActions {
    override val canSaveToAlbum = true
    override val canShare = true

    override suspend fun saveToAlbum(url: String, fileName: String): String = withContext(Dispatchers.IO) {
        val bytes = readImageBytes(context, url) ?: return@withContext NOT_READY
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return@withContext "Android 9 及以下写相册要存储权限；点「分享图片」也能存下来"
        val ext = imageExtension(bytes)
        val name = fileName.ensureExtension(ext)
        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, name)
            put(MediaStore.Images.Media.MIME_TYPE, imageMimeType(ext))
            put(MediaStore.Images.Media.RELATIVE_PATH, "${Environment.DIRECTORY_PICTURES}/feedhub")
            put(MediaStore.Images.Media.IS_PENDING, 1)
        }
        val uri = runCatching { resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) }.getOrNull()
            ?: return@withContext "相册写不进去（系统拒绝了 MediaStore）"
        try {
            val out = resolver.openOutputStream(uri) ?: error("没有可写的输出流")
            out.use { it.write(bytes) }
            values.clear()
            values.put(MediaStore.Images.Media.IS_PENDING, 0)
            runCatching { resolver.update(uri, values, null, null) }
            "已保存到相册（Pictures/feedhub/）"
        } catch (e: Exception) {
            runCatching { resolver.delete(uri, null, null) }
            "保存失败：${e.message ?: e::class.simpleName}"
        }
    }

    override suspend fun share(url: String, caption: String?): String = withContext(Dispatchers.IO) {
        val bytes = readImageBytes(context, url) ?: return@withContext NOT_READY
        val ext = imageExtension(bytes)
        val name = imageFileName(caption, 0, LocalDateTime.now().format(StampFormat), ext)
        val dir = File(context.cacheDir, "share").apply { mkdirs() }
        // 清掉上一次分享留下的（同名会覆盖，这里只防越攒越多）
        dir.listFiles()?.filter { it.lastModified() < System.currentTimeMillis() - 24 * 3600_000L }?.forEach { it.delete() }
        val file = File(dir, name)
        try {
            file.writeBytes(bytes)
        } catch (e: Exception) {
            return@withContext "分享失败：${e.message ?: e::class.simpleName}"
        }
        val uri = runCatching { FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file) }
            .getOrElse { return@withContext "分享失败：文件共享通道没配好" }
        val intent = Intent(Intent.ACTION_SEND).apply {
            type = imageMimeType(ext)
            putExtra(Intent.EXTRA_STREAM, uri)
            caption?.takeIf { it.isNotBlank() }?.let { putExtra(Intent.EXTRA_TEXT, it) }
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        withContext(Dispatchers.Main) {
            val chooser = Intent.createChooser(intent, "分享图片")
            // context 是 applicationContext（不持有 Activity），分享面板要新开一个任务
            chooser.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            runCatching { context.startActivity(chooser) }
                .getOrElse { return@withContext "没有能接收图片的 App" }
        }
        ""
    }

    private fun String.ensureExtension(ext: String): String =
        if (endsWith(".$ext", ignoreCase = true)) this else "${substringBeforeLast('.')}.$ext"
}

private const val NOT_READY = "原图还没下载完，等它显示出来再试"

/**
 * 取一张图的**原始字节**：优先 Coil 磁盘缓存（查看器里显示过这张图，缓存里就是原图，不发请求、不重新编码），
 * 缓存里没有原始字节时退回"解码 + 编码成 PNG"。
 */
private suspend fun readImageBytes(context: Context, url: String): ByteArray? {
    val loader = SingletonImageLoader.get(context)
    val request = ImageRequest.Builder(context).data(url).build()
    val cache = loader.diskCache
    // 注意：`ImageRequest.diskCacheKey` 在 execute 之前是 null（Coil 要等跑起来才把 options 算出来），
    // 而网络图片的磁盘缓存键就是地址本身（Coil 默认 Keyer 对非 Uri 数据取 `data.toString()`，
    // 离线用例 ImageDiskCacheTest 盯着这一条）。所以这里退回用 url 当键。
    val key = request.diskCacheKey ?: url
    if (cache != null) {
        val snapshot = runCatching { cache.openSnapshot(key) }.getOrNull()
        val path = snapshot?.data
        if (snapshot != null && path != null) {
            try {
                val bytes = runCatching { cache.fileSystem.read(path) { readByteArray() } }.getOrNull()
                if (bytes != null && bytes.isNotEmpty()) return bytes
            } finally {
                runCatching { snapshot.close() }
            }
        } else {
            runCatching { snapshot?.close() }
        }
    }
    val result = runCatching { loader.execute(request) }.getOrNull() as? SuccessResult ?: return null
    val bitmap = runCatching { result.image.toBitmap() }.getOrNull() ?: return null
    return ByteArrayOutputStream().use { out ->
        bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
        out.toByteArray()
    }
}
