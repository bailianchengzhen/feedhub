package feedhub.app.library

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.ContentType
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.Platform
import feedhub.core.UserRef
import feedhub.app.embed.embeddingVecHalfToFloats
import feedhub.core.sync.LibrarySnapshot

/**
 * 本地资料库：收藏（带收藏夹）、历史记录（含视频播放进度）、屏蔽的用户。都只存在本机，不碰平台的写接口。
 *
 * 读的方法能被 Compose 观察到，界面才能跟着变，分两类：
 * - 小而常用的（[isFavorite]、[isBlocked]、[folders]、[blockedUsers]、[progress]、[favoriteCount]）直接读 snapshot state，
 *   在组合里调就会随数据重组；
 * - 可能很大的列表（[favorites]、[history]）是 suspend 查询（在后台线程跑），界面把 [favoritesRevision] /
 *   [historyRevision] 当 key，数据变了（计数器变了）就重新查（见 `mine/` 下的用法）。
 *
 * 实现：桌面和安卓的生产路径是 `RustLibrary`，
 * 测试 / 没有数据库时用 [MemoryLibrary]；[SqliteLibrary] 只留作对拍参照（`RustLibraryDualRunTest`）。
 *
 * **平台标成 18+ 的条目（[FeedItem.adult]）在读取时一律滤掉**：[favorites]、[history]、[historyCount]、[favoriteCount] 都不含它们，
 * 查询的分页（LIMIT / OFFSET）也不会把隐藏的算进一页里。写入照旧存下来（字段在 JSON 里），
 * 只是永远不显示；[isFavorite] 仍返回 true（收藏状态本身没变）。
 */
interface Library {
    // ---------- 收藏 ----------
    fun isFavorite(key: String): Boolean
    /**
     * 收藏到某个收藏夹，[folder] 是 [FavoriteFolder.id]（null = 默认收藏夹；id 不存在也当默认）。
     * 已收藏就移到这个收藏夹（收藏时间不变）。同时把条目的完整内容存下来，这样离线也能显示卡片。
     */
    fun addFavorite(item: FeedItem, folder: String? = null)
    fun removeFavorite(key: String)

    /** 用户建的收藏夹，按建立时间排。默认收藏夹不在里面（它的 id 是 null）。 */
    val folders: List<FavoriteFolder>
    fun createFolder(name: String): FavoriteFolder
    fun renameFolder(id: String, name: String)

    /** 删收藏夹。[deleteItems] 为 false 时里面的收藏移回默认收藏夹，true 时连收藏一起删。 */
    fun deleteFolder(id: String, deleteItems: Boolean = false)

    /** 收藏数（某个收藏夹或全部）。 */
    fun favoriteCount(folder: FolderFilter = FolderFilter.All): Int

    /** 收藏有任何变化（增删、移动、改收藏夹）就加一；列表查询拿它当 key。 */
    val favoritesRevision: Int

    /** 按条件查收藏，新收藏的在前。 */
    suspend fun favorites(query: FavoriteQuery = FavoriteQuery()): List<Favorite>

    // ---------- 历史 ----------
    /** 打开详情时调：记一条（同一条内容只保留最新一次，之前的播放进度保留）。 */
    fun recordVisit(item: FeedItem)

    /** 视频播放进度（毫秒），下次从这里续播。这条内容不在历史里（被删了）就忽略。 */
    fun saveProgress(key: String, positionMs: Long, durationMs: Long?)
    fun progress(key: String): Long?

    /**
     * 只在内存里查播放进度（不碰数据库）。历史列表这类"每张卡片都要读"的地方用它：
     * 未命中时调用方退回列表查询带回来的 `progressMs` 即可，不要在主线程逐条查库。
     */
    fun progressCached(key: String): Long? = null

    /** 历史有任何变化（新增、删除、清空）就加一；列表查询拿它当 key。播放进度的更新不算（几秒一次，不值得重查）。 */
    val historyRevision: Int

    /** 按条件查历史，最近看的在前；[HistoryQuery.limit] / [HistoryQuery.offset] 用来分页。 */
    suspend fun history(query: HistoryQuery = HistoryQuery()): List<HistoryEntry>
    suspend fun historyCount(platform: Platform? = null, search: String = ""): Int
    fun removeHistory(key: String)
    fun clearHistory()

    /** 删掉最近一次访问早于 [olderThanMs]（毫秒时间戳）的历史，返回删了多少条。设置里"只保留最近 N 天"用。 */
    fun pruneHistory(olderThanMs: Long): Int

    // ---------- 屏蔽 ----------
    fun isBlocked(user: UserRef): Boolean
    fun block(user: UserRef)
    fun unblock(user: UserRef)

    /** 屏蔽的用户，最近屏蔽的在前。 */
    val blockedUsers: List<UserRef>

    /**
     * 屏蔽的作者 + 什么时候屏蔽的：算"屏蔽作者"这个负信号要时间衰减，
     * 而 [blockedUsers] 只有人、没有时间。没有时间的实现按 0 返回（信号里就是不衰减的那一档）。
     */
    fun blockedAuthors(): List<BlockedAuthor> = blockedUsers.map { BlockedAuthor(it, 0L) }

