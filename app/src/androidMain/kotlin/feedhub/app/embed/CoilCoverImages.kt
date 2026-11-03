package feedhub.app.embed

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.util.Log
import coil3.SingletonImageLoader
import coil3.request.ImageRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import feedhub.app.image.ImageFetch

/**
 * 封面图从哪来。
 *
 * 顺序：
 * 1. **App 的图片磁盘缓存**（Coil 单例那份，就是首页卡片上已经在显示的那张）：按
 *    [feedhub.app.embed.coverImageUrls] 给的几档缩略图地址逐个试，命中就**一个请求都不发**。
 *    研究里量过：首页那张 480×270 / 640 宽的缩略图算出来的向量，和原图只差 **0.009**
 * ，所以根本不值得为向量再下一张原图；
 * 2. 缓存里没有 → 按第一个地址取**一次**，条件：**不按流量计费的网络**（Wi-Fi / 以太网，
 *    `NET_CAPABILITY_NOT_METERED`）。走的是 App 的图片客户端（[http]：浏览器 UA + 按域名的代理 +
 *    fail-closed），和界面上取图**同一条路**，不额外造一个客户端，也不碰 Cookie。
 *
 * 取不到（离线 / 移动数据 / 403 / 解不开）就返回 null，调度器把这条留到下次再试。
 */
class CoilCoverImages(
    private val context: Context,
    /** App 的取图那一层（`AppGraph.imageFetch`，每次现读：安卓上 Activity 重建会换一份新的 graph）。 */
    private val fetch: () -> ImageFetch,
    private val log: (String) -> Unit = { Log.i("feedhub-embed", it) },
) : CoverImageSource {
    /** 这个进程里真发出去的封面图片请求数（缓存命中不算）。 */
    @Volatile
    override var fetched: Int = 0
        private set

    override fun unmetered(): Boolean = runCatching {
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return false
        val network = cm.activeNetwork ?: return false
        val caps = cm.getNetworkCapabilities(network) ?: return false
        val notMetered = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        val wired = caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ||
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)
        notMetered && wired
    }.getOrDefault(false)

    override suspend fun load(urls: List<String>): ByteArray? {
        if (urls.isEmpty()) return null
        withContext(Dispatchers.IO) { fromCache(urls) }?.let { return it }
        if (!unmetered()) {
            if (!loggedMetered) {
                loggedMetered = true
                log("[封面] 现在不是不按流量计费的网络：封面图只从缓存里取（缺的下次在 Wi-Fi 上再取）")
            }
            return null
        }
        return download(urls.first())
    }

    private var loggedMetered = false

    /** 在 Coil 的磁盘缓存里找：**不同的卡片宽度档会话成不同的缓存键**，所以几个地址都要试。 */
    private fun fromCache(urls: List<String>): ByteArray? {
        val loader = runCatching { SingletonImageLoader.get(context) }.getOrNull() ?: return null
        val cache = loader.diskCache ?: return null
        for (url in urls) {
            val request = runCatching { ImageRequest.Builder(context).data(url).build() }.getOrNull() ?: continue
            // 网络图片的磁盘缓存键就是地址本身（和 `ImageExport.android.kt` / `ImageDiskCacheTest` 同一个口径：
            // `diskCacheKey` 在 execute 之前是 null，所以退回用 url）
            val key = request.diskCacheKey ?: url
            val snapshot = runCatching { cache.openSnapshot(key) }.getOrNull() ?: continue
            try {
                val path = snapshot.data ?: continue
                val bytes = runCatching { cache.fileSystem.read(path) { readByteArray() } }.getOrNull()
                if (bytes != null && bytes.isNotEmpty()) return bytes
            } finally {
                runCatching { snapshot.close() }
            }
        }
        return null
    }

    /**
     * 真的发一次 GET（只在 Wi-Fi / 以太网上会走到这里）。最多收 [MAX_BYTES] 字节。
     *
     * 走的是 App 那条取图的路，不额外造一个客户端，也不碰 Cookie。
     */
    private suspend fun download(url: String): ByteArray? = runCatching {
        val result = fetch().fetch(url, listOf("Accept" to "image/*"))
        if (result.code !in 200..299) {
            log("[封面] 取封面图失败：HTTP ${result.code}")
            return@runCatching null
        }
        val bytes = result.bytes
        if (bytes.isEmpty() || bytes.size > MAX_BYTES) {
            log("[封面] 封面图是空的 / 太大（${bytes.size} B），跳过")
            null
        } else {
            fetched++
            bytes
        }
    }.onFailure { log("[封面] 取封面图失败：${it::class.simpleName}: ${it.message}") }.getOrNull()

    private companion object {
        /** 一张封面最多收 4 MB（缩略图量级是几十 KB，超过这个只能是取错了）。 */
        const val MAX_BYTES = 4 * 1024 * 1024
    }
}
