package feedhub.player

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiCodecPreference
import uniffi.feedhub_ffi.FfiDashTrack
import uniffi.feedhub_ffi.FfiDecodeCapability
import uniffi.feedhub_ffi.FfiDefaultQuality
import uniffi.feedhub_ffi.FfiPersist
import uniffi.feedhub_ffi.FfiPixelRateEntry
import uniffi.feedhub_ffi.FfiPlayCodec
import uniffi.feedhub_ffi.FfiRelayRoute
import uniffi.feedhub_ffi.FfiAudioForPick
import uniffi.feedhub_ffi.playerAutoOrder
import uniffi.feedhub_ffi.playerResolveUrl
import uniffi.feedhub_ffi.playerRewriteDash
import uniffi.feedhub_ffi.playerRewriteHls
import uniffi.feedhub_ffi.playerPickAudio
import uniffi.feedhub_ffi.playerPickQuality
import uniffi.feedhub_ffi.playerTargetQuality
import uniffi.feedhub_ffi.playerClampRate
import uniffi.feedhub_ffi.playerCodecLabel
import uniffi.feedhub_ffi.playerCodecOrder
import uniffi.feedhub_ffi.playerChipLabel
import uniffi.feedhub_ffi.playerDashManifest
import uniffi.feedhub_ffi.playerDecodeLabel
import uniffi.feedhub_ffi.playerDurationOf
import uniffi.feedhub_ffi.playerExtOf
import uniffi.feedhub_ffi.playerFrameRate
import uniffi.feedhub_ffi.playerInitialPartIndex
import uniffi.feedhub_ffi.playerIsDashUrl
import uniffi.feedhub_ffi.playerIsLoopbackUrl
import uniffi.feedhub_ffi.playerLimitQuality
import uniffi.feedhub_ffi.playerNoPartText
import uniffi.feedhub_ffi.playerNoStreamText
import uniffi.feedhub_ffi.playerPixelRate
import uniffi.feedhub_ffi.playerPixelRateLabel
import uniffi.feedhub_ffi.playerQualityOptions
import uniffi.feedhub_ffi.playerSelect
import uniffi.feedhub_ffi.playerSliceFor
import uniffi.feedhub_ffi.playerSpeedLabel
import uniffi.feedhub_ffi.playerSpeedStep
import uniffi.feedhub_ffi.playerSpeedSteps
import uniffi.feedhub_ffi.playerStartPosition

/**
 * **播放器决策层**的 Rust 门面（`rust/crates/feedhub-core/src/player/`）。
 *
 * 有跨调用状态的几件（账本 / 时间线 / 排布器 / 装载策略 /
 * 播放状态机 / 中转映射）在 [RustDecodeLedger] / [RustDanmakuTimeline] / [RustDanmakuScheduler] /
 * [RustDanmakuLoader] / [RustPlaybackSession] / [RustRelay]。
 *
 * 和别的 `feedhub.*` 门面一样：**没有开关**、**没有 Kotlin 备用实现**，
 * `.so` 加载不起来就抛一句说得清的话，不悄悄换成另一种行为。
 */
object RustPlayer {
    // ---------- 选流 ----------

    /**
     * 选一路流。`infoJson` 是 `PlayInfo` 的 JSON：选流只要其中几项，过一份 JSON 比过几十个 record 便宜。
     *
     * @param auto 没有 `capability` 时用的编码顺序（安卓是查 MediaCodec 得到的"硬解优先"）；
     * @param capability 本机的解码能力：给了它就按**这一档清晰度的实际负载**算自动顺序。
     */
    fun select(
        infoJson: String,
        preference: CodecPreference,
        defaultQuality: DefaultQuality,
        quality: Int? = null,
        auto: List<feedhub.core.VideoCodec> = emptyList(),
        capability: DecodeCapability? = null,
    ): StreamChoice? = native {
        playerSelect(
            infoJson,
            preference.toFfi(),
            defaultQuality.toFfi(),
            quality,
            auto.map { it.toFfi() },
            capability?.toFfi(),
        )?.let { c ->
            StreamChoice(
                quality = c.quality,
                video = c.video?.toInt(),
                audio = c.audio?.toInt(),
                progressive = c.progressive?.toInt(),
            )
        }
    }

    /** 自动的编码顺序（`StreamSelection.autoOrder`）。 */
    fun autoOrder(capability: DecodeCapability, pixelsPerSecond: Long? = null): List<feedhub.core.VideoCodec> =
        native { playerAutoOrder(capability.toFfi(), pixelsPerSecond?.toULong()).map { it.toCore() } }

    /** 编码优先级（`StreamSelection.codecOrder`）。 */
    fun codecOrder(
        preference: CodecPreference,
        auto: List<feedhub.core.VideoCodec> = DESKTOP_AUTO,
    ): List<feedhub.core.VideoCodec> = native {
        playerCodecOrder(preference.toFfi(), auto.map { it.toFfi() }).map { it.toCore() }
    }

    /** 默认清晰度想要哪一档（`StreamSelection.targetQuality`）。 */
    fun targetQuality(pref: DefaultQuality, available: Collection<Int>): Int? =
        native { playerTargetQuality(pref.toFfi(), available.toList()) }

