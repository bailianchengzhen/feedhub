package feedhub.app.embed

import feedhub.app.library.ItemEmbedding
import feedhub.app.library.Library
import uniffi.feedhub_ffi.FfiEmbedRow
import uniffi.feedhub_ffi.FfiEmbedStore

/**
 * 把 [Library] 接给 Rust 调度器。
 *
 * 为什么调度器不直接拿 `RustLibrary`：
 * - **测试要能用 `MemoryLibrary`**（不建真库、不落盘）：生产走 `RustLibrary`、测试走内存实现，同一个接口；
 * - 这几个操作**全是同步的**（`Library.embeddingKeys` / `isFavorite` / `insertEmbeddings`），Rust 在
 *   `next_step` / `finish_batch` 里同步回调；调用方在 `Dispatchers.IO` 上调那两个 Rust 方法（见调度器），
 *   所以这里不会把数据库 I/O 放到主线程上。
 *
 * 一次一批：查重是一批 key 一次、写是一批行一次（不是一条一次过 FFI）。
 */
class LibraryEmbedStore(private val library: Library) : FfiEmbedStore {
    override fun embeddingKeys(keys: List<String>, model: String): List<String> =
        library.embeddingKeys(keys, model).toList()

    override fun isFavorite(keys: List<String>): List<Boolean> = keys.map { library.isFavorite(it) }

    override fun insertEmbeddings(rows: List<FfiEmbedRow>) {
        if (rows.isEmpty()) return
        library.insertEmbeddings(
            rows.map {
                ItemEmbedding(
                    key = it.key,
                    model = it.model,
                    dim = it.dim,
                    vec = it.vec,
                    createdAt = it.createdAt,
                    source = it.source,
                )
            },
        )
    }
}
