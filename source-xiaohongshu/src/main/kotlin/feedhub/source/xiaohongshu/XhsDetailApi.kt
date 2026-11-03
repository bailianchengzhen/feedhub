package feedhub.source.xiaohongshu

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.Page
import feedhub.core.anyString
import feedhub.core.parseJsonObject
import kotlinx.serialization.json.JsonObject

/**
 * **详情 / 评论 / 楼中楼"取一次"的接缝**。
 *
 * 两个实现：
 * - [KotlinXhsDetailApi]：现有的 `XhsSource.detail` / `comments` / `replies` 那几行（请求由 [XhsApi] 拼 /
 *   签 / 发，解析走 [XhsParse]），行为和搬进 Rust 之前**一模一样**，是对拍的参照；
 * - `HostedXhsDetailApi`（`XhsHosted.kt`）：Rust 平台对象拼 URL / body / 请求头 / 签名 / 判错 / 解析，
 *   宿主只负责发（`XhsApi.http` 那一个客户端）。
 *
 * 三条都是**用户正等着的请求**（`RequestPriority.INTERACTIVE`）：Kotlin 实现给 `XhsApi` 传
 * `priority = RequestPriority.INTERACTIVE`，Rust 实现在请求头里挂内部伪请求头、由宿主转成同一个 tag。
 *
 * 表情表**两个实现都不取**（不挡首屏）：调用方把"手上已有的那份"传进来（没有就空表），
 * 认不出的记号按原文显示，后台补取（`XhsSource.emotesForBody` 的时序一个字没改）。
 */
internal interface XhsDetailApi {
    /**
     * 笔记详情。[id] 是卡片 `detail` 里的 `<笔记 id>@<xsec_source>@<xsec_token>`。
     *
     * **缺 `xsec_token` 也照发**（和 Kotlin 版一字不差）：`detail` 本来就不校验 token，
     * 入口里带了空 token 时就把空串交给 `POST feed`，由平台回 `300031`「当前笔记暂时无法浏览」，
     * 用户看到的是平台给的原因。**只有评论 / 楼中楼**在入口里没有 token 时**发请求之前**报错
     * （Kotlin `requireToken` 就是这两条）。改动这里会让"结果 / 错误等价"两条门都对不上。
     */
    suspend fun detail(id: String, nowMs: Long): FeedDetail

    /** 一页一级评论。[sort] 收下但**不影响请求**（这家没有排序参数）。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, nowMs: Long): Page<Comment>

    /** 一页楼中楼。[commentId] 是一级评论的 id。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?, nowMs: Long): Page<Comment>
}

/**
 * Kotlin 参照实现：就是搬进 Rust 之前 `XhsSource` 里那三个方法（一行语义都没改，只是挪到这个类里）。
 *
 * 路由常量、请求体（`feedBody`）、参数顺序、`RequestPriority.INTERACTIVE`、错误映射都在这里照旧；
 * 表情表由调用方给（[emotes] 是一个返回"手上已有的表"的函数，**不在这里发请求**）。
 */
internal class KotlinXhsDetailApi(
    private val api: XhsApi,
    private val parse: XhsParse,
    /**
     * 手上已有的表情表（内存 → `ctx.store` 里一周内的那份），**不发请求**。没有就是空表。
     *
     * Rust 实现那边由 `RustXhsHosted.Detail.hasEmotes` 决定要不要后台补取；
     * 这里的参照实现自己扫。
     */
    private val emoteTable: () -> Map<String, String>,
    /** 后台补取表情表（不挡正文，失败就算了）。参照实现里只在"有记号、表又不在手上"时调。 */
    private val warmEmotes: () -> Unit,
    /** 拿到原始响应之后回调一次（`XhsSource` 用它回填 `recentNotes`，播放要复用）。 */
    private val rememberNote: (String, JsonObject) -> Unit = { _, _ -> },
) : XhsDetailApi {
    override suspend fun detail(id: String, nowMs: Long): FeedDetail {
        val ref = XhsParser.parseNativeId(id)
        val json = api.post(
            FEED,
            feedBody(ref.noteId, ref.xsecSource ?: SOURCE_FEED, ref.xsecToken.orEmpty()),
            priority = feedhub.core.RequestPriority.INTERACTIVE,
        )
        rememberNote(ref.noteId, json)
        val note = XhsNote.noteCard(json)
        return XhsNote.detail(note, ref, emotesForBody(XhsNote.texts(note)))
    }

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort, nowMs: Long): Page<Comment> {
        val ref = XhsParser.parseNativeId(id)
        val token = requireToken(ref)
        val json = api.get(
            COMMENT_PAGE,
            listOf(
                "note_id" to ref.noteId,
                "cursor" to cursor?.value.orEmpty(),
                "top_comment_id" to "",
                "image_formats" to IMAGE_FORMATS,
                "xsec_token" to token,
            ),
            priority = feedhub.core.RequestPriority.INTERACTIVE,
        )
        val page = XhsComments.page(json, emotesForBodyTree(json))
        return Page(page.items, page.next?.let(::Cursor))
    }

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?, nowMs: Long): Page<Comment> {
        val ref = XhsParser.parseNativeId(id)
        val token = requireToken(ref)
        val json = api.get(
            SUB_COMMENT_PAGE,
            listOf(
                "note_id" to ref.noteId,
                "root_comment_id" to commentId,
                "num" to "10",
                "cursor" to cursor?.value.orEmpty(),
                "image_formats" to IMAGE_FORMATS,
                "top_comment_id" to "",
                "xsec_token" to token,
            ),
            priority = feedhub.core.RequestPriority.INTERACTIVE,
        )
        val page = XhsComments.page(json, emotesForBodyTree(json))
        return Page(page.items, page.next?.let(::Cursor))
    }

    /**
     * 有记号而手上没有表就**后台**补取、这次先按原文显示。
     */
    private fun emotesForBody(text: String): Map<String, String> {
        if (!XhsBody.EMOTE_TOKEN.containsMatchIn(text)) return emptyMap()
        return emoteTable().ifEmpty { warmEmotes(); emptyMap() }
    }

    /** 同 [emotesForBody]，扫的是已经解析好的响应树（不把整棵树再 `toString()` 一遍）。 */
    private fun emotesForBodyTree(json: JsonObject): Map<String, String> {
        if (!json.anyString { XhsBody.EMOTE_TOKEN.containsMatchIn(it) }) return emptyMap()
        return emoteTable().ifEmpty { warmEmotes(); emptyMap() }
    }

    private fun requireToken(ref: XhsParser.NoteRef): String = ref.xsecToken
        ?: throw feedhub.core.ApiException(
            XHS, "xsec_token",
            "评论要带这篇笔记的 xsec_token，入口里没有（很久以前收藏的？），请从信息流重新打开",
        )
}
