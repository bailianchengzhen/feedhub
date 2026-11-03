package feedhub.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * 统一信息流里的一条。各平台的 Source 负责把自己的响应映射成它。
 *
 * 字段原则：拿不到就是 null，不要编默认值（例如没有播放数就别填 0）；
 * 文本一律是纯文本：HTML 在映射时去掉，平台表情保留它的文字代码（`[doge]`），分享 / 复制 / 搜索 / 屏蔽词都用这个文字；
 * 信息流卡片想把表情画成图片，看 [emotes]。
 */
@Serializable
data class FeedItem(
    val platform: Platform,
    /** 平台内唯一的 id（B站视频用 bvid，动态用 id_str，知乎回答用回答 id……）。 */
    val id: String,
    val kind: ItemKind,
    val title: String? = null,
    /** 摘要或正文开头，纯文本。里面的表情还是文字代码，要画成图片的看 [emotes]。 */
    val summary: String? = null,
    /**
     * 平台自己给的语言（BCP 47，如 `zh` / `en`；X 的 `legacy.lang`）。翻译门控优先用它：
     * 平台已经判过语言，就不用本地模型再猜。没有这个字段的平台是 null，交给本地语言识别。
     */
    val language: String? = null,
    val author: UserRef? = null,
    /** 发布时间，Unix 秒。 */
    val publishedAt: Long? = null,
    /**
     * [publishedAt] 精确到哪一档（默认 [TimePrecision.SECOND]，即平台给了确切时间戳）。
     *
     * 有些平台只给相对文字（YouTube TV 的 tile 是"4 months ago"、B站动态是"3小时前"），换算出来的
     * Unix 秒里除了那一档以外都是假的（"4 months ago" 换算成 `now - 4×30 天`，它的"几号、几点"是取数据那一刻
     * 的）。所以展示时必须按这一档截断：**只给到天的不要显示钟点，只给到月 / 年的不要显示日**——
     * 具体见 [TimeText]。带默认值，旧 JSON 读回来是 [TimePrecision.SECOND]。
     */
    val publishedAtPrecision: TimePrecision = TimePrecision.SECOND,
    val media: List<Media> = emptyList(),
    val stats: Stats = Stats(),
    /** 平台给的推荐理由 / 动作说明，例如"已关注""投稿了视频""赞同了回答"。 */
    val reason: String? = null,
    /** 所属圈子：贴吧的吧名、小黑盒的社区、知乎的问题标题等，没有就 null。 */
    val community: String? = null,
    /**
     * 所属圈子的图标（Reddit 的版块图标：列表加 `sr_detail=1` 后每条帖子带的
     * `sr_detail.community_icon`，没有就退回 `icon_img`）。
     *
     * 信息流卡片优先画它、没有才画作者头像（Reddit 的列表 / 评论 JSON 不带作者头像，官方首页也是版块图标为主）。
     * 带默认值，收藏 / 历史里的旧 JSON 读回来是 null。
     */
    val communityIcon: String? = null,
    /** 转发 / 引用的原内容（B站转发动态、酷安转发等）。 */
    val repost: FeedItem? = null,
    /**
     * 卡片上的表情：[title] 和 [summary] 里**真实出现**的表情文字代码
     * （原样，带括号，如 `[doge]`、`#(笑眼)`）→ 图片地址，只收这两段文字里有的，不是平台的整张表情表。
     * 转发的原内容 [repost] 有它自己的 [emotes]。没有表情就是空表（序列化时不写出，收藏 / 历史里的旧数据没有这个字段，读回来就是空表）。
     * 图片地址可以是网络地址，也可以是应用自带图片的 [BundledEmote] 地址；`Span.Emote` 同理。
     * 文字本身不变：前端把这些代码画成行内小图（[Emotes.split]），认不得的代码、图片加载失败的照旧是文字。
     * 各平台用 [Emotes.used] 从自己的表情来源里挑出文字里真有的那几个。
     */
    val emotes: Map<String, String> = emptyMap(),
    /**
     * 平台自己把这条标成了 18+ / 敏感内容（Reddit 的 `over_18`、X 的 `possibly_sensitive` / 敏感媒体、
     * YouTube 的年龄限制、TikTok 的成人标记……）。**只表示平台标了，不是我们猜的**：不按关键词、不按图片分类、
     * 不按社区判断，那些留给以后的"外出模式"。各平台 Source 只负责把平台的标记映射到这里，
     * 拿不到（没这个字段、或者含义拿不准）就是 false / 不映射。
     *
     * 展示给用户前统一过 `feedhub.brain.AdultFilter`（信息流聚合层、收藏 / 历史查询、详情页直接打开都算），
     * 隐藏的条目不算曝光、也不进别的统计。详情页直接打开时只显示一行 `feedhub.brain.AdultFilter.NOTICE`。
     *
     * 字段带默认值、序列化时 false 不写出，所以加这个字段之前存进收藏 / 历史的旧 JSON 读回来是 false
     * （见 `FeedItemAdultCompatTest`）。
     */
    val adult: Boolean = false,
    /** 这条内容的官方电脑版网页地址（分享用，生成深链也从它推）。 */
    val url: String? = null,
    /** 点进去去哪。 */
    val detail: DetailTarget,
) {
    /** 跨平台唯一键。 */
    val key: String get() = "${platform.key}:$id"
}

