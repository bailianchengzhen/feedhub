package feedhub.app.video

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import feedhub.core.VideoCodec
import feedhub.player.GstMediaSource
import feedhub.player.GstSurfaceLayout
import feedhub.player.RustGstPlayerEngine
import feedhub.player.RustGstRuntime
import java.awt.GraphicsEnvironment
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * **桌面播放器（首选）：GStreamer 1.26 + VA-API 硬解，引擎本体在 Rust（`gstreamer-rs`）。**
 *
 * 管线、总线、状态机、seek / 倍速 / 循环 / 缓冲 /
 * 硬解退软解 / 备用地址 / 帧节奏记账 / `souphttpsrc` 的代理与请求头**全在那边**；
 * 这个类只剩宿主该做的那四件事：
 *
 * 1. **画**：每帧问一次 Rust 的 `frameSeq()`，变了就 `acquireFrame()` 占住那一帧，
 *    把**共享直接内存**的地址交给 [FrameSurface]（零拷贝，像素不过 UniFFI），再放开；
 * 2. **状态容器**：每 250ms 问一次快照，把字段赋给 [PlayerState]（这里不判断任何东西）；
 * 3. 转发控制调用（`play` / `pause` / `seekTo` …）；
 * 4. 定时器。
 *
 * 原来写在 `GstPlayerEngine.kt` 头上那一段"为什么这样建管线"的说明，跟着逻辑一起搬进了
 * `gst_engine/engine.rs` 的模块注释和各个函数头（那些注释是踩出来的，一条都没丢）。
 *
 * @param hardware false = 不用硬解（对比测试用），环境变量 `FEEDHUB_HWDEC=0` 同效。
 * @param debugSync 检查工具用：保留音频 / 视频 sink 的引用，[debugSinkPositionsMs] 能分别问它们
 *   "此刻渲染到媒体的哪个位置"，看音画同步。
 */
