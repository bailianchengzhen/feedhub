package feedhub.feed

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiFeedPreloadTrigger
import uniffi.feedhub_ffi.feedPreloadFeedScreens
import uniffi.feedhub_ffi.feedPreloadImageScreens
import uniffi.feedhub_ffi.feedPreloadMemoryCacheBytes
import uniffi.feedhub_ffi.feedPreloadNearEnd
import uniffi.feedhub_ffi.feedPreloadRange

/**
 * **预加载策略**（`rust/crates/feedhub-core/src/feed/preload.rs`）的 Kotlin 门面。
 *
 *
 * 平台接口请求不比正常滑动多**。
 *
 * Kotlin 剩下的：屏幕尺寸 / 滚动位置 / 是否插电这些**测量**（框架接口）、Coil 的加载调用、协程。
 */
object PreloadPolicy {
    /** 插电时：剩不到 1.5 屏就取下一页。 */
    val FEED_SCREENS_PLUGGED: Float get() = load { feedPreloadFeedScreens(true) }

    /** 电池时：剩不到 1 屏就取下一页。 */
    val FEED_SCREENS_BATTERY: Float get() = load { feedPreloadFeedScreens(false) }

    /** 插电时：图片预取到可见区下面 2 屏。 */
    val IMAGE_SCREENS_PLUGGED: Float get() = load { feedPreloadImageScreens(true) }

    /** 电池时：图片预取到可见区下面 1 屏。 */
    val IMAGE_SCREENS_BATTERY: Float get() = load { feedPreloadImageScreens(false) }

    /** 原来的触发点：剩不到 8 张（按屏算的触发点不会比它晚）。 */
    const val MIN_REMAINING: Int = 8

    /** 插电时 Coil 内存缓存的上限（1.5 GiB）。 */
    const val PLUGGED_MEMORY_CACHE_BYTES: Long = 1536L * 1024 * 1024

    /** 图片预取同时最多几个（低于正在显示的图片：显示的不受这个限制）。 */
    const val IMAGE_PARALLEL: Int = 2

    fun feedScreens(plugged: Boolean): Float = load { feedPreloadFeedScreens(plugged) }

    fun imageScreens(plugged: Boolean): Float = load { feedPreloadImageScreens(plugged) }

    /** 最后一张可见卡片是 [lastVisible]，一共 [total] 张，一屏大约 [perScreen] 张：剩下的不到 [screens] 屏了吗。 */
    fun nearEnd(lastVisible: Int, total: Int, perScreen: Int, screens: Float): Boolean =
        load { feedPreloadNearEnd(lastVisible, total, perScreen, screens) }

    /** 要预取图片的卡片下标：可见区下面 [screens] 屏（空区间时返回空）。 */
    fun prefetchRange(lastVisible: Int, total: Int, perScreen: Int, screens: Float): IntRange = load {
        val indices = feedPreloadRange(lastVisible, total, perScreen, screens)
        if (indices.isEmpty()) IntRange.EMPTY else indices.first()..indices.last()
    }

    /** Coil 内存缓存的上限：插电放大，电池用 Coil 默认的 [initial]。 */
    fun memoryCacheBytes(plugged: Boolean, initial: Long): Long =
        load { feedPreloadMemoryCacheBytes(plugged, initial) }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}

/**
 * 信息流"该取下一页了吗"（Rust 的 `FeedPreloadTrigger`）。剩的不到 N 屏、没有一页在飞，
 * 并且**从上次发起之后可见位置变过**。
 *
 * [onFrame] 的 [start] 是"真的调一次 `loadMore`"——Rust 只回答该不该发起，发起本身是宿主的协程。
 */
class FeedPreloadTrigger : AutoCloseable {
    init { RustNative.ensureLoaded() }

    private val native: FfiFeedPreloadTrigger = FfiFeedPreloadTrigger()

    /**
     * 这一帧那个"该不该发起"的判断题（宿主还没真的发）。
     *
     * 两步走：先问 [shouldStart]，宿主真的发起成功（`loadMore()` 返回 true）之后再调 [noteStarted]——
     * "发起"是宿主的协程，Rust 里没有回调。
     */
    fun shouldStart(
        lastVisible: Int,
        total: Int,
        perScreen: Int,
        screens: Float,
        loading: Boolean,
    ): Boolean {
        RustNative.ensureLoaded()
        return native.shouldStart(lastVisible, total, perScreen, screens, loading)
    }

    /** 宿主真的发起了一次 loadMore：记下这次的位置（用户不动就不再取）。 */
    fun noteStarted(lastVisible: Int) {
        RustNative.ensureLoaded()
        native.noteStarted(lastVisible)
    }

    fun reset() {
        RustNative.ensureLoaded()
        native.reset()
    }

    override fun close() {
        native.close()
    }
}
