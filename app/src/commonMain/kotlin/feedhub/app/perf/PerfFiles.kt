package feedhub.app.perf

import feedhub.perf.RustPerf
import java.io.File
import java.util.TimeZone

/** 记录文件保留几天（任务：只留最近 7 天）。 */
const val PERF_KEEP_DAYS: Int = 7

/** 一天一个文件的名字（`perf-YYYYMMDD.jsonl`）。 */
internal const val PERF_FILE_PREFIX = "perf-"

/** 记录目录现在多大、有几个文件（设置页那一行小字用）。[dirPath] 为 null = 这台设备 / 这个构建不记录。 */
data class PerfStatus(val dirPath: String?, val files: Int, val bytes: Long) {
    val supported: Boolean get() = dirPath != null

    companion object {
        val Unsupported = PerfStatus(null, 0, 0)
    }
}

/**
 * 记录文件的存放：**JSON Lines、按天一个文件、只留最近 [keepDays] 天**。
 *
 * 底层文件 IO、轮转淘汰、缓冲写入已完全下沉至 Rust (`feedhub-core::perf::files`)，此处为纯委托。
 *
 * @param dir 目录（安卓是 `context.getExternalFilesDir("perf")`；拿不到外置存储时是 null，此时 [append] 什么都不做）。
 * @param now 时钟（测试注入，用来拨到第二天验轮转）。
 */
class PerfFileStore(
    private val dir: () -> File?,
    private val now: () -> Long = System::currentTimeMillis,
    private val keepDays: Int = PERF_KEEP_DAYS,
) {
    private val core = RustPerf.createFileStore(keepDays)

    /** 上一次写盘失败的原因（磁盘满 / 没权限）；成功一次就清掉。设置页可以显示它。 */
    val lastError: String?
        get() = core.lastError()

    /** 追加一行（自动补换行 + flush）。返回写成功没有。 */
    fun append(line: String): Boolean {
        val d = dir()?.path
        val nowMs = now()
        val offset = offsetSec(nowMs)
        val keepFromMs = nowMs - (keepDays - 1) * 86_400_000L
        val keepFromOffset = offsetSec(keepFromMs)
        return core.append(d, line, nowMs, offset, keepFromOffset)
    }

    /** 删掉 [keepDays] 天以前的文件，返回删了几个。 */
    fun prune(): Int {
        val d = dir()?.path
        val nowMs = now()
        val keepFromMs = nowMs - (keepDays - 1) * 86_400_000L
        val keepFromOffset = offsetSec(keepFromMs)
        return core.prune(d, nowMs, keepFromOffset).toInt()
    }

    /** 现在目录里有哪些记录文件（按名字升序）。 */
    fun files(): List<File> {
        val d = dir() ?: return emptyList()
        val names = core.files(d.path)
        return names.map { File(d, it) }
    }

    /** 目录 + 文件数 + 总字节（给设置页）。 */
    fun status(): PerfStatus {
        val d = dir() ?: return PerfStatus.Unsupported
        val st = core.status(d.path)
        return PerfStatus(st.dirPath, st.files, st.bytes)
    }

    /** 清空记录（删掉所有记录文件），返回删了几个；同时把打开着的文件关掉。 */
    fun clear(): Int {
        val d = dir()?.path
        return core.clear(d).toInt()
    }

    /** 关掉打开着的文件（退出 / 关开关时调；不删文件）。 */
    fun close() {
        core.shutdown()
    }

    private fun offsetSec(ms: Long): Int =
        TimeZone.getDefault().getOffset(ms) / 1000

    companion object {
        /** 安卓记录目录的名字（`getExternalFilesDir` 下面这一层）。 */
        const val ANDROID_DIR_NAME = "perf"
    }
}

