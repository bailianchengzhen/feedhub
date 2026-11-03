package feedhub.app.feed

import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import feedhub.app.ui.compactCount
import feedhub.app.ui.duration as formatDuration
import feedhub.core.FeedItem
import feedhub.core.TimePrecision
import feedhub.present.RustPresent

/**
 * **一屏卡片的文案**。
 *
 * 卡片的元信息行 / 角标 / 点赞数都是"每次重组都会问一遍"的纯函数（时间文案、计数、时长）。它们现在
 * 在 Rust（`feedhub-core/src/present/`），**逐条过 FFI 是不行的**：一屏 40 张卡、每张 5 处，
 * 一次重组就是 200 次边界调用。
 *
 * 做法：**一屏一次**——列表变化时 [cardTextsOf] 把这一屏要用的字一次算好
 * （`RustPresent.Batch` 只过 3 次边界：时间、计数、时长），重组时卡片只查表。
 * 时间文案的"现在"就是这一屏算好的那一刻：列表刷新 / 界面重建时重算。
 *
 * 没提供 [LocalCardTexts] 的地方（详情 / 收藏 / 历史那些面）走 [RustPresent] 的单条入口。
 */
class CardTexts internal constructor(
    private val times: Map<String, String>,
    private val counts: Map<Long, String>,
    private val durations: Map<Int, String>,
) {
    /** 卡片的发布时间（没有就是 null）。 */
    fun time(item: FeedItem): String? {
        val at = item.publishedAt ?: return null
        return times[timeKey(at, item.publishedAtPrecision)]
    }

    /** 计数（"1.2万"）。查不到就退回单条入口（正常不会：这一屏的都算过了）。 */
    fun count(n: Long): String = counts[n] ?: compactCount(n)

    /** 时长（"12:34"）。 */
    fun duration(sec: Int): String = durations[sec] ?: formatDuration(sec)

    internal companion object {
        fun timeKey(epochSec: Long, precision: TimePrecision): String = "$epochSec:${precision.name}"
    }
}

/** 当前这一屏卡片的文案；null = 没有批量（单条入口）。 */
val LocalCardTexts = staticCompositionLocalOf<CardTexts?> { null }

/**
 * 把一屏 [items] 的卡片文案算好，供给下面所有 [FeedCard]。
 *
 * `remember(items)`：列表换了一份（刷新 / 换页 / 换面）才算一次；同一份列表的重组不碰 Rust。
 * 空列表直接透传（不建 batch，也不加载 `.so`）。
 */
@Composable
fun ProvideCardTexts(items: List<FeedItem>, content: @Composable () -> Unit) {
    if (items.isEmpty()) {
        content()
        return
    }
    val texts = remember(items) { cardTextsOf(items) }
    CompositionLocalProvider(LocalCardTexts provides texts, content = content)
}

/**
 * 一屏卡片的文案（纯函数，`CardTextsTest` 离线测）：把这一屏真正会用到的三样收集起来，
 * 各过**一次**边界。
 */
fun cardTextsOf(items: List<FeedItem>, nowMs: Long = System.currentTimeMillis()): CardTexts {
    val batch = RustPresent.Batch(nowMs)

    // 时间：每张卡自己的 (epoch, precision)
    val timeRequests = ArrayList<Pair<Long, TimePrecision>>()
    for (item in items) {
        val at = item.publishedAt ?: continue
        timeRequests += at to item.publishedAtPrecision
    }
    val times = HashMap<String, String>(timeRequests.size * 2)
    val timeTexts = if (timeRequests.isEmpty()) emptyList() else batch.relativeTimes(timeRequests)
    timeRequests.forEachIndexed { i, (at, precision) ->
        times[CardTexts.timeKey(at, precision)] = timeTexts[i]
    }

    // 计数：卡片上会出现的所有数字（播放 / 弹幕 / 点赞 / 评论）
    val counts = LinkedHashSet<Long>()
    for (item in items) {
        item.stats.views?.let(counts::add)
        item.stats.danmaku?.let(counts::add)
        item.stats.likes?.let(counts::add)
        item.stats.comments?.let(counts::add)
    }
    val countMap = HashMap<Long, String>(counts.size * 2)
    if (counts.isNotEmpty()) {
        val list = counts.toList()
        val texts = batch.compactCounts(list)
        list.forEachIndexed { i, n -> countMap[n] = texts[i] }
    }

    // 时长：视频卡角上那个
    val secs = LinkedHashSet<Int>()
    for (item in items) {
        val video = item.media.firstOrNull { it is feedhub.core.Media.Video } as? feedhub.core.Media.Video
        video?.durationSec?.let(secs::add)
    }
    val durationMap = HashMap<Int, String>(secs.size * 2)
    if (secs.isNotEmpty()) {
        val list = secs.toList()
        val texts = batch.durations(list)
        list.forEachIndexed { i, sec -> durationMap[sec] = texts[i] }
    }

    return CardTexts(times, countMap, durationMap)
}