    // ---------- 行为数据（曝光 / 停留） ----------
    /**
     * 记一次曝光（含这次累计的停留）。**同一条内容（同 [key]）在同一个 [surface] 上、
     * 距这条记录上次被看到的时刻不超过 [mergeWindowMs] 时，合并进上一条记录**：
     * 只把 [dwellMs] 累加上去、把"最近看到"的时间推后，不新增行；隔得超过 [mergeWindowMs] 再看到才算新的一次曝光
     * （新起一行）。这样滚动来回蹭不会灌出一堆行，又保留了"隔了很久又看到一次"的信号。
     *
     * @param at 这次曝光成立的时刻（毫秒）——这一段可见时间的**开始**。
     * @param lastSeenAt 这一段可见时间的**结束**时刻（毫秒），写进"最近一次看到"。合并窗口和保留期都按它算，
     *   所以"连续看了 10 分钟"这种长停留的 `last_seen_at` 是结束时刻，不会因为开始得早就被当成很久以前。
     * @param dwellMs 这次在屏幕上停留的毫秒数。"可见面积 ≥ 50% 且连续 ≥ 1 秒"的判定和停留计时在
     *   `feedhub.app.feed.ExposureTracker` 里，本方法只管存。
     */
    fun recordExposure(
        key: String,
        platform: Platform,
        at: Long,
        dwellMs: Long,
        surface: ExposureSurface = ExposureSurface.HOME,
        lastSeenAt: Long = at,
        mergeWindowMs: Long = EXPOSURE_MERGE_WINDOW_MS,
    )

    /** 现在存了多少条曝光（设置页显示用，也决定"清除"按钮能不能点）。 */
    fun exposureCount(): Int

    /**
     * 一段时间（按"最近一次看到"落在 `[fromMs], [toMs)` 里算）的曝光统计，可以再按 [platform] /
     * [surface] 过滤。[ExposureStats.clicked] 是其中之后被点开过的条数，
     * 所以"**曝光了但没点**" = `exposures - clicked`。
     */
    suspend fun exposureStats(
        fromMs: Long,
        toMs: Long,
        platform: Platform? = null,
        surface: ExposureSurface? = null,
    ): ExposureStats

    /**
     * 全部曝光行（导出行为数据用，新的在前）。[limit] 是保护上限，正常用不到（默认 10 万行 ≈ 10 MB）。
     * 界面上不直接显示这些行（那是 [exposureStats] 的事），导出工具需要原始数据。
     */
    suspend fun exposureRows(limit: Int = 100_000): List<ExposureRow> = emptyList()

    /** 删掉"最近一次看到"早于 [olderThanMs] 的曝光，返回删了多少条（照 [pruneHistory] 的写法）。 */
    fun pruneExposure(olderThanMs: Long): Int

    /** 清空全部行为数据（设置页的"清除行为数据"）：曝光 + **曝光过的卡片内容**（[recordSeenItem]）。 */
    fun clearExposure()

    // ---------- 曝光过的卡片内容 ----------
    /**
     * 记一条"曝光过的卡片内容"（表 `seen_item`）：**和 [recordExposure] 同一个时机**（卡片可见面积 ≥ 50%
     * 且连续 ≥ 1 秒，见 `feedhub.app.feed.ExposureTracker`），受设置里的"记录行为数据"开关管（关着时 tracker
     * 根本不调这里）。
     *
     * 为什么要另存一份内容：[exposure] 只有 key / 平台 / 停留，而 [history] 只收**点开过**的，"曝光了但没点开"
     * 的卡片在库里没有标题——以后要拿它们做 embedding 回放（"用户看过、划过了的是什么"）就只剩光秃秃的 key。
     *
     * - 同一个 key 只写**第一次**（`INSERT OR IGNORE`）：重复曝光不覆盖，[at] 就是第一次看到的时间；
     * - [batchId] 是同一次刷新 / 加载更多的一批共用的 id；[platformRank] 是这条在它所属平台**原始返回顺序**里的
     *   序号（0 起，每批每平台重新数），以后打分拿它当"平台自己排在第几"的特征；
     * - 平台标成 18+ 的（[FeedItem.adult]）**不写**（和"整条隐藏"一致）；
     * - **不进跨设备同步**（`sync_*` 那套只搬收藏 / 历史 / 屏蔽）；保留期**单独**是 [SEEN_ITEM_RETENTION_DAYS]
     *   （30 天，比曝光的 [EXPOSURE_RETENTION_DAYS] 短：它每行带整段 JSON，见常量上的说明）。
     */
    fun recordSeenItem(
        item: FeedItem,
        feedKind: FeedKind,
        at: Long,
        batchId: Long,
        platformRank: Int,
    ) {
    }

    /** 现在存了多少条"曝光过的卡片"（设置页 / 测试用；没有这张表的实现返回 0）。 */
    fun seenItemCount(): Int = 0

    /** 最近的 N 条曝光内容，第一次看到的在前（以后离线回放 / 打分用）。 */
    suspend fun seenItems(limit: Int = 200): List<SeenItem> = emptyList()

