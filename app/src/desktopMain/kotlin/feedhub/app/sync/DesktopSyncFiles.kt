package feedhub.app.sync

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.swing.Swing
import kotlinx.coroutines.withContext
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions
import javax.swing.JFileChooser
import javax.swing.filechooser.FileNameExtensionFilter

/**
 * 桌面版的选文件层：`JFileChooser` 另存 / 打开，写文件用**临时文件 + 原子替换**，写完把权限设成 600
 * （和 `~/.config/feedhub/` 下别的文件一致；导出文件里是浏览历史）。
 *
 * 只在用户点「导出 / 导入」时弹框，平时不碰磁盘
 * （[feedhub.app.settings.SyncSettings] 里点按钮才调）。
 */
class DesktopSyncFiles : SyncFiles {
    override suspend fun save(defaultName: String, bytes: ByteArray): String? {
        val target = withContext(Dispatchers.Swing) {
            val chooser = JFileChooser().apply {
                dialogTitle = "导出同步文件"
                selectedFile = File(defaultName)
                fileFilter = FileNameExtensionFilter("feedhub 同步文件 (*.${feedhub.core.sync.LibrarySyncFile.EXTENSION})", "feedhub-sync")
                isAcceptAllFileFilterUsed = true
            }
            if (chooser.showSaveDialog(null) != JFileChooser.APPROVE_OPTION) return@withContext null
            var file = chooser.selectedFile
            if (!file.name.endsWith(".${feedhub.core.sync.LibrarySyncFile.EXTENSION}")) {
                file = File(file.parentFile, file.name + ".${feedhub.core.sync.LibrarySyncFile.EXTENSION}")
            }
            file.toPath()
        } ?: return null
        withContext(Dispatchers.IO) { writeAtomically(target, bytes) }
        return target.fileName.toString()
    }

    override suspend fun open(): SyncPickedFile? {
        val file = withContext(Dispatchers.Swing) {
            val chooser = JFileChooser().apply {
                dialogTitle = "选择同步文件"
                fileFilter = FileNameExtensionFilter("feedhub 同步文件 (*.${feedhub.core.sync.LibrarySyncFile.EXTENSION})", "feedhub-sync")
                isAcceptAllFileFilterUsed = true
            }
            if (chooser.showOpenDialog(null) != JFileChooser.APPROVE_OPTION) return@withContext null
            chooser.selectedFile
        } ?: return null
        val bytes = withContext(Dispatchers.IO) {
            require(file.isFile) { "不是一个文件" }
            val size = Files.size(file.toPath())
            require(size <= feedhub.core.sync.LibrarySyncFile.MAX_FILE_BYTES) {
                "文件太大（${size / 1024 / 1024} MB）"
            }
            Files.readAllBytes(file.toPath())
        }
        return SyncPickedFile(file.name, bytes)
    }

    private fun writeAtomically(target: Path, bytes: ByteArray) {
        val dir = target.toAbsolutePath().parent ?: error("没有父目录")
        val tmp = Files.createTempFile(dir, ".feedhub-sync-", ".tmp")
        try {
            Files.write(tmp, bytes)
            runCatching {
                Files.setPosixFilePermissions(tmp, PosixFilePermissions.fromString("rw-------"))
            }
            try {
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
            } catch (e: Exception) {
                // 有些文件系统（FAT / 某些 fuse）不支持原子替换，退回普通 move
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING)
            }
            runCatching {
                Files.setPosixFilePermissions(target, PosixFilePermissions.fromString("rw-------"))
            }
        } finally {
            runCatching { Files.deleteIfExists(tmp) }
        }
    }
}
