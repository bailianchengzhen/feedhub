package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiParam
import uniffi.feedhub_ffi.FfiTiebaFeed
import uniffi.feedhub_ffi.tiebaFollowedForums
import uniffi.feedhub_ffi.tiebaParseComments
import uniffi.feedhub_ffi.tiebaParseDetail
import uniffi.feedhub_ffi.tiebaParseFrsPage
import uniffi.feedhub_ffi.tiebaParsePersonalized
import uniffi.feedhub_ffi.tiebaParseReplies
import uniffi.feedhub_ffi.tiebaParseSearchForum
import uniffi.feedhub_ffi.tiebaParseSearchGlobal
import uniffi.feedhub_ffi.tiebaSign
import uniffi.feedhub_ffi.tiebaSigned
import uniffi.feedhub_ffi.tiebaVersion

/**
 * 贴吧的 Rust 门面：**只暴露 `feedhub.core` 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 [RustBili] 的分工一样，只有解析和签名；HTTP / Cookie / 代理 / 限速还在 `source-tieba` 的 OkHttp 里，
 * 一个字节都没变。区别是**平台错误码不在这里归类**：贴吧的归类规则（未登录 / 风控 / 签名不对）在
 * `TiebaSource.mapError`，那边还有搜索 / 关注管理等别的调用方，所以这里原样把
 * `errorCode` / `userMsg` / `errorMsg` 带回去，由接缝层按同一份规则翻成异常。
 */