    /** 删掉第一次看到早于 [olderThanMs] 的曝光内容，返回删了多少条（保留期是 [SEEN_ITEM_RETENTION_DAYS]，和曝光分开）。 */
    fun pruneSeenItems(olderThanMs: Long): Int = 0

    // ---------- 本地向量 ----------
    /**
     * 写入一批卡片向量（表 `item_embedding`，**只存在本机、不进跨设备同步**）。
     *
     * 表的主键是 **(key, model)**：**同一个 key + 同一个 model 再来一条就整行替换**
     * （重算时不用先删），**换 model 是另一行**——同一条内容可以同时有纯文字向量（`…-htp-l128`）和封面向量
     * （`…-htp-l128-img8x8`），互不覆盖。
     * [dim] 是截断 + 归一化之后**真正存下来的**维数，所以以后改维数不用改表。
     *
     * 返回写成功的行数。**开关关着时调用方根本不该调它**（见 `EmbeddingScheduler`），这里不做开关判断。
     */
    fun insertEmbeddings(rows: List<ItemEmbedding>): Int = 0

    /** 这批 key 里**已经有向量**的那些（同一个 [model] 下）。算之前先查它，别对同一条反复算。 */
    fun embeddingKeys(keys: Collection<String>, model: String = EMBEDDING_MODEL_ID): Set<String> = emptySet()

    /** 现在存了多少条向量（设置页显示"已算条数"）。 */
    fun embeddingCount(): Int = 0

    /**
     * 按 `(model, source)` 数向量：设置页要显示"已算条数里有多少来自收藏 / 历史"，
     * 还要按后端（CPU / NPU 两个向量空间）分开。
     *
     * [ItemEmbedding.source] 是**算这条向量时它是怎么进队列的**（首页新卡片 / 收藏补算 / 历史补算），
     * 不是"它现在在不在收藏里"——同一条内容先被首页算过、后来才收藏，来源仍是首页（同一条只会算一次）。
     */
    fun embeddingStats(): List<EmbeddingStat> = emptyList()

    /**
     * 收藏 / 历史里**还没有这个模型向量**的条目，最近的在前。
     *
     * [sinceMs] 是时间窗口（和 `item_embedding` 的保留期一致：更早的算出来也会被清掉）；
     * [limit] 是这一块的上限——`EmbeddingScheduler` 一次只取一小块，免得把队列塞满、把首页新卡片挤到后面。
     */
    suspend fun embeddingBackfill(model: String, sinceMs: Long, limit: Int): List<EmbeddingCandidate> = emptyList()

    /** 删掉 [createdAt] 早于 [olderThanMs] 的向量，返回删了多少条（保留期和 `seen_item` 一样是 30 天）。 */
    fun pruneEmbeddings(olderThanMs: Long): Int = 0

    /**
     * 最近的 [limit] 条向量，新的在前（导出用；向量在库里是 float16 的原始字节，见 [ItemEmbedding.vec]）。
     * 导出时按 base64 写进 JSON Lines，离线回放工具 `tools/embed/replay.py` 解码。
     */
    suspend fun embeddings(limit: Int = 100_000): List<ItemEmbedding> = emptyList()

    /** 清空全部向量（设置里的「清除行为数据」和它一起清）。 */
    fun clearEmbeddings() {}

    // ---------- 跨设备同步 ----------
    /**
     * 这个实现能不能导出 / 导入同步文件。[MemoryLibrary] 是 false（它没有落盘的墓碑 / 收藏夹 uid，
     * 导出的东西不完整）；`RustLibrary` / [SqliteLibrary] 是 true。
     */
    val supportsSync: Boolean get() = false

    /** 本机的设备标识（8 字节 base64url；第一次用时生成，存在库里）。 */
    fun syncDeviceId(): String? = null

    /**
     * 生成一份**全量**快照（含墓碑），同时把本机的快照序号 +1（导入端靠它挡"旧文件回滚"）。
     * 只在 [supportsSync] 的库上有意义。
     */
    fun syncSnapshot(now: Long = System.currentTimeMillis()): LibrarySnapshot? = null

    /**
     * 预览一次导入：**只算不写**。返回"[ImportPreview.counts] 新增 / 更新 / 删除各几条"和收藏夹认领情况，
     * 界面拿它给用户确认。[retentionCutoffMs] 是本机"只保留最近 N 天"的截止时间（null = 不裁剪），
     * 保留期是本机策略，只过滤远端进来的旧历史。
     */
    fun previewImport(
        remote: LibrarySnapshot,
        now: Long = System.currentTimeMillis(),
        retentionCutoffMs: Long? = null,
    ): ImportPreview? = null

    /**
     * 把远端快照合并进本机（**一个事务**，逐条 LWW + 墓碑 + 清空标记，绝不整库覆盖）。
     * [ignoreStale] 只在用户确认"这是一个旧备份，我就要恢复它"时给 true。
     */
    fun applyImport(
        remote: LibrarySnapshot,
        now: Long = System.currentTimeMillis(),
        retentionCutoffMs: Long? = null,
        ignoreStale: Boolean = false,
    ): ImportResult? = null

    // ---------- 跨设备同步 ----------

