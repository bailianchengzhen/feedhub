package feedhub.present

import feedhub.core.TimePrecision
import feedhub.rust.RustNative
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.text.DecimalFormatSymbols
import uniffi.feedhub_ffi.FfiApiFailureKind
import uniffi.feedhub_ffi.FfiContentType
import uniffi.feedhub_ffi.FfiDashTrack
import uniffi.feedhub_ffi.FfiFeedBlank
import uniffi.feedhub_ffi.FfiFeedEmptyAction
import uniffi.feedhub_ffi.FfiFeedEmptyButton
import uniffi.feedhub_ffi.FfiFeedEmptyCopy
import uniffi.feedhub_ffi.FfiFeedEmptyInputs
import uniffi.feedhub_ffi.FfiFeedEmptyPlatform
import uniffi.feedhub_ffi.FfiFeedErrorFact
import uniffi.feedhub_ffi.FfiFeedEmptyReason
import uniffi.feedhub_ffi.FfiMediaFirstKind
import uniffi.feedhub_ffi.FfiMediaPlan
import uniffi.feedhub_ffi.FfiMediaPlanInput
import uniffi.feedhub_ffi.FfiMediaPlanKind
import uniffi.feedhub_ffi.FfiPresentBatch
import uniffi.feedhub_ffi.FfiPresentTimeRequest
import uniffi.feedhub_ffi.FfiTimePrecision

/**
 * **展示层纯函数的 Rust 门面**。
 *
 * 规则本体在 `rust/crates/feedhub-core/src/present/`（`core/Text.kt` / `core/TimeText.kt` /
 * `app/ui/Format.kt` / `app/detail/DetailFormat.kt` / `app/mine/DayLabel.kt` /
 * `app/feed/FeedEmpty.kt` / `app/video/{VideoPosts,ImmersionFollow,MediaPlan}.kt` /
 * `app/detail/FailureText.kt` 里"不是画界面"的那一半）。这里只做类型转换 + 确保 `.so` 在。
 * **没有开关、没有 Kotlin 备用实现**。
 *
 * ## 时区 / 时钟是宿主的事
 *
 * Rust 不读系统时区、不读时钟：每个时间函数要么收 `nowMs`，要么收**那一刻的时区偏移**
 * （`java.time` 的 `ZoneRules.getOffset(instant)` 的秒数——`Instant.atZone(zone)` 用的就是它，
 * 所以带夏令时的时区也逐字节一致）。
 *
 * ## 过边界的次数（性能）
 *
 * - 单条入口：调用点本来就只有一条（详情头部、账号页"上次查到"、播放器每帧的钟点）。
 * - **一批入口**（[Batch]）：卡片列表 / 评论列表那种"一屏几十条、每次重组都会问一遍"的地方，
 *   用 [`Batch.relativeTimes`] / [`Batch.commentTimes`] / [`Batch.compactCounts`] / [`Batch.durations`]
 *   一屏一次过边界，界面里只剩一次 map / list 查表。
 */
object RustPresent {
    // =====================================================================
    // 文本 / 计数 / 时长
    // =====================================================================

    /** `parseCount`："1.2万" → 12000；"--" → null。 */
    fun parseCount(text: String?): Long? = load { uniffi.feedhub_ffi.ffiPresentParseCount(text) }

    /** `parseDuration`："12:34" → 754。 */
    fun parseDuration(text: String?): Int? = load { uniffi.feedhub_ffi.ffiPresentParseDuration(text) }

    /** `parseRelativeTimePrecise`：平台给的相对文字 → Unix 秒 + 精度档。 */
    fun parseRelativeTimePrecise(text: String?, nowSec: Long): RelativeTime? =
        load { uniffi.feedhub_ffi.ffiPresentRelativeTime(text, nowSec) }?.let {
            RelativeTime(it.epochSec, it.precision.toKotlin())
        }

    /** `String.toHttps`。 */
    fun toHttps(url: String): String = load { uniffi.feedhub_ffi.ffiPresentToHttps(url) }

    /** `stripHtml`（全仓唯一一份）。 */
    fun stripHtml(html: String): String = load { uniffi.feedhub_ffi.ffiPresentStripHtml(html) }

    /** `compactCount`：12345 → "1.2万"。小数点跟宿主的默认 Locale（Java 的 `%.1f` 就是这么来的）。 */
    fun compactCount(n: Long): String =
        load { uniffi.feedhub_ffi.ffiPresentCompactCount(n, DECIMAL_SEP) }

