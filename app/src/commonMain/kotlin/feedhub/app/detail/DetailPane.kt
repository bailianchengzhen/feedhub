package feedhub.app.detail

import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.captcha.CaptchaOverlay
import feedhub.app.library.ExposureSurface
import feedhub.app.ui.EmoteText
import feedhub.app.ui.PlatformBackHandler
import feedhub.app.video.VideoDetailScreen
import feedhub.app.video.isVideoPost
import feedhub.brain.AdultFilter
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.VideoSource
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import feedhub.feed.exposureInputs
import feedhub.feed.VisibleItem
import feedhub.app.feed.ExposureTracker
import feedhub.app.feed.ExposureTicker

/**
 * 详情页的入口：按条目类型分给视频详情（能原生播放的平台）或图文详情。
 * 双栏 / 全屏的摆放由 App 决定，这里只管内容。打开时记一条历史。
 *
 * **直接打开一条被平台标成 18+ 的内容**（从评论里的链接、从通知点进来，信息流里已经看不到它了）：
 * 整条隐藏，只画一行 [ADULT_HIDDEN_NOTICE]，不加载正文 / 媒体、不记历史、不记曝光。
 *
 * 它还负责详情页里的"页面级"东西，里面的界面通过 [LocalDetailHost] 用：
 * - 详情栈：正文里的卡片 / 转发点开的另一条内容压在上面，返回先弹栈，栈空了才 [onClose]；
 * - 覆盖层：大图查看层（盖住整个窗口）、楼中楼面板（宽屏右侧滑出，窄屏盖满）、"已复制"之类的提示；
 * - Esc：依次关大图 → 楼中楼 → 返回上一层 / 关闭详情。
 */
@Composable
fun DetailPane(graph: AppGraph, item: FeedItem, onClose: () -> Unit, modifier: Modifier = Modifier) {
    val stack = remember(item.key) { mutableStateListOf(item) }
    val top = stack.last()
    val adult = detailHiddenAsAdult(top)
    LaunchedEffect(top.key) {
        // 记历史会写一次 SQLite；放 IO，别卡详情页打开。
        if (!adult) {
            withContext(Dispatchers.IO) { graph.library.recordVisit(top) }
            // 本地向量：点开的那条也排进"算向量"的队列（从历史 / 收藏点进来的条目不在首页那一批里）
            graph.onItemOpened(top)
        }
    }
    // 详情页停留：打开详情时按 item.key、surface = detail 记一行，
    // 停留 = 详情可见的前台时长，关闭 / 切换到另一条内容（top.key 变了）/ 窗口失焦时结算（走 [ExposureTracker]）。
    // 门槛还是 1 秒：扫一眼就退的不算一次曝光。
    // 详情页在最上面，没有"被谁盖住"这回事（covered = false）；窗口失焦照旧走结算。接线见 exposureInputs。
    // 被标成 18+ 的一条不记（整条隐藏，见 detailHiddenAsAdult）：不建 tracker，也不喂可见项。
    if (!adult) {
        val exposure = rememberUpdatedState(
            exposureInputs(
                recordBehavior = graph.settings.value.recordBehavior,
                covered = false,
                windowFocused = LocalWindowInfo.current.isWindowFocused,
            ),
        )
        val detailTracker = remember(top.key) {
            ExposureTracker(graph.library, ExposureSurface.DETAIL, enabled = { exposure.value.enabled })
        }
        ExposureTicker(detailTracker, foreground = exposure.value.foreground) { listOf(VisibleItem(top, 1f)) }
    }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current
    val closeAll by rememberUpdatedState(onClose)
    val back: () -> Unit = { if (stack.size > 1) stack.removeAt(stack.lastIndex) else onClose() }

    // 外层容器与 VideoDetailScreen 不套在 key(top.key) 里：
    // 竖屏沉浸连划时手指可能在上一条收尾动画未结束前就再次按下并提交切换，若整棵子树按 top.key 卸载重建，
    // 正在进行的手势协程（pointerInput）会被当场取消导致连划断触。图文详情 / 覆盖层照旧按 top.key 独立重置。
    val host = remember(stack, top.key) {
        DetailHost(
            graph, top.platform, scope,
            push = { if (it.key != stack.last().key) stack.add(it) },
            closeAllDetail = { closeAll() },
            copyToClipboard = { clipboard.setText(AnnotatedString(it)) },
        )
    }
    // 详情页的「原文 / 译文」总开关：整页一个，评论 / 楼中楼共用；换一条内容就重置
    // 卡片上已经下好的那张缩略图：详情里的图先用它垫着，原图到了再淡入替换。
    // 只在打开这一条内容时取一次（卡片画过才记，取不到就是空的 = 照旧加载）。
    val thumbnails = remember(top.key) { DetailThumbnails.of(top.key) }
    SideEffect { DetailDebug.host = host }
    // 时间线：每打开一条内容就把"图到没到齐"的计数归零，
    // 这样第二次点进同一条内容也能单独看出它是不是又从 0 开始。正常界面这两下加减没有别的作用。
    LaunchedEffect(top.key) { DetailDebug.resetImageCounters() }
    // 安卓的系统返回键 / 返回手势：和 Esc 一样，先关大图 → 关楼中楼 → 返回上一层 → 关详情（桌面是空实现）
    PlatformBackHandler { if (!host.dismissTop()) back() }
    val focus = remember { FocusRequester() }
    LaunchedEffect(top.key) { focus.requestFocus() }
    SideEffect { host.refocus = { runCatching { focus.requestFocus() } } }
    BoxWithConstraints(
        modifier
            .focusRequester(focus)
            .focusable()
            // 点了详情里的任何地方都把键盘焦点拉回来，Esc 才一直有效
            .pointerInput(Unit) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                    focus.requestFocus()
                }
            }
            .onKeyEvent { e ->
                if (e.type == KeyEventType.KeyDown && e.key == Key.Escape) {
                    if (!host.dismissTop()) back()
                    true
                } else {
                    false
                }
            },
    ) {
        val wide = maxWidth >= 600.dp
        CompositionLocalProvider(
            LocalDetailHost provides host,
            LocalDetailThumbnails provides thumbnails,
            // 评论 / 楼中楼没有平台字段，自动翻译按平台判定时靠它（见 TranslationStore.shouldAutoTranslate）
            LocalDetailPlatform provides top.platform,
        ) {
            if (adult) {
                // 整条隐藏：正文、评论、媒体一个都不请求，只给这一行 + 返回
                key(top.key) {
                    AdultHiddenDetail(back, Modifier.fillMaxSize())
                }
            } else {
                when (val target = top.detail) {
                    is DetailTarget.Native ->
                        if (top.isVideoPost() && graph.sourceOrNull(top.platform) is VideoSource) {
                            VideoDetailScreen(graph, top, target.id, back, Modifier.fillMaxSize())
                        } else {
                            key(top.key) {
                                PostDetailScreen(graph, top, target.id, back, Modifier.fillMaxSize())
                            }
                        }
                    is DetailTarget.Web -> key(top.key) {
                        UnsupportedDetail(top, target.url, back, Modifier.fillMaxSize())
                    }
                }
                key(top.key) {
                    host.replies?.let { RepliesPanel(it, host, wide, onClose = host::closeReplies, modifier = Modifier.fillMaxSize()) }
                    host.viewer?.let { ImageViewer(it, onClose = host::closeViewer) }
                }
            }
        }
        host.toast?.let { message ->
            Surface(
                shape = RoundedCornerShape(50),
                color = MaterialTheme.colorScheme.inverseSurface,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 32.dp),
            ) {
                Text(message, color = MaterialTheme.colorScheme.inverseOnSurface, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(horizontal = 18.dp, vertical = 10.dp))
            }
        }
        // 酷安评论的人机验证：只盖住这个详情区（双栏=右栏，全屏=整屏），不弹全局对话框
        CaptchaOverlay(graph)
    }
}

