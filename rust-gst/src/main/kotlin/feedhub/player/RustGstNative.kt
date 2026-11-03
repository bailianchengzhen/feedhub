package feedhub.player

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

/**
 * 找桌面播放引擎的 `libfeedhub_gst.so` 并交给 UniFFI（`uniffi.feedhub_gst`）。
 *
 * 构建期同时写入 `libfeedhub_gst.so.sha256`（`<sha256> <size>`），命中缓存时免去读取与哈希 `.so` 资源。
 */
internal object RustGstNative {
    private const val RESOURCE = "/feedhub-native/host/libfeedhub_gst.so"
    private const val STAMP_RESOURCE = "/feedhub-native/host/libfeedhub_gst.so.sha256"
    private const val OVERRIDE_PROPERTY = "uniffi.component.feedhub_gst.libraryOverride"

    @Volatile
    private var failure: Throwable? = null

    @Volatile
    private var loaded = false

    fun ensureLoaded() {
        if (loaded) return
        synchronized(this) {
            if (loaded) return
            failure?.let { throw IllegalStateException("GStreamer Rust 引擎加载失败：${it.message}", it) }
            try {
                extractAndPoint()
                loaded = true
            } catch (e: Throwable) {
                failure = e
                throw IllegalStateException("GStreamer Rust 引擎加载失败：${e.message}", e)
            }
        }
    }

    inline fun <T> native(block: () -> T): T = try {
        ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("GStreamer 引擎加载失败（找不到 libfeedhub_gst.so）：${e.message}", e)
    }

    private fun extractAndPoint() {
        val cacheRoot = cacheRoot()
        val stamp = RustGstNative::class.java.getResourceAsStream(STAMP_RESOURCE)
            ?.bufferedReader()
            ?.use { it.readText().trim() }
        if (!stamp.isNullOrEmpty()) {
            val parts = stamp.split(' ')
            val fullHex = parts.getOrNull(0)?.takeIf { it.length >= 16 }
            val expectedSize = parts.getOrNull(1)?.toLongOrNull()
            if (fullHex != null && expectedSize != null) {
                val hash = fullHex.take(16)
                val dir = cacheRoot.resolve(hash)
                val target = dir.resolve("libfeedhub_gst.so")
                if (Files.exists(target) && Files.size(target) == expectedSize) {
                    System.setProperty(OVERRIDE_PROPERTY, target.toAbsolutePath().toString())
                    return
                }
                val bytes = readSoResource()
                Files.createDirectories(dir)
                val tmp = Files.createTempFile(dir, "libfeedhub_gst", ".so.tmp")
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
        val target = dir.resolve("libfeedhub_gst.so")
        if (!Files.exists(target) || Files.size(target) != bytes.size.toLong()) {
            val tmp = Files.createTempFile(dir, "libfeedhub_gst", ".so.tmp")
            Files.write(tmp, bytes)
            Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        }
        System.setProperty(OVERRIDE_PROPERTY, target.toAbsolutePath().toString())
    }

    private fun readSoResource(): ByteArray =
        RustGstNative::class.java.getResourceAsStream(RESOURCE)?.use { it.readBytes() }
            ?: error(
                "jar 里没有 GStreamer Rust 引擎（$RESOURCE）。桌面版应该由 :rust-gst 的 stageGstLib 放进去；" +
                    "如果是从 IDE 直接跑，先 ./gradlew :rust-gst:stageGstLib。",
            )

    private fun cacheRoot(): Path {
        val xdg = System.getenv("XDG_CACHE_HOME")?.takeIf { it.isNotBlank() }
        if (xdg != null) return Path.of(xdg, "feedhub", "native", "rust")
        val home = System.getProperty("user.home") ?: "."
        return Path.of(home, ".cache", "feedhub", "native", "rust")
    }
}
