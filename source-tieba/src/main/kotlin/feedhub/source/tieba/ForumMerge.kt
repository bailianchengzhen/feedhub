package feedhub.source.tieba

import feedhub.core.toHttps
import feedhub.core.FollowTarget
import feedhub.core.FollowKind
import feedhub.core.FeedItem
import kotlinx.serialization.Serializable

/*
 * 贴吧"关注"流的合并逻辑：把关注的几十个吧各自按发帖时间倒序的帖子列表，合并成一条按发帖时间倒序的流。
 * 这个文件里全是纯逻辑（没有网络、没有存储），单测用假的 [ForumPager] 驱动。
 *
 * 思路是"惰性 k 路归并"：
 * - 每个吧有一个缓冲 [ForumState.items]（已经抓到的帖子，新的在前）。缓冲是服务端列表从头开始的一段连续前缀，
 *   最旧那条的键叫"前沿"（frontier）；前沿之下的帖子是未知的，除非这个吧已经 [ForumState.exhausted]。
 * - 要从全局取"下一条"（比边界 before 更旧的最新一条）：先看各吧缓冲里低于边界的最新帖子，取其中最新的 best；
 *   然后看有没有哪个吧"未知区域可能比 best 更新"——没耗尽、前沿比 best 还新（包括缓冲里一条低于边界的都没有）。
 *   有的话先抓那个吧的下一页，再重新比；没有才能放心取 best。
 *   所以冷清的吧（缓冲里的帖子跨好几个月）几乎永远不会触发额外请求，繁忙的吧才会被翻页。
 * - 取够一页之后再"偷看"一条，保证返回的 [MergePage.more] 为 true 时下一页一定非空。
 *
 * 排序键见 [ThreadKey]：发帖时间，同秒按 tid。置顶帖和直播推广卡片在解析时就丢掉了（见 TiebaParser.frsPage），
 * 不会进缓冲，所以乱序的置顶帖不可能破坏顺序。
 */

/** 帖子的排序键：先比发帖时间（秒），同一秒的比 tid（tid 随发帖递增）。越大越新。 */
internal data class ThreadKey(val time: Long, val tid: Long) : Comparable<ThreadKey> {
    override fun compareTo(other: ThreadKey): Int =
        if (time != other.time) time.compareTo(other.time) else tid.compareTo(other.tid)

    /** 游标里的写法。 */
    fun encode(): String = "$time:$tid"

    companion object {
        val MAX = ThreadKey(Long.MAX_VALUE, Long.MAX_VALUE)

        fun decode(s: String): ThreadKey? {
            val time = s.substringBefore(':', "").toLongOrNull() ?: return null
            val tid = s.substringAfter(':', "").toLongOrNull() ?: return null
            return ThreadKey(time, tid)
        }
    }
}

/** 缓冲里的帖子一定有 tid 和发帖时间（入缓冲前过滤过）。 */
internal val FeedItem.threadKey: ThreadKey
    get() = ThreadKey(publishedAt ?: error("帖子缺少发帖时间 $id"), id.toLong())

/**
 * 关注的吧，来自 `forumGuide`。[dayThreads] 是服务端给的"今日新帖数"；[avatar]、[memberCount]（关注人数）、[threadCount]（主题帖数）
 * 给"我的 → 关注管理"用（[FollowTarget]），关注流用不到。旧缓存里没有，读出来是 null。
 */
@Serializable
internal data class FollowedForum(
    val id: Long,
    val name: String,
    val dayThreads: Int = 0,
    val avatar: String? = null,
    val memberCount: Long? = null,
    val threadCount: Long? = null,
)

/**
 * 关注的吧 → 关注列表里的一项。描述是"关注 N · 主题 N"（过万写成"x.x万"），地址是吧的网页。
 */
internal fun FollowedForum.toFollowTarget(): FollowTarget {
    val desc = listOfNotNull(
        memberCount?.let { "关注 ${forumCountText(it)}" },
        threadCount?.let { "主题 ${forumCountText(it)}" },
    ).joinToString(" · ").ifEmpty { null }
    return FollowTarget(
        platform = TIEBA,
        kind = FollowKind.FORUM,
        id = id.toString(),
        name = name,
        avatar = avatar?.toHttps(),
        description = desc,
        url = "https://tieba.baidu.com/f?kw=" + java.net.URLEncoder.encode(name, "UTF-8"),
    )
}

private fun forumCountText(n: Long): String =
    if (n < 10_000) n.toString() else "%.1f万".format(n / 10_000.0).replace(".0万", "万")

