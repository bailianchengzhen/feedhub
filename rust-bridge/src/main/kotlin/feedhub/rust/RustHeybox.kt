package feedhub.rust

import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiEmote
import uniffi.feedhub_ffi.FfiHeyboxGameInfo
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.heyboxHkey
import uniffi.feedhub_ffi.heyboxNormalizePath
import uniffi.feedhub_ffi.heyboxParseComments
import uniffi.feedhub_ffi.heyboxParseDetail
import uniffi.feedhub_ffi.heyboxParseFeeds
import uniffi.feedhub_ffi.heyboxParseReplies
import uniffi.feedhub_ffi.heyboxSignedParams

/**
 * 小黑盒的 Rust 门面：只把 [feedhub.core] 的类型交给业务代码，`Ffi*` 一律不漏出去。
 *
 * 和 [RustBili] 一样的三件事：把 `FfiException` 映射回现有异常、把 `FfiPage` / `FfiDetail` /
 * `FfiCommentPage` 还原成 [Page] / [FeedDetail] / [Page]<[Comment]>（共用 [RustModel] 那一份映射，
 * 不再自己写一套）、加载失败时说清楚（**不悄悄退回 Kotlin 实现**）。
 *
 * 不再有"回退 Kotlin"那条路）；播放 / 扫码 / whoami 还是 Kotlin。
 */
object RustHeybox {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("heybox") { coreVersion() }

    fun available(): Boolean = try {
        RustNative.ensureLoaded()
        true
    } catch (_: Throwable) {
        false
    }

    /**
     * 解析推荐流。[emotes] 是表情表（`cube_喜欢` → 图片地址，**顺序即 Kotlin `LinkedHashMap` 的顺序**），
     * 空表就是这次没有表情表（卡片上的表情降级成文字）。
     */
    fun parseFeeds(
        body: String,
        offset: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(
            heyboxParseFeeds(body, offset, emotes.map { FfiEmote(it.key, it.value) }),
            platform,
        )
    }

    /**
     * 详情。正文（含文章 / 资讯的 HTML）、视频、游戏卡片全由 Rust 解析，返回的就是完整的
     * [FeedDetail]（没有"正文回退"这回事了）。[games] 是 `/game/get_game_infos/` 已经取到的卡片，
     * 没取到就传空（卡片降级成只有 id）。
     */
    fun parseDetail(
        body: String,
        emotes: Map<String, String>,
        games: List<RustHeyboxGame>,
        platform: Platform,
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(
            heyboxParseDetail(
                body,
                emotes.map { FfiEmote(it.key, it.value) },
                games.map { FfiHeyboxGameInfo(it.id, it.name, it.cover, it.subtitle, it.url) },
            ),
            platform,
        )
    }

    /** 一级评论（`page` 是这一页的页码，游标就是下一页的页码）。 */
    fun parseComments(
        body: String,
        page: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(
            heyboxParseComments(body, page, emotes.map { FfiEmote(it.key, it.value) }),
            platform,
        )
    }

    /** 楼中楼（[from] 是上一页的游标，用来判定"游标没推进"）。 */
    fun parseReplies(
        body: String,
        rootId: String,
        emotes: Map<String, String>,
        from: Cursor?,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(
            heyboxParseReplies(body, rootId, emotes.map { FfiEmote(it.key, it.value) }, from?.value),
            platform,
        )
    }

    fun hkey(path: String, timestamp: Long, nonce: String): String = guard("heybox") {
        heyboxHkey(path, timestamp, nonce)
    }

    fun normalizePath(path: String): String = guard("heybox") { heyboxNormalizePath(path) }

    /** 要追加到 query 的三个签名参数，**保持这个顺序**（按顺序拼 query，请求字节才不变）。 */
    fun signedParams(path: String, timestamp: Long, nonce: String): List<Pair<String, String>> =
        guard("heybox") {
            heyboxSignedParams(path, timestamp, nonce).map { it.name to it.value }
        }

    // ---------------- 内部 ----------------

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /** 纯函数入口：加载 `.so` + 把 `FfiException` 折成现有异常（实现在 [RustModel.guard]）。 */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = RustModel.guard(platform, block)
}

/**
 * 游戏卡片（桥接用的小值类型）：和 `source-heybox` 的 `GameInfo` 一一对应。
 *
 * 为什么不在 `rust-bridge` 里直接用插件的 `GameInfo`：依赖方向是 `:source-heybox` → `:rust-bridge`，
 * 反过来引插件类型会成环。也不用 FFI 的 `FfiHeyboxGameInfo`：那个只在 [RustHeybox] 内部构造，
 * 业务代码看不见。
 */
class RustHeyboxGame(
    val id: String,
    val name: String,
    val cover: String?,
    val subtitle: String?,
    val url: String?,
)
