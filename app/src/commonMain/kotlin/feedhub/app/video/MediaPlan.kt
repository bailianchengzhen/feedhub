package feedhub.app.video

import feedhub.core.VideoCodec
import feedhub.present.RustPresent

/**
 * "这次开哪个流、用什么形式开"的纯描述（不依赖任何平台）。安卓的 Media3 引擎按它建 MediaSource；
 * 桌面不用（GStreamer 自己按 [MediaSource] 拼管线）。
 *
 * 抽到 commonMain 是因为**安卓那边建不出可单测的 ExoPlayer**：这里只有纯逻辑（选形式、拼 MPD），
 * `MediaPlanTest` 在桌面 JVM 里离线跑。
 *
 * MPD 也由
 * `player::manifest::dash_manifest` 拼），这里只剩把 `MediaSource` 的字段搬过去、把结果搬回来。
 */
data class MediaPlan(
    val kind: Kind,
    /** [Kind.DASH]：我们自己拼的 MPD 正文（[DashManifest]），播放器 sideload 这份清单（不用再去请求它）。 */
    val mpd: String = "",
    /** 主地址在前、备用地址在后。 */
    val videoUrls: List<String> = emptyList(),
    val audioUrls: List<String> = emptyList(),
    val durationMs: Long? = null,
    val codec: VideoCodec? = null,
    val bandwidthBps: Long? = null,
) {
    /** 流的形态，对应安卓侧的哪种 MediaSource。 */
    enum class Kind {
        /**
         * 视频轨 + 音频轨，各自是**带 sidx 的单文件分片**（B站的 DASH、Reddit 的 `DASHPlaylist.mpd`）：
         * 用现有 [DashManifest] 拼一份静态 MPD 交给 `DashMediaSource`。
         * 为什么不用两个 ProgressiveMediaSource 合（[MERGED]）：分片 mp4 靠 sidx 才能精确跳转，
         * 而且 MP4 初始化段 / 索引范围是平台直接给的（`MediaSource.dash`），照抄进 MPD 最省事、和桌面一致。
         */
        DASH,

        /** 两条独立的轨但**没有** SegmentBase 信息：两个"整段文件"source 合成一个（`MergingMediaSource`）。 */
        MERGED,

        /** 音画一体的 HLS 播放列表（Reddit 的退路 `.m3u8`）。 */
        HLS,

        /** 音画一体的整段文件（抖音 / X 的 mp4，B站老视频的 `durl`）。 */
        PROGRESSIVE,
    }

    /** 主地址（第一个）。 */
    val primaryVideoUrl: String? get() = videoUrls.firstOrNull()
}

/**
 * 把 [MediaSource] 翻译成 [MediaPlan]；没有可播的地址返回 null。
 *
 * 规则：
 * - 有 `dash` 信息（视频轨的 SegmentBase 齐全）→ [MediaPlan.Kind.DASH]：拼 MPD，音轨没给 SegmentBase 时退回 [MediaPlan.Kind.MERGED]；
 * - 没有 SegmentBase 但给了两条轨 → [MediaPlan.Kind.MERGED]；
 * - 一条轨、地址是 `.m3u8` → [MediaPlan.Kind.HLS]；
 * - 其余 → [MediaPlan.Kind.PROGRESSIVE]。
 */
fun mediaPlanOf(source: MediaSource): MediaPlan? {
    val plan = RustPresent.mediaPlan(
        RustPresent.MediaPlanFacts(
            videoUrls = source.videoUrls,
            audioUrls = source.audioUrls,
            durationMs = source.durationMs,
            dashVideo = source.dash?.video?.let {
                RustPresent.DashTrackFacts(
                    initialization = it.segmentBase.initialization,
                    indexRange = it.segmentBase.indexRange,
                    codecs = it.codecs,
                    bandwidth = it.bandwidth,
                    width = it.width,
                    height = it.height,
                    frameRate = it.frameRate,
                )
            },
            dashAudio = source.dash?.audio?.let {
                RustPresent.DashTrackFacts(
                    initialization = it.segmentBase.initialization,
                    indexRange = it.segmentBase.indexRange,
                    codecs = it.codecs,
                    bandwidth = it.bandwidth,
                    width = it.width,
                    height = it.height,
                    frameRate = it.frameRate,
                )
            },
        ),
    ) ?: return null
    return MediaPlan(
        kind = when (plan.kind) {
            RustPresent.MediaPlanKind.DASH -> MediaPlan.Kind.DASH
            RustPresent.MediaPlanKind.MERGED -> MediaPlan.Kind.MERGED
            RustPresent.MediaPlanKind.HLS -> MediaPlan.Kind.HLS
            RustPresent.MediaPlanKind.PROGRESSIVE -> MediaPlan.Kind.PROGRESSIVE
        },
        mpd = plan.mpd,
        videoUrls = plan.videoUrls,
        audioUrls = plan.audioUrls,
        durationMs = source.durationMs,
        codec = source.codec,
        bandwidthBps = source.bandwidthBps,
    )
}

/** 地址是不是一份 HLS 播放列表（按扩展名认，带查询串 / 片段也算；规则在 Rust）。 */
fun isHlsUrl(url: String): Boolean = RustPresent.isHlsUrl(url)
