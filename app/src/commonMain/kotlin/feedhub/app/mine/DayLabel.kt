package feedhub.app.mine

import feedhub.present.RustPresent
import java.time.LocalDate
import java.time.ZoneId

/**
 * 历史分组的日期标题：今天 / 昨天 / "10月12日 周日"（今年）/ "2025年10月12日 周日"（往年）。
 *
 * "怎么叫"由 Rust 决定。
 */
fun dayLabel(date: LocalDate, today: LocalDate): String = RustPresent.dayLabel(date, today)

/**
 * 「我的 → 账号」页脚那行"上次查到：…"：日期照 [dayLabel]，再带上时分，
 * 例如 `今天 14:03` / `昨天 21:05` / `10月20日 周一 09:12`。账号状态是存本机的、不会自动重查，用户靠这一行
 * 判断要不要点刷新。
 */
fun checkedAtLabel(at: Long, now: Long = System.currentTimeMillis(), zone: ZoneId = ZoneId.systemDefault()): String =
    RustPresent.checkedAtLabel(at, now, zone)
