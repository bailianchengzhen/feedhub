package feedhub.core

import feedhub.present.RustPresent

/**
 * 平台给的文本 / 计数 / 相对时间 → 数字。
 *
 * 规则本体在 Rust**（`rust/crates/feedhub-core/src/present/text.rs`），
 * 这里只剩过河。文件从 `:core` 搬到了 `:rust-bridge`（整类搬、包名不变）——`:core` 编译期看不见
 * `:rust-bridge`，依赖方向反着；留在生产路径上的调用方（app / cli / source-*）本来就依赖 `:rust-bridge`。
 *
 * 两处口径写在这里，免得下一个人"顺手优化"：
 * - `parseCount` 的数字部分走 Java 的 `Double.parseDouble`（比 Rust 的 `f64::from_str` 严：
 *   `"1d"` 认、`"inf"` 不认），结果按 `Math.round`（`floor(x + 0.5)`）；
 * - `stripHtml` 全仓只有一份（`feedhub_core::html::strip_html`，就是原来 `core/Text.kt` 那一串正则的
 *   逐条复刻）；小黑盒 / 知乎里那两份拷贝已经删了。
 */

/** 把平台给的计数文本转成数字："1.2万" → 12000，"3亿" → 300000000，"--" → null。 */
fun parseCount(text: String?): Long? = RustPresent.parseCount(text)

/** "12:34" / "1:02:03" → 秒数。 */
fun parseDuration(text: String?): Int? = RustPresent.parseDuration(text)

/**
 * 平台只给"相对时间"时（YouTube TV 的 `3 days ago`、B站动态的 `3小时前`）换算成 Unix 秒 **+ 精度档**。
 * 按 [now] 往前推，
 * **不假装精确**——"3 天前"就是 `now - 3*86400`，返回的 [TimePrecision] 说明它只精确到"天"，
 * 展示时由 [TimeText] 按这一档截断（[FeedItem.publishedAtPrecision]）。
 * `刚刚` / `just now` 返回 [now] + [TimePrecision.SECOND]。
 * 认不出来（含 "Preparing"、"Scheduled" 之类）返回 null，调用方宁可不要这个时间也别编一个。
 */
fun parseRelativeTimePrecise(text: String?, now: Long = System.currentTimeMillis() / 1000): RelativeTime? =
    RustPresent.parseRelativeTimePrecise(text, now)?.let { RelativeTime(it.epochSec, it.precision) }

/** [parseRelativeTimePrecise] 的结果：Unix 秒 + 它精确到哪一档。 */
data class RelativeTime(val epochSec: Long, val precision: TimePrecision)

/** 只要 Unix 秒的老入口（精度信息丢掉了，展示会当成确切时间；新代码用 [parseRelativeTimePrecise]）。 */
fun parseRelativeTime(text: String?, now: Long = System.currentTimeMillis() / 1000): Long? =
    parseRelativeTimePrecise(text, now)?.epochSec

/** 图片地址统一成 https（B站等会给 "//i0.hdslb.com/..." 或 http://）。 */
fun String.toHttps(): String = RustPresent.toHttps(this)

/** 去掉 HTML 标签并还原常见实体，得到纯文本。 */
fun stripHtml(html: String): String = RustPresent.stripHtml(html)
