//! **展示层的纯函数**：时间 / 文本文案 / 空状态归因 / 小规则 /
//! 出错时那句人话。
//!
//! 搬进来的是这些 Kotlin 文件里"不是画界面"的那一半：
//!
//! | Kotlin | 这里 |
//! |---|---|
//! | `core/Text.kt`（`parseCount` / `parseDuration` / `parseRelativeTimePrecise` / `toHttps` / `stripHtml`） | [`text`] |
//! | `core/TimeText.kt`（`relative` / `absolute` / `full`） | [`text`] |
//! | `app/ui/Format.kt`（`relativeTime` / `compactCount` / `duration`） | [`text`] |
//! | `app/detail/DetailFormat.kt`（`fullTime` / `commentTime`） | [`text`] |
//! | `app/mine/DayLabel.kt`（`dayLabel` / `checkedAtLabel`） | [`text`] |
//! | `app/feed/FeedEmpty.kt`（`feedEmptyFor` / `feedBlankFor`） | [`rules`] |
//! | `app/video/VideoPosts.kt`（`isVideoPost`） | [`rules`] |
//! | `app/video/ImmersionFollow.kt`（显示条件 / 按钮字 / 提示语） | [`rules`] |
//! | `app/video/MediaPlan.kt`（`mediaPlanOf` / `isHlsUrl`） | [`rules`] |
//! | `app/detail/FailureText.kt`（异常 → 人话） | [`errors`] |
//!
//! ## 宿主必须给的两样事实
//!
//! - **时区**：Rust **不读系统时区**。调用方传的是 `java.time` 在**那一刻**算出来的
//!   `ZoneOffset`（秒）——`Instant.atZone(zone)` 用的就是它，所以带夏令时的时区也逐字节一致。
//!   需要两处的是"这一刻"和"现在"（`relative` 用 `now` 判"今天 / 昨天"，用 `epoch` 取钟点）。
//! - **时钟**：`nowMs` 由宿主传（`System.currentTimeMillis()`）。**Rust 里没有任何 `now()` 调用**。
//!
//! ## 和 Kotlin 逐字节一致的几处"口径"
//!
//! - **空白**：Kotlin 的 `String.trim()` 用的是 `Char.isWhitespace`（= `Character.isWhitespace ||
//!   isSpaceChar`，**含 NBSP 类**），不是 Java 的 `Character.isWhitespace`。这里统一用
//!   [`crate::html::kotlin_trim`]（那一份是照 `Char.isWhitespace` 写的）。
//! - **`"%.1f".format(…)`**：Java 的 Formatter 对 double 的**精确十进制展开**做 HALF_UP 舍入
//!   （`1.25` → `"1.3"`，Rust 的 `{:.1}` 是 half-to-even 会给 `"1.2"`）。见
//!   [`text::java_fixed_1`]。小数点是宿主给的一个字符（Java 的是 `DecimalFormatSymbols` 的默认值）。
//! - **`Double.parseDouble`**：比 Rust 的 `f64::from_str` 严（`"1d"` / `"NaN"`
//!   这类大小写敏感的后缀 / 特殊值），见 [`text::java_parse_double`]。
//!
//! ## 留在宿主的
//!
//! 画界面（Compose 的 `FailureBlock` / `FeedEmpty` 那些 `@Composable`）、`Intent` / 剪贴板 / 播放器
//! 引擎调用、以及"这一刻的时区偏移"（`java.time` 的时区库）。这一层只有纯函数，没有 IO、没有时钟。

pub mod errors;
pub mod rules;
pub mod text;

pub use errors::friendly_error;
pub use rules::{
    feed_blank_for, feed_empty_for, follow_failed_message, follow_label, follow_login_message,
    follow_success_message, immersion_follow_visible, is_hls_url, is_video_post, media_plan_of, names_of,
    ContentType, DashTrackFacts, FeedBlank, FeedEmptyAction, FeedEmptyButton, FeedEmptyCopy,
    FeedEmptyInputs, FeedEmptyPlatform, FeedEmptyReason, FeedErrorFact, MediaFirstKind,
    MediaPlanKind, MediaPlanOutcome, MediaSourceFacts,
};
pub use text::{
    checked_at_label, compact_count, comment_time, day_label, duration_text, java_fixed_1, java_parse_double,
    parse_count, parse_duration, parse_relative_time_precise, relative, relative_time_of,
    relative_times, strip_html, time_absolute, time_full, to_https, week_label, RelativeTime,
};

pub mod cache;
pub mod comments;
pub mod hints;
pub mod images;
pub mod links;
pub mod paging;
pub mod snapshot;
