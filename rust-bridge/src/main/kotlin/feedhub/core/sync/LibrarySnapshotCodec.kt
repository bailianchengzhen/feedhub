package feedhub.core.sync

import feedhub.rust.RustSync

/** 文件 / 报文格式不对（不是同步文件、版本不认、太大……）。消息是给用户看的，不带条目内容。 */
class LibrarySyncFormatException(message: String) : Exception(message)

/**
 * 快照的 JSON 编解码（加密之前的明文）。**实现在 Rust**（`feedhub-core/src/sync/snapshot.rs`），
 * 差一个转义就互不认），所以编解码只能有一份实现。
 *
 * 校验**宁严勿松**（和登录同步的二维码解析一个原则）：magic / 协议版本 / 资料库版本任何一个不对就整份拒；
 * 未知字段忽略（以后加字段不破坏旧程序）。尺寸上限见 [LIBRARY_SYNC_MAX_ENTRIES] / [LIBRARY_SYNC_MAX_ITEM_BYTES]。
 *
 * Kotlin 的原实现在 `core` 的 testFixtures（`feedhub.core.sync.legacy.LegacyLibrarySnapshotCodec`），
 * 只当对拍参照（`SyncCodecDualRunTest`）。
 */
object LibrarySnapshotCodec {
    fun encode(snapshot: LibrarySnapshot): ByteArray = RustSync.encodeSnapshot(snapshot)

    fun decode(bytes: ByteArray): LibrarySnapshot = RustSync.decodeSnapshot(bytes)

    /**
     * magic / 协议版本 / 资料库版本的检查。
     *
     * 这一条**没有**过 Rust：它是三个 `if`，而且调用方（`RustLibrary.applyImport`）要在**不解码整份**
     * 的情况下先挡一次坏文件。两边的消息逐字相同（`SyncCodecDualRunTest` 里对拍）。
     */
    fun checkHeader(magic: String, v: Int, schema: Int) {
        if (magic != LIBRARY_SYNC_MAGIC) throw LibrarySyncFormatException("不是 feedhub 同步文件")
        if (v != LIBRARY_SYNC_PROTOCOL) {
            throw LibrarySyncFormatException("同步文件的协议版本是 $v，这个程序只认 $LIBRARY_SYNC_PROTOCOL（请升级 App）")
        }
        if (schema != LIBRARY_SYNC_SCHEMA) {
            throw LibrarySyncFormatException("同步文件来自资料库版本 $schema，这个程序只认 $LIBRARY_SYNC_SCHEMA")
        }
    }
}

/**
 * 把远端快照里"塞不进去 / 解不开"的条目剔掉，**只影响这一条**，不让整份快照失败：
 * `item` 解不成 `FeedItem`（或超过 [LIBRARY_SYNC_MAX_ITEM_BYTES]）的收藏 / 历史、`user` 解不开的屏蔽、
 * uid / key 为空的记录。剔掉多少条记在 [SanitizedSnapshot.skipped]。
 *
 * **实现在 Rust**；"能不能解成 `FeedItem` / `UserRef`"那一步由 Rust 回调 Kotlin 的 `FeedJson` 判
 * （Rust 里没有派生解码器，也不该为了这个把模型抄一份）。
 *
 * 正常导出的文件不会有这些，这是挡"云盘里被塞了坏文件"。
 */
object LibrarySnapshotSanitizer {
    fun sanitize(snapshot: LibrarySnapshot): SanitizedSnapshot {
        val result = RustSync.sanitize(snapshot)
        return SanitizedSnapshot(result.snapshot, result.skipped)
    }
}

/** [LibrarySnapshotSanitizer] 的结果。 */
data class SanitizedSnapshot(val snapshot: LibrarySnapshot, val skipped: Int)
