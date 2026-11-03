package feedhub.player

import uniffi.feedhub_gst.FfiGstAudioSink
import uniffi.feedhub_gst.FfiGstAudioStamp
import uniffi.feedhub_gst.FfiGstCodec
import uniffi.feedhub_gst.FfiGstConfig
import uniffi.feedhub_gst.FfiGstEngine
import uniffi.feedhub_gst.FfiGstFrameHeader
import uniffi.feedhub_gst.FfiGstFrameSink
import uniffi.feedhub_gst.FfiGstFrameStamp
import uniffi.feedhub_gst.FfiGstHeader
import uniffi.feedhub_gst.FfiGstLedgerSink
import uniffi.feedhub_gst.FfiGstMedia
import uniffi.feedhub_gst.FfiGstSnapshot
import uniffi.feedhub_gst.FfiGstSurface

/**
 * **桌面播放引擎**（Rust `FfiGstEngine` 的 Kotlin 面，替代 Kotlin 的 `GstPlayerEngine`）。
 *
 * 引擎本体全在独立 crate `feedhub-gst`（`libfeedhub_gst.so`：建管线 / 总线 /
 * 状态机 / seek / 倍速 / 循环 / 缓冲 / 硬解退软解 / 备用地址 / 帧节奏记账 / souphttpsrc 的
 * 代理与请求头）。这一层只做四件事：
 *
 * 1. 转发控制调用（`play` / `pause` / `seek_to` / `set_rate` …）；
 * 2. 把 [pollSnapshot] 的快照字段搬进 `PlayerState`（宿主只赋值，不判断）；
 * 3. 把共享帧内存的地址交出去（[surface]），界面那一侧自己读 BGRA 造 Skia 的 `Image`；
 * 4. 定时器：每帧问一次 [frameSeq]。
 */
