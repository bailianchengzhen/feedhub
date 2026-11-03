package feedhub.rust

import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.SearchSort
import feedhub.core.SourceContext
import feedhub.core.UserRef
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.FfiCaptchaAnswer
import uniffi.feedhub_ffi.FfiCoolapkSearchSort
import uniffi.feedhub_ffi.FfiCoolapkSource
import uniffi.feedhub_ffi.FfiEmote

/**
 * **酷安平台对象的 Kotlin 门面**：`FfiCoolapkSource`（Rust 里的 `uniffi::Object`）+ 宿主回调。
 *
 * 和 [RustCoolapk]（纯函数门面）的分工：
 * - [RustCoolapk] 只做"给我一段 body、我给你一页"；
 * - 这个类让 Rust 管**整个请求**：拼 URL / 参数顺序 / 请求头顺序、翻页游标、`X-App-Token` 的计算与按秒缓存、
 *   状态码与错误码判定，真正的收发回调宿主用传进来的两个客户端做。
 *
 * **三个客户端**（和 `CoolapkSource` 里那三个一样，不能合成一个）：
 * - [client]：酷安接口客户端（设备默认头 + 600ms 限速 + 代理 + Cookie 罐），
 *   推荐 / 关注 / 详情 / 评论 / 搜索 / 账号都走它（评论是 `api.coolapk.com`，同一个客户端）；
 * - [links]：`b23.tv` 短链跟重定向用的，必须是 **B站 的客户端**（不跟重定向、不带 Cookie、B站 的代理设置）；
 * - [media]：播放地址的 `Range: bytes=0-0` 探针（`CoolapkSource.mediaHttp`：`ctx.client(platform)` +
 *   `CookieJar.NO_COOKIES`，**没有**设备默认头、**没有** 600ms 限速）——只有 [play] 用，
 *   所以拉流 / 探针不会把 `.coolapk.com` 的登录 Cookie 带到 `video-qc.coolapk.com`。
 *
 * 设备（`X-App-Device` / UA / `X-Sdk-Int`）和 `installTime` **不从 Kotlin 传**：Rust 直接读 `ctx.store` 里那两份
 * （`coolapk.device` / `coolapk.install_time`，由 `CoolapkDevice.loadOrCreate` 生成），所以"只用 store 里那一份"
 * 是结构上的保证，不是纪律。
 *
 * **人机验证**：`comments` / `replies` 遇到 `err_request_captcha_v2` 时 Rust 返回
 * [CaptchaRequiredException]（带 `captchaId` / `field` / 服务端原话），**本类不处理**——平台门面
 * `RustCoolapkSource` 负责让用户本人验证一次、把答案塞回 [comments] / [replies] 的 `captcha` 参数重试一次。
 * 这边只提供"把答案带过去"的形状，验证码一个字都不解、不绕。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来，
 * 见 [HostedCall.blocking]），所以 `.so` 里没有 tokio、没有后台线程；方法返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会在飞的 OkHttp Call 被掐掉（见 [RustHttpHost]）。
 */
