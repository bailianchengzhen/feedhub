package feedhub.source.xiaohongshu

import feedhub.core.Cursor

/**
 * 推荐流 / 关注流"**取一页**"的接缝：调度、缓存、表情表还在 [XhsSource] / [XhsFollowFeed] 里，
 * 这里只回答"这一个请求怎么发、响应怎么解析"。
 *
 * 两个实现：
 * - [KotlinXhsFeedApi]：`XhsApi.getRaw` / `postRaw` + 原来的 [XhsParse]；
 * - `HostedXhsFeedApi`（`XhsHosted.kt`）：Rust 平台对象（Rust 拼 URL / 参数 / 头 / 签名 / 判错 / 解析，
 *   收发回调宿主用 `XhsApi.http`）。
 *
 * 关注流的调度（谁有新笔记、关注列表 TTL、作者预算、存盘）一行没改，所以两种实现跑的是**同一份调度代码**，
 * 请求等价比的才是"同一个流程、两种请求实现"。
 */
internal interface XhsFeedApi {
    /** 推荐流一页。[nowMs] 是这一次请求的"现在"（签名和解析用同一份）。 */
    suspend fun homefeed(cursor: Cursor?, nowMs: Long): Homefeed

    /** 关注列表一页。[cursor] 是上一页响应的游标，第一页空串。 */
    suspend fun followings(selfId: String, cursor: String, nowMs: Long): XhsFollowParser.FollowingsPage

    /** 谁有新笔记（`followings/extra`，一批人一次）。 */
    suspend fun unread(selfId: String, userIds: List<String>, nowMs: Long): Map<String, Int>

    /** 一个作者的主页第一页（`user_posted`）。 */
    suspend fun posted(author: FollowedAuthor, nowMs: Long): XhsFollowParser.PostedPage
}

/**
 * Kotlin 参照实现：就是搬进 Rust 之前那几行（`XhsSource.recommend` / `XhsFollowFeed` 的请求 + 解析），
 * 一行语义都没改，只是挪到这个类里。签名 / 解析仍然走 [XhsParse]。
 */
internal class KotlinXhsFeedApi(
    private val api: XhsApi,
    private val parse: XhsParse,
) : XhsFeedApi {
    override suspend fun homefeed(cursor: Cursor?, nowMs: Long): Homefeed {
        val score = cursor?.value?.substringBefore('|').orEmpty()
        val index = cursor?.value?.substringAfter('|', "0")?.toIntOrNull() ?: 0
        val res = api.postRaw(HOMEFEED, homefeedBody(score, if (cursor == null) 1 else 3, index))
        return parse.homefeed(res, index, SOURCE_FEED, nowMs / 1000, emptyMap())
    }

    override suspend fun followings(
        selfId: String,
        cursor: String,
        nowMs: Long,
    ): XhsFollowParser.FollowingsPage = parse.followings(
        // 参数名和顺序照网页；order / source 网页给什么值没记下来，留空实测可用
        api.getRaw(
            FOLLOWINGS_SELF,
            listOf("user_id" to selfId, "cursor" to cursor, "order" to "", "tab_type" to "all", "source" to ""),
        ),
    )

    override suspend fun unread(selfId: String, userIds: List<String>, nowMs: Long): Map<String, Int> =
        parse.unread(
            api.getRaw(
                FOLLOWINGS_EXTRA,
                listOf(
                    "user_id" to selfId,
                    "following_user_id_list" to userIds.joinToString(","),
                    "tab_type" to "all",
                    "source" to "",
                ),
            ),
        )

    override suspend fun posted(author: FollowedAuthor, nowMs: Long): XhsFollowParser.PostedPage {
        val res = api.getRaw(
            USER_POSTED,
            listOf(
                "num" to POSTED_NUM.toString(),
                "cursor" to "",
                "user_id" to author.id,
                "image_formats" to IMAGE_FORMATS,
                "xsec_token" to author.xsecToken.orEmpty(),
                "xsec_source" to SOURCE_FOLLOWED,
            ),
        )
        return parse.posted(res, author, nowMs / 1000, emptyMap())
    }

    private companion object {
        const val HOMEFEED = "/api/sns/web/v1/homefeed"
        const val FOLLOWINGS_SELF = "/api/sns/web/v1/user/followings/self"
        const val FOLLOWINGS_EXTRA = "/api/sns/web/v1/user/followings/extra"
        const val USER_POSTED = "/api/sns/web/v1/user_posted"
        const val IMAGE_FORMATS = "jpg,webp,avif"

        /** `user_posted` 的 `num`（网页的值，一页 30 条，另外还有置顶的）。 */
        const val POSTED_NUM = 30

        /** 从关注列表进主页时网页用的 `xsec_source`。 */
        const val SOURCE_FOLLOWED = "pc_followed"
    }
}
