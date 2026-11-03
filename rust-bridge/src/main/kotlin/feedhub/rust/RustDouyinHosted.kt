package feedhub.rust

import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.UserRef
import feedhub.core.SourceContext
import okhttp3.OkHttpClient
import uniffi.feedhub_ffi.FfiDouyinDevice
import uniffi.feedhub_ffi.FfiDouyinQr
import uniffi.feedhub_ffi.FfiDouyinSource

/**
 * **抖音平台对象的 Kotlin 门面**：`FfiDouyinSource`（Rust 里的 `uniffi::Object`）+ 三个宿主回调。
 *
 * 和 [RustDouyin]（纯函数门面）的分工：
 * - [RustDouyin] 只做"给我一段 body / 一串 query，我给你一页 / 一个签名"；
 * - 这个类让 Rust 管**整个推荐流请求**：拼 URL / 参数顺序 / `version_code` 那几套常量 / 请求头顺序 /
 *   翻页游标 / a_bogus / 空 body 与业务码 8 与两条风控头的判定，真正的收发回调宿主用 [client] 做
 *   （必须是 `ctx.client(platform)` 派生出来的那个客户端，见 `DouyinSource.httpClient`）。
 *
 * 设备（UA / 平台串 / 屏幕 / 核数 / 内存 / `install_time` / `verify_fp`）由 Kotlin 的 `DouyinDevice`
 * 读好传进来（**不是 Rust 造的**）；`webid` / `msToken` / `uifid` 由 Rust 自己按 Kotlin 的顺序从
 * `ctx.store` 和 Cookie 罐里读，没有就不塞。
 *
 * **线程模型**：所有调用都从 `Dispatchers.IO` 发起（Rust 的宿主回调在这条线程上同步回来），
 * 所以 `.so` 里没有 tokio、没有后台线程；[feed] 返回时一切都结束了。
 * 协程被取消时，[HostCallScope] 会把在飞的 OkHttp Call 掐掉（见 [RustHttpHost]）。
 */
