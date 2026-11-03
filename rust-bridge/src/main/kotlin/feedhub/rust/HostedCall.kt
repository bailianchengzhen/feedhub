package feedhub.rust

import feedhub.core.ApiException
import feedhub.core.CommentSort
import feedhub.core.FeedKind
import feedhub.core.NotLoggedInException
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.ProxyUnavailableException
import feedhub.core.RiskControlException
import feedhub.core.SignatureException
import feedhub.core.SourceException
import feedhub.core.inBackgroundRequestScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.feedhub_ffi.FfiCommentSort
import uniffi.feedhub_ffi.FfiException
import uniffi.feedhub_ffi.FfiFeedKind
import java.io.IOException

/**
 * **平台对象门面（`Rust*Hosted.kt`）的公共零件**。
 *
 * 每个门面都把这些照抄了一份。现在合完了，只剩这一份，11 家全部改成调这里：
 *
 * | 零件 | 干什么 |
 * |---|---|
 * | [blocking] | 在 `Dispatchers.IO` 上跑阻塞的 Rust 调用；**取消时掐掉在飞的 OkHttp Call** |
 * | [guard] | 加载 `.so` + 把 `FfiException` 映射回现有异常（网络 / fail-closed 不包装） |
 * | [native] | `by lazy` 建平台对象：第一次真的要用的时候才加载 `.so`，失败也走 `guard` 的统一报错 |
 *
 * **纯函数门面（`RustBili` / `RustZhihu` / …）用的是 [RustModel.guard]**：那条路上不发请求，
 * 不可能出现宿主的 `Network` / `ProxyUnavailable`，所以它们照旧折成 `SourceException`（原来的语义）。
 */
internal object HostedCall {
    /**
     * 懒建平台对象：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，
     * 见 [RustNative]），而且失败时要走 [guard] 的统一报错。
     *
     * 用法：`private val native: FfiBiliSource by HostedCall.native { FfiBiliSource(...) }`
     */
    fun <T> native(create: () -> T): Lazy<T> = lazy {
        RustNative.ensureLoaded()
        create()
    }

    /**
     * 在 IO 线程上跑阻塞的 Rust 调用；**取消时掐掉在飞的请求**。
     *
     * 为什么不用 `job.invokeOnCompletion { }`：那个回调要等 job **真正完成**才跑——而这里的 job 正卡在
     * 一次不可中断的 JNA 调用里，等它完成就是等请求自然结束（那正好是我们要避免的）。
     * 所以挂一个 `awaitCancellation()` 的子协程：父 job 一取消，子协程立刻抛 `CancellationException`，
     * `finally` 里就能把在飞的 OkHttp Call 掐掉（[HostCallScope] 是线程局部的，Rust 的宿主回调
     * 就在这条线程上同步回来）。
     */
    /**
     * 和 [blocking] 一样，额外登记一个**取消动作**（Rust 联网层用）。
     *
     * 为什么要单独开一个：`withNet` 那条路上请求是 Rust 内部直接发的，Kotlin 这边**没有 host 对象**
     * 可以在 `execute` 里注册（那是 `RustNetHttpHost` 那条旧路才有的）。所以门面把"掐掉这份栈的
     * 在飞请求"作为一个动作传进来，取消时和 OkHttp 的 `Call.cancel()` 一起触发。
     */
    suspend fun <T> blocking(cancelInFlight: (() -> Unit)?, block: () -> T): T = coroutineScope {
        val scope = HostCallScope(
            background = inBackgroundRequestScope(),
            job = currentCoroutineContext()[Job],
        )
        if (cancelInFlight != null) {
            scope.registerNet(cancelInFlight)
        }
        val watcher = launch(start = CoroutineStart.UNDISPATCHED) {
            try {
                awaitCancellation()
            } finally {
                scope.cancelAll()
            }
        }
        try {
            withContext(Dispatchers.IO) {
                try {
                    HostCallScope.withScope(scope) { block() }
                } catch (t: Throwable) {
                    // 外部协程已取消时，JNA 返回的 IOException("Canceled") 只是让阻塞线程脱困的手段；
                    // 对齐 Call.await()（if (cont.isActive)）与 Hosts.kt 契约，优先抛 CancellationException。
                    currentCoroutineContext().ensureActive()
                    throw t
                } finally {
                    // 正常返回（或抛业务异常）时必须在 watcher.cancel() 前摘掉栈级取消动作；
                    // 否则 watcher.cancel() 会误触发 stack.cancel()，把同一平台并发在飞的其它请求（如详情+评论）掐断。
                    // 真被外部取消时，watcher 先于阻塞的 block() 收到取消并触发 scope.cancelAll()，取消语义不变。
                    if (cancelInFlight != null) {
                        scope.unregisterNet(cancelInFlight)
                    }
                }
            }
        } finally {
            watcher.cancel()
        }
    }

