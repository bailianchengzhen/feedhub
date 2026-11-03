package feedhub.app.sync

import feedhub.app.AppSettings
import feedhub.app.library.ImportPreview
import feedhub.app.library.ImportResult
import feedhub.app.library.Library
import feedhub.app.library.SyncPeer
import feedhub.rust.RustSync
import feedhub.core.sync.DeviceKey
import feedhub.core.sync.DeviceKeyStore
import feedhub.core.sync.LibrarySnapshot
import feedhub.core.sync.LibrarySnapshotCodec
import feedhub.core.sync.LibrarySyncFile
import feedhub.core.sync.LibrarySyncFormatException
import feedhub.core.sync.SyncCrypto

/**
 * 手动导出 / 导入的编排层。
 *
 * **导出**默认发给全部已配对设备（+ 本机自己）：随机文件密钥 → 正文 gzip + AES-256-GCM，
 * 文件头里给每台设备包一份文件密钥（见 `LibrarySyncFile.sealToRecipients`）。
 * 给没配对的设备 / 备用；密码最短 12 位。
 *
 * **导入**先读文件头：里面有本机的收件条目就用本机设备密钥直接解（**不问密码**、界面写"已用本机密钥解开"）；
 * 密码文件才回落到输密码；不是给本机的按设备文件直接报错（没有密码可回落）。
 *
 * 解密之后先算一份 [ImportPreview] 给用户看，确认了才在**一个事务**里合并。密码 / 私钥都不进日志、不落盘。
 *
 * @param deviceKeys 设备密钥的落盘位置（桌面 `~/.config/feedhub`，安卓 App 私有目录）；演示 / 测试不给就只支持密码。
 * @param deviceName 配对时给对面看的名字（桌面主机名、安卓机型）。
 */
