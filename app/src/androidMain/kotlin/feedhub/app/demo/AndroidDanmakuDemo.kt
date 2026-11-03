package feedhub.app.demo

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Shader
import android.os.Handler
import android.os.Looper
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import feedhub.app.AppGraph
import feedhub.app.ui.FeedhubTheme
import feedhub.app.video.MediaSource
import feedhub.app.video.PlaybackSession
import feedhub.app.video.PlayerEngine
import feedhub.app.video.PlayerState
import feedhub.app.video.PlayerView
import feedhub.app.video.SAMPLE_LOOP_MS
import feedhub.app.video.sampleDanmaku
import feedhub.core.AudioStream
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.LoginSpec
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.Quality
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.UserRef
import feedhub.core.VideoCodec
import feedhub.core.VideoPart
import feedhub.core.VideoSource
import feedhub.core.VideoStream
import kotlinx.coroutines.Dispatchers

/**
 * **只给 debug 启动参数用**的离线弹幕演示页（`--es screen danmaku`，见 `DebugStartActivity`）：
 * 一块自己画的"视频"（背景 `SurfaceView`，静态风景色块，不联网、不解码）+ 真正的 [PlayerView] +
 * [sampleDanmaku] 的假弹幕，用来在模拟器上验"弹幕在自己那层上、控件在弹幕上面"和 `dumpsys gfxinfo` 的帧数对比。
 *
 * 为什么不用真的视频：演示环境（[androidDemoGraph]）一个请求都不发，本地也没有视频文件；而弹幕那层是
 * **media overlay**（在视频 SurfaceView 之上、窗口之下），底下必须真有一块视频 SurfaceView 垫着，
 * 否则透明的地方会透到桌面。所以这里自己画一块"假视频"，它只在 surface 建好 / 尺寸变化时画一次（静态），
 * 这样 `gfxinfo` 里多出来的帧就只可能是弹幕引起的。
 */
@Composable
fun AndroidDanmakuDemo(graph: AppGraph) {
    val scope = rememberCoroutineScope()
    val engine = remember { DanmakuDemoEngine() }
    val session = remember(engine) {
        PlaybackSession(
            engine = engine,
            source = DanmakuDemoSource,
            item = DEMO_ITEM,
            id = "BVdemo",
            parts = listOf(VideoPart("1", "演示", durationSec = (SAMPLE_LOOP_MS / 1000).toInt())),
            danmakuSegmentMs = SAMPLE_LOOP_MS,
            library = graph.library,
            settings = { graph.settings.value },
            scope = scope,
            io = Dispatchers.Default,
        )
    }
    DisposableEffect(session) {
        session.loadPart(0)
        onDispose { session.release() }
    }
    FeedhubTheme(graph.settings.value.darkMode) {
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            PlayerView(
                session = session,
                danmaku = graph.settings.value.danmaku,
                onDanmakuChange = { v -> graph.settings.update { it.copy(danmaku = v) } },
                zoomed = false,
                onToggleZoom = {},
                modifier = Modifier.fillMaxSize(),
            )
        }
    }
}

private val BILI = Platform("bili")

private val DEMO_ITEM = FeedItem(BILI, "BVdemo", ItemKind.VIDEO, title = "弹幕演示", detail = DetailTarget.Native("BVdemo"))

/**
 * 假播放器：不解码，只记状态；位置每 250 ms 报一次（和 Media3 引擎一个节奏），到 24 秒回到开头接着循环。
 * 画面是 [DemoBackdrop] 那块 SurfaceView 画的一幅静态风景色块——弹幕要压在**真的 SurfaceView** 上才看得出来层级。
 */
private class DanmakuDemoEngine : PlayerEngine {
    override val state = PlayerState()
    override val unavailable: String? = null
    private val main = Handler(Looper.getMainLooper())

    private val ticker = object : Runnable {
        override fun run() {
            if (!state.playing || state.ended) return
            val pos = state.nowMs()
            if (pos >= SAMPLE_LOOP_MS - 100) state.jump(0) else state.report(pos)
            main.postDelayed(this, 250)
        }
    }

    private fun sync() {
        main.removeCallbacks(ticker)
        if (state.playing) main.postDelayed(ticker, 250)
    }

