//! **展示层纯函数**的 UniFFI 边界，前缀 `FfiPresent*`。
//!
//! 逻辑在 [`feedhub_core::present`]（`core/Text.kt` / `core/TimeText.kt` / `app/ui/Format.kt` /
//! `app/detail/DetailFormat.kt` / `app/mine/DayLabel.kt` / `app/feed/FeedEmpty.kt` /
//! `app/video/{VideoPosts,ImmersionFollow,MediaPlan}.kt` / `app/detail/FailureText.kt` 的那一半）。
//! 这里只做"record ↔ core 结构体"的搬运。
//!
//! **过边界的两种方式**：
//! - 单条：调用点本来就只有一条（详情头部的发布时间、"上次查到"那一行、播放器每帧的钟点）。
//! - **一批**：[`ffi_present_relative_times`] / [`ffi_present_comment_times`] /
//!   [`ffi_present_compact_counts`] / [`ffi_present_durations`]——界面一屏一次
//!   （卡片列表、评论列表），一次调用换一整屏的字。
//!
//! 时区话在这里：每个时间函数都带 `offset_at_*`（`java.time` 在那一刻算出来的偏移，秒）。
//! Rust 不读系统时区、不读时钟。

use std::sync::Arc;

use feedhub_core::appapi::FailureKind;
use feedhub_core::model::TimePrecision;
use feedhub_core::present::{self as core, text as core_text};

use crate::appapi::FfiApiFailureKind;
use crate::player::FfiDashTrack;
use crate::FfiTimePrecision;

// ===========================================================================
// 类型
// ===========================================================================

/// [`ffi_present_relative_time`] 的结果：Unix 秒 + 精度档（Kotlin `RelativeTime`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiRelativeTime {
    pub epoch_sec: i64,
    pub precision: FfiTimePrecision,
}

/// 一批时间文案的一次请求（`relative` / `comment_time` 共用）。
///
/// 一个时间点要不要两个偏移：`relative` 用 `now` 判"今天 / 昨天"、用 `epoch` 取钟点，
/// 两个瞬间的时区偏移可能不同（跨夏令时），所以分开传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiPresentTimeRequest {
    pub epoch_sec: i64,
    pub precision: FfiTimePrecision,
    pub now_ms: i64,
    pub offset_at_epoch: i64,
    pub offset_at_now: i64,
}

/// 空白的时候该画什么（Kotlin `FeedBlank`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedBlank {
    Content,
    Loading,
    Empty,
}

/// 空状态到底在说什么（Kotlin `FeedEmptyReason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedEmptyReason {
    NoPlatforms,
    NoPlatformHasFeed,
    ContentType,
    NotLoggedIn,
    NoContent,
    Offline,
    Failed,
}

/// 空状态给的出口（Kotlin `FeedEmptyAction`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFeedEmptyAction {
    GoLogin,
    GoRecommend,
    GoPlatforms,
    GoContentType,
}

/// 空状态里的一个按钮。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedEmptyButton {
    pub action: FfiFeedEmptyAction,
    pub label: String,
}

/// 空状态要画的东西。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedEmptyCopy {
    pub reason: FfiFeedEmptyReason,
    pub headline: String,
    pub body: String,
    pub buttons: Vec<FfiFeedEmptyButton>,
    pub retry: bool,
}

/// 一个平台 + 它的显示名（顺序 = 插件顺序）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedEmptyPlatform {
    pub id: String,
    pub label: String,
}

/// 这一刷失败的平台。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedErrorFact {
    pub platform: String,
    /// 失败原因就是"网络错误"。
    pub network_error: bool,
    /// 平台明确回了"没登录"。
    pub refused: bool,
}

/// 内容类型（Kotlin `ContentType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiContentType {
    Mixed,
    Video,
    Text,
}

/// 空状态判定要用的事实（Kotlin `FeedEmptyInputs`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedEmptyInputs {
    pub following: bool,
    pub content_type: FfiContentType,
    pub lit: Vec<FfiFeedEmptyPlatform>,
    pub supports_feed: Vec<String>,
    pub dropped_by_content_type: Vec<String>,
    pub requested: Vec<String>,
    pub logged_out: Vec<String>,
    pub refused: Vec<String>,
    pub errors: Vec<FfiFeedErrorFact>,
}

