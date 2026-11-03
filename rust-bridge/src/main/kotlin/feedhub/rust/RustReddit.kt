package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiParam
import uniffi.feedhub_ffi.redditParseCommentThread
import uniffi.feedhub_ffi.redditParseComments
import uniffi.feedhub_ffi.redditParseDetail
import uniffi.feedhub_ffi.redditParseMoreChildren

/**
 * Reddit **纯函数**门面：给我一段 body，我给你详情 / 一页评论。
 *
 * 和 [RustRedditHosted]（平台对象，连请求一起管）的分工，和 [RustBili] 与 [RustBiliHosted] 一样：
 * - 这个类只做解析，**不发请求**——给对拍（`RedditDetailDualRunTest`）和测量
 *   （`RedditDetailRustBenchTest`）用；
 * - 生产路径走 [RustRedditHosted]（`RustRedditSource` 的三个方法），解析是**同一份** Rust 代码。
 *
 * 作者头像表是 `author_fullname` → 地址（Rust 那边要它才会填 `UserRef.avatar`）；
 * 过边界用 [FfiParam] 的 `name` / `value`，不额外造一个 record。
 */
object RustReddit {
    /** 平台 id 和 `RedditPlugin` 里那份同一个（`Platform.define` 幂等）。 */
    private val REDDIT: Platform = Platform.define("reddit", "Reddit")

    private fun Map<String, String>.toParams(): List<FfiParam> =
        map { (name, value) -> FfiParam(name, value) }

    /** `/comments/<id>.json` 里那个帖子（`[0].data.children[0].data`）→ 详情。 */
    fun parseDetail(
        post: String,
        avatars: Map<String, String> = emptyMap(),
        platform: Platform = REDDIT,
    ): FeedDetail = RustModel.guard(platform) {
        RustModel.toDetail(redditParseDetail(post, avatars.toParams()), platform)
    }

    /** 一个 Listing（评论区的那个）→ 一页一级评论。 */
    fun parseComments(
        listing: String,
        avatars: Map<String, String> = emptyMap(),
        platform: Platform = REDDIT,
    ): Page<Comment> = RustModel.guard(platform) {
        RustModel.toCommentPage(redditParseComments(listing, avatars.toParams()), platform)
    }

    /** `/comments/<postId>/_/<commentId>.json` 的响应（数组 / 单个 Listing 都认）→ 铺平的一层楼中楼。 */
    fun parseCommentThread(
        json: String,
        postId: String,
        commentId: String,
        avatars: Map<String, String> = emptyMap(),
        platform: Platform = REDDIT,
    ): Page<Comment> = RustModel.guard(platform) {
        RustModel.toCommentPage(
            redditParseCommentThread(json, postId, commentId, avatars.toParams()),
            platform,
        )
    }

    /** `/api/morechildren` 的响应 → 一页评论。 */
    fun parseMoreChildren(
        json: String,
        avatars: Map<String, String> = emptyMap(),
        platform: Platform = REDDIT,
    ): Page<Comment> = RustModel.guard(platform) {
        RustModel.toCommentPage(redditParseMoreChildren(json, avatars.toParams()), platform)
    }
}
