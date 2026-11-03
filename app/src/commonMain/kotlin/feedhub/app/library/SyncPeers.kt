package feedhub.app.library

/**
 * 一台**已配对**的设备。
 *
 * 存在 `sync_peer` 表里（和导入序号同一行）：`peer` = 8 字节设备 id（base64url）、`name` 设备名、
 * `public_key` 设备公钥（X25519，base64url）、`paired_at` 配对时间。
 * 只有 `public_key` 不为空的行才算"已配对"；导入过文件但没有配对过的设备也会有一行 sync_peer，
 * 但不会出现在已配对列表里、导出也不会发给它。
 *
 * 私钥不在这个结构里（它只在 `DeviceKeyStore` 的 600 文件里）。
 */
data class SyncPeer(
    val deviceId: String,
    val name: String,
    val publicKey: String,
    val pairedAt: Long,
)