/// 媒体第一项的类型（`FeedItem.media.firstOrNull()`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMediaFirstKind {
    Image,
    Video,
}

/// 一次播放的流的形态（Kotlin `MediaPlan.Kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMediaPlanKind {
    Dash,
    Merged,
    Hls,
    Progressive,
}

/// `MediaSource` 里"决定开哪个流"的那些事实。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiMediaPlanInput {
    pub video_urls: Vec<String>,
    pub audio_urls: Vec<String>,
    pub duration_ms: Option<i64>,
    /// DASH 的视频轨（`MediaSource.dash.video`）；`None` = 没有 SegmentBase。
    pub dash_video: Option<FfiDashTrack>,
    /// DASH 的音频轨（`MediaSource.dash.audio`）。
    pub dash_audio: Option<FfiDashTrack>,
}

/// 决定的结果：形态 + 排序后的地址 + （DASH 时）自己拼的 MPD。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiMediaPlan {
    pub kind: FfiMediaPlanKind,
    /// 只有 DASH 有（自己拼的静态 MPD，播放器 sideload）。
    pub mpd: String,
    pub video_urls: Vec<String>,
    pub audio_urls: Vec<String>,
}

// ===========================================================================
// 转换
// ===========================================================================

fn precision_out(p: TimePrecision) -> FfiTimePrecision {
    match p {
        TimePrecision::Second => FfiTimePrecision::Second,
        TimePrecision::Minute => FfiTimePrecision::Minute,
        TimePrecision::Hour => FfiTimePrecision::Hour,
        TimePrecision::Day => FfiTimePrecision::Day,
        TimePrecision::Month => FfiTimePrecision::Month,
        TimePrecision::Year => FfiTimePrecision::Year,
    }
}

fn precision_in(p: FfiTimePrecision) -> TimePrecision {
    match p {
        FfiTimePrecision::Second => TimePrecision::Second,
        FfiTimePrecision::Minute => TimePrecision::Minute,
        FfiTimePrecision::Hour => TimePrecision::Hour,
        FfiTimePrecision::Day => TimePrecision::Day,
        FfiTimePrecision::Month => TimePrecision::Month,
        FfiTimePrecision::Year => TimePrecision::Year,
    }
}

fn content_type_in(t: FfiContentType) -> core::ContentType {
    match t {
        FfiContentType::Mixed => core::ContentType::Mixed,
        FfiContentType::Video => core::ContentType::Video,
        FfiContentType::Text => core::ContentType::Text,
    }
}

fn feed_blank_out(b: core::FeedBlank) -> FfiFeedBlank {
    match b {
        core::FeedBlank::Content => FfiFeedBlank::Content,
        core::FeedBlank::Loading => FfiFeedBlank::Loading,
        core::FeedBlank::Empty => FfiFeedBlank::Empty,
    }
}

fn feed_reason_out(r: core::FeedEmptyReason) -> FfiFeedEmptyReason {
    match r {
        core::FeedEmptyReason::NoPlatforms => FfiFeedEmptyReason::NoPlatforms,
        core::FeedEmptyReason::NoPlatformHasFeed => FfiFeedEmptyReason::NoPlatformHasFeed,
        core::FeedEmptyReason::ContentType => FfiFeedEmptyReason::ContentType,
        core::FeedEmptyReason::NotLoggedIn => FfiFeedEmptyReason::NotLoggedIn,
        core::FeedEmptyReason::NoContent => FfiFeedEmptyReason::NoContent,
        core::FeedEmptyReason::Offline => FfiFeedEmptyReason::Offline,
        core::FeedEmptyReason::Failed => FfiFeedEmptyReason::Failed,
    }
}

fn feed_action_out(a: core::FeedEmptyAction) -> FfiFeedEmptyAction {
    match a {
        core::FeedEmptyAction::GoLogin => FfiFeedEmptyAction::GoLogin,
        core::FeedEmptyAction::GoRecommend => FfiFeedEmptyAction::GoRecommend,
        core::FeedEmptyAction::GoPlatforms => FfiFeedEmptyAction::GoPlatforms,
        core::FeedEmptyAction::GoContentType => FfiFeedEmptyAction::GoContentType,
    }
}