class RustCoolapkHosted(
    ctx: SourceContext,
    base: String,
    commentsBase: String,
    /** **Rust 联网层**：三个端口的请求都由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    /**
     * 设备默认头（`CoolapkSource` 的 `device.headers()`：UA / X-App-Version / X-App-Code /
     * X-Api-Version / X-App-Supported）。**设备身份归平台模块**，所以由 `RustCoolapkSource` 传进来。
     */
    deviceHeaders: Map<String, String>,
    private val platform: Platform,
    /** 表情表（"正文原文 → 图片地址"）。**构造时传一次**，Rust 侧不每页搬。 */
    emotes: Map<String, String>,
    /**
     * v3 token 的 930 字节密码表（插件资源 `auth_blob.b64` 解码后，`AppToken.v3BlobBytes()`）。
     * 表归插件所有，**构造时传一次**（评论那套 16.2.0 身份算 token 要用）。
     */
    v3Blob: ByteArray,
) {
    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiCoolapkSource by HostedCall.native {
        FfiCoolapkSource.withNet(
            base,
            commentsBase,
            emotes.map { (code, url) -> FfiEmote(code, url) },
            v3Blob,
            net.inner,
            // ① 接口：设备头 + 600ms 限速 + **带 Cookie**
            net.spec(
                platform = platform,
                defaultHeaders = deviceHeaders,
                defaultHeaderHosts = API_HOSTS.toList(),
                minIntervalMs = CoolapkRate.MIN_INTERVAL_MS,
            ),
            // ② 媒体：**不带 Cookie、不占限速**（拉媒体文件 / `Range: bytes=0-0` 探针）
            net.spec(platform = platform),
            // ③ b23.tv 短链：**B站 的代理档**、不带 Cookie、**不跟重定向**（要自己读 Location）
            net.spec(platform = platform, followRedirects = false),
            RustStoreHost(ctx.store),
        )
    }

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /** Rust 侧记住的接口根（排障用：确认拿到的是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** Rust 侧记住的评论接口根（排障用）。 */
    fun commentsBase(): String = native.commentsBase()

    /**
     * 一页。[nowSec] 是秒级时间戳（`X-App-Token` 只取决于设备码 + 秒）——
     * 由调用方注入，测试才能把两次运行的请求逐字节比。
     */
    suspend fun feed(kind: FeedKind, cursor: Cursor?, nowSec: Long): Page<FeedItem> {
        val ffiKind = kind.toFfi()
        return HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPage(native.feed(ffiKind, cursor?.value, nowSec), platform)
            }
        }
    }

    /** 详情。[nowSec] 同上；`b23.tv` 短链会走 [RustHttpHost] 的第二个客户端跟一次重定向。 */
    suspend fun detail(id: String, nowSec: Long): FeedDetail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toDetail(native.detail(id, nowSec), platform)
        }
    }

    /**
     * 一级评论 / 楼中楼。[captcha] 是"用户刚完成验证、这次把答案带上"的那一份；第一次调用传 `null`。
     *
     * 要验证时这里会抛 [CaptchaRequiredException]（**不是风控错**）：交给平台门面去弹验证、再带着答案重来一次。
     */
    suspend fun comments(
        id: String,
        cursor: Cursor?,
        sort: CommentSort,
        nowSec: Long,
        captcha: CaptchaAnswer? = null,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(
                native.comments(id, cursor?.value, sort.toFfi(), nowSec, captcha?.toFfi()),
                platform,
            )
        }
    }

    /**
     * 搜索的一页：**请求怎么拼 + 解析都在 Rust**。
     *
     * 游标是 `page|lastItem`（上一页最后一条内容的 id），`null` = 第一页；[sort] 是酷安自己的三档
     * （综合 / 最新 / 最热）。撞上 `err_request_captcha*` 时 Rust 直接回风控错、**不重试**
     */
    suspend fun search(text: String, sort: SearchSort, cursor: Cursor?, nowSec: Long): Page<FeedItem> =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            val ffiSort = when (sort) {
                SearchSort.RELEVANCE -> FfiCoolapkSearchSort.RELEVANCE
                SearchSort.LATEST -> FfiCoolapkSearchSort.LATEST
                SearchSort.HOT -> FfiCoolapkSearchSort.HOT
            }
            HostedCall.guard(platform) {
                RustModel.toPage(native.search(text, ffiSort, cursor?.value, nowSec), platform)
            }
        }

    /**
     * 播放（只有酷安自己托管的视频动态，`partId == "main"`）。
     *
     * Rust 先取 `/v6/feed/detail`，再用 [media] 那个客户端发一次 `Range: bytes=0-0`：
     * 401 / 403（防盗链 `need sign info`）→ [feedhub.core.SignatureException]，
     * 别的非 2xx → `ApiException("HTTP <code>")`。返回的是**播放通用模型**（和 B站 同一条路）。
     */
    suspend fun play(id: String, partId: String, quality: Int?, nowSec: Long): PlayInfo =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            HostedCall.guard(platform) {
                RustModel.toPlayInfo(native.play(id, partId, quality, nowSec))
            }
        }

    /**
     * 当前登录的用户：**没有 `uid` Cookie 时 Rust 回 `null`、一个请求都不发**；
     * 服务端说"登录信息有误"也是 `null`（Kotlin 原逻辑）。Cookie 只查"有没有"，值不过边界。
     */
    suspend fun whoami(nowSec: Long): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            // `toUserRef` 是 `RustModel` 的成员扩展（映射只有那一份，别在这里再抄一遍）
            RustModel.run { native.whoami(nowSec)?.toUserRef() }
        }
    }

    /** 楼中楼（`commentId` 是一级评论的 id）。 */
    suspend fun replies(
        commentId: String,
        cursor: Cursor?,
        nowSec: Long,
        captcha: CaptchaAnswer? = null,
    ): Page<Comment> = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            RustModel.toCommentPage(native.replies(commentId, cursor?.value, nowSec, captcha?.toFfi()), platform)
        }
    }

    /**
     * 用户完成人机验证之后的答案（`field` 为空 = 已经写进 `validate` cookie）。
     * `proof` 是短时凭据，**不许打日志**。
     */
    class CaptchaAnswer(val field: String, val proof: String) {
        internal fun toFfi(): FfiCaptchaAnswer = FfiCaptchaAnswer(field, proof)
    }
}

/** 酷安的接口域名（`CoolapkSource.API_HOSTS`，默认头只对这几个生效）。 */
private val API_HOSTS = setOf("api2.coolapk.com", "api.coolapk.com")

/** 酷安的限速（`CoolapkSource.MIN_INTERVAL_MS`，**只在这一处**）。 */
private object CoolapkRate {
    const val MIN_INTERVAL_MS = 600L
}