/**
 * 一个吧的缓冲和调度信息。不可变，改动用 copy。会被序列化存起来（见 ForumStore）。
 *
 * @property items 已抓到的普通主题帖，新的在前，按 [ThreadKey] 严格降序、无重复。
 * @property tailPos 服务端列表里"缓冲末尾"对应的位置数（1 起算的位置计数，包含置顶帖），下一页就是 `tailPos / rn + 1`。
 *   不用 `items.size` 推算，因为置顶帖、直播推广卡片也占位置，而它们不在 [items] 里。
 * @property exhausted 缓冲末尾已经是这个吧的最后一条（再往下翻是空页）。
 * @property headFetchedAt 上次抓第一页的时间（毫秒），0 = 从没抓过。
 * @property perDay 上次抓第一页时从帖子时间估出来的"每天新帖数"，抓不出来是 null。
 * @property closedUntil 吧不可用（例如 340001"神秘升级中"）时，到这个时间之前不再请求（毫秒）。
 */
@Serializable
internal data class ForumState(
    val id: Long,
    val name: String,
    val dayThreads: Int = 0,
    val items: List<FeedItem> = emptyList(),
    val tailPos: Int = 0,
    val exhausted: Boolean = false,
    val headFetchedAt: Long = 0,
    val perDay: Double? = null,
    val closedUntil: Long = 0,
) {
    fun isClosed(now: Long) = closedUntil > now

    /**
     * 未知区域的上界：缓冲之下的帖子键都严格小于它。没抓过 = 无穷大；已耗尽或暂时关闭 = 没有未知区域（null）。
     */
    fun frontier(now: Long): ThreadKey? = when {
        exhausted || isClosed(now) -> null
        items.isEmpty() -> ThreadKey.MAX
        else -> items.last().threadKey
    }

    /** 缓冲里严格比 [bound] 旧的最新一条；[bound] 为 null 表示不限。 */
    fun newestBelow(bound: ThreadKey?): FeedItem? =
        if (bound == null) items.firstOrNull() else items.firstOrNull { it.threadKey < bound }
}

/** 抓回来的一页：只含普通主题帖（置顶帖、推广卡片已丢），新的在前。[last] = 这是最后一页（再往后没有了）。 */
internal class ForumPage(val items: List<FeedItem>, val last: Boolean)

/** 这个吧现在读不了（例如 340001"神秘升级中"），不是网络问题或风控。[cause] 是接口返回的原始错误。 */
internal class ForumClosedException(message: String, cause: Throwable? = null) : Exception(message, cause)

internal interface ForumPager {
    /** 服务端每页的位置数（`rn`）。 */
    val pageSize: Int

    /** 取 [forum] 的第 [page] 页（1 起）。吧本身读不了时抛 [ForumClosedException]，网络错误、风控等其他异常原样抛。 */
    suspend fun fetch(forum: ForumState, page: Int): ForumPage
}

/** 合并出的一页。[more] 为 true 表示后面至少还有一条。 */
internal class MergePage(val items: List<FeedItem>, val more: Boolean)

