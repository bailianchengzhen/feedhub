package feedhub.core.local

import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.locks.ReentrantLock

/**
 * 跨进程文件锁。
 *
 * 为什么不用 `FileChannel.tryLock` 轮询：我们要的是"排队"，不是"抢不到就报错"——[withLock] 会**阻塞**
 * 到拿到锁为止。锁是**排他**的，锁文件放在被保护文件旁边（`<文件>.lock`），换了进程也认。
 *
 * 两层的锁：
 * 1. 同一个 JVM 里先过一把 [ReentrantLock]（按绝对路径分）：`FileChannel.lock()` 对同一个文件
 *    第二次加锁会抛 `OverlappingFileLockException`，同一进程的两个线程（测试 / 将来 cli 内嵌 MCP）也会互相排队；
 * 2. 进程之间用操作系统的文件锁（Linux 上是 `fcntl` 记录锁——`FileChannel.lock()` 就是它，Rust 侧
 *    `cookies/file.rs` 用的是同一套；Windows 是 `LockFile`），进程异常退出时系统自动释放，
 *    不会留下"死锁文件"。
 *
 * 锁文件**不存内容**（创建出来当门闩用），可以随时删；不会写被保护的那份文件。
 *
 * 用法只有一种：[withLock]（"拿 → 跑 → 放"，一段代码在锁里）。
 *
 * 因为罐子的 `persist`
 * 搬进了 Rust、跨 FFI 塞不进 Kotlin 的回调。
 * 那个入口没有调用方了，已删。
 */
object FileLocks {
    /** 同一进程内的那一层（按绝对路径）。数量很少（每人几个文件），不清理。 */
    private val local = ConcurrentHashMap<String, ReentrantLock>()

    /**
     * 拿锁 → 跑 → 放（`ReentrantLock` + `FileChannel.lock()`，两层都拿）。
     *
     * Kotlin 这边没有跨 FFI 的"拿 / 还"需求了；
     * 这里剩下的调用方（`state.json`、cli 的限速文件）都是"一段代码在锁里"。
     */
    fun <T> withLock(lockFile: Path, block: () -> T): T {
        val key = lockFile.toAbsolutePath().normalize().toString()
        val localLock = local.computeIfAbsent(key) { ReentrantLock() }
        localLock.lock()
        try {
            lockFile.parent?.let { Files.createDirectories(it) }
            val channel = FileChannel.open(
                lockFile,
                StandardOpenOption.CREATE,
                StandardOpenOption.WRITE,
            )
            try {
                val fileLock: FileLock = channel.lock()
                try {
                    return block()
                } finally {
                    runCatching { fileLock.release() }
                }
            } finally {
                runCatching { channel.close() }
            }
        } finally {
            localLock.unlock()
        }
    }

    /** 被保护文件 [file] 的锁文件：`cookies.json` → `cookies.json.lock`。 */
    fun lockFileFor(file: Path): Path = file.resolveSibling(file.fileName.toString() + ".lock")
}
