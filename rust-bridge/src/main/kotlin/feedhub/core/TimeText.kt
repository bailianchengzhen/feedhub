package feedhub.core

import feedhub.present.RustPresent
import java.time.ZoneId

/**
 * 按 [TimePrecision] 显示时间。**cli 和桌面 App 共用这一份**：
 * 平台只给相对文字时，换算出来的 Unix 秒里只有那一档是真的，展示必须按同一档截断。
 *
 * 规则本体在 Rust**（`rust/crates/feedhub-core/src/present/text.rs`）：
 * 文件从 `:core` 搬到了 `:rust-bridge`（整类搬、包名不变，`:core` 看不见 `:rust-bridge`）。
 * **时区由宿主给**：Rust 不读系统时区，这里把 `java.time` 在那一刻算出来的偏移（秒）传过去——
 * `Instant.atZone(zone)` 用的就是它，所以带夏令时的时区也逐字节一致。
 *
 * 规则：
 * - 相对说法（[relative]）：只用平台给得起的那一档或更粗的一档，绝不出现更细的档。
 *   `SECOND` / `MINUTE` 保持原来的"刚刚 / X分钟前 / X小时前 / 昨天 HH:mm / MM-dd / yyyy-MM-dd"；
 *   `HOUR` 不显示分钟，最先到"X小时前"，再粗用"X天前 / MM-dd / yyyy-MM-dd"；
 *   `DAY` 连钟点都没有："今天 / 昨天 / X天前 / MM-dd / yyyy-MM-dd"；
 *   `MONTH` 不显示日："X个月前"（不到一年）或 "yyyy-MM"；`YEAR` 只给 "X年前"。
 * - 绝对说法：[absolute] 是 cli 的紧凑写法（今年省掉年份），[full] 是详情页头部的写法（总带年份）。
 *   两者都**按精度档截断**——`HOUR` 显示到小时（分写 00，因为平台没给），`DAY` 显示到日，
 *   `MONTH` 显示到月，`YEAR` 只显示年。这样不会出现"09-19 12:39"那种拿"取数据那一刻"冒充发布时间的假钟点。
 */
object TimeText {
    const val HOUR_SEC: Long = 3_600
    const val DAY_SEC: Long = 86_400

    fun relative(
        epochSec: Long,
        precision: TimePrecision = TimePrecision.SECOND,
        nowMs: Long = System.currentTimeMillis(),
        zone: ZoneId = ZoneId.systemDefault(),
    ): String = RustPresent.relative(epochSec, precision, nowMs, zone)

    /** 紧凑的绝对时间（cli 用； */
    fun absolute(
        epochSec: Long,
        precision: TimePrecision = TimePrecision.SECOND,
        nowMs: Long = System.currentTimeMillis(),
        zone: ZoneId = ZoneId.systemDefault(),
    ): String = RustPresent.absolute(epochSec, precision, nowMs, zone)

    fun full(
        epochSec: Long,
        precision: TimePrecision = TimePrecision.SECOND,
        zone: ZoneId = ZoneId.systemDefault(),
    ): String = RustPresent.full(epochSec, precision, zone)
}