    /** 一批 `compactCount`（等长等序；一屏卡片一次）。 */
    fun compactCounts(counts: List<Long>): List<String> =
        load { uniffi.feedhub_ffi.ffiPresentCompactCounts(counts, DECIMAL_SEP) }

    /** `duration`：754 → "12:34"。 */
    fun duration(sec: Int): String = load { uniffi.feedhub_ffi.ffiPresentDuration(sec) }

    /** 一批 `duration`（等长等序）。 */
    fun durations(secs: List<Int>): List<String> = load { uniffi.feedhub_ffi.ffiPresentDurations(secs) }

    /** 宿主的小数点（`DecimalFormatSymbols.getInstance()` 那份，`%.1f` 用它）。只取一次。 */
    val DECIMAL_SEP: String by lazy {
        DecimalFormatSymbols.getInstance().decimalSeparator.toString()
    }

    // =====================================================================
    // 时间文案（时区由宿主给）
    // =====================================================================

    fun relative(epochSec: Long, precision: TimePrecision, nowMs: Long): String =
        relative(epochSec, precision, nowMs, ZoneId.systemDefault())

    /** 同上，时区显式给（cli 用 `Asia/Shanghai`、测试用固定时区）。 */
    fun relative(epochSec: Long, precision: TimePrecision, nowMs: Long, zone: ZoneId): String = load {
        uniffi.feedhub_ffi.ffiPresentTimeRelative(
            epochSec,
            precision.toFfi(),
            nowMs,
            offsetAt(zone, epochSec),
            offsetAtMs(zone, nowMs),
        )
    }

    /** `TimeText.absolute`（cli 的紧凑写法，今年省掉年份）。 */
    fun absolute(epochSec: Long, precision: TimePrecision, nowMs: Long, zone: ZoneId): String = load {
        uniffi.feedhub_ffi.ffiPresentTimeAbsolute(
            epochSec,
            precision.toFfi(),
            nowMs,
            offsetAt(zone, epochSec),
            offsetAtMs(zone, nowMs),
        )
    }

    /** `TimeText.full`（详情页头部，总带年份）。 */
    fun full(epochSec: Long, precision: TimePrecision, zone: ZoneId = ZoneId.systemDefault()): String =
        load { uniffi.feedhub_ffi.ffiPresentTimeFull(epochSec, precision.toFfi(), offsetAt(zone, epochSec)) }

    /** `commentTime`：今年的 "10-08 15:17"， */
    fun commentTime(epochSec: Long, nowMs: Long, precision: TimePrecision, zone: ZoneId): String = load {
        uniffi.feedhub_ffi.ffiPresentCommentTime(
            epochSec,
            nowMs,
            precision.toFfi(),
            offsetAt(zone, epochSec),
            offsetAtMs(zone, nowMs),
        )
    }

    /** `dayLabel`：今天 / 昨天 / "10月12日 周一"（今年）/ "2025年10月12日 周日"（往年）。 */
    fun dayLabel(date: LocalDate, today: LocalDate): String =
        dayLabel(date.year, date.monthValue, date.dayOfMonth, today.year, today.monthValue, today.dayOfMonth)

    fun dayLabel(
        year: Int,
        month: Int,
        day: Int,
        todayYear: Int,
        todayMonth: Int,
        todayDay: Int,
    ): String = load {
        uniffi.feedhub_ffi.ffiPresentDayLabel(year, month, day, todayYear, todayMonth, todayDay)
    }

    /** "上次查到：今天 14:03" 那一行。 */
    fun checkedAtLabel(at: Long, now: Long, zone: ZoneId = ZoneId.systemDefault()): String = load {
        uniffi.feedhub_ffi.ffiPresentCheckedAtLabel(at, now, offsetAtMs(zone, at), offsetAtMs(zone, now))
    }

    // =====================================================================
    // 规则
    // =====================================================================

    /** 空白的时候该画什么。 */
    fun feedBlank(itemCount: Int, refreshing: Boolean, loadedOnce: Boolean): FeedBlank =
        when (load { uniffi.feedhub_ffi.ffiPresentFeedBlank(itemCount, refreshing, loadedOnce) }) {
            FfiFeedBlank.CONTENT -> FeedBlank.CONTENT
            FfiFeedBlank.LOADING -> FeedBlank.LOADING
            FfiFeedBlank.EMPTY -> FeedBlank.EMPTY
        }

    /** 空状态归因 + 文案 + 按钮。 */
    fun feedEmpty(facts: FeedEmptyFacts): FeedEmptyCopy = load {
        uniffi.feedhub_ffi.ffiPresentFeedEmpty(facts.toFfi())
    }.toKotlin()

