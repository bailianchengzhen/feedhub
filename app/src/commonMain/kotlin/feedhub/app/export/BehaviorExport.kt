package feedhub.app.export

import feedhub.app.embed.BehaviorSnapshotLimits
import feedhub.app.embed.collectBehaviorSnapshot
import feedhub.app.library.ItemEmbedding
import feedhub.app.library.Library
import feedhub.app.library.blockKey
import feedhub.core.FeedItem
import feedhub.rust.RustEmbed
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * 行为数据的导出：给**离线回放**准备的数据入口。
 *
 * 只有用户点设置页「导出」时才会跑（不自动导出、不起定时器）。产物是**一个 JSON Lines 文件**，
 * 每行一个对象，`table` 字段说明它属于哪张表：`meta` / `seen_item` / `exposure` / `history` / `favorite` /
 * `signal` / `profile` / `item_embedding`。离线工具是 `tools/embed/replay.py`。
 *
 * **不含任何 Cookie / token / URL**：
 * - 内容只导出"回放用得上的字段"（[sanitizedItem]：平台、id、类型、标题、摘要、作者名、圈子、时间、语言、18+），
 *   `item_json` 里的媒体直链 / 视频播放地址 / 外链**一律不写**——平台的直链经常带签名参数（token 的一种）；
 * - 曝光 / 历史 / 收藏只有 key / 平台 / 时间 / 停留，没有请求头、没有 Cookie 罐的东西；
 * - 单元测试 `BehaviorExportTest` 守住"导出的文本里没有 cookie / token / http 这些字样"。
 *
 * 落盘位置由平台层决定（[BehaviorExportTarget]）：安卓 `getExternalFilesDir(null)/export/`，桌面 `~/.config/feedhub/export/`。
 *
 * 逐字节和原来的 Kotlin 实现对齐）。
 * 这个文件只做三件事：从库里读那几张表（宿主 I/O）、拼一份过边界的输入、把行写出去。
 */
interface BehaviorExportTarget {
    /** 这个组装能不能导出（安卓拿不到外置存储时 false；演示 / 测试不给就是 null）。 */
    val available: Boolean

    /** 给人看的目录（设置页显示；不含用户主目录之外的敏感路径）。 */
    val location: String

    /** 写一个文件（UTF-8，每行一个 JSON），返回落盘的绝对路径；失败返回 null。 */
    suspend fun write(name: String, lines: List<String>): String?
}

/** 一次导出的结果（设置页显示"已导出 N 行 / 大小"）。 */
data class ExportSummary(
    val path: String,
    val lines: Int,
    val bytes: Long,
    /** 每张表各几行（`meta` 是文件头那一行）。 */
    val tables: Map<String, Int>,
)

/** 导出文件的格式版本（`meta` 行里的 `schema`）；改了字段就加一，回放工具据此报错而不是猜。 */
val EXPORT_SCHEMA_VERSION: Int get() = RustEmbed.exportSchemaVersion

/** 导出时摘要最多留多少字（回放只要语义，不需要整篇；也避免文件过大）。 */
val EXPORT_SUMMARY_MAX_CHARS: Int get() = RustEmbed.exportSummaryMaxChars

/**
 * 把库里这几张表读出来写成一个 JSON Lines 文件，返回路径 / 行数 / 大小。
 *
 * 读的顺序：`seen_item`（最新在前）→ `exposure` → `history` → `favorite` → `item_embedding`。
 * 同一份数据可以重复导出（文件名带时间戳，不会覆盖上一次）。
 */
suspend fun exportBehaviorData(
    library: Library,
    target: BehaviorExportTarget,
    now: Long = System.currentTimeMillis(),
    /** 设置里的「屏蔽词」（只导出"命中过"这件事，**不导出词本身**；外出模式的关键词不算口味，不在这里）。 */
    blockWords: List<String> = emptyList(),
): Result<ExportSummary> {
    if (!target.available) return Result.failure(IllegalStateException("这个设备 / 这次组装不能导出（拿不到外置存储目录）"))
    val limits = BehaviorSnapshotLimits.EXPORT
    val snapshot = collectBehaviorSnapshot(library, blockWords, limits)
    val embeddings = library.embeddings(limit = limits.favorites)
    val input = RustEmbed.exportInput(
        seen = snapshot.seen.map {
            RustEmbed.exportSeen(it.item, it.firstSeenAt, it.feedKind.key, it.batchId, it.platformRank)
        },
        exposures = snapshot.exposures.map {
            RustEmbed.exportExposure(it.key, it.platform.key, it.surface.key, it.exposedAt, it.lastSeenAt, it.dwellMs)
        },
        history = snapshot.history.map {
            RustEmbed.exportHistory(it.item, it.visitedAt, it.progressMs, it.durationMs)
        },
        favorites = snapshot.favorites.map {
            RustEmbed.exportFavorite(it.item, it.folder, it.addedAt)
        },
        embeddings = embeddings.map {
            RustEmbed.embeddingRow(it.key, it.model, it.dim, it.vec, it.createdAt, it.source)
        },
        blocked = snapshot.blockedAuthors.map {
            RustEmbed.exportBlocked(it.user.blockKey(), it.blockedAt)
        },
        blockWords = snapshot.blockWords,
        now = now,
    )
    val out = RustEmbed.export(input)
    val path = target.write("behavior-$now.jsonl", out.lines)
        ?: return Result.failure(IllegalStateException("写文件失败（磁盘满 / 没权限），看 logcat 里的 [导出]"))
    return Result.success(ExportSummary(path, out.lines.size, out.bytes, out.tables))
}