class RustGstPlayerEngine constructor(
    /** 引擎配置。 */
    config: FfiGstConfig = FfiGstConfigBuilder.build(),
) : AutoCloseable {
    /** Rust 那边的引擎句柄。**构造它就会触发 GStreamer 的进程级初始化 + VA-API 探测**。 */
    val engine: FfiGstEngine = RustGstNative.native { FfiGstEngine(config) }

    /** 这台机器没法播放的原因（`null` = 能用）。 */
    val unavailable: String? = RustGstNative.native { engine.unavailable() }

    /** 现在能硬解的编码（硬解关着时是空）。 */
    fun hardwareCodecs(): Set<feedhub.core.VideoCodec> = RustGstNative.native {
        engine.hardwareCodecs().map { it.toCoreCodec() }.toSet()
    }

    // ---------------- 控制 ----------------

    fun open(source: GstMediaSource, startMs: Long, autoPlay: Boolean) = RustGstNative.native {
        engine.open(source.toFfi(), startMs, autoPlay)
    }

    fun play() = RustGstNative.native { engine.play() }
    fun pause() = RustGstNative.native { engine.pause() }
    fun seekTo(positionMs: Long) = RustGstNative.native { engine.seekTo(positionMs) }
    fun setVolume(percent: Int) = RustGstNative.native { engine.setVolume(percent) }
    fun setMuted(muted: Boolean) = RustGstNative.native { engine.setMuted(muted) }
    fun setLooping(looping: Boolean) = RustGstNative.native { engine.setLooping(looping) }
    fun setRate(rate: Float) = RustGstNative.native { engine.setRate(rate) }

    /** 改视频 appsink 的 `max-buffers`（检查工具用；下一次 `open` 生效）。 */
    fun setSinkMaxBuffers(n: Int) = RustGstNative.native { engine.setSinkMaxBuffers(n.toUInt()) }

    /** 画布变成了 `w×h` 像素（界面里是 Canvas 的尺寸回调）。 */
    fun setSurfaceSize(w: Int, h: Int) = RustGstNative.native { engine.setSurfaceSize(w, h) }

    // ---------------- 状态 ----------------

    /** 当前快照。 */
    fun snapshot(): FfiGstSnapshot = RustGstNative.native { engine.snapshot() }

    /** 变了才返回（`null` = 没变，宿主要不要写 Compose 状态由它决定）。 */
    fun pollSnapshot(): FfiGstSnapshot? = RustGstNative.native { engine.pollSnapshot() }

    fun stats() = RustGstNative.native { engine.stats() }

    /** 接上解码账本。 */
    fun attachLedger(ledger: RustDecodeLedger) = RustGstNative.native {
        engine.attachLedger(object : FfiGstLedgerSink {
            override fun `recordDecode`(
                `codec`: FfiGstCodec,
                `pixelsPerSecond`: Long,
                `smooth`: Boolean,
                `nowMs`: Long,
            ) {
                ledger.record(codec.toCoreCodec(), pixelsPerSecond, smooth, nowMs)
            }
        })
    }

    // ---------------- 出帧 ----------------

    /** 共享帧内存的描述；`open` 之前是 `null`。 */
    fun surface(): FfiGstSurface? = RustGstNative.native { engine.surface() }

    /** 最新一帧的序号（每帧读一次这个标量决定要不要重画）。 */
    fun frameSeq(): Long = RustGstNative.native { engine.frameSeq().toLong() }

    /**
     * **读者占住当前这一帧**（零拷贝读帧的第一步）：拿到槽号 / 尺寸 / 行距，
     * 读完必须调 [releaseFrameSlot] 放开它。还没有帧就 null。
     */
    fun acquireFrame(): FfiGstFrameHeader? = RustGstNative.native { engine.acquireFrame() }

    /** 读完这一帧，放开槽（写者就可以覆写它了）。 */
    fun releaseFrameSlot() = RustGstNative.native { engine.releaseFrameSlot() }

    /** `(已写帧数, 因三槽占满跳过的帧数)`。 */
    fun frameCounters(): Pair<Long, Long> = RustGstNative.native {
        val c = engine.frameCounters()
        c.written.toLong() to c.dropped.toLong()
    }

    /** 把当前这一帧编成 PNG（验证 / 调试用）。 */
    fun frameAsPng(): ByteArray? = RustGstNative.native { engine.frameAsPng() }

    // ---------------- 测量 / 验证 ----------------

    /**
     * 装帧回调。**只在测量时装**：每帧过一次 JNA 边界，生产路径一个回调都不挂。
     */
    fun setFrameHook(hook: ((ptsNs: Long, runningNs: Long, luma: Int, width: Int, height: Int) -> Unit)?) =
        RustGstNative.native {
            engine.setFrameSink(hook?.let { h ->
                object : FfiGstFrameSink {
                    override fun `onFrame`(`stamp`: FfiGstFrameStamp) {
                        h(stamp.`ptsNs`, stamp.`runningNs`, stamp.`luma`, stamp.`width`, stamp.`height`)
                    }
                }
            })
        }

    /**
     * 装音频回调（要在 `open` 之前设；会顺带把音频 sink 换成不出声的 appsink）。
     */
    fun setAudioHook(hook: ((ptsNs: Long, runningNs: Long, data: ByteArray, rate: Int, channels: Int) -> Unit)?) =
        RustGstNative.native {
            engine.setAudioSink(hook?.let { h ->
                object : FfiGstAudioSink {
                    override fun `onAudio`(`stamp`: FfiGstAudioStamp) {
                        h(stamp.`ptsNs`, stamp.`runningNs`, stamp.`data`, stamp.`rate`, stamp.`channels`)
                    }
                }
            })
        }

    fun debugElements(): List<String> = RustGstNative.native { engine.debugElements() }
    fun debugAudioCaps(): String? = RustGstNative.native { engine.debugAudioCaps() }
    fun debugVideoCaps(): String? = RustGstNative.native { engine.debugVideoCaps() }
    fun debugVaDevices(): List<String> = RustGstNative.native { engine.debugVaDevices() }
    fun debugPipelineName(): String? = RustGstNative.native { engine.debugPipelineName() }

    /** 开 / 关 `uridecodebin3` 的 `use-buffering`（下一次 `open` 生效）。 */
    fun setUseBuffering(on: Boolean) = RustGstNative.native { engine.setUseBuffering(on) }

    /** 开 / 关即时变速（false = 换倍速强制走 flush seek 回退；验证回退路径用）。 */
    fun setAllowInstantRate(allowed: Boolean) = RustGstNative.native { engine.setAllowInstantRate(allowed) }

    /** 给管线里的 `dashdemux2` 设一个属性（检查工具压水位用）；元素还没建出来时 false。 */
    fun setDemuxProperty(name: String, value: String): Boolean = RustGstNative.native {
        engine.setDemuxProperty(name, value)
    }

    /** 每条 BUFFERING 报上来的百分比：`元素名=百分比`（查"卡在谁身上"用）。 */
    fun debugBufferPct(): List<String> = RustGstNative.native { engine.debugBufferPct() }

    /** 当前管线的时钟名（`"GstSystemClock"` = 系统时钟；声卡 sink 的名字 = 被声卡抢走了）。 */
    fun debugClockName(): String? = RustGstNative.native { engine.debugClockName() }

    /** 音频 / 视频 sink 各自此刻渲染到的媒体位置（毫秒）。 */
    fun debugSinkPositionsMs(): Pair<Long, Long>? = RustGstNative.native {
        engine.debugSinkPositionsMs()?.let { it.`audioMs` to it.`videoMs` }
    }

    /** 停止并释放。 */
    fun release() = RustGstNative.native { engine.release() }

    override fun close() = release()
}