object RustTieba {
    /** 解析出来的一页 + 平台错误码（`errorCode` 非 0 时 `page.items` 是空的）。 */
    class Feed(
        val page: Page<FeedItem>,
        /** 吧内帖子列表用：这是不是这个吧的最后一页（推荐流恒为 false）。 */
        val last: Boolean,
        val errorCode: Int,
        val userMsg: String,
        val errorMsg: String,
    ) {
        /**
         * 平台错误的文案。规则和 Kotlin 那两处一致：
         * `check()` 是 `user_msg.ifBlank { error_msg }`，`FrsPager` 是 `user_msg.ifBlank { null } ?: error_msg`。
         */
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 关注的吧（`forumGuide` 的 `like_forum[]`）。 */
    class Forum(
        val id: Long,
        val name: String,
        val dayThreads: Int,
        val avatar: String?,
        val memberCount: Long?,
        val threadCount: Long?,
    )

    /**
     * 一份详情 + 平台错误。`error` 为 true 时 [detail] 是 null——**平台错误码的归类仍由调用方按
     * `TiebaSource.mapError` 做**（唯一的规则），这里只把码和两个文案带回来。
     */
    class Detail(
        val detail: FeedDetail?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        /** 文案规则和 Kotlin 的 `check()` 一致：`user_msg.ifBlank { error_msg }`。 */
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** 一页评论（一级评论 / 楼中楼）+ 平台错误。 */
    class Comments(
        val page: Page<Comment>?,
        val error: Boolean,
        val errorCode: Int,
        private val userMsg: String,
        private val errorMsg: String,
    ) {
        val errorMessage: String get() = if (userMsg.isBlank()) errorMsg else userMsg
    }

    /** `.so` 的版本（排障时看一眼：确认加载到的不是旧的 `.so`）。 */
    fun version(platform: Platform): String = RustModel.guard(platform) { tiebaVersion() }

    /** 推荐流一页（`pn` 是页码，下一页永远是 `pn + 1`）。 */
    fun parsePersonalized(bytes: ByteArray, pn: Int, platform: Platform): Feed =
        RustModel.guard(platform) { tiebaParsePersonalized(bytes, pn).toFeed(platform) }

    /**
     * 纯函数入口：全吧搜索的响应 → 一页（平台错误码当数据带回去，归类仍走 `TiebaSource.mapError`）。
     *
     * 生产路径走 `RustTiebaHosted.search`（请求 + 解析都在 Rust）；这个只给对拍 / 排障用。
     */
    fun parseSearchGlobal(body: String, platform: Platform): Feed =
        RustModel.guard(platform) { tiebaParseSearchGlobal(body).toFeed(platform) }

    /** 纯函数入口：吧内搜索的响应 → 一页（同上，`pn` 决定下一页游标）。 */
    fun parseSearchForum(body: String, pn: Int, platform: Platform): Feed =
        RustModel.guard(platform) { tiebaParseSearchForum(body, pn).toFeed(platform) }

    /** 吧内帖子列表一页（关注流里某个吧的缓冲）。 */
    fun parseFrsPage(bytes: ByteArray, forumName: String, platform: Platform): Feed =
        RustModel.guard(platform) { tiebaParseFrsPage(bytes, forumName).toFeed(platform) }

    /** 帖子详情（`pb/page` 第一页）。[loggedIn] 决定"是否已关注这个吧"能不能报。 */
    fun parseDetail(bytes: ByteArray, tid: String, loggedIn: Boolean, platform: Platform): Detail =
        RustModel.guard(platform) {
            val ffi = tiebaParseDetail(bytes, tid, loggedIn)
            Detail(
                detail = ffi.detail?.let { RustModel.toDetail(it, platform) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }

    /** 一页楼层（`pb/page` 的响应）。[requestedPage] 是请求的页码（游标）。 */
    fun parseComments(bytes: ByteArray, requestedPage: Int, platform: Platform): Comments =
        RustModel.guard(platform) {
            val ffi = tiebaParseComments(bytes, requestedPage)
            Comments(
                page = ffi.page?.let { RustModel.toCommentPage(it, platform) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }

    /** 一页楼中楼（`pb/floor` 的响应）。 */
    fun parseReplies(bytes: ByteArray, requestedPage: Int, platform: Platform): Comments =
        RustModel.guard(platform) {
            val ffi = tiebaParseReplies(bytes, requestedPage)
            Comments(
                page = ffi.page?.let { RustModel.toCommentPage(it, platform) },
                error = ffi.error,
                errorCode = ffi.errorCode,
                userMsg = ffi.userMsg,
                errorMsg = ffi.errorMsg,
            )
        }

    /** 关注的吧。 */
    fun followedForums(body: String, platform: Platform): List<Forum> = RustModel.guard(platform) {
        tiebaFollowedForums(body).map {
            Forum(it.id, it.name, it.dayThreads, it.avatar, it.memberCount, it.threadCount)
        }
    }

    /** 签名（返回 md5 小写十六进制，不含参数排序）。 */
    fun sign(params: List<Pair<String, String>>, salt: String, platform: Platform): String =
        RustModel.guard(platform) {
            tiebaSign(params.map { FfiParam(it.first, it.second) }, salt)
        }

    /**
     * 去掉 `sign`、排好序、末尾补上 `sign`。返回**保持顺序**的参数表：
     * 调用方按顺序拼表单 / URL，顺序变了请求字节就变了。
     */
    fun signed(params: List<Pair<String, String>>, salt: String, platform: Platform): List<Pair<String, String>> =
        RustModel.guard(platform) {
            tiebaSigned(params.map { FfiParam(it.first, it.second) }, salt).map { it.name to it.value }
        }

    /** 搜索那条路返回的是 `FfiTiebaHostedFeed`（多一个 `error` 布尔，因为错误外壳里 `error_code` 可能是 0）。 */
    private fun uniffi.feedhub_ffi.FfiTiebaHostedFeed.toFeed(platform: Platform): Feed = Feed(
        page = RustModel.toPage(page, platform),
        last = last,
        // 归类的调用方要的是"码"，`error` 只是"要不要归类"——这两条路在 `errorCode != 0` 时等价，
        // 纯函数入口只会拿到带码的那一支；`errorCode == 0` 时的错误外壳在这里给 -1（和 Kotlin 的 `?-1` 一样）。
        errorCode = if (error) errorCode else 0,
        userMsg = userMsg,
        errorMsg = errorMsg,
    )

    private fun FfiTiebaFeed.toFeed(platform: Platform): Feed = Feed(
        page = RustModel.toPage(page, platform),
        last = last,
        errorCode = errorCode,
        userMsg = userMsg,
        errorMsg = errorMsg,
    )
}
