package feedhub.app.library

import feedhub.core.sync.SyncCounts

/**
 * 一次导入的预览（只算不写）：界面拿它给用户确认"新增 / 更新 / 删除各几条"。
 *
 * @param counts 相对本机的增 / 改 / 删（见 [SyncCounts]）
 * @param foldersCreated 合并后会在本机新建几个收藏夹
 * @param foldersClaimed 本机有几个收藏夹会和文件里的**同名**收藏夹接通（不新建第二个）
 * @param historyClearedByRemote 文件里的"清空历史"标记比本机新：导入会按它清掉本机旧历史
 * @param clearedRows 会被清掉的本机历史条数
 * @param skippedItems 文件里解不开 / 太大的条目（只跳过这几条，不影响整份）
 * @param stale 这个文件比上次从同一台设备导入的**旧**（可能是把旧备份放回来了）
 * @param lastImportedSeq 上次从这台设备导入的序号
 */
data class ImportPreview(
    val counts: SyncCounts,
    val foldersCreated: Int,
    val foldersClaimed: Int,
    val historyClearedByRemote: Boolean,
    val clearedRows: Int,
    val skippedItems: Int,
    val stale: Boolean,
    val lastImportedSeq: Long,
    val sourceDevice: String,
    val sourceSeq: Long,
    val sourceGeneratedAt: Long,
) {
    /** 有没有会改动本机的东西（没有就只显示"和本机一致"）。 */
    val hasChanges: Boolean
        get() = counts.total > 0 || foldersCreated > 0 || foldersClaimed > 0 || historyClearedByRemote || clearedRows > 0
}

/**
 * 一次导入的结果。[applied] 为 false 时看 [stale] / [rejectedReason]：
 * 文件名 / 密码不对在更早的一步就报错了，这里是"文件没问题但不该导入"（旧快照）。
 */
data class ImportResult(
    val applied: Boolean,
    val stale: Boolean,
    val counts: SyncCounts,
    val foldersCreated: Int,
    val foldersClaimed: Int,
    val clearedRows: Int,
    val skippedItems: Int,
    val rejectedReason: String? = null,
)
