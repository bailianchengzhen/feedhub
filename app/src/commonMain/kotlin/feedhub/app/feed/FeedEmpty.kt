package feedhub.app.feed

import feedhub.app.ContentType
import feedhub.app.feed.NETWORK_ERROR_TEXT
import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.present.RustPresent
import feedhub.present.RustPresent.FeedBlank
import feedhub.present.RustPresent.FeedEmptyAction
import feedhub.present.RustPresent.FeedEmptyButton
import feedhub.present.RustPresent.FeedEmptyCopy
import feedhub.present.RustPresent.FeedEmptyReason

/*
 * 空状态归因+「空白的时候该画什么」。
 *
 * 判定和文案都在 Rust**（`rust/crates/feedhub-core/src/present/rules.rs`）：
 * 这里只剩"把事实备齐"（谁点亮了、谁有这个流、谁被内容类型筛掉了、谁没登录、谁报错）和
 * "把决定搬回来"。文案里的平台名要按插件顺序排，所以 `lit` 是**有序的 (id, 显示名) 列表**——
 * 显示名是插件登记的（`Platform.label`），Rust 读不到插件注册表，这一格由宿主给。
 *
 * 类型是 `:rust-bridge` 里那几个（`feedhub.present.RustPresent.*`）的别名：界面和测试照旧按
 * `feedhub.app.feed.FeedEmptyReason` 引用。
 */

/** 空状态到底在说什么。 */
typealias FeedEmptyReason = RustPresent.FeedEmptyReason

/** 页面上空白的时候该画什么。 */
typealias FeedBlank = RustPresent.FeedBlank

/**
 * 空状态给的出口。**界面只把动作报出来**，真正去哪儿由 `App` 接（页签 / 设置 / 「我的 → 账号」）——
 * `FeedScreen` 是桌面和安卓共用的，它自己不长导航。
 */
typealias FeedEmptyAction = RustPresent.FeedEmptyAction

/** 空状态里的一个按钮：动作 + 文字（文字跟着原因走，同一个动作在两种情况里可以叫不一样）。 */
typealias FeedEmptyButton = RustPresent.FeedEmptyButton

/** 空状态要画的东西：一行标题、一句人话，最多两个按钮，另加可选的「重试」。 */
typealias FeedEmptyCopy = RustPresent.FeedEmptyCopy

/**
 * 空状态判定要用的事实（**纯数据**）。只有一处组装：`AppGraph.feedEmptyInputs`（不构造 Source，见那里）。
 *
 * 平台集合都按"插件顺序"排好传进来，文案里的名字顺序才稳定（`FeedEmptyTest` 断言的就是这个顺序）。
 */
data class FeedEmptyInputs(
    val kind: FeedKind = FeedKind.RECOMMEND,
    /** 当前内容类型（设置里「混合 / 只看视频 / 只看图文」）。 */
    val contentType: ContentType = ContentType.MIXED,
    /** 设置里点亮、而且这台设备上能用的平台。 */
    val lit: List<Platform> = emptyList(),
    /** [lit] 里插件声明支持 [kind] 的。 */
    val supportsFeed: Set<Platform> = emptySet(),
    /** [supportsFeed] 里被 [contentType] 筛掉的（只看视频不要放不了视频的，只看图文不要纯视频的）。 */
    val droppedByContentType: Set<Platform> = emptySet(),
    /** 这一刷**真的请求了**的平台（= [supportsFeed] 减掉 [droppedByContentType]）。 */
    val requested: Set<Platform> = emptySet(),
    /**
     * 已经知道**没登录 / 登录过期**的平台（账号页那份状态 + 插件描述的必填 Cookie 判出来的）。
     * 只有"请求了的全都在这个集合里"才说"要先登录"——判不准的平台（[loggedOut] 里没有的）不算。
     */
    val loggedOut: Set<Platform> = emptySet(),
    /** 这一刷平台**明确回了"没登录"**的（`NotLoggedInException`，见 `FeedState.notLoggedIn`）。 */
    val refused: Set<Platform> = emptySet(),
    /** 这一刷失败的平台和原因（`FeedState.errors`；和标题行那个"N 个平台加载失败"是同一份）。 */
    val errors: Map<Platform, String> = emptyMap(),
)

/**
 * [FeedBlank] 的判定：有内容 → 内容；还没加载过 / 正在刷新 → 加载中；否则 → 空状态。
 *
 * [loadedOnce] 就是 `FeedState.loadedOnce`，「加载过」= 第一次刷新的结果出来了（有内容 / 空 / 失败都算）
 * 或者冷启动摆出了上一批。**取消掉的那一次不算**（没有结论）。
 */
fun feedBlankFor(itemCount: Int, refreshing: Boolean, loadedOnce: Boolean): FeedBlank =
    RustPresent.feedBlank(itemCount, refreshing, loadedOnce)

/**
 * 文案里的平台名：按 [order]（插件顺序）排，最多列 [max] 家——再多就"…等 N 家"。
 * 360 宽的手机上正文一行放不下四五个平台名。规则在 Rust（`present::rules::names_of`）。
 */
internal fun namesOf(order: Collection<Platform>, picked: Collection<Platform>, max: Int = 3): String =
    RustPresent.namesOf(
        order.map { RustPresent.FeedEmptyPlatform(it.key, it.label) },
        picked.map { it.key },
        max,
    )

/**
 * 空状态怎么画（纯函数，实现在 Rust）。
 *
 * 原则：**别把"这家没有这个流"说成"去登录"**，也别在没有意义的时候给「重试」；
 * 文案短、说人话，每种情况给一个真能做的动作。
 */
fun feedEmptyFor(inputs: FeedEmptyInputs): FeedEmptyCopy = RustPresent.feedEmpty(
    RustPresent.FeedEmptyFacts(
        following = inputs.kind == FeedKind.FOLLOWING,
        contentType = when (inputs.contentType) {
            ContentType.MIXED -> RustPresent.FeedContentType.MIXED
            ContentType.VIDEO -> RustPresent.FeedContentType.VIDEO
            ContentType.TEXT -> RustPresent.FeedContentType.TEXT
        },
        // 名字来自插件登记表（`Platform.label`）：Rust 读不到它，这一格只能由宿主给。
        lit = inputs.lit.map { RustPresent.FeedEmptyPlatform(it.key, it.label) },
        supportsFeed = inputs.supportsFeed.map { it.key },
        droppedByContentType = inputs.droppedByContentType.map { it.key },
        requested = inputs.requested.map { it.key },
        loggedOut = inputs.loggedOut.map { it.key },
        refused = inputs.refused.map { it.key },
        // "失败原因是不是网络错误"的判据只有一处（标题行那条"没有网络"用的同一个常量）
        errors = inputs.errors.map { (platform, message) ->
            RustPresent.FeedErrorFact(
                platform = platform.key,
                networkError = message == NETWORK_ERROR_TEXT,
                refused = platform in inputs.refused,
            )
        },
    ),
)