    /**
     * 已配对的设备（有公钥的 `sync_peer` 行），按配对时间从新到旧。
     * 只是"导入过文件"的设备也会有一行 `sync_peer`，但**不算已配对**、导出不会发给它。
     */
    fun syncPeers(): List<SyncPeer> = emptyList()

    /** 记下一台已配对设备（同一台重复配对 = 更新名字 / 公钥 / 配对时间，已有的导入序号保留）。 */
    fun upsertSyncPeer(peer: SyncPeer) {}

    /** 移除一台已配对设备（同时清掉它的导入序号；以后重新配对从 0 开始）。 */
    fun removeSyncPeer(deviceId: String): Boolean = false

    /** 排空后台待写队列（`onStop` / `shutdown` 时保证已投递的进度、曝光、收藏全部落盘）。 */
    fun flush() {}
}

/**
 * 同一条内容的两次曝光相隔多久以内算"同一次"（见 [Library.recordExposure]）。
 * 5 分钟：够长到把"滚过去又滚回来""切出去一下又回来"并成一次，够短到几次真的分开的浏览还留得下来。
 * 合并只在同一个 [ExposureSurface] 内发生（首页看到和详情里再看到是两回事）。
 */
const val EXPOSURE_MERGE_WINDOW_MS = 5 * 60_000L

/**
 * 这次曝光发生在哪个面。
 *
 * - [HOME] / [FOLLOWING]：首页 / 关注页的信息流卡片；
 * - [DETAIL]：详情页本身（打开一条内容时记一行，停留 = 详情可见的前台时长）；
 * - [FAVORITES] / [HISTORY]：收藏页 / 历史页的卡片；
 * - [RELATED]：视频详情的"相关推荐"。
 *
 * 存的是 [key]（列名 `surface`），换名字等于换数据，定了就别改。统计时可以按它过滤
 * （[Library.exposureStats] 的 `surface` 参数）。
 */
enum class ExposureSurface(val key: String) {
    HOME("home"),
    FOLLOWING("following"),
    DETAIL("detail"),
    FAVORITES("favorites"),
    HISTORY("history"),
    RELATED("related"),
    ;

    companion object {
        /** 读库时按 [key] 找；认不出的（以后删过面、或者别处写进去的）当 [HOME]。 */
        fun fromKey(key: String): ExposureSurface = entries.firstOrNull { it.key == key } ?: HOME
    }
}

/**
 * 行为数据保留多少天。
 * 曝光是"每条内容都产生"的数据，量级比收藏 / 历史大一两个数量级，必须有滚动清理（见 [Library.pruneExposure]）。
 */
const val EXPOSURE_RETENTION_DAYS = 90

/**
 * 曝光过的卡片内容（`seen_item`）保留多少天。
 *
 * 为什么不跟曝光一样：`exposure` 一行只有 key / 平台 / 停留，`seen_item` 一行还带**整段
 * `item_json`**（标题、摘要、媒体列表，粗估 1~3 KB）——一天 500~1000 条 × 90 天会到上百 MB。这份数据只用来
 * 离线回放 / 打分（"用户看过、划过了的是什么"），30 天足够。
 */
const val SEEN_ITEM_RETENTION_DAYS = 30

/**
 * [nowMs] 这一刻该清掉"第一次看到早于哪一刻"的曝光内容（[SEEN_ITEM_RETENTION_DAYS] 天前）。
 * 单独拿出来是为了能离线测（[feedhub.app.AppGraph.applyBehaviorRetention] 两个 cutoff 各算各的）。
 */
fun seenItemCutoff(nowMs: Long): Long = nowMs - SEEN_ITEM_RETENTION_DAYS * 86_400_000L

/**
 * 卡片向量当前存多少维。
 * 模型原图输出 768 维；256 维在 EmbeddingGemma 2 的 MTEB 上只掉 ~1 个百分点，存储是 1/3。
 */
const val EMBEDDING_DIM: Int = 256

/**
 * 当前写进 `item_embedding.model` 的模型标识：**EmbeddingGemma 2 的 q4 图（`accuracy_level=4`）**。
 * 换模型 = 换这个字符串，老向量自然不会再被当成"已经算过"（[Library.embeddingKeys] 按它过滤），
 * 也不会被离线回放混着用。
 */
const val EMBEDDING_MODEL_ID: String = "embeddinggemma-2-q4-acc4"

/**
 * NPU（QNN / Hexagon HTP）那条路写进 `item_embedding.model` 的标识。
 *
 * **和 [EMBEDDING_MODEL_ID] 是两个向量空间**：同一批权重、不同的量化（w8a16 + int8 词表）和不同的截断，
 * 库里按 `model` 过滤，所以切后端不会把另一份模型的向量
 * 当成"已经算过"，也绝不在同一个索引里混着比。
 *
 * **为什么长度也要进名字**：HTP 的图是固定形状的（一个长度一张图，做不了动态形状），`[8,64]` 和 `[8,128]`
 * 对同一条长摘要会看到不同多的正文，算出来**不是同一批点**。所以它和"换模型"一样处理：换长度 = 换 `model`，老向量自然失效、各自建索引。
 *
 * @param seqLen 这张图每条最多几个 token（含 `<bos>` / `<eos>`），例如 32 / 64 / 128。
 */