    /** 空状态文案里的平台名（按插件顺序，最多 `max` 家，多了"等 N 家"）。 */
    fun namesOf(order: List<FeedEmptyPlatform>, picked: List<String>, max: Int = 3): String = load {
        uniffi.feedhub_ffi.ffiPresentNamesOf(
            order.map { FfiFeedEmptyPlatform(it.id, it.label) },
            picked,
            max,
        )
    }

    /** 这条内容算不算视频（kind + 媒体第一项）。 */
    fun isVideoPost(kindIsVideo: Boolean, firstMedia: MediaFirst?): Boolean {
        val first = when (firstMedia) {
            null -> null
            MediaFirst.IMAGE -> FfiMediaFirstKind.IMAGE
            MediaFirst.VIDEO -> FfiMediaFirstKind.VIDEO
        }
        return load { uniffi.feedhub_ffi.ffiPresentIsVideoPost(kindIsVideo, first) }
    }

    /** `FeedItem.media.firstOrNull()` 的类型（只关心"是不是视频"）。 */
    enum class MediaFirst { IMAGE, VIDEO }

    /** 沉浸界面的关注按钮显不显示。 */
    fun immersionFollowVisible(hasFollowState: Boolean, hasFollowSource: Boolean): Boolean =
        load { uniffi.feedhub_ffi.ffiPresentImmersionFollowVisible(hasFollowState, hasFollowSource) }

    /** 关注按钮上的字。 */
    fun followLabel(busy: Boolean, following: Boolean?, forum: Boolean): String =
        load { uniffi.feedhub_ffi.ffiPresentFollowLabel(busy, following, forum) }

    fun followSuccessMessage(follow: Boolean, targetName: String): String =
        load { uniffi.feedhub_ffi.ffiPresentFollowSuccessMessage(follow, targetName) }

    fun followLoginMessage(platformLabel: String): String =
        load { uniffi.feedhub_ffi.ffiPresentFollowLoginMessage(platformLabel) }

    fun followFailedMessage(detail: String?): String =
        load { uniffi.feedhub_ffi.ffiPresentFollowFailedMessage(detail) }

    /** `mediaPlanOf`：开哪个流、用什么形式（没有可播的地址返回 null）。 */
    fun mediaPlan(facts: MediaPlanFacts): MediaPlanValue? = load {
        uniffi.feedhub_ffi.ffiPresentMediaPlan(facts.toFfi())
    }?.toKotlin()

    /** 地址是不是 HLS 播放列表。 */
    fun isHlsUrl(url: String): Boolean = load { uniffi.feedhub_ffi.ffiPresentIsHlsUrl(url) }

    /**
     * `friendlyError`：异常种类（复用对外接口的 [FfiApiFailureKind]）→ 界面上的那句话。
     * "哪一种异常对应哪一类"由宿主决定（那是宿主异常类型）；"哪一类对应哪个字"在 Rust。
     */
    fun friendlyError(kind: FfiApiFailureKind, message: String?): String =
        load { uniffi.feedhub_ffi.ffiPresentFriendlyError(kind, message) }

    // =====================================================================
    // 一批（一屏一次）
    // =====================================================================

    /**
     * 一屏卡片 / 一页评论的批量入口：把"现在"和"现在的时区偏移"记住，调用点只传一组
     * `(epochSec, precision)`，一屏一次过边界。
     *
     * **时间文案什么时候刷新由宿主决定**：列表刷新时重建这个对象（`remember(items) { Batch(...) }`），
     * 重组时不再过边界。
     */
    class Batch(
        nowMs: Long = System.currentTimeMillis(),
        private val zone: ZoneId = ZoneId.systemDefault(),
    ) {
        private val native: FfiPresentBatch = load { FfiPresentBatch(nowMs, offsetAtMs(zone, nowMs)) }

        /** 这一批的"现在"（毫秒）。 */
        val nowMs: Long = native.nowMs()

        /** 一批相对时间（`(epoch, precision)`，等长等序）。 */
        fun relativeTimes(items: List<Pair<Long, TimePrecision>>): List<String> = load {
            native.relativeTimes(
                items.map { it.first },
                items.map { it.second.toFfi() },
                items.map { offsetAt(zone, it.first) },
            )
        }

        /** 一批评论时间（等长等序）。 */
        fun commentTimes(items: List<Pair<Long, TimePrecision>>): List<String> = load {
            native.commentTimes(
                items.map { it.first },
                items.map { it.second.toFfi() },
                items.map { offsetAt(zone, it.first) },
            )
        }

        /** 一批计数（等长等序）。 */
        fun compactCounts(counts: List<Long>): List<String> = load { native.compactCounts(counts, DECIMAL_SEP) }

        /** 一批时长（等长等序）。 */
        fun durations(secs: List<Int>): List<String> = load { native.durations(secs) }
    }

