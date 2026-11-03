package feedhub.rust

import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.KeyValueStore
import kotlinx.serialization.builtins.ListSerializer
import uniffi.feedhub_ffi.FfiTiebaForum
import uniffi.feedhub_ffi.FfiTiebaForumBuffer
import uniffi.feedhub_ffi.FfiTiebaForumStore
import uniffi.feedhub_ffi.tiebaForumStoreKeys

/**
 * 贴吧关注管理那条路的**存盘**。
 *
 * 键名（`tieba.following.forums` / `tieba.following.buffers`）、JSON 字段顺序、`gz1:` + base64(gzip(JSON))、
 * 每吧保留 20 条、版本号、老 `Web` 详情的升级**全在 Rust**：和关注流的调度（`following.rs`）读写的是同一份。
 * Kotlin 这边只做两件宿主的事：把 `ctx.store` 交出去、把 `FeedItem` 的 JSON 变成模型（[FeedJson]）。
 *
 * **条目走 JSON 文本**（[Buffer.itemsJson]）而不是一条条过 `FfiItem`：`FeedItem` 的 JSON 写法全仓只有一份
 * （`xhs::feeditem_json`，Rust 写的就是 Kotlin 读得回来的那份），这样两边不用再对一遍每个字段。
 *
 * 一次刷新 / 一次关注按钮各调一两次，不在界面的热路径上。
 */
object RustTiebaForum {
    /** 存盘常量（排障 / 测试断言用；和 Kotlin `ForumStore` 原来那几个一样）。 */
    class Keys(
        val forums: String,
        val buffers: String,
        val version: Long,
        val forumsVersion: Long,
        val keepPerForum: Int,
    )

    fun keys(): Keys = load {
        val k = tiebaForumStoreKeys()
        Keys(
            forums = k.forums,
            buffers = k.buffers,
            version = k.version,
            forumsVersion = k.forumsVersion,
            keepPerForum = k.keepPerForum.toInt(),
        )
    }

    /** 关注的一个吧（`ForumStore.ForumList.forums` 的一项）。 */
    class Forum(
        val id: Long,
        val name: String,
        val dayThreads: Int,
        val avatar: String?,
        val memberCount: Long?,
        val threadCount: Long?,
    )

    /** 吧列表 + 它的抓取时间（毫秒）。 */
    class ForumList(val at: Long, val forums: List<Forum>)

    /** 一个吧的缓冲（`ForumState`）。 */
    class Buffer(
        val id: Long,
        val name: String,
        val dayThreads: Int,
        val items: List<FeedItem>,
        val tailPos: Int,
        val exhausted: Boolean,
        val headFetchedAt: Long,
        val perDay: Double?,
        val closedUntil: Long,
    )

    /** `ForumStore.loadForums`：明文 JSON，版本不对 / 读不出来 → null。 */
    fun loadForums(store: KeyValueStore): ForumList? = store(store).loadForums()?.let { list ->
        ForumList(
            at = list.at,
            forums = list.forums.map {
                Forum(
                    id = it.id,
                    name = it.name,
                    dayThreads = it.dayThreads,
                    avatar = it.avatar,
                    memberCount = it.memberCount,
                    threadCount = it.threadCount,
                )
            },
        )
    }

    /** `ForumStore.saveForums`：`{"v":2,"at":<毫秒>,"forums":[…]}`。 */
    fun saveForums(store: KeyValueStore, at: Long, forums: List<Forum>) {
        store(store).saveForums(
            at = at,
            forums = forums.map {
                FfiTiebaForum(
                    id = it.id,
                    name = it.name,
                    dayThreads = it.dayThreads,
                    avatar = it.avatar,
                    memberCount = it.memberCount,
                    threadCount = it.threadCount,
                )
            },
        )
    }

    /** `ForumStore.invalidateForums`：下次关注流刷新时重新取（各吧缓冲不动）。 */
    fun invalidateForums(store: KeyValueStore) = store(store).invalidateForums()

    /** `ForumStore.loadBuffers`：读不出来 / 版本不对 → 空表；老 `Web` 详情已升级成 `Native`。 */
    fun loadBuffers(store: KeyValueStore): Map<Long, Buffer> =
        store(store).loadBuffers().associate { it.id to it.toBuffer() }

    /** `ForumStore.saveBuffers`：每个吧先压到 20 条，再 `gz1:` + base64(gzip(JSON))，整体一个键。 */
    fun saveBuffers(store: KeyValueStore, buffers: Collection<Buffer>) {
        store(store).saveBuffers(buffers.map { it.toFfi() })
    }

    private fun store(store: KeyValueStore): FfiTiebaForumStore = load { FfiTiebaForumStore(RustStoreHost(store)) }

    private fun FfiTiebaForumBuffer.toBuffer(): Buffer = Buffer(
        id = id,
        name = name,
        dayThreads = dayThreads,
        items = decodeItems(itemsJson),
        tailPos = tailPos,
        exhausted = exhausted,
        headFetchedAt = headFetchedAt,
        perDay = perDay,
        closedUntil = closedUntil,
    )

    private fun Buffer.toFfi(): FfiTiebaForumBuffer = FfiTiebaForumBuffer(
        id = id,
        name = name,
        dayThreads = dayThreads,
        itemsJson = encodeItems(items),
        tailPos = tailPos,
        exhausted = exhausted,
        headFetchedAt = headFetchedAt,
        perDay = perDay,
        closedUntil = closedUntil,
    )

    private val itemList = ListSerializer(FeedItem.serializer())

    /** `FeedItem` 列表 → JSON 数组文本（Rust 的 `feeditem_json` 读得回来）。 */
    private fun encodeItems(items: List<FeedItem>): String =
        FeedJson.encodeToString(itemList, items)

    /** JSON 数组文本 → `FeedItem` 列表。解不出来的整串当空（和"缓存坏了重抓"一个口径）。 */
    private fun decodeItems(json: String): List<FeedItem> =
        runCatching { FeedJson.decodeFromString(itemList, json) }.getOrDefault(emptyList())

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