fun npuModelId(seqLen: Int): String = "embeddinggemma-2-w8a16-htp-l$seqLen"

/**
 * **封面向量**（文字 + 封面图）写进 `item_embedding.model` 的标识。
 *
 * 和纯文字的那两个 id（[EMBEDDING_MODEL_ID] / [npuModelId]）是**三个不同的向量空间**，理由都是数字：
 * 同一条内容带不带封面，256 维余弦**中位只有 0.9156、p05 0.803**——
 * 比"同长度下量化的差"（0.997）大一个数量级。所以：
 * - **并存、不替换**：同一条内容可以两种向量都有（纯文字那份照旧算、照旧用）；
 * - 库里按 `model` 过滤（`Library.embeddingKeys`），加了这个后缀就等于"这批卡片还没有封面向量"，
 *   会按新口径算——**不需要迁移脚本**；
 * - 画像（`TasteProfile`）/ 导出回放也按 `model` 分开，绝不混着比。
 *
 * @param seqLen 文本+图那张图的序列长度。
 */
fun npuCoverModelId(seqLen: Int): String = "embeddinggemma-2-w8a16-htp-l$seqLen-img8x8"

/**
 * 一段时间的曝光统计。
 *
 * **"曝光了但没点"不单独存字段**，它是 [exposures] - [clicked]，可以从曝光表和 history 表 join 出来：
 * 【同一条内容在曝光之后被打开过】= `exposure.key` 在 `history` 里有一条 `visited_at >= exposed_at` 的记录
 * （见 [Library.exposureStats] 的实现）。
 *
 * 两点要知道的：
 * - history 同一条内容只留**最近一次**打开，所以"很久以前曝光、之后点开过、最近又打开了一次"这种只能用最近这次判断；
 * - [clicked] 只说明"曝光之后打开过"，不代表是因为这次曝光点进去的（没有更细的归因数据）。
 */
data class ExposureStats(
    /** 曝光条数（表里的行数；同一次持续观看只算一条）。 */
    val exposures: Int,
    /** 其中之后被点开过的条数。 */
    val clicked: Int,
    /** 停留时长合计（毫秒）。 */
    val dwellMs: Long,
) {
    /** 曝光了但没点开的条数（"划过"的主要信号）。 */
    val notClicked: Int get() = exposures - clicked

    /** 点开率（0~1）。 */
    val clickRate: Float get() = if (exposures == 0) 0f else clicked.toFloat() / exposures
}

/**
 * 一行曝光（表 `exposure` 的原始数据，**导出行为数据**用）：同一条内容 + 同一个面在合并窗口内只留一行，
 * [dwellMs] 是累计停留，[lastSeenAt] 是最近一次看到的时刻。
 */
data class ExposureRow(
    val key: String,
    val platform: Platform,
    val surface: ExposureSurface,
    val exposedAt: Long,
    val lastSeenAt: Long,
    val dwellMs: Long,
)

/** 收藏夹。默认收藏夹不是一条记录：[Library.addFavorite] 里它的 id 是 null。 */
data class FavoriteFolder(val id: String, val name: String)

data class Favorite(val item: FeedItem, val folder: String?, val addedAt: Long)

/** 历史里的一条：内容、最近一次打开的时间（毫秒）、视频看到哪了。 */
data class HistoryEntry(val item: FeedItem, val visitedAt: Long, val progressMs: Long?, val durationMs: Long?)

/**
 * 一条**曝光过的卡片内容**：完整的 [FeedItem]（和 `history.item_json` 同一种编码）、
 * 第一次看到的时刻、属于哪个流（首页 / 关注）、哪一批、在平台的原始顺序里排第几。
 *
 * 给以后的离线回放 / embedding 打分用：`exposure` 只有 key，而没点开过的卡片在 `history` 里没有内容。
 */
data class SeenItem(
    val item: FeedItem,
    val firstSeenAt: Long,
    val feedKind: FeedKind,
    val batchId: Long,
    val platformRank: Int,
)

/**
 * 一条卡片向量（表 `item_embedding` 的一行）。
 *
 * [vec] 是**存进库的原始字节**：当前是 [dim] 个 **float16**（小端，2 字节一个）。选 float16 而不是 float32：
 * 256 维 × 30 天 × 每天 1000 条 = 7.7 MB（f16）/ 15 MB（f32），而 f16 只有 ~3 位十进制有效数字，
 * 和我们关心的余弦差异（回放里 AUC 那点差距）比起来误差小两个数量级（离线测过往返误差 < 1e-3）。
 * 解码 / 编码在 `feedhub.app.embed.embeddingVecHalfToFloat` / `floatVecToHalfBytes`。
 */