    // =====================================================================
    // 类型
    // =====================================================================

    /** `parseRelativeTimePrecise` 的结果。 */
    data class RelativeTime(val epochSec: Long, val precision: TimePrecision)

    /** 空白的时候该画什么（原 `feedhub.app.feed.FeedBlank`）。 */
    enum class FeedBlank { CONTENT, LOADING, EMPTY }

    /** 空状态到底在说什么（原 `feedhub.app.feed.FeedEmptyReason`）。 */
    enum class FeedEmptyReason {
        NO_PLATFORMS,
        NO_PLATFORM_HAS_FEED,
        CONTENT_TYPE,
        NOT_LOGGED_IN,
        NO_CONTENT,
        OFFLINE,
        FAILED,
    }

    /** 空状态给的出口（原 `feedhub.app.feed.FeedEmptyAction`）。 */
    enum class FeedEmptyAction { GO_LOGIN, GO_RECOMMEND, GO_PLATFORMS, GO_CONTENT_TYPE }

    /** 一个按钮：动作 + 文字。 */
    data class FeedEmptyButton(val action: FeedEmptyAction, val label: String)

    /** 空状态要画的东西。 */
    data class FeedEmptyCopy(
        val reason: FeedEmptyReason,
        val headline: String,
        val body: String,
        val buttons: List<FeedEmptyButton>,
        val retry: Boolean,
    )

    /** 一个平台 + 它的显示名（顺序 = 插件顺序）。 */
    data class FeedEmptyPlatform(val id: String, val label: String)

    /** 这一刷失败的平台。 */
    data class FeedErrorFact(val platform: String, val networkError: Boolean, val refused: Boolean)

    /** 内容类型（原 `feedhub.app.ContentType`）。 */
    enum class FeedContentType { MIXED, VIDEO, TEXT }

    /** 空状态判定要用的事实（原 `FeedEmptyInputs` 的那半）。 */
    data class FeedEmptyFacts(
        val following: Boolean,
        val contentType: FeedContentType,
        val lit: List<FeedEmptyPlatform>,
        val supportsFeed: List<String>,
        val droppedByContentType: List<String>,
        val requested: List<String>,
        val loggedOut: List<String>,
        val refused: List<String>,
        val errors: List<FeedErrorFact>,
    )

    /** DASH 一条轨的索引信息（原 `feedhub.app.video.DashTrack` 的那半）。 */
    data class DashTrackFacts(
        val initialization: String,
        val indexRange: String,
        val codecs: String? = null,
        val bandwidth: Long? = null,
        val width: Int? = null,
        val height: Int? = null,
        val frameRate: String? = null,
    )

    /** `MediaSource` 里"决定开哪个流"的那些事实。 */
    data class MediaPlanFacts(
        val videoUrls: List<String>,
        val audioUrls: List<String> = emptyList(),
        val durationMs: Long? = null,
        val dashVideo: DashTrackFacts? = null,
        val dashAudio: DashTrackFacts? = null,
    )

    /** 一次播放的流的形态（原 `MediaPlan.Kind`）。 */
    enum class MediaPlanKind { DASH, MERGED, HLS, PROGRESSIVE }

    /** 决定的结果：形态 + 排序后的地址 + （DASH 时）自己拼的 MPD。 */
    data class MediaPlanValue(
        val kind: MediaPlanKind,
        val mpd: String,
        val videoUrls: List<String>,
        val audioUrls: List<String>,
    )

    // =====================================================================
    // 内部：类型转换
    // =====================================================================

    private fun TimePrecision.toFfi(): FfiTimePrecision = when (this) {
        TimePrecision.SECOND -> FfiTimePrecision.SECOND
        TimePrecision.MINUTE -> FfiTimePrecision.MINUTE
        TimePrecision.HOUR -> FfiTimePrecision.HOUR
        TimePrecision.DAY -> FfiTimePrecision.DAY
        TimePrecision.MONTH -> FfiTimePrecision.MONTH
        TimePrecision.YEAR -> FfiTimePrecision.YEAR
    }

    private fun FfiTimePrecision.toKotlin(): TimePrecision = when (this) {
        FfiTimePrecision.SECOND -> TimePrecision.SECOND
        FfiTimePrecision.MINUTE -> TimePrecision.MINUTE
        FfiTimePrecision.HOUR -> TimePrecision.HOUR
        FfiTimePrecision.DAY -> TimePrecision.DAY
        FfiTimePrecision.MONTH -> TimePrecision.MONTH
        FfiTimePrecision.YEAR -> TimePrecision.YEAR
    }

