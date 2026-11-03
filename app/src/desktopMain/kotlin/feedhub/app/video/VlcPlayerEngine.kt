package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import uk.co.caprica.vlcj.factory.MediaPlayerFactory
import uk.co.caprica.vlcj.player.base.MediaPlayer
import uk.co.caprica.vlcj.player.base.MediaPlayerEventAdapter
import uk.co.caprica.vlcj.player.embedded.videosurface.callback.BufferFormat
import uk.co.caprica.vlcj.player.embedded.videosurface.callback.BufferFormatCallback
import uk.co.caprica.vlcj.player.embedded.videosurface.callback.RenderCallback
import uk.co.caprica.vlcj.player.embedded.videosurface.callback.format.RV32BufferFormat
import uk.co.caprica.vlcj.player.embedded.EmbeddedMediaPlayer
import java.awt.GraphicsEnvironment
import java.nio.ByteBuffer
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit

/**
 * 桌面播放器：VLCJ（libvlc 3.0）。
 *
 * 画面：用 `CallbackVideoSurface`，libvlc 把解码后的每一帧（RV32 = BGRA）交给我们，拷进一张 Skia 图片，
 * 在 Compose 的 Canvas 里画出来，所以弹幕和控件能盖在视频上面（嵌原生窗口的做法会盖住 Compose 内容）。
 * 声音：libvlc 自己的音频输出（PulseAudio 等）。
 * B站 DASH：视频轨当主输入，音频轨用 `:input-slave=` 挂上；请求头用 `:http-referrer=` / `:http-user-agent=`。
 * **分片中转**（[VideoRelay]，和 GStreamer 引擎同一套）：给了 `MediaSource.relayClient` 时，**所有**视频地址
 * （DASH 的两条轨、HLS 播放列表、整段 mp4）都换成本机回环地址再交给 libvlc，真正的字节由这个平台的 OkHttp
 * 客户端按平台代理去取——libvlc 只连本机。这样"指定代理 / 直连 / fail-closed"和接口那一路完全一致
 * （libvlc 自己的 `:http-proxy=` / `:socks=` 只在没有 relayClient 时用）。
 * 注意：libvlc 的 http access 会自己读 env `http_proxy`，会不会把本机回环请求也截走**没在本机验证过**
 * （见 [vlcProxyOptions] 的注释）；env 里没有代理时不受影响。
 * 解码：软解（`--avcodec-hw=none`）。libvlc 3.0 的回调式输出（vmem）没有硬解设备，VA-API / VDPAU 会先试一遍再退回软解，不如直接跳过。
 * 这是 [GstPlayerEngine]（硬解）不可用时的退路。
 */
class VlcPlayerEngine : PlayerEngine {
    override val state = PlayerState()
    override val unavailable: String? = null

    private val headless = GraphicsEnvironment.isHeadless()
    private val factory: MediaPlayerFactory
    private val player: EmbeddedMediaPlayer

