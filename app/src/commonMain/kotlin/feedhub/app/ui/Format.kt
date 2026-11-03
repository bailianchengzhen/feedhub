package feedhub.app.ui

import feedhub.core.TimePrecision
import feedhub.core.TimeText
import feedhub.present.RustPresent

/**
 * 真正干活的是 [TimeText.relative]：
 * [precision] 是平台给的精度档，只给到天的不会显示钟点、只给到月的不会显示日
 *
 * **界面里别逐条调它**：卡片列表那种"一屏几十条、每次重组都会问一遍"的地方用
 * [RustPresent.Batch.relativeTimes] 一屏一次过边界（见 `feed/CardTexts.kt`）。
 */
fun relativeTime(
    epochSec: Long,
    precision: TimePrecision = TimePrecision.SECOND,
    nowMs: Long = System.currentTimeMillis(),
): String = TimeText.relative(epochSec, precision, nowMs)

/** 12345 → "1.2万"，123456789 → "1.2亿"（Rust `present::text::compact_count`）。 */
fun compactCount(n: Long): String = RustPresent.compactCount(n)

/** 754 → "12:34"（Rust `present::text::duration_text`）。 */
fun duration(sec: Int): String = RustPresent.duration(sec)
