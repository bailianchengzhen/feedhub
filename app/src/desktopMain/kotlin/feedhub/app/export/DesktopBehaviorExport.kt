package feedhub.app.export

import feedhub.core.local.LocalFiles
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.BufferedWriter
import java.io.File
import java.io.OutputStreamWriter
import java.nio.file.Path

/**
 * 桌面的行为数据导出：写到 `~/.config/feedhub/export/`（[LocalFiles.home] 旁边——桌面版的数据
 * （`library.db`、`state.json`、Cookie）本来就都放这个目录，不另起一个"数据目录"约定）。
 *
 * 只写自己这个目录里的 `behavior-*.jsonl`。
 *
 * @param dir 测试 / 截图可以换成临时目录。
 */
class DesktopBehaviorExport(private val dir: Path = LocalFiles.home.resolve("export")) : BehaviorExportTarget {
    override val available: Boolean get() = true

    override val location: String get() = dir.toString()

    override suspend fun write(name: String, lines: List<String>): String? = withContext(Dispatchers.IO) {
        runCatching {
            val d = dir.toFile()
            if (!d.isDirectory && !d.mkdirs()) error("目录建不出来：${d.absolutePath}")
            val file = File(d, name)
            BufferedWriter(OutputStreamWriter(file.outputStream(), Charsets.UTF_8)).use { w ->
                for (line in lines) {
                    w.write(line)
                    w.write("\n")
                }
            }
            file.absolutePath
        }.getOrElse { e ->
            System.err.println("[导出] 写文件失败：${e.message}")
            null
        }
    }
}
