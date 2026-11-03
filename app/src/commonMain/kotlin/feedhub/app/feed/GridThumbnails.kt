package feedhub.app.feed

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalWindowInfo
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import coil3.request.SuccessResult
import coil3.size.Precision
import coil3.size.Scale
import feedhub.app.AppForeground
import feedhub.app.detail.DetailContentMaxWidth
import feedhub.app.detail.DetailSidePadding
import feedhub.app.detail.GridGap
import feedhub.app.detail.imageGridColumns
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlin.math.roundToInt

/**
 * 九宫格 / 多图卡片的**第 2～9 张**缩略图预取。
 *
 * 上一期只给"卡片上画过的那一张"垫了缩略图，九宫格第 2 张起进详情
 * 还是要等。当前：
 *
 * 1. [FeedCard] 组合时（= 真的画到屏幕上）用 [gridThumbnailTargets] 算出这张卡第 2～9 张的**卡片尺寸缩略图**
 *    地址（[CardThumbnails] 的同一套写法），先记进 [CardImageHints]（详情页 [feedhub.app.detail.DetailThumbnails]
 *    按"图片身份"认得出来），再由 [prefetchGridThumbnails] 交给 Coil 取进内存 / 磁盘缓存；
 * 2. 尺寸取**详情里宫格格子的显示宽度**那一档（不是卡片那一档，也不是原图），见 [detailGridCellWidthPx]；
 * 3. 平台拿不出比原图小的写法（[CardThumbnails.thumbnailFor] 返回 null）就**不取**，免得变成预取原图；
 * 4. **只在前台取**；卡片滑出屏幕（Compose 的 effect 被取消）时在飞的一起取消；同一张不重复取；每张卡最多 8 张。
 *
 * 「默认显示原图」开着时详情里照旧是"缩略图垫着 → 原图到了淡入替换"，这条规则没变（`DetailPhoto`）。
 */

/** 一张卡最多预取几张（第 2～9 张：加上卡片上画的第一张正好是九宫格，见 [feedhub.app.detail.MaxGridCells]）。 */
internal const val MAX_GRID_PREFETCH = 8

/** 同时最多几个九宫格缩略图在取（和卡片图预取一样克制：省电、也不和正在显示的图抢带宽）。 */
internal const val GRID_PREFETCH_PARALLEL = 2

/**
 * 详情里一个宫格格子的宽度（像素，**上限**）：正文内容宽度 = min([DetailContentMaxWidth], 窗口宽 - 两侧内边距)，
 * 扣掉格子间距后按列数均分。窄屏时详情就是整个窗口，所以这个上限就是实际宽度；宽屏双栏时右栏更窄，
 * 算出来的偏大一点（缩略图只是垫着的那一层，宁可清楚一点）。
 *
 * 纯函数（窗口像素宽 / 密度 / 列数都是参数），离线测。
 */
internal fun detailGridCellWidthPx(windowWidthPx: Int, density: Float, columns: Int): Int {
    if (windowWidthPx <= 0 || density <= 0f || columns <= 0) return 0
    val windowDp = windowWidthPx / density
    val bodyDp = minOf(DetailContentMaxWidth.value, windowDp - 2 * DetailSidePadding.value)
    if (bodyDp <= 0f) return 0
    val cellDp = (bodyDp - GridGap.value * (columns - 1)) / columns
    return (cellDp * density).roundToInt().coerceAtLeast(0)
}

/** 一张要预取的九宫格缩略图：[hint] 里是原图地址 + 缩略图地址 + 真实宽高，尺寸是格子要的像素大小。 */
internal data class GridThumbnail(val hint: CardImageHint, val widthPx: Int, val heightPx: Int) {
    /** 去重的身份：缩略图地址（同一个地址只发一次）。 */
    val key: String get() = hint.thumbnail
}

/**
 * 这张卡要预取哪些图（**纯函数**，界面那边只是照着做）：
 *
 * - 只有**图文**卡片、`media` 里 ≥2 张图才有（视频卡只有一张封面，单图卡也没有第 2 张）；
 * - 第 2 张起，最多 [MAX_GRID_PREFETCH] 张（卡片上画的第一张不在这里，它已经下过了）；
 * - 地址按详情里格子的宽度取档（[detailGridCellWidthPx] + [CardThumbnails.thumbnailFor]），拿不出缩略写法的跳过。
 *
 * @param windowWidthPx 窗口的像素宽（宫格格子的宽度按它算上限）；0 / 负数 = 还不知道，不取。
 * @param density 当前密度（dp → px）。
 */