    override fun open(source: MediaSource, startMs: Long, autoPlay: Boolean) {
        state.durationMs = SAMPLE_LOOP_MS
        state.videoWidth = 1280
        state.videoHeight = 720
        state.jump(startMs)
        state.report(startMs)
        state.hasFrame = true
        state.buffering = false
        state.playing = autoPlay
        state.setRunning(autoPlay)
        sync()
    }

    override fun play() {
        state.playing = true
        state.setRunning(true)
        main.post { state.buffering = false }
        sync()
    }

    override fun pause() {
        state.playing = false
        state.setRunning(false)
        sync()
    }

    override fun seekTo(positionMs: Long) {
        state.jump(positionMs)
        state.report(positionMs)
    }

    override fun setVolume(percent: Int) {
        state.volume = percent
    }

    override fun setMuted(muted: Boolean) {
        state.muted = muted
    }

    override fun setRate(rate: Float) {
        state.setSpeed(rate)
    }

    @Composable
    override fun Surface(modifier: Modifier) {
        AndroidView(modifier = modifier, factory = { ctx -> DemoBackdrop(ctx) })
    }

    override fun release() {
        main.removeCallbacks(ticker)
    }
}

/** 假的"视频画面"：一块静态的渐变风景（只在 surface 建好 / 尺寸变化时画一次，平时一帧都不画）。 */
private class DemoBackdrop(context: Context) : SurfaceView(context), SurfaceHolder.Callback {
    init {
        holder.addCallback(this)
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        draw(holder)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        draw(holder)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {}

    private fun draw(holder: SurfaceHolder) {
        val canvas: Canvas = try {
            holder.lockCanvas() ?: return
        } catch (_: RuntimeException) {
            return
        }
        try {
            val w = canvas.width.toFloat()
            val h = canvas.height.toFloat()
            val sky = Paint().apply {
                shader = LinearGradient(0f, 0f, 0f, h, intArrayOf(0xFF8EC5E8.toInt(), 0xFFF2D9B1.toInt(), 0xFF6E8B5B.toInt()), null, Shader.TileMode.CLAMP)
            }
            canvas.drawRect(0f, 0f, w, h, sky)
            canvas.drawCircle(w * 0.78f, h * 0.28f, h * 0.12f, Paint().apply { color = 0xFFFFF3C4.toInt() })
            val hill = android.graphics.Path().apply {
                moveTo(0f, h)
                lineTo(0f, h * 0.72f)
                lineTo(w * 0.22f, h * 0.50f)
                lineTo(w * 0.40f, h * 0.68f)
                lineTo(w * 0.62f, h * 0.42f)
                lineTo(w * 0.85f, h * 0.66f)
                lineTo(w, h * 0.58f)
                lineTo(w, h)
                close()
            }
            canvas.drawPath(hill, Paint().apply { color = 0xFF2F4A3A.toInt() })
            canvas.drawRect(0f, h * 0.86f, w, h, Paint().apply { color = 0xFF1E2F25.toInt() })
            canvas.drawText("离线演示画面（没有解码、没有联网）", 24f, h - 24f, Paint().apply {
                color = Color.WHITE
                textSize = h * 0.035f
                isAntiAlias = true
            })
        } finally {
            holder.unlockCanvasAndPost(canvas)
        }
    }
}

/** 假视频源：弹幕是 [sampleDanmaku]（第 1 段，24 秒一循环），播放地址不会被读（[DanmakuDemoEngine.open] 不解码）。 */
private object DanmakuDemoSource : VideoSource {
    override val platform = BILI
    override val feeds = emptySet<FeedKind>()
    override val login = LoginSpec("", "", "", emptySet())
    override val apiBase = ""
    private val samples by lazy { sampleDanmaku() }

    override suspend fun whoami(): UserRef? = null
    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> = Page(emptyList(), null)
    override suspend fun raw(request: RawRequest): RawResponse = throw java.io.IOException("演示环境：不联网")
    override suspend fun detail(id: String): FeedDetail = throw java.io.IOException("演示环境：不联网")
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> = Page(emptyList(), null)
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> = Page(emptyList(), null)
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo = PlayInfo(
        durationMs = SAMPLE_LOOP_MS,
        quality = 80,
        qualities = listOf(Quality(80, "1080P 演示")),
        video = listOf(VideoStream(80, VideoCodec.AVC, url = "demo", bandwidth = 2000)),
        audio = listOf(AudioStream(30280, "mp4a.40.2", "demo", bandwidth = 100)),
    )

    override suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> = if (segment == 1) samples else emptyList()
}