internal class RustGstEngine(
    private val hardware: Boolean = true,
    private val debugSync: Boolean = false,
    /**
     * `uridecodebin3` 的 `use-buffering`（`RelayStallProbeTest` 用它压高水位）。
     *
     * **只能构造时定**：`uridecodebin3` 的属性是建管线的时候读的，事后改对已经起来的管线没用。
     * 产品路径恒为 `true`（和原来 Kotlin 引擎一样）。
     */
    internal val debugUseBuffering: Boolean = true,
) : PlayerEngine {
    /**
     * Rust 那边的引擎句柄。构造它会触发 GStreamer 的进程级初始化 + VA-API 探测
     * （一次，几百毫秒）——所以 [feedhub.app.video.createPlayerEngine] 的 `try` 能接住它。
     */
    private val inner: RustGstPlayerEngine = try {
        RustGstPlayerEngine(
            feedhub.player.FfiGstConfigBuilder.build(
                hardware = hardware && System.getenv("FEEDHUB_HWDEC") != "0",
                debugSync = debugSync,
                // `use-buffering`：DASH / HLS 的 demuxer 会按它报百分比；关掉之后界面就不看
                // "缓冲到 100%"，直接按 preroll + 时钟走（检查工具用它压高水位，见 RelayStallProbeTest）
                useBuffering = debugUseBuffering,
                silent = GraphicsEnvironment.isHeadless() || System.getenv("FEEDHUB_AUDIO") == "none",
            ),
        )
    } catch (e: Throwable) {
        error("GStreamer（Rust 引擎）起不来：${e.message}")
    }

    override val state = PlayerState()
    override val unavailable: String? = inner.unavailable

    override val hardwareCodecs: Set<VideoCodec> get() = inner.hardwareCodecs()

    /** 能硬解的编码 + 播放中实测出来的"多大负载还跟得上"（见 [DecodeLedger]）。 */
    override val decodeCapability: DecodeCapability get() = currentDecodeCapability(hardwareCodecs)

    private val surface = FrameSurface(state)

    /** 定时器：每 250ms 拉一次快照 + 每帧轮询共享内存。 */
    private val ticker: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "gst-stats").apply { isDaemon = true }
    }

    @Volatile private var released = false

    /** 上一次看到的帧序号（变了才去读一帧）。 */
    @Volatile private var lastSeq = 0L

    /** 帧计数（给 [framesReceived]；比每帧过边界问一次省）。 */
    private val framesSeen = AtomicLong()

    // ---------- 测量 / 验证 ----------

    /**
     * 一帧的记录。
     *
     * `pts_ns` 是媒体时间轴上的呈现时间，
     * `running_ns` 是管线运行时间（时钟 - base_time），`luma` 是画面 3×3 个点亮度的最小值
     * （0~255，AvSyncRig 靠它认"整屏白闪"）。
     */
    internal class FrameStamp(val ptsNs: Long, val runningNs: Long, val luma: Int, val width: Int, val height: Int)

    /** 一段音频的记录（和原 Kotlin 引擎的 `AudioStamp` 同名）。`data` 是 F32LE 交错的采样。 */
    internal class AudioStamp(
        val ptsNs: Long,
        val runningNs: Long,
        val data: java.nio.ByteBuffer,
        val rate: Int,
        val channels: Int,
    )

    /** 装帧回调（每帧过一次 JNA 边界，**生产路径不装**）。 */
    internal var debugFrameHook: ((FrameStamp) -> Unit)? = null
        set(value) {
            field = value
            if (!released) {
                inner.setFrameHook(value?.let { h ->
                    { pts: Long, run: Long, luma: Int, w: Int, h2: Int -> h(FrameStamp(pts, run, luma, w, h2)) }
                })
            }
        }

    /**
     * 装音频回调（要在 [open] 之前设；会顺带把音频 sink 换成不出声的 appsink）。
     * 采样数据从 Rust 过边界是 `ByteArray`，这里包成 `ByteBuffer`（测试里 `getFloat` 用得着）。
     */
    internal var debugAudioHook: ((AudioStamp) -> Unit)? = null
        set(value) {
            field = value
            if (!released) {
                inner.setAudioHook(value?.let { h ->
                    { pts: Long, run: Long, data: ByteArray, rate: Int, ch: Int ->
                        h(AudioStamp(pts, run, java.nio.ByteBuffer.wrap(data).order(java.nio.ByteOrder.LITTLE_ENDIAN), rate, ch))
                    }
                })
            }
        }

    /** 硬解输出宽度取不取 64 的倍数（检查工具量"不对齐会怎样"用）。 */
    internal var debugAlignWidth: Boolean = true

    /**
     * `uridecodebin3` 的 `use-buffering`（`RelayStallProbeTest` 用它压高水位）。
     *
     * **构造时定**（`uridecodebin3` 的属性只在建管线时设一次），所以这个 setter 只对
     * "还没 `open` 过"的实例有效——检查工具就是那个用法。
     */
    /** 装帧回调（每帧过一次 JNA 边界，**生产路径不装**）。 */
    internal fun setDebugFrameHook(hook: ((ptsNs: Long, runningNs: Long, luma: Int, width: Int, height: Int) -> Unit)?) =
        inner.setFrameHook(hook)

    /** 装音频回调（要在 [open] 之前设；会顺带把音频 sink 换成不出声的 appsink）。 */
    internal fun setDebugAudioHook(hook: ((ptsNs: Long, runningNs: Long, data: ByteArray, rate: Int, channels: Int) -> Unit)?) =
        inner.setAudioHook(hook)

    /**
     * 这份引擎上发出过几次 **flush** seek（拖动 / 循环 / 回退的变速都算）。
     *
     * 是 `AtomicInteger` 而不是 `Int`：原来的 Kotlin 引擎叫 `debugFlushSeeks.get()`，验收门
     * （`GstRateChangeTest` 断言"换倍速 flush seek 恒为 0"）是按那个形状写的，对拍和探针
     * 也都在用——**口径和形状都不改**，只是背后的数从 Rust 的 `EngineStats` 来。
     */
    internal val debugFlushSeeks: java.util.concurrent.atomic.AtomicInteger
        get() = java.util.concurrent.atomic.AtomicInteger(inner.stats().`flushSeeks`.toInt())

    /** 这份引擎上成功走上即时变速（`INSTANT_RATE_CHANGE`）的次数。 */
    internal val debugInstantRateChanges: java.util.concurrent.atomic.AtomicInteger
        get() = java.util.concurrent.atomic.AtomicInteger(inner.stats().`instantRateChanges`.toInt())

    /** 一共换过几次备用地址。 */
    internal val debugFailovers: java.util.concurrent.atomic.AtomicInteger
        get() = java.util.concurrent.atomic.AtomicInteger(inner.stats().`failovers`.toInt())

    /** 退回软解几次。 */
    internal val debugSoftwareFallbacks: java.util.concurrent.atomic.AtomicInteger
        get() = java.util.concurrent.atomic.AtomicInteger(inner.stats().`softwareFallbacks`.toInt())

    internal fun debugElements(): List<String> = inner.debugElements()
    internal fun debugAudioCaps(): String? = inner.debugAudioCaps()
    internal fun debugVideoCaps(): String? = inner.debugVideoCaps()
    internal fun debugVaDevices(): List<String> = inner.debugVaDevices()
    internal fun debugPipelineName(): String? = inner.debugPipelineName()

    /** 音频 sink 和视频 sink 各自此刻渲染到的媒体位置（毫秒）；取不到返回 null。 */
    internal fun debugSinkPositionsMs(): Pair<Long, Long>? = inner.debugSinkPositionsMs()

    /**
     * 这次播放用的中转（检查工具读它挂记账钩子）。
     *
     * 所以这里从"宿主递进来的 `VideoRelay`"变成引擎自己的 [relay]——探针还是能看"中转向上游取过
     * 几次 / 并发峰值 / 关掉之后在飞的请求归不归零"，而且看到的就是真在跑的那一份。
     */
    internal fun debugRelay(): RelayHandle? = relay

    /** 中转向上游取过几个请求 + 上游主机集合；没挂中转就是 null。 */
    internal fun debugRelayStats(): Pair<Int, Set<String>>? =
        relay?.let { it.upstreamRequests to it.upstreamHostNames }

    /** 帧序号（量每帧成本用；和 `tick()` 里那个 `frameSeq()` 是同一个）。 */
    internal fun frameSeqForMeasurement(): Long = inner.frameSeq()

    /**
     * **只量一帧**的取用成本（`GstFrameCostTest` 用）：`acquireFrame` → 造 Skia 图 → 放开，
     * 把前两段的纳秒数交给 [sink]。
     *
     * 和 [pumpFrame] 的差别只有两处：不碰 Compose 状态、不 `latest?.close()`（量的是"取一帧"，
     * 不是"换掉上一帧"）。像素一个字节都不拷。
     */
    internal fun measureOneFrame(sink: (acquireNs: Long, submitNs: Long) -> Unit) {
        if (released) return
        val info = inner.surface() ?: return
        val t0 = System.nanoTime()
        val header = inner.acquireFrame() ?: return
        val t1 = System.nanoTime()
        try {
            surface.submitSharedForMeasurement(
                addr = info.`addr`.toLong(),
                slot = header.`slot`.toLong(),
                slotStride = info.`slotStride`.toLong(),
                headerBytes = GstSurfaceLayout.slotHeaderBytes,
                w = header.`width`,
                h = header.`height`,
                stride = header.`stride`,
            )
        } finally {
            inner.releaseFrameSlot()
        }
        val t2 = System.nanoTime()
        sink(t1 - t0, t2 - t1)
    }

    /** 实际在用的视频解码器元素名。 */
    internal fun debugDecoder(): String? =
        debugElements().firstOrNull { it.contains("dec") && !it.contains("demux") }

    /** 每条 BUFFERING 报上来的百分比：`元素名=百分比`（查"卡在谁身上"用）。 */
    internal fun debugBufferPct(): List<String> = inner.debugBufferPct()

    /** 给管线里的 `dashdemux2` 设一个属性（探针压水位用）；元素还没建出来时 false。 */
    internal fun setDemuxProperty(name: String, value: String): Boolean = inner.setDemuxProperty(name, value)

    /** 假装画布变成了 [w]×[h] 像素（检查工具用，界面里是 Canvas 的尺寸回调）。 */
    internal fun requestSurfaceSize(w: Int, h: Int) = surface.onSurfaceSize?.invoke(w, h)

    /** 到目前为止收到的帧数（验证用）。 */
    val framesReceived: Long get() = framesSeen.get()

    /**
     * 画面的像素尺寸（GPU 缩放之后）和管线协商的格式（验证用）。
     *
     * 只在出帧时更新（每帧一次字符串拼接会拖慢主路径），格式来自 Rust 那边的
     * `debug_video_caps`——检查工具要看"到底协商成了什么"时才去问。
     */
    @Volatile var lastFrameInfo: String = ""
        private set

    /** 视频 appsink 的 `max-buffers`（检查工具调；`-1` = 用引擎默认）。 */
    var debugSinkMaxBuffers: Int
        get() = sinkBuffers
        set(value) {
            sinkBuffers = value
            if (!released) inner.setSinkMaxBuffers(value.coerceAtLeast(1))
        }

    private var sinkBuffers: Int = 2

    /** 把当前这一帧存成 PNG（检查工具用）。 */
    fun snapshotPng(): ByteArray? = inner.frameAsPng()

    init {
        surface.onSurfaceSize = { w, h ->
            if (!released) inner.setSurfaceSize(w, h)
        }
        // 帧节奏的实测写进**同一份**账本（选流和设置页读到的就是它）
        runCatching { inner.attachLedger(DecodeLedger.rustLedger()) }
        // 20ms 一次轮询：够 50 fps 的片子不丢帧，也不会把界面线程唤醒得太勤
        ticker.scheduleWithFixedDelay({ runCatching { tick() } }, 250, 20, TimeUnit.MILLISECONDS)
    }

    // ---------- 一次播放 ----------

    override fun open(source: MediaSource, startMs: Long, autoPlay: Boolean) {
        if (released) return
        // 中转按 `MediaSource.relayFactory` 现开一份。地址里凡是要改写的，`toGstMedia` 会把
        // 它们换成本机回环；开不出来就是"这次播放没有中转"，那时远端清单只能按老办法直连。
        //
        // **每次 open 都先关上一份**。
        closeRelay()
        relay = source.relayFactory?.let { factory ->
            runCatching { factory.open() }.getOrElse { e ->
                System.err.println("gst: 建中转失败（${e.message}），这次播放不走中转")
                null
            }
        }
        surface.clear()
        lastSeq = 0
        state.jump(startMs)
        // `autoPlay` 时**一开始就报"在播"**（和原来 Kotlin 引擎的 `state.playing = autoPlay` 一致）：
        // 预卷还没完，但用户看到的是"正在加载然后播"，不是"弹一下大播放键再自己走"。
        if (autoPlay) markPlaying() else markStopped()
        inner.open(source.toGstMedia(), startMs, autoPlay)
    }

    override fun play() {
        if (released) return
        markPlaying()
        inner.play()
    }

    override fun pause() {
        if (released) return
        markStopped()
        inner.pause()
    }

    override fun seekTo(positionMs: Long) {
        if (released) return
        state.jump(positionMs)
        inner.seekTo(positionMs)
    }

    override fun setVolume(percent: Int) {
        if (released) return
        state.volume = percent.coerceIn(0, 100)
        inner.setVolume(percent)
    }

    override fun setMuted(muted: Boolean) {
        if (released) return
        state.muted = muted
        inner.setMuted(muted)
    }

    override fun setRate(rate: Float) {
        if (released) return
        state.setSpeed(rate)
        speedGraceUntil = System.nanoTime() + 400_000_000 // 400 ms 宽限
        val before = inner.stats().`flushSeeks`.toInt()
        inner.setRate(rate)
        // 回退的那次 seek 是异步发的；等它真的发生了再开窗口（`setRate` 返回时可能还没发）
        if (!allowInstantRate) {
            fallbackSeekWindow = true
            fallbackTicks = 0
        }
        openFallbackWindowIfSeeked(before)
    }

    /** 换倍速走不走即时变速（`false` = 强制走 flush seek 回退；验证回退路径用）。 */
    internal var debugAllowInstantRate: Boolean
        get() = allowInstantRate
        set(value) {
            allowInstantRate = value
            if (!released) inner.setAllowInstantRate(value)
        }

    /** 这次换倍速是不是走了回退（flush seek）——由引擎的 `flushSeeks` 计数变化判断。 */
    private fun openFallbackWindowIfSeeked(before: Int) {
        if (inner.stats().`flushSeeks`.toInt() > before) {
            fallbackSeekWindow = true
            fallbackTicks = 0
        }
    }

    private var allowInstantRate: Boolean = true

    override fun setLooping(loop: Boolean) {
        if (!released) inner.setLooping(loop)
    }

    override fun release() {
        if (released) return
        released = true
        runCatching { ticker.shutdownNow() }
        runCatching { inner.release() }
        closeRelay()
        surface.close()
    }

    // ---------- 周期任务 ----------

    /**
     * 每 20ms：有新帧就交一帧；每 250ms（4 次里 1 次）再把快照搬进 [PlayerState]。
     *
     * 为什么位置更新不跟着每帧走：位置是 Compose 状态，写一次就要一帧；250ms 一次和原来
     * Kotlin 引擎的 ticker 一样（界面自己按插值往前推，见 `PlayerState.nowMs`）。
     */
    private var tickCount = 0

    /**
     * "刚走了一次换倍速的回退 seek"的窗口：这段时间里拒掉向后的位置报告。
     *
     * 为什么需要窗口而不是只看一个快照：flush seek 落地（ASYNC_DONE）之后 appsink 里还压着
     * 几帧，它们带着 seek 目标**之前**的 PTS，于是"位置往回"会晚一两个 tick 才出现。
     */
    @Volatile private var fallbackSeekWindow = false
    private var fallbackTicks = 0

    /**
     * 换倍速之后的"宽限期"（纳秒时刻）：这段时间里不接受 `buffering`。
     *
     * 为什么需要它：`instant_rate_change` 本身不清管线，但 dashdemux2 在时间轴速率变了之后
     * 会重排一次下游、顺带报一条百分比 < 100 的 BUFFERING 消息（不是真的在等数据）。引擎照实
     * 报，旧 Kotlin 引擎在这条上表现一样
     * （这次的差别只在于它从来没被 DASH + 倍速这个组合测过）。
     */
    @Volatile private var speedGraceUntil = 0L

    private fun tick() {
        if (released) return
        val seq = inner.frameSeq()
        if (seq != lastSeq) {
            lastSeq = seq
            pumpFrame()
        }
        tickCount++
        if (tickCount % 12 != 0) return // 12 × 20ms ≈ 250ms
        val snap = inner.pollSnapshot() ?: return
        // 引擎在"静默 seek"（换倍速回退）期间会报 buffering=false；但**倍速变化瞬间**
        // demuxer 可能发一条百分比 < 100 的 BUFFERING，引擎照实报一次 → 界面转一下圈。
        // 换倍速这一侧不该有转圈，所以只要倍速刚改过（`speedGraceUntil`），就不接受 buffering。
        state.buffering = snap.`buffering` && System.nanoTime() > speedGraceUntil
        if (snap.`ended` && !state.ended) {
            // 播到结尾：位置钉在时长上、界面停在最后一帧（和不循环时原来 Kotlin 引擎一样）
            state.ended = true
            markStopped()
            if (snap.`durationMs` > 0) state.jump(snap.`durationMs`)
        }
        if (snap.`error` != null) markStopped()
        if (snap.`durationMs` > 0) state.durationMs = snap.`durationMs`
        if (snap.`videoWidth` > 0) state.videoWidth = snap.`videoWidth`
        if (snap.`videoHeight` > 0) state.videoHeight = snap.`videoHeight`
        if (snap.`hasFrame` != state.hasFrame) state.hasFrame = snap.`hasFrame`
        snap.`decoder`?.let { state.decoder = it }
        snap.`hardwareDecoding`?.let { state.hardwareDecoding = it }
        snap.`error`?.let { state.error = it }
        // **换倍速的回退 seek 之后，只接受往前的位置**。
        //
        // 那条回退路径 = "用不了即时变速，只能带新速率 flush 一次"。它 seek 到"界面上那一刻的
        // 位置"，但 flush 之后管线实际落在目标**之前**几十~几百毫秒（GOP 边界 / demuxer 重新
        // 起算），于是下一份快照报回来的位置比界面已经显示的小 → 进度条 visibly 往回跳一格。
        // 旧 Kotlin 引擎的回退也是这个毛病（这次是第一次被测量抓住）。
        //
        // 只在"这次倍速是回退过来的"那一段窗口里拒掉向后的位置：**循环**（seek 到 0）和
        // **拖动**（用户自己往回想走）都该照实报，所以不能一刀切成"位置永不许退"。
        // **回退窗口期间整个不报位置**（不是"只拒掉向后的"）。
        //
        // 为什么：`PlayerState.report` 里有一条"预测值和真实值差超过 DRIFT_MS(350) 就把锚点
        // 重置到真实值"。回退 seek 之后真实位置比界面已经推到的位置落后 600ms 左右，那一条
        // 一触发，`nowMs()` 就当场往回掉——进度条往后跳一格，弹幕也跟着倒。
        // 而回退 seek 的**目标就是界面上那一刻的位置**，所以这段时间里"什么都不更新"才是对的：
        // 位置停在原地（不冻进度，因为插值还在按新倍速走），等管线真的从那儿接上再报。
        val holdPosition = fallbackSeekWindow
        if (!holdPosition && snap.`positionMs` >= 0 && !snap.`ended`) {
            state.report(snap.`positionMs`)
        }
        // 窗口**只按时间关**（6 个 tick ≈ 1.5 秒），不因为"这一格没往回"就当场关掉：
        // seek 落地（ASYNC_DONE）之后 appsink 里还压着几帧，它们带着目标**之前**的 PTS，
        // 往回跳要晚一两个 tick 才出现——"没往回就立刻解锁"会正好把它漏过去。
        if (fallbackSeekWindow && ++fallbackTicks > 6) {
            fallbackSeekWindow = false
            fallbackTicks = 0
        }
        if (snap.`bufferedMs` > 0) state.bufferedMs = snap.`bufferedMs`
        // **`state.playing` 只跟着"用户想要的状态"走，不跟管线此刻的实际状态**：
        // 引擎的 `snapshot.playing` 在预卷完成之前 / 缓冲 / seek 期间是 false（它描述的是
        // "画面此刻真的有在按节奏走吗"），拿它盖 `state.playing` 会让界面在大播放键和画面上
        // 反复横跳——用户点了播放、界面却弹回暂停。原来 Kotlin 引擎也是这个口径：`play()`
        // 写 true、`pause()` 写 false，只有出错 / EOS 时才改。
        if (snap.`playing` && !state.playing) markPlaying()
    }

    /**
     * 改 `playing` **和**推进锚点。
     *
     * 两个是一对，不能只改一个：`state.playing` 是 Compose 状态（界面弹不弹大播放键看它），
     * `PlayerState.setRunning` 是"进度插值的锚点"（`nowMs()` 靠它决定要不要往前推）。原来
     * Kotlin 引擎里这两件事是分开写的（`state.playing = true` + `setRunning(true)`）；
     * 于是 `playing` 永远是 false——界面一直显示暂停，而测试里
     * `waitFor { engine.state.playing }` 直接等到超时。
     */
    private fun markPlaying() {
        state.playing = true
        state.setRunning(true)
    }

    private fun markStopped() {
        state.playing = false
        state.setRunning(false)
    }

    /**
     * 从共享内存取一帧交给 [FrameSurface]（**零拷贝**）。
     *
     * 协议：`acquireFrame()` 占住槽→ 造 Skia 的 `Image`
     * （`Data.makeWithoutCopy`，不拷像素）→ `releaseFrameSlot()` 放开。
     */
    private fun pumpFrame() {
        if (released) return
        val info = inner.surface() ?: return
        val header = inner.acquireFrame() ?: return
        try {
            surface.submitShared(
                addr = info.`addr`.toLong(),
                slot = header.`slot`.toLong(),
                slotStride = info.`slotStride`.toLong(),
                headerBytes = GstSurfaceLayout.slotHeaderBytes,
                w = header.`width`,
                h = header.`height`,
                stride = header.`stride`,
            )
            framesSeen.incrementAndGet()
        } finally {
            inner.releaseFrameSlot()
        }
    }

    @Composable
    override fun Surface(modifier: Modifier) = surface.Draw(modifier)

    /**
     * [MediaSource] → Rust 要的那份（只挑引擎用得上的字段）。
     *
     * **中转那一半留在这里**（服务本体在 Rust，`VideoRelay` / `MpdServer` 只是门面；引擎只拿
     * "已经拼好的地址"）。所以这一层要做四件事，和原来 Kotlin 引擎的 `start` 一样：
     *
     * 1. **DASH 音画分离**（`dash` 给全了 SegmentBase）：自己拼一份 MPD 交给中转发（`relay.publishMpd`），
     *    播放器去本机取这份清单、再按它去取 init / 分片。**不能**把 m4s 地址直接喂给 uridecodebin3：
     *    那种单文件分片要靠 sidx 才能定位 / 跳转（`DashManifest` 头上的注释写了为什么）；
     * 2. **该不该走中转**（[needsRelay]）：远端 m3u8 / .mpd（现取现改写）、以及 souphttpsrc
     *    表达不了的「直连 / SOCKS5 / 内置核心」——那几种只能由中转去连；
     * 3. 走中转时把地址换成**本机回环**（`relay.playlist` / `relay.resource`），请求头由中转加；
     * 4. 不走中转的（音画一体的直链）照旧由 souphttpsrc 直接取，代理和请求头由引擎设。
     */
    private fun MediaSource.toGstMedia(): GstMediaSource {
        val relay = relay
        val uris = ArrayList<String>()
        var relayed = false
        val dash = dash
        val first = videoUrls.firstOrNull()
        // 音轨给没给要和 dash.audio 对上（旧引擎的口径）：对不上说明这份 `dash` 不是这次要用的
        if (dash != null && first != null && (audioUrls.isNotEmpty() == (dash.audio != null))) {
            val mpd = DashManifest.build(first, audioUrls.firstOrNull(), dash, durationMs)
            if (relay != null) {
                // 自己拼的 MPD 里 BaseURL 是 CDN 地址：中转把它们换成本机地址（SegmentBase 的字节范围照旧）
                uris += relay.publishMpd(mpd, first)
            } else {
                // 没有中转（开发工具 / 测试）：清单仍要放在本机回环上，分片则直连 CDN
                val server = manifestServer ?: MpdServer().also { manifestServer = it }
                uris += server.publish(mpd)
            }
            // 没有中转时 `relayed` 保持 false：分片还是 demux 自己直连 CDN，请求头得由 source-setup 带
            relayed = relay != null
            return GstMediaSource(
                videoUrls = uris,
                audioUrls = emptyList(),
                headers = if (relayed) emptyList() else headers.toList(),
                codec = codec,
                durationMs = durationMs,
                proxyUri = gstProxyUri(proxy, uris.firstOrNull().orEmpty()),
                relayed = relayed,
            )
        }
        for (v in videoUrls) {
            if (relay != null && needsRelay(v, proxy)) {
                uris += if (isHlsUrl(v) || isDashManifestUrl(v)) relay.playlist(v) else relay.resource(v)
                relayed = true
            } else {
                uris += v
            }
        }
        // 音画分离（DASH 的双轨）没走中转时，音频地址单独交给另一个 uridecodebin3
        if (!relayed) audioUrls.forEach { uris += it }
        return GstMediaSource(
            // 走中转时地址已经是本机回环；音画分离（DASH）由中转拼的清单带两条轨，所以这里不再单列音频
            videoUrls = uris,
            audioUrls = if (relayed) emptyList() else audioUrls,
            headers = if (relayed) emptyList() else headers.toList(),
            codec = codec,
            durationMs = durationMs,
            // 本机回环显式直连（空串），远端直链按平台代理翻译；「直连 / SOCKS5」在上面已经改走中转
            proxyUri = gstProxyUri(proxy, uris.firstOrNull().orEmpty()),
            relayed = relayed,
        )
    }

    /**
     * 这个地址要不要走中转。
     *
     * 三种情况必须走：远端 m3u8 / .mpd（要现取现改写清单）、`ProxyMode.Direct`（用户要直连，
     * 而 souphttpsrc 表达不了"不用代理"——env 里的 `http_proxy` 会把它接走）、SOCKS5 和内置核心
     * （souphttpsrc 的 `proxy` 只吃 HTTP 代理）。
     */
    private fun needsRelay(url: String, proxy: ProxyMode): Boolean {
        if (!url.startsWith("http://", ignoreCase = true) && !url.startsWith("https://", ignoreCase = true)) {
            return false
        }
        return isHlsUrl(url) || isDashManifestUrl(url) || when (proxy) {
            ProxyMode.Direct -> true
            is ProxyMode.Custom -> proxy.type == ProxyType.SOCKS5
            // 内置核心是本机 SOCKS5：souphttpsrc 表达不了，整段 mp4 也交给中转
            is ProxyMode.BuiltinCore -> true
            else -> false
        }
    }

    /**
     * 这次播放用的中转（由 [open] 按 `MediaSource.relayFactory` 开，[release] / 下一次 open 关）。
     *
     * 引擎只拿"已经拼好的地址"；服务本体在 Rust（`feedhub.player.RustRelayServer`），这里是一个
     * `RelayHandle`——探针 / 测试想读中转的记账就看 [debugRelay] / [debugRelayStats]。
     */
    private var relay: RelayHandle? = null

    /**
     * **没有中转时**放 MPD 的本机小服务（`MpdServer`，只回内存里那份清单，一个上游请求都不发）。
     *
     * 只有开发工具 / 测试会走到（产品路径一定给了 `relayFactory`）；dashdemux2 必须从 `http:` 取清单
     * 才会把清单的请求头带到分片上，`file:` / `data:` 不行。和 [relay] 一起在 [closeRelay] 里关。
     */
    private var manifestServer: MpdServer? = null

    /**
     * 关掉上一次播放的中转。
     *
     * **必须关**：中转现在整个在 Rust 里（`RelayServer`：1 个 accept 线程 + 8 个处理线程 + 一个
     * 监听端口），`close()` 才会把它们和在飞的上游请求一起收掉。不关就是"开播一次漏一个服务"——
     * 切集 / 重开几十次之后机器上堆几十个空闲线程、端口也不释放。在每次 `open` 开头和 `release` 里各关一次（幂等）。
     */
    private fun closeRelay() {
        relay?.let { r -> runCatching { r.close() } }
        relay = null
        manifestServer?.let { s -> runCatching { s.close() } }
        manifestServer = null
    }
}

/**
 * 这台机器能不能用 GStreamer（Rust 引擎）。**会触发一次进程级初始化 + VA-API 探测**。
 *
 * 给 `PlayerEngineFactory` 的判断用：它要在建引擎之前就知道"要不要退回 VLC"。
 */
internal fun gstRuntimeProblem(): String? = runCatching { RustGstRuntime.problem() }
    .getOrElse { "Rust 核心加载失败：${it.message}" }