internal fun gridThumbnailTargets(item: FeedItem, windowWidthPx: Int, density: Float): List<GridThumbnail> {
    if (item.kind == ItemKind.VIDEO || item.kind == ItemKind.LIVE) return emptyList()
    val images = postImages(item)
    if (images.size < 2) return emptyList()
    val cell = detailGridCellWidthPx(windowWidthPx, density, imageGridColumns(images.size))
    if (cell <= 0) return emptyList()
    return gridThumbnailTargets(images.drop(1).take(MAX_GRID_PREFETCH), cell)
}

/**
 * 同上，但**已经挑好的那几张图 + 格子宽度**（多张卡一起算的时候用：地址改写一次过边界）。
 *
 * @param images `(原地址, 宽, 高)`，调用方已经去掉卡片上画的第一张、也截到 [MAX_GRID_PREFETCH]。
 */
internal fun gridThumbnailTargets(images: List<Triple<String, Int?, Int?>>, cell: Int): List<GridThumbnail> {
    if (images.isEmpty() || cell <= 0) return emptyList()
    val bucket = CardThumbnails.bucket(cell)
    val thumbs = CardThumbnails.thumbnailForMany(images.map { it.first }, bucket)
    return images.mapIndexedNotNull { i, (url, w, h) ->
        val thumbnail = thumbs[i] ?: return@mapIndexedNotNull null
        GridThumbnail(CardImageHint(source = url, thumbnail = thumbnail, width = w, height = h), cell, cell)
    }
}

/**
 * 已经取过 / 正在取的九宫格缩略图（**同一张不重复取**）。
 *
 * 取失败、被取消（滚出屏幕 / 退到后台）的会从表里拿掉，以后回到屏幕上还能再取；真的取到了就留着
 * （Coil 的内存 / 磁盘缓存里已经有了，就算留着也没关系）。只留最近 [maxStarted] 条，免得一直涨。
 */
internal class GridThumbnailRequests(private val maxStarted: Int = 512) {
    private val started = object : LinkedHashMap<String, Unit>(128, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Unit>): Boolean = size > maxStarted
    }

    /** 开始取这一张；已经在取 / 已经取过就返回 false（调用方直接跳过）。 */
    fun begin(key: String): Boolean = synchronized(started) {
        if (started.containsKey(key)) return false
        started[key] = Unit
        true
    }

    /** 没取成（失败 / 取消）：从表里拿掉，以后还能再试。 */
    fun failed(key: String) {
        synchronized(started) { started.remove(key) }
    }

    val size: Int get() = synchronized(started) { started.size }

    fun clear() {
        synchronized(started) { started.clear() }
    }

    companion object {
        /** 进程里共用一份（同一张图可能出现在好几张卡上）。 */
        val shared = GridThumbnailRequests()
    }
}

/**
 * 预取活动的记录（只给截图工具 `print:gridthumbs` 和诊断看）：计划了几张、真的取了几张、取消了几个。
 * 不参与界面、不影响缓存；只存计数和最近取到的地址（最多 [MAX_URLS] 条）。
 */
internal object GridPrefetchTrace {
    const val MAX_URLS = 40

    private var plannedTotal = 0
    private var plannedCards = 0
    private var loadedTotal = 0
    private var failedCount = 0
    private var cancelledCount = 0
    private val loadedUrls = LinkedHashSet<String>()

    fun planned(targets: List<GridThumbnail>) {
        if (targets.isEmpty()) return
        synchronized(this) {
            plannedCards++
            plannedTotal += targets.size
        }
    }

    fun loaded(target: GridThumbnail) = synchronized(this) {
        loadedTotal++
        if (loadedUrls.size < MAX_URLS) loadedUrls += target.key
    }

    fun failed() = synchronized(this) { failedCount++ }

    fun cancelled() = synchronized(this) { cancelledCount++ }

    /** 计划 / 取到的张数（测试和诊断用；界面只读 [dump]）。 */
    val plannedCount: Int get() = synchronized(this) { plannedTotal }

    val loadedCount: Int get() = synchronized(this) { loadedTotal }

