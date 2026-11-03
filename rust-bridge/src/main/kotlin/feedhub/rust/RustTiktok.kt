package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiTiktokSigned
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.tiktokParseComments
import uniffi.feedhub_ffi.tiktokParseDetail
import uniffi.feedhub_ffi.tiktokParseFeed
import uniffi.feedhub_ffi.tiktokParseVideoPage
import uniffi.feedhub_ffi.tiktokSign

/**
 * TikTok 的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustBili` / `RustCoolapk` / `RustZhihu` 同一套做法（错误映射 / "节点数组 + 下标"还原 /
 * 加载失败说清楚），故意**没有抽公共父类**：这块按平台各放一个文件，
 *
 * TikTok 这边两类东西：
 * - **纯函数** [sign]：`X-Dynosaur` / `X-Gnarly` 签名（和 Kotlin `TiktokSign.sign` 同一套入参）。
 *   生产路径由 [RustTiktokHosted] 里那个平台对象直接调，
 *   这里导出来是给签名对拍门（定值 + 200 组固定种子随机参数）和排障用；
 * - **平台对象**（[RustTiktokHosted]）：连"请求怎么发"也一起管了，这里只留 `guard` / `page` 两个小门面。
 * 时间戳 / 随机密钥由调用方传进来（Rust 不取时间、不取随机数），`msToken` / `s_v_web_id` 只从 Cookie / store 读。
 */
object RustTiktok {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard(Platform(TIKTOK)) { coreVersion() }

    /**
     * 解析一页推荐流（**纯函数**）：给对拍（Kotlin `TiktokParser.feed` vs Rust）和排障用。
     * [httpCode] 不用传——TikTok 的错误判定在 `checkTransport`（平台对象）里，纯解析只看业务码。
     */
    fun parseFeed(body: String, fallbackCursor: String?, platform: Platform): Page<FeedItem> =
        guard(platform) { RustModel.toPage(tiktokParseFeed(body, fallbackCursor), platform) }

    /**
     * 签名（**纯函数**）。[query] 是已经编码好的业务查询串（不含四个签名参数），
     * [key] / [key2] 是两组各 12 个 32 位密钥，[nonce] / [nonce2] 也是 32 位——
     * 全部由调用方给，同一组输入两次调用逐字节相同。
     */
    fun sign(
        query: String,
        userAgent: String,
        msToken: String,
        body: ByteArray,
        timestamp: Long,
        nonce: Long,
        nonce2: Long,
        sequence: Int,
        key: List<UInt>,
        key2: List<UInt>,
    ): FfiTiktokSigned = guard(Platform(TIKTOK)) {
        tiktokSign(query, userAgent, msToken, body, timestamp, nonce, nonce2, sequence, key, key2)
    }

    // ---------------- 详情 / 评论 ----------------

    /** 解析一条 itemStruct（详情 fixture 里的作品对象）；`id` 是 `<作者>/<itemId>` 形态。 */
    fun parseDetail(itemBody: String, id: String, platform: Platform = Platform(TIKTOK)): FeedDetail =
        guard(platform) { RustModel.toDetail(tiktokParseDetail(itemBody, id), platform) }

    /** 从视频页 HTML 抠 hydration + 取 itemStruct + 解析。 */
    fun parseVideoPage(html: String, id: String, platform: Platform = Platform(TIKTOK)): FeedDetail =
        guard(platform) { RustModel.toDetail(tiktokParseVideoPage(html, id), platform) }

    /** 解析一页评论（一级 / 楼中楼同一个结构）。 */
    fun parseComments(body: String, platform: Platform = Platform(TIKTOK)): Page<Comment> =
        guard(platform) { RustModel.toCommentPage(tiktokParseComments(body), platform) }

    // ---------------- 内部 ----------------

    private const val TIKTOK = "tiktok"

    /**
     * 跨边界调用的统一出口：加载 `.so` + 把 `FfiException` 映射回现有异常。
     *
     * 实现在 [HostedCall.guard]（和 11 家平台对象共用同一份：网络错误 / fail-closed 不包装成
     * `SourceException`，原样抛 `java.io.IOException` / `ProxyUnavailableException`）。
     */
    internal inline fun <T> guard(platform: Platform, block: () -> T): T = HostedCall.guard(platform, block)
}
