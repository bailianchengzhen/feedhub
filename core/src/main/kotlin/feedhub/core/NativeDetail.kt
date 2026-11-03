package feedhub.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * 原生详情页的数据：正文、评论、楼中楼。
 *
 * `id` 一律是 [DetailTarget.Native.id]（不一定等于 [FeedItem.id]），三个方法用同一个 id。
 * id 里要带够信息让 Source 不查表就知道是什么（例如知乎 `answer:123`、B站 `BV1xx` / 动态 id），
 * 实在要先查一次（比如 B站动态的评论区 oid）就在 Source 里缓存。
 */
interface NativeDetailSource : Source {
    suspend fun detail(id: String): FeedDetail

    /** 一级评论，每条可带几条楼中楼预览。贴吧的"楼层"也走这里（一楼是正文，不在评论里）。 */
    suspend fun comments(id: String, cursor: Cursor? = null, sort: CommentSort = CommentSort.DEFAULT): Page<Comment>

    /** 某条一级评论下的楼中楼。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor? = null): Page<Comment>
}

/**
 * 评论排序。[DEFAULT] 是平台自己的默认顺序（B站、知乎是热门，贴吧是楼层正序，酷安是最近回复）；
 * 平台不支持的排序按 [DEFAULT] 处理，不报错。
 */
@Serializable
enum class CommentSort { DEFAULT, HOT, NEWEST }

@Serializable
data class FeedDetail(
    /** 详情接口拿到的完整信息（标题、作者、计数通常比信息流里的全）。 */
    val item: FeedItem,
    /** 正文。视频是简介。 */
    val body: RichText,
    /** 视频才有：分 P 列表，播放和弹幕用 [VideoPart.id]。 */
    val video: VideoInfo? = null,
    /** 页面上"关注"按钮对应的对象和当前状态（B站是 UP 主，贴吧是吧）；不支持关注的平台是 null。 */
    val follow: FollowState? = null,
    /** 相关推荐（B站视频下面那一列），没有就空。 */
    val related: List<FeedItem> = emptyList(),
)

@Serializable
data class Comment(
    val platform: Platform,
    val id: String,
    val author: UserRef? = null,
    val content: RichText,
    val publishedAt: Long? = null,
    /**
     * [publishedAt] 精确到哪一档（默认 [TimePrecision.SECOND]）。YouTube TV 的评论只给"6 years ago"这类
     * 相对文字，换算出的秒数里"几月几号几点"都是取数据那一刻的，展示要按这一档截断（见 [TimeText]）。
     */
    val publishedAtPrecision: TimePrecision = TimePrecision.SECOND,
    val likes: Long? = null,
    /** 楼中楼总数。 */
    val replyCount: Long? = null,
    /** 回复的是谁（楼中楼里"回复 @xxx"）。 */
    val replyTo: UserRef? = null,
    val ipLocation: String? = null,
    /** 楼层号（贴吧"第 N 楼"），没有就 null。 */
    val floor: Int? = null,
    /** 置顶评论。 */
    val pinned: Boolean = false,
    val previewReplies: List<Comment> = emptyList(),
    /** 平台自己给的语言（BCP 47），和 [FeedItem.language] 一样用于翻译门控；没有就 null。 */
    val language: String? = null,
)

/**
 * 平台无关的富文本。前端只认这几种块，平台特有的格式在映射时降级成它们：
 * 列表项变成带 "• " / "1. " 前缀的段落，表格变成逐行文字，投票 / 商品 / 抽奖之类的附加卡片变成 [Block.Card] 或文字。
 * HTML 正文用 [HtmlRichText] 转。
 */
@Serializable
data class RichText(val blocks: List<Block>) {
    /** 拼成纯文本，调试和 cli 输出用。 */
    fun plainText(): String = blocks.joinToString("\n") { textOf(it) }

    /**
     * 正文里的文字，但不把 [Block.Image] / [Block.Video] 降级成 "[图片]" / "[视频]" 占位符。
     * 信息流摘要用这个：卡片自己会把 [FeedItem.media] 画出来，摘要里再出现一次占位符就是重复。
     */
    fun textOnly(): String = blocks
        .filterNot { it is Block.Image || it is Block.Video }
        .joinToString("\n") { textOf(it) }