    /** libvlc 的调用都排在这条线程上：stop / release 可能阻塞（等网络），不能卡界面。 */
    private val executor = Executors.newSingleThreadExecutor { r -> Thread(r, "vlc-control").apply { isDaemon = true } }
    private val ticker: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "vlc-stats").apply { isDaemon = true }
    }

    @Volatile private var released = false
    @Volatile private var current: MediaSource? = null

    /** 这次播放的分片中转（换媒体 / 释放时关掉）。 */
    private var relay: RelayHandle? = null
    @Volatile private var attempt = 0
    @Volatile private var pendingStartMs = 0L
    @Volatile private var autoPlay = true
    @Volatile private var looping = false

    // 估算已缓冲：打开 / 跳转时记下起点和当时读到的字节数，之后每读一点按码率折算成时长
    @Volatile private var bufferBaseMs = 0L
    @Volatile private var bufferBaseBytes = 0L

    // ---------- 帧 ----------
    private val surface = FrameSurface(state)

    init {
        val args = buildList {
            add("--no-video-title-show")
            add("--no-osd")
            add("--no-snapshot-preview")
            add("--no-xlib")
            if (System.getenv("FEEDHUB_VLC_ARGS") == null) add("--quiet")
            add("--avcodec-hw=none")
            // 无窗口渲染（截图工具）时不要在用户电脑上出声
            if (headless) add("--aout=adummy")
            // 调试用：FEEDHUB_VLC_ARGS="-vv --avcodec-hw=none" 之类，空格分隔
            System.getenv("FEEDHUB_VLC_ARGS")?.split(' ')?.filter { it.isNotBlank() }?.let { addAll(it) }
        }
        factory = MediaPlayerFactory(args)
        player = factory.mediaPlayers().newEmbeddedMediaPlayer()
        player.videoSurface().set(
            factory.videoSurfaces().newVideoSurface(
                object : BufferFormatCallback {
                    override fun getBufferFormat(sourceWidth: Int, sourceHeight: Int): BufferFormat {
                        // 超过 1920 宽的让 libvlc 缩小再给我们：窗口里用不着更大的，省拷贝
                        var w = sourceWidth
                        var h = sourceHeight
                        if (w > MAX_FRAME_WIDTH) {
                            h = (h.toLong() * MAX_FRAME_WIDTH / w).toInt()
                            w = MAX_FRAME_WIDTH
                        }
                        w = w and 1.inv()
                        h = h and 1.inv()
                        return RV32BufferFormat(w, h)
                    }

                    override fun newFormatSize(bufferWidth: Int, bufferHeight: Int, displayWidth: Int, displayHeight: Int) {
                        // 先用缓冲区的尺寸；libvlc 报出视频真实尺寸后（readVideoSize）再校正，缓冲区有时比画面多几行
                        if (state.videoWidth == 0) {
                            state.videoWidth = displayWidth
                            state.videoHeight = displayHeight
                        }
                    }

                    override fun allocatedBuffers(buffers: Array<ByteBuffer>) {}
                },
                object : RenderCallback {
                    override fun lock(mediaPlayer: MediaPlayer) {}
                    override fun display(
                        mediaPlayer: MediaPlayer,
                        nativeBuffers: Array<ByteBuffer>,
                        bufferFormat: BufferFormat,
                        displayWidth: Int,
                        displayHeight: Int,
                    ) = onFrame(nativeBuffers[0], displayWidth, displayHeight)

                    override fun unlock(mediaPlayer: MediaPlayer) {}
                },
                true,
            ),
        )
        player.events().addMediaPlayerEventListener(object : MediaPlayerEventAdapter() {
            override fun videoOutput(mediaPlayer: MediaPlayer, newCount: Int) {
                readVideoSize()
            }

            override fun playing(mediaPlayer: MediaPlayer) {
                readVideoSize()
                // 换了媒体后速率会回到 1.0：把倍速再设回去
                if (state.speed != 1f) player.controls().setRate(state.speed)
                state.error = null
                state.ended = false
                state.playing = true
                state.setRunning(true)
            }

            override fun paused(mediaPlayer: MediaPlayer) {
                state.playing = false
                state.setRunning(false)
            }

            override fun stopped(mediaPlayer: MediaPlayer) {
                state.playing = false
                state.setRunning(false)
            }

            override fun finished(mediaPlayer: MediaPlayer) {
                if (looping) {
                    // 循环：从头重新开始（libvlc 的退路引擎没有无缝循环，重开一次）
                    current?.let { open(it, 0L, true) }
                    return
                }
                state.playing = false
                state.ended = true
                state.setRunning(false)
                state.report(state.durationMs)
            }

            override fun buffering(mediaPlayer: MediaPlayer, newCache: Float) {
                state.buffering = newCache < 100f
            }

            override fun timeChanged(mediaPlayer: MediaPlayer, newTime: Long) {
                if (newTime >= 0) state.report(newTime)
                if (state.videoWidth == 0) readVideoSize()
            }

            override fun lengthChanged(mediaPlayer: MediaPlayer, newLength: Long) {
                if (newLength > 0) state.durationMs = newLength
            }

            override fun error(mediaPlayer: MediaPlayer) {
                onError()
            }
        })
        ticker.scheduleWithFixedDelay({ runCatching { updateBuffered() } }, 1, 1, TimeUnit.SECONDS)
    }

    /** libvlc 报告的视频真实尺寸（用来算画面比例；缓冲区尺寸有时比画面多几行）。 */
    private fun readVideoSize() {
        val dim = runCatching { player.video().videoDimension() }.getOrNull() ?: return
        if (dim.width > 0 && dim.height > 0) {
            state.videoWidth = dim.width
            state.videoHeight = dim.height
        }
    }

    // ---------- 控制 ----------

    override fun open(source: MediaSource, startMs: Long, autoPlay: Boolean) {
        current = source
        attempt = 0
        pendingStartMs = startMs
        this.autoPlay = autoPlay
        state.error = null
        state.ended = false
        state.buffering = true
        state.durationMs = source.durationMs ?: 0L
        state.jump(startMs)
        state.videoWidth = 0
        state.videoHeight = 0
        state.bufferedMs = startMs
        state.decoder = "libvlc（软解）"
        state.hardwareDecoding = false
        exec { startCurrent() }
    }

    /** 在控制线程上：按 [attempt] 选主地址 / 备用地址，开始播。 */
    private fun startCurrent() {
        val src = current ?: return
        if (released) return
        val video = src.videoUrls.getOrNull(attempt) ?: return
        val audio = src.audioUrls.let { it.getOrNull(attempt) ?: it.lastOrNull() }
        // 分片中转：DASH / HLS 的地址换成本机回环，真正的分片由这个平台的 OkHttp 客户端按平台代理去取。
        // 每次重开（换地址 / 换清晰度）都新建一个：旧的先关掉，在飞的请求一起取消。
        relay?.close()
        val r = src.relayFactory?.let { it.open() }
        relay = r
        val v = when {
            r == null -> video
            // m3u8 / .mpd 直链：中转现取现改写；别的（单文件、分片）登记成本机的一个资源
            isHlsUrl(video) || isDashManifestUrl(video) -> r.playlist(video)
            else -> r.resource(video)
        }
        val a = if (audio == null) null else r?.resource(audio) ?: audio
        val options = buildList {
            src.headers.entries.firstOrNull { it.key.equals("Referer", true) }?.let { add(":http-referrer=${it.value}") }
            src.headers.entries.firstOrNull { it.key.equals("User-Agent", true) }?.let { add(":http-user-agent=${it.value}") }
            if (a != null) add(":input-slave=$a")
            if (pendingStartMs > 1000) add(":start-time=${pendingStartMs / 1000.0}")
            add(":network-caching=3000")
            // 经中转的媒体只连本机回环，代理由中转的 OkHttp 决定；没中转才用 libvlc 自己的代理选项
            if (r == null) addAll(vlcProxyOptions(src.proxy))
        }
        bufferBaseMs = pendingStartMs
        bufferBaseBytes = 0
        if (autoPlay) player.media().play(v, *options.toTypedArray()) else player.media().startPaused(v, *options.toTypedArray())
    }

    /** libvlc 报错：试下一个备用地址（从当前位置接着播），都不行才报给界面。 */
    private fun onError() {
        val src = current ?: return
        if (released) return
        if (attempt + 1 < src.videoUrls.size) {
            attempt++
            pendingStartMs = state.nowMs()
            exec { startCurrent() }
        } else {
            state.error = "播放失败：连不上视频服务器，或这个视频暂时播不了"
            state.playing = false
            state.buffering = false
            state.setRunning(false)
        }
    }

    override fun play() {
        val src = current ?: return
        if (state.ended) {
            open(src, 0L, true)
            return
        }
        state.playing = true
        state.setRunning(true)
        exec { player.controls().setPause(false) }
    }

    override fun pause() {
        state.playing = false
        state.setRunning(false)
        exec { player.controls().setPause(true) }
    }

    override fun seekTo(positionMs: Long) {
        val target = positionMs.coerceIn(0, state.durationMs.takeIf { it > 0 } ?: Long.MAX_VALUE)
        if (state.ended) {
            current?.let { open(it, target, true) }
            return
        }
        state.jump(target)
        bufferBaseMs = target
        bufferBaseBytes = lastReadBytes
        exec { player.controls().setTime(target) }
    }

    override fun setVolume(percent: Int) {
        val p = percent.coerceIn(0, 100)
        state.volume = p
        exec { player.audio().setVolume(p) }
    }

    override fun setMuted(muted: Boolean) {
        state.muted = muted
        exec { player.audio().setMute(muted) }
    }

    override fun setLooping(loop: Boolean) {
        looping = loop
    }

    override fun setRate(rate: Float) {
        val v = rate.coerceIn(0.25f, 4f)
        state.setSpeed(v)
        exec { player.controls().setRate(v) }
    }

    @Volatile private var lastReadBytes = 0L

    private fun updateBuffered() {
        val src = current ?: return
        if (released || !state.playing && !state.buffering) return
        val stats = player.media().info().statistics() ?: return
        val bytes = stats.inputBytesRead().toLong()
        lastReadBytes = bytes
        val bw = src.bandwidthBps ?: return
        if (bw <= 0) return
        val sec = (bytes - bufferBaseBytes).coerceAtLeast(0) * 8.0 / bw
        val est = bufferBaseMs + (sec * 1000).toLong()
        val limit = state.durationMs.takeIf { it > 0 } ?: Long.MAX_VALUE
        state.bufferedMs = est.coerceAtLeast(state.positionMs).coerceAtMost(limit)
    }

    // ---------- 画面 ----------

    private fun onFrame(buf: ByteBuffer, w: Int, h: Int) {
        if (released) return
        surface.submit(buf, w, h)
        if (!state.hasFrame) state.hasFrame = true
    }

    /** libvlc 的播放统计（验证 / 调试用）：解了多少视频帧、音频块，丢了多少。 */
    fun statsSummary(): String {
        val st = player.media().info().statistics() ?: return "无统计"
        return "视频解码 ${st.decodedVideo()} 显示 ${st.picturesDisplayed()} 丢 ${st.picturesLost()}；音频解码 ${st.decodedAudio()} 播放 ${st.audioBuffersPlayed()} 丢 ${st.audioBuffersLost()}"
    }

    /** libvlc 已显示的帧数（验证 / 对比用）。 */
    fun framesDisplayed(): Int = player.media().info().statistics()?.picturesDisplayed() ?: 0

    /** 把最新一帧编码成 PNG（验证 / 调试用）；还没有帧就 null。 */
    fun frameAsPng(): ByteArray? = surface.png()

    @Composable
    override fun Surface(modifier: Modifier) = surface.Draw(modifier)

    override fun release() {
        if (released) return
        released = true
        ticker.shutdownNow()
        // 先把控制线程上排着的活干完，再停、释放；全在后台线程，不卡界面
        executor.execute {
            runCatching { relay?.close() }
            relay = null
            runCatching { player.controls().stop() }
            runCatching { player.release() }
            runCatching { factory.release() }
            surface.close()
        }
        executor.shutdown()
    }

    private fun exec(block: () -> Unit) {
        if (released) return
        runCatching { executor.execute { runCatching(block).onFailure { System.err.println("vlc: $it") } } }
    }

    private companion object {
        const val MAX_FRAME_WIDTH = 1920
    }
}