class ItemEmbedding(
    val key: String,
    val model: String,
    val dim: Int,
    val vec: ByteArray,
    val createdAt: Long,
    /**
     * 这条向量是怎么进队列的：[EMBEDDING_SOURCE_FEED]（首页 / 关注的新卡片）、
     * [EMBEDDING_SOURCE_FAVORITE] / [EMBEDDING_SOURCE_HISTORY]（收藏 / 历史的补算）。
     */
    val source: String = EMBEDDING_SOURCE_FEED,
) {
    /** 存下来的向量（float32 一份，方便调用方直接点积；越界 / 长度不对就返回 null）。 */
    fun toFloats(): FloatArray? {
        if (dim <= 0 || vec.size != dim * 2) return null
        return embeddingVecHalfToFloats(vec, dim)
    }

    override fun toString(): String =
        "ItemEmbedding(key=$key, model=$model, dim=$dim, bytes=${vec.size}, at=$createdAt, source=$source)"
}

/** 向量来源：首页 / 关注的新卡片（`FeedState.onNewContent` 和点开详情）。 */
const val EMBEDDING_SOURCE_FEED: String = "feed"

/** 向量来源：收藏里的条目补算。 */
const val EMBEDDING_SOURCE_FAVORITE: String = "favorite"

/** 向量来源：历史里的条目补算。 */
const val EMBEDDING_SOURCE_HISTORY: String = "history"

/** 一条补算候选：卡片内容 + 它从哪来（收藏 / 历史）+ 那个动作的时刻（最近的先算）。 */
data class EmbeddingCandidate(
    val item: FeedItem,
    /** [EMBEDDING_SOURCE_FAVORITE] / [EMBEDDING_SOURCE_HISTORY]。 */
    val source: String,
    val at: Long,
)

/** [Library.embeddingStats] 的一行：某个模型下某种来源各几条。 */
data class EmbeddingStat(val model: String, val source: String, val count: Int)

/** 一个被屏蔽的作者 + 屏蔽时刻（[Library.blockedAuthors]）。 */
data class BlockedAuthor(val user: UserRef, val blockedAt: Long)

sealed interface FolderFilter {
    /** 全部收藏。 */
    data object All : FolderFilter

    /** 只看默认收藏夹。 */
    data object Default : FolderFilter

    data class Folder(val id: String) : FolderFilter
}

/**
 * 收藏的筛选。[type]：混合 = 不筛，视频 = 视频和直播，图文 = 其他。
 * [search] 在标题 / 摘要 / 作者 / 圈子里找（不分大小写，ASCII）。
 */
data class FavoriteQuery(
    val folder: FolderFilter = FolderFilter.All,
    val platform: Platform? = null,
    val type: ContentType = ContentType.MIXED,
    val search: String = "",
    val limit: Int = 5000,
)

data class HistoryQuery(
    val platform: Platform? = null,
    val search: String = "",
    val limit: Int = 200,
    val offset: Int = 0,
)

fun UserRef.blockKey() = "${platform.key}:$id"

/** 视频和直播算"视频"，其他算"图文"（和信息流卡片的分法一致）。 */
fun FeedItem.isVideoLike() = kind == ItemKind.VIDEO || kind == ItemKind.LIVE

/** 搜索用的文本：标题、摘要开头、作者、圈子。 */
fun FeedItem.searchText(): String =
    listOfNotNull(title, summary?.take(120), author?.name, community).joinToString(" ")

internal fun FeedItem.matches(platform: Platform?, type: ContentType, search: String): Boolean {
    if (platform != null && this.platform != platform) return false
    when (type) {
        ContentType.MIXED -> Unit
        ContentType.VIDEO -> if (!isVideoLike()) return false
        ContentType.TEXT -> if (isVideoLike()) return false
    }
    return search.isBlank() || searchText().contains(search.trim(), ignoreCase = true)
}

/** 只在内存里的实现：测试用，也是没有数据库时的退路（重启就没了）。行为和 [SqliteLibrary] 一致。 */
class MemoryLibrary(private val clock: () -> Long = System::currentTimeMillis) : Library {
    private class Fav(val item: FeedItem, val folder: String?, val addedAt: Long, val seq: Long)
    private class Visit(val item: FeedItem, val visitedAt: Long, val progressMs: Long?, val durationMs: Long?, val seq: Long)

    private val favs = mutableStateMapOf<String, Fav>()
    private val visits = mutableStateMapOf<String, Visit>()
    private val blocked = mutableStateMapOf<String, Pair<UserRef, Long>>()
    private var nextFolderId = 1L

    /** 写入顺序号：同一毫秒里的两次写入靠它分先后。 */
    private var seq = 0L

    override var folders: List<FavoriteFolder> by mutableStateOf(emptyList())
        private set
    override var favoritesRevision by mutableIntStateOf(0)
        private set
    override var historyRevision by mutableIntStateOf(0)
        private set

    private fun stamp(): Long = clock()

    @Synchronized
    private fun nextSeq() = ++seq

    override fun isFavorite(key: String) = key in favs

    override fun addFavorite(item: FeedItem, folder: String?) {
        val f = folder?.takeIf { id -> folders.any { it.id == id } }
        val old = favs[item.key]
        favs[item.key] = Fav(item, f, old?.addedAt ?: stamp(), old?.seq ?: nextSeq())
        favoritesRevision++
    }

    override fun removeFavorite(key: String) {
        if (favs.remove(key) != null) favoritesRevision++
    }