class LibrarySync(
    private val library: Library,
    private val files: SyncFiles?,
    private val settings: () -> AppSettings,
    private val deviceKeys: DeviceKeyStore? = null,
    private val deviceName: () -> String = { "这台设备" },
    private val now: () -> Long = System::currentTimeMillis,
) {
    /** 这台设备这个构建能不能导出 / 导入（桌面 / 安卓的 `RustLibrary` + 文件选择器都在才有）。 */
    val available: Boolean get() = files != null && library.supportsSync

    /** 能不能用设备密钥（配对 / 按设备导出）——文件选择器不是必须的。 */
    val devicePairingAvailable: Boolean get() = deviceKeys != null && library.supportsSync

    /** 本机的设备 id（8 字节 base64url；库不支持就是 null）。 */
    fun selfDeviceId(): String? = library.syncDeviceId()

    /** 本机的设备密钥；**第一次真的要配对 / 按设备导出时才生成并落盘**。 */
    fun deviceKey(): DeviceKey? {
        val store = deviceKeys ?: return null
        val id = library.syncDeviceId() ?: return null
        return store.loadOrCreate(id)
    }

    /**
     * 已经在盘上的设备密钥（**不生成**）。导入按设备加密的文件时用它：
     * 密钥文件被删 / 换过就明说"配对时的设备密钥不在了"，而不是悄悄生成一把新的再报"文件被改过"。
     */
    fun existingDeviceKey(): DeviceKey? {
        val store = deviceKeys ?: return null
        val id = library.syncDeviceId() ?: return null
        return runCatching { store.load(id) }.getOrNull()
    }

    /** 已配对设备（读库，界面列表用）。 */
    fun peers(): List<SyncPeer> = library.syncPeers()

    /** 配对成功后由界面对话框调用：写进 `sync_peer`（下次导出就发给它）。 */
    fun rememberPeer(deviceId: String, name: String, publicKey: ByteArray) {
        library.upsertSyncPeer(
            SyncPeer(
                deviceId = deviceId,
                name = name,
                publicKey = SyncCrypto.base64Url(publicKey),
                pairedAt = now(),
            ),
        )
    }

    /** 移除一台已配对设备（导出不再发给它；它的导入序号也清掉）。 */
    fun forgetPeer(deviceId: String): Boolean = library.removeSyncPeer(deviceId)

    /** 配对时给对面看的名字。 */
    fun pairingName(): String = deviceName()

    /**
     * 导出：默认发给全部已配对设备（含本机自己），**不问密码**。
     *
     * 挑收件人 / 判失败原因 / 加密都在 Rust（[feedhub.rust.RustSync.planExportToPaired] +
     * `ffi_sync_file_seal_to_recipients`），这里只剩文件选择器和界面那层。
     */
    suspend fun exportToPairedDevices(): ExportOutcome {
        val picker = files ?: return ExportOutcome.Failed("这个版本没有接文件选择")
        if (!library.supportsSync) return ExportOutcome.Failed("这台设备上的资料库不支持同步")
        val store = deviceKeys ?: return ExportOutcome.Failed("这个版本没有接设备密钥")
        val selfId = library.syncDeviceId() ?: return ExportOutcome.Failed("这台设备还没有设备标识")
        val self = runCatching { store.loadOrCreate(selfId) }.getOrNull()
            ?: return ExportOutcome.Failed("生成 / 读取本机设备密钥失败")
        val plan = try {
            RustSync.planExportToPaired(
                selfId = self.deviceId,
                selfPublicKey = SyncCrypto.base64Url(self.publicKey),
                peers = library.syncPeers().map { it.deviceId to it.publicKey },
            )
        } catch (e: Exception) {
            return ExportOutcome.Failed(e.message ?: "生成 / 读取本机设备密钥失败")
        }
        val at = now()
        val snapshot = library.syncSnapshot(at) ?: return ExportOutcome.Failed("生成快照失败")
        // 加密（快照 → 明文 → 每个收件人包一份文件密钥）在 Rust 里：这里只给设备 id / 序号 / 收件人。
        val sealed = try {
            RustSync.sealToRecipients(
                plaintext = RustSync.encodeSnapshot(snapshot),
                device = snapshot.device,
                generatedAt = snapshot.generatedAt,
                seq = snapshot.seq,
                recipients = plan.recipients.map { (id, key) -> id to (SyncCrypto.base64UrlDecode(key) ?: ByteArray(0)) },
                fileKey = secureRandom(32),
                ephemeralSecret = secureRandom(32),
                nonce = secureRandom(12),
                wrapNonces = plan.recipients.map { secureRandom(12) },
            )
        } catch (e: Exception) {
            return ExportOutcome.Failed(e.message ?: "加密失败")
        }
        val name = picker.save(defaultFileName(at), sealed) ?: return ExportOutcome.Cancelled
        return ExportOutcome.Saved(
            fileName = name,
            bytes = sealed.size,
            favorites = snapshot.favorites.size,
            history = snapshot.history.size,
            mode = LibrarySyncFile.Mode.RECIPIENTS,
            recipients = plan.recipients.size,
            skippedPeers = plan.skippedPeers,
        )
    }

    /**
     * 导出：改用密码（给没配对的设备 / 备用），
     *
     * 密码长度校验在 Rust（[RustSync.checkPassword]，最短 12 位），加密也是（`ffi_sync_file_seal_with_password`）。
     */
    suspend fun export(password: CharArray): ExportOutcome {
        val picker = files ?: return ExportOutcome.Failed("这个版本没有接文件选择")
        if (!library.supportsSync) return ExportOutcome.Failed("这台设备上的资料库不支持同步")
        try {
            RustSync.checkPassword(password.size)
        } catch (e: Exception) {
            return ExportOutcome.Failed(e.message ?: "同步密码至少 $MIN_PASSWORD_LENGTH 位")
        }
        val at = now()
        val snapshot = library.syncSnapshot(at) ?: return ExportOutcome.Failed("生成快照失败")
        val sealed = try {
            RustSync.sealWithPassword(
                plaintext = RustSync.encodeSnapshot(snapshot),
                password = password.concatToString().encodeToByteArray(),
                device = snapshot.device,
                generatedAt = snapshot.generatedAt,
                seq = snapshot.seq,
                // 生产的迭代次数（和 Kotlin 原实现的 `LibrarySyncFile.DEFAULT_ITERATIONS` 一致）
                iterations = LibrarySyncFile.DEFAULT_ITERATIONS,
                salt = secureRandom(16),
                nonce = secureRandom(12),
            )
        } catch (e: Exception) {
            return ExportOutcome.Failed(e.message ?: "加密失败")
        }
        val name = picker.save(defaultFileName(at), sealed) ?: return ExportOutcome.Cancelled
        return ExportOutcome.Saved(
            fileName = name,
            bytes = sealed.size,
            favorites = snapshot.favorites.size,
            history = snapshot.history.size,
            mode = LibrarySyncFile.Mode.PASSWORD,
            recipients = 0,
            skippedPeers = 0,
        )
    }

    /** 选一个文件（还没解密）。 */
    suspend fun pickImportFile(): SyncPickedFile? = files?.open()

    /**
     * 只看文件头：这份文件是按设备加密还是密码、发给谁、有没有本机。
     * 界面据此决定"直接解（不问密码）"还是"弹密码框"。
     */
    fun inspect(file: SyncPickedFile): ImportInspection {
        // 判定在 Rust（`ffi_sync_inspect`）：头读不出来时给一句话、给不给本机也能一眼看出来。
        val view = RustSync.inspect(file.bytes, library.syncDeviceId())
        val mode = if (view.mode == "recipients") LibrarySyncFile.Mode.RECIPIENTS else LibrarySyncFile.Mode.PASSWORD
        return ImportInspection(
            fileName = file.name,
            mode = if (view.error != null) null else mode,
            recipients = view.recipients,
            forThisDevice = view.forThisDevice,
            error = view.error,
        )
    }

    /**
     * 解开刚选中的文件并算一份导入预览。
     *
     * @param password 只在密码文件上用；按设备加密的文件**不看这个参数**（用本机设备密钥）。
     * @return [ImportOutcome.NeedsPassword] = 这是密码文件，界面弹密码框。
     */
    fun preview(file: SyncPickedFile, password: CharArray? = null): ImportOutcome {
        // 三条判定（是不是发给本机的 / 要不要问密码 / 头尾对不对）全在 Rust
        // （`feedhub-core/src/sync/flow.rs` 的 `preview`）：这里只把设备私钥和密码递进去。
        val outcome = RustSync.preview(
            bytes = file.bytes,
            selfId = library.syncDeviceId(),
            devicePrivateKey = runCatching { existingDeviceKey()?.copyPrivateKey() }.getOrNull(),
            password = password?.concatToString(),
        )
        val (snapshot, unlock) = when (outcome) {
            is RustSync.UnlockOutcome.Opened -> outcome.snapshot to
                if (outcome.byDeviceKey) LibrarySyncFile.Unlock.DEVICE_KEY else LibrarySyncFile.Unlock.PASSWORD
            RustSync.UnlockOutcome.NeedsPassword -> return ImportOutcome.NeedsPassword
            is RustSync.UnlockOutcome.Failed -> return ImportOutcome.Failed(outcome.message)
        }
        val preview = try {
            library.previewImport(snapshot, now(), retentionCutoffMs())
        } catch (e: Exception) {
            return ImportOutcome.Failed(e.message ?: "读不了本机资料库")
        } ?: return ImportOutcome.Failed("这台设备上的资料库不支持同步")
        return ImportOutcome.Ready(file.name, snapshot, preview, unlock)
    }

    /** 用户确认之后真正合并（一个事务；[ignoreStale] 只用于"我就要恢复这个旧备份"）。 */
    fun apply(ready: ImportOutcome.Ready, ignoreStale: Boolean = false): ImportResult =
        library.applyImport(ready.snapshot, now(), retentionCutoffMs(), ignoreStale)
            ?: ImportResult(
                applied = false, stale = false, counts = feedhub.core.sync.SyncCounts(),
                foldersCreated = 0, foldersClaimed = 0, clearedRows = 0, skippedItems = 0,
                rejectedReason = "这台设备上的资料库不支持同步",
            )

    /** 本机"只保留最近 N 天"的截止时间；没开就是 null（保留期是本机策略，不传播）。Rust 算。 */
    private fun retentionCutoffMs(): Long? {
        val s = settings()
        return RustSync.retentionCutoff(s.historyLimitOn, s.historyDays, now())
    }

    /**
     * 导出的默认文件名：`feedhub-yyyyMMdd-HHmm.fhshare`。
     *
     * **本地时间的年月日时分由这里算**（时区是操作系统的知识，Rust 不碰），拼字符串在 Rust 里
     * （[RustSync.defaultFileName]），这样两边只有一套格式。
     */
    private fun defaultFileName(at: Long): String {
        val cal = java.util.Calendar.getInstance()
        cal.timeInMillis = at
        return RustSync.defaultFileName(
            year = cal.get(java.util.Calendar.YEAR),
            month = cal.get(java.util.Calendar.MONTH) + 1,
            day = cal.get(java.util.Calendar.DAY_OF_MONTH),
            hour = cal.get(java.util.Calendar.HOUR_OF_DAY),
            minute = cal.get(java.util.Calendar.MINUTE),
        )
    }

    /**
     * 加密要用的随机字节（文件密钥 / 临时私钥 / nonce / salt）。
     *
     * 这里是**宿主**的活（`SecureRandom`）：`.so` 里不自己取随机数（和别处"随机由宿主给"一条规矩）。
     */
    private fun secureRandom(size: Int): ByteArray = ByteArray(size).also { SECURE_RANDOM.nextBytes(it) }

    companion object {
        /** 密码模式的最短长度。现在读 Rust 的那份。 */
        val MIN_PASSWORD_LENGTH: Int get() = RustSync.minPasswordLength

        private val SECURE_RANDOM = java.security.SecureRandom()
    }
}

