package feedhub.core.sync

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import java.security.SecureRandom


/**
 * 收藏 / 历史跨设备同步的报文模型。
 *
 *   比较时先比 HLC 再比设备 id，保证两台设备合并结果确定且一致（交换律 / 幂等）；
 * - 删除不删行，写 `deleted = true` 的墓碑（清空历史例外：用全局 [LibrarySnapshot.historyClearedHlc]）；
 * - 收藏夹用跨设备稳定的 `uid`（v4 迁移时新建的 UUID），本地整数 id 只做本机外键；
 * - `item` 直接放现有 `item_json` 解析出来的 JSON，不裁剪、不重新映射，接收端不联网也能显示卡片。
 *
 * 不认识就整份拒（宁严勿松，和登录同步的 [feedhub.core.sync.SyncOffer] 一致）。
 */
const val LIBRARY_SYNC_MAGIC = "feedhub-sync"

/** 文件 / 报文的协议版本。改了字段语义就 +1，旧程序会整份拒绝。 */
const val LIBRARY_SYNC_PROTOCOL = 1

/** 本地库表结构的版本（和 `SqliteLibrary.SCHEMA_VERSION` 一致）。 */
const val LIBRARY_SYNC_SCHEMA = 4

/** 一份快照里最多多少条记录（收藏 + 历史 + 收藏夹 + 屏蔽）。挡坏文件用，正常数据远小于它。 */
const val LIBRARY_SYNC_MAX_ENTRIES = 50_000

/** 单条 `item` 的 JSON 上限（字节）；超出的这一条跳过并计数，不让整份快照失败。 */
const val LIBRARY_SYNC_MAX_ITEM_BYTES = 64 * 1024

@Serializable
data class LibrarySnapshot(
    val magic: String = LIBRARY_SYNC_MAGIC,
    val v: Int = LIBRARY_SYNC_PROTOCOL,
    val schema: Int = LIBRARY_SYNC_SCHEMA,
    /** 生成这份快照的设备 id（8 字节 base64url）。 */
    val device: String = "",
    /** 这台设备单调递增的快照序号（防"把网盘里的旧文件放回来"）。 */
    val seq: Long = 0,
    val generatedAt: Long = 0,
    /** "清空历史"的全局标记（HLC）。0 = 从来没清空过。 */
    val historyClearedHlc: Long = 0,
    val folders: List<SyncFolderRow> = emptyList(),
    val favorites: List<SyncFavoriteRow> = emptyList(),
    val history: List<SyncHistoryRow> = emptyList(),
    val blocked: List<SyncBlockedRow> = emptyList(),
) {
    val entryCount: Int get() = folders.size + favorites.size + history.size + blocked.size

    /** 只取可以合并的那部分（不含 magic / v / schema / seq 这些报文元数据）。 */
    fun toState(): SyncState = SyncState(
        historyClearedHlc = historyClearedHlc,
        folders = folders,
        favorites = favorites,
        history = history,
        blocked = blocked,
    )
}

/**
 * 合并用的四张表（含墓碑）。和 [LibrarySnapshot] 的区别：没有报文元数据，纯数据。
 * 合并引擎只认它，所以可以脱离文件 / 数据库单测（`LibraryMergeTest`）。
 */
data class SyncState(
    val historyClearedHlc: Long = 0,
    val folders: List<SyncFolderRow> = emptyList(),
    val favorites: List<SyncFavoriteRow> = emptyList(),
    val history: List<SyncHistoryRow> = emptyList(),
    val blocked: List<SyncBlockedRow> = emptyList(),
)

/**
 * 一个收藏夹。[uid] 跨设备稳定（v4 迁移时新建的 UUID 字符串），本地整数 id 不进快照。
 * 删除的收藏夹留一条 `deleted = true` 的墓碑，名字还在（同名认领时用得到）。
 */
@Serializable
data class SyncFolderRow(
    val uid: String,
    val name: String,
    val createdAt: Long,
    val revHlc: Long,
    val revDev: String,
    val deleted: Boolean = false,
)

/** 一条收藏。`folderUid` 为 null = 默认收藏夹。 */
@Serializable
data class SyncFavoriteRow(
    val key: String,
    val folderUid: String? = null,
    val addedAt: Long,
    val revHlc: Long,
    val revDev: String,
    val deleted: Boolean = false,
    val item: JsonElement? = null,
)

/**
 * 一条历史。访问时间和播放进度**分开定序**（[revHlc] / [progressHlc]）：
 * 打开一次详情不会碰进度，进度每秒几次地写，混在一个时间戳里会互相覆盖。
 */
@Serializable
data class SyncHistoryRow(
    val key: String,
    val visitedAt: Long,
    val revHlc: Long,
    val revDev: String,
    val deleted: Boolean = false,
    val progressMs: Long? = null,
    val durationMs: Long? = null,
    val progressHlc: Long = 0,
    val progressDev: String = "",
    val item: JsonElement? = null,
)

/** 一个屏蔽的用户。 */
@Serializable
data class SyncBlockedRow(
    val key: String,
    val user: JsonElement? = null,
    val blockedAt: Long,
    val revHlc: Long,
    val revDev: String,
    val deleted: Boolean = false,
)

/**
 * 合并结果里各类记录改了多少条（相对**本机**）。
 *
 * `added` = 本机没有（或本机是墓碑）而合并后活着；`deleted` = 本机活着而合并后是墓碑 / 没了；
 * `updated` = 两边都活着但内容不同（历史的进度变化也算）。
 */