    override fun createFolder(name: String): FavoriteFolder {
        require(name.isNotBlank()) { "收藏夹名字不能为空" }
        val f = FavoriteFolder((nextFolderId++).toString(), name.trim())
        folders = folders + f
        favoritesRevision++
        return f
    }

    override fun renameFolder(id: String, name: String) {
        require(name.isNotBlank()) { "收藏夹名字不能为空" }
        folders = folders.map { if (it.id == id) it.copy(name = name.trim()) else it }
        favoritesRevision++
    }

    override fun deleteFolder(id: String, deleteItems: Boolean) {
        for ((key, fav) in favs.entries.toList()) {
            if (fav.folder != id) continue
            if (deleteItems) favs.remove(key) else favs[key] = Fav(fav.item, null, fav.addedAt, fav.seq)
        }
        folders = folders.filter { it.id != id }
        favoritesRevision++
    }

    override fun favoriteCount(folder: FolderFilter) = favs.values.count { !it.item.adult && it.inFolder(folder) }

    override suspend fun favorites(query: FavoriteQuery): List<Favorite> = favs.values
        .filter { !it.item.adult && it.inFolder(query.folder) && it.item.matches(query.platform, query.type, query.search) }
        .sortedWith(compareByDescending<Fav> { it.addedAt }.thenByDescending { it.seq })
        .take(query.limit)
        .map { Favorite(it.item, it.folder, it.addedAt) }

    private fun Fav.inFolder(f: FolderFilter) = when (f) {
        FolderFilter.All -> true
        FolderFilter.Default -> folder == null
        is FolderFilter.Folder -> folder == f.id
    }

    override fun recordVisit(item: FeedItem) {
        val old = visits[item.key]
        visits[item.key] = Visit(item, stamp(), old?.progressMs, old?.durationMs, nextSeq())
        historyRevision++
    }

    override fun saveProgress(key: String, positionMs: Long, durationMs: Long?) {
        val v = visits[key] ?: return
        visits[key] = Visit(v.item, v.visitedAt, positionMs, durationMs ?: v.durationMs, v.seq)
    }

    override fun progress(key: String) = visits[key]?.progressMs

    override fun progressCached(key: String) = visits[key]?.progressMs

    override suspend fun history(query: HistoryQuery): List<HistoryEntry> = visits.values
        .filter { !it.item.adult && it.item.matches(query.platform, ContentType.MIXED, query.search) }
        .sortedWith(compareByDescending<Visit> { it.visitedAt }.thenByDescending { it.seq })
        .drop(query.offset)
        .take(query.limit)
        .map { HistoryEntry(it.item, it.visitedAt, it.progressMs, it.durationMs) }

    override suspend fun historyCount(platform: Platform?, search: String) =
        visits.values.count { !it.item.adult && it.item.matches(platform, ContentType.MIXED, search) }

    override fun removeHistory(key: String) {
        if (visits.remove(key) != null) historyRevision++
    }

    override fun clearHistory() {
        visits.clear()
        historyRevision++
    }

    override fun pruneHistory(olderThanMs: Long): Int {
        val old = visits.filterValues { it.visitedAt < olderThanMs }.keys
        old.forEach(visits::remove)
        if (old.isNotEmpty()) historyRevision++
        return old.size
    }

    override fun isBlocked(user: UserRef) = user.blockKey() in blocked
    override fun block(user: UserRef) {
        blocked[user.blockKey()] = user to stamp()
    }

    override fun unblock(user: UserRef) {
        blocked.remove(user.blockKey())
    }

    override val blockedUsers: List<UserRef>
        get() = blocked.values.sortedByDescending { it.second }.map { it.first }

    override fun blockedAuthors(): List<BlockedAuthor> =
        blocked.values.sortedByDescending { it.second }.map { BlockedAuthor(it.first, it.second) }

    // ---------- 行为数据（曝光 / 停留） ----------

    /** 一次曝光（行）。同一次持续观看只用一行：[lastSeenAt] 跟着最后一次看到的时刻往后走，[dwellMs] 一直累加。 */
    private class Exposure(
        val key: String,
        val platform: Platform,
        val surface: ExposureSurface,
        val exposedAt: Long,
        var lastSeenAt: Long,
        var dwellMs: Long,
    )

    private val exposures = mutableListOf<Exposure>()

    override fun recordExposure(
        key: String,
        platform: Platform,
        at: Long,
        dwellMs: Long,
        surface: ExposureSurface,
        lastSeenAt: Long,
        mergeWindowMs: Long,
    ) {
        // 同一条 key + surface 的行是按时间往后追加的，所以"最后一个 key / surface 相同且在窗口内"的就是最近那一条
        val last = exposures.lastOrNull { it.key == key && it.surface == surface && at - it.lastSeenAt <= mergeWindowMs }
        if (last != null) {
            last.dwellMs += dwellMs
            last.lastSeenAt = maxOf(last.lastSeenAt, lastSeenAt)
        } else {
            exposures += Exposure(key, platform, surface, at, lastSeenAt, dwellMs)
        }
    }

    override fun exposureCount() = exposures.size