/** 直播、番剧这类没有原生详情的：给出链接，可以复制。 */
@Composable
private fun UnsupportedDetail(item: FeedItem, url: String, onClose: () -> Unit, modifier: Modifier) {
    val clipboard = LocalClipboardManager.current
    Column(
        modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        // 标题 / 摘要也带表情：和详情其他位置同一套，别在这里漏一处
        val heading = item.title ?: item.summary.orEmpty()
        EmoteText(
            heading,
            emotes = remember(heading, item.emotes) { feedhub.core.Emotes.used(item.emotes, heading) },
            style = MaterialTheme.typography.titleMedium,
        )
        Text("这类内容（${item.kind}）还不能在应用里打开", color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(url, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        TextButton(onClick = { clipboard.setText(AnnotatedString(url)) }) { Text("复制链接") }
        TextButton(onClick = onClose) { Text("返回") }
    }
}

/**
 * 详情页打开一条内容时要不要真的加载：**平台标成 18+ 的整条隐藏**，
 * 只画 [ADULT_HIDDEN_NOTICE] 一行，不取正文 / 评论 / 播放地址，也不记历史 / 曝光。
 *
 * 判断只有 [feedhub.brain.AdultFilter] 一处；这里包一层是为了让"详情页照不加载"这条规则能被单测直接盯住。
 */
internal fun detailHiddenAsAdult(item: FeedItem): Boolean = AdultFilter.hidden(item)

/** 直接打开一条被平台标成 18+ 的内容时显示的那一行。 */
internal const val ADULT_HIDDEN_NOTICE: String = AdultFilter.NOTICE

/** 被标成 18+ 的内容直接打开时的界面：一行说明 + 返回，标题 / 封面 / 正文都不显示。 */
@Composable
internal fun AdultHiddenDetail(onClose: () -> Unit, modifier: Modifier) {
    Column(
        modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(ADULT_HIDDEN_NOTICE, style = MaterialTheme.typography.titleMedium)
        TextButton(onClick = onClose) { Text("返回") }
    }
}