data class SyncCounts(
    val favoritesAdded: Int = 0,
    val favoritesUpdated: Int = 0,
    val favoritesDeleted: Int = 0,
    val foldersAdded: Int = 0,
    val foldersUpdated: Int = 0,
    val foldersDeleted: Int = 0,
    val historyAdded: Int = 0,
    val historyUpdated: Int = 0,
    val historyDeleted: Int = 0,
    val blockedAdded: Int = 0,
    val blockedUpdated: Int = 0,
    val blockedDeleted: Int = 0,
) {
    val added: Int get() = favoritesAdded + foldersAdded + historyAdded + blockedAdded
    val updated: Int get() = favoritesUpdated + foldersUpdated + historyUpdated + blockedUpdated
    val deleted: Int get() = favoritesDeleted + foldersDeleted + historyDeleted + blockedDeleted
    val total: Int get() = added + updated + deleted
}

/**
 * [LibraryMerge.merge] 的结果。
 *
 * @param state 合并后的完整状态（含墓碑；被清空标记去掉的历史行已经不在里面）
 * @param counts 相对本机的增 / 改 / 删条数
 * @param folderAliases 本机收藏夹 uid → 合并后的 uid（同名认领时才会出现；正常情况下是空的）
 * @param foldersCreated 合并后在**本机**新建的收藏夹数
 * @param historyClearedByRemote 远端"清空历史"标记比本机大（本机要按它清一次）
 * @param clearedRows 被清空标记去掉的本机历史行数
 */
data class MergeOutcome(
    val state: SyncState,
    val counts: SyncCounts,
    val folderAliases: Map<String, String> = emptyMap(),
    val foldersCreated: Int = 0,
    val historyClearedByRemote: Boolean = false,
    val clearedRows: Int = 0,
)

/**
 * 混合逻辑时钟（HLC）的编解码和比较：`物理毫秒 << 16 | 逻辑计数`（计数 0..65535）。
 * 同一毫秒里的两次写靠计数分先后；收到远端更大的 HLC 时把本机抬到它之后（见 [HlcClock.observe]）。
 *
 * 比较两个"版本"要连设备 id 一起比（[compareVersion]）：HLC 相同（两台恰好同一毫秒、计数也相同）时，
 * 按设备 id 字典序定胜负，这样交换律才成立。
 *
 * **数值运算在 Rust**（`feedhub-core/src/sync/crypto.rs` 的 `hlc`）：平局规则是合并正确性的一部分，
 * 两份实现漂一点点两台设备就合不出一样的结果。
 */
object Hlc {
    const val COUNTER_BITS = 16
    const val MAX_COUNTER = 0xFFFF

    fun of(physicalMs: Long, counter: Int = 0): Long = SyncHlcNative.of(physicalMs, counter)

    fun physical(hlc: Long): Long = SyncHlcNative.physical(hlc)

    fun counter(hlc: Long): Int = SyncHlcNative.counter(hlc)

    /** 先比 HLC（无符号：HLC 永远是非负的），再比设备 id。返回值同 [Comparable.compareTo]。 */
    fun compareVersion(hlcA: Long, devA: String, hlcB: Long, devB: String): Int =
        SyncHlcNative.compareVersion(hlcA, devA, hlcB, devB)

    /** 取两个版本里较大的那一个的 HLC。平局时返回 [hlcA]（设备 id 相同才能平局）。 */
    fun maxVersion(hlcA: Long, devA: String, hlcB: Long, devB: String): Long =
        SyncHlcNative.maxVersion(hlcA, devA, hlcB, devB)
}

/**
 * 本机的 HLC 发生器：只保证**本机单调**，`now = max(系统时钟, 上次的毫秒 + 1)`。
 * 持久化的最后值在 [last]；[observe] 在合并远端快照后调用，保证"我看到了你的修改再改"一定排在后面。
 *
 * **留在 Kotlin**：它要读系统时钟、要跨调用保持单调状态（`.so` 里不读时钟、不留状态）。
 */
class HlcClock(private val clock: () -> Long, initialLast: Long = 0) {
    @Volatile
    var last: Long = initialLast
        private set

    @Synchronized
    fun next(): Long {
        val now = clock()
        val lastMs = Hlc.physical(last)
        val lastCounter = Hlc.counter(last)
        val ms = maxOf(now, lastMs)
        val counter = when {
            ms > lastMs -> 0
            lastCounter < Hlc.MAX_COUNTER -> lastCounter + 1
            // 一毫秒里改了 65536 次（不可能），把物理时间往后推一毫秒
            else -> {
                last = Hlc.of(lastMs + 1, 0)
                return last
            }
        }
        last = Hlc.of(ms, counter)
        return last
    }

    @Synchronized
    fun observe(other: Long) {
        if (other > last) last = other
    }
}

/**
 * 8 字节随机设备 id（base64url，无填充）。**不是**硬件指纹，重装 App 会变。
 *
 * 随机量由这一层取（[SecureRandom]），编码 / 解码用 Rust 的 base64url（口径同 `java.util.Base64`）。
 */
object SyncDevice {
    const val BYTES = 8

    fun random(random: SecureRandom = SecureRandom()): String {
        val bytes = ByteArray(BYTES).also(random::nextBytes)
        return encode(bytes)
    }

    fun encode(bytes: ByteArray): String = SyncCrypto.base64Url(bytes)

    fun decode(text: String): ByteArray? = SyncCrypto.base64UrlDecode(text)?.takeIf { it.size == BYTES }
}