    private fun textOf(block: Block): String = when (block) {
        is Block.Paragraph -> block.spans.joinToString("") { it.text }
        is Block.Heading -> block.spans.joinToString("") { it.text }
        is Block.Quote -> "> " + block.spans.joinToString("") { it.text }
        is Block.Code -> block.text
        is Block.Image -> "[图片]"
        is Block.Card -> "[${block.title}]"
        is Block.Video -> "[视频]"
        Block.Divider -> "----"
    }

    fun isEmpty(): Boolean = blocks.isEmpty()

    companion object {
        val EMPTY = RichText(emptyList())

        fun ofText(text: String) = RichText(listOf(Block.Paragraph(listOf(Span.Text(text)))))
    }
}

@Serializable
sealed interface Block {
    @Serializable
    @SerialName("p")
    data class Paragraph(val spans: List<Span>) : Block

    /** 小标题（知乎、专栏正文里的 h1~h6 都降成它）。 */
    @Serializable
    @SerialName("h")
    data class Heading(val spans: List<Span>) : Block

    /** 引用块，多段引用就是连续几个 Quote。 */
    @Serializable
    @SerialName("quote")
    data class Quote(val spans: List<Span>) : Block

    @Serializable
    @SerialName("code")
    data class Code(val text: String, val language: String? = null) : Block

    /**
     * 图片。[url] 是正常显示用的地址；[original] 是原图（和 [url] 不同时才给），设置里"默认显示原图"时用它。
     * GIF 也是图片。
     */
    @Serializable
    @SerialName("img")
    data class Image(
        val url: String,
        val width: Int? = null,
        val height: Int? = null,
        val original: String? = null,
        val caption: String? = null,
    ) : Block

    /**
     * 卡片：正文里嵌的视频、链接卡片、引用的另一条内容、投票等。
     * [target] 是能原生打开的站内内容（例如知乎回答里嵌的 B站视频不算，嵌的知乎文章算）；打不开就只给 [url]。
     */
    @Serializable
    @SerialName("card")
    data class Card(
        val title: String,
        val subtitle: String? = null,
        val cover: String? = null,
        val url: String? = null,
        val target: DetailTarget? = null,
    ) : Block

    /**
     * 正文里夹着的视频（贴吧楼层里的、小黑盒图文里的、知乎回答里的），前端原地播放：先显示封面，点了才开始加载。
     * 不放播放地址（会过期）：点播放时前端用 [id] / [partId] 调 [VideoSource.play] 现取，所以它们的写法由各平台约定
     * （见 [VideoSource]）。整条就是视频的帖子不用这个，走 [FeedDetail.video]。
     * 新加的类型：旧数据里没有它，旧版本读不到它——现在没有任何地方把 [RichText] 存档（收藏 / 历史存的是 [FeedItem]）。
     */
    @Serializable
    @SerialName("video")
    data class Video(
        /** 交给 [VideoSource.play] 的 id。 */
        val id: String,
        /** 交给 [VideoSource.play] 的 partId。 */
        val partId: String,
        val cover: String? = null,
        val durationSec: Int? = null,
        val width: Int? = null,
        val height: Int? = null,
        val title: String? = null,
        /** 官方网页地址，播不了时用来复制链接。 */
        val url: String? = null,
    ) : Block

    @Serializable
    @SerialName("hr")
    data object Divider : Block
}

@Serializable
sealed interface Span {
    val text: String

    @Serializable
    @SerialName("text")
    data class Text(override val text: String, val bold: Boolean = false, val italic: Boolean = false) : Span

    /** 链接。站内内容（话题、另一条帖子）能原生打开的给 [target]。 */
    @Serializable
    @SerialName("link")
    data class Link(override val text: String, val url: String, val target: DetailTarget? = null) : Span

    @Serializable
    @SerialName("mention")
    data class Mention(override val text: String, val user: UserRef) : Span

    /**
     * 平台表情，[text] 是它的文字代码（如 "[doge]"），图片加载不出来时显示它。[url] 是图片地址；
     * 图片随应用带的（酷安）用 [BundledEmote] 的 `feedhub-emoji:` 地址，由前端的图片加载器从应用资源里取。
     */
    @Serializable
    @SerialName("emote")
    data class Emote(override val text: String, val url: String) : Span
}
