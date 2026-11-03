package feedhub.rust

import feedhub.core.FeedJson
import feedhub.core.sync.LibrarySyncFormatException
import feedhub.core.sync.LibrarySnapshot
import feedhub.core.sync.MergeOutcome
import feedhub.core.sync.SyncBlockedRow
import feedhub.core.sync.SyncCounts
import feedhub.core.sync.SyncCrypto
import feedhub.core.sync.SyncFavoriteRow
import feedhub.core.sync.SyncFolderRow
import feedhub.core.sync.SyncHistoryRow
import feedhub.core.sync.SyncState
import uniffi.feedhub_ffi.FfiSyncBlockedRow
import uniffi.feedhub_ffi.FfiSyncCounts
import uniffi.feedhub_ffi.FfiSyncException
import uniffi.feedhub_ffi.FfiSyncFavoriteRow
import uniffi.feedhub_ffi.FfiSyncFolderAlias
import uniffi.feedhub_ffi.FfiSyncFolderRow
import uniffi.feedhub_ffi.FfiSyncHistoryRow
import uniffi.feedhub_ffi.FfiSyncSanitizeHost
import uniffi.feedhub_ffi.FfiSyncSnapshot
import uniffi.feedhub_ffi.FfiSyncState
import uniffi.feedhub_ffi.FfiSyncPeer
import uniffi.feedhub_ffi.FfiSyncUnlockOutcome
import uniffi.feedhub_ffi.FfiSyncRecipient
import uniffi.feedhub_ffi.ffiSyncCheckPassword
import uniffi.feedhub_ffi.ffiSyncFileSealToRecipients
import uniffi.feedhub_ffi.ffiSyncFileSealWithPassword
import uniffi.feedhub_ffi.ffiSyncDefaultFileName
import uniffi.feedhub_ffi.ffiSyncEncodeSnapshot
import uniffi.feedhub_ffi.ffiSyncInspect
import uniffi.feedhub_ffi.ffiSyncMinPasswordLength
import uniffi.feedhub_ffi.ffiSyncPlanExportToPaired
import uniffi.feedhub_ffi.ffiSyncPreview
import uniffi.feedhub_ffi.ffiSyncRetentionCutoff
import uniffi.feedhub_ffi.ffiSyncDecodeSnapshot
import uniffi.feedhub_ffi.ffiSyncMerge
import uniffi.feedhub_ffi.ffiSyncSanitize

/**
 * **资料库同步的 Kotlin 门面**：只暴露 [feedhub.core.sync] 的类型，`FfiSync*` 一个都不漏出去。
 *
 * 三件事（和 `RustBili` 那类纯函数门面一样）：
 * 1. 加载 `.so`（[RustNative]），失败时说清楚，**不悄悄退回 Kotlin 实现**；
 * 2. 把 `FfiSyncException` 映射回现有的 `LibrarySyncFormatException`（"文件不对"就是这一类，
 *    调用方的 `catch` 一行不用改）；
 * 3. 在 `Package` 的模型和 `FfiSync*` record 之间搬数据（`item_json` 是透传的 JSON 原文）。
 *
 * **不搬的**（故意留在 Kotlin）：局域网收发（`SyncServer` / `SyncClient` / `PairingServer` /
 * `PairingClient`）、读网卡的 `LanAddresses`、`DeviceKeyStore` 落盘、Cookie 罐的挑 / 写。
 */
object RustSync {
    /** `.so` 能加载吗（不抛）。 */
    fun available(): Boolean = try {
        RustNative.ensureLoaded()
        true
    } catch (_: Throwable) {
        false
    }

    // ---------------------------------------------------------------- 快照编解码

    /** 快照 → JSON 字节（**和 Kotlin 原实现逐字节相同**）。 */
    fun encodeSnapshot(snapshot: LibrarySnapshot): ByteArray = guard {
        ffiSyncEncodeSnapshot(snapshot.toFfi())
    }

    /** JSON 字节 → 快照（magic / 版本 / schema 任何一个不对就整份拒）。 */
    fun decodeSnapshot(bytes: ByteArray): LibrarySnapshot = guard {
        ffiSyncDecodeSnapshot(bytes).toModel()
    }

    // ---------------------------------------------------------------- 清洗 / 合并