internal class ForumMerger(
    private val pager: ForumPager,
    private val now: () -> Long = System::currentTimeMillis,
) {
    /** 连续这么多个吧读不了就认为不是某个吧的问题，直接把原始错误抛出去，免得把几十个吧都标成关闭。 */
    private var failures = 0

    /** 从上次 [resetStats] 以来真正发出去的抓取请求数（调用方据此决定要不要存盘）。 */
    var fetches = 0
        private set

    fun resetStats() {
        fetches = 0
    }

    /**
     * 让 [states] 里这些吧的第一页变新（调度器认为该刷新的）。
     * 单个吧读不了（[ForumClosedException]）时标记它暂时关闭，继续下一个；连续 [MAX_FAILURES] 个读不了就把原始错误抛出。
     */
    suspend fun refreshHeads(states: MutableMap<Long, ForumState>, ids: List<Long>, deadline: Long = Long.MAX_VALUE) {
        failures = 0
        for (id in ids) {
            // 预算到点就不再发新请求；剩下的吧下次刷新还在 due 里（排序把活跃的放前面）
            if (now() >= deadline) break
            fetchInto(states, id, page = 1)
        }
    }

    /**
     * 取比 [before] 更旧的 [limit] 条（[before] 为 null = 从最新开始）。按需抓取各吧的后续页。
     * 状态更新写回 [states]；中途出错时已经完成的抓取也留在 [states] 里，调用方照常存盘即可。
     */
    suspend fun nextPage(states: MutableMap<Long, ForumState>, before: ThreadKey?, limit: Int): MergePage {
        failures = 0
        val out = ArrayList<FeedItem>(limit)
        var bound = before
        while (out.size < limit) {
            val item = selectNext(states, bound) ?: return MergePage(out, more = false)
            out += item
            bound = item.threadKey
        }
        return MergePage(out, more = selectNext(states, bound) != null)
    }

    /** 比 [bound] 旧的全局最新一条；必要时先抓取，保证不会有更新的帖子藏在某个吧的未知区域里。 */
    private suspend fun selectNext(states: MutableMap<Long, ForumState>, bound: ThreadKey?): FeedItem? {
        while (true) {
            val t = now()
            var best: FeedItem? = null
            for (f in states.values) {
                val c = f.newestBelow(bound) ?: continue
                if (best == null || c.threadKey > best.threadKey) best = c
            }
            val bestKey = best?.threadKey
            // 未知区域可能比 best 新的吧：前沿 > best（best 不存在时，所有没耗尽的吧都算）。前沿最高的先抓。
            var blocker: ForumState? = null
            var blockerFrontier: ThreadKey? = null
            for (f in states.values) {
                val frontier = f.frontier(t) ?: continue
                if (bestKey != null && frontier <= bestKey) continue
                if (blockerFrontier == null || frontier > blockerFrontier) {
                    blocker = f
                    blockerFrontier = frontier
                }
            }
            if (blocker == null) return best
            val f = blocker
            fetchInto(states, f.id, page = if (f.items.isEmpty()) 1 else f.tailPos / pager.pageSize + 1)
        }
    }

    private suspend fun fetchInto(states: MutableMap<Long, ForumState>, id: Long, page: Int) {
        val f = states[id] ?: return
        fetches++
        val result = try {
            pager.fetch(f, page)
        } catch (e: ForumClosedException) {
            if (++failures >= MAX_FAILURES) throw e.cause ?: e
            states[id] = f.copy(closedUntil = now() + CLOSED_FOR_MS)
            return
        }
        failures = 0
        states[id] = if (page == 1) integrateHead(f, result, now(), pager.pageSize) else integrateTail(f, page, result, pager.pageSize)
    }

    companion object {
        const val MAX_FAILURES = 3
        const val CLOSED_FOR_MS = 24 * 3_600_000L

        /** 并入一个吧新抓的第一页（[page] 是整个第一页）。 */
        fun integrateHead(f: ForumState, page: ForumPage, now: Long, pageSize: Int): ForumState {
            val fresh = page.items
            val perDay = ForumFreshness.observedPerDay(fresh) ?: f.perDay
            if (fresh.isEmpty()) {
                // 第一页一条普通帖子都没有：没东西可看，也别再往下翻
                return f.copy(items = emptyList(), tailPos = 0, exhausted = true, headFetchedAt = now, perDay = perDay)
            }
            val old = f.items
            val freshOldest = fresh.last().threadKey
            val oldIds = old.mapTo(HashSet()) { it.id }
            // 新抓的页和旧缓冲接得上：有相同的帖子，或者新页一直翻到了旧缓冲最新那条之下
            val connected = old.isNotEmpty() &&
                (fresh.any { it.id in oldIds } || freshOldest <= old.first().threadKey)
            if (!connected) {
                // 新帖子多到一页装不下，中间的空档不知道是什么：丢掉旧缓冲，只留这一页
                return f.copy(
                    items = fresh,
                    tailPos = if (page.last) 0 else pageSize,
                    exhausted = page.last,
                    headFetchedAt = now,
                    perDay = perDay,
                )
            }
            // 新页覆盖的时间范围（>= freshOldest）里，旧缓冲的帖子以新页为准：还在的被新版本替换，不在的是被删了
            val below = old.filter { it.threadKey < freshOldest }
            val freshIds = fresh.mapTo(HashSet()) { it.id }
            val purged = old.count { it.threadKey >= freshOldest && it.id !in freshIds }
            val tailPos = when {
                page.last -> 0
                // 新页一直翻到了旧缓冲末尾之下：合并后的末尾就是新页的末尾
                below.isEmpty() -> pageSize
                // 否则旧缓冲末尾被新增的帖子往下顶、被删掉的往上提
                else -> {
                    val added = fresh.count { it.id !in oldIds && it.threadKey > old.first().threadKey }
                    maxOf(f.tailPos + added - purged, pageSize)
                }
            }
            return f.copy(
                items = fresh + below,
                tailPos = tailPos,
                exhausted = f.exhausted || page.last,
                headFetchedAt = now,
                perDay = perDay,
            )
        }

        /** 并入后面的一页（第 [pageNo] 页，>= 2）。和缓冲重叠的部分按帖子去重。 */
        fun integrateTail(f: ForumState, pageNo: Int, page: ForumPage, pageSize: Int): ForumState {
            val have = f.items.mapTo(HashSet()) { it.id }
            val frontier = f.items.lastOrNull()?.threadKey
            // 只收比前沿更旧的：整页都落在缓冲范围内（前面有大量新帖把列表顶下去了）时没有新东西，但 tailPos 照样前进
            val older = page.items.filter { it.id !in have && (frontier == null || it.threadKey < frontier) }
            return f.copy(
                items = f.items + older,
                tailPos = if (page.last) f.tailPos else maxOf(f.tailPos, pageNo * pageSize),
                exhausted = page.last,
            )
        }

        /**
         * 存盘前压缩：每个吧只留最新的 [keep] 条。被丢掉的那部分变回"未知区域"，tailPos 往回退相应的位置数，
         * 下次需要时从对应的页重新抓。
         */
        fun trimmed(f: ForumState, keep: Int): ForumState {
            if (f.items.size <= keep) return f
            val dropped = f.items.size - keep
            return f.copy(
                items = f.items.take(keep),
                tailPos = (f.tailPos - dropped).coerceAtLeast(0),
                exhausted = false,
            )
        }
    }
}
