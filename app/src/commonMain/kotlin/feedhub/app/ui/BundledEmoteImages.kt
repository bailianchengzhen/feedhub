package feedhub.app.ui

import androidx.compose.ui.graphics.ImageBitmap
import feedhub.core.BundledEmote

/**
 * 把一段图片字节解成 [ImageBitmap]；解不出来（不是图 / 缺库）就是 null。
 * 桌面用 Skia、安卓用 `BitmapFactory`（见各自源集的 actual）。
 */
expect fun decodeBundledEmote(bytes: ByteArray): ImageBitmap?

/**
 * 随插件带的表情图（`feedhub-emoji:` 地址，字节在插件 jar 的类路径资源里）的**解码结果**缓存。
 *
 * 为什么要有它：这些图本来就是本地的，走图片加载器（Coil）那条异步链路
 * ——取字节、解 PNG、写内存缓存、再回来画——每张都要等一两帧，第一次出现时是一块空白。用户看到的就是
 * "表情要等一下才出来"（卡片、评论区一片空白然后图才蹦出来）。这里改成**第一次用到时同步读 + 同步解码**：
 * 一帧都不等，后面同一个地址直接拿同一张 [ImageBitmap]（不再读资源、不再解码）。
 *
 * 只放内置表情（随插件带的 PNG，静态图）：网络表情仍然走 Coil（磁盘 / 内存缓存、动图按帧解码都在它那儿，
 * 这里不能替代——把动图的解码结果抄成一张静态位图会让动图不动）。
 *
 * **只在界面线程用**（组合 / 绘制 / 各平台自己的检查脚本都从界面线程调）：不加锁，避免 common 源集里
 * 用不了 `synchronized` 的问题。缓存按字节数封顶（[MAX_BYTES]），超了丢最久没用的。
 */
object BundledEmoteImages {
    /** 上限（字节，按 宽×高×4 估）。酷安 478 张 + 知乎 56 张全解出来约 12 MB，所以封在 8 MB：常用的都在，冷门的用完就丢。 */
    private const val MAX_BYTES = 8L * 1024 * 1024

    /** 条目数上限（很小的图按字节算不占多少，也封一个数，免得缓存无限长大）。 */
    private const val MAX_ENTRIES = 1024

    private val cache = LinkedHashMap<String, ImageBitmap>()
    private var cachedBytes = 0L

    /** 读不到 / 解不开的地址：记住，别每帧再读一遍资源。 */
    private val missing = HashSet<String>()

    /** 解码次数（测试用：同一个地址只解一次）。 */
    var decodeCount: Int = 0
        private set

    /** 资源读取次数（测试用）。 */
    var readCount: Int = 0
        private set

    /**
     * 内置表情 → 图片。命中就直接给（并标记为最近用过）；没命中就**同步**读资源 + 解码 + 进缓存。
     * 不是 `feedhub-emoji:` 地址、或者资源里没有 / 不是图 → null（调用方按加载失败处理，改回文字代码）。
     */
    fun get(url: String): ImageBitmap? {
        cache[url]?.let { hit ->
            // 手动 LRU：命中就挪到队尾
            cache.remove(url)
            cache[url] = hit
            return hit
        }
        if (url in missing) return null
        val ref = BundledEmote.parse(url) ?: return null
        readCount++
        val bytes = BundledEmoteFetcher.readBytes(ref)
        val bitmap = bytes?.let(::decodeBundledEmote)
        if (bitmap == null || bitmap.width <= 0 || bitmap.height <= 0) {
            missing += url
            return null
        }
        decodeCount++
        put(url, bitmap)
        return bitmap
    }

    private fun put(url: String, bitmap: ImageBitmap) {
        cache[url] = bitmap
        cachedBytes += sizeOf(bitmap)
        trim()
    }

    private fun sizeOf(bitmap: ImageBitmap): Long = bitmap.width.toLong() * bitmap.height * 4

    private fun trim() {
        while (cache.size > MAX_ENTRIES || cachedBytes > MAX_BYTES) {
            val oldest = cache.keys.firstOrNull() ?: return
            val removed = cache.remove(oldest) ?: return
            cachedBytes -= sizeOf(removed)
        }
    }

    /** 缓存里现在有多少张 / 多少字节（测试和安卓的 `emojicheck` 用）。 */
    fun stats(): Stats = Stats(cache.size, cachedBytes)

    /** 清空（测试用；App 里不清——资源不会中途冒出来，清了只是白白重新解码）。 */
    fun clear() {
        cache.clear()
        missing.clear()
        cachedBytes = 0
        decodeCount = 0
        readCount = 0
    }

    data class Stats(val entries: Int, val bytes: Long)
}
