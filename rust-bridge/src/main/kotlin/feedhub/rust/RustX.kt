package feedhub.rust

import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.Page
import feedhub.core.Platform
import uniffi.feedhub_ffi.FfiXTransaction
import uniffi.feedhub_ffi.coreVersion
import uniffi.feedhub_ffi.xDualRunBatch
import uniffi.feedhub_ffi.xParseTweetComments
import uniffi.feedhub_ffi.xParseTweetDetail

/**
 * X 的 Rust 核心门面：**只暴露 [feedhub.core] 的类型**，不把 UniFFI 的 `Ffi*` 漏给业务代码。
 *
 * 和 `RustCoolapk` / `RustXhs` 同一套做法（[guard] 映射错误、`FfiPage` → `Page<FeedItem>`、
 * 加载失败说清楚），故意**没有抽公共父类**：这块按平台各放一个文件，
 *
 * X 这边两类东西：
 * - **平台对象**（[RustXHosted]）：推荐流 / 关注流的整个请求逻辑 + 解析；
 * - **纯函数**：`x-client-transaction-id` 的算法（[transaction]，调用时机在宿主的 `XSource` 拦截器里，
 *   限速之后），以及详情 / 评论的解析。
 */
object RustX {
    /** `.so` 的版本（构建时写进 crate 的版本号）。第一次调用会加载原生库。 */
    fun version(): String = guard("x") { coreVersion() }

    /**
     * 建一个 txid 生成器：`pageHtml` 是 x.com 页面，`signFileText` 是 loading 链第三跳的 sign 模块。
     * 抠不出输入时抛 [SourceException]（Kotlin 版那时抛的是 `XClientTransactionException`，
     * 上层都是"尽力而为"地吞掉，见 `XSource.ensureTxid`）。
     *
     * 关键字 / 随机数由调用方传（`XClientTransaction.DEFAULT_KEYWORD` / `ADDITIONAL_RANDOM_NUMBER`）：
     * 它们属于算法参数，不在桥接层再写一份。
     */
    fun transaction(
        pageHtml: String,
        signFileText: String,
        randomKeyword: String,
        randomNumber: Int,
    ): RustXTransaction = guard("x") {
        RustXTransaction(FfiXTransaction(pageHtml, signFileText, randomKeyword, randomNumber.toUByte()))
    }

    /** 平台 id 和 `XPlugin` 里那份同一个（`Platform.define` 幂等）。 */
    private val X: Platform = Platform.define("x", "X")

    /** 详情响应（`{"data":{"threaded_conversation_with_injections_v2":…}}`）→ 详情（**不发请求**）。 */
    fun parseTweetDetail(body: String, focalId: String, platform: Platform = X): FeedDetail =
        guard(platform) { RustModel.toDetail(xParseTweetDetail(body, focalId), platform) }

    /** 详情响应 → 一级评论（焦点之后的推文 + `conversationthread-` 模块）。 */
    fun parseTweetComments(body: String, focalId: String, platform: Platform = X): Page<Comment> =
        guard(platform) { RustModel.toCommentPage(xParseTweetComments(body, focalId), platform) }

    fun dualRunBatch(stepsJson: String): String = guard(X) { xDualRunBatch(stepsJson) }

    internal inline fun <T> guard(platform: String, block: () -> T): T = guard(Platform(platform), block)

    /**
     * 跨边界调用的统一出口：加载 `.so`、把 `FfiException` 映射回现有异常。
     *
     * 实现在 [HostedCall.guard]（和 11 家平台对象共用同一份：网络错误 / fail-closed 不包装成
     * `SourceException`，原样抛 `java.io.IOException` / `ProxyUnavailableException`）。
     */
    internal inline fun <T> guard(platform: Platform, block: () -> T): T = HostedCall.guard(platform, block)
}

/** `x-client-transaction-id` 生成器的 Kotlin 门面（算法在 Rust，见 [RustX.transaction]）。 */
class RustXTransaction internal constructor(private val native: FfiXTransaction) {
    /**
     * 生成一次请求用的 id。[path] 是 URL 的 path（不含 query），[method] 用大写，
     * [nowMs] / [randomByte] 由调用方注入（生产里是"真要发出去那一刻"的时间 + 随机字节）。
     */
    fun generate(method: String, path: String, nowMs: Long, randomByte: Int): String =
        native.generate(method, path, nowMs, randomByte.toUByte())

    /** 抠出来的 48 字节 key 的十六进制（**只给测试断言用**）。 */
    fun keyHex(): String = native.keyHex()

    /** 算出来的动画 key（测试断言用）。 */
    fun animationKey(): String = native.animationKey()
}