sealed interface ExportOutcome {
    data class Saved(
        val fileName: String,
        val bytes: Int,
        val favorites: Int,
        val history: Int,
        /** 按设备加密还是密码。 */
        val mode: LibrarySyncFile.Mode,
        /** 收件设备数（密码模式是 0）。 */
        val recipients: Int,
        /** 公钥读不出来、被跳过的已配对设备（正常是 0）。 */
        val skippedPeers: Int,
    ) : ExportOutcome

    data object Cancelled : ExportOutcome
    data class Failed(val message: String) : ExportOutcome
}

sealed interface ImportOutcome {
    data class Ready(
        val fileName: String,
        val snapshot: LibrarySnapshot,
        val preview: ImportPreview,
        /** 怎么解开的：设备密钥（没问密码）还是密码。 */
        val unlock: LibrarySyncFile.Unlock,
    ) : ImportOutcome

    /** 这是密码文件：界面弹密码框，输完再 [LibrarySync.preview]。 */
    data object NeedsPassword : ImportOutcome

    data object Cancelled : ImportOutcome
    data class Failed(val message: String) : ImportOutcome
}

/** 只看文件头的结论（决定导入时要不要问密码）。 */
data class ImportInspection(
    val fileName: String,
    val mode: LibrarySyncFile.Mode?,
    /** 收件设备 id（只有按设备加密才有）。 */
    val recipients: List<String>,
    /** 文件里有本机的收件条目 → 可以用本机密钥直接解，不问密码。 */
    val forThisDevice: Boolean,
    /** 文件头都读不出来时的错误。 */
    val error: String? = null,
)

/** 截图工具用来直接打开"导入预览"弹框（平时的入口是点按钮）。 */
data class SyncPreviewDebug(
    val fileName: String,
    val ready: ImportOutcome.Ready,
)

/**
 * 截图工具用来直接摆出配对弹框（**不监听端口、不发请求**）：
 * [qrContent] 不为空 → 显示二维码那一步；否则按 [code] 显示核对 6 位码那一步。
 */
data class SyncPairingDebug(
    val qrContent: String = "",
    val peerName: String = "",
    val code: String = "",
)