/**
 * [FeedItem.publishedAt] 的精度档位（由粗到细）：平台只给到哪一档，展示就只显示到哪一档。
 * 见 [FeedItem.publishedAtPrecision] 和 [TimeText]。
 */
@Serializable
enum class TimePrecision {
    /** 确切时间戳（平台给了 Unix 秒 / 完整日期）。 */
    SECOND,

    /** 只到分钟（"5 minutes ago"）。 */
    MINUTE,

    /** 只到小时（"3 hours ago"）。 */
    HOUR,

    /** 只到天（"3 days ago"、"yesterday"）。 */
    DAY,

    /** 只到月（"4 months ago"）。 */
    MONTH,

    /** 只到年（"6 years ago"）。 */
    YEAR,
}

@Serializable
enum class ItemKind {    /** 视频（B站投稿、番剧等）。 */
    VIDEO,

    /** 长文：B站专栏、知乎文章。 */
    ARTICLE,

    /** 知乎回答。 */
    ANSWER,

    /** 知乎问题、小黑盒 / 酷安的提问帖。 */
    QUESTION,

    /** 帖子：贴吧主题帖、小黑盒帖子、酷安动态。 */
    POST,

    /** 短动态：B站图文 / 文字 / 转发动态、知乎想法。 */
    MOMENT,

    LIVE,
    OTHER,
}

@Serializable
data class UserRef(
    val platform: Platform,
    val id: String,
    val name: String,
    val avatar: String? = null,
    /** 个人主页网页地址。 */
    val url: String? = null,
)

@Serializable
sealed interface Media {
    @Serializable
    @SerialName("image")
    data class Image(val url: String, val width: Int? = null, val height: Int? = null) : Media

    @Serializable
    @SerialName("video")
    data class Video(
        val cover: String? = null,
        val durationSec: Int? = null,
        /** 封面（= 视频画面）的像素宽高，知道的话信息流卡片按这个比例画（小红书的视频多是竖屏）；不知道就 null，卡片按 16:10 画。 */
        val width: Int? = null,
        val height: Int? = null,
    ) : Media
}

/** 互动数据。缺失就是 null，不是 0。 */
@Serializable
data class Stats(
    val views: Long? = null,
    val likes: Long? = null,
    val comments: Long? = null,
    val shares: Long? = null,
    val danmaku: Long? = null,
    /** 收藏数。 */
    val favorites: Long? = null,
    /** B站投币数。 */
    val coins: Long? = null,
)

@Serializable
sealed interface DetailTarget {
    /**
     * 没有原生详情的内容（直播、B站番剧等）：前端只能给"在官方 App 打开"。
     */
    @Serializable
    @SerialName("web")
    data class Web(val url: String) : DetailTarget

    /**
     * 原生详情：把 [id] 交给 [NativeDetailSource.detail] / [NativeDetailSource.comments]。
     * [platform] 平时是 null（和当前内容同一个平台）；站外链接指向另一个平台的内容时给出那个平台
     * （例如酷安动态里的 B站视频链接 → `Native("BV1xx", Platform("bili"))`，按 id 引用、不依赖 B站插件），前端用那个平台的 Source 去开。
     */
    @Serializable
    @SerialName("native")
    data class Native(val id: String, val platform: Platform? = null) : DetailTarget
}

/**
 * 一页结果。[next] 是不透明游标：各平台的分页方式完全不同（页码、offset 字符串、lastval……），
 * 公共层只负责原样传回去。null 表示没有下一页。
 */
@Serializable
data class Page<T>(val items: List<T>, val next: Cursor?)

@Serializable
@JvmInline
value class Cursor(val value: String) {
    override fun toString(): String = value
}
