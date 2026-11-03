package feedhub.app.video

import feedhub.core.VideoCodec
import feedhub.player.DecodeCapability
import feedhub.player.RustPlayer

/**
 * 这台机器各编码的解码能力，"自动"选编码据此排序（[StreamSelection.autoOrder]）。
 *
 * 规则（"没测出上限就不限制"、
 * "缺宽 / 高 / 帧率就当不限制"、"分数帧率先算浮点再截断"）一个字都不在 Kotlin。原版在
 * `app/src/desktopTest/.../video/legacy/DecodeCapability.kt` 当对拍参照。
 *
 * 为什么类型放在 `:rust-bridge`：它是 `RustPlayer.select` / `autoOrder` 的入参，而 `:core` 在依赖顺序上
 * 在 `:rust-bridge` 下面（和 `feedhub.brain` 一个理由）。
 */
typealias DecodeCapability = feedhub.player.DecodeCapability

/** [VideoStream.frameRate] / `Structure` 里的帧率文本（"30"、"60/1"、"30000/1001"）→ 每秒帧数；认不出就 null。 */
fun frameRateOf(text: String?): Double? = RustPlayer.frameRateOf(text)

/** 一路视频轨的负载（每秒像素数）；缺宽 / 高 / 帧率就 null（那就按"不限制"处理）。 */
fun pixelRateOf(width: Int?, height: Int?, frameRate: String?): Long? =
    RustPlayer.pixelRateOf(width, height, frameRate)

/** 像素率上限 → "这一档以下还能实时"的标签。 **真值在 Rust**（`pixel_rate_label`）。 */
fun pixelRateLabel(rate: Long): String = RustPlayer.pixelRateLabel(rate)

/** 上面那套能力的快照；[feedhub.app.video.GstRuntime.hardwareCodecs]（桌面）是"设备侧确认能硬解"的那一半。 */
internal fun currentDecodeCapability(hardware: Set<VideoCodec>): DecodeCapability =
    DecodeCapability(hardware = hardware, smoothPixelRate = DecodeLedger.snapshot())
