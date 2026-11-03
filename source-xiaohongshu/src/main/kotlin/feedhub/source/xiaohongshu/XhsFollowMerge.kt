package feedhub.source.xiaohongshu

import feedhub.core.FeedItem
import kotlinx.serialization.Serializable

/*
 * 小红书"关注"流的合并逻辑：网页没有关注流，是拼出来的——关注的人各自的主页第一页（`user_posted`）按发布时间合并。
 * 这个文件里全是纯逻辑（没有网络、没有存储），照贴吧的 ForumMerge / ForumFreshness 写，但比它简单：
 *
 * - 贴吧会为了保证顺序去翻某个吧的后续页；这里**不翻**：每个作者只缓存第一页（最新的 [XhsFollowFeed.KEEP_PER_AUTHOR] 条），
 *   加载更多只是在缓存里继续往下排。所以流是"已经拉到的所有笔记，按发布时间倒序"，还没拉过的作者（每次刷新最多拉
 *   [XhsFollowFeed.MAX_AUTHORS_PER_REFRESH] 个，其余下次再拉）和缓存之外更老的笔记不在里面——这是有意的，和请求量上限是同一个取舍。
 * - 置顶笔记在作者主页里排在最前，但它们的 `time` 是自己真实的发布时间。排序只看 `time`，
 *   置顶不是特殊情况，会按自己的时间落到该在的位置，不会总顶在最前面。
 * - 哪些作者该重新拉第一页：[XhsFollowFreshness]。
 */

/** 笔记的排序键：先比发布时间（秒），同一秒的比笔记 id（24 位小写十六进制，前 8 位是创建时间，所以大致随发布递增）。越大越新。 */
internal data class NoteKey(val time: Long, val id: String) : Comparable<NoteKey> {
    override fun compareTo(other: NoteKey): Int =
        if (time != other.time) time.compareTo(other.time) else id.compareTo(other.id)

    /** 游标里的写法：`<秒>:<笔记 id>`。 */
    fun encode(): String = "$time:$id"

    companion object {
        fun decode(s: String): NoteKey? {
            val time = s.substringBefore(':', "").toLongOrNull() ?: return null
            val id = s.substringAfter(':', "").takeIf { it.isNotEmpty() } ?: return null
            return NoteKey(time, id)
        }
    }
}

/** 缓冲里的笔记一定有发布时间（解析时没有时间的已经丢掉）。 */
internal val FeedItem.noteKey: NoteKey
    get() = NoteKey(publishedAt ?: error("笔记缺少发布时间 $id"), id)

/**
 * 一个关注的作者的缓冲和调度信息。不可变，改动用 copy。会被序列化存起来（见 [XhsFollowStore]）。
 *
 * @property xsecToken 关注列表项里的 token（`user_posted` 要用）。
 * @property items 第一页的笔记（含置顶），按 [NoteKey] 严格降序、无重复，最多 [XhsFollowFeed.KEEP_PER_AUTHOR] 条。
 * @property fetchedAt 上次拉第一页的时间（毫秒），0 = 从没拉过。
 * @property unread `followings/extra` 最近一次说的"未看笔记数"。官方按它自己的浏览记录算，我们不上报浏览记录（`metrics_report` /
 *   `history/report_web`），所以光靠我们看过不一定会减少；
 *   可能是取详情时服务端记了已读（只观察到一次，没确认）。所以它既不能当"有没有新笔记"的直接依据，也可能自己变小，
 *   只看"比上次拉的时候涨没涨"，见 [XhsFollowFreshness.isDue] 和 [withUnread]。
 * @property unreadAtFetch 上次拉第一页时的 [unread]。之后 [unread] 比它大 = 这之后又有新笔记。
 * @property closedUntil 这个人的主页读不了（接口报错：账号注销、token 失效等）时，到这个时间（毫秒）之前不再请求。
 */
