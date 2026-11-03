package feedhub.app.feed

import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.present.RustPresentPaging
import java.nio.file.Path

/**
 * 信息流的"上一批"。
 *
 * 持久化格式、过期校验、临时文件写入与原子替换均下沉至 Rust（[RustPresentPaging]），
 * 磁盘文件逐字节与 Kotlin `FeedJson.encodeToString(Stored.serializer(), Stored(savedAt, items.take(120)))` 兼容。
 */
class FeedSnapshot(private val file: Path, private val now: () -> Long = System::currentTimeMillis) {
    /** 读上一批；没有、坏了、太旧都给空列表。 */
    fun load(): List<FeedItem> {
        val strings = RustPresentPaging.loadSnapshot(file.toString(), now())
        if (strings.isEmpty()) return emptyList()
        return runCatching {
            strings.map { FeedJson.decodeFromString(FeedItem.serializer(), it) }
        }.getOrElse { emptyList() }
    }

    /** 存这一批（最上面 [MAX_ITEMS] 条）；先写临时文件再换名，写一半崩了也不会留下坏文件。失败不抛。 */
    fun save(items: List<FeedItem>) {
        runCatching {
            val itemsJson = items.take(MAX_ITEMS).map { FeedJson.encodeToString(FeedItem.serializer(), it) }
            RustPresentPaging.saveSnapshot(file.toString(), now(), itemsJson)
        }
    }

    companion object {
        const val MAX_ITEMS = 120
        const val MAX_AGE_MS = 7L * 24 * 3600 * 1000

        /** 首页 / 关注各一个文件。 */
        fun fileName(kind: feedhub.core.FeedKind): String =
            RustPresentPaging.snapshotFileName(kind.name)
    }
}
