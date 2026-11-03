package feedhub.app.feed

import androidx.compose.foundation.lazy.staggeredgrid.LazyStaggeredGridState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import coil3.ImageLoader
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import coil3.size.Precision
import coil3.size.Scale
import feedhub.app.AppForeground
import feedhub.core.FeedItem
import feedhub.feed.FeedPreloadTrigger
import feedhub.feed.PreloadPolicy
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlin.math.roundToInt

/**
 * 现在插着电吗。桌面由 `Main.kt` 按 `/sys/class/power_supply` 给（每 30 秒看一次），
 * 读不到 / 别的平台一律当插电（宁可多预载一点，不会因此多发平台接口请求，见 [PreloadPolicy]）。
 *
 * **留在宿主**：这是"读操作系统"的那一类（`/sys/class/power_supply`）。
 */
val LocalPluggedIn = compositionLocalOf { true }

/*
 * Kotlin 门面在 `feedhub.feed`（`:rust-bridge`）。
 * 这个文件只剩**界面那一侧的三件宿主的事**：
 * 1. 测量（`LazyStaggeredGridState.layoutInfo` 那些框架接口）；
 * 2. Coil 的加载调用（`loader.execute`）；
 * 3. 每帧 / 每个组合的协程触发（`LaunchedEffect` / `snapshotFlow`）。
 *
 * 平台接口请求不比正常滑动多**；卡片图片只打图片 CDN，不打平台接口；详情接口一律不预取。
 */

/**
 * 卡片图片预取：可见区下面 N 屏（[PreloadPolicy.imageScreens]）的卡片图，按卡片上的同一个地址、同一个像素尺寸
 * 交给 Coil（进内存 + 磁盘缓存），滚到的时候直接命中。只打图片 CDN；每张只发一次，排队时已经滚走的不发；同时最多
 * [PreloadPolicy.IMAGE_PARALLEL] 个。
 *
 * **后台不预取**：[foreground] 变 false 时把在飞的请求一起取消。不能只靠重组——
 * 安卓 Activity 停下来以后帧时钟也停了，等下一次重组可能已经是回到前台的时候了；所以 [AppForeground] 的监听器
 * 直接取消这个 effect 的 Job。回到前台时 [foreground] 变 true → 重组 → effect 重起，按当时的位置重新预取。
 */
@Composable
internal fun CardImagePrefetch(grid: LazyStaggeredGridState, shown: List<FeedItem>, loader: ImageLoader, foreground: AppForeground) {
    val plugged = LocalPluggedIn.current
    val cardWidth = LocalCardWidthPx.current
    val density = LocalDensity.current
    val context = LocalPlatformContext.current
    val items = rememberUpdatedState(shown)
    val active = foreground.isForeground
    val generation = foreground.generation
    LaunchedEffect(grid, loader, plugged, cardWidth, density, active, generation) {
        if (!active) return@LaunchedEffect
        val self = currentCoroutineContext()[Job]
        val listener: (Boolean) -> Unit = { nowActive -> if (!nowActive) self?.cancel() }
        foreground.addListener(listener)
        try {
            val gate = Semaphore(PreloadPolicy.IMAGE_PARALLEL)
            val started = java.util.Collections.synchronizedSet(
                java.util.Collections.newSetFromMap(
                    object : LinkedHashMap<Triple<String, Int, Int>, Boolean>(256, 0.75f, true) {
                        override fun removeEldestEntry(
                            eldest: MutableMap.MutableEntry<Triple<String, Int, Int>, Boolean>?,
                        ): Boolean = size > MAX_PREFETCH_STARTED
                    },
                ),
            )
            var wanted = emptySet<Triple<String, Int, Int>>()
            val insetPx = with(density) { PostCardPadding.roundToPx() } * 2
            val maxHeightPx = with(density) { CardMaxImageHeight.toPx() }
            snapshotFlow {
                val info = grid.layoutInfo
                val visible = info.visibleItemsInfo.filter { it.key != FEED_FOOTER_KEY }
                val last = visible.maxOfOrNull { it.index } ?: -1
                val cardW = visible.firstOrNull()?.size?.width ?: 0
                val list = items.value
                val range = PreloadPolicy.prefetchRange(last, list.size, visible.size, PreloadPolicy.imageScreens(plugged))
                if (cardW <= 0) emptyList() else {
                    // 地址改写按**一屏一批**问 Rust（`CardThumbnails.ofMany`）：一次过边界，不逐张
                    val specs = range.mapNotNull { i ->
                        val spec = cardImageSpec(list[i]) ?: return@mapNotNull null
                        val w = if (spec.inset) cardW - insetPx else cardW
                        val size = cardImageRequestSize(w, spec.ratio, maxHeightPx) ?: return@mapNotNull null
                        Triple(spec, size, w)
                    }
                    val width = CardThumbnails.bucket(if (cardWidth > 0) cardWidth else cardW)
                    val urls = CardThumbnails.ofMany(specs.map { it.first.url }, width)
                    specs.mapIndexed { i, entry -> Triple(urls[i], entry.second.first, entry.second.second) }
                }
            }.distinctUntilChanged().collect { targets ->
                // 每张图只发一次；在飞的不因为目标变了就取消（取消会把下了一半的磁盘缓存丢掉，滚到时又得重下）；
                // 排着队还没轮到、已经不在目标里的就不发了（以后又进了目标再排）
                wanted = targets.toSet()
                for (t in targets) {
                    if (!started.add(t)) continue
                    launch {
                        gate.withPermit {
                            if (t !in wanted) { started.remove(t); return@withPermit }
                            // 和 AsyncImage 一样的尺寸 / 缩放 / 精度（Fit → FIT、不放大），内存缓存才对得上
                            loader.execute(ImageRequest.Builder(context).data(t.first).size(t.second, t.third).scale(Scale.FIT).precision(Precision.INEXACT).build())
                        }
                    }
                }
            }
        } finally {
            foreground.removeListener(listener)
        }
    }
}