class RustDouyinHosted(
    ctx: SourceContext,
    base: String,
    /** **Rust 联网层**：请求由 `feedhub-net` 发，不再经 OkHttp。 */
    private val net: RustNetStack,
    /** 只给扫码（`FfiDouyinQr`）用：扫码是另一条宿主能力（临时罐）， */
    private val client: OkHttpClient,
    private val platform: Platform,
    private val device: Device,
    private val profile: RustDouyin.AbogusProfile,
    private val env: RustDouyin.AbogusEnv,
    private val enterPageTs: Long,
    /** passport 的根（线上 `https://login.douyin.com`；测试里和 [base] 同一个 MockWebServer）。 */
    private val passportBase: String,
    /** a_bogus 的随机源（生产是 `Math.random()`，测试是固定种子）。Rust 不取随机数。 */
    private val random: () -> Double,
    /** 毫秒级时间戳（a_bogus 用它）。Rust 不读系统时钟。 */
    private val clock: () -> Long,
) {
    /**
     * 抖音 web 端的"设备环境"（Kotlin `DouyinDevice` 的那几个值）。
     *
     * `install_time` / `verify_fp` 是 `DouyinDevice` 从 `ctx.store`（`douyin.install_time` /
     * `douyin.verify_fp`）取的那一份。风控红线：设备身份只认 Kotlin 那一套。
     */
    class Device(
        val userAgent: String,
        val platform: String,
        val uaPlatform: String,
        val osName: String,
        val osVersion: String,
        val libraDivert: String,
        val browserMajor: String,
        val screenWidth: Int,
        val screenHeight: Int,
        /** 窗口尺寸（`account_sdk_source_info` 用的是窗口，不是屏幕）。 */
        val windowWidth: Int,
        val windowHeight: Int,
        val cpuCoreNum: Int,
        val deviceMemory: Int,
        /** `install_time` 参数（Kotlin `DouyinDevice.installTime`）。 */
        val installTime: Long,
        /** `verifyFp` 的兜底值（Cookie 里有 `s_v_web_id` 时用 Cookie 的）。 */
        val verifyFp: String,
    )

    /** 一页推荐流 + `desc` 拼串 + **原始响应体** + 这一页的 `refreshIndex`。 */
    class Feed(
        val page: Page<FeedItem>,
        val texts: String,
        /** 第二遍解析（带表情表）要用它；**不发请求**，请求条数和以前完全一样。 */
        val body: String,
        val refreshIndex: Int,
    )

    /** 详情 + `desc`（判断要不要取表情表）+ **原始响应体**（第二遍解析 / `play` 的缓存都用它）。 */
    class Detail(
        val detail: FeedDetail,
        val texts: String,
        val body: String,
    )

    /** 一页评论 + 这一页所有评论正文的拼串 + **原始响应体**（第二遍解析用）。 */
    class Comments(
        val page: Page<Comment>,
        val texts: String,
        val body: String,
    )

    /**
     * 懒建：**.so 的加载放在第一次真的要用的时候**（桌面要先解压到缓存目录、把路径告诉 JNA，见 [RustNative]），
     * 失败时走 [HostedCall.guard] 的统一报错（不会悄悄退回 Kotlin 实现）。
     */
    private val native: FfiDouyinSource by HostedCall.native {
        val obj = FfiDouyinSource.withNet(
            base,
            FfiDouyinDevice(
                userAgent = device.userAgent,
                platform = device.platform,
                uaPlatform = device.uaPlatform,
                osName = device.osName,
                osVersion = device.osVersion,
                libraDivert = device.libraDivert,
                browserMajor = device.browserMajor,
                screenWidth = device.screenWidth,
                screenHeight = device.screenHeight,
                cpuCoreNum = device.cpuCoreNum,
                deviceMemory = device.deviceMemory,
                installTime = device.installTime,
                verifyFp = device.verifyFp,
            ),
            profile.toFfi(),
            env.toFfi(),
            enterPageTs,
            net.inner,
            net.spec(
                platform = platform,
                minIntervalMs = DouyinRate.MIN_INTERVAL_MS,
                jitterMs = DouyinRate.JITTER_MS,
            ),
            RustStoreHost(ctx.store),
        )
        // a_bogus 的随机数改成"签的那一刻问宿主抽"：
        // 详情 / 评论里出现 `[赞]` 时 Rust 会**在同一个调用里**多签一次 `emoji/list`，
        // 要不要发只有当 Rust 解析完响应才知道。预抽几个会和 Kotlin 的随机流错位。
        obj.setRandom(RustRandomHost(random))
        obj
    }

    /** 扫码登录的平台对象（另一个 Rust 对象：多一个"临时 Cookie 罐"的宿主能力）。 */
    private val qrNative: FfiDouyinQr by HostedCall.native {
        FfiDouyinQr(
            base,
            passportBase,
            FfiDouyinDevice(
                userAgent = device.userAgent,
                platform = device.platform,
                uaPlatform = device.uaPlatform,
                osName = device.osName,
                osVersion = device.osVersion,
                libraDivert = device.libraDivert,
                browserMajor = device.browserMajor,
                screenWidth = device.screenWidth,
                screenHeight = device.screenHeight,
                cpuCoreNum = device.cpuCoreNum,
                deviceMemory = device.deviceMemory,
                installTime = device.installTime,
                verifyFp = device.verifyFp,
            ),
            profile.toFfi(),
            env.toFfi(),
            enterPageTs,
            device.windowWidth,
            device.windowHeight,
            RustHttpHost(client),
            RustStoreHost(ctx.store),
            RustCookieHost(ctx.cookies),
            qrHostOf(
                net = net,
                platform = platform,
                minIntervalMs = DouyinRate.MIN_INTERVAL_MS,
                jitterMs = DouyinRate.JITTER_MS,
            ),
        )
    }

    /** Rust 侧记住的接口根（排障 / 测试用：确认两边是同一份 base）。 */
    fun apiBase(): String = native.base()

    /** 取消在飞的请求（`HostCallScope` 在协程被取消时调）。 */
    internal fun cancelInFlight() = net.cancel()

    /**
     * 推荐流一页（`cursor` 就是上一页的 `refresh_index`；Rust 负责解析）。
     *
     * a_bogus 的随机数由 Rust 在**签名的那一刻**问宿主抽（[RustRandomHost]，接的是同一个 [random]）：
     * 抽取的个数与顺序和 Kotlin 参照实现逐位一致，而且"这一趟要不要多签一个 `emoji/list`"
     * （详情 / 评论里出现表情记号时）由 Rust 自己决定，调用方不用猜该预抽几个。
     */
    suspend fun feed(cursor: Cursor?): Feed = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val feed = native.feed(cursor?.value, nowMs, emptyList())
            Feed(
                page = RustModel.toPage(feed.page, platform),
                texts = feed.texts,
                body = feed.body,
                refreshIndex = feed.refreshIndex,
            )
        }
    }

    /**
     * 详情（`GET /aweme/v1/web/aweme/detail/`，**secsdk 保护路径**）：Rust 拼请求 + 判定 + 解析。
     * 随机数照 [feed]（Rust 在签名那一刻问宿主抽）。
     */
    suspend fun detail(id: String): Detail = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val hosted = native.detail(id, nowMs, emptyList())
            Detail(
                detail = RustModel.toDetail(hosted.detail, platform),
                texts = hosted.texts,
                body = hosted.body,
            )
        }
    }

    /** 一级评论（`comment/list/`）。游标就是响应里的 `cursor`（`null` = 第一页）。 */
    suspend fun comments(id: String, cursor: Cursor?): Comments = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val hosted = native.comments(id, cursor?.value, nowMs, emptyList())
            Comments(
                page = RustModel.toCommentPage(hosted.page, platform),
                texts = hosted.texts,
                body = hosted.body,
            )
        }
    }

    /**
     * 播放地址（`aweme/detail` 里的 `video`）。
     *
     * **60 秒内 [detail] 取过同一条作品就不再发请求**：那份原始响应缓存在 Rust 的 `recentAwemes` 里
     * 点开视频时详情刚取过，省掉一个 secsdk 保护路径上的请求和 1～1.5 秒限速。超过 60 秒才重新取一次。
     */
    suspend fun play(id: String, partId: String, quality: Int?): PlayInfo = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            RustModel.toPlayInfo(native.play(id, partId, quality, nowMs, emptyList()))
        }
    }

    /** 楼中楼（`comment/list/reply/`）：`verifyFp` / `fp` 放在 `a_bogus` 之后、不参与签名。 */
    suspend fun replies(id: String, commentId: String, cursor: Cursor?): Comments = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val hosted = native.replies(id, commentId, cursor?.value, nowMs, emptyList())
            Comments(
                page = RustModel.toCommentPage(hosted.page, platform),
                texts = hosted.texts,
                body = hosted.body,
            )
        }
    }
    /** 「我是谁」：没登录就是 `null`（不抛，也不发请求）。 */
    suspend fun whoami(): UserRef? = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            native.whoami(nowMs, emptyList())?.let { RustModel.toUser(it) }
        }
    }

    /** 扫码登录第一步：要一张二维码。[traceId] 由调用方生成（Kotlin `newTraceId()`）。 */
    suspend fun startQrLogin(traceId: String): QrLogin = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val count = RustDouyin.abogusRandomCount(env, nowMs, platform)
            val rand = DoubleArray(count) { random() }
            RustModel.toQrLogin(qrNative.start(nowMs, traceId, rand.toList()))
        }
    }

    /**
     * 扫码登录第二步：问一次状态。可能要两次签名（passport 一跳 + 验证 `whoami` 一跳），
     * 所以一次性取 `2 × count` 个随机数（Rust 按顺序消费；多给的不用）。
     */
    suspend fun pollQrLogin(login: QrLogin, traceId: String): QrLoginState = HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
        HostedCall.guard(platform) {
            val nowMs = clock()
            val count = RustDouyin.abogusRandomCount(env, nowMs, platform)
            val rand = DoubleArray(count * 2) { random() }
            RustModel.toQrState(qrNative.poll(login.toFfi(), nowMs, traceId, rand.toList()))
        }
    }
}

/** 抖音的限速（`DouyinSource.MIN_INTERVAL_MS` / `MIN_INTERVAL_JITTER_MS`，**只在这一处**）。 */
private object DouyinRate {
    const val MIN_INTERVAL_MS = 1000L
    const val JITTER_MS = 500L
}
