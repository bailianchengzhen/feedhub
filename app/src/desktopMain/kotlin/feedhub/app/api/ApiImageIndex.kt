package feedhub.app.api

import feedhub.core.Platform
import feedhub.rust.RustAppApi

/**
 * **`/image` 的图片清单缓存**。
 *
 * 内存 LRU 缓存与线程安全完全由 Rust 端 `ApiImageIndex` 维护。
 */
internal class ApiImageIndex(
    detailCapacity: Int = 16,
    commentCapacity: Int = 64,
) {
    private val inner = RustAppApi.createImageIndex(detailCapacity, commentCapacity)

    @Synchronized
    fun detail(platform: Platform, id: String): DetailImages? =
        inner.detail(platform.key, id)?.toModel()

    @Synchronized
    fun putDetail(platform: Platform, id: String, images: DetailImages) {
        inner.putDetail(platform.key, id, images.toFfi())
    }

    @Synchronized
    fun comment(platform: Platform, id: String, commentId: String): ContentImages? =
        inner.comment(platform.key, id, commentId)?.toModel()

    /** 只记**有图**的评论（没图的记了只会把有用的挤出去）。 */
    @Synchronized
    fun putComment(platform: Platform, id: String, commentId: String, images: ContentImages) {
        if (images.count == 0) return
        inner.putComment(platform.key, id, commentId, images.toFfi())
    }

    /** 现在记着几条（测试 / 调试用）。 */
    @Synchronized
    fun size(): Pair<Int, Int> {
        val s = inner.size()
        return s.details to s.comments
    }
}
