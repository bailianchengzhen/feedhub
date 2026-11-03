package feedhub.app.feed

import feedhub.rust.RustThumbs

/**
 * 首页 / 关注卡片用缩略图：能让图片 CDN 按宽度缩的平台，卡片要一张
 * "不比卡片窄"的图；详情页、看大图仍然取原图（不经过这里）。
 *
 * 这个对象只剩一层门面：界面取图（[of]）、九宫格预取（[thumbnailFor]）、详情占位（[identity]）、
 * 封面向量（同样的 [of]）**都调 Rust 那一份**，所以不存在"界面一套规则、向量一套规则"。
 *
 * **口径表在那边的文件头**（改规则两边一起改：Kotlin 这边没有第二份）：
 *
 * | 平台 | 写法 | 实测 |
 * |---|---|---|
 * | B站 `hdslb.com` | `原图@{宽}w.webp` | 1080×1440 / 1.59 MB → 640×853 / 177 KB |
 * | 知乎 `zhimg.com` | 文件名后缀换成 `_720w` / `_1440w` | 给 720 宽 |
 * | 小黑盒 `max-c.com` | `?imageMogr2/thumbnail/{宽}x>`（只缩不放） | 810×1080 / 181 KB → 640×853 / 53 KB |
 * | 酷安 `image.coolapk.com` | `原图.s.jpg`（固定 720 宽；卡片比 720 宽就用原图） | 800×800 / 60 KB → 720×720 / 40 KB |
 * | X `pbs.twimg.com` | `?format=…&name=medium`（长边 ≤1200；卡片更宽用 `large`） | 1125×1278 / 160 KB → 1056×1200 / 143 KB |
 *
 * 不改、照旧用平台给的地址：贴吧（信息流给的 `bigPic` 本来就是服务端缩好的 `w=720;q=60`，还带 `tbpicau` 鉴权参数）、
 * 抖音 / TikTok（地址带 `x-signature` 签名；封面本来就是 `tplv-…:330`）、Reddit（`preview.redd.it` 带 `s=` 签名）、
 * YouTube（已经是 `hq720`）、小红书。
 * GIF 一律不改（缩略图不能把动图变成静态的）。
 */
object CardThumbnails {
    private data class UrlCacheKey(val url: String, val widthPx: Int)

    private val urlCacheLock = Any()
    private val urlCache = object : LinkedHashMap<UrlCacheKey, String>(512, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<UrlCacheKey, String>?): Boolean =
            size > MAX_URL_CACHE
    }

    private const val MAX_URL_CACHE = 2048

    /** 卡片宽度向上取到 160 的倍数（窗口拉一点点不换地址、缓存还能用），夹在 320～1920（与 `thumb::bucket` 同式，免过 FFI）。 */
    fun bucket(cardWidthPx: Int): Int {
        val steps = kotlin.math.ceil(cardWidthPx.coerceAtLeast(1) / 160.0).toInt()
        return (steps * 160).coerceIn(320, 1920)
    }

    /** 卡片该用的地址：认得的 CDN 换成不比 [widthPx] 窄的缩略图，其余原样（带 LRU 缓存，组合路径 0 FFI）。 */
    fun of(url: String, widthPx: Int): String {
        val key = UrlCacheKey(url, widthPx)
        synchronized(urlCacheLock) {
            urlCache[key]?.let { return it }
        }
        val resolved = RustThumbs.of(url, widthPx)
        synchronized(urlCacheLock) {
            urlCache[key] = resolved
        }
        return resolved
    }

    /** 一批 [of]（未命中的合并成一次 FFI，结果写入同一份 LRU 供卡片上屏时直接命中）。 */
    fun ofMany(urls: List<String>, widthPx: Int): List<String> {
        if (urls.isEmpty()) return emptyList()
        val out = arrayOfNulls<String>(urls.size)
        val missingIndices = ArrayList<Int>()
        val missingUrls = ArrayList<String>()
        synchronized(urlCacheLock) {
            for ((i, u) in urls.withIndex()) {
                val hit = urlCache[UrlCacheKey(u, widthPx)]
                if (hit != null) {
                    out[i] = hit
                } else {
                    missingIndices += i
                    missingUrls += u
                }
            }
        }
        if (missingUrls.isNotEmpty()) {
            val resolved = RustThumbs.ofMany(missingUrls, widthPx)
            synchronized(urlCacheLock) {
                for (j in missingUrls.indices) {
                    val r = resolved[j]
                    urlCache[UrlCacheKey(missingUrls[j], widthPx)] = r
                    out[missingIndices[j]] = r
                }
            }
        }
        @Suppress("UNCHECKED_CAST")
        return out.asList() as List<String>
    }

    /**
     * 这张图**能不能**拿缩略图预取：
     *
     * - 认得的、能按宽度改写的 CDN（[of]）→ 改写后的地址（比原图小得多）；
     * - 信息流给的地址**本身就是服务端缩好的那一档** → 原地址（见下面"已是缩略图"那几条）；
     * - 别的（拿不准是不是原图 / 认不出的主机 / 动图）→ **null，不取**：预取原图既费流量又违背省电那条。
     *
     * 和 [of] 分开是因为卡片上"必须有一张图"（拿不到缩略图就用原图），而**预取**宁可不取也不能把原图拉下来。
     */
    fun thumbnailFor(url: String, widthPx: Int): String? = RustThumbs.prefetch(url, widthPx)

    /** 一批 [thumbnailFor]（九宫格第 2～9 张一次算完，等长等序）。 */
    fun thumbnailForMany(urls: List<String>, widthPx: Int): List<String?> = RustThumbs.prefetchMany(urls, widthPx)

    /** 同一个地址、好几个宽度档（去重保序；封面向量试缓存用，一档一档问 Rust 的代价在这里合成一次）。 */
    fun ofWidths(url: String, widths: List<Int>): List<String> = RustThumbs.ofWidths(url, widths)

    /**
     * 地址看起来**已经是**服务端缩好的那一档（不是原图）。拿不准就不算。
     */
    fun isServerThumbnail(url: String): Boolean = RustThumbs.isServerThumbnail(url)

    /**
     * 同一张图的两种写法 → 同一个"身份"。
     *
     * 详情正文里的图片地址经常和卡片上的不完全一样，但指的是同一张图：缩略写法不同、签名查询串不同、
     * 同一家 CDN 的不同主机。按"平台的图片身份"比：认得的 CDN 取文件名或哈希，认不得的退回"主机 + 路径、去掉查询串"。
     * 比不上的最多是不用占位（照旧加载），不会把两张不同的图认成一张。
     */
    fun identity(url: String): String = RustThumbs.identity(url)

    /** 一批 [identity]（详情页一屏的占位比对只过一次 FFI）。 */
    fun identityMany(urls: List<String>): List<String> = RustThumbs.identityMany(urls)

    /**
     * 一屏卡片的批量入口（宽度档记在对象里）。**名字和用法都和逐条一样**，只是过边界一次。
     *
     * 调用点在 Compose 里（每次都可能是新的一屏），所以这个对象**不缓存结果**：缓存归 `remember` / Coil。
     */
    class Batch(cardWidthPx: Int) {
        private val batch = RustThumbs.Batch(cardWidthPx)

        /** 这一档的宽度（`bucket` 之后的结果，复用到 Coil 请求尺寸时用）。 */
        val widthPx: Int get() = batch.widthPx

        fun ofMany(urls: List<String>): List<String> = batch.ofMany(urls)

        fun thumbnailForMany(urls: List<String>): List<String?> = batch.prefetchMany(urls)
    }
}
