package feedhub.app.library

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.ContentType
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.core.UserRef
import feedhub.core.sync.HlcClock
import feedhub.core.sync.LibraryMerge
import feedhub.core.sync.LibrarySnapshot
import feedhub.core.sync.LibrarySnapshotCodec
import feedhub.core.sync.LibrarySnapshotSanitizer
import feedhub.core.sync.SyncBlockedRow
import feedhub.core.sync.SyncCounts
import feedhub.core.sync.SyncDevice
import feedhub.core.sync.SyncFavoriteRow
import feedhub.core.sync.SyncFolderRow
import feedhub.core.sync.SyncHistoryRow
import feedhub.core.sync.SyncState
import feedhub.rust.RustLibraryBridge
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import uniffi.feedhub_ffi.FfiLibContentType
import uniffi.feedhub_ffi.FfiLibEmbedding
import uniffi.feedhub_ffi.FfiLibException
import uniffi.feedhub_ffi.FfiLibFolderAlias
import uniffi.feedhub_ffi.FfiLibFolderFilter
import uniffi.feedhub_ffi.FfiLibItemColumns
import uniffi.feedhub_ffi.FfiLibMergeHost
import uniffi.feedhub_ffi.FfiLibMergeOutcome
import uniffi.feedhub_ffi.FfiLibRemoteMeta
import uniffi.feedhub_ffi.FfiLibSyncBlockedRow
import uniffi.feedhub_ffi.FfiLibSyncFavoriteRow
import uniffi.feedhub_ffi.FfiLibSyncFolderRow
import uniffi.feedhub_ffi.FfiLibSyncHistoryRow
import uniffi.feedhub_ffi.FfiLibSyncState
import uniffi.feedhub_ffi.FfiLibrary
import java.nio.file.Files
import java.nio.file.Path
import java.security.SecureRandom
import java.util.UUID
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * [Library] 的 **Rust 实现**：`feedhub-core` 的 `library` 模块 + rusqlite，
 * SQL / 事务 / 迁移全在 Rust 里；这里只是门面。
 *
 * **和 [SqliteLibrary] 的关系**：同一套表结构、同一个库文件（`PRAGMA user_version` 还是 9；v9 =
 * `item_embedding` 主键改成 `(key, model)`，和封面向量那条"并存、不替换"的口径一致），
 * 用户已有的库原地打开就能用、不迁移不丢数据。[SqliteLibrary] 从此只当**对拍参照**
 * （`RustLibraryDualRunTest` / `LibraryContractTest` 两边一起跑）——生产路径走这里。
 *
 * 门面这边保留的东西和以前一模一样（它们是 Compose 能观察到的内存状态，不能搬进 Rust）：
 *
 * - 小而常用的（是否已收藏 / 屏蔽、收藏夹、进度）在 `mutableState*` 里留一份，同步读；
 * - 列表查询（收藏 / 历史 / 曝光 / 向量）是 `suspend`，在 `Dispatchers.IO` 上调 Rust（Rust 侧一次调用
 *   换一整页，不逐字段过 JNI）；
 * - 写先改内存再写库，**库写失败只打日志**（磁盘问题不该让界面崩），内存里那份本次运行照常用。
 *
 * 时间（`stamp()`）和 HLC 都在 Kotlin 算好再传过去：`.so` 里不读时钟、不取随机数（新建收藏夹的 uid、
 * 迁移用的设备 id / 随机种子也都由这里生成），所以两份库在同一串操作下逐字节相同。
 *
 * 线程模型见 `rust/crates/feedhub-core/src/library/mod.rs` 的文件头：Rust 里两个连接（写 / 读）各一把锁，
 * WAL 模式，读不挡写；`.so` 里没有后台线程、没有定时器。
 */
