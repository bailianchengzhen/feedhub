package feedhub.app.detail

import androidx.compose.runtime.Stable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.AppGraph
import feedhub.app.feed.describe
import feedhub.app.video.InlineVideoState
import feedhub.core.Comment
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.NativeDetailSource
import feedhub.core.Platform
import feedhub.core.Source
import feedhub.core.UserRef
import feedhub.core.VideoSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** 查看大图时的一张图：[url] 是要看的（原图优先），[thumb] 是正文里已经加载过的那张（原图出来前先垫着）。 */
data class ViewerImage(val url: String, val thumb: String? = null, val caption: String? = null)

/** 评论区里"楼主 / UP 主"标签用：正文作者的 id 和标签文字。 */
data class Poster(val id: String?, val label: String = "楼主")

/** 打开了哪条评论的楼中楼。 */
data class RepliesRequest(val contentId: String, val comment: Comment, val poster: Poster)

/**
 * 详情页里各部件共用的"页面级"动作和覆盖层状态：大图查看层、楼中楼面板、"已复制"提示、在详情里打开另一条内容。
 *
 * 由 [DetailPane] 给每条打开的内容建一个，通过 [LocalDetailHost] 提供给里面的所有界面（图文详情、视频详情、
 * [RichTextView]、评论）。覆盖层由 [DetailPane] 画，所以视频详情的简介 / 评论也能直接用——只要调这里的方法。
 * 不在 [DetailPane] 里时（[LocalDetailHost] 为 null）部件照样能画，只是点击没有反应。
 *
 * @param platform 这条内容所属的平台：正文里卡片、链接的站内 target 都当成同一个平台解析。
 */
