package feedhub.cli

import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteStatement
import androidx.sqlite.driver.bundled.BundledSQLiteDriver
import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.core.subcommands
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.types.int
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.Platform
import feedhub.core.local.LocalFiles
import kotlinx.serialization.json.JsonPrimitive
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * `library favorites` / `library history`：**只读**读 App 的同一个 SQLite
 * （桌面的 `~/.config/feedhub/library.db`，路径规则和 App 一样走 [LocalFiles.home]，认 `FEEDHUB_HOME`）。
 *
 * - 以 **SQLITE_OPEN_READONLY** 打开，不加写锁：App 开着（自己正在写）时也能读；
 * - WAL 模式下读到的是已提交的最新内容（读连接看 WAL，不需要 checkpoint）；
 * - 万一只读打开失败（例如 App 崩过、留下 `-wal` 却没有 `-shm`），退回到"把 db / wal / shm 复制到临时目录再读"，
 *   **绝不写原文件**，读完把临时副本删掉；
 * - 过滤：`--platform` / `--since` / `--limit` / `--query`，`--query` 是**本地子串匹配**
 *   （App 写库时算好的 `search_text`：标题 / 摘要 / 作者 / 圈子），**一个网络请求都不发**；
 * - 18+ 整条隐藏、取消收藏 / 删历史的墓碑（`deleted`）不出现——和 App 的读取同一口径；
 * - 本期**不做**收藏 / 取消收藏，也不读行为数据（`exposure` / `seen_item`）。
 */
class LibraryCmd : CliktCommand(name = "library") {
    override fun help(context: Context) =
        "本地资料库（只读）：favorites 收藏、history 历史。读 App 的同一个 library.db，不发网络请求"

    override fun run() = Unit
}

/** 一页收藏 / 历史的共同参数。 */
private const val DEFAULT_LIMIT = 20
private const val MAX_LIMIT = 500

private fun checkLimit(limit: Int): Int {
    if (limit < 1) throw CliFailure(ErrorCode.BAD_INPUT, "--limit 要 ≥ 1（收到 $limit）")
    if (limit > MAX_LIMIT) throw CliFailure(ErrorCode.BAD_INPUT, "--limit 最多 $MAX_LIMIT（收到 $limit），要更多请分页查")
    return limit
}

class LibraryFavoritesCmd : JsonCmd("favorites") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val platformKey by option("--platform", help = "只看这个平台（如 bili）")
    private val since by option("--since", help = "从这个时间起（例如 24h / 7d / 2026-10-01 / Unix 秒）")
    private val query by option("--query", help = "在标题 / 作者 / 正文摘要里做本地子串匹配（不发请求）")
    private val limit by option("--limit", help = "最多几条（默认 $DEFAULT_LIMIT，最多 $MAX_LIMIT）").int().default(DEFAULT_LIMIT)

    override fun help(context: Context) = "按收藏时间倒序列出收藏（只读，本地 SQLite；--json 时每行一条）"

    override fun run() {
        configure(session)
        val max = checkLimit(limit)
        val platform = platformKey?.let(::platformOf)
        val sinceMs = since?.let { parseSince(it) }
        LibraryStore.open(LocalFiles.home.resolve("library.db")).use { store ->
            val rows = store.favorites(platform, sinceMs, query, max)
            if (session.json) {
                for (row in rows) {
                    session.emit(
                        "favorite",
                        FeedJson.encodeToJsonElement(FeedItem.serializer(), row.item),
                        buildMap {
                            put("addedAt", JsonPrimitive(row.addedAt))
                            row.folderId?.let { put("folder", JsonPrimitive(it)) }
                        },
                    )
                }
                session.emit(
                    "done",
                    extra = mapOf(
                        "count" to JsonPrimitive(rows.size),
                        "command" to JsonPrimitive("library favorites"),
                        "requests" to JsonPrimitive(session.requests.get()),
                    ),
                )
            } else {
                rows.forEachIndexed { i, row ->
                    session.say(formatItem(row.item, i + 1))
                    session.say("   收藏于 ${timeText(row.addedAt)}" + (row.folderId?.let { "（收藏夹 #$it）" } ?: ""))
                }
                session.say("共 ${rows.size} 条（只读，最多 $max 条）")
            }
        }
    }
}

