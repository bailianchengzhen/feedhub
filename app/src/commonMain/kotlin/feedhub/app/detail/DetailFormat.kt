package feedhub.app.detail

import feedhub.core.TimePrecision
import feedhub.core.TimeText
import feedhub.present.RustPresent
import java.time.ZoneId

/**
 * 平台只给到天 / 月 / 年的（见 [TimePrecision]）按那一档截断，
 * 不会拿"取数据那一刻"的假钟点冒充发布时间。
 */
fun fullTime(epochSec: Long, precision: TimePrecision = TimePrecision.SECOND): String =
    TimeText.full(epochSec, precision)

/**
 * 评论的时间：今年的 "10-08 15:17"，平台只给相对时间时（YouTube TV 的评论是
 * "6 years ago"，见 [TimePrecision]）按那一档截断，不会出现假钟点。
 *
 * **评论列表别逐条调它**：用 [RustPresent.Batch.commentTimes] 一屏一次（见 `feed/CardTexts.kt`）。
 */
fun commentTime(
    epochSec: Long,
    nowMs: Long = System.currentTimeMillis(),
    precision: TimePrecision = TimePrecision.SECOND,
): String = RustPresent.commentTime(epochSec, nowMs, precision, ZoneId.systemDefault())
