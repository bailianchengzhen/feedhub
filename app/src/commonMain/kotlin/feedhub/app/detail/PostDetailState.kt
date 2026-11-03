package feedhub.app.detail

import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.feed.describe
import feedhub.core.FeedDetail
import feedhub.core.NativeDetailSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlin.coroutines.CoroutineContext

/**
 * 图文详情页的状态：详情（正文）和评论区各自加载、各自出错，互不影响。
 * 评论第一页由列表底部的 [PagedFooter] 出现时触发，不在这里提前请求。
 *
 * **命中 [DetailCache] 时不再"先垫占位再整块换掉"**：构造那一刻 [detail] 就是上次那条内容的
 * 正文，界面第一次画的就已经是**完整版面**（标题 / 作者 / 正文 / 图片位置都是最终的），不会闪、不会跳。
 * 命中且 [DetailCache.isFresh] 时连请求都不发；过期了先显示旧内容、后台静默刷新，回来原地替换。
 *
 * @param cacheKey 缓存的键（默认按 source.platform + id 算，和 [DetailCache.key] 一致）；测试里可以换。
 * @param cache 正文缓存（默认 [DetailCache]）；测试里可以换。
 */
class PostDetailState(
    private val source: NativeDetailSource,
    val id: String,
    private val scope: CoroutineScope,
    private val io: CoroutineContext = Dispatchers.IO,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
    private val cache: DetailCacheLike = DetailCache,
    cacheKey: String = DetailCache.key(source.platform, id),
) {
    private val key: String = cacheKey

    /** 上次打开这条内容时拿到的正文（命中缓存时构造完就已经有值）。 */
    var detail: FeedDetail? by mutableStateOf(cache.get(key))
        private set
    var loading by mutableStateOf(false)
        private set

    /** 详情加载失败时界面上那一句人话（[friendlyError]），技术细节在 [technicalError]。 */
    var error: String? by mutableStateOf(null)
        private set

    /** 上一次失败的技术细节（界面上不显示；写日志 / 调试用）。 */
    var technicalError: String? by mutableStateOf(null)
        private set

    /** 这次是"有旧内容、在后台悄悄刷新"（界面上不显示加载态，也不清空任何东西）。 */
    var refreshing by mutableStateOf(false)
        private set

    val comments = CommentsState(source, id, scope, io, platforms)

    /**
     * 加载（或重试）详情；已经在加载就不重复请求。
     *
     * 三种情况：
     * 1. 没有缓存 → 正常请求（界面照旧垫摘要 + 卡片图）；
     * 2. 有缓存且够新 → **一个请求都不发**（[reload] = true 可以强制刷新）；
     * 3. 有缓存但过期 → 先留着旧内容，[refreshing] = true 后台刷新；成功原地替换，失败**不动**旧内容
     *    （旧内容至少是对的，把用户正在看的东西换成一行错误没有意义——这一条以前没有缓存时看不出来）。
     */
    fun load(reload: Boolean = false) {
        if (loading) return
        val cached = cache.get(key)
        if (!reload && cached != null && detail != null && cache.isFresh(key)) return
        loading = true
        refreshing = detail != null
        error = null
        scope.launch {
            // 性能记录：平台 + 类型 + 从发起请求到正文回来（"首屏"的近似；真正画出来的那一帧靠帧统计看）。
            // meteredRequest 顺手做三件事：限速器排队记账（queue_ms）、总超时（超时按网络错误抛）、失败也记
            // （detail_failed + logcat）。
            try {
                val metered = feedhub.app.perf.meteredRequest(feedhub.app.perf.PerfRequest.DETAIL, source.platform, io) {
                    feedhub.app.perf.traceSection("detail.load.${source.platform.key}") { source.detail(id) }
                }
                detail = metered.value
                cache.put(key, metered.value)
                feedhub.app.perf.PerfHooks.detailLoaded(source.platform, metered.value.item.kind, metered.dMs, metered.queueMs)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // 界面上只留人话；平台名 / 参数名 / 状态码写日志
                technicalError = describe(e)
                // 已经有旧内容就不换成错误：用户正在看的还是对的，后台这次没刷新成功而已
                if (detail == null) error = friendlyError(e)
                println("[feedhub] 详情加载失败：$technicalError")
            } finally {
                loading = false
                refreshing = false
            }
        }
    }
}