fn feed_inputs_in(inputs: FfiFeedEmptyInputs) -> core::FeedEmptyInputs {
    core::FeedEmptyInputs {
        following: inputs.following,
        content_type: content_type_in(inputs.content_type),
        lit: inputs
            .lit
            .into_iter()
            .map(|p| core::FeedEmptyPlatform {
                id: p.id,
                label: p.label,
            })
            .collect(),
        supports_feed: inputs.supports_feed,
        dropped_by_content_type: inputs.dropped_by_content_type,
        requested: inputs.requested,
        logged_out: inputs.logged_out,
        refused: inputs.refused,
        errors: inputs
            .errors
            .into_iter()
            .map(|e| core::FeedErrorFact {
                platform: e.platform,
                network_error: e.network_error,
                refused: e.refused,
            })
            .collect(),
    }
}

fn media_plan_kind_out(k: core::MediaPlanKind) -> FfiMediaPlanKind {
    match k {
        core::MediaPlanKind::Dash => FfiMediaPlanKind::Dash,
        core::MediaPlanKind::Merged => FfiMediaPlanKind::Merged,
        core::MediaPlanKind::Hls => FfiMediaPlanKind::Hls,
        core::MediaPlanKind::Progressive => FfiMediaPlanKind::Progressive,
    }
}

fn dash_track_in(t: FfiDashTrack) -> core::DashTrackFacts {
    core::DashTrackFacts {
        initialization: t.initialization,
        index_range: t.index_range,
        codecs: t.codecs,
        bandwidth: t.bandwidth,
        width: t.width,
        height: t.height,
        frame_rate: t.frame_rate,
    }
}

// ===========================================================================
// 文本 / 计数 / 时长
// ===========================================================================

/// `parseCount`：`"1.2万"` → 12000。
#[uniffi::export]
pub fn ffi_present_parse_count(text: Option<String>) -> Option<i64> {
    core::parse_count(text.as_deref())
}

/// `parseDuration`：`"12:34"` → 754。
#[uniffi::export]
pub fn ffi_present_parse_duration(text: Option<String>) -> Option<i32> {
    core::parse_duration(text.as_deref())
}

/// `parseRelativeTimePrecise`：平台给的相对文字 → Unix 秒 + 精度档。
#[uniffi::export]
pub fn ffi_present_relative_time(text: Option<String>, now_sec: i64) -> Option<FfiRelativeTime> {
    core::parse_relative_time_precise(text.as_deref(), now_sec).map(|r| FfiRelativeTime {
        epoch_sec: r.epoch_sec,
        precision: precision_out(r.precision),
    })
}

/// `String.toHttps`：`//…` / `http://…` 统一成 https。
#[uniffi::export]
pub fn ffi_present_to_https(url: String) -> String {
    core::to_https(&url)
}

/// `stripHtml`（全仓唯一一份，见 `feedhub_core::present::text::strip_html`）。
#[uniffi::export]
pub fn ffi_present_strip_html(html: String) -> String {
    core::strip_html(&html)
}

/// `compactCount`：12345 → `"1.2万"`。`decimal_sep` 是宿主的小数点（Java 的 `%.1f` 用默认 Locale）。
#[uniffi::export]
pub fn ffi_present_compact_count(n: i64, decimal_sep: String) -> String {
    core::compact_count(n, decimal_sep.chars().next().unwrap_or('.'))
}

/// 一批 [`ffi_present_compact_count`]（等长等序；一屏卡片一次过边界）。
#[uniffi::export]
pub fn ffi_present_compact_counts(counts: Vec<i64>, decimal_sep: String) -> Vec<String> {
    let sep = decimal_sep.chars().next().unwrap_or('.');
    counts.into_iter().map(|n| core::compact_count(n, sep)).collect()
}