class LibraryHistoryCmd : JsonCmd("history") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val platformKey by option("--platform", help = "只看这个平台（如 bili）")
    private val since by option("--since", help = "从这个时间起（例如 24h / 7d / 2026-10-01 / Unix 秒）")
    private val query by option("--query", help = "在标题 / 作者 / 正文摘要里做本地子串匹配（不发请求）")
    private val limit by option("--limit", help = "最多几条（默认 $DEFAULT_LIMIT，最多 $MAX_LIMIT）").int().default(DEFAULT_LIMIT)
    private val failedOnly by option("--failed-only", help = "只看没看完的（有播放进度、且进度 < 时长）").flag()

    override fun help(context: Context) = "按看过的时间倒序列出历史（只读，本地 SQLite；--json 时每行一条）"

    override fun run() {
        configure(session)
        val max = checkLimit(limit)
        val platform = platformKey?.let(::platformOf)
        val sinceMs = since?.let { parseSince(it) }
        LibraryStore.open(LocalFiles.home.resolve("library.db")).use { store ->
            val rows = store.history(platform, sinceMs, query, max)
                .filter {
                    if (!failedOnly) return@filter true
                    val progress = it.progressMs ?: 0
                    val duration = it.durationMs ?: 0
                    progress > 0 && duration > 0 && progress < duration
                }
            if (session.json) {
                for (row in rows) {
                    session.emit(
                        "history",
                        FeedJson.encodeToJsonElement(FeedItem.serializer(), row.item),
                        buildMap {
                            put("visitedAt", JsonPrimitive(row.visitedAt))
                            row.progressMs?.let { put("progressMs", JsonPrimitive(it)) }
                            row.durationMs?.let { put("durationMs", JsonPrimitive(it)) }
                        },
                    )
                }
                session.emit(
                    "done",
                    extra = mapOf(
                        "count" to JsonPrimitive(rows.size),
                        "command" to JsonPrimitive("library history"),
                        "requests" to JsonPrimitive(session.requests.get()),
                    ),
                )
            } else {
                rows.forEachIndexed { i, row ->
                    session.say(formatItem(row.item, i + 1))
                    val progress = if (row.progressMs != null && row.durationMs != null && row.durationMs > 0) {
                        "，看到 %d%%".format(row.progressMs * 100 / row.durationMs)
                    } else {
                        ""
                    }
                    session.say("   看过于 ${timeText(row.visitedAt)}$progress")
                }
                session.say("共 ${rows.size} 条（只读，最多 $max 条）")
            }
        }
    }
}

private val libraryTimeFmt: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm").withZone(ZoneId.systemDefault())

internal fun timeText(epochMs: Long): String = libraryTimeFmt.format(Instant.ofEpochMilli(epochMs))

/**
 * 返回**截止时间**（Unix 毫秒），比它新（>=）的才算。
 */
internal fun parseSince(text: String, now: Long = System.currentTimeMillis()): Long {
    val t = text.trim()
    if (t.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "--since 不能为空")
    Regex("(\\d+)([smhdw])").matchEntire(t)?.let { m ->
        val n = m.groupValues[1].toLongOrNull() ?: 0
        val unit = when (m.groupValues[2]) {
            "s" -> 1_000L
            "m" -> 60_000L
            "h" -> 3_600_000L
            "d" -> 86_400_000L
            "w" -> 7 * 86_400_000L
            else -> 0L
        }
        return now - n * unit
    }
    Regex("\\d{4}-\\d{2}-\\d{2}([ T]\\d{2}:\\d{2}(:\\d{2})?)?").matchEntire(t)?.let { m ->
        val zone = ZoneId.systemDefault()
        val instant = runCatching {
            if (m.groupValues[1].isEmpty()) {
                LocalDate.parse(t).atStartOfDay(zone).toInstant()
            } else {
                val normalized = t.replace(' ', 'T')
                LocalDateTime.parse(if (normalized.length == 16) "$normalized:00" else normalized).atZone(zone).toInstant()
            }
        }.getOrNull() ?: throw CliFailure(ErrorCode.BAD_INPUT, "--since 的时间看不懂：$text（例：2026-10-01 或 2026-10-01 12:30）")
        return instant.toEpochMilli()
    }
    t.toLongOrNull()?.let { n ->
        // 10 位当秒、13 位当毫秒；都按"这个时刻之后"算
        return if (n > 100_000_000_000L) n else n * 1000
    }
    throw CliFailure(ErrorCode.BAD_INPUT, "--since 看不懂：$text（例：24h / 7d / 2026-10-01 / Unix 秒）")
}