@Stable
class DetailHost(
    val graph: AppGraph,
    val platform: Platform,
    private val scope: CoroutineScope,
    private val push: (FeedItem) -> Unit,
    private val closeAllDetail: () -> Unit,
    private val copyToClipboard: (String) -> Unit,
    private val io: kotlin.coroutines.CoroutineContext = Dispatchers.IO,
    /** 按平台取 Source（插件没装 / 这一端用不了是 null）；测试里换成假的。 */
    private val sourceOf: (Platform) -> Source? = graph::sourceOrNull,
) {
    /** 正在看的大图（null = 没开）。 */
    var viewer: ImageViewerState? by mutableStateOf(null)
        private set

    /** 打开着的楼中楼面板（null = 没开）。 */
    var replies: RepliesRequest? by mutableStateOf(null)
        private set

    /** 底部的短提示（"已复制"之类），几秒后自己消失。 */
    var toast: String? by mutableStateOf(null)
        private set

    /** 设置里的"默认显示原图"。 */
    val originalImages: Boolean get() = graph.settings.value.originalImages

    /** 正文里夹着的视频（[feedhub.core.Block.Video]）嵌入播放的协调：同一时间只有一个持有播放器。 */
    val videos = InlineVideoState()

    /**
     * 视频详情页顶上那个主播放器**放大 / 全屏**着没有：由 [feedhub.app.video.VideoDetailScreen] 写，
     * 系统的返回键 / Esc 先在 [dismissTop] 里退出放大，而不是把整个详情关掉（官方播放器都是这个层级：
     * 返回一次退全屏、再返回才退页）。放大本身仍然是 App 内的一层（桌面的 Esc 也走这里）。
     */
    var playerZoomed by mutableStateOf(false)

    /**
     * 沉浸界面（竖屏视频）的评论面板开着没有（同一个层级问题）：由视频详情页写，返回键先关面板（和 Esc 一致）。
     */
    var playerCommentsOpen by mutableStateOf(false)

    /** 这条内容所在平台的视频源（行内视频点了之后取播放地址用）；这个平台还不能播视频就是 null。 */
    fun videoSource(): VideoSource? = sourceOf(platform) as? VideoSource

    /**
     * 把键盘焦点拉回详情页（[DetailPane] 设置）。行内视频放大 / 还原时，点过的那个按钮跟着旧的播放器一起离开了组合，焦点就丢了，
     * Esc 收不到；放大后调一下，Esc 才能退出放大。
     */
    var refocus: () -> Unit = {}

    private var toastJob: Job? = null

    fun showToast(message: String) {
        toast = message
        toastJob?.cancel()
        toastJob = scope.launch {
            delay(2000)
            toast = null
        }
    }

    fun copy(text: String, message: String = "已复制") {
        copyToClipboard(text)
        showToast(message)
    }

    /** 打开大图查看层，从第 [index] 张开始，可以左右切换。 */
    fun openImages(images: List<ViewerImage>, index: Int) {
        if (images.isEmpty()) return
        viewer = ImageViewerState(images, index.coerceIn(images.indices))
    }

    fun closeViewer() {
        viewer = null
    }

    /** 打开某条评论的楼中楼：宽屏是右侧滑出的面板，窄屏是盖满详情的一层。 */
    fun openReplies(comment: Comment, poster: Poster, contentId: String) {
        replies = RepliesRequest(contentId, comment, poster)
    }

    fun closeReplies() {
        replies = null
    }

    /** 在详情里打开另一条内容（已经有 [FeedItem] 的，例如转发的原动态）；不是原生详情就复制它的链接。 */
    fun openItem(item: FeedItem) {
        when (val target = item.detail) {
            is DetailTarget.Native -> push(item)
            is DetailTarget.Web -> openExternal(target.url)
        }
    }

    /**
     * 点作者：**在官方 App 里打开他的主页**（插件描述 `appLinks.user`），
     * 没装 App 就退化成网页，网页也打不开就复制主页链接。**不做站内作者页**。
     */
    fun openAuthor(author: UserRef) {
        val open = externalOpenOfAuthor(author, graph.platforms)
        if (open == null) {
            showToast("这个平台没有作者主页")
            return
        }
        val result = performExternalOpen(graph.systemActions, open) { text -> copy(text, "已复制主页链接") }
        if (result == ExternalOpenResult.FAILED) showToast("打不开作者主页")
    }

    /**
     * 打开正文里卡片 / 链接的目标：站内（[DetailTarget.Native]）就取一下详情在详情页里打开；
     * 站外的走 [openExternal]（安卓 = 系统浏览器 / Custom Tabs，桌面 = 复制链接）。
     */
    fun openTarget(target: DetailTarget?, url: String?) {
        when {
            target is DetailTarget.Native -> scope.launch {
                try {
                    // 目标可以指向另一个平台（酷安动态里的 B站视频链接 -> B站播放器）
                    val targetPlatform = target.platform ?: platform
                    val source = sourceOf(targetPlatform) as? NativeDetailSource ?: run {
                        // 指向的平台没装插件（拆掉了）：说一声，不打开
                        if (sourceOf(targetPlatform) == null) showToast("${targetPlatform.label}的插件没装，打不开")
                        return@launch
                    }
                    // 这里只是"取一下条目再推详情页"，成功不另记一行（详情页自己会记）；但要总超时 / 失败记一行，
                    // 免得点了正文里的卡片之后一直没有反应
                    val item = feedhub.app.perf.meteredRequest(
                        feedhub.app.perf.PerfRequest.DETAIL, targetPlatform, io,
                    ) { source.detail(target.id) }.value.item
                    push(item.copy(detail = target))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    showToast(describe(e))
                }
            }
            target is DetailTarget.Web -> openExternal(target.url)
            url != null -> openExternal(url)
        }
    }

    /**
     * 外链：安卓用系统浏览器（Custom Tabs，保留返回栈）打开，打不开才复制；
     * [SystemActions.None] 就让它自动走这一档。
     */
    private fun openExternal(url: String) {
        if (graph.systemActions.openWeb(url)) return
        copy(url, "已复制链接")
    }

    /** 链接被点：站内能原生打开的打开，站外的交给系统浏览器 / 复制。 */
    fun openLink(url: String, target: DetailTarget?) = openTarget(target, url)

    /** 关掉整个详情（屏蔽作者之后用）。 */
    fun closeAll() = closeAllDetail()

    /**
     * 按 Esc / 返回键依次关：主播放器放大 → 沉浸界面的评论面板 → 放大的行内视频 → 大图 → 楼中楼 → 返回上一层；
     * 返回 true 表示这次按键用掉了。顺序和 [feedhub.app.video.VideoDetailScreen] 里 Esc 的顺序一致。
     */
    internal fun dismissTop(): Boolean = when {
        playerZoomed -> { playerZoomed = false; true }
        playerCommentsOpen -> { playerCommentsOpen = false; true }
        videos.zoomed -> { videos.zoomed = false; true }
        viewer != null -> { viewer = null; true }
        replies != null -> { replies = null; true }
        else -> false
    }
}

/** 当前详情页的 [DetailHost]；不在 [DetailPane] 里就是 null。 */
val LocalDetailHost = compositionLocalOf<DetailHost?> { null }

/**
 * 当前详情页的平台。评论 / 楼中楼没有自己的平台字段，自动翻译按平台判定时要靠它
 * （见 [feedhub.app.translate.TranslationStore.shouldAutoTranslate]）；不在详情里就是 null（不按平台限制）。
 */
val LocalDetailPlatform = compositionLocalOf<Platform?> { null }