/**
 * 导出用的内容：**只留文字和分类**，媒体 / 视频 / 外链地址全部丢掉。
 *
 * 现在生产路径由 Rust 拼（`export::push_sanitized_item`，逐字节一致）；这个函数留着给测试 / 工具按同一个口径
 * 看一条内容长什么样（**不要**在导出路径上再调它）。
 */
fun sanitizedItem(item: FeedItem): JsonObject = buildJsonObject {
    put("platform", item.platform.key)
    put("id", item.id)
    put("kind", item.kind.name)
    item.title?.let { put("title", it) }
    item.summary?.let { put("summary", it.take(EXPORT_SUMMARY_MAX_CHARS)) }
    item.author?.let { put("author", it.name) }
    item.publishedAt?.let { put("published_at", it) }
    item.language?.let { put("language", it) }
    item.community?.let { put("community", it) }
    item.reason?.let { put("reason", it) }
    put("adult", item.adult)
}

private const val B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

/**
 * 标准 base64（带 `=` 补齐）。
 *
 * 导出文件里的 `vec` / `positive` / `negative` 现在由 Rust 编（同一个字母表）；这个函数留给测试 /
 * 回放工具反解用（`decodeBase64` 是它的对偶）。
 */
fun encodeBase64(bytes: ByteArray): String {
    if (bytes.isEmpty()) return ""
    val sb = StringBuilder((bytes.size + 2) / 3 * 4)
    var i = 0
    while (i + 2 < bytes.size) {
        val n = ((bytes[i].toInt() and 0xFF) shl 16) or ((bytes[i + 1].toInt() and 0xFF) shl 8) or (bytes[i + 2].toInt() and 0xFF)
        sb.append(B64[(n ushr 18) and 0x3F])
        sb.append(B64[(n ushr 12) and 0x3F])
        sb.append(B64[(n ushr 6) and 0x3F])
        sb.append(B64[n and 0x3F])
        i += 3
    }
    when (bytes.size - i) {
        1 -> {
            val n = (bytes[i].toInt() and 0xFF) shl 16
            sb.append(B64[(n ushr 18) and 0x3F])
            sb.append(B64[(n ushr 12) and 0x3F])
            sb.append("==")
        }
        2 -> {
            val n = ((bytes[i].toInt() and 0xFF) shl 16) or ((bytes[i + 1].toInt() and 0xFF) shl 8)
            sb.append(B64[(n ushr 18) and 0x3F])
            sb.append(B64[(n ushr 12) and 0x3F])
            sb.append(B64[(n ushr 6) and 0x3F])
            sb.append('=')
        }
    }
    return sb.toString()
}

/** 测试 / 回放工具反解用；和 [encodeBase64] 成对（导出的向量是 f16 小端原始字节）。 */
fun decodeBase64(text: String): ByteArray {
    val clean = text.trim().trimEnd('=')
    val out = ByteArray(clean.length * 3 / 4)
    var n = 0
    var acc = 0
    var bits = 0
    for (c in clean) {
        val v = B64.indexOf(c)
        require(v >= 0) { "不是合法的 base64 字符：$c" }
        acc = (acc shl 6) or v
        bits += 6
        if (bits >= 8) {
            bits -= 8
            out[n++] = ((acc ushr bits) and 0xFF).toByte()
        }
    }
    return if (n == out.size) out else out.copyOf(n)
}

/**
 * 一行 JSON 里出现的表名（给测试 / 工具用）：只认导出的这几种，认不出来返回 null。
 * 这里不做完整的 JSON 解析——导出工具只需要 `table` 字段，而它就是每行第一个键。
 */
fun tableOf(line: String): String? {
    val prefix = "{\"table\":\""
    if (!line.startsWith(prefix)) return null
    val end = line.indexOf('"', prefix.length)
    return if (end <= prefix.length) null else line.substring(prefix.length, end)
}

/** 一行 `item_embedding` 的向量长度检查（测试用：确保 dim 和 base64 解出来的字节对得上）。 */
fun ItemEmbedding.vectorByteCountOk(): Boolean = vec.size == dim * 2