/**
 * 一次播放的入参：Kotlin 的 `MediaSource` 摊平（**只有引擎要的那几个字段**）。
 *
 * 引擎只管"拿到的地址"（回环 URL 或直链），
 */
class GstMediaSource(
    val videoUrls: List<String>,
    val audioUrls: List<String> = emptyList(),
    val headers: List<Pair<String, String>> = emptyList(),
    val codec: feedhub.core.VideoCodec? = null,
    val durationMs: Long? = null,
    val proxyUri: String? = null,
    val relayed: Boolean = false,
) {
    internal fun toFfi(): FfiGstMedia = FfiGstMedia(
        `videoUrls` = videoUrls,
        `audioUrls` = audioUrls,
        `headers` = headers.map { FfiGstHeader(`name` = it.first, `value` = it.second) },
        `codec` = codec?.toFfiCodec(),
        `durationMs` = durationMs,
        `proxyUri` = proxyUri,
        `relayed` = relayed,
    )
}

/** 造 [uniffi.feedhub_gst.FfiGstConfig] 的便利函数（产品路径有默认值，测量路径改几项）。 */
object FfiGstConfigBuilder {
    fun build(
        hardware: Boolean = true,
        debugSync: Boolean = false,
        alignWidth: Boolean = true,
        sinkMaxBuffers: Int = 2,
        useSystemClock: Boolean = true,
        useBuffering: Boolean = true,
        allowInstantRate: Boolean = true,
        silent: Boolean = false,
        debugAudio: Boolean = false,
        rawSink: Boolean = false,
        rawSinkScaled: Boolean = false,
    ): FfiGstConfig = FfiGstConfig(
        `hardware` = hardware,
        `debugSync` = debugSync,
        `alignWidth` = alignWidth,
        `sinkMaxBuffers` = sinkMaxBuffers.toUInt(),
        `useSystemClock` = useSystemClock,
        `useBuffering` = useBuffering,
        `allowInstantRate` = allowInstantRate,
        `silent` = silent,
        `debugAudio` = debugAudio,
        `rawSink` = rawSink,
        `rawSinkScaled` = rawSinkScaled,
    )
}