    /**
     * 坏条目清洗：`item` / `user` 能不能解成 [feedhub.core.FeedItem] / [feedhub.core.UserRef]
     * 由这个回调里的 `FeedJson` 判（Rust 只查"是不是 JSON 对象 + 字节上限"）。
     */
    fun sanitize(snapshot: LibrarySnapshot): SanitizedResult = guard {
        val result = ffiSyncSanitize(
            snapshot.toFfi(),
            object : FfiSyncSanitizeHost {
                override fun validItem(itemJson: String): Boolean =
                    runCatching { FeedJson.decodeFromString(feedhub.core.FeedItem.serializer(), itemJson) }.isSuccess

                override fun validUser(userJson: String): Boolean =
                    runCatching { FeedJson.decodeFromString(feedhub.core.UserRef.serializer(), userJson) }.isSuccess
            },
        )
        SanitizedResult(result.snapshot.toModel(), result.skipped.toInt())
    }

    /** 合并两边（纯函数，不碰库 / 网络）。 */
    fun merge(local: SyncState, remote: SyncState, retentionCutoffMs: Long? = null): MergeOutcome = guard {
        val outcome = ffiSyncMerge(local.toFfi(), remote.toFfi(), retentionCutoffMs)
        MergeOutcome(
            state = outcome.state.toModel(),
            counts = outcome.counts.toModel(),
            folderAliases = outcome.folderAliases.associate { it.oldUid to it.newUid },
            foldersCreated = outcome.foldersCreated,
            historyClearedByRemote = outcome.historyClearedByRemote,
            clearedRows = outcome.clearedRows,
        )
    }

    /**
     * 加载 `.so` + 把 `FfiSyncException` 翻成 [LibrarySyncFormatException]。
     *
     * 为什么不复用 [RustModel.guard]：那个把错误折成 `SourceException`（平台接口那套），
     * 而同步的用户可见错误一直是 [LibrarySyncFormatException]（`LibrarySync.preview` 里
     * `catch (e: LibrarySyncFormatException)` 直接拿 `message` 给用户看）。
     */
    // ---------------------------------------------------------------- 导出 / 导入的编排

    /** 同步密码的最短长度（Kotlin `LibrarySync.MIN_PASSWORD_LENGTH` 现在读它）。 */
    val minPasswordLength: Int by lazy { guard { ffiSyncMinPasswordLength().toInt() } }

    /** 密码够不够长；不够就抛出和原来一样的那句话。 */
    fun checkPassword(passwordLen: Int) = guard { ffiSyncCheckPassword(passwordLen.toUInt()) }

    /**
     * **发给全部已配对设备**的导出计划（原 `LibrarySync.exportToPairedDevices` 的前半）。
     *
     * [selfPublicKey] 是本机设备密钥的公钥（base64url）；[peers] 是已配对设备的 `(deviceId, publicKey)`。
     * 失败时抛的 `IllegalStateException.message` 就是界面上那句话（和 Kotlin 原话一致）。
     */
    fun planExportToPaired(
        selfId: String,
        selfPublicKey: String,
        peers: List<Pair<String, String>>,
    ): PairedExportPlan = guard {
        val ffi = ffiSyncPlanExportToPaired(
            selfId,
            selfPublicKey,
            peers.map { (id, key) -> FfiSyncPeer(id, key) },
        )
        PairedExportPlan(
            // 公钥过边界是字节，交回去时编成 base64url（和 `SyncPeer.publicKey` 同一种写法）
            recipients = ffi.recipients.map { it.device to SyncCrypto.base64Url(it.publicKey) },
            skippedPeers = ffi.skippedPeers.toInt(),
        )
    }

    /** 只看文件头（原 `LibrarySync.inspect`）。 */
    fun inspect(bytes: ByteArray, selfId: String?): Inspection = guard {
        val ffi = ffiSyncInspect(bytes, selfId)
        Inspection(
            mode = ffi.mode,
            recipients = ffi.recipients,
            forThisDevice = ffi.forThisDevice,
            error = ffi.error,
        )
    }

    /**
     * 解开一份同步文件（原 `LibrarySync.preview` 的判定那半边）。
     *
     * [devicePrivateKey] 是本机设备私钥的 32 字节（宿主从 `DeviceKeyStore` 读）；[password] 只在密码文件上用。
     */
    fun preview(
        bytes: ByteArray,
        selfId: String?,
        devicePrivateKey: ByteArray?,
        password: String?,
    ): UnlockOutcome = guard {
        when (val ffi = ffiSyncPreview(bytes, selfId, devicePrivateKey, password)) {
            is FfiSyncUnlockOutcome.Opened -> UnlockOutcome.Opened(
                snapshot = ffi.snapshot.toModel(),
                byDeviceKey = ffi.unlock == "device",
            )
            FfiSyncUnlockOutcome.NeedsPassword -> UnlockOutcome.NeedsPassword
            is FfiSyncUnlockOutcome.Failed -> UnlockOutcome.Failed(ffi.message)
        }
    }