/// `duration`：754 → `"12:34"`。
#[uniffi::export]
pub fn ffi_present_duration(sec: i32) -> String {
    core::duration_text(sec)
}

/// 一批 [`ffi_present_duration`]（等长等序）。
#[uniffi::export]
pub fn ffi_present_durations(secs: Vec<i32>) -> Vec<String> {
    secs.into_iter().map(core::duration_text).collect()
}

// ===========================================================================
// 时间文案
// ===========================================================================

/// `TimeText.relative`（宿主给两个时区偏移）。
#[uniffi::export]
pub fn ffi_present_time_relative(
    epoch_sec: i64,
    precision: FfiTimePrecision,
    now_ms: i64,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    core::relative(
        epoch_sec,
        precision_in(precision),
        now_ms,
        offset_at_epoch,
        offset_at_now,
    )
}

/// 一批 [`ffi_present_time_relative`]（卡片列表一屏一次）。
#[uniffi::export]
pub fn ffi_present_relative_times(requests: Vec<FfiPresentTimeRequest>) -> Vec<String> {
    let reqs: Vec<core_text::RelativeRequest> = requests
        .into_iter()
        .map(|r| core_text::RelativeRequest {
            epoch_sec: r.epoch_sec,
            precision: precision_in(r.precision),
            now_ms: r.now_ms,
            offset_at_epoch: r.offset_at_epoch,
            offset_at_now: r.offset_at_now,
        })
        .collect();
    core_text::relative_times(&reqs)
}

/// `TimeText.absolute`（cli 的紧凑写法，今年省掉年份）。
#[uniffi::export]
pub fn ffi_present_time_absolute(
    epoch_sec: i64,
    precision: FfiTimePrecision,
    now_ms: i64,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    core::time_absolute(
        epoch_sec,
        precision_in(precision),
        now_ms,
        offset_at_epoch,
        offset_at_now,
    )
}

/// `TimeText.full`（详情页头部，总带年份）。
#[uniffi::export]
pub fn ffi_present_time_full(
    epoch_sec: i64,
    precision: FfiTimePrecision,
    offset_at_epoch: i64,
) -> String {
    core::time_full(epoch_sec, precision_in(precision), offset_at_epoch)
}

/// `commentTime`：今年的 `10-08 15:17`，只给到天 / 月 / 年时退回相对说法。
#[uniffi::export]
pub fn ffi_present_comment_time(
    epoch_sec: i64,
    now_ms: i64,
    precision: FfiTimePrecision,
    offset_at_epoch: i64,
    offset_at_now: i64,
) -> String {
    core::comment_time(
        epoch_sec,
        now_ms,
        precision_in(precision),
        offset_at_epoch,
        offset_at_now,
    )
}

/// 一批 [`ffi_present_comment_time`]（评论列表一屏一次）。
#[uniffi::export]
pub fn ffi_present_comment_times(requests: Vec<FfiPresentTimeRequest>) -> Vec<String> {
    let reqs: Vec<core_text::RelativeRequest> = requests
        .into_iter()
        .map(|r| core_text::RelativeRequest {
            epoch_sec: r.epoch_sec,
            precision: precision_in(r.precision),
            now_ms: r.now_ms,
            offset_at_epoch: r.offset_at_epoch,
            offset_at_now: r.offset_at_now,
        })
        .collect();
    core_text::comment_times(&reqs)
}

/// `dayLabel`：今天 / 昨天 / `"10月12日 周一"`。
#[uniffi::export]
pub fn ffi_present_day_label(
    year: i32,
    month: i32,
    day: i32,
    today_year: i32,
    today_month: i32,
    today_day: i32,
) -> String {
    core::day_label(
        year as i64,
        month.max(0) as u32,
        day.max(0) as u32,
        today_year as i64,
        today_month.max(0) as u32,
        today_day.max(0) as u32,
    )
}

/// `checkedAtLabel`：`"今天 14:03"`。
#[uniffi::export]
pub fn ffi_present_checked_at_label(
    at_ms: i64,
    now_ms: i64,
    offset_at_at: i64,
    offset_at_now: i64,
) -> String {
    core::checked_at_label(at_ms, now_ms, offset_at_at, offset_at_now)
}

