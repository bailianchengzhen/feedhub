package feedhub.rust

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

/**
 * 找 `.so` 并把它加载进进程。
 *
 * - **桌面（JVM）**：`.so` 以资源形式在 jar 里（`feedhub-native/host/libfeedhub_ffi.so`），构建期同时写入
 *   `libfeedhub_ffi.so.sha256`（`<sha256> <size>`）。启动时优先读这几十字节的戳记并核对缓存文件大小，
 *   命中时**不读取也不哈希几 MB 的 `.so` 资源**；未命中时才解压到
 *   `~/.cache/feedhub/native/rust/<内容哈希前 16 位>/`，再用 UniFFI 的
 *   `uniffi.component.feedhub_ffi.libraryOverride` 属性指过去。
 * - **安卓**：`.so` 在 APK 的 `nativeLibraryDir` 里，JNA 的 `Native.load("feedhub_ffi")` 会走
 *   `System.loadLibrary` 自己找到它，**不要**设 override（设了反而指不到 APK 里的路径）。
 *
 * 加载是懒的：开关关着时一个字节的 `.so` 都不读（`RustNative` 第一次被调用时才解压 / 加载）。
 */
internal object RustNative {
    private const val RESOURCE = "/feedhub-native/host/libfeedhub_ffi.so"
    private const val STAMP_RESOURCE = "/feedhub-native/host/libfeedhub_ffi.so.sha256"
    private const val OVERRIDE_PROPERTY = "uniffi.component.feedhub_ffi.libraryOverride"

    @Volatile
    private var failure: Throwable? = null

    @Volatile
    private var loaded = false

    /** 已经是安卓（ART / Dalvik）就用系统的库搜索路径。 */
    val isAndroid: Boolean
        get() = System.getProperty("java.vm.name")?.contains("Dalvik", ignoreCase = true) == true ||
            System.getProperty("java.vendor")?.contains("Android", ignoreCase = true) == true

    /** 确保 `.so` 能用；不能时抛出一条说得清的错误（不会悄悄退回 Kotlin 实现）。 */
    fun ensureLoaded() {
        if (loaded) return
        synchronized(this) {
            if (loaded) return
            failure?.let { throw IllegalStateException("Rust 核心加载失败：${it.message}", it) }
            try {
                if (!isAndroid) extractAndPoint()
                installCallbackExceptionHandler()
                loaded = true
            } catch (e: Throwable) {
                failure = e
                throw IllegalStateException("Rust 核心加载失败：${e.message}", e)
            }
        }
    }

    /**
     * Rust 回调宿主（JNA callback）时宿主抛了异常：JNA 默认把**整条栈**经 `java.util.logging` 打出来，
     * 回调里栈溢出时一条就是上千层、几 MB 的字符串。这里只记一行：异常类型、消息、最上面几层。
     */
    private fun installCallbackExceptionHandler() {
        com.sun.jna.Native.setCallbackExceptionHandler { callback, e ->
            val top = e.stackTrace.take(CALLBACK_TRACE_FRAMES).joinToString(" <- ")
            System.err.println("[rust] 回调 ${callback.javaClass.name} 抛了 ${e.javaClass.name}: ${e.message} @ $top")
        }
    }

    private const val CALLBACK_TRACE_FRAMES = 5

    private fun extractAndPoint() {
        val cacheRoot = cacheRoot()
        val stamp = RustNative::class.java.getResourceAsStream(STAMP_RESOURCE)
            ?.bufferedReader()
            ?.use { it.readText().trim() }
        if (!stamp.isNullOrEmpty()) {
            val parts = stamp.split(' ')
            val fullHex = parts.getOrNull(0)?.takeIf { it.length >= 16 }
            val expectedSize = parts.getOrNull(1)?.toLongOrNull()
            if (fullHex != null && expectedSize != null) {
                val hash = fullHex.take(16)
                val dir = cacheRoot.resolve(hash)
                val target = dir.resolve("libfeedhub_ffi.so")
                if (Files.exists(target) && Files.size(target) == expectedSize) {
                    System.setProperty(OVERRIDE_PROPERTY, target.toAbsolutePath().toString())
                    return
                }
                val bytes = readSoResource()
                Files.createDirectories(dir)
                val tmp = Files.createTempFile(dir, "libfeedhub_ffi", ".so.tmp")
                Files.write(tmp, bytes)
                Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
                System.setProperty(OVERRIDE_PROPERTY, target.toAbsolutePath().toString())
                return
            }
        }

        val bytes = readSoResource()
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
        val hash = digest.joinToString("") { "%02x".format(it) }.take(16)
        val dir = cacheRoot.resolve(hash)
        Files.createDirectories(dir)
        val target = dir.resolve("libfeedhub_ffi.so")
        if (!Files.exists(target) || Files.size(target) != bytes.size.toLong()) {
            val tmp = Files.createTempFile(dir, "libfeedhub_ffi", ".so.tmp")
            Files.write(tmp, bytes)
            Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        }
        System.setProperty(OVERRIDE_PROPERTY, target.toAbsolutePath().toString())
    }

    private fun readSoResource(): ByteArray =
        RustNative::class.java.getResourceAsStream(RESOURCE)?.use { it.readBytes() }
            ?: error(
                "jar 里没有 Rust 核心（$RESOURCE）。桌面版应该由 :rust-bridge 的 stageHostLib 放进去；" +
                    "如果是从 IDE 直接跑，先 ./gradlew :rust-bridge:stageHostLib。",
            )

    private fun cacheRoot(): Path {
        val xdg = System.getenv("XDG_CACHE_HOME")?.takeIf { it.isNotBlank() }
        if (xdg != null) return Path.of(xdg, "feedhub", "native", "rust")
        val home = System.getProperty("user.home") ?: "."
        return Path.of(home, ".cache", "feedhub", "native", "rust")
    }
}
