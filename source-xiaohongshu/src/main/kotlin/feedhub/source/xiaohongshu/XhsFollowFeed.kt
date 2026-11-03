package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.Cursor
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.KeyValueStore
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.IOException

/**
 * 小红书的关注流（网页版没有，拼出来的）：**关注的人最近发的笔记，按发布时间倒序**。要登录。
 *
 * 一次刷新（游标为空）：
 * 1. **关注列表** `GET user/followings/self`：翻页取全（`has_more` 在最后一页之前就可能是 true，所以也看是不是空页 / 游标没变），
 *    存进 `ctx.store`，一天刷新一次（列表项里的 `xsec_token` 会过期）；取不到但有旧缓存就用旧的。
 * 2. **先只拉一部分作者的第一页** `GET user_posted`（`xsec_source=pc_followed`，token 用关注列表项里的）：选哪些见 [XhsFollowFreshness]，
 *    **每次最多 [MAX_AUTHORS_PER_REFRESH] 个**，剩下的下次刷新再拉。拉到的笔记缓存进 `ctx.store`（[XhsFollowStore]）。
 *    某个作者的主页读不了（接口报错）就把他关 [CLOSED_FOR_MS] 再继续，连着 [MAX_FAILURES] 个读不了就把错误抛出去。
 *    这一步先做（有预算的话），保证首刷 / 关注 >10 人时页面一定有作者内容。
 * 3. **谁有新笔记** `GET user/followings/extra`：把关注的人的 id 用**逗号**分隔，每 [EXTRA_BATCH] 个一批问"未看笔记数"
 *    两次之间至少隔 [EXTRA_MIN_GAP_MS]；**只有第 2 步之后还有剩余预算才问**，
 *    没问完 `extraAt` 不推进。这一步出错（非风控）就不管它，只按"多久没拉"来选。
 * 4. **按 `time` 归并**（[XhsFollowMerge]）。
 *
 * 翻页（游标非空）不联网，只在缓存里继续往下排。游标 = 上一页最后一条的 `<发布时间秒>:<笔记 id>`（[NoteKey]），不依赖进程内状态。
 *
 * 风控（461 / 471 / 300011 / 300012）原样往外抛，不重试；抛之前已经拉到的照常存盘。
 * 卡片 id 就是笔记 id，和推荐流一致，两条流能按 key 去重；详情入口 `<笔记 id>@pc_user@<笔记自己的 xsec_token>`。
 */
