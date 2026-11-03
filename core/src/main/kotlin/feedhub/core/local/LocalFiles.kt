package feedhub.core.local

import feedhub.core.FeedCookieJar
import feedhub.core.KeyValueStore
import feedhub.core.PersistentCookieJar
import feedhub.core.SourceContext
import feedhub.core.enableSystemProxies
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.TimeUnit

/**
 * 电脑上（cli 和在线冒烟测试）用的本地文件：
 * - `cookies.json`：所有平台的 Cookie，权限 600
 * - `state.json`：[KeyValueStore]
 *
 * 目录优先级：环境变量 `FEEDHUB_HOME` > `-Dfeedhub.home=…` > `$XDG_CONFIG_HOME/feedhub` > `~/.config/feedhub`。
 * 刻意放在仓库外面，避免 Cookie 被提交。
 */
object LocalFiles {
    val home: Path by lazy {
        System.getenv("FEEDHUB_HOME")?.takeIf { it.isNotBlank() }?.let { Paths.get(it) }
            // `-Dfeedhub.home=…`：测试和"把配置放在别处"的启动脚本用（环境变量优先）
            ?: System.getProperty("feedhub.home")?.takeIf { it.isNotBlank() }?.let { Paths.get(it) }
            ?: System.getenv("XDG_CONFIG_HOME")?.takeIf { it.isNotBlank() }?.let { Paths.get(it, "feedhub") }
            ?: Paths.get(System.getProperty("user.home"), ".config", "feedhub")
    }

    val cookiesFile: Path get() = home.resolve("cookies.json")
    val stateFile: Path get() = home.resolve("state.json")

    /** 用本地文件组装一个 [SourceContext]。[extraInterceptors] 例如 cli 的录制拦截器。 */
    fun context(extraInterceptors: List<Interceptor> = emptyList()): SourceContext {
        // 「跟随系统」这一档要 JVM 真的去问系统 / 环境变量里的代理；java.net.useSystemProxies 只在
        // 启动时读一次，所以必须在建下面这个客户端之前设。cli 和桌面 App 都经过这里。
        enableSystemProxies()
        val jar = FileCookieJar(cookiesFile)
        val http = OkHttpClient.Builder()
            .cookieJar(jar)
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .apply { extraInterceptors.forEach { addInterceptor(it) } }
            .build()
        return SourceContext(http, jar, FileKeyValueStore(stateFile))
    }
}

private val fileJson = Json { prettyPrint = true; ignoreUnknownKeys = true }

/**
 * 读整个文件当 UTF-8 文本。**不用 `Files.readString` / `Files.writeString`**：那是 Java 11 的 API，
 * 安卓的 core-oj 里没有。core 两端共用，只能用 Java 8 的写法。
 */
internal fun readUtf8(path: Path): String = String(Files.readAllBytes(path), Charsets.UTF_8)

internal fun writePrivate(path: Path, text: String) {
    Files.createDirectories(path.parent)
    val tmp = path.resolveSibling(path.fileName.toString() + ".tmp")
    Files.write(tmp, text.toByteArray(Charsets.UTF_8)) // 不用 Files.writeString：Java 11 的，安卓 core-oj 里没有
    try {
        Files.setPosixFilePermissions(tmp, PosixFilePermissions.fromString("rw-------"))
    } catch (_: UnsupportedOperationException) {
    } catch (_: java.io.IOException) {
    }
    try {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (_: AtomicMoveNotSupportedException) {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING)
    }
}

/**
 * 存在 JSON 文件里的 Cookie 罐（`cookies.json`）。
 *
 * 600 权限、临时文件 + 原子替换、
 * 坏文件留底（`cookies.json.bad-<毫秒>`）、`cookies.json.lock` 上的跨进程锁
 * 全在 `crates/feedhub-core/src/cookies/file.rs`（`fcntl` 记录锁，和旧版 Kotlin 的
 * `FileChannel.lock()` 是同一套，老 cli 进程和新 App 能同时开着、互相不丢改动）。
 */
class FileCookieJar private constructor(
    private val delegate: PersistentCookieJar,
) : FeedCookieJar by delegate, feedhub.core.ForwardingRustJar {
    constructor(file: Path) : this(PersistentCookieJar(file.toFile()))

    override val innerJar: FeedCookieJar get() = delegate
}

/**
 * `state.json`（[KeyValueStore]）：App 的设置、YouTube 的 token、cli 的限速状态……都在这一份里。
 *
 * **写回是"读改写合并"，不是整份覆盖**：桌面 App 和 `./feedhub` 是两个进程，各自在启动时读一份，
 * 之后只改自己动过的键。原来的 `put` 把内存里那一整份写回去，于是后写的进程会把先写的设置 / 登录态盖掉。
 * 现在每次写都先拿 `state.json.lock`（[FileLocks]，和 App 是同一把），再读一遍磁盘、**只改这一个键**、写回。
 * 内存那份也跟着换成合并后的内容，`get` 之后读到的就是最新的。
 */
class FileKeyValueStore(private val file: Path) : KeyValueStore {
    private val serializer = MapSerializer(String.serializer(), String.serializer())
    private val map: MutableMap<String, String> = readDisk().toMutableMap()

    /** 内存这份对应磁盘的哪一版（修改时间 + 大小）：别的进程改过就整份重读，`get` 不会读到旧值。 */
    private var stamp: String = stampOf(file)

    private fun readDisk(): Map<String, String> =
        if (Files.exists(file)) fileJson.decodeFromString(serializer, readUtf8(file)) else emptyMap()

    private fun stampOf(f: Path): String =
        if (Files.exists(f)) "${Files.getLastModifiedTime(f).toMillis()}:${Files.size(f)}" else "missing"

    /** 磁盘被别的进程改过（`put` 写了新的一份 / 被删了）就重读。没变时只多一次 `stat`。 */
    private fun refreshIfChanged() {
        val now = stampOf(file)
        if (now == stamp) return
        map.clear()
        map.putAll(readDisk())
        stamp = now
    }

    @Synchronized
    override fun get(key: String): String? {
        refreshIfChanged()
        return map[key]
    }

    @Synchronized
    override fun put(key: String, value: String?) {
        FileLocks.withLock(FileLocks.lockFileFor(file)) {
            val merged = readDisk().toMutableMap()
            if (value == null) merged.remove(key) else merged[key] = value
            writePrivate(file, fileJson.encodeToString(serializer, merged))
            map.clear()
            map.putAll(merged)
            stamp = stampOf(file)
        }
    }
}