    private fun FfiFeedEmptyReason.toKotlin(): FeedEmptyReason = when (this) {
        FfiFeedEmptyReason.NO_PLATFORMS -> FeedEmptyReason.NO_PLATFORMS
        FfiFeedEmptyReason.NO_PLATFORM_HAS_FEED -> FeedEmptyReason.NO_PLATFORM_HAS_FEED
        FfiFeedEmptyReason.CONTENT_TYPE -> FeedEmptyReason.CONTENT_TYPE
        FfiFeedEmptyReason.NOT_LOGGED_IN -> FeedEmptyReason.NOT_LOGGED_IN
        FfiFeedEmptyReason.NO_CONTENT -> FeedEmptyReason.NO_CONTENT
        FfiFeedEmptyReason.OFFLINE -> FeedEmptyReason.OFFLINE
        FfiFeedEmptyReason.FAILED -> FeedEmptyReason.FAILED
    }

    private fun FfiFeedEmptyAction.toKotlin(): FeedEmptyAction = when (this) {
        FfiFeedEmptyAction.GO_LOGIN -> FeedEmptyAction.GO_LOGIN
        FfiFeedEmptyAction.GO_RECOMMEND -> FeedEmptyAction.GO_RECOMMEND
        FfiFeedEmptyAction.GO_PLATFORMS -> FeedEmptyAction.GO_PLATFORMS
        FfiFeedEmptyAction.GO_CONTENT_TYPE -> FeedEmptyAction.GO_CONTENT_TYPE
    }

    private fun FfiFeedEmptyCopy.toKotlin(): FeedEmptyCopy = FeedEmptyCopy(
        reason = reason.toKotlin(),
        headline = headline,
        body = body,
        buttons = buttons.map { FeedEmptyButton(it.action.toKotlin(), it.label) },
        retry = retry,
    )

    private fun FeedEmptyFacts.toFfi(): FfiFeedEmptyInputs = FfiFeedEmptyInputs(
        following = following,
        contentType = when (contentType) {
            FeedContentType.MIXED -> FfiContentType.MIXED
            FeedContentType.VIDEO -> FfiContentType.VIDEO
            FeedContentType.TEXT -> FfiContentType.TEXT
        },
        lit = lit.map { FfiFeedEmptyPlatform(it.id, it.label) },
        supportsFeed = supportsFeed,
        droppedByContentType = droppedByContentType,
        requested = requested,
        loggedOut = loggedOut,
        refused = refused,
        errors = errors.map { FfiFeedErrorFact(it.platform, it.networkError, it.refused) },
    )

    private fun FfiMediaPlan.toKotlin(): MediaPlanValue = MediaPlanValue(
        kind = when (kind) {
            FfiMediaPlanKind.DASH -> MediaPlanKind.DASH
            FfiMediaPlanKind.MERGED -> MediaPlanKind.MERGED
            FfiMediaPlanKind.HLS -> MediaPlanKind.HLS
            FfiMediaPlanKind.PROGRESSIVE -> MediaPlanKind.PROGRESSIVE
        },
        mpd = mpd,
        videoUrls = videoUrls,
        audioUrls = audioUrls,
    )

    private fun MediaPlanFacts.toFfi(): FfiMediaPlanInput = FfiMediaPlanInput(
        videoUrls = videoUrls,
        audioUrls = audioUrls,
        durationMs = durationMs,
        dashVideo = dashVideo?.toFfi(),
        dashAudio = dashAudio?.toFfi(),
    )

    private fun DashTrackFacts.toFfi(): FfiDashTrack = FfiDashTrack(
        initialization = initialization,
        indexRange = indexRange,
        codecs = codecs,
        bandwidth = bandwidth,
        width = width,
        height = height,
        frameRate = frameRate,
    )

    /** `java.time` 在那一刻算出来的偏移（秒）：`Instant.atZone(zone)` 用的就是它。 */
    private fun offsetAt(zone: ZoneId, epochSec: Long): Long =
        zone.rules.getOffset(Instant.ofEpochSecond(epochSec)).totalSeconds.toLong()

    private fun offsetAtMs(zone: ZoneId, epochMs: Long): Long =
        zone.rules.getOffset(Instant.ofEpochMilli(epochMs)).totalSeconds.toLong()

    /** 每次过边界前确保 `.so` 在（[RustNative] 自己会记住"已经加载过"）。 */
    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