class RustLibrary private constructor(
    private val native: FfiLibrary,
    private val clock: () -> Long,
    /**
     * 新建收藏夹时用的跨设备 uid。生产就是 `UUID.randomUUID()`；**对拍**（`RustLibraryDualRunTest`）
     * 传一个确定性的生成器，两份库才能逐字节比 `.dump`（uid 是唯一一处不来自调用方的随机量）。
     */
    private val newFolderUid: () -> String,
) : Library, AutoCloseable {
    // 内存里的一份：收藏的 key → 收藏夹 + 是否被平台标成 18+，收藏夹，屏蔽的用户，最近存过的播放进度
    private val favFolder = mutableStateMapOf<String, Fav>()

    /** 收藏在内存里的一行：收藏夹 id（null = 默认收藏夹）+ [FeedItem.adult]（读取时据此整条隐藏）。 */
    private class Fav(val folder: String?, val adult: Boolean)

    private val blocked = mutableStateMapOf<String, Pair<UserRef, Long>>()
    private val progressCache = mutableStateMapOf<String, Long>()

    /** 收藏夹本地 id → 跨设备 uid（v4 起每本夹都有一个，导出 / 导入时用）。 */
    private val folderUidById = mutableMapOf<String, String>()

    /** 本机设备标识（8 字节 base64url，存在 `sync_meta` 里）。 */
    private val deviceId: String

    /** 本机的混合逻辑时钟（每次写一条同步记录前进一次）。 */
    private val hlc: HlcClock

    /**
     * 单写线程队列：所有 SQLite 写入与卡片 JSON 编码都串行落在这条后台线程上，
     * 主线程只更新内存里的 Compose 状态并投递任务，绝不在主线程等待 SQLite 写锁或 `busy_timeout`。
     * 空闲时阻塞在 `LinkedBlockingQueue` 的条件变量上，零唤醒。
     */
    @Volatile
    private var writerThread: Thread? = null
    private val writeQueue = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "feedhub-lib-writer").apply {
            isDaemon = true
            writerThread = this
        }
    }
    private val enqueuedSeq = AtomicLong(0L)
    private val completedSeq = AtomicLong(0L)

    override var folders: List<FavoriteFolder> by mutableStateOf(emptyList())
        private set
    override var favoritesRevision by mutableIntStateOf(0)
        private set
    override var historyRevision by mutableIntStateOf(0)
        private set

    init {
        deviceId = native.deviceId()
        hlc = HlcClock(clock, native.lastHlc())
        loadState()
    }

    companion object {
        /**
         * 打开（不存在就建）[file] 的资料库。**打不开会抛**（库是更新版本的程序建的、磁盘坏、`.so` 加载失败），
         * 由调用方决定退回 [MemoryLibrary]——和以前 `SqliteLibrary.open` 的语义一样。
         */
        fun open(
            file: Path,
            clock: () -> Long = System::currentTimeMillis,
            newFolderUid: () -> String = { UUID.randomUUID().toString() },
        ): RustLibrary {
            file.parent?.let { Files.createDirectories(it) }
            RustLibraryBridge.ensureLoaded()
            // 设备 id 只在 fresh 库里用得上（v4 迁移）；随机种子给迁移 v4 补收藏夹 uid 用。
            // 两个都由这里生成（Rust 里不取随机数），和以前 `SyncDevice.random()` / `UUID.randomUUID()` 同一种取值。
            val deviceHint = SyncDevice.random()
            val seed = ByteArray(32).also { SecureRandom().nextBytes(it) }
            val native = try {
                FfiLibrary(file.toString(), deviceHint, seed, MergeBridge())
            } catch (e: FfiLibException.Unsupported) {
                // 和 SqliteLibrary 一字不差：App 的 openLibrary 抓 Throwable 退回内存实现
                throw IllegalStateException(e.detail, e)
            } catch (e: FfiLibException) {
                throw IllegalStateException(e.message ?: e.toString(), e)
            }
            return RustLibrary(native, clock, newFolderUid)
        }
    }

    // ---------------------------------------------------------------- 内存镜像

    private fun loadState() {
        // 重新载入时先清掉内存里那份（导入 / 迁移之后会再调一次）
        favFolder.clear()
        blocked.clear()
        folderUidById.clear()
        val foldersNow = guarded("读收藏夹", emptyList()) { native.folders() }.map { row ->
            row.uid?.takeIf { it.isNotEmpty() }?.let { folderUidById[row.id.toString()] = it }
            FavoriteFolder(row.id.toString(), row.name)
        }
        folders = foldersNow
        for (row in guarded("读收藏 key", emptyList()) { native.favoriteRefs() }) {
            favFolder[row.key] = Fav(row.folderId?.toString(), row.adult)
        }
        for (row in guarded("读屏蔽", emptyList()) { native.blockedRows() }) {
            val user = jsonOf(row.userJson)?.let { runCatching { FeedJson.decodeFromJsonElement(UserRef.serializer(), it) }.getOrNull() }
            if (user != null) blocked[user.blockKey()] = user to row.blockedAt
        }
    }

    // ---------------------------------------------------------------- 收藏

    override fun isFavorite(key: String) = key in favFolder

    override fun addFavorite(item: FeedItem, folder: String?) {
        val f = folder?.takeIf { id -> folders.any { it.id == id } }
        val now = stamp()
        val rev = hlc.next()
        favFolder[item.key] = Fav(f, item.adult)
        favoritesRevision++
        enqueueWrite("收藏") {
            native.addFavorite(
                key = item.key,
                folderId = f?.toLongOrNull(),
                platform = item.platform.key,
                isVideo = item.isVideoLike(),
                searchText = item.searchText(),
                itemJson = encode(item),
                addedAt = now,
                adult = item.adult,
                rev = rev,
            )
        }
    }

    override fun removeFavorite(key: String) {
        favFolder.remove(key)
        favoritesRevision++
        val rev = hlc.next()
        enqueueWrite("取消收藏") { native.removeFavorite(key, rev) }
    }

    override fun createFolder(name: String): FavoriteFolder {
        require(name.isNotBlank()) { "收藏夹名字不能为空" }
        val clean = name.trim()
        val uid = newFolderUid()
        val now = stamp()
        val rev = hlc.next()
        val id = submitWrite("新建收藏夹", fallback = null) { native.createFolder(clean, uid, now, rev) }
        // 库写失败时用一个只在内存里的临时 id，免得界面报错
        val folder = FavoriteFolder((id ?: -(folders.size + 1).toLong()).toString(), clean)
        if (id != null) folderUidById[folder.id] = uid
        folders = folders + folder
        favoritesRevision++
        return folder
    }

    override fun renameFolder(id: String, name: String) {
        require(name.isNotBlank()) { "收藏夹名字不能为空" }
        val clean = name.trim()
        val rev = hlc.next()
        folders = folders.map { if (it.id == id) it.copy(name = clean) else it }
        favoritesRevision++
        id.toLongOrNull()?.let { fid ->
            enqueueWrite("改收藏夹名字") { native.renameFolder(fid, clean, rev) }
        }
    }

    override fun deleteFolder(id: String, deleteItems: Boolean) {
        val rev = hlc.next()
        val fid = id.toLongOrNull()
        for ((key, fav) in favFolder.entries.toList()) {
            if (fav.folder != id) continue
            if (deleteItems) favFolder.remove(key) else favFolder[key] = Fav(null, fav.adult)
        }
        folders = folders.filter { it.id != id }
        folderUidById.remove(id)
        favoritesRevision++
        enqueueWrite("删收藏夹") { native.deleteFolder(fid, deleteItems, rev) }
    }

    /** 收藏数（某个收藏夹或全部）。**不含**被平台标成 18+ 的（它们整条隐藏，见 [Library] 的注释）。 */
    override fun favoriteCount(folder: FolderFilter): Int {
        val shown = favFolder.values.filterNot { it.adult }
        return when (folder) {
            FolderFilter.All -> shown.size
            FolderFilter.Default -> shown.count { it.folder == null }
            is FolderFilter.Folder -> shown.count { it.folder == folder.id }
        }
    }

    override suspend fun favorites(query: FavoriteQuery): List<Favorite> = withContext(Dispatchers.IO) {
        flush()
        guarded("读收藏", fallback = emptyList()) {
            val folder = when (val f = query.folder) {
                FolderFilter.All -> FfiLibFolderFilter.All
                FolderFilter.Default -> FfiLibFolderFilter.Default
                is FolderFilter.Folder -> FfiLibFolderFilter.Folder(f.id.toLongOrNull() ?: -1L)
            }
            val type = when (query.type) {
                ContentType.MIXED -> FfiLibContentType.MIXED
                ContentType.VIDEO -> FfiLibContentType.VIDEO
                ContentType.TEXT -> FfiLibContentType.TEXT
            }
            native.favorites(folder, query.platform?.key, type, query.search, query.limit.toLong()).mapNotNull { row ->
                val item = decode(row.itemJson) ?: return@mapNotNull null
                Favorite(item, row.folderId?.toString(), row.addedAt)
            }
        }
    }

    // ---------------------------------------------------------------- 历史

    @Synchronized
    override fun recordVisit(item: FeedItem) {
        val now = stamp()
        val rev = hlc.next()
        // 删过又打开一次：进度在库里已经被清掉，内存里那份也不能再用
        progressCache.remove(item.key)
        historyRevision++
        enqueueWrite("记历史") {
            native.recordVisit(
                key = item.key,
                platform = item.platform.key,
                searchText = item.searchText(),
                itemJson = encode(item),
                visitedAt = now,
                adult = item.adult,
                rev = rev,
            )
        }
    }

    override fun saveProgress(key: String, positionMs: Long, durationMs: Long?) {
        progressCache[key] = positionMs
        val rev = hlc.next()
        enqueueWrite("存播放进度") {
            val changed = native.saveProgress(key, positionMs, durationMs, rev)
            // 这条历史不在（或被删成了墓碑）时 SQL 一行都没改，别在内存里留下一个假的进度
            if (changed == 0 && progressCache[key] == positionMs) {
                progressCache.remove(key)
            }
        }
    }

    override fun progress(key: String): Long? = progressCache[key] ?: run {
        flush()
        guarded("读播放进度", fallback = null) { native.progress(key) }
    }

    override fun progressCached(key: String): Long? = progressCache[key]

    override suspend fun history(query: HistoryQuery): List<HistoryEntry> = withContext(Dispatchers.IO) {
        flush()
        guarded("读历史", fallback = emptyList()) {
            native.history(
                platform = query.platform?.key,
                search = query.search,
                limit = query.limit.toLong(),
                offset = query.offset.toLong(),
            ).mapNotNull { row ->
                val item = decode(row.itemJson) ?: return@mapNotNull null
                HistoryEntry(item, row.visitedAt, row.progressMs, row.durationMs)
            }
        }
    }

    /** 历史条数（某个平台 / 搜索词）：**不含**被平台标成 18+ 的（它们整条隐藏，见 [Library] 的注释）。 */
    override suspend fun historyCount(platform: Platform?, search: String): Int = withContext(Dispatchers.IO) {
        flush()
        guarded("数历史", fallback = 0) { native.historyCount(platform?.key, search) }
    }

    override fun removeHistory(key: String) {
        progressCache.remove(key)
        val rev = hlc.next()
        historyRevision++
        enqueueWrite("删历史") { native.removeHistory(key, rev) }
    }

    override fun clearHistory() {
        progressCache.clear()
        val rev = hlc.next()
        historyRevision++
        enqueueWrite("清空历史") { native.clearHistory(rev) }
    }

    override fun pruneHistory(olderThanMs: Long): Int {
        val n = submitWrite("清理旧历史", fallback = 0) { native.pruneHistory(olderThanMs) }
        if (n > 0) {
            progressCache.clear()
            historyRevision++
        }
        return n
    }

    // ---------------------------------------------------------------- 屏蔽

    override fun isBlocked(user: UserRef) = user.blockKey() in blocked

    override fun block(user: UserRef) {
        val now = stamp()
        val rev = hlc.next()
        val key = user.blockKey()
        blocked[key] = user to now
        enqueueWrite("屏蔽") {
            native.block(key, FeedJson.encodeToString(UserRef.serializer(), user), now, rev)
        }
    }

    override fun unblock(user: UserRef) {
        val key = user.blockKey()
        blocked.remove(key)
        val rev = hlc.next()
        enqueueWrite("解除屏蔽") { native.unblock(key, rev) }
    }

    override val blockedUsers: List<UserRef>
        get() = blocked.values.sortedByDescending { it.second }.map { it.first }

    override fun blockedAuthors(): List<BlockedAuthor> =
        blocked.values.sortedByDescending { it.second }.map { BlockedAuthor(it.first, it.second) }

    // ---------------------------------------------------------------- 行为数据（曝光 / 停留）

    override fun recordExposure(
        key: String,
        platform: Platform,
        at: Long,
        dwellMs: Long,
        surface: ExposureSurface,
        lastSeenAt: Long,
        mergeWindowMs: Long,
    ) {
        val pKey = platform.key
        val sKey = surface.key
        enqueueWrite("记曝光") {
            native.recordExposure(key, pKey, at, dwellMs, sKey, lastSeenAt, mergeWindowMs)
        }
    }

    override fun exposureCount(): Int {
        flush()
        return guarded("数曝光", fallback = 0) { native.exposureCount() }
    }

    override suspend fun exposureStats(
        fromMs: Long,
        toMs: Long,
        platform: Platform?,
        surface: ExposureSurface?,
    ): ExposureStats = withContext(Dispatchers.IO) {
        flush()
        guarded("读曝光统计", fallback = ExposureStats(0, 0, 0)) {
            val stats = native.exposureStats(fromMs, toMs, platform?.key, surface?.key)
            ExposureStats(stats.exposures, stats.clicked, stats.dwellMs)
        }
    }

    override suspend fun exposureRows(limit: Int): List<ExposureRow> = withContext(Dispatchers.IO) {
        flush()
        guarded("读曝光行", fallback = emptyList()) {
            native.exposureRows(limit.toLong()).map { row ->
                ExposureRow(
                    row.key,
                    Platform(row.platform),
                    ExposureSurface.fromKey(row.surface),
                    row.exposedAt,
                    row.lastSeenAt,
                    row.dwellMs,
                )
            }
        }
    }

    override fun pruneExposure(olderThanMs: Long): Int =
        submitWrite("清理旧曝光", fallback = 0) { native.pruneExposure(olderThanMs) }

    override fun clearExposure() {
        // "清除行为数据"连**曝光过的卡片内容**和**向量**一起清（和以前一样）
        enqueueWrite("清空曝光") { native.clearExposure() }
    }

    // ---------------------------------------------------------------- 曝光过的卡片内容

    override fun recordSeenItem(item: FeedItem, feedKind: FeedKind, at: Long, batchId: Long, platformRank: Int) {
        if (item.adult) return // 平台标成 18+ 的不写（和"整条隐藏"一致）
        val pKey = item.platform.key
        val fKey = feedKind.key
        enqueueWrite("记曝光内容") {
            native.recordSeenItem(item.key, pKey, encode(item), at, fKey, batchId, platformRank)
        }
    }

    override fun seenItemCount(): Int {
        flush()
        return guarded("数曝光内容", fallback = 0) { native.seenItemCount() }
    }

    override suspend fun seenItems(limit: Int): List<SeenItem> = withContext(Dispatchers.IO) {
        flush()
        guarded("读曝光内容", fallback = emptyList()) {
            native.seenItems(limit.toLong()).mapNotNull { row ->
                val item = decode(row.itemJson) ?: return@mapNotNull null
                SeenItem(item, row.firstSeenAt, FeedKind.fromKey(row.feedKind) ?: FeedKind.RECOMMEND, row.batchId, row.platformRank)
            }
        }
    }

    override fun pruneSeenItems(olderThanMs: Long): Int =
        submitWrite("清理曝光内容", fallback = 0) { native.pruneSeenItems(olderThanMs) }

    // ---------------------------------------------------------------- 卡片向量

    override fun insertEmbeddings(rows: List<ItemEmbedding>): Int {
        if (rows.isEmpty()) return 0
        return submitWrite("写向量", fallback = 0) {
            native.insertEmbeddings(
                rows.map { row ->
                    FfiLibEmbedding(row.key, row.model, row.dim, row.vec, row.createdAt, row.source)
                },
            )
        }
    }

    override fun embeddingKeys(keys: Collection<String>, model: String): Set<String> {
        if (keys.isEmpty()) return emptySet()
        flush()
        // 分批查（和以前一样一次 400 个 key，别撞 SQLite 的参数上限）
        return guarded("查已有向量", fallback = emptySet()) {
            val out = HashSet<String>(keys.size * 2)
            for (chunk in keys.chunked(400)) out += native.embeddingKeys(chunk, model)
            out
        }
    }

    override fun embeddingCount(): Int {
        flush()
        return guarded("数向量", fallback = 0) { native.embeddingCount() }
    }

    override fun embeddingStats(): List<EmbeddingStat> {
        flush()
        return guarded("数向量来源", fallback = emptyList()) {
            native.embeddingStats().map { EmbeddingStat(it.model, it.source, it.count) }
        }
    }

    override suspend fun embeddingBackfill(model: String, sinceMs: Long, limit: Int): List<EmbeddingCandidate> =
        withContext(Dispatchers.IO) {
            if (limit <= 0) return@withContext emptyList()
            flush()
            guarded("查补算候选", fallback = emptyList()) {
                // 先多取一些（同一条可能两边都有，去重后会少），再在内存里截到 limit
                val raw = native.embeddingBackfill(model, sinceMs, limit.toLong()).mapNotNull { row ->
                    val item = decode(row.itemJson) ?: return@mapNotNull null
                    RawCandidate(row.key, item, row.at, row.source)
                }
                val out = LinkedHashMap<String, EmbeddingCandidate>()
                for (r in raw) {
                    val old = out[r.key]
                    if (old == null) {
                        out[r.key] = EmbeddingCandidate(r.item, r.source, r.at)
                    } else {
                        val src = if (old.source == EMBEDDING_SOURCE_FAVORITE || r.source == EMBEDDING_SOURCE_FAVORITE) {
                            EMBEDDING_SOURCE_FAVORITE
                        } else {
                            old.source
                        }
                        out[r.key] = EmbeddingCandidate(old.item, src, maxOf(old.at, r.at))
                    }
                }
                out.values.sortedByDescending { it.at }.take(limit)
            }
        }

    private class RawCandidate(val key: String, val item: FeedItem, val at: Long, val source: String)

    override fun pruneEmbeddings(olderThanMs: Long): Int =
        submitWrite("清理向量", fallback = 0) { native.pruneEmbeddings(olderThanMs) }

    override suspend fun embeddings(limit: Int): List<ItemEmbedding> = withContext(Dispatchers.IO) {
        flush()
        guarded("读向量", fallback = emptyList()) {
            native.embeddings(limit.toLong()).map {
                ItemEmbedding(it.key, it.model, it.dim, it.vec, it.createdAt, it.source)
            }
        }
    }

    override fun clearEmbeddings() {
        enqueueWrite("清空向量") { native.clearEmbeddings() }
    }

    // ---------------------------------------------------------------- 跨设备同步

    override val supportsSync: Boolean get() = true

    override fun syncDeviceId(): String = deviceId

    override fun syncSnapshot(now: Long): LibrarySnapshot {
        flush()
        // 快照序号每次导出 +1：导入端拿它挡"把网盘里的旧文件放回来"
        val seq = submitWrite("生成同步快照", fallback = 0L) { native.bumpSnapshotSeq() }
        val state = readState()
        return LibrarySnapshot(
            device = deviceId,
            seq = seq,
            generatedAt = now,
            historyClearedHlc = state.historyClearedHlc,
            folders = state.folders,
            favorites = state.favorites,
            history = state.history,
            blocked = state.blocked,
        )
    }

    override fun previewImport(remote: LibrarySnapshot, now: Long, retentionCutoffMs: Long?): ImportPreview {
        flush()
        LibrarySnapshotCodec.checkHeader(remote.magic, remote.v, remote.schema)
        val sanitized = LibrarySnapshotSanitizer.sanitize(remote)
        val local = readState()
        val outcome = LibraryMerge.merge(local, sanitized.snapshot.toState(), retentionCutoffMs)
        val lastSeq = lastImportedSeq(remote.device)
        val localFolderUids = local.folders.mapTo(HashSet()) { it.uid }
        return ImportPreview(
            counts = outcome.counts,
            foldersCreated = outcome.foldersCreated,
            foldersClaimed = outcome.folderAliases.count { (oldUid, newUid) ->
                oldUid in localFolderUids || newUid in localFolderUids
            },
            historyClearedByRemote = outcome.historyClearedByRemote,
            clearedRows = outcome.clearedRows,
            skippedItems = sanitized.skipped,
            stale = remote.seq < lastSeq,
            lastImportedSeq = lastSeq,
            sourceDevice = remote.device,
            sourceSeq = remote.seq,
            sourceGeneratedAt = remote.generatedAt,
        )
    }

    override fun applyImport(
        remote: LibrarySnapshot,
        now: Long,
        retentionCutoffMs: Long?,
        ignoreStale: Boolean,
    ): ImportResult {
        flush()
        LibrarySnapshotCodec.checkHeader(remote.magic, remote.v, remote.schema)
        val sanitized = LibrarySnapshotSanitizer.sanitize(remote)
        val lastSeq = lastImportedSeq(remote.device)
        val stale = remote.seq < lastSeq
        if (stale && !ignoreStale) {
            return ImportResult(
                applied = false, stale = true, counts = SyncCounts(),
                foldersCreated = 0, foldersClaimed = 0, clearedRows = 0, skippedItems = sanitized.skipped,
                rejectedReason = "这个文件比上次从同一台设备导入的旧（序号 ${remote.seq} < $lastSeq）",
            )
        }
        // 合并 + 写回在单写线程 + Rust 的**写锁 + 同一个事务**里做完（合并引擎通过 [MergeBridge] 回调回 Kotlin）
        val applied = submitWrite("导入同步文件", fallback = null) {
            native.applyImport(
                remote = sanitized.snapshot.toState().toFfi(),
                remoteMeta = FfiLibRemoteMeta(device = remote.device, seq = remote.seq, generatedAt = remote.generatedAt),
                retentionCutoffMs = retentionCutoffMs,
                hlcLast = hlc.last,
            )
        }
        if (applied == null) {
            return ImportResult(
                applied = false, stale = stale, counts = SyncCounts(),
                foldersCreated = 0, foldersClaimed = 0, clearedRows = 0, skippedItems = sanitized.skipped,
                rejectedReason = "写库失败，这次的导入没有生效",
            )
        }
        hlc.observe(applied.remoteMaxHlc)
        synchronized(this) { loadState() }
        progressCache.clear()
        favoritesRevision++
        historyRevision++
        return ImportResult(
            applied = true,
            stale = stale,
            counts = applied.outcome.counts(),
            foldersCreated = applied.outcome.foldersCreated,
            foldersClaimed = applied.outcome.foldersClaimed,
            clearedRows = applied.outcome.clearedRows,
            skippedItems = sanitized.skipped,
        )
    }

    override fun syncPeers(): List<SyncPeer> {
        flush()
        return guarded("读取已配对设备", emptyList()) {
            native.syncPeers().map { SyncPeer(it.deviceId, it.name, it.publicKey, it.pairedAt) }
        }
    }

    override fun upsertSyncPeer(peer: SyncPeer) {
        enqueueWrite("记录已配对设备") {
            native.upsertSyncPeer(peer.deviceId, peer.name, peer.publicKey, peer.pairedAt)
        }
    }

    override fun removeSyncPeer(deviceId: String): Boolean =
        submitWrite("移除已配对设备", fallback = false) { native.removeSyncPeer(deviceId) }

    // ---------------------------------------------------------------- 内部

    /**
     * 把单写线程队列里的待写任务排空（队列为空时仅两次原子读，零开销）。
     * 供后台查询、同步导出、`onStop` 与 `close()` 保证读写一致性与落盘完整。
     */
    override fun flush() {
        if (Thread.currentThread() === writerThread) return
        if (completedSeq.get() >= enqueuedSeq.get()) return
        runCatching { writeQueue.submit {}.get() }
    }

    private fun enqueueWrite(what: String, block: () -> Unit) {
        if (Thread.currentThread() === writerThread) {
            guarded(what, block)
            return
        }
        val seq = enqueuedSeq.incrementAndGet()
        try {
            writeQueue.execute {
                try {
                    guarded(what, block)
                } finally {
                    completedSeq.set(seq)
                }
            }
        } catch (_: RejectedExecutionException) {
            try {
                guarded(what, block)
            } finally {
                completedSeq.set(seq)
            }
        }
    }

    private fun <T> submitWrite(what: String, fallback: T, block: () -> T): T {
        if (Thread.currentThread() === writerThread) {
            return guarded(what, fallback, block)
        }
        val seq = enqueuedSeq.incrementAndGet()
        return try {
            writeQueue.submit(
                Callable {
                    try {
                        guarded(what, fallback, block)
                    } finally {
                        completedSeq.set(seq)
                    }
                },
            ).get()
        } catch (_: RejectedExecutionException) {
            try {
                guarded(what, fallback, block)
            } finally {
                completedSeq.set(seq)
            }
        } catch (e: Exception) {
            System.err.println("[资料库] $what 失败：${e.message}")
            fallback
        }
    }

    /** 写入时间。同一毫秒里的两次写入，列表靠 rowid 分先后（查询里的 `ORDER BY ... , rowid DESC`）。 */
    private fun stamp(): Long = clock()

    private fun encode(item: FeedItem) = FeedJson.encodeToString(FeedItem.serializer(), item)

    private fun decode(json: String): FeedItem? =
        runCatching { FeedJson.decodeFromString(FeedItem.serializer(), json) }.getOrNull()

    private fun jsonOf(text: String?): JsonElement? =
        text?.let { runCatching { FeedJson.parseToJsonElement(it) }.getOrNull() }

    /** 读全量（含墓碑），导出和合并预览都用它。 */
    private fun readState(): SyncState = guarded("读同步状态", SyncState()) { native.readSyncState().toSyncState() }

    private fun lastImportedSeq(device: String): Long =
        guarded("读导入序号", 0L) { native.lastImportedSeq(device) }

    /** 库出错（磁盘满、文件被删、`.so` 没装好……）只打日志，不让界面线程跟着崩。 */
    private inline fun <T> guarded(what: String, fallback: T, block: () -> T): T = try {
        block()
    } catch (e: Exception) {
        System.err.println("[资料库] $what 失败：${e.message}")
        fallback
    }

    private inline fun guarded(what: String, block: () -> Unit) = guarded(what, Unit, block)

    override fun close() {
        flush()
        writeQueue.shutdown()
        runCatching { writeQueue.awaitTermination(5, TimeUnit.SECONDS) }
        runCatching { native.shutdown() }
    }

    // ---------------------------------------------------------------- 合并回调

    /**
     * 宿主的合并引擎（`LibraryMerge.merge`）。**Rust 在写锁里同步调进来一次**：只做纯计算，
     * 绝不回调 `native`（会把写锁等死）。
     */
    private class MergeBridge : FfiLibMergeHost {
        override fun merge(
            local: FfiLibSyncState,
            remote: FfiLibSyncState,
            retentionCutoffMs: Long?,
        ): FfiLibMergeOutcome {
            val localState = local.toSyncState()
            val remoteState = remote.toSyncState()
            val outcome = LibraryMerge.merge(localState, remoteState, retentionCutoffMs)
            val localUids = localState.folders.mapTo(HashSet()) { it.uid }
            return FfiLibMergeOutcome(
                state = outcome.state.toFfi(),
                aliases = outcome.folderAliases.map { (oldUid, newUid) -> FfiLibFolderAlias(oldUid, newUid) },
                favoritesAdded = outcome.counts.favoritesAdded,
                favoritesUpdated = outcome.counts.favoritesUpdated,
                favoritesDeleted = outcome.counts.favoritesDeleted,
                foldersAdded = outcome.counts.foldersAdded,
                foldersUpdated = outcome.counts.foldersUpdated,
                foldersDeleted = outcome.counts.foldersDeleted,
                historyAdded = outcome.counts.historyAdded,
                historyUpdated = outcome.counts.historyUpdated,
                historyDeleted = outcome.counts.historyDeleted,
                blockedAdded = outcome.counts.blockedAdded,
                blockedUpdated = outcome.counts.blockedUpdated,
                blockedDeleted = outcome.counts.blockedDeleted,
                foldersCreated = outcome.foldersCreated,
                foldersClaimed = outcome.folderAliases.count { (oldUid, newUid) ->
                    oldUid in localUids || newUid in localUids
                },
                historyClearedByRemote = outcome.historyClearedByRemote,
                clearedRows = outcome.clearedRows,
            )
        }
    }
}

