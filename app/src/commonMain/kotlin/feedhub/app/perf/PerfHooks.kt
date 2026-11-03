package feedhub.app.perf

import androidx.compose.runtime.snapshotFlow
import feedhub.app.video.PlaybackSession
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.Platform
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import java.util.Collections
import java.util.WeakHashMap

/**
 * 各页面往性能日志里 "打点" 的唯一入口。
 *
 * 为什么用全局对象而不是把 [PerfRecorder] 一路传进去：调这些点的地方（`FeedState` 的每家请求、`PostDetailState`、
 * `PagedState`、`PlaybackSession`）手里没有 graph，任务又要求"需要钩子的地方只加一行调用"（不动那些模块的逻辑）。
 * 一个进程只有一份 App，和 [feedhub.app.video.ActivePlaybackSessions]、`AppGraph.current` 是同一个做法。
 *
 * **隐私口径**（§5）：这里所有函数只收平台 id、内容类型、页面名、计数、耗时——**收不到**标题 / 正文 / URL /
 * 用户名 / Cookie，调用方想传也没有参数可传（`PerfPrivacyTest` 守着 JSON 的键名清单）。
 */
object PerfHooks {
    @Volatile
    private var recorder: PerfRecorder? = null

    /** 性能记录开关开着吗（关着的时候这些点**一个多余协程 / 唤醒都不留**，见 [watchVideo]）。 */
    val active: Boolean get() = recorder?.enabled == true

    fun attach(rec: PerfRecorder?) {
        recorder = rec
        rec?.setScreen(PerfScreen.current)
    }

    fun detach() {
        recorder = null
    }

    /** 页面变了（`App` 的导航状态算好后调）。 */
    fun screen(name: String) {
        PerfScreen.setBase(name)
        recorder?.setScreen(PerfScreen.current)
    }

    /**
     * 盖在页面上的弹层（图片查看器 / 沉浸）进来、出去；null = 出去。记录器那份页面名也得跟着换——
     * 之前只改了 [PerfScreen]，采样行里从来没出现过"沉浸"，竖屏刷视频的那几段全记成了"详情-视频"。
     */
    fun overlay(name: String?) {
        PerfScreen.setOverlay(name)
        recorder?.setScreen(PerfScreen.current)
    }

    /** 当前时间（毫秒）：调用方在开始处拍一次，结束时算差。 */
    fun nowMs(): Long = System.currentTimeMillis()

    // ---- 信息流刷新（首页 / 关注各一行开始、一行结束） ----

    fun feedRefreshStart(kind: FeedKind, platforms: List<Platform>): Long {
        val at = nowMs()
        recorder?.eventFeedRefresh(kind, start = true, platforms = platforms)
        return at
    }

    fun feedRefreshEnd(kind: FeedKind, platforms: List<Platform>, count: Int, startedAt: Long) {
        recorder?.eventFeedRefresh(kind, start = false, platforms = platforms, count = count, dMs = (nowMs() - startedAt).coerceAtLeast(0))
    }

    // ---- 详情 / 评论 ----

    fun detailLoaded(platform: Platform, kind: ItemKind, dMs: Long, queueMs: Long? = null) {
        recorder?.eventDetailLoaded(platform, kind, dMs.coerceAtLeast(0), queueMs)
    }

    fun commentsLoaded(platform: Platform, count: Int, dMs: Long, queueMs: Long? = null) {
        recorder?.eventCommentsLoaded(platform, count, dMs.coerceAtLeast(0), queueMs)
    }

    /**
     * 详情 / 评论 / 楼中楼 / 播放**失败**了：写一条 `<kind>_failed` 事件，**并且无条件**在 logcat 里打一行
     * （tag `feedhub-perf`，安卓实现见 `androidMain/.../perf/PerfLog.android.kt`）。
     *
     * 为什么日志不受性能记录开关管：用户报"完全不动"的时候记录器多半是关着的，而这一行是事后唯一能看的东西。
     * 一行里只有平台 id、错误类型、毫秒数——**没有 URL、没有 token / Cookie / 签名**（[errorReason] 只返回分类名）。
     */
    fun requestFailed(kind: PerfRequest, platform: Platform, error: Throwable, dMs: Long, queueMs: Long? = null) {
        val ms = dMs.coerceAtLeast(0)
        val reason = errorReason(error)
        recorder?.eventRequestFailed(kind.key, platform, reason, ms, queueMs)
        perfLogLine(feedhub.perf.RustPerf.requestFailedLogLine(kind.key, platform.key, reason, ms, queueMs))
    }

