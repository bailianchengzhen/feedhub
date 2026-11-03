package feedhub.app.video

import feedhub.core.AudioStream
import feedhub.core.FeedJson
import feedhub.core.PlayInfo
import feedhub.core.ProgressiveStream
import feedhub.core.ProxyMode
import feedhub.core.VideoCodec
import feedhub.core.VideoStream
import feedhub.player.CodecPreference
import feedhub.player.DecodeCapability
import feedhub.player.DefaultQuality
import feedhub.player.RustPlayer
import feedhub.player.StreamChoice as RustStreamChoice

/** 选出来的一路流：DASH（视频轨 + 音频轨）或音画一体的整段文件。 */
data class StreamChoice(
    val quality: Int?,
    val video: VideoStream? = null,
    val audio: AudioStream? = null,
    val progressive: ProgressiveStream? = null,
) {
    val codec: VideoCodec? get() = video?.codec ?: progressive?.codec

    /**
     * 交给播放器的形式；[headers] 用 [PlayInfo.headers]，[proxy] 用这个平台的代理（见 [MediaSource.proxy]），
     * [relayFactory] 造分片中转（按平台算代理，见 [MediaSource.relayFactory]）。
     */
    fun toMediaSource(
        headers: Map<String, String>,
        durationMs: Long? = null,
        proxy: ProxyMode = ProxyMode.System,
        relayFactory: RelayFactory? = null,
    ): MediaSource? = when {
        video != null -> MediaSource(
            videoUrls = listOf(video.url) + video.backupUrls,
            audioUrls = audio?.let { listOf(it.url) + it.backupUrls }.orEmpty(),
            headers = headers,
            bandwidthBps = (video.bandwidth ?: 0) + (audio?.bandwidth ?: 0),
            codec = video.codec,
            durationMs = durationMs,
            proxy = proxy,
            relayFactory = relayFactory,
            dash = video.segmentBase?.let { vsb ->
                DashInfo(
                    video = DashTrack(vsb, video.codecs, video.bandwidth, video.width, video.height, video.frameRate),
                    audio = audio?.segmentBase?.let { DashTrack(it, audio.codecs, audio.bandwidth) },
                )
            },
        )
        progressive != null -> MediaSource(
            videoUrls = listOf(progressive.url) + progressive.backupUrls,
            headers = headers,
            bandwidthBps = progressive.bandwidth,
            codec = progressive.codec,
            durationMs = durationMs,
            proxy = proxy,
            relayFactory = relayFactory,
        )
        else -> null
    }
}

/** 清晰度菜单里的一项：[enabled] 为 false（这次没给流，比如没登录 / 不是大会员）就灰掉。 */
data class QualityOption(val id: Int, val label: String, val enabled: Boolean)

/**
 * 选流：按设置里的编码偏好和默认清晰度，从 [PlayInfo] 里挑一路视频 + 一路音频。选不到的就退而求其次。
 *
 * 这里只剩两件事：
 * 1. 把 `PlayInfo` 序列化成 JSON 交给 Rust（选流只要其中几项，过 JSON 比过几十个 record 便宜）；
 * 2. 拿回来的三个**下标**还原成 [StreamChoice]（`video` / `audio` / `progressive` 指回自己的列表）。
 *
 * 规则一个字都不在这里：自动顺序、清晰度怎么挑、同档里挑哪条编码、音频挑哪条、HDR 为什么不算"最高"——
 * 全在 `rust/crates/feedhub-core/src/player/select.rs`，写法在那边写着出处。Kotlin 原版只在
 * `app/src/desktopTest/.../video/legacy/StreamSelection.kt` 当对拍参照。
 */
object StreamSelection {
    /** 没有硬解信息（或一个都不能硬解）时"自动"的编码顺序（真值在 Rust）。 */
    val DESKTOP_AUTO: List<VideoCodec> get() = RustPlayer.DESKTOP_AUTO

    /** "自动"的编码顺序：能硬解且这个负载下实测能实时的排前面（AV1 > HEVC > AVC），其余按软解顺序。 */
    fun autoOrder(capability: DecodeCapability, pixelsPerSecond: Long? = null): List<VideoCodec> =
        RustPlayer.autoOrder(capability, pixelsPerSecond)

    /** 只有"能硬解哪些"（还不知道负载表现）时的自动顺序。 */
    fun autoOrder(hardware: Set<VideoCodec>): List<VideoCodec> =
        RustPlayer.autoOrder(DecodeCapability.ofHardware(hardware))

