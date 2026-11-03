package feedhub.source.tieba

import feedhub.core.KeyValueStore
import feedhub.rust.RustTiebaForum

/**
 * 关注流的持久化：吧列表和各吧缓冲。键都以 `tieba.following.` 开头。
 *
 * - 吧列表（几 KB）：明文 JSON，最多一天刷新一次。
 * - 各吧缓冲：每个吧只留最新 [KEEP_PER_FORUM] 条，JSON 之后再 gzip + base64（FeedItem 的 JSON 里全是重复的链接和字段名，
 *   压缩后大约是原来的五分之一），整体一个键，一次 `put`（`FileKeyValueStore` 每次 `put` 都重写整个文件）。
 * 缓存读不出来（版本不对、损坏）就当没有，重新抓；不会因此抛异常。
 *
 * 和关注流的调度（`following.rs`）读写的是**同一份**。这个对象只剩薄门面：
 * 把 `ctx.store` 交出去、`FeedItem` 的模型映射（[RustTiebaForum] 里），没有第二份格式。
 *
 * **为什么要搬**：以前关注流的调度在 Rust、关注管理（`followings` / `setFollowing`）还在 Kotlin，
 * 两边读写同一个键、同一份格式，只能靠"必须逐字节一致"的约定盯着；现在只有一份实现。
 */
internal object ForumStore {
    const val KEY_FORUMS = "tieba.following.forums"
    const val KEY_BUFFERS = "tieba.following.buffers"
    const val VERSION = 1

    /** 旧缓存作废重取一次（一个请求），不牵连各吧的缓冲。 */
    const val FORUMS_VERSION = 2
    const val KEEP_PER_FORUM = 20

    /** 吧列表和它的抓取时间（毫秒）。 */
    class ForumList(val at: Long, val forums: List<FollowedForum>)

    fun loadForums(store: KeyValueStore): ForumList? = RustTiebaForum.loadForums(store)?.let { list ->
        ForumList(
            at = list.at,
            forums = list.forums.map {
                FollowedForum(
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

    fun saveForums(store: KeyValueStore, list: ForumList) {
        RustTiebaForum.saveForums(
            store = store,
            at = list.at,
            forums = list.forums.map {
                RustTiebaForum.Forum(
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

    /** 吧列表作废：下次关注流刷新时重新取（关注 / 取关之后调用）。各吧的缓冲不动，下次对一遍列表时自己增删。 */
    fun invalidateForums(store: KeyValueStore) = RustTiebaForum.invalidateForums(store)

    /** 各吧缓冲；读不出来 / 版本不对就是空表。老数据里 `detail` 是 `Web` 的已经升级成 `Native`（在 Rust 里）。 */
    fun loadBuffers(store: KeyValueStore): Map<Long, ForumState> =
        RustTiebaForum.loadBuffers(store).mapValues { (_, buffer) ->
            ForumState(
                id = buffer.id,
                name = buffer.name,
                dayThreads = buffer.dayThreads,
                items = buffer.items,
                tailPos = buffer.tailPos,
                exhausted = buffer.exhausted,
                headFetchedAt = buffer.headFetchedAt,
                perDay = buffer.perDay,
                closedUntil = buffer.closedUntil,
            )
        }

    /** 存盘前每个吧压缩到 [KEEP_PER_FORUM] 条（压缩也在 Rust 的 `save_buffers` 里做）。 */
    fun saveBuffers(store: KeyValueStore, states: Collection<ForumState>) {
        RustTiebaForum.saveBuffers(
            store = store,
            buffers = states.map {
                RustTiebaForum.Buffer(
                    id = it.id,
                    name = it.name,
                    dayThreads = it.dayThreads,
                    items = it.items,
                    tailPos = it.tailPos,
                    exhausted = it.exhausted,
                    headFetchedAt = it.headFetchedAt,
                    perDay = it.perDay,
                    closedUntil = it.closedUntil,
                )
            },
        )
    }
}