    /**
     * 错误 → 性能记录 / 日志里的一个分类名（**不是给用户看的文案**，[feedhub.app.feed.describe] 那套在界面上）。
     *
     * 顺序有讲究：`CancellationException` 要在 `IOException` 前面（取消是超时 / 返回上一层最常见的原因）；
     * `ProxyUnavailableException` 是 `IOException` 的子类，也要在它前面。
     */
    fun errorReason(error: Throwable): String = when (error) {
        is kotlinx.coroutines.CancellationException -> "Cancelled"
        is java.net.SocketTimeoutException -> "Timeout"
        is feedhub.core.RiskControlException -> "RiskControl"
        is feedhub.core.NotLoggedInException -> "NotLoggedIn"
        is feedhub.core.SignatureException -> "Signature"
        is feedhub.core.ApiException -> "Api"
        is feedhub.core.ParseException -> "Parse"
        is feedhub.core.ProxyUnavailableException -> "ProxyUnavailable"
        is java.io.IOException -> "Network"
        else -> "Other"
    }

    // ---- 播放 ----

    private val watched: MutableMap<PlaybackSession, Job> = Collections.synchronizedMap(WeakHashMap())

    /**
     * 盯住一个播放会话：打开 → 首帧（耗时 + 异步 trace 切片）、播放 / 暂停、播完。
     *
     * 只在开关开着时挂（关着不挂，连一个协程都不多）；[stopWatchingVideo] 在 `PlaybackSession.release()` 里摘掉。
     * 用 `snapshotFlow` 而不是轮询：Compose 状态一变才醒，静止时不产生任何唤醒。
     */
    fun watchVideo(session: PlaybackSession, platform: Platform, scope: CoroutineScope) {
        val rec = recorder ?: return
        if (!rec.enabled) return
        synchronized(watched) {
            if (watched.containsKey(session)) return
            watched[session] = scope.launch { observeVideo(session, platform, rec) }
        }
    }

    fun stopWatchingVideo(session: PlaybackSession) {
        synchronized(watched) { watched.remove(session)?.cancel() }
    }

    /**
     * 播放会话的状态信号。`hw` / `qn` / `h` 来自会话选中的那一路流（只读）。
     * 单独抽成 data class：`snapshotFlow` 只在真的变了才发（`equals` 去重）。
     */
    private data class VideoSignal(
        val playing: Boolean,
        val hasFrame: Boolean,
        val ended: Boolean,
        val hardware: Boolean?,
        val qn: Int?,
        val height: Int?,
    )

    private suspend fun observeVideo(session: PlaybackSession, platform: Platform, rec: PerfRecorder) {
        val openedAt = nowMs()
        val trace = traceBeginAsync("video.open.${platform.key}")
        var firstFrame = false
        var lastPlaying = false
        var lastEnded = false
        snapshotFlow {
            VideoSignal(
                playing = session.state.playing,
                hasFrame = session.state.hasFrame,
                ended = session.state.ended,
                hardware = session.state.hardwareDecoding,
                qn = session.choice?.quality,
                height = session.choice?.video?.height,
            )
        }.distinctUntilChanged().collect { s ->
            if (s.hasFrame && !firstFrame) {
                firstFrame = true
                traceEndAsync("video.open.${platform.key}", trace)
                rec.eventVideo(platform, "first_frame", s.playing, s.hardware, s.qn, s.height, nowMs() - openedAt)
            }
            if (s.ended && !lastEnded) {
                lastEnded = true
                rec.eventVideo(platform, "end", false, s.hardware, s.qn, s.height)
            }
            if (s.playing != lastPlaying) {
                lastPlaying = s.playing
                rec.eventVideo(platform, if (s.playing) "play" else "pause", s.playing, s.hardware, s.qn, s.height)
            }
        }
    }
}
