package feedhub.player

import feedhub.core.DanmakuMode
import feedhub.core.VideoCodec
import kotlinx.serialization.Serializable

/**
 * 播放器决策层的**模型**（Rust `feedhub_core::player::*` 的 Kotlin 面）。
 *
 * 为什么模型住在 `:rust-bridge`（和 `feedhub.brain` 一个理由）：这些类型是"要调 Rust 的规则"的入参 / 出参，
 * 而 `:core` 在依赖顺序上在 `:rust-bridge` 下面（`api(project(":core"))`），调不到 Rust。业务代码
 * （`app/.../video/`）import 一行不用改——那边用 `typealias` 指着这里。
 *
 * **每一处"值"的唯一真相在 Rust**：这个文件里只有形状 + 那些必须编译期就是常量的东西（`@Serializable`
 * 的默认值、枚举顺序）。范围 / 档位 / 上限这些**可变的**值一律问 Rust（`RustDanmaku.maxRules()` 之类），
 * 由 `PlayerModelParityTest` 逐条盯着。
 */

// ===========================================================================
// 选流
// ===========================================================================

/** 设置里的编码偏好（`AppSettings.codec`）。 */
@Serializable
enum class CodecPreference { AUTO, AVC, HEVC, AV1 }

/**
 * 设置里的默认清晰度。**枚举顺序就是"从高到低"**：`limitQuality` 用的就是 ordinal。
 */
@Serializable
enum class DefaultQuality { HIGHEST, P1080, P720 }

/**
 * 这台机器各编码的解码能力，"自动"选编码据此排序。
 *
 * 两个维度都是**实测**出来的：`hardware`（设备侧确认过有真硬解）、`smoothPixelRate`（播放过程中量出来的
 * "这种编码在这个负载以下能稳定实时出帧"的上限，单位是每秒像素数）。空表 = 还没测出问题。
 */
data class DecodeCapability(
    /** 确认有真硬解的编码（"真" = 设备侧确认，不是只看元素存不存在）。 */
    val hardware: Set<VideoCodec> = emptySet(),
    /** 实测"稳定实时"的像素率上限（每秒像素数）；不在表里 = 没测出问题，不限制。 */
    val smoothPixelRate: Map<VideoCodec, Long> = emptyMap(),
) {
    /** 这个像素率下这种编码能不能实时：没测出上限就不限制。 */
    fun smoothAt(codec: VideoCodec, pixelsPerSecond: Long?): Boolean {
        if (pixelsPerSecond == null) return true
        val limit = smoothPixelRate[codec] ?: return true
        return pixelsPerSecond <= limit
    }

    companion object {
        /** 只知道"能硬解哪些"，没有实测负载数据。 */
        fun ofHardware(hardware: Set<VideoCodec>) = DecodeCapability(hardware = hardware)
    }
}

/** 选流的结果：三个**下标**（回 `PlayInfo` 里取对象，不深拷贝）。 */
data class StreamChoice(
    val quality: Int?,
    val video: Int? = null,
    val audio: Int? = null,
    val progressive: Int? = null,
)

/** 清晰度菜单里的一项。 */
data class QualityOption(val id: Int, val label: String, val enabled: Boolean)

/** DASH 里一条轨的描述（拼 MPD 用）。 */
class DashTrack(
    val initialization: String,
    val indexRange: String,
    val codecs: String? = null,
    val bandwidth: Long? = null,
    val width: Int? = null,
    val height: Int? = null,
    val frameRate: String? = null,
)

// ===========================================================================
// 弹幕：过滤
// ===========================================================================

/**
 * 弹幕进时间线时做的过滤和合并（Rust `danmaku::Filter`）。
 *
 * 顺序照 BiliPai：丢掉不能显示的（`OTHER`、空文字）→ 按类型 / 颜色 / 权重 / 屏蔽词过滤 →
 * 顶部底部转滚动 → 按时间排序 → 合并重复。
 */
@Serializable
data class DanmakuFilter(
    val allowScroll: Boolean = true,
    val allowTop: Boolean = true,
    val allowBottom: Boolean = true,
    val allowColorful: Boolean = true,
    /** 权重低于它的弹幕丢掉；0 = 不按权重丢。没有权重的弹幕不受影响。 */
    val weightLevel: Int = 0,
    /** 屏蔽词，写法见 [RuleKind]。 */
    val rules: List<String> = emptyList(),
    val staticToScroll: Boolean = false,
    val merge: Boolean = false,
    val mergeWindowMs: Long = 500,
    val mergeThreshold: Int = 2,
) {
    companion object {
        /** 什么都不过滤、不合并（只丢掉 OTHER 和空文字）。 */
        val NONE = DanmakuFilter()
    }
}

