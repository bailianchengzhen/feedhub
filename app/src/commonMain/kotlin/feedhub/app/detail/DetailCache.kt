package feedhub.app.detail

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.Platform
import feedhub.present.RustPresentPaging

/**
 * 详情正文 + 评论第一页的**会话级缓存**。
 *
 * 访问序 LRU、时间戳、新鲜度判定全部下沉至 Rust（[uniffi.feedhub_ffi.FfiDetailCache]），
 * 宿主仅作为不可变对象引用的轻量持有槽位。
 */
object DetailCache : DetailCacheLike {
    const val MAX_ENTRIES = 16
    const val FRESH_MS = 10 * 60 * 1000L

    data class CommentSeed(val items: List<Comment>, val next: Cursor?, val ended: Boolean, val at: Long)

    private val engine = RustPresentPaging.defaultDetailCache()

    private val detailStore = HashMap<ULong, FeedDetail>()
    private val commentsStore = HashMap<ULong, List<Comment>>()

    fun key(platform: Platform, id: String): String = RustPresentPaging.detailCacheKey(platform.key, id)

    fun itemKey(item: feedhub.core.FeedItem): String = item.key

    // ---------------- 正文 ----------------

    override fun get(key: String): FeedDetail? = synchronized(this) {
        val h = engine.get(key) ?: return null
        detailStore[h]
    }

    override fun isFresh(key: String, now: Long): Boolean = synchronized(this) {
        engine.isFresh(key, now)
    }

    override fun put(key: String, detail: FeedDetail, now: Long) {
        synchronized(this) {
            val handle = engine.allocHandle()
            detailStore[handle] = detail
            val evicted = engine.put(key, handle, now)
            evicted.detailHandles.forEach { detailStore.remove(it) }
            evicted.commentsHandles.forEach { commentsStore.remove(it) }
        }
    }

    override fun remove(key: String) {
        synchronized(this) {
            val evicted = engine.remove(key)
            evicted.detailHandles.forEach { detailStore.remove(it) }
            evicted.commentsHandles.forEach { commentsStore.remove(it) }
        }
    }

    // ---------------- 评论第一页 ----------------

    fun comments(key: String, sort: CommentSort): CommentSeed? = synchronized(this) {
        val seed = engine.comments(key, sort.name) ?: return null
        val items = commentsStore[seed.handle] ?: return null
        CommentSeed(items, seed.next?.let(::Cursor), seed.ended, seed.at)
    }

    fun commentsFresh(key: String, sort: CommentSort, now: Long = System.currentTimeMillis()): Boolean = synchronized(this) {
        engine.commentsFresh(key, sort.name, now)
    }

    fun putComments(
        key: String,
        sort: CommentSort,
        items: List<Comment>,
        next: Cursor?,
        ended: Boolean,
        now: Long = System.currentTimeMillis(),
    ) {
        synchronized(this) {
            val handle = engine.allocHandle()
            commentsStore[handle] = items
            val evicted = engine.putComments(key, sort.name, handle, next?.value, ended, now)
            evicted.detailHandles.forEach { detailStore.remove(it) }
            evicted.commentsHandles.forEach { commentsStore.remove(it) }
        }
    }

    fun clear() {
        synchronized(this) {
            val evicted = engine.clear()
            detailStore.clear()
            commentsStore.clear()
        }
    }

    fun size(): Int = synchronized(this) { engine.size().toInt() }
}

interface DetailCacheLike {
    fun get(key: String): FeedDetail?
    fun isFresh(key: String, now: Long = System.currentTimeMillis()): Boolean
    fun put(key: String, detail: FeedDetail, now: Long = System.currentTimeMillis())
    fun remove(key: String)
}
