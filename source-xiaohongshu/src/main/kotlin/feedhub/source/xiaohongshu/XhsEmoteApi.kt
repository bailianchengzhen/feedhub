package feedhub.source.xiaohongshu

import feedhub.core.KeyValueStore
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.SourceException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import java.io.IOException

/**
 * **表情表的接缝**：详情 / 评论 / 信息流要的那份表从哪儿来、要不要去取、失败了怎么退化。
 *
 * 两个实现：
 * - [KotlinXhsEmoteApi]：搬进 Rust 之前 `XhsSource` 里那一整套（内存缓存 → `ctx.store` 一周 → 取
 *   `GET /api/im/redmoji/detail` → 失败 10 分钟不再试），行为和搬之前**一模一样**，是对拍的参照；
 * - `HostedXhsEmoteApi`（`XhsHosted.kt`）：**抓取 / 解析 / 缓存都在 Rust 平台对象里**，
 *   详情 / 评论用的那份表 Rust 直接拿来用，**不再每次把 166 条过 FFI 边界**。
 *
 * 两条时序不变：
 * - **详情 / 评论不等表**：手上没有就按原文 `[笑哭R]` 显示，Rust 顺手把"要不要补取"算好带回来
 *   （`has_emotes`），Kotlin 再调 [warm] 在后台补；
 * - **信息流 / 搜索 / 关注流**（[used]）：刷新本来就在后台跑，多等一个限速槽没关系，直接同步取。
 *
 * 同一时间只许有一个在飞的取表请求（风控名额很贵）：这条在 Rust 实现里由平台对象管，
 * 参照实现里是那个 `AtomicBoolean`。
 */
internal interface XhsEmoteApi {
    /**
     * 信息流 / 搜索 / 关注流用：文字里有 `[笑哭R]` 这样的记号才去取表（可能发一次请求），
     * 返回文字里**真出现过**的那几项（`Emotes.used` 的口径；调用方还会再 `Emotes.used` 一次，幂等）。
     */
    suspend fun used(text: String): Map<String, String>

    /**
     * 手上已经有的表（内存 → `ctx.store` 里一周内那份），**不发请求**。
     *
     * 只有 Kotlin 参照实现的详情 / 评论用它；Rust 那条路的表住在平台对象里。
     */
    fun cached(): Map<String, String>

    /** 后台补取（不挡正文、失败就算了）。 */
    fun warm()

    /** 测试用：等后台那次补取跑完（生产代码不调）。 */
    suspend fun awaitWarm()
}

/**
 * Kotlin 参照实现：就是搬进 Rust 之前 `XhsSource` 里那块表情表代码（一行语义都没改，只是挪过来）。
 *
 * - 文字里有记号才去取（[used]）；
 * - 表缓存在 `ctx.store` 里一周（[XhsEmoteTable.TTL_SEC]），内存里再存一份；
 * - 风控异常照常往外抛（不吞），其他错误这次用旧表、10 分钟内不再试；
 * - 同一时间只许一个在飞（[fetching]）：首页和关注本来就可能同时刷新，两边都发现"表不在手上"
 *   就会各发一个，白占一个风控名额，后来的人直接用手上有的。
 */
internal class KotlinXhsEmoteApi(
    private val api: XhsApi,
    private val store: KeyValueStore,
    private val platform: Platform,
) : XhsEmoteApi {
    @Volatile
    private var cache: XhsEmoteTable? = null

    @Volatile
    private var retryAtMs = 0L

    /** 后台取表情表的任务（详情 / 评论不等它，见 [used]）。一次只起一个；表情表自己缓存一周。 */
    @Volatile
    private var warmJob: Job? = null

    /** 只跑"补取表情表"这种不等结果的小活（[Dispatchers.IO]，不是详情 / 评论的取消链）。 */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** 有没有人在取表情表（进程内一次只许一个在飞）。 */
    private val fetching = java.util.concurrent.atomic.AtomicBoolean(false)

    override suspend fun used(text: String): Map<String, String> =
        if (XhsBody.EMOTE_TOKEN.containsMatchIn(text)) table() else emptyMap()

    override fun cached(): Map<String, String> = cachedTable()?.map.orEmpty()

    /**
     * 文字里有 `[笑哭R]` 这样的记号才去取表情表；表缓存在 `ctx.store` 里一周（和小黑盒一样），
     * 取不到就降级成文字。风控异常照常往外抛（不吞），其他错误这次用旧表，10 分钟内不再试。
     */
    private suspend fun table(): Map<String, String> {
        cache?.let { return it.map }
        val nowMs = System.currentTimeMillis()
        val nowSec = nowMs / 1000
        val stored = store.get(XhsEmoteTable.KEY)?.let(XhsEmoteTable::fromStored)
        if (stored != null && nowSec - stored.fetchedAtSec < XhsEmoteTable.TTL_SEC) return stored.also { cache = it }.map
        if (nowMs < retryAtMs) return stored?.map.orEmpty()
        // 已经有人在取了（首页 + 关注同时刷新、或者详情刚触发的后台补取）：不重复发请求，用手上有的
        if (!fetching.compareAndSet(false, true)) return stored?.map.orEmpty()
        try {
            val table = try {
                XhsEmoteTable.fromDetail(api.get(REDMOJI_DETAIL), nowSec).also {
                    if (!it.isEmpty()) store.put(XhsEmoteTable.KEY, it.toStored())
                }
            } catch (e: RiskControlException) {
                throw e
            } catch (_: SourceException) {
                retryAtMs = nowMs + 10 * 60_000L
                return stored?.map.orEmpty()
            } catch (_: IOException) {
                retryAtMs = nowMs + 10 * 60_000L
                return stored?.map.orEmpty()
            }
            cache = table
            return table.map
        } finally {
            fetching.set(false)
        }
    }

    /** 手上已经有的表（内存 → `ctx.store` 里一周内的那份）；过期 / 没有就是 null。**不发请求。** */
    private fun cachedTable(): XhsEmoteTable? {
        cache?.let { return it }
        val nowSec = System.currentTimeMillis() / 1000
        val stored = store.get(XhsEmoteTable.KEY)?.let(XhsEmoteTable::fromStored) ?: return null
        if (nowSec - stored.fetchedAtSec >= XhsEmoteTable.TTL_SEC) return null
        cache = stored
        return stored
    }

    /**
     * 后台把表情表补上（同一时间只起一个）：**不挡任何人的正文**，失败就算了（[retryAtMs] 十分钟内不再试）；
     * 风控异常同样记下重试时间、不重试。
     */
    override fun warm() {
        if (warmJob?.isActive == true) return
        if (System.currentTimeMillis() < retryAtMs) return
        warmJob = scope.launch {
            try {
                table()
            } catch (_: RiskControlException) {
                retryAtMs = System.currentTimeMillis() + 10 * 60_000L
            } catch (_: Exception) {
                // table() 里已经把 retryAt 往后推了；这里只是别让异常跑到协程外面
            }
        }
    }

    override suspend fun awaitWarm() {
        warmJob?.join()
    }
}
