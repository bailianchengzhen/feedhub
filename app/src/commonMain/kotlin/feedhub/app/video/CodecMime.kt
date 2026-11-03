package feedhub.app.video

import feedhub.core.VideoCodec

/**
 * 安卓 `MediaCodec` 的 MIME → [VideoCodec]（`MediaCodecInfo.supportedTypes` 里就是这些字符串）。
 * 纯函数，离线可测（`CodecMimeTest`）；查硬解能力的地方在 `AndroidCodecs`。
 */
fun videoCodecOfMime(mime: String): VideoCodec? = when (mime.trim().lowercase()) {
    "video/avc", "video/x-avc", "video/h264" -> VideoCodec.AVC
    "video/hevc", "video/x-hevc", "video/h265" -> VideoCodec.HEVC
    "video/av01", "video/av1" -> VideoCodec.AV1
    else -> null
}
