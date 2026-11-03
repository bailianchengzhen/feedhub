package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.UserRef
import uniffi.feedhub_ffi.FfiEmote
import uniffi.feedhub_ffi.FfiItemNode
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.coolapkBase64
import uniffi.feedhub_ffi.coolapkMd5Hex
import uniffi.feedhub_ffi.coolapkParseAccount
import uniffi.feedhub_ffi.coolapkParseDetail
import uniffi.feedhub_ffi.coolapkParseFeed
import uniffi.feedhub_ffi.coolapkParseItem
import uniffi.feedhub_ffi.coolapkParsePlay
import uniffi.feedhub_ffi.coolapkParseReplies
import uniffi.feedhub_ffi.coolapkParseSearch
import uniffi.feedhub_ffi.coolapkV2Token
import uniffi.feedhub_ffi.coolapkV3Token

/**
 * 酷安的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustBili` / `RustZhihu` 同一套做法（错误映射 / "节点数组 + 下标"还原 / 加载失败说清楚），
 * 故意**没有抽公共父类**：这块按平台各放一个文件，
 *
 * 酷安这边两类东西：
 * - **纯函数**：推荐流 / 关注流 / 详情 / 评论 / 楼中楼 / **搜索 / 播放 / 账号**的解析
 *   （`coolapk` 模块）、`X-App-Token` 的 v2 / v3；对拍和排障走这里；
 * - **平台对象**：[RustCoolapkHosted] 让 Rust 连"请求怎么发"
 *   也一起管了（现在只剩 `raw` 和扫码在 Kotlin）。
 * 设备码 / 安装时间由 Rust 直接从 `ctx.store` 读，时间戳由调用方传进来（Rust 不取时间、不取随机数），
 * v3 的密码表由调用方传（`source-coolapk` 的资源解码后那 930 字节）。
 */
object RustCoolapk {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("coolapk") { coreVersion() }

    /**
     * 解析一页信息流。[httpCode] 是 HTTP 状态码，[page] / [firstItem] / [lastItem] 是**当前**游标；
     * 返回值里的 `next` 是下一行游标。错误由 Rust 带类型回来（含"关注流返回 unLoginCard → 未登录"）。
     */
    fun parseFeed(
        kind: FeedKind,
        httpCode: Int,
        page: Int,
        firstItem: String?,
        lastItem: String?,
        body: String,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(
            coolapkParseFeed(kind.toFfi(), body, httpCode, page, firstItem, lastItem, emotes.toFfi()),
            platform,
        )
    }

    /**
     * 解析详情（`/v6/feed/detail` 的响应）。[resolvedMedia] 是 `b23.tv` 短链跟完重定向后的地址
     * （没有就传 null）。对拍 / 排障用；生产路径是 [RustCoolapkHosted.detail]。
     */
    fun parseDetail(
        body: String,
        resolvedMedia: String?,
        emotes: Map<String, String>,
        platform: Platform,
    ): FeedDetail = guard(platform) {
        RustModel.toDetail(coolapkParseDetail(body, resolvedMedia, emotes.toFfi()), platform)
    }

    /**
     * 解析一页评论 / 楼中楼（`/v6/feed/replyList` 的响应）。[page] / [firstItem] / [lastItem] 是**当前**游标，
     * 返回值里的 `next` 是下一页游标。生产路径是 [RustCoolapkHosted.comments] / [RustCoolapkHosted.replies]。
     */
    fun parseReplies(
        body: String,
        page: Int,
        firstItem: String?,
        lastItem: String?,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<Comment> = guard(platform) {
        RustModel.toCommentPage(
            coolapkParseReplies(body, page, firstItem, lastItem, emotes.toFfi()),
            platform,
        )
    }

    /**
     * 解析一页搜索结果（`/v6/search` 的响应）。[page] 是**当前**页（下一页游标是 `page+1|lastItem`）。
     *
     * 对拍 / 排障用；生产路径是 [RustCoolapkHosted.search]（连请求也在 Rust）。
     */
    fun parseSearch(
        body: String,
        httpCode: Int,
        page: Int,
        emotes: Map<String, String>,
        platform: Platform,
    ): Page<FeedItem> = guard(platform) {
        RustModel.toPage(coolapkParseSearch(body, httpCode, page, emotes.toFfi()), platform)
    }

    /**
     * 解析播放（`/v6/feed/detail` 的响应 → 播放通用模型）。**不含 Range 探针**（那是请求层的事）。
     *
     * 对拍 / 排障用；生产路径是 [RustCoolapkHosted.play]。
     */
    fun parsePlay(body: String, httpCode: Int, platform: Platform): PlayInfo = guard(platform) {
        RustModel.toPlayInfo(coolapkParsePlay(body, httpCode))
    }

    /**
     * 解析 `checkLoginInfo` 的响应：未登录（服务端"登录信息有误"）是 `null`，别的错误照常抛。
     *
     * 对拍 / 排障用；生产路径是 [RustCoolapkHosted.whoami]（Cookie 判断与请求都在那边）。
     */
    fun parseAccount(body: String, httpCode: Int, platform: Platform): UserRef? =
        RustModel.run { guard(platform) { coolapkParseAccount(body, httpCode)?.toUserRef() } }

    /** 一份不带外壳的 `feed` 实体（测试的边界样本）。 */
    fun parseItem(body: String, emotes: Map<String, String>, platform: Platform): FeedItem? = guard(platform) {
        toItem(coolapkParseItem(body, emotes.toFfi()), platform)
    }

    /** v2 token（信息流 / 关注流 / 详情的身份）。 */
    fun v2Token(deviceCode: String, timestampSec: Long): String = guard("coolapk") {
        coolapkV2Token(deviceCode, timestampSec)
    }

    /** v3 token（评论接口的身份）。[blob] 是 `auth_blob.b64` 解码后的 930 字节。 */
    fun v3Token(deviceCode: String, timestampSec: Long, appCode: Long, blob: ByteArray): String =
        guard("coolapk") { coolapkV3Token(deviceCode, timestampSec, appCode, blob) }

    /** 小写十六进制 MD5（固定值测试用）。 */
    fun md5Hex(text: String): String = guard("coolapk") { coolapkMd5Hex(text) }

    /** 标准 base64、去掉 `=`（固定值测试用）。 */
    fun base64(text: String): String = guard("coolapk") { coolapkBase64(text) }

    // ---------------- 内部 ----------------

    private fun Map<String, String>.toFfi(): List<FfiEmote> = map { FfiEmote(it.key, it.value) }

    private inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /** 跨边界调用的统一出口：加载 `.so` + 把 `FfiException` 映射回现有异常（实现在 [HostedCall.guard]）。 */
    private inline fun <T> guard(platform: Platform, block: () -> T): T = HostedCall.guard(platform, block)

    private fun toItem(node: FfiItemNode, platform: Platform): FeedItem? {
        val root = node.root ?: return null
        return RustModel.buildItem(root, node.nodes, platform)
    }
}