/** 帧内存的布局（`surface()` 给的那几个数 + 常量）。宿主解析共享内存时照这个来。 */
object GstSurfaceLayout {
    /** 每个槽头部的字节数。 */
    val slotHeaderBytes: Int = RustGstNative.native { uniffi.feedhub_gst.gstSlotHeaderBytes().toInt() }

    /** 共享内存的槽数。 */
    val slots: Int = RustGstNative.native { uniffi.feedhub_gst.gstSurfaceSlots().toInt() }

    /** 槽头里 `format` 字段的取值：BGRA8888。 */
    val formatBgra: Int = RustGstNative.native { uniffi.feedhub_gst.gstFormatBgra().toInt() }

    /** 槽头里的字段偏移（小端）：宽度 / 高度 / 行距 / 格式 / PTS。 */
    const val OFFSET_WIDTH = 0
    const val OFFSET_HEIGHT = 4
    const val OFFSET_STRIDE = 8
    const val OFFSET_FORMAT = 12
    const val OFFSET_PTS = 16
}

// ---------------- 编码映射 ----------------

internal fun FfiGstCodec?.toCoreCodec(): feedhub.core.VideoCodec =
    when (this) {
        FfiGstCodec.AVC -> feedhub.core.VideoCodec.AVC
        FfiGstCodec.HEVC -> feedhub.core.VideoCodec.HEVC
        FfiGstCodec.AV1 -> feedhub.core.VideoCodec.AV1
        FfiGstCodec.OTHER, null -> feedhub.core.VideoCodec.OTHER
    }

internal fun feedhub.core.VideoCodec.toFfiCodec(): FfiGstCodec =
    when (this) {
        feedhub.core.VideoCodec.AVC -> FfiGstCodec.AVC
        feedhub.core.VideoCodec.HEVC -> FfiGstCodec.HEVC
        feedhub.core.VideoCodec.AV1 -> FfiGstCodec.AV1
        feedhub.core.VideoCodec.OTHER -> FfiGstCodec.OTHER
    }

/** 一块 VA-API 设备的探测结果（`dev/VaProbe` 打印用）。 */
class RustVaDevice(
    val path: String,
    val vendor: String?,
    val driver: String,
    val vaVersion: String,
    val profiles: List<Pair<Int, List<Int>>>,
    val available: Boolean,
) {
    fun supportsDecode(codec: feedhub.core.VideoCodec): Boolean {
        val want: List<Int> = when (codec) {
            feedhub.core.VideoCodec.AVC -> listOf(7, 6, 13)
            feedhub.core.VideoCodec.HEVC -> listOf(17, 18)
            feedhub.core.VideoCodec.AV1 -> listOf(32, 33)
            feedhub.core.VideoCodec.OTHER -> emptyList()
        }
        if (want.isEmpty()) return false
        return profiles.any { (id, eps) -> id in want && 1 in eps }
    }
}

/**
 * **VA-API 体检**（`dev/VaProbe` 用）：扫 `/dev/dri/renderD*`、问每块卡的能力。
 */
object RustVaProbe {
    fun scanAll(): List<RustVaDevice> = RustGstNative.native {
        uniffi.feedhub_gst.vaScanAll().map { d ->
            RustVaDevice(
                path = d.`path`,
                vendor = d.`vendor`,
                driver = d.`driver`,
                vaVersion = d.`vaVersion`,
                profiles = d.`profiles`.map { it.`id` to it.`entrypoints` },
                available = true,
            )
        }.let { found ->
            val seen = found.map { it.path }.toSet()
            val all = java.io.File("/dev/dri").listFiles().orEmpty()
                .map { it.path }
                .filter { Regex("renderD\\d+$").containsMatchIn(it) }
                .sorted()
            found + all.filter { it !in seen }.map {
                RustVaDevice(it, null, "", "", emptyList(), available = false)
            }
        }
    }

