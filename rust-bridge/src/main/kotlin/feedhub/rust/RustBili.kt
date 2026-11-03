package feedhub.rust

import feedhub.core.Comment
import feedhub.core.Danmaku
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import uniffi.feedhub_ffi.FfiItemNode
import uniffi.feedhub_ffi.FfiParam
import uniffi.feedhub_ffi.biliAvToBv
import uniffi.feedhub_ffi.biliBvToAv
import uniffi.feedhub_ffi.biliCommentTarget
import uniffi.feedhub_ffi.biliIsBvid
import uniffi.feedhub_ffi.biliIsOpus
import uniffi.feedhub_ffi.biliParseComments
import uniffi.feedhub_ffi.biliParseDanmaku
import uniffi.feedhub_ffi.biliParsePlay
import uniffi.feedhub_ffi.biliParseDynamicDetail
import uniffi.feedhub_ffi.biliParseReplies
import uniffi.feedhub_ffi.biliParseVideoDetail
import uniffi.feedhub_ffi.biliParseDynamic
import uniffi.feedhub_ffi.biliParseDynamicItem
import uniffi.feedhub_ffi.biliParseDynamics
import uniffi.feedhub_ffi.biliParseRecommend
import uniffi.feedhub_ffi.biliParseSearch
import uniffi.feedhub_ffi.biliWbiKeyFromUrl
import uniffi.feedhub_ffi.biliWbiKeysFromNav
import uniffi.feedhub_ffi.biliWbiMixinKey
import uniffi.feedhub_ffi.biliWbiSign
import uniffi.feedhub_ffi.coreVersion

/**
 * Rust 核心的 Kotlin 门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 三件事：
 * 1. 把 `FfiException` 映射回现有的异常（`NotLoggedInException` / `RiskControlException` /
 *    `SignatureException` / `ApiException` / `ParseException`）——调用方的 `catch` 一行不用改；
 * 2. 把 `FfiPage` / `FfiItem` 还原成 `FeedItem`（转发是"节点数组 + 下标"，见 `feedhub-ffi` 的说明）；
 * 3. 加载失败（没编出 `.so`、ABI 不对）时说清楚，**不悄悄退回 Kotlin 实现**。
 */
object RustBili {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("bili") { coreVersion() }

    /** 已加载 / 能加载。设置页用它决定要不要把开关画成可点（不加载也返回 false，不会抛）。 */
    fun available(): Boolean = try {
        RustNative.ensureLoaded()
        true
    } catch (_: Throwable) {
        false
    }

    fun parseRecommend(body: String, freshIdx: Int, platform: Platform): Page<FeedItem> = guard(platform) {
        RustModel.toPage(biliParseRecommend(body, freshIdx.toLong()), platform)
    }

    fun parseDynamics(body: String, platform: Platform): Page<FeedItem> = guard(platform) {
        RustModel.toPage(biliParseDynamics(body), platform)
    }

    /**
     * 纯函数入口：一处搜索响应的 body → 一页（[page] 是页码，决定有没有下一页）。
     *
     * 生产路径走 `RustBiliHosted.search`（请求 + 解析都在 Rust）；这个只给对拍 / 排障用。
     */
    fun parseSearch(body: String, page: Int, platform: Platform = Platform(BILIBILI)): Page<FeedItem> = guard(platform) {
        RustModel.toPage(biliParseSearch(body, page), platform)
    }

    /** `v1/detail` 那种带 `{code}` 外壳、内容在 `data.item` 的响应。 */
    fun parseDynamicItem(body: String, platform: Platform): FeedItem? = guard(platform) {
        toItem(biliParseDynamicItem(body), platform)
    }

    /** 不带外壳的单条动态对象（测试边界样本用）。 */
    fun parseDynamic(body: String, platform: Platform): FeedItem? = guard(platform) {
        toItem(biliParseDynamic(body), platform)
    }

    fun wbiKeys(navBody: String): Pair<String, String>? = guard("bili") {
        biliWbiKeysFromNav(navBody)?.let { it.imgKey to it.subKey }
    }

    fun wbiMixinKey(imgKey: String, subKey: String): String = guard("bili") {
        biliWbiMixinKey(imgKey, subKey)
    }