/** 一条屏蔽词算哪种（Rust `RuleKind`）。 */
enum class RuleKind {
    /** 关键词：弹幕里包含它就屏蔽（不分大小写）。 */
    KEYWORD,

    /** 正则：弹幕里能匹配上就屏蔽（不分大小写）。 */
    REGEX,

    /** 写成了正则但编译不了、或者会匹配空文字（那会屏蔽所有弹幕）：不起作用。 */
    INVALID,
}

// ===========================================================================
// 弹幕：设置与排布
// ===========================================================================

/**
 * 弹幕设置（Rust `danmaku::Settings`）。纯数据 + 换算，不碰 Compose。
 *
 * 各选项的含义、取值范围和默认值照 BiliPai（DanmakuConfig / SettingsManager）；默认值里和 BiliPai 不一样的
 * 几个（滚动时长、行高、字重）是为了和这个项目原来的弹幕看起来一样。选项按"改了会影响什么"分三类，
 * 见 [filter]、[layout] 和只管绘制的 [opacity]、[strokeWidth]。
 */
@Serializable
data class DanmakuSettings(
    /** 透明度，0.3~1。 */
    val opacity: Float = 0.85f,
    /** 字号缩放，0.5~2：在"字号随播放器高度走"的基础上再乘。 */
    val fontScale: Float = 1f,
    /** 字重 1~9（×100 就是 CSS 的 font-weight），4 = 常规 400，9 = 最粗。 */
    val fontWeight: Int = 4,
    /** 描边粗细，0~4 dp。 */
    val strokeWidth: Float = 1.5f,
    /** 显示区域：弹幕占画面高度的多少，只能取 [AREA_OPTIONS] 里的八档。 */
    val area: Float = 0.5f,
    /** 行高，0.8~2.2。 */
    val lineHeight: Float = 1.4f,
    /** 海量模式：行距压到 3/4（不低于 1.0）。 */
    val massive: Boolean = false,
    /** 滚动弹幕从出现到完全消失的时间，2~15 秒（正常倍速下）。 */
    val scrollSeconds: Float = 8f,
    /** 固定像素速度：播放器越宽滚动时间越长。 */
    val fixedVelocity: Boolean = false,
    /** 顶部 / 底部弹幕停留的时间，2~15 秒。 */
    val fixedSeconds: Float = 4f,
    /** 顶部 / 底部弹幕改成滚动弹幕。 */
    val staticToScroll: Boolean = false,
    val allowScroll: Boolean = true,
    val allowTop: Boolean = true,
    val allowBottom: Boolean = true,
    /** 显示彩色弹幕（颜色不是纯白的）。 */
    val allowColorful: Boolean = true,
    /** 智能云屏蔽等级，0~10。 */
    val weightLevel: Int = 0,
    /** 合并重复弹幕。 */
    val mergeDuplicates: Boolean = true,
    /** 合并的时间窗口，100~3000 毫秒。 */
    val mergeWindowMs: Int = 500,
    /** 一批至少这么多条才在合并后的弹幕后面标 "x3"（2~10）。 */
    val mergeThreshold: Int = 2,
    /** 本地屏蔽词，只存在这台机器上（写法见 [RuleKind]）。 */
    val blockRules: List<String> = emptyList(),
) {
    @kotlinx.serialization.Transient
    @Volatile
    private var cachedFilter: DanmakuFilter? = null

    @kotlinx.serialization.Transient
    @Volatile
    private var cachedLayout: LayoutCacheEntry? = null

    private class LayoutCacheEntry(
        val viewWidth: Float,
        val viewHeight: Float,
        val density: Float,
        val layout: DanmakuLayout,
    )

    /** 过滤用的那几项（**换算在 Rust**，同实例缓存免逐帧过 FFI）。 */
    val filter: DanmakuFilter
        get() = cachedFilter ?: RustDanmaku.filterOf(this).also { cachedFilter = it }

    /** 排布要用的那几项换算成像素（**换算在 Rust**，尺寸与密度未变时直接命中缓存）。 */
    fun layout(viewWidth: Float, viewHeight: Float, density: Float): DanmakuLayout {
        val hit = cachedLayout
        if (hit != null && hit.viewWidth == viewWidth && hit.viewHeight == viewHeight && hit.density == density) {
            return hit.layout
        }
        return RustDanmaku.layout(this, viewWidth, viewHeight, density).also {
            cachedLayout = LayoutCacheEntry(viewWidth, viewHeight, density, it)
        }
    }

    /** 把超出范围的值收回范围里；屏蔽词去掉空白、重复和过长的（**规则在 Rust**）。 */
    fun normalized(): DanmakuSettings = RustDanmaku.normalized(this)

    /** 恢复默认，但屏蔽词是用户自己攒的，留着。 */
    fun reset(): DanmakuSettings = RustDanmaku.reset(this)

    companion object {
        /** 透明度范围。 */
        val OPACITY = 0.3f..1f
        val FONT_SCALE = 0.5f..2f
        val FONT_WEIGHT = 1..9
        val STROKE_WIDTH = 0f..4f
        val LINE_HEIGHT = 0.8f..2.2f
        val SECONDS = 2f..15f
        val WEIGHT_LEVEL = 0..10
        val MERGE_WINDOW_MS = 100..3000
        val MERGE_THRESHOLD = 2..10

        /** 显示区域的八档：1/8、1/4 …… 全屏（真值在 Rust）。 */
        val AREA_OPTIONS: List<Float> by lazy { RustDanmaku.areaOptions() }

        /** 屏蔽词最多存多少条（真值在 Rust）。 */
        val MAX_RULES: Int by lazy { RustDanmaku.maxRules() }

        /** 一条屏蔽词最长多少字（真值在 Rust）。 */
        val MAX_RULE_LENGTH: Int by lazy { RustDanmaku.maxRuleLength() }

        /** 离 [value] 最近的一档显示区域（真值在 Rust）。 */
        fun nearestArea(value: Float): Float = RustDanmaku.nearestArea(value)

        /** 海量模式下的行高（真值在 Rust）。 */
        fun massiveLineHeight(lineHeight: Float): Float = RustDanmaku.massiveLineHeight(lineHeight)

        /** 旧版"弹幕密度"（0~100）→ 新设置（真值在 Rust）。 */
        fun fromLegacyDensity(density: Int): DanmakuSettings = RustDanmaku.fromLegacyDensity(density)
    }
}

