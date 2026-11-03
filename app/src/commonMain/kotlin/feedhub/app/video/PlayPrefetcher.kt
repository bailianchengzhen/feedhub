package feedhub.app.video

import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.VideoSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.coroutines.CoroutineContext

/**
 * 沉浸播放（上下滑连续刷）的预取：**只预取下一条**的详情和播放地址。
 *
 * 为什么连详情一起取：播放地址要的是 `(内容 id, 分 P id)`，分 P id 在详情里（B站是 cid），光有一条信息流条目拿不到。
 * 所以预取 = 这一条的详情 + 它的第一 P 的播放地址，两项都留着；切过去时 `VideoDetailScreen` 用 [detail] 省掉一次
 * 详情请求，[PlaybackSession] 用 [playInfo] 省掉一次取流。
 *
 * **只保留一条**：换目标（[prefetch] 给另一条）或 [cancel] 就把上一份连同在飞的请求一起丢掉。预取失败不报错
 * （切过去时照旧现取，那边的错误提示管）。
 *
 * **只在前台**：[canPrefetch] 为 false（App 退到后台）时一个请求都不发，在飞的也当场取消——后台什么都不留
 * 。界面把前后台变化放进 `LaunchedEffect` 的 key，回前台会重新叫一次。
 */
class PlayPrefetcher(
    private val sourceOf: (Platform) -> VideoSource?,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
    /** 现在能不能预取（App 在前台）。false 时 [prefetch] 直接返回，并丢掉手上的预取。 */
    private val canPrefetch: () -> Boolean = { true },
) {
    /** 预取好的下一条：[partId] 是第一 P 的 id（[PlaybackSession] 取流时对得上才用）。 */
    class Ready(val key: String, val partId: String, val detail: FeedDetail, val info: PlayInfo)

    private var job: Job? = null
    private var ready: Ready? = null

    /** 正在预取 / 已经预取好的那条内容的 key（null = 没有）。 */
    var targetKey: String? = null
        private set

    /** 现在手上有没有取好的（测试 / 截图工具看状态用）。 */
    val isReady: Boolean get() = ready != null

    /** 内容 id：详情接口用的那个（不一定等于 [FeedItem.id]）。 */
    private fun detailId(item: FeedItem) = (item.detail as? DetailTarget.Native)?.id ?: item.id

    /**
     * 预取这一条（已经在预取它、或者已经取好了就什么都不做）。
     * [item] 为 null（到尾部 / 没有下一条）等于 [cancel]：不留上一条的预取。
     */
    fun prefetch(item: FeedItem?) {
        if (item == null) {
            cancel()
            return
        }
        if (!canPrefetch()) {
            cancel()
            return
        }
        if (item.key == targetKey && (ready != null || job?.isActive == true)) return
        cancel()
        val source = sourceOf(item.platform) ?: return
        targetKey = item.key
        val id = detailId(item)
        job = scope.launch {
            try {
                // 预取的是"下一条"、用户还没滑过去：按**后台优先级**排队（[feedhub.core.backgroundRequestScope]），
                // 别把用户眼前这条的详情 / 评论 / 播放挤到后面（小红书这种 1.5～2.3 秒一个槽的平台差很多）
                ready = withContext(io) {
                    feedhub.core.backgroundRequestScope {
                        val detail = source.detail(id)
                        val partId = detail.video?.parts?.firstOrNull()?.id ?: id
                        Ready(item.key, partId, detail, source.play(id, partId))
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // 预取失败算了：切过去的时候照旧现取（可能仍然失败，那边的错误提示管）
            }
        }
    }

    /** 预取到的详情（[item] 对得上才有）。 */
    fun detail(item: FeedItem): FeedDetail? = ready?.takeIf { it.key == item.key }?.detail

    /** 预取到的那一份（[item] 对得上才有）。播放地址要按分 P id 自己挑（[PlaybackSession] 的 `prefetchedPlayInfo`）。 */
    fun readyFor(item: FeedItem): Ready? = ready?.takeIf { it.key == item.key }

    /** 丢掉预取（切走 / 换页签 / 关详情 / 退到后台）。 */
    fun cancel() {
        job?.cancel()
        job = null
        ready = null
        targetKey = null
    }
}