// ===========================================================================
// 空状态 / 小规则
// ===========================================================================

/// `feedBlankFor`：有内容 / 加载中 / 空。
#[uniffi::export]
pub fn ffi_present_feed_blank(
    item_count: i32,
    refreshing: bool,
    loaded_once: bool,
) -> FfiFeedBlank {
    feed_blank_out(core::feed_blank_for(item_count, refreshing, loaded_once))
}

/// `feedEmptyFor`：空状态归因 + 文案 + 按钮。
#[uniffi::export]
pub fn ffi_present_feed_empty(inputs: FfiFeedEmptyInputs) -> FfiFeedEmptyCopy {
    let copy = core::feed_empty_for(&feed_inputs_in(inputs));
    FfiFeedEmptyCopy {
        reason: feed_reason_out(copy.reason),
        headline: copy.headline,
        body: copy.body,
        buttons: copy
            .buttons
            .into_iter()
            .map(|b| FfiFeedEmptyButton {
                action: feed_action_out(b.action),
                label: b.label,
            })
            .collect(),
        retry: copy.retry,
    }
}

/// `FeedEmpty` 文案里的平台名：按 `order`（插件顺序）排，最多 `max` 家，多了就说"等 N 家"。
#[uniffi::export]
pub fn ffi_present_names_of(
    order: Vec<FfiFeedEmptyPlatform>,
    picked: Vec<String>,
    max: i32,
) -> String {
    let order: Vec<core::FeedEmptyPlatform> = order
        .into_iter()
        .map(|p| core::FeedEmptyPlatform {
            id: p.id,
            label: p.label,
        })
        .collect();
    core::names_of(&order, &picked, max.max(1) as usize)
}

/// `VideoPosts.isVideoPost`。
#[uniffi::export]
pub fn ffi_present_is_video_post(
    kind_is_video: bool,
    first_media: Option<FfiMediaFirstKind>,
) -> bool {
    core::is_video_post(
        kind_is_video,
        first_media.map(|k| match k {
            FfiMediaFirstKind::Image => core::MediaFirstKind::Image,
            FfiMediaFirstKind::Video => core::MediaFirstKind::Video,
        }),
    )
}

/// `immersionFollowVisible`。
#[uniffi::export]
pub fn ffi_present_immersion_follow_visible(
    has_follow_state: bool,
    has_follow_source: bool,
) -> bool {
    core::immersion_follow_visible(has_follow_state, has_follow_source)
}

/// 关注按钮上的字（`ImmersionFollow.label`）。
#[uniffi::export]
pub fn ffi_present_follow_label(busy: bool, following: Option<bool>, forum: bool) -> String {
    core::follow_label(busy, following, forum)
}

/// 关注 / 取关成功的提示。
#[uniffi::export]
pub fn ffi_present_follow_success_message(follow: bool, target_name: String) -> String {
    core::follow_success_message(follow, &target_name)
}

/// 没登录时的提示。
#[uniffi::export]
pub fn ffi_present_follow_login_message(platform_label: String) -> String {
    core::follow_login_message(&platform_label)
}

/// 失败时的提示（`detail` 是异常的 `message`，可能是 null）。
#[uniffi::export]
pub fn ffi_present_follow_failed_message(detail: Option<String>) -> String {
    core::follow_failed_message(detail.as_deref())
}

/// `mediaPlanOf`：开哪个流、用什么形式。
#[uniffi::export]
pub fn ffi_present_media_plan(input: FfiMediaPlanInput) -> Option<FfiMediaPlan> {
    let facts = core::MediaSourceFacts {
        video_urls: input.video_urls,
        audio_urls: input.audio_urls,
        duration_ms: input.duration_ms,
        dash_video: input.dash_video.map(dash_track_in),
        dash_audio: input.dash_audio.map(dash_track_in),
    };
    core::media_plan_of(&facts).map(|plan| FfiMediaPlan {
        kind: media_plan_kind_out(plan.kind),
        mpd: plan.mpd,
        video_urls: plan.video_urls,
        audio_urls: plan.audio_urls,
    })
}