    fun dump(): String = synchronized(this) {
        buildString {
            append("九宫格预取：计划 $plannedTotal 张（$plannedCards 张卡片）· 取到 $loadedTotal · 取消 $cancelledCount · 失败 $failedCount")
            if (loadedUrls.isNotEmpty()) {
                append("\n  取到的地址（${loadedUrls.size}/$loadedTotal）：")
                loadedUrls.forEach { append("\n    ").append(it) }
            }
        }
    }

    fun clear() = synchronized(this) {
        plannedTotal = 0
        plannedCards = 0
        loadedTotal = 0
        failedCount = 0
        cancelledCount = 0
        loadedUrls.clear()
    }
}

/**
 * 真的去取：一张张排在 [GRID_PREFETCH_PARALLEL] 个并发里，取到的记一笔，失败 / 取消的把"正在取"放开。
 *
 * **只在前台取**：进来时后台就一个都不取（[AppForeground.isForeground]），取到一半退后台就当场取消
 * （[AppForeground.addListener]；和卡片图预取同一个理由——后台安卓的帧时钟停了，靠重组去停来不及）。
 *
 * 取消（[CancellationException]）由调用方的 Job 传进来：卡片滑出屏幕 = Compose 的 effect 被取消。
 *
 * 离线测试直接调它，`load` 换成假的（见 `GridThumbnailsTest`）。
 */
internal suspend fun prefetchGridThumbnails(
    foreground: AppForeground,
    itemKey: String,
    targets: List<GridThumbnail>,
    requests: GridThumbnailRequests = GridThumbnailRequests.shared,
    parallel: Int = GRID_PREFETCH_PARALLEL,
    load: suspend (GridThumbnail) -> Boolean,
) {
    if (targets.isEmpty() || !foreground.isForeground) return
    val self = currentCoroutineContext()[Job]
    val listener: (Boolean) -> Unit = { nowActive -> if (!nowActive) self?.cancel() }
    foreground.addListener(listener)
    try {
        val gate = Semaphore(parallel.coerceAtLeast(1))
        coroutineScope {
            for (target in targets) {
                launch {
                    if (!requests.begin(target.key)) return@launch
                    var ok = false
                    try {
                        gate.withPermit {
                            // 一张图失败不影响别的（预取本来就可以失败）：取消照旧往上抛，其余当没取到
                            ok = try {
                                load(target)
                            } catch (e: CancellationException) {
                                throw e
                            } catch (e: Exception) {
                                false
                            }
                        }
                    } finally {
                        if (!ok) requests.failed(target.key)
                    }
                    if (ok) GridPrefetchTrace.loaded(target) else GridPrefetchTrace.failed()
                }
            }
        }
    } catch (e: CancellationException) {
        GridPrefetchTrace.cancelled()
        throw e
    } finally {
        foreground.removeListener(listener)
    }
}

/**
 * 卡片这边的一行接线（[FeedCard] 里调）：把这张卡第 2～9 张的缩略图交给 Coil。
 *
 * effect 的 key 里带 [AppForeground.generation]：退到后台时这个 effect 被取消，回前台（哪怕 `isForeground`
 * 又变回 true、中间没重组过）key 变了会重新起一次，没取完的接着取。
 */
@Composable
internal fun GridThumbnailPrefetch(itemKey: String, targets: List<GridThumbnail>, foreground: AppForeground) {
    if (targets.isEmpty()) return
    val context = LocalPlatformContext.current
    val active = foreground.isForeground
    val generation = foreground.generation
    LaunchedEffect(itemKey, targets, active, generation) {
        if (!active) return@LaunchedEffect
        val loader = SingletonImageLoader.get(context)
        prefetchGridThumbnails(foreground, itemKey, targets) { target -> loadGridThumbnail(loader, context, target) }
    }
}

/** 取一张：尺寸 / 缩放 / 精度和详情里那个格子（`AsyncImage` + `ContentScale.Crop`）一致，内存缓存才对得上。 */
private suspend fun loadGridThumbnail(loader: ImageLoader, context: PlatformContext, target: GridThumbnail): Boolean {
    val request = ImageRequest.Builder(context)
        .data(target.hint.thumbnail)
        .size(target.widthPx, target.heightPx)
        .scale(Scale.FILL)
        .precision(Precision.INEXACT)
        .build()
    return loader.execute(request) is SuccessResult
}

/** 现在窗口有多宽（像素）：宫格格子的宽度按它算上限。0 = 还不知道（不取）。 */
@Composable
internal fun windowWidthPx(): Int = LocalWindowInfo.current.containerSize.width