/** 影响轨道排布的那几项设置，已换算成像素（Rust `danmaku::Layout`）。 */
data class DanmakuLayout(
    val fontPx: Float,
    val fontWeight: Int,
    val laneHeight: Float,
    val area: Float,
    val scrollMs: Long,
    val fixedMs: Long,
    val reserveTop: Boolean = true,
    val reserveBottom: Boolean = true,
)

/** 排布表里的一条。 */
data class PlannedDanmaku(
    val id: ULong,
    val mode: DanmakuMode,
    val lane: Int,
    val startMs: Long,
    val durationMs: Long,
    val width: Float,
    val viewWidth: Float,
) {
    val endMs: Long get() = startMs + durationMs

    /** 滚动弹幕的速度：像素 / 毫秒。 */
    val speed: Float get() = (viewWidth + width) / durationMs

    /**
     * 滚动弹幕此刻左边缘的 x；顶部 / 底部弹幕居中。**每帧每条调一次**，不过 FFI——Rust 那边是同一个
     * 公式（`Planned::x`），由 `DanmakuDualRunTest` 逐个位置比。
     */
    fun x(nowMs: Long, viewWidth: Float): Float = when (mode) {
        DanmakuMode.SCROLL -> viewWidth - (viewWidth + width) * (nowMs - startMs) / durationMs
        else -> (viewWidth - width) / 2
    }
}

/** 一次排布过 FFI 的整批。 */
data class DanmakuPlan(
    val items: List<PlannedDanmaku>,
    val viewWidth: Float,
    val viewHeight: Float,
    val laneHeight: Float,
    val usableLanes: Int,
    val scrollLanes: Int,
    val topFixedLanes: Int,
    val bottomFixedLanes: Int,
    /** 下一次该来问的时刻（null = 后面什么都没有了）。 */
    val nextMs: Long?,
) {
    /** 弹幕上边缘的 y（每帧算一次，纯算术）。 */
    fun y(item: PlannedDanmaku): Float = when (item.mode) {
        DanmakuMode.BOTTOM -> (usableLanes - item.lane - 1) * laneHeight
        DanmakuMode.SCROLL -> (topFixedLanes + item.lane) * laneHeight
        else -> item.lane * laneHeight
    }

    companion object {
        val EMPTY = DanmakuPlan(emptyList(), 0f, 0f, 0f, 0, 0, 0, 0, null)
    }
}