    /** 导出的默认文件名（[year]/[month]/[day]/[hour]/[minute] 是**本地时间**，由宿主算好）。 */
    fun defaultFileName(year: Int, month: Int, day: Int, hour: Int, minute: Int): String = guard {
        ffiSyncDefaultFileName(year, month.toUInt(), day.toUInt(), hour.toUInt(), minute.toUInt())
    }

    /** 本机"只保留最近 N 天"的截止时间；没开就是 null。 */
    fun retentionCutoff(historyLimitOn: Boolean, historyDays: Int, nowMs: Long): Long? = guard {
        ffiSyncRetentionCutoff(historyLimitOn, historyDays.toLong(), nowMs)
    }

    /** 一次"发给已配对设备"的导出计划。 */
    class PairedExportPlan(
        /** 收件人 `(deviceId, publicKey 的 base64url)`，**本机在最前面**。 */
        val recipients: List<Pair<String, String>>,
        /** 公钥读不出来、被跳过的已配对设备（正常是 0）。 */
        val skippedPeers: Int,
    )

    /** 只看文件头的结论（原 `ImportInspection`）。 */
    class Inspection(
        /** `"recipients"` / `"password"`（读不出来时给 `"password"` 兜底）。 */
        val mode: String,
        val recipients: List<String>,
        val forThisDevice: Boolean,
        val error: String?,
    )

    /** 解开文件的结果。 */
    sealed interface UnlockOutcome {
        class Opened(val snapshot: LibrarySnapshot, val byDeviceKey: Boolean) : UnlockOutcome
        data object NeedsPassword : UnlockOutcome
        class Failed(val message: String) : UnlockOutcome
    }

    // ---------------------------------------------------------------- 同步文件加密（`LibrarySync` 导出用）

    /** 给配对设备加密（`ffi_sync_file_seal_to_recipients`）；随机量由宿主给，收件人是 (设备 id, 公钥)。 */
    fun sealToRecipients(
        plaintext: ByteArray,
        device: String,
        generatedAt: Long,
        seq: Long,
        recipients: List<Pair<String, ByteArray>>,
        fileKey: ByteArray,
        ephemeralSecret: ByteArray,
        nonce: ByteArray,
        wrapNonces: List<ByteArray>,
    ): ByteArray = guard {
        ffiSyncFileSealToRecipients(
            plaintext = plaintext,
            device = device,
            generatedAt = generatedAt,
            seq = seq,
            recipients = recipients.map { (id, key) -> FfiSyncRecipient(id, key) },
            fileKey = fileKey,
            ephemeralSecret = ephemeralSecret,
            nonce = nonce,
            wrapNonces = wrapNonces,
        )
    }

    /** 用密码加密（`ffi_sync_file_seal_with_password`，PBKDF2 + AES-GCM）。 */
    fun sealWithPassword(
        plaintext: ByteArray,
        password: ByteArray,
        device: String,
        generatedAt: Long,
        seq: Long,
        iterations: Int,
        salt: ByteArray,
        nonce: ByteArray,
    ): ByteArray = guard {
        ffiSyncFileSealWithPassword(
            plaintext = plaintext,
            password = password,
            device = device,
            generatedAt = generatedAt,
            seq = seq,
            iterations = iterations.toUInt(),
            salt = salt,
            nonce = nonce,
        )
    }

    internal inline fun <T> guard(block: () -> T): T {
        try {
            RustNative.ensureLoaded()
            return block()
        } catch (e: FfiSyncException) {
            throw when (e) {
                is FfiSyncException.Format -> LibrarySyncFormatException(e.detail)
                is FfiSyncException.Internal -> LibrarySyncFormatException("同步的内部错误：${e.detail}")
            }
        } catch (e: UnsatisfiedLinkError) {
            throw LibrarySyncFormatException("Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}")
        } catch (e: IllegalStateException) {
            throw LibrarySyncFormatException("Rust 核心不可用：${e.message}")
        }
    }

    // ---------------------------------------------------------------- 模型 ↔ record

}

/** [RustSync.sanitize] 的结果：干净的快照 + 跳过了多少条。 */
data class SanitizedResult(val snapshot: LibrarySnapshot, val skipped: Int)

// ===========================================================================
// Package 模型 → 过边界
// ===========================================================================

internal fun LibrarySnapshot.toFfi(): FfiSyncSnapshot = FfiSyncSnapshot(
    magic = magic,
    v = v.toLong(),
    schema = schema.toLong(),
    device = device,
    seq = seq,
    generatedAt = generatedAt,
    historyClearedHlc = historyClearedHlc,
    folders = folders.map { it.toFfi() },
    favorites = favorites.map { it.toFfi() },
    history = history.map { it.toFfi() },
    blocked = blocked.map { it.toFfi() },
)

