package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiEmote
import uniffi.feedhub_ffi.FfiItemNode
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.zhihuEncrypt
import uniffi.feedhub_ffi.zhihuMd5Hex
import uniffi.feedhub_ffi.zhihuParseComments
import uniffi.feedhub_ffi.zhihuParseDetail
import uniffi.feedhub_ffi.zhihuParseMoments
import uniffi.feedhub_ffi.zhihuParseRecommend
import uniffi.feedhub_ffi.zhihuParseSearch
import uniffi.feedhub_ffi.zhihuParseTarget
import uniffi.feedhub_ffi.zhihuZse96

/**
 * 知乎的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustBili` 同一套做法（错误映射 / "节点数组 + 下标"还原 / 加载失败说清楚），
 * 但**故意没有抽公共父类**：这块按平台各放一个文件，
 *
 * 知乎这边只有两个纯函数分类：推荐流 / 关注动态的解析、`x-zse-96` 签名。
 * HTTP 状态码和 `Location` 由调用方传进来（知乎的错误判断要看它们，见 `ZhihuParser.error`）。
 */
object RustZhihu {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("zhihu") { coreVersion() }

    fun parseRecommend(
        body: String,
        httpCode: Int,
        location: String?,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(zhihuParseRecommend(body, httpCode, location, emotes.toFfi()), platform)
    }

    fun parseMoments(
        body: String,
        httpCode: Int,
        location: String?,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(zhihuParseMoments(body, httpCode, location, emotes.toFfi()), platform)
    }

    /**
     * 纯函数入口：一处搜索响应的 body → 一页。
     *
     * 生产路径走 `RustZhihuHosted.search`（请求 + 解析都在 Rust）；这个只给对拍 / 排障用。
     * [emotes] 是**插件带的表情表**（占位符 → 资源地址，和卡片共用；正文那套 data URI 表情表不经过这里）。
     */
    fun parseSearch(body: String, emotes: Map<String, String>, platform: Platform): Page<FeedItem> =
        guard(platform) {
            RustModel.toPage(zhihuParseSearch(body, emotes.toFfi()), platform)
        }

    /**
     * 详情（回答 / 文章 / 想法 / 视频 / 问题）。`kind` 是 `answer` / `article` / `pin` / `zvideo` / `question`；
     * [emotes] 是**正文用**的表情表（想法正文里的 `[感谢]` 要它），[bundled] 是**条目用**的
     * （`ZhihuSticker.urls`，和推荐流同一份，`FeedItem.emotes` 用）。
     */
    fun parseDetail(
        kind: String,
        body: String,
        emotes: Map<String, String>,
        bundled: Map<String, String>,
        platform: Platform,
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(zhihuParseDetail(kind, body, emotes.toFfi(), bundled.toFfi()), platform)
    }

    /** 评论列表（`root_comment` / `child_comment`）：`emotes` 同上（评论正文里的表情图）。 */
    fun parseComments(body: String, emotes: Map<String, String>, platform: Platform): Page<Comment> =
        guard(platform) {
            RustModel.toCommentPage(zhihuParseComments(body, emotes.toFfi()), platform)
        }

    /** 一份不带外壳的 `target` 对象。 */
    fun parseTarget(body: String, emotes: Map<String, String>, platform: Platform): FeedItem? = guard(platform) {
        toItem(zhihuParseTarget(body, emotes.toFfi()), platform)
    }

    /** 完整的 `x-zse-96`。`seed` 由 Kotlin 生成（网页是 `Math.random() * 127`），测试里注入固定值。 */
    fun zse96(pathAndQuery: String, dc0: String, body: String?, seed: Int): String = guard("zhihu") {
        zhihuZse96(pathAndQuery, dc0, body, seed)
    }

    /** 不带 `2.0_` 前缀的密文（固定值测试用）。 */
    fun encrypt(input: String, seed: Int): String = guard("zhihu") { zhihuEncrypt(input, seed) }

    /** 小写十六进制 MD5（固定值测试用）。 */
    fun md5Hex(text: String): String = guard("zhihu") { zhihuMd5Hex(text) }

    // ---------------- 内部 ----------------

    private fun Map<String, String>.toFfi(): List<FfiEmote> = map { FfiEmote(it.key, it.value) }

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /** 纯函数入口：加载 `.so` + 把 `FfiException` 折成现有异常（实现在 [RustModel.guard]）。 */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = RustModel.guard(platform, block)

    private fun toItem(node: FfiItemNode, platform: Platform): FeedItem? {
        val root = node.root ?: return null
        return RustModel.buildItem(root, node.nodes, platform)
    }
}
