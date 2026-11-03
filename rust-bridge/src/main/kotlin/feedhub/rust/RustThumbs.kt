package feedhub.rust

/**
 * 缩略图地址改写那一层的 Rust 门面。
 *
 * 规则本体在 `rust/crates/feedhub-core/src/thumb.rs`（`CardThumbnails.kt` 的 Rust 版），这里只做类型转换 +
 * 确保 `.so` 在。**没有开关、没有 Kotlin 备用实现**：界面、九宫格预取、详情占位、封面向量全走这一份，
 * 所以不存在"界面一套规则、向量一套规则"。
 *
 * 调用方式：**一屏一批**（[ofMany] / [prefetchMany] / [identityMany]）。单条那几个函数留给调用点本来
 * 就只有一条的地方（详情占位、封面窗口的 8 个宽度档）。
 */
object RustThumbs {
    /** 卡片宽度向上取到 160 的倍数（夹在 320～1920）。 */
    fun bucket(cardWidthPx: Int): Int = load { uniffi.feedhub_ffi.ffiThumbBucket(cardWidthPx) }

    /** 卡片该用的地址（认得的 CDN 换成缩略图，其余原样）。 */
    fun of(url: String, widthPx: Int): String = load { uniffi.feedhub_ffi.ffiThumbOf(url, widthPx) }

    /** 一批（等长等序）：一屏卡片只过一次边界。 */
    fun ofMany(urls: List<String>, widthPx: Int): List<String> =
        load { uniffi.feedhub_ffi.ffiThumbOfMany(urls, widthPx) }

    /** 这张图**能不能**拿缩略图预取（null = 拿不准是不是原图，不取）。 */
    fun prefetch(url: String, widthPx: Int): String? =
        load { uniffi.feedhub_ffi.ffiThumbPrefetch(url, widthPx) }

    /** 一批预取地址（等长等序；九宫格第 2～9 张一次算完）。 */
    fun prefetchMany(urls: List<String>, widthPx: Int): List<String?> =
        load { uniffi.feedhub_ffi.ffiThumbPrefetchMany(urls, widthPx) }

    /** 同一个地址、好几个宽度档（去重保序；封面向量试缓存用）。 */
    fun ofWidths(url: String, widths: List<Int>): List<String> =
        load { uniffi.feedhub_ffi.ffiThumbOfWidths(url, widths) }

    /** 同一张图的"身份"（缩略写法 / 签名 / 主机不同也认得出来）。 */
    fun identity(url: String): String = load { uniffi.feedhub_ffi.ffiThumbIdentity(url) }

    /** 一批身份（详情页拿卡片缩略图当占位时一屏一次）。 */
    fun identityMany(urls: List<String>): List<String> =
        load { uniffi.feedhub_ffi.ffiThumbIdentityMany(urls) }

    /** 地址看起来已经是服务端缩好的那一档。 */
    fun isServerThumbnail(url: String): Boolean =
        load { uniffi.feedhub_ffi.ffiThumbIsServerThumbnail(url) }

    /** 一次过边界处理一屏卡片（宽度档记在对象里，调用点只传地址）。 */
    class Batch(cardWidthPx: Int) {
        private val native: uniffi.feedhub_ffi.FfiThumbnails = load { uniffi.feedhub_ffi.FfiThumbnails(cardWidthPx) }

        /** 这一档的宽度（复用到 Coil 请求尺寸时用）。 */
        val widthPx: Int = load { native.widthPx() }

        fun ofMany(urls: List<String>): List<String> = load { native.ofMany(urls) }

        fun prefetchMany(urls: List<String>): List<String?> = load { native.prefetchMany(urls) }
    }

    /** 每次过边界前确保 `.so` 在（[RustNative] 自己会记住"已经加载过"）。 */
    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