    /** 在 `available` 里找最接近 `target` 的一档（`StreamSelection.pickQuality`）。 */
    fun pickQuality(available: Collection<Int>, target: Int?): Int? =
        native { playerPickQuality(available.toList(), target) }

    /** 挑一条音频轨（AAC 里码率最高的；没有 AAC 就码率最高的）。 */
    fun pickAudio(audio: List<feedhub.core.AudioStream>): feedhub.core.AudioStream? = native {
        playerPickAudio(
            audio.map { FfiAudioForPick(it.id, it.codecs, it.bandwidth) },
        )?.let { audio.getOrNull(it.toInt()) }
    }

    /** 清晰度菜单（`StreamSelection.qualityOptions`）。 */
    fun qualityOptions(infoJson: String): List<QualityOption> = native {
        playerQualityOptions(infoJson).map { QualityOption(it.id, it.label, it.enabled) }
    }

    /** `AVC` / `HEVC` / `AV1` / 空。 */
    fun codecLabel(codec: feedhub.core.VideoCodec?): String = native { playerCodecLabel(codec?.toFfi()) }

    /** `硬解` / `软解` / 空。 */
    fun decodeLabel(hardware: Boolean?): String = native { playerDecodeLabel(hardware) }

    /** `AVC · 硬解` 这种。 */
    fun chipLabel(codec: feedhub.core.VideoCodec?, hardware: Boolean?): String =
        native { playerChipLabel(codec?.toFfi(), hardware) }

    // ---------- 倍速 ----------

    fun speedSteps(): List<Float> = native { playerSpeedSteps() }

    /** `1.5x` 这种。 */
    fun speedLabel(speed: Float): String = native { playerSpeedLabel(speed) }

    /** 往快 / 慢走一档。 */
    fun speedStep(current: Float, delta: Int): Float = native { playerSpeedStep(current, delta) }

    /** 默认清晰度被封顶之后是哪一档（`limitQuality`）。 */
    fun limitQuality(setting: DefaultQuality, limit: DefaultQuality?): DefaultQuality =
        native { playerLimitQuality(setting.toFfi(), limit?.toFfi()).toCore() }

    /** 倍速夹到引擎认的范围（0.25~4，`NaN` → 1）。 */
    fun clampRate(rate: Float): Float = native { playerClampRate(rate) }

    // ---------- 解码能力 ----------

    /** 帧率文本（`30`、`60/1`、`30000/1001`）→ 每秒帧数。 */
    fun frameRateOf(text: String?): Double? = native { playerFrameRate(text) }

    /** 一路视频轨的负载（每秒像素数）；缺宽 / 高 / 帧率就 null。 */
    fun pixelRateOf(width: Int?, height: Int?, frameRate: String?): Long? =
        native { playerPixelRate(width, height, frameRate)?.toLong() }

    /** 像素率上限 → `1080P60` 这种标签。 */
    fun pixelRateLabel(rate: Long): String = native { playerPixelRateLabel(rate.toULong()) }

    // ---------- 清单 ----------

    /** 拼一份只含 `SegmentBase` 的静态 MPD（`DashManifest.build`）。 */
    fun dashManifest(
        videoUrl: String,
        audioUrl: String?,
        video: DashTrack,
        audio: DashTrack?,
        durationMs: Long?,
    ): String = native {
        playerDashManifest(videoUrl, audioUrl, video.toFfi(), audio?.toFfi(), durationMs)
    }

    /** 改写一份 DASH 清单（远端地址登记进 [RustRelay]，值换成本机地址）。 */
    fun rewriteDash(mpd: String, baseUrl: String?, relay: RustRelay): String =
        native { playerRewriteDash(mpd, baseUrl, relay.inner) }

    /** 改写一份 HLS 播放列表。 */
    fun rewriteHls(playlistUrl: String, text: String, relay: RustRelay): String =
        native { playerRewriteHls(playlistUrl, text, relay.inner) }

    /** `URI.resolve` 的口径（和改写用的是同一份实现）。 */
    fun resolveUrl(base: String, ref: String): String = native { playerResolveUrl(base, ref) }

    /** `bytes=a-b` 解析成闭区间（null = 当整段处理）。 */
    fun sliceFor(range: String?, total: Int): IntRange? = native {
        playerSliceFor(range, total.toUInt())?.let { IntRange(it.from.toInt(), it.to.toInt()) }
    }

    /** 远端地址的扩展名（`.m4s` 这类，认不出就是空串）。 */
    fun extOf(url: String): String = native { playerExtOf(url) }

    fun isDashUrl(url: String): Boolean = native { playerIsDashUrl(url) }

    /** [url] 的主机是不是本机回环（中转出来的清单不该被代理）。 */
    fun isLoopbackUrl(url: String): Boolean = native { playerIsLoopbackUrl(url) }

    // ---------- 状态机（纯函数那几个） ----------

    /** 续播位置：离结尾不到 5 秒就从 0 开始。 */
    fun startPosition(resume: Boolean, savedMs: Long?, startMs: Long, durationMs: Long): Long =
        native { playerStartPosition(resume, savedMs, startMs, durationMs) }

