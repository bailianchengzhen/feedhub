package feedhub.app.export

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.BufferedWriter
import java.io.File
import java.io.OutputStreamWriter

/**
 * 安卓的行为数据导出：写到 `getExternalFilesDir(null)/export/`（和性能记录同一种做法）。
 *
 * 写**应用私有目录之外**的东西（比如系统下载目录）在 Android 11+ 要 MediaStore / SAF，当前不需要。
 * **release 包里也能用**（这里没有任何 debuggable 判断）。
 *
 * 只写自己这个目录里的 `behavior-*.jsonl`，不碰别的文件。
 */
class AndroidBehaviorExport(private val context: Context) : BehaviorExportTarget {
    private val dir: File? get() = context.getExternalFilesDir(null)?.resolve("export")

    override val available: Boolean get() = dir != null

    override val location: String get() = dir?.absolutePath ?: "（这台设备没有外置应用目录）"

    override suspend fun write(name: String, lines: List<String>): String? = withContext(Dispatchers.IO) {
        val d = dir ?: return@withContext null
        runCatching {
            if (!d.isDirectory && !d.mkdirs()) error("目录建不出来：${d.absolutePath}")
            val file = File(d, name)
            BufferedWriter(OutputStreamWriter(file.outputStream(), Charsets.UTF_8)).use { w ->
                for (line in lines) {
                    w.write(line)
                    w.write("\n")
                }
            }
            Log.i(TAG, "[导出] 写好了 ${file.name}（${lines.size} 行 / ${file.length() / 1024} KB）")
            file.absolutePath
        }.getOrElse { e ->
            Log.w(TAG, "[导出] 写文件失败：${e.message}")
            null
        }
    }

    companion object {
        private const val TAG = "feedhub-embed"
    }
}