    /** 控件里编码标签后面的"硬解 / 软解"。 */
    fun decodeLabel(hardware: Boolean?): String = RustPlayer.decodeLabel(hardware)

    /** 控件里那个小标签的文字，如 "AVC · 硬解"。 */
    fun chipLabel(codec: VideoCodec?, hardware: Boolean?): String = RustPlayer.chipLabel(codec, hardware)

    /** 编码优先级：用户指定了就先试它，再按 [auto] 的顺序退；最后是 OTHER。 */
    fun codecOrder(pref: CodecPreference, auto: List<VideoCodec> = DESKTOP_AUTO): List<VideoCodec> =
        RustPlayer.codecOrder(pref, auto)

    /** 默认清晰度想要哪一档（B站的清晰度 id）。 */
    fun targetQuality(pref: DefaultQuality, available: Collection<Int>): Int? =
        RustPlayer.targetQuality(pref, available)

    /** 在 [available] 里找最接近 [target] 的一档。 */
    fun pickQuality(available: Collection<Int>, target: Int?): Int? =
        RustPlayer.pickQuality(available, target)

    /**
     * @param quality 用户在菜单里手动选的清晰度（没选就 null，用 [defaultQuality]）。
     * @param capability 本机的解码能力（[PlayerEngine.decodeCapability]）。
     * @return 选不出（一路流都没有）就 null。
     */
    fun select(
        info: PlayInfo,
        codecPref: CodecPreference,
        defaultQuality: DefaultQuality,
        quality: Int? = null,
        auto: List<VideoCodec> = DESKTOP_AUTO,
        capability: DecodeCapability? = null,
    ): StreamChoice? {
        val choice = RustPlayer.select(
            infoJson = info.toRustJson(),
            preference = codecPref,
            defaultQuality = defaultQuality,
            quality = quality,
            auto = auto,
            capability = capability,
        ) ?: return null
        return StreamChoice(
            quality = choice.quality,
            video = choice.video?.let { info.video.getOrNull(it) },
            audio = choice.audio?.let { info.audio.getOrNull(it) },
            progressive = choice.progressive?.let { info.progressive.getOrNull(it) },
        )
    }

    /** 音频：AAC 里码率最高的（兼容最好）；没有 AAC 就取码率最高的。 */
    fun pickAudio(audio: List<AudioStream>): AudioStream? = RustPlayer.pickAudio(audio)

    /** 清晰度菜单：平台列出的全部档位（高的在前），这次有流的可选，没流的灰掉。 */
    fun qualityOptions(info: PlayInfo): List<QualityOption> =
        RustPlayer.qualityOptions(info.toRustJson()).map { QualityOption(it.id, it.label, it.enabled) }

    /** 编码在界面上的名字。 */
    fun codecLabel(c: VideoCodec?): String = RustPlayer.codecLabel(c)
}

/**
 * `PlayInfo` → Rust 选流要的那份 JSON。
 *
 * **只放 Rust 读的字段**（视频轨 / 音频轨 / 音画一体 / 清晰度列表）：`headers` 是有序的请求头，选流用不到，
 * 而且 `Map` 序列化会把它变成对象（顺序在 JSON 里能保住，但没必要搬）。用 `FeedJson` 的配置，
 * 字段名和 `PlayInfo` 的 `@Serializable` 声明一致，Rust 那边照 `durationMs` / `backupUrls` / `frameRate`
 * 这几个名字读（见 `feedhub-ffi/src/player.rs` 的 `parse_play_info`）。
 */
internal fun PlayInfo.toRustJson(): String = FeedJson.encodeToString(
    PlayInfo.serializer(),
    copy(headers = emptyMap()),
)

/** 倍速的档位和显示。 */
object PlaybackSpeed {
    /** 可选档位（真值在 Rust）。 */
    val STEPS: List<Float> get() = RustPlayer.speedSteps()

    const val DEFAULT = 1.0f

    /** 按钮 / 菜单上的字："1.0x"、"0.75x"。 */
    fun label(speed: Float): String = RustPlayer.speedLabel(speed)

    /** 往快 / 慢走一档（[delta] 为 +1 / -1），到头不动。 */
    fun step(current: Float, delta: Int): Float = RustPlayer.speedStep(current, delta)
}