    /** 首次打开该播第几 P。 */
    fun initialPartIndex(parts: List<String>, saved: String?): Int =
        native { playerInitialPartIndex(parts, saved).toInt() }

    /** 时长：播放地址给的优先，没有就用分 P 自带的。 */
    fun durationOf(infoJson: String, partDurationSec: Int?): Long =
        native { playerDurationOf(infoJson, partDurationSec) }

    fun noStreamText(): String = native { playerNoStreamText() }

    fun noPartText(): String = native { playerNoPartText() }

    // ---------- 过河 ----------

    /**
     * 走 Rust：`.so` 加载失败**不悄悄退回 Kotlin 实现**，给一句说得清的话（和 `feedhub.brain.Brain` 一样）。
     * 这一层是纯函数（没有宿主回调、没有平台异常），失败只剩"`.so` 加载不起来"这一种。
     */
    internal inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("播放器决策：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
    }

    // ---------- 类型映射（只有这一份） ----------

    internal fun feedhub.core.VideoCodec.toFfi(): FfiPlayCodec = when (this) {
        feedhub.core.VideoCodec.AVC -> FfiPlayCodec.AVC
        feedhub.core.VideoCodec.HEVC -> FfiPlayCodec.HEVC
        feedhub.core.VideoCodec.AV1 -> FfiPlayCodec.AV1
        feedhub.core.VideoCodec.OTHER -> FfiPlayCodec.OTHER
    }

    internal fun FfiPlayCodec.toCore(): feedhub.core.VideoCodec = when (this) {
        FfiPlayCodec.AVC -> feedhub.core.VideoCodec.AVC
        FfiPlayCodec.HEVC -> feedhub.core.VideoCodec.HEVC
        FfiPlayCodec.AV1 -> feedhub.core.VideoCodec.AV1
        FfiPlayCodec.OTHER -> feedhub.core.VideoCodec.OTHER
    }

    internal fun CodecPreference.toFfi(): FfiCodecPreference = when (this) {
        CodecPreference.AUTO -> FfiCodecPreference.AUTO
        CodecPreference.AVC -> FfiCodecPreference.AVC
        CodecPreference.HEVC -> FfiCodecPreference.HEVC
        CodecPreference.AV1 -> FfiCodecPreference.AV1
    }

    internal fun DefaultQuality.toFfi(): FfiDefaultQuality = when (this) {
        DefaultQuality.HIGHEST -> FfiDefaultQuality.HIGHEST
        DefaultQuality.P1080 -> FfiDefaultQuality.P1080
        DefaultQuality.P720 -> FfiDefaultQuality.P720
    }

    internal fun FfiDefaultQuality.toCore(): DefaultQuality = when (this) {
        FfiDefaultQuality.HIGHEST -> DefaultQuality.HIGHEST
        FfiDefaultQuality.P1080 -> DefaultQuality.P1080
        FfiDefaultQuality.P720 -> DefaultQuality.P720
    }

    internal fun DecodeCapability.toFfi(): FfiDecodeCapability = FfiDecodeCapability(
        hardware = hardware.map { it.toFfi() },
        smoothPixelRate = smoothPixelRate.map { (codec, rate) ->
            FfiPixelRateEntry(codec.toFfi(), rate.toULong())
        },
    )

    internal fun FfiDecodeCapability.toCore(): DecodeCapability = DecodeCapability(
        hardware = hardware.map { it.toCore() }.toSet(),
        smoothPixelRate = smoothPixelRate.associate {
            it.codec.toCore() to it.pixelsPerSecond.toLong()
        },
    )

    internal fun DashTrack.toFfi(): FfiDashTrack = FfiDashTrack(
        initialization = initialization,
        indexRange = indexRange,
        codecs = codecs,
        bandwidth = bandwidth,
        width = width,
        height = height,
        frameRate = frameRate,
    )

    /** 落盘动作（账本）。 */
    internal fun persistFromFfi(p: FfiPersist): LedgerPersist = when (p) {
        FfiPersist.Delete -> LedgerPersist.Delete
        is FfiPersist.Write -> LedgerPersist.Write(p.text)
    }

    /** 桌面 / 安卓"没有硬解信息"时的自动顺序。 */
    val DESKTOP_AUTO: List<feedhub.core.VideoCodec> by lazy {
        native { playerAutoOrder(FfiDecodeCapability(emptyList(), emptyList()), null).map { it.toCore() } }
    }
}

/** 中转路由的映射（只有这一份）。 */
internal fun FfiRelayRoute.toCore(): RelayRoute = when (this) {
    FfiRelayRoute.NotFound -> RelayRoute.NotFound
    FfiRelayRoute.BadTemplate -> RelayRoute.BadTemplate
    is FfiRelayRoute.Body -> RelayRoute.Body(body, contentType)
    is FfiRelayRoute.RemoteManifest -> RelayRoute.RemoteManifest(url, dash)
    is FfiRelayRoute.Upstream -> RelayRoute.Upstream(url)
}