/// `isHlsUrl`。
#[uniffi::export]
pub fn ffi_present_is_hls_url(url: String) -> bool {
    core::is_hls_url(&url)
}

/// `FailureText.friendlyError`：异常种类（复用对外接口的 `FailureKind`）→ 界面上的那句话。
#[uniffi::export]
pub fn ffi_present_friendly_error(kind: FfiApiFailureKind, message: Option<String>) -> String {
    let kind: FailureKind = kind.into();
    core::friendly_error(kind, message.as_deref())
}

// ===========================================================================
// 对象：一屏一次的批量入口
// ===========================================================================

/// **一屏卡片 / 一页评论的批量入口**。
///
/// 为什么要有对象：界面那边是 Compose，重组时会把同一批字再问一遍；这个对象把"这一屏的时钟和时区
/// 偏移"记住，调用点只传一组 `(epochSec, precision)`，一屏一次过边界。
///
/// 它**不缓存结果**（规则是纯函数）：`nowMs` 和两个偏移由构造 / [`Self::set_now`] 定，
/// 所以同一批请求算出来永远是同一份字——"时间文案什么时候刷新"由宿主决定（列表变了 / 界面重建）。
#[derive(uniffi::Object)]
pub struct FfiPresentBatch {
    now_ms: i64,
    offset_at_now: i64,
}

impl FfiPresentBatch {
    fn request(&self, epoch_sec: i64, precision: FfiTimePrecision, offset_at_epoch: i64) -> core_text::RelativeRequest {
        core_text::RelativeRequest {
            epoch_sec,
            precision: precision_in(precision),
            now_ms: self.now_ms,
            offset_at_epoch,
            offset_at_now: self.offset_at_now,
        }
    }
}

#[uniffi::export]
impl FfiPresentBatch {
    /// 用"现在"和"现在的时区偏移"建一批。
    #[uniffi::constructor]
    pub fn new(now_ms: i64, offset_at_now: i64) -> Arc<Self> {
        Arc::new(FfiPresentBatch {
            now_ms,
            offset_at_now,
        })
    }

    /// 换一个"现在"（列表刷新时重建这个对象更省事，这个留给"同一屏过一分钟再问一遍"）。
    pub fn set_now(&self, now_ms: i64, offset_at_now: i64) -> Arc<Self> {
        Arc::new(FfiPresentBatch {
            now_ms,
            offset_at_now,
        })
    }

    /// 这一批的"现在"（毫秒）。
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// 一批相对时间（等长等序）。`epochs` 和 `precisions` 必须等长。
    pub fn relative_times(
        &self,
        epochs: Vec<i64>,
        precisions: Vec<FfiTimePrecision>,
        offset_at_epoch: Vec<i64>,
    ) -> Vec<String> {
        let reqs: Vec<core_text::RelativeRequest> = epochs
            .into_iter()
            .zip(precisions)
            .zip(offset_at_epoch)
            .map(|((epoch_sec, precision), offset)| self.request(epoch_sec, precision, offset))
            .collect();
        core_text::relative_times(&reqs)
    }

    /// 一批评论时间（等长等序）。
    pub fn comment_times(
        &self,
        epochs: Vec<i64>,
        precisions: Vec<FfiTimePrecision>,
        offset_at_epoch: Vec<i64>,
    ) -> Vec<String> {
        let reqs: Vec<core_text::RelativeRequest> = epochs
            .into_iter()
            .zip(precisions)
            .zip(offset_at_epoch)
            .map(|((epoch_sec, precision), offset)| self.request(epoch_sec, precision, offset))
            .collect();
        core_text::comment_times(&reqs)
    }

    /// 一批计数（等长等序）。
    pub fn compact_counts(&self, counts: Vec<i64>, decimal_sep: String) -> Vec<String> {
        let sep = decimal_sep.chars().next().unwrap_or('.');
        counts.into_iter().map(|n| core::compact_count(n, sep)).collect()
    }

    /// 一批时长（等长等序）。
    pub fn durations(&self, secs: Vec<i32>) -> Vec<String> {
        secs.into_iter().map(core::duration_text).collect()
    }
}