    suspend fun <T> blocking(block: () -> T): T = blocking(cancelInFlight = null, block = block)

    /**
     * 平台对象的统一出口：加载 `.so`、把 `FfiException` 映射回现有异常。
     *
     * **网络错误和代理不可用不包装成 `SourceException`**：现有约定是原样抛 `java.io.IOException`
     * （[ProxyUnavailableException] 是它的子类，cli 靠它分退出码 6）。这两类在平台对象这条路上真的会出现——
     * 断网和 fail-closed 从宿主回调原样带回来。
     */
    inline fun <T> guard(platform: Platform, block: () -> T): T {
        try {
            RustNative.ensureLoaded()
            return block()
        } catch (e: FfiException) {
            throw toThrowable(e, platform)
        } catch (e: SourceException) {
            throw e
        } catch (e: UnsatisfiedLinkError) {
            throw SourceException(platform, "Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}")
        } catch (e: IllegalStateException) {
            throw SourceException(platform, "Rust 核心不可用：${e.message}")
        }
    }

    /**
     * 返回 `Throwable` 而不是 `SourceException`：网络错误 / fail-closed 是 `IOException` 家族（和 Kotlin 版同一类）。
     *
     * 注意：`FfiException` 的每个变体自己都有一个 `platform: String` 字段，在 `when (this)` 的分支里会把
     * 外层的 `platform: Platform` **遮住**（踩过：报 "actual type is String"），所以这里传的是另一个名字。
     */
    fun toThrowable(e: FfiException, p: Platform): Throwable = when (e) {
        is FfiException.NotLoggedIn -> NotLoggedInException(p, e.detail ?: "未登录或登录已失效")
        is FfiException.RiskControl -> RiskControlException(p, e.code, e.detail)
        is FfiException.Signature -> SignatureException(p, e.detail)
        is FfiException.Api -> ApiException(p, e.code, e.detail)
        is FfiException.Parse -> ParseException(p, e.detail)
        is FfiException.Network -> IOException(e.detail)
        is FfiException.ProxyUnavailable -> ProxyUnavailableException(e.detail)
        is FfiException.Internal -> SourceException(p, "Rust 内部错误：${e.detail}")
        // 要人机验证（酷安评论 / 楼中楼）：**不在这里抛风控错**——平台门面要先让用户本人验证一次、
        // 再重试同一个请求一次；它翻成风控错的条件是"没有 handler / 用户取消 / 验证仍不过"。
        is FfiException.CaptchaRequired -> CaptchaRequiredException(p, e.captchaId, e.field, e.detail)
    }
}

/**
 * `FeedKind` → 跨边界的枚举（只有推荐 / 关注两条流）。
 *
 * 6 家平台对象原来各写一遍 `when`，收在这里；**只有一条流的平台**（小黑盒 / TikTok / 抖音 / 小红书）
 * 连 `FfiFeedKind` 都不需要，流种类在 Kotlin 门面就判掉了。
 */
internal fun FeedKind.toFfi(): FfiFeedKind = when (this) {
    FeedKind.RECOMMEND -> FfiFeedKind.RECOMMEND
    FeedKind.FOLLOWING -> FfiFeedKind.FOLLOWING
}

/**
 * `CommentSort` → 跨边界的枚举。B站 只有热门 / 最新两档（`DEFAULT` 和 `HOT` 都是热门），
 * 但**映射放在这一层**、平台对象自己决定怎么用（别在门面里先折成两档，下一个平台可能用得上三档）。
 */
internal fun CommentSort.toFfi(): FfiCommentSort = when (this) {
    CommentSort.DEFAULT -> FfiCommentSort.DEFAULT
    CommentSort.HOT -> FfiCommentSort.HOT
    CommentSort.NEWEST -> FfiCommentSort.NEWEST
}