    fun wbiKeyFromUrl(url: String): String = guard("bili") { biliWbiKeyFromUrl(url) }

    /**
     * WBI 签名。返回**保持插入顺序**的参数表（入参顺序 + wts + w_rid），
     * 调用方按顺序拼 query——顺序变了请求字节就变了。
     */
    fun wbiSign(params: Map<String, String>, mixinKey: String, wts: Long): Map<String, String> = guard("bili") {
        val out = LinkedHashMap<String, String>(params.size + 2)
        for (p in biliWbiSign(params.map { FfiParam(it.key, it.value) }, mixinKey, wts)) {
            out[p.name] = p.value
        }
        out
    }

    fun toBvid(aid: Long): String? = guard("bili") { biliAvToBv(aid) }

    fun toAid(bvid: String): Long? = guard("bili") { biliBvToAv(bvid) }

    fun isBvid(value: String): Boolean = guard("bili") { biliIsBvid(value) }

    // ---------------- 详情 / 评论 ----------------

    /** 视频详情（`/x/web-interface/wbi/view/detail` 的响应）。 */
    fun parseVideoDetail(body: String, loggedIn: Boolean, platform: Platform = Platform(BILIBILI)): FeedDetail =
        guard(platform) { RustModel.toDetail(biliParseVideoDetail(body, loggedIn), platform) }

    /** 动态详情（`v1/detail` 的响应 + 可选的 `opus/detail` 响应）。 */
    fun parseDynamicDetail(detailBody: String, opusBody: String?, platform: Platform = Platform(BILIBILI)): FeedDetail =
        guard(platform) { RustModel.toDetail(biliParseDynamicDetail(detailBody, opusBody), platform) }

    /** 这条动态的正文在 `opus/detail` 里吗（图文 / 专栏）。 */
    fun isOpus(detailBody: String): Boolean = guard("bili") { biliIsOpus(detailBody) }

    /** `v1/detail` 里的评论区位置：`(comment_type, oid)`；没有就 `null`。 */
    fun commentTarget(detailBody: String): Pair<Int, String>? = guard("bili") {
        biliCommentTarget(detailBody)?.let { it.commentType to it.oid }
    }

    /** 一级评论（`/x/v2/reply/wbi/main` 的响应）。 */
    fun parseComments(body: String, platform: Platform = Platform(BILIBILI)): Page<Comment> =
        guard(platform) { RustModel.toCommentPage(biliParseComments(body), platform) }

    /** 楼中楼（`/x/v2/reply/reply` 的响应）。`pn` 从 1 开始。 */
    fun parseReplies(body: String, pn: Int, ps: Int, platform: Platform = Platform(BILIBILI)): Page<Comment> =
        guard(platform) { RustModel.toCommentPage(biliParseReplies(body, pn, ps), platform) }

    // ---------------- 播放 / 弹幕 ----------------

    /** 播放地址（`/x/player/wbi/playurl` 的响应）。拉流头按 Rust 给的顺序放。 */
    fun parsePlay(body: String, platform: Platform = Platform(BILIBILI)): PlayInfo =
        guard(platform) { RustModel.toPlayInfo(biliParsePlay(body)) }

    /** 一段弹幕（`/x/v2/dm/wbi/web/seg.so` 的裸 protobuf）。空 body 是空列表。 */
    fun parseDanmaku(bytes: ByteArray, platform: Platform = Platform(BILIBILI)): List<Danmaku> =
        guard(platform) { RustModel.toDanmakuList(biliParseDanmaku(bytes)) }

    // ---------------- 内部 ----------------

    private const val BILIBILI = "bili"

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /**
     * 跨边界调用的统一出口：加载 `.so`、把 `FfiException` 映射回现有异常。
     *
     * 实现在 [HostedCall.guard]（和 11 家平台对象共用同一份）；这里的纯函数入口不发请求，
     * `Network` / `ProxyUnavailable` 理论上碰不到，真碰到了也按同样的类型抛出去（保持 B站 这一份的老行为）。
     */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = HostedCall.guard(platform, block)

    private fun toItem(node: FfiItemNode, platform: Platform): FeedItem? {
        val root = node.root ?: return null
        return RustModel.buildItem(root, node.nodes, platform)
    }
}
