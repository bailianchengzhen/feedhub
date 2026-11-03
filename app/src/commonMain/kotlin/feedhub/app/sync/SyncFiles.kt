package feedhub.app.sync

/**
 * 导出 / 导入同步文件时"选文件"的那一层：桌面用 `JFileChooser`，安卓用 SAF
 * （`ACTION_CREATE_DOCUMENT` / `ACTION_OPEN_DOCUMENT`）。界面（[feedhub.app.settings.SyncSettings]）
 * 只认这个接口，不碰平台 API。
 *
 * 实现里只做"选位置 / 读写字节"，加密和合并都在 [LibrarySync] / `feedhub.core.sync` 里，
 * 所以这层可以两端各写一份薄的（安卓那端只编译，不在设备上验）。
 */
interface SyncFiles {
    /** 让用户选保存位置并写入 [bytes]，返回给用户看的文件名；用户取消返回 null。 */
    suspend fun save(defaultName: String, bytes: ByteArray): String?

    /** 让用户选一个同步文件并读回来；用户取消返回 null。 */
    suspend fun open(): SyncPickedFile?
}

/** 用户选中的一个文件（名字只用于显示，不参与任何路径拼接）。 */
data class SyncPickedFile(val name: String, val bytes: ByteArray) {
    // ByteArray 在 data class 里默认按引用比较，这里显式写成内容比较，免得测试里踩坑
    override fun equals(other: Any?): Boolean =
        other is SyncPickedFile && name == other.name && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = 31 * name.hashCode() + bytes.contentHashCode()
}