@Serializable
internal data class AuthorState(
    val id: String,
    val name: String,
    val xsecToken: String? = null,
    val items: List<FeedItem> = emptyList(),
    val fetchedAt: Long = 0,
    val unread: Int = 0,
    val unreadAtFetch: Int = 0,
    val closedUntil: Long = 0,
) {
    fun isClosed(now: Long) = closedUntil > now

    /** 上次拉过之后新增的"未看"数（没拉过时就是全部）。 */
    val newSinceFetch: Int get() = (unread - unreadAtFetch).coerceAtLeast(0)

    /**
     * 记下 `followings/extra` 新给的未看数 [n]。数变小（用户在官方 App / 网页里看过了）时，"上次拉的时候的数"也跟着降到 [n]，
     * 免得 2 → 0（看过了）→ 1（又发了一篇）被当成"没变"而漏拉。
     */
    fun withUnread(n: Int): AuthorState = copy(unread = n, unreadAtFetch = minOf(unreadAtFetch, n))
}

/**
 * 每次刷新哪些作者该重拉第一页。
 *
 * 一个作者要拉，当且仅当：
 * 1. 从没拉过；或者
 * 2. `followings/extra` 说的未看数比上次拉的时候多了（又发了新笔记）；或者
 * 3. 距上次拉超过 [STALE_MS]（一天）。
 *
 * 不靠"未看数 > 0"：它不会因为我们拉过主页而减少（我们不上报浏览记录），靠它会每次刷新都重拉同一批人。所以已经拉过、未看数没涨的作者不拉，
 * 就像"最新一条没变"。另外设最短间隔 [MIN_GAP_MS]，连着下拉刷新时不会反复打同一个人。
 * 拉的顺序：有新笔记的在前（新得多的更前），然后是没拉过的，然后是最久没拉的；一次最多 [limit] 个，剩下的下次再拉。
 */
internal object XhsFollowFreshness {
    const val STALE_MS = 24 * 3_600_000L
    const val MIN_GAP_MS = 60_000L

    fun isDue(a: AuthorState, now: Long): Boolean {
        if (a.isClosed(now) || a.xsecToken.isNullOrEmpty()) return false
        if (a.fetchedAt <= 0L) return true
        val age = now - a.fetchedAt
        if (age < MIN_GAP_MS) return false
        return age >= STALE_MS || a.newSinceFetch > 0
    }

    fun due(states: Collection<AuthorState>, now: Long, limit: Int): List<String> =
        states.filter { isDue(it, now) }
            .sortedWith(
                compareBy<AuthorState> { if (it.newSinceFetch > 0) 0 else 1 }
                    .thenByDescending { it.newSinceFetch }
                    .thenBy { it.fetchedAt },
            )
            .take(limit)
            .map { it.id }
}

/** 合并出的一页。[more] 为 true 表示后面（缓存里）至少还有一条。 */
internal class MergePage(val items: List<FeedItem>, val more: Boolean)

internal object XhsFollowMerge {
    /**
     * 并入新拉到的第一页：以新的为准（还在的被新版本替换，不在的是被删了），按 [NoteKey] 降序排，只留最新的 [keep] 条。
     * [unread] 是这次拉的时候 `followings/extra` 给的未看数，记下来当"已经拉过了"的标记。
     */
    fun integrate(a: AuthorState, fresh: List<FeedItem>, now: Long, keep: Int): AuthorState {
        val items = fresh.distinctBy { it.id }.sortedByDescending { it.noteKey }.take(keep)
        return a.copy(items = items, fetchedAt = now, unreadAtFetch = a.unread, closedUntil = 0)
    }

    /**
     * 取比 [before] 更旧的 [limit] 条（[before] 为 null = 从最新开始）：所有作者的缓冲合在一起，按发布时间倒序。
     * 不联网——要新的靠刷新时重拉第一页。
     */
    fun nextPage(states: Collection<AuthorState>, before: NoteKey?, limit: Int): MergePage {
        val older = states.asSequence()
            .flatMap { it.items.asSequence() }
            .filter { before == null || it.noteKey < before }
            .distinctBy { it.id }
            .sortedByDescending { it.noteKey }
            .take(limit + 1)
            .toList()
        return MergePage(older.take(limit), older.size > limit)
    }
}
