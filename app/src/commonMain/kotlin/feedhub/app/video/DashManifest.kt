package feedhub.app.video

/**
 * 把 B站的"视频轨 + 音频轨"（各是一个带 sidx 的分片 mp4）拼成一份只含 SegmentBase 的静态 MPD，交给 GStreamer 的 dashdemux2。
 * 为什么不直接两路 souphttpsrc ! qtdemux：分片 mp4 在推模式下 qtdemux 不支持跳转（"ignoring seek in push mode"），
 * 用 queue2 环形缓冲凑出拉模式又要把中间的每个 moof 都读一遍（跳到 400 秒要 16 秒）；dashdemux2 靠 sidx 直接定位，跳转不到 1 秒。
 */
object DashManifest {
    /**
     * 逐字节和原来一样）。
     * 这里只把 [DashInfo] 翻译成 Rust 的入参（`FfiDashTrack`），拼字符串一个字节都不在 Kotlin。
     */
    fun build(videoUrl: String, audioUrl: String?, info: DashInfo, durationMs: Long?): String =
        feedhub.player.RustPlayer.dashManifest(
            videoUrl = videoUrl,
            audioUrl = audioUrl,
            video = info.video.toRust(),
            audio = info.audio?.toRust(),
            durationMs = durationMs,
        )

    private fun DashTrack.toRust(): feedhub.player.DashTrack = feedhub.player.DashTrack(
        initialization = segmentBase.initialization,
        indexRange = segmentBase.indexRange,
        codecs = codecs,
        bandwidth = bandwidth,
        width = width,
        height = height,
        frameRate = frameRate,
    )
}