    override suspend fun exposureStats(fromMs: Long, toMs: Long, platform: Platform?, surface: ExposureSurface?): ExposureStats {
        val rows = exposures.filter {
            it.lastSeenAt >= fromMs && it.lastSeenAt < toMs &&
                (platform == null || it.platform == platform) &&
                (surface == null || it.surface == surface)
        }
        val clicked = rows.count { e -> visits[e.key]?.visitedAt?.let { it >= e.exposedAt } == true }
        return ExposureStats(rows.size, clicked, rows.sumOf { it.dwellMs })
    }

    override suspend fun exposureRows(limit: Int): List<ExposureRow> = exposures
        .sortedByDescending { it.lastSeenAt }
        .take(limit)
        .map { ExposureRow(it.key, it.platform, it.surface, it.exposedAt, it.lastSeenAt, it.dwellMs) }

    override fun pruneExposure(olderThanMs: Long): Int {
        val n = exposures.count { it.lastSeenAt < olderThanMs }
        if (n > 0) exposures.removeAll { it.lastSeenAt < olderThanMs }
        return n
    }

    override fun clearExposure() {
        exposures.clear()
        seen.clear()
        embeddings.clear()
    }

    // ---------- 曝光过的卡片内容 ----------

    /** 一行 `seen_item`：内容和第一次看到的时刻 / 流 / 批 / 平台序号。 */
    private class Seen(
        val item: FeedItem,
        val feedKind: FeedKind,
        val firstSeenAt: Long,
        val batchId: Long,
        val platformRank: Int,
    )

    /** 按 [Seen.firstSeenAt] 升序（写入顺序），去重靠 key。 */
    private val seen = LinkedHashMap<String, Seen>()

    override fun recordSeenItem(item: FeedItem, feedKind: FeedKind, at: Long, batchId: Long, platformRank: Int) {
        if (item.adult) return // 平台标成 18+ 的不写（和"整条隐藏"一致）
        seen.putIfAbsent(item.key, Seen(item, feedKind, at, batchId, platformRank))
    }

    override fun seenItemCount() = seen.size

    override suspend fun seenItems(limit: Int): List<SeenItem> = seen.values
        .sortedByDescending { it.firstSeenAt }
        .take(limit)
        .map { SeenItem(it.item, it.firstSeenAt, it.feedKind, it.batchId, it.platformRank) }

    override fun pruneSeenItems(olderThanMs: Long): Int {
        val old = seen.filterValues { it.firstSeenAt < olderThanMs }.keys.toList()
        old.forEach(seen::remove)
        return old.size
    }

    // ---------- 卡片向量 ----------

    /** 表 `item_embedding` 的主键是 **(key, model)**（v9）：同一条内容的不同口径各存一行。 */
    private fun embeddingId(model: String, key: String) = "$model\u0000$key"

    private val embeddings = LinkedHashMap<String, ItemEmbedding>()

    override fun insertEmbeddings(rows: List<ItemEmbedding>): Int {
        var n = 0
        for (row in rows) {
            embeddings[embeddingId(row.model, row.key)] = row
            n++
        }
        return n
    }

    override fun embeddingKeys(keys: Collection<String>, model: String): Set<String> =
        keys.filterTo(HashSet()) { embeddings[embeddingId(model, it)] != null }

    override fun embeddingCount() = embeddings.size

    override fun embeddingStats(): List<EmbeddingStat> = embeddings.values
        .groupingBy { it.model to it.source }
        .eachCount()
        .map { (k, n) -> EmbeddingStat(k.first, k.second, n) }
        .sortedWith(compareBy({ it.model }, { it.source }))

    override suspend fun embeddingBackfill(model: String, sinceMs: Long, limit: Int): List<EmbeddingCandidate> {
        if (limit <= 0) return emptyList()
        val out = LinkedHashMap<String, EmbeddingCandidate>()
        fun add(item: FeedItem, source: String, at: Long) {
            if (item.adult || at < sinceMs) return
            if (embeddings[embeddingId(model, item.key)] != null) return
            val old = out[item.key]
            if (old == null) {
                out[item.key] = EmbeddingCandidate(item, source, at)
            } else {
                // 收藏优先（同一条既收藏过又打开过时，来源记"收藏"），时间取两边最近的
                val src = if (old.source == EMBEDDING_SOURCE_FAVORITE || source == EMBEDDING_SOURCE_FAVORITE) {
                    EMBEDDING_SOURCE_FAVORITE
                } else {
                    old.source
                }
                out[item.key] = EmbeddingCandidate(old.item, src, maxOf(old.at, at))
            }
        }
        for (f in favs.values) add(f.item, EMBEDDING_SOURCE_FAVORITE, f.addedAt)
        for (v in visits.values) add(v.item, EMBEDDING_SOURCE_HISTORY, v.visitedAt)
        return out.values.sortedByDescending { it.at }.take(limit)
    }

    override fun pruneEmbeddings(olderThanMs: Long): Int {
        val old = embeddings.filterValues { it.createdAt < olderThanMs }.keys.toList()
        old.forEach(embeddings::remove)
        return old.size
    }

    override suspend fun embeddings(limit: Int): List<ItemEmbedding> =
        embeddings.values.sortedByDescending { it.createdAt }.take(limit)

    override fun clearEmbeddings() {
        embeddings.clear()
    }
}