internal class XhsFollowFeed(
    /**
     * "取一页"的接缝：关注列表 / extra / user_posted 三个请求怎么发、怎么解析。
     * 调度、缓存、存盘、表情表都在这个类里，两种实现（Kotlin 参照 / Rust 平台对象）跑同一份调度代码。
     */
    private val feed: XhsFeedApi,
    private val store: KeyValueStore,
    /** 有没有登录（`a1` + `web_session`）；和改造前一样先判再发请求。 */
    private val hasLogin: () -> Boolean,
    /** 自己的 user_id（接口参数）：已经存过就直接用，否则调 `user/me`。 */
    private val selfId: suspend () -> String,
    /** 表情表（`[笑哭R]` → 图片地址），参数是要检查的文字。 */
    private val emotesFor: suspend (String) -> Map<String, String>,
    private val now: () -> Long = System::currentTimeMillis,
    private val maxAuthorsPerRefresh: Int = MAX_AUTHORS_PER_REFRESH,
    /**
     * 一次刷新在"拉作者第一页 + 问未看数"上的预算（毫秒）。**先拉作者，再用剩余时间问 unread**：
     * 每发一个请求前看一眼，到点就不再发新的；已经发出的请求会正常跑完（最坏再加一次请求的等待 + RTT）。
     * 这样无缓存首刷 / 关注超过 10 人时都不会被 extra 挤掉作者；unread 只影响下一轮 due 的排序。
     * 剩下的作者下次刷新接着拉（due 的排序本来就把没拉过的、最久没拉的放前面）。
     */
    private val refreshBudgetMs: Long = HEAD_REFRESH_BUDGET_MS,
) {
    private val platform = XHS
    private val lock = Mutex()

    /** 进程内一份（第一次用时从 `ctx.store` 读）。 */
    private var cache: XhsFollowStore.Authors? = null

    suspend fun page(cursor: Cursor?): Page<FeedItem> {
        val before = cursor?.let { NoteKey.decode(it.value) ?: throw ParseException(platform, "关注流游标无效") }
        if (!hasLogin()) throw NotLoggedInException(platform)
        return lock.withLock {
            val loaded = cache ?: XhsFollowStore.loadAuthors(store).also { cache = it }
            val states = loaded.states.toMutableMap()
            var extraAt = loaded.extraAt
            var dirty = false
            try {
                dirty = syncUsers(states, allowRefresh = before == null)
                if (before == null) {
                    val deadline = now() + refreshBudgetMs
                    // 先拉作者：首刷 / 关注 >10 人时页面一定有内容；unread 是辅助信息，只有还剩预算才问，
                    // 问到多少算多少（extraAt 没问完就不推进），它不挡作者。
                    val fetched = fetchDue(states, deadline)
                    if (now() < deadline) {
                        extraAt = refreshUnread(states, extraAt, deadline)
                        // unread 现在是在作者拉取之后拿到的。这轮拉过的作者要把当前 unread 记成"拉取时已看到"，
                        // 否则下一轮会误判成"有新笔记"，每隔一轮多拉一次。
                        for (id in fetched) {
                            val s = states[id] ?: continue
                            states[id] = s.copy(unreadAtFetch = s.unread)
                        }
                    }
                    dirty = true
                }
                val page = XhsFollowMerge.nextPage(states.values, before, PAGE_SIZE)
                Page(page.items, if (page.more) Cursor(page.items.last().noteKey.encode()) else null)
            } finally {
                // 中途出错（风控、断网）时已经拉到的也存下来。纯缓存翻页（没联网、状态没变）不重写整份缓存。
                val saved = XhsFollowStore.Authors(extraAt, states)
                cache = saved
                if (before == null || dirty) XhsFollowStore.saveAuthors(store, saved)
            }
        }
    }

    // ---------- 关注列表 ----------

    /**
     * 让 [states] 和关注列表一致：新关注的加进来，取关的删掉，token 换新（token 变了，之前"读不了"的记录作废）。
     * 列表缓存一天；[allowRefresh] 为 false（翻页）时只要有缓存就不联网。联网失败（IOException）但有旧缓存时用旧的。
     * 返回这次是否联网取了关注列表（= 状态可能变了，需要落盘）。
     */
    private suspend fun syncUsers(states: MutableMap<String, AuthorState>, allowRefresh: Boolean): Boolean {
        val cached = XhsFollowStore.loadUsers(store)
        val stale = cached == null || (allowRefresh && now() - cached.at >= LIST_TTL_MS)
        var refreshed = false
        val list = if (!stale) cached else try {
            XhsFollowStore.UserList(now(), fetchFollowings()).also {
                XhsFollowStore.saveUsers(store, it)
                refreshed = true
            }
        } catch (e: IOException) {
            cached ?: throw e
        }
        val ids = list.users.mapTo(HashSet()) { it.id }
        states.keys.retainAll(ids)
        for (u in list.users) {
            val old = states[u.id]
            states[u.id] = when {
                old == null -> AuthorState(u.id, u.name, u.xsecToken)
                old.xsecToken != u.xsecToken -> old.copy(name = u.name, xsecToken = u.xsecToken, closedUntil = 0)
                else -> old.copy(name = u.name)
            }
        }
        return refreshed
    }

    private suspend fun fetchFollowings(): List<FollowedAuthor> {
        val me = selfId()
        val out = LinkedHashMap<String, FollowedAuthor>()
        var cursor = ""
        for (pageNo in 1..MAX_LIST_PAGES) {
            // 参数名和顺序照网页（请求那一半在 [XhsFeedApi] 里）
            val page = feed.followings(me, cursor, now())
            page.users.forEach { out.putIfAbsent(it.id, it) }
            val next = page.cursor
            if (!page.hasMore || page.users.isEmpty() || next.isNullOrEmpty() || next == cursor) break
            cursor = next
        }
        return out.values.toList()
    }

    // ---------- 谁有新笔记 ----------

    /**
     * 问 `followings/extra`，把未看数记进 [states]。返回这次问的时间（没问完 / 没问就是原来的 [extraAt]）。
     * 只在 [fetchDue] 之后、预算还有剩余时调用；它只影响下一轮 due 的排序，不应挤掉当前的作者。
     */
    private suspend fun refreshUnread(states: MutableMap<String, AuthorState>, extraAt: Long, deadline: Long): Long {
        val t = now()
        if (t - extraAt < EXTRA_MIN_GAP_MS) return extraAt
        val me = selfId()
        val ids = states.values.filter { !it.isClosed(t) }.map { it.id }
        var asked = false
        for (batch in ids.chunked(EXTRA_BATCH)) {
            // 到预算就停；extraAt 不往后推，下次刷新接着问，没问完的批次不会被 2 分钟间隔挡住
            if (asked && now() >= deadline) return extraAt
            asked = true
            val unread = try {
                feed.unread(me, batch, now())
            } catch (_: ApiException) {
                continue // 这个接口只是帮着挑人，出错了就按"多久没拉"来
            } catch (_: ParseException) {
                continue
            }
            for (id in batch) states[id] = states.getValue(id).withUnread(unread[id] ?: 0)
        }
        return t
    }

    // ---------- 作者的第一页 ----------

    /** 返回当前成功拉到第一页的作者 id（给 unread 的 unreadAtFetch 对齐用）。 */
    private suspend fun fetchDue(states: MutableMap<String, AuthorState>, deadline: Long): Set<String> {
        var failures = 0
        val fetched = LinkedHashSet<String>()
        for (id in XhsFollowFreshness.due(states.values, now(), maxAuthorsPerRefresh)) {
            // 预算到点就不再发新请求；没拉到的下次刷新还在 due 里
            if (now() >= deadline) break
            val a = states[id] ?: continue
            val atMs = now()
            val author = FollowedAuthor(a.id, a.name, null, a.xsecToken)
            // 接口错误（业务码 / 状态码）现在由解析这一层映射成 ApiException，所以解析也放在 try 里
            val page = try {
                val raw = feed.posted(author, atMs)
                // 标题里有表情记号才去取表（和以前一样只在有记号时才请求表情表），取到之后贴回条目
                val emotes = emotesFor(raw.titles)
                if (emotes.isEmpty()) {
                    raw
                } else {
                    XhsFollowParser.PostedPage(
                        raw.notes.map { note ->
                            XhsFollowParser.PostedNote(note.item.copy(emotes = Emotes.used(emotes, note.item.title)), note.sticky)
                        },
                        raw.hasMore,
                        raw.titles,
                    )
                }
            } catch (e: ApiException) {
                if (++failures >= MAX_FAILURES) throw e
                states[id] = a.copy(closedUntil = now() + CLOSED_FOR_MS)
                continue
            }
            failures = 0
            states[id] = XhsFollowMerge.integrate(a, page.notes.map { it.item }, atMs, KEEP_PER_AUTHOR)
            fetched += id
        }
        return fetched
    }

    companion object {
        const val PAGE_SIZE = 20

        /** 每次刷新最多拉几个作者的第一页。 */
        const val MAX_AUTHORS_PER_REFRESH = 10

        /** 每个作者缓存最新的几条：整个第一页（30 条普通笔记 + 最多几条置顶），再多的本来也拿不到。 */
        const val KEEP_PER_AUTHOR = 40

        const val LIST_TTL_MS = 24 * 3_600_000L
        const val EXTRA_MIN_GAP_MS = 120_000L

        /** `followings/extra` 一批问几个人。没测出上限（账号只关注了 1 个人），取保守的 10。 */
        const val EXTRA_BATCH = 10
        const val MAX_LIST_PAGES = 30
        const val MAX_FAILURES = 3
        const val CLOSED_FOR_MS = 6 * 3_600_000L

        /**
         * 一次刷新的网络预算：2 秒。最后一次已发出的请求最坏再加 1.5–2.3 秒（限速 + RTT），
         * 合计仍留出余量给 5 秒的聚合等待；详见 [refreshBudgetMs]。
         */
        const val HEAD_REFRESH_BUDGET_MS = 2_000L
    }
}