/** 弹幕的默认值（真值在 Rust；这几个是编译期常量，改要先改 Rust）。 */
object DanmakuDefaults {
    /** 滚动弹幕从出现到完全消失的时间（正常速度下，视频时间）。 */
    const val SCROLL_MS: Long = 8000

    /** 顶部 / 底部弹幕停留的时间。 */
    const val FIXED_MS: Long = 4000

    /** 同一轨道上前后两条滚动弹幕之间至少留的空隙（像素）。 */
    const val LANE_GAP_PX: Float = 16f

    /** 播放进度往前跳超过这么多毫秒，就当成"跳转"，重新对齐。 */
    const val JUMP_MS: Long = 1500

    /** 倍速 [speed] 下滚动弹幕在视频时间里走完全程要多久（真值在 Rust）。 */
    fun scrollMs(speed: Float, baseMs: Long = SCROLL_MS): Long = RustDanmaku.scrollMs(speed, baseMs)

    /** "固定像素速度"的宽度系数（真值在 Rust）。 */
    fun fixedVelocityFactor(widthDp: Float): Float = RustDanmaku.fixedVelocityFactor(widthDp)
}

// ===========================================================================
// 播放状态机
// ===========================================================================

/** 宿主要做的一件事（Rust `session::Effect`）。 */
sealed interface PlayEffect {
    data object Play : PlayEffect
    data object Pause : PlayEffect
    data object Replay : PlayEffect
    data class Seek(val positionMs: Long) : PlayEffect
    data class SetRate(val rate: Float) : PlayEffect
    data class SaveProgress(val positionMs: Long, val durationMs: Long?) : PlayEffect

    /** 记"上次看到第几 P"（第一 P 是 null）。 */
    data class SavePart(val partId: String?) : PlayEffect
}

/** 播放器此刻的状态（`PlayerState` 里那几项）。 */
data class PlaybackSnapshot(
    val playing: Boolean = false,
    val buffering: Boolean = false,
    val positionMs: Long = 0,
    val durationMs: Long = 0,
    val ended: Boolean = false,
    val speed: Float = 1f,
    val muted: Boolean = false,
    val volume: Int = 100,
)

/** 取流失败的三种（宿主把异常映射成这个，见 [PlayFailure.of]）。 */
sealed interface PlayFailure {
    /** 登录能解决（平台要求登录 / "确认不是机器人"没登录时也归到这一类）。 */
    data object NotLoggedIn : PlayFailure

    /** 风控：**不重试**，让用户自己决定什么时候再来。 */
    data object RiskControl : PlayFailure

    /** 其它（拼一句 `取不到播放地址：…`）。 */
    data class Other(val message: String) : PlayFailure

    companion object {
        /** 把现有异常映射成 Rust 那一套分类（风控红线：只分类，不重试、不优化）。 */
        fun of(e: Throwable): PlayFailure = when (e) {
            is feedhub.core.NotLoggedInException -> NotLoggedIn
            is feedhub.core.RiskControlException -> RiskControl
            else -> Other(e.message ?: e::class.simpleName ?: "未知错误")
        }
    }
}

/** 取流失败要显示的文案。 */
data class FailureText(val needsLogin: Boolean, val error: String, val hint: String?)

// ===========================================================================
// 落盘 / 中转
// ===========================================================================

/** 账本落盘要做的动作。 */
sealed interface LedgerPersist {
    /** 删掉 `app.decodeLedger` 这个键。 */
    data object Delete : LedgerPersist

    /** 写这份内容。 */
    data class Write(val text: String) : LedgerPersist
}

/** 账本界面那一行的一格。 */
data class LedgerRow(
    val codec: VideoCodec,
    val cap: Long,
    val label: String,
    val measuredAtMs: Long?,
)

/** 中转路由：宿主照做。 */
sealed interface RelayRoute {
    data object NotFound : RelayRoute
    data object BadTemplate : RelayRoute
    data class Body(val body: String, val contentType: String?) : RelayRoute
    data class RemoteManifest(val url: String, val dash: Boolean) : RelayRoute
    data class Upstream(val url: String) : RelayRoute
}