// ---------------------------------------------------------------- 只读打开 SQLite

data class FavoriteRow(val item: FeedItem, val addedAt: Long, val folderId: String?)
data class HistoryRow(val item: FeedItem, val visitedAt: Long, val progressMs: Long?, val durationMs: Long?)

/**
 * 只读的资料库。**不写、不加写锁**；打开方式见类注释（只读优先，退回到临时副本）。
 *
 * 为什么不复用 `app` 里的 `SqliteLibrary`：那是 Compose 界面模块里的类（`app.library`），cli 不依赖 `app`
 * ，所以这里只读它写下的**同一份表结构**。口径一致由
 * `app/src/desktopTest/.../CliLibraryReadTest.kt` 里"真 SqliteLibrary 写、cli 读"的对拍测试守着。
 */
class LibraryStore private constructor(
    private val conn: SQLiteConnection,
    /** 退回到副本时：临时目录，[close] 时整个删掉。 */
    private val tempDir: Path? = null,
) : AutoCloseable {

    override fun close() {
        runCatching { conn.close() }
        tempDir?.let { runCatching { it.toFile().deleteRecursively() } }
    }

    fun favorites(platform: Platform?, sinceMs: Long?, query: String?, limit: Int): List<FavoriteRow> {
        val version = userVersion()
        val where = Where(version)
        where.platform(platform)
        if (sinceMs != null) where.add("added_at >= ?", sinceMs)
        where.search(query)
        return rows(
            "SELECT item_json, added_at, folder_id FROM favorite ${where.sql()} " +
                "ORDER BY added_at DESC, rowid DESC LIMIT ?",
            *where.args, limit,
        ) { stmt ->
            val item = decode(stmt.getText(0)) ?: return@rows null
            FavoriteRow(item, stmt.getLong(1), if (stmt.isNull(2)) null else stmt.getLong(2).toString())
        }.filterNotNull()
    }

    fun history(platform: Platform?, sinceMs: Long?, query: String?, limit: Int): List<HistoryRow> {
        val version = userVersion()
        val where = Where(version)
        where.platform(platform)
        if (sinceMs != null) where.add("visited_at >= ?", sinceMs)
        where.search(query)
        return rows(
            "SELECT item_json, visited_at, progress_ms, duration_ms FROM history ${where.sql()} " +
                "ORDER BY visited_at DESC, rowid DESC LIMIT ?",
            *where.args, limit,
        ) { stmt ->
            val item = decode(stmt.getText(0)) ?: return@rows null
            HistoryRow(
                item,
                stmt.getLong(1),
                if (stmt.isNull(2)) null else stmt.getLong(2),
                if (stmt.isNull(3)) null else stmt.getLong(3),
            )
        }.filterNotNull()
    }

    private fun userVersion(): Int =
        rows("PRAGMA user_version") { it.getLong(0).toInt() }.firstOrNull() ?: 0

    private fun decode(json: String): FeedItem? =
        runCatching { FeedJson.decodeFromString(FeedItem.serializer(), json) }.getOrNull()

    private fun <T> rows(sql: String, vararg binds: Any?, map: (SQLiteStatement) -> T?): List<T> {
        val out = ArrayList<T>()
        conn.prepare(sql).use { stmt ->
            binds.forEachIndexed { i, v ->
                val n = i + 1
                when (v) {
                    null -> stmt.bindNull(n)
                    is String -> stmt.bindText(n, v)
                    is Int -> stmt.bindLong(n, v.toLong())
                    is Long -> stmt.bindLong(n, v)
                    else -> error("不支持的 SQL 参数类型 ${v::class}")
                }
            }
            while (stmt.step()) map(stmt)?.let { out += it }
        }
        return out
    }

    /** 和 App 的 `Where` 同一口径：墓碑 / 18+ 按表版本加条件（老库没有这些列）。 */
    private class Where(version: Int) {
        private val parts = mutableListOf<String>()
        private val params = mutableListOf<Any?>()
        val args: Array<Any?> get() = params.toTypedArray()

        init {
            if (version >= 4) parts += "deleted = 0"
            if (version >= 3) parts += "is_adult = 0"
        }

        fun add(sql: String, vararg a: Any?) {
            parts += sql
            params.addAll(a)
        }

        fun platform(p: Platform?) {
            if (p != null) add("platform = ?", p.key)
        }

        fun search(q: String?) {
            val s = q?.trim().orEmpty()
            if (s.isNotEmpty()) {
                add(
                    "search_text LIKE ? ESCAPE '\\'",
                    "%" + s.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%",
                )
            }
        }

        fun sql(): String = if (parts.isEmpty()) "" else "WHERE " + parts.joinToString(" AND ")
    }

    companion object {
        /** sqlite3 的 `SQLITE_OPEN_READONLY`（`androidx.sqlite` 的 `open(name, flags)` 直接透传给 sqlite3_open_v2）。 */
        private const val OPEN_READONLY: Int = 0x0000_0001

        /** 打开桌面的资料库（[path] 一般是 `LocalFiles.home/library.db`）。 */
        fun open(path: Path): LibraryStore {
            if (!Files.isRegularFile(path)) {
                throw CliFailure(
                    ErrorCode.BAD_INPUT,
                    "还没有本地资料库（$path 不存在）：先在 App 里收藏 / 看过内容再来；" +
                        "如果 App 装在别处，用 FEEDHUB_HOME 指向它的配置目录。",
                )
            }
            // 1) 只读打开（首选）：WAL 下能读到最新已提交的内容，不会拿写锁
            runCatching { openReadOnly(path) }.let { direct ->
                if (direct.isSuccess) return LibraryStore(direct.getOrThrow())
                val first = direct.exceptionOrNull()
                // 2) 退路：整份复制到临时目录再读（只读打开失败通常是 -wal 在、-shm 不在：App 崩溃过）
                runCatching { openSnapshot(path) }.let { snap ->
                    if (snap.isSuccess) {
                        val (conn, dir) = snap.getOrThrow()
                        return LibraryStore(conn, tempDir = dir)
                    }
                    throw CliFailure(
                        ErrorCode.BAD_INPUT,
                        "打不开资料库 $path：${snap.exceptionOrNull()?.message ?: first?.message}",
                    )
                }
            }
        }

        private fun openReadOnly(path: Path): SQLiteConnection {
            val conn = BundledSQLiteDriver().open(path.toString(), OPEN_READONLY)
            conn.prepare("PRAGMA busy_timeout=3000").use { it.step() }
            return conn
        }

        /**
         * 把 `library.db` 和它的 `-wal` / `-shm` 复制到一个临时目录，打开副本（副本是临时的，读写都无所谓）。
         * 复制期间 App 正在写也不要紧：WAL 的帧带校验和，读到的要么是完整的已提交内容、要么更旧一点，
         * 不会读到半条。
         */
        private fun openSnapshot(path: Path): Pair<SQLiteConnection, Path> {
            val dir = Files.createTempDirectory("feedhub-cli-lib")
            val copy = dir.resolve("library.db")
            Files.copy(path, copy, StandardCopyOption.REPLACE_EXISTING)
            for (suffix in listOf("-wal", "-shm")) {
                val src = path.resolveSibling(path.fileName.toString() + suffix)
                if (Files.isRegularFile(src)) {
                    runCatching { Files.copy(src, dir.resolve(copy.fileName.toString() + suffix), StandardCopyOption.REPLACE_EXISTING) }
                }
            }
            val conn = BundledSQLiteDriver().open(copy.toString())
            conn.prepare("PRAGMA busy_timeout=3000").use { it.step() }
            return conn to dir
        }
    }
}