    fun query(path: String): RustVaDevice = RustGstNative.native {
        uniffi.feedhub_gst.vaQuery(path)?.let { d ->
            RustVaDevice(
                path = d.`path`,
                vendor = d.`vendor`,
                driver = d.`driver`,
                vaVersion = d.`vaVersion`,
                profiles = d.`profiles`.map { it.`id` to it.`entrypoints` },
                available = true,
            )
        } ?: RustVaDevice(path, null, "", "", emptyList(), available = false)
    }

    fun available(): Boolean = RustGstNative.native { uniffi.feedhub_gst.vaAvailable() }

    fun vaElements(): List<String> = RustGstNative.native {
        val e = FfiGstEngine(FfiGstConfigBuilder.build(hardware = false))
        try {
            e.debugVaDevices()
        } finally {
            e.release()
        }
    }
}

/**
 * **GStreamer 运行时的探测**（`GstRuntime` 的替身）。
 */
object RustGstRuntime {
    fun problem(): String? = RustGstNative.native { uniffi.feedhub_gst.gstProblem() }

    fun version(): String? = RustGstNative.native { uniffi.feedhub_gst.gstVersion() }

    fun vaDevicePath(): String? = RustGstNative.native { uniffi.feedhub_gst.gstVaDevicePath() }
    fun vaVendor(): String? = RustGstNative.native { uniffi.feedhub_gst.gstVaVendor() }
    fun vaDriver(): String? = RustGstNative.native { uniffi.feedhub_gst.gstVaDriver() }
    fun vaResolvedDevice(): String? = RustGstNative.native { uniffi.feedhub_gst.gstVaResolvedDevice() }

    fun hardwareCodecs(): Set<feedhub.core.VideoCodec> = RustGstNative.native {
        uniffi.feedhub_gst.gstHardwareCodecs().map { it.toCoreCodec() }.toSet()
    }

    fun rejectedCodecs(): Map<feedhub.core.VideoCodec, String> = RustGstNative.native {
        uniffi.feedhub_gst.gstRejectedCodecs().associate { it.`codec`.toCoreCodec() to it.`reason` }
    }

    fun instantRateChange(): Boolean = RustGstNative.native { uniffi.feedhub_gst.gstInstantRateChange() }
    fun hasVaPostProc(): Boolean = RustGstNative.native { uniffi.feedhub_gst.gstHasVaPostproc() }

    fun vaDevices(): List<String> = RustGstNative.native {
        val e = FfiGstEngine(FfiGstConfigBuilder.build(hardware = false))
        try {
            e.debugVaDevices()
        } finally {
            e.release()
        }
    }

    fun probeUrl(url: String, proxy: String?, timeoutSeconds: Int = 5): Boolean = RustGstNative.native {
        uniffi.feedhub_gst.gstProbeUrl(url, proxy, timeoutSeconds.toUInt())
    }

    fun audioSinkName(): String? = RustGstNative.native { uniffi.feedhub_gst.gstAudioSinkName() }
    fun setHardware(enabled: Boolean) = RustGstNative.native { uniffi.feedhub_gst.gstSetHardware(enabled) }

    fun vendorOf(devicePath: String): String? = RustGstNative.native {
        uniffi.feedhub_gst.gstVendorOf(devicePath)
    }

    fun pciIdOf(devicePath: String): String? = RustGstNative.native {
        uniffi.feedhub_gst.gstPciIdOf(devicePath)
    }

    fun vendorName(vendor: String?): String = RustGstNative.native {
        uniffi.feedhub_gst.gstVendorName(vendor)
    }

    fun nvidiaVendor(): String = RustGstNative.native { uniffi.feedhub_gst.gstNvidiaVendor() }

    fun ensureLoopbackNotProxied() = RustGstNative.native {
        uniffi.feedhub_gst.gstEnsureLoopbackNotProxied()
    }

    fun decodeEnvironmentFingerprint(): String? = RustGstNative.native {
        uniffi.feedhub_gst.gstDecodeEnvironmentFingerprint()
    }
}
