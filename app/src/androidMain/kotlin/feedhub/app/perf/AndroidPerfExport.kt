package feedhub.app.perf

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File

/**
 * 把记录文件复制到**系统下载目录**（`Download/feedhub-perf/`），给 `adb pull` 兜底。
 *
 * 为什么要这一步：
 * 主记录位置是 `context.getExternalFilesDir("perf")` = `/sdcard/Android/data/feedhub.app/files/perf/`。
 * **Android 11 起分区存储对 `Android/data` 有 shell 访问限制**：非 debuggable 的 release 包，`adb pull` 这个目录
 * 可能被拒（"Permission denied"）。release 包又必须是 release（debug 版 Compose 慢约 3 倍，量出来不准），
 * 所以设置页给了这个「导出到下载目录」：点一下，文件就落到
 * `Download/feedhub-perf/`——那是公共目录，`adb pull /sdcard/Download/feedhub-perf/` 一定拉得到。
 * 只复制我们自己目录里 `perf-????????.jsonl` 这些文件，别的什么都不碰。
 *
 * API 29+ 走 MediaStore（不需要任何存储权限）；API 26~28 直接写公共目录需要 `WRITE_EXTERNAL_STORAGE`，
 * 那几版 `Android/data` 本来就能 pull，所以这里只提示"不用导出"。
 */
internal object AndroidPerfExport {
    /** 下载目录下的子目录名（`Download/feedhub-perf`）。 */
    const val DOWNLOAD_DIR = "feedhub-perf"

    /** 把 [files] 复制过去；返回给人看的一句话（不含路径以外的机密——这里也没有机密）。 */
    fun export(context: Context, files: List<File>): String {
        if (files.isEmpty()) return "还没有记录文件"
        if (Build.VERSION.SDK_INT < 29) {
            return "Android 10 以下直接 adb pull 应用目录就行，不用导出"
        }
        val resolver = context.contentResolver
        var copied = 0
        var failed: String? = null
        for (f in files) {
            runCatching {
                // 同名先删掉，不然 MediaProvider 会自动改名成 "xxx (1).jsonl"，越导越多
                resolver.delete(
                    MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                    "${MediaStore.MediaColumns.DISPLAY_NAME} = ? AND ${MediaStore.MediaColumns.RELATIVE_PATH} = ?",
                    arrayOf(f.name, "Download/$DOWNLOAD_DIR/"),
                )
                val values = ContentValues().apply {
                    put(MediaStore.MediaColumns.DISPLAY_NAME, f.name)
                    put(MediaStore.MediaColumns.MIME_TYPE, "application/json")
                    put(MediaStore.MediaColumns.RELATIVE_PATH, "Download/$DOWNLOAD_DIR")
                }
                val uri: Uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                    ?: error("系统没给下载目录的写入位置")
                resolver.openOutputStream(uri)?.use { out ->
                    f.inputStream().use { input -> input.copyTo(out) }
                } ?: error("打不开下载文件的输出流")
                copied++
            }.onFailure { failed = it.message ?: it::class.simpleName }
        }
        return when {
            copied > 0 && failed == null -> "已导出 $copied 份到 Download/$DOWNLOAD_DIR/"
            copied > 0 -> "导出了 $copied 份，还有失败：$failed"
            else -> "导出失败：${failed ?: "未知原因"}"
        }
    }

    /** 老系统（API < 29）公共下载目录的路径，只用来在提示里写清楚（不写文件）。 */
    @Suppress("DEPRECATION")
    fun legacyDir(): File = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), DOWNLOAD_DIR)
}
