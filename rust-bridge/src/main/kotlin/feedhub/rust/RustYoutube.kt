package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.youtubeParseComments
import uniffi.feedhub_ffi.youtubeParseCommentsToken
import uniffi.feedhub_ffi.youtubeParseDetail
import uniffi.feedhub_ffi.youtubeParseSearch

/**
 * YouTube 的 Rust 核心门面（**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码）。
 *
 * 和 `RustBili` / `RustTiktok` 同一套做法（错误映射 / "节点数组 + 下标"还原 / 加载失败说清楚），故意
 * **没有抽公共父类**：这块按平台各放一个文件，
 *
 * 这里只有**纯函数**：详情、评论、评论面板的 token。生产路径不经过它们
 * ——整套请求 + 解析在 [RustYoutubeHosted] 的那个平台对象里。
 */
object RustYoutube {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard(Platform(YOUTUBE)) { coreVersion() }

    /**
     * 详情解析：`player` 的 body +（可选）TV `next {videoId}` 的 body。
     *
     * 组合的就是 Kotlin `YoutubeSource.detail` 里那几步（`video` → `videoMeta` → `mergeMeta` →
     * `itemOf` / 正文 / `VideoInfo`）；年龄限制同样转成 `item.adult = true` 的隐藏详情。
     */
    fun parseDetail(
        playerBody: String,
        nextBody: String?,
        nowSec: Long,
        platform: Platform = Platform(YOUTUBE),
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(youtubeParseDetail(playerBody, nextBody, nowSec), platform)
    }

    /** 一页评论 / 楼中楼（`next {continuation}` 的 body）。[parentId] 是楼中楼要跳过的父评论。 */
    fun parseComments(
        body: String,
        nowSec: Long,
        parentId: String? = null,
        platform: Platform = Platform(YOUTUBE),
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(youtubeParseComments(body, nowSec, parentId), platform)
    }

    /**
     * 纯函数入口：一处搜索响应（WEB 客户端的形状）→ 一页。
     *
     * 生产路径走 `RustYoutubeHosted.search`（请求 + 解析都在 Rust）；这个只给对拍 / 排障用。
     * [cursor] 是这一页发出去的那个 `continuation`（用来跳过自己）；[nowSec] 给相对时间换算。
     */
    fun parseSearch(
        body: String,
        cursor: String?,
        nowSec: Long,
        platform: Platform = Platform(YOUTUBE),
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(youtubeParseSearch(body, cursor, nowSec), platform)
    }

    /** `next {videoId}` 的响应里评论面板的续页 token（按排序挑；拿不到就是 `null`）。 */
    fun parseCommentsToken(
        nextBody: String,
        sort: CommentSort,
        platform: Platform = Platform(YOUTUBE),
    ): String? = guard(platform) { youtubeParseCommentsToken(nextBody, sort.toFfi()) }

    // ---------------- 内部 ----------------

    /** 平台 id（Kotlin `Platform.define("yt", "YouTube")`：**是 `yt` 不是 `youtube`**）。 */
    private const val YOUTUBE = "yt"

    /**
     * 跨边界调用的统一出口：加载 `.so` + 把 `FfiException` 映射回现有异常（和平台对象共用
     * [HostedCall.guard]）。
     */
    internal inline fun <T> guard(platform: Platform, block: () -> T): T =
        HostedCall.guard(platform, block)
}