/**
 * 预取的那一批的卡片图先下好：
 * 用户点刷新、新的一批放到最上面时图已经在缓存里。只取最上面 [PreloadPolicy.imageScreens] 屏那么多张（按现在一屏几张算），
 * 尺寸 / 地址和 [CardImagePrefetch] 一样的算法（卡片宽用 [LocalCardWidthPx]）。只打图片 CDN。
 */
@Composable
internal fun PrefetchedImages(grid: LazyStaggeredGridState, prefetched: List<FeedItem>, loader: ImageLoader, foreground: AppForeground) {
    val plugged = LocalPluggedIn.current
    val cardWidth = LocalCardWidthPx.current
    val density = LocalDensity.current
    val context = LocalPlatformContext.current
    val active = foreground.isForeground
    val generation = foreground.generation
    LaunchedEffect(prefetched, cardWidth, plugged, active, generation) {
        if (!active) return@LaunchedEffect
        if (prefetched.isEmpty() || cardWidth <= 0) return@LaunchedEffect
        val self = currentCoroutineContext()[Job]
        val listener: (Boolean) -> Unit = { nowActive -> if (!nowActive) self?.cancel() }
        foreground.addListener(listener)
        try {
            val perScreen = grid.layoutInfo.visibleItemsInfo.count { it.key != FEED_FOOTER_KEY }.coerceAtLeast(8)
            val count = (perScreen * PreloadPolicy.imageScreens(plugged)).roundToInt().coerceAtMost(prefetched.size)
            val insetPx = with(density) { PostCardPadding.roundToPx() } * 2
            val maxHeightPx = with(density) { CardMaxImageHeight.toPx() }
            val gate = Semaphore(PreloadPolicy.IMAGE_PARALLEL)
            // 地址改写一屏一批（`CardThumbnails.ofMany`）：预取的一批最多就几屏，一次过边界
            val targets = prefetched.take(count).mapNotNull { item ->
                val spec = cardImageSpec(item) ?: return@mapNotNull null
                val w = if (spec.inset) cardWidth - insetPx else cardWidth
                val size = cardImageRequestSize(w, spec.ratio, maxHeightPx) ?: return@mapNotNull null
                spec to size
            }
            val urls = CardThumbnails.ofMany(targets.map { it.first.url }, CardThumbnails.bucket(cardWidth))
            for ((i, entry) in targets.withIndex()) {
                val size = entry.second
                launch {
                    gate.withPermit {
                        loader.execute(
                            ImageRequest.Builder(context).data(urls[i])
                                .size(size.first, size.second).scale(Scale.FIT).precision(Precision.INEXACT).build(),
                        )
                    }
                }
            }
        } finally {
            foreground.removeListener(listener)
        }
    }
}

/** 信息流网格这一帧的样子（[FeedPreloadTrigger] 的输入）。 */
internal data class FeedFrame(val lastVisible: Int, val total: Int, val perScreen: Int, val loading: Boolean)

/** 信息流网格最后那个"加载中"脚注的 key（不是卡片，不算一屏几张）。 */
internal const val FEED_FOOTER_KEY = "footer"

/** 卡片图片预取去重表的上限（防 B5 随滚动无限增长）。 */
private const val MAX_PREFETCH_STARTED = 1024
