package feedhub.source.tieba

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.SearchRequest
import feedhub.core.Platform
import feedhub.core.SourceContext
import feedhub.rust.RustTiebaHosted

/**
 * 贴吧每一种请求的"拼 + 收 + 解析"都在 Rust 平台对象（[RustTiebaHosted]）里；这一层只把带回来的平台错误码
 * 按 [TiebaErrors] 归类成异常。[RustTiebaSource] 直接用它；测试里的 Kotlin 参照实现（`TiebaSource`）
 * 也通过这个接口接 Rust 版，好在同一套请求上对拍。
 */
internal interface TiebaHostedFetch {
    /** 推荐流一页（游标是页码，下一页由 Rust 算）。平台错误在这里按 [TiebaErrors.mapError] 抛。 */
    suspend fun recommend(cursor: Cursor?): Page<FeedItem>

    /** 关注的吧（`forumGuide`）。平台错误 / 未登录按原来的规则抛。 */
    suspend fun followedForums(): List<FollowedForum>

    /** 某个吧的一页。平台错误**不抛**，带回去由调用方归类（见 [FrsParsed]）。 */
    suspend fun frsPage(forumName: String, page: Int): FrsParsed

    /** 帖子详情。平台错误按 [TiebaErrors.mapError] 抛（和 Kotlin 版 `firstPage` 的 `check` 一样）。 */
    suspend fun detail(id: String): FeedDetail

    /** 一页楼层（二楼起）。游标是页码，平台错误按 [TiebaErrors.mapError] 抛。 */
    suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment>

    /** 某一楼的楼中楼。`commentId` 是楼层的 pid，游标是页码。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment>

    /**
     * 搜一页。
     *
     * `request.community` 决定走哪种：空 → 全吧搜索（网页 GET，PC salt）；
     * 给了吧名 → 吧内搜索（App 表单 POST，App salt）。平台错误按 [TiebaErrors.mapError] 抛。
     */
    suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem>

    /**
     * 播放地址（`partId=main` 或 `url:<mp4>`）。
     *
     * `main` 时 5 分钟内详情取过的第一页直接复用（**0 个请求**）；`url:<mp4>` 一个请求都不发；
     * `partId` 写法不对时 Rust 在发请求之前就报错。平台错误按 [TiebaErrors.mapError] 抛；
     * 没有平台错误、也没有播放信息 = 帖子里没有那个视频（调用方翻成 `ParseException`）。
     */
    suspend fun play(id: String, partId: String): PlayInfo?
}

/**
 * Rust 平台对象（[RustTiebaHosted]）的接缝实现。
 *
 * 归类的规则**只有一份**（[TiebaErrors.mapError]，搜索 / 关注管理也用它），所以这里只做"带回来的错误码
 * → 现有异常"这一步，一行新规则都不加：
 * - 推荐流：`error_code` 非 0 → 抛（和 `TiebaParse.personalized` 一样）；
 * - 关注的吧：`error_code` 非 0 → 抛；`is_login == 0` → [NotLoggedInException]（和 `fetchFollowedForums` 一样）；
 * - 吧内帖子：错误码**原样带回去**（"吧读不了"要包成 `ForumClosedException`，那是 `FrsPager` 的事）。
 *
 * 设备标识：Rust 只**读** `ctx.store` 里那三个键，所以第一次用之前先 [TiebaDevice.ensure] 一次
 * （和 Kotlin 版 `by lazy { TiebaDevice.load(ctx.store) }` 是同一件事，装一次就不变）。
 */
internal class RustTiebaHostedFetch(
    ctx: SourceContext,
    private val hosted: RustTiebaHosted,
    private val nowMs: () -> Long,
    private val platform: Platform = TIEBA,
) : TiebaHostedFetch {
    init {
        TiebaDevice.ensure(ctx.store)
    }

    override suspend fun recommend(cursor: Cursor?): Page<FeedItem> {
        val feed = hosted.recommend(cursor, nowMs())
        if (feed.error) throw TiebaErrors.mapError(feed.errorCode, feed.errorMessage)
        return feed.page
    }

    override suspend fun followedForums(): List<FollowedForum> {
        val forums = hosted.forumGuide()
        if (forums.error) throw TiebaErrors.mapError(forums.errorCode, forums.errorMsg)
        if (forums.isLogin == 0L) throw NotLoggedInException(platform)
        return forums.forums.map {
            FollowedForum(it.id, it.name, it.dayThreads, it.avatar, it.memberCount, it.threadCount)
        }
    }

    override suspend fun frsPage(forumName: String, page: Int): FrsParsed {
        val feed = hosted.frsPage(forumName, page, nowMs())
        if (feed.error) return FrsParsed(null, feed.errorCode, feed.errorMessage)
        return FrsParsed(ForumPage(feed.page.items, feed.last), 0, "")
    }

    // ---------- 详情 / 楼层 / 楼中楼 ----------

    override suspend fun detail(id: String): FeedDetail {
        val detail = hosted.detail(id, nowMs())
        if (detail.error) throw TiebaErrors.mapError(detail.errorCode, detail.errorMessage)
        return detail.detail ?: error("Rust 返回的详情既没有平台错误也没有内容")
    }

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        val page = hosted.comments(id, cursor, sort, nowMs())
        if (page.error) throw TiebaErrors.mapError(page.errorCode, page.errorMessage)
        return page.page ?: error("Rust 返回的评论页既没有平台错误也没有内容")
    }

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        val page = hosted.replies(id, commentId, cursor, nowMs())
        if (page.error) throw TiebaErrors.mapError(page.errorCode, page.errorMessage)
        return page.page ?: error("Rust 返回的楼中楼页既没有平台错误也没有内容")
    }

    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        val feed = hosted.search(request, cursor, nowMs())
        if (feed.error) throw TiebaErrors.mapError(feed.errorCode, feed.errorMessage)
        return feed.page
    }

    override suspend fun play(id: String, partId: String): PlayInfo? {
        val result = hosted.play(id, partId, nowMs())
        if (result.error) throw TiebaErrors.mapError(result.errorCode, result.errorMessage)
        return result.info
    }
}