internal fun SyncState.toFfi(): FfiSyncState = FfiSyncState(
    historyClearedHlc = historyClearedHlc,
    folders = folders.map { it.toFfi() },
    favorites = favorites.map { it.toFfi() },
    history = history.map { it.toFfi() },
    blocked = blocked.map { it.toFfi() },
)

internal fun SyncFolderRow.toFfi(): FfiSyncFolderRow = FfiSyncFolderRow(
    uid = uid,
    name = name,
    createdAt = createdAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
)

internal fun SyncFavoriteRow.toFfi(): FfiSyncFavoriteRow = FfiSyncFavoriteRow(
    key = key,
    folderUid = folderUid,
    addedAt = addedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
    itemJson = item?.let { FeedJson.encodeToString(kotlinx.serialization.json.JsonElement.serializer(), it) },
)

internal fun SyncHistoryRow.toFfi(): FfiSyncHistoryRow = FfiSyncHistoryRow(
    key = key,
    visitedAt = visitedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
    progressMs = progressMs,
    durationMs = durationMs,
    progressHlc = progressHlc,
    progressDev = progressDev,
    itemJson = item?.let { FeedJson.encodeToString(kotlinx.serialization.json.JsonElement.serializer(), it) },
)

internal fun SyncBlockedRow.toFfi(): FfiSyncBlockedRow = FfiSyncBlockedRow(
    key = key,
    userJson = user?.let { FeedJson.encodeToString(kotlinx.serialization.json.JsonElement.serializer(), it) },
    blockedAt = blockedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
)

// ===========================================================================
// 过边界 → Package 模型
// ===========================================================================

internal fun FfiSyncSnapshot.toModel(): LibrarySnapshot = LibrarySnapshot(
    magic = magic,
    v = v.toInt(),
    schema = schema.toInt(),
    device = device,
    seq = seq,
    generatedAt = generatedAt,
    historyClearedHlc = historyClearedHlc,
    folders = folders.map { it.toModel() },
    favorites = favorites.map { it.toModel() },
    history = history.map { it.toModel() },
    blocked = blocked.map { it.toModel() },
)

internal fun FfiSyncState.toModel(): SyncState = SyncState(
    historyClearedHlc = historyClearedHlc,
    folders = folders.map { it.toModel() },
    favorites = favorites.map { it.toModel() },
    history = history.map { it.toModel() },
    blocked = blocked.map { it.toModel() },
)

internal fun FfiSyncFolderRow.toModel(): SyncFolderRow = SyncFolderRow(
    uid = uid,
    name = name,
    createdAt = createdAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
)

internal fun FfiSyncFavoriteRow.toModel(): SyncFavoriteRow = SyncFavoriteRow(
    key = key,
    folderUid = folderUid,
    addedAt = addedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
    item = itemJson?.let { parseJsonElement(it) },
)

internal fun FfiSyncHistoryRow.toModel(): SyncHistoryRow = SyncHistoryRow(
    key = key,
    visitedAt = visitedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
    progressMs = progressMs,
    durationMs = durationMs,
    progressHlc = progressHlc,
    progressDev = progressDev,
    item = itemJson?.let { parseJsonElement(it) },
)

internal fun FfiSyncBlockedRow.toModel(): SyncBlockedRow = SyncBlockedRow(
    key = key,
    user = userJson?.let { parseJsonElement(it) },
    blockedAt = blockedAt,
    revHlc = revHlc,
    revDev = revDev,
    deleted = deleted,
)

internal fun FfiSyncCounts.toModel(): SyncCounts = SyncCounts(
    favoritesAdded = favoritesAdded,
    favoritesUpdated = favoritesUpdated,
    favoritesDeleted = favoritesDeleted,
    foldersAdded = foldersAdded,
    foldersUpdated = foldersUpdated,
    foldersDeleted = foldersDeleted,
    historyAdded = historyAdded,
    historyUpdated = historyUpdated,
    historyDeleted = historyDeleted,
    blockedAdded = blockedAdded,
    blockedUpdated = blockedUpdated,
    blockedDeleted = blockedDeleted,
)

/** 解不开的 `item_json` 一律当 `null`（和 `RustLibrary` 里的 `runCatching` 一个口径）。 */
private fun parseJsonElement(text: String): kotlinx.serialization.json.JsonElement? =
    runCatching { FeedJson.parseToJsonElement(text) }.getOrNull()