// ===========================================================================
// 过边界的数据转换
// ===========================================================================

/** `FfiLibMergeOutcome` 里的计数 → `SyncCounts`。 */
private fun FfiLibMergeOutcome.counts(): SyncCounts = SyncCounts(
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

/** 从库里读出来的全量 → 合并引擎认的模型（`item_json` 在这里解析成 JSON 树）。 */
private fun FfiLibSyncState.toSyncState(): SyncState = SyncState(
    historyClearedHlc = historyClearedHlc,
    folders = folders.map {
        SyncFolderRow(
            uid = it.uid,
            name = it.name,
            createdAt = it.createdAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
        )
    },
    favorites = favorites.map {
        SyncFavoriteRow(
            key = it.key,
            folderUid = it.folderUid,
            addedAt = it.addedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
            item = parseJson(it.itemJson),
        )
    },
    history = history.map {
        SyncHistoryRow(
            key = it.key,
            visitedAt = it.visitedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
            progressMs = it.progressMs,
            durationMs = it.durationMs,
            progressHlc = it.progressHlc,
            progressDev = it.progressDev,
            item = parseJson(it.itemJson),
        )
    },
    blocked = blocked.map {
        SyncBlockedRow(
            key = it.key,
            user = parseJson(it.userJson),
            blockedAt = it.blockedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
        )
    },
)

/**
 * 合并引擎的结果 → 过边界的样子。**那四列（platform / is_video / search_text / adult）在这里算**：
 * 老平台 enum 名的映射（`Platform.fromStored`）、`searchText()` 的 `take(120)` 口径都在 Kotlin，
 * Rust 只负责落列（理由见 `feedhub_core::library::ItemColumns`）。
 */
private fun SyncState.toFfi(): FfiLibSyncState = FfiLibSyncState(
    historyClearedHlc = historyClearedHlc,
    folders = folders.map {
        FfiLibSyncFolderRow(
            uid = it.uid,
            name = it.name,
            createdAt = it.createdAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
        )
    },
    favorites = favorites.map {
        val item = it.item?.let { el -> runCatching { FeedJson.decodeFromJsonElement(FeedItem.serializer(), el) }.getOrNull() }
        FfiLibSyncFavoriteRow(
            key = it.key,
            folderUid = it.folderUid,
            addedAt = it.addedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
            // 写回的 JSON 用**解码后的 FeedItem 重新编码**（和 `SqliteLibrary.applyMerged` 的 `encode(item)` 一字不差）：
            // 直接把 JsonElement 打印出来会在"远端 JSON 显式写了默认值"时和旧实现漂开（`.dump` 就不再逐行相同）。
            itemJson = item?.let { feed -> FeedJson.encodeToString(FeedItem.serializer(), feed) },
            columns = item?.toColumns(),
        )
    },
    history = history.map {
        val item = it.item?.let { el -> runCatching { FeedJson.decodeFromJsonElement(FeedItem.serializer(), el) }.getOrNull() }
        FfiLibSyncHistoryRow(
            key = it.key,
            visitedAt = it.visitedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
            progressMs = it.progressMs,
            durationMs = it.durationMs,
            progressHlc = it.progressHlc,
            progressDev = it.progressDev,
            itemJson = item?.let { feed -> FeedJson.encodeToString(FeedItem.serializer(), feed) },
            columns = item?.toColumns(),
        )
    },
    blocked = blocked.map {
        // 同 favorites：解不开的 `user` 一律当"只有墓碑"（和旧实现 `applyMerged` 的分支一致）。
        val user = it.user?.let { el -> runCatching { FeedJson.decodeFromJsonElement(UserRef.serializer(), el) }.getOrNull() }
        FfiLibSyncBlockedRow(
            key = it.key,
            userJson = user?.let { u -> FeedJson.encodeToString(UserRef.serializer(), u) },
            blockedAt = it.blockedAt,
            revHlc = it.revHlc,
            revDev = it.revDev,
            deleted = it.deleted,
        )
    },
)

private fun parseJson(text: String?): JsonElement? =
    text?.let { runCatching { FeedJson.parseToJsonElement(it) }.getOrNull() }

private fun FeedItem.toColumns() = FfiLibItemColumns(
    platform = platform.key,
    isVideo = isVideoLike(),
    searchText = searchText(),
    adult = adult,
)
