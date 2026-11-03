package feedhub.source.douyin

import feedhub.core.KeyValueStore
import feedhub.rust.RustDouyinDevice
import java.util.Random

/**
 * 抖音 web 端请求要用的一堆"环境"值：UA、屏幕、CPU 核数、内存，以及几个要**稳定不变**的本地标识
 * （`verifyFp` / `fp` / `s_v_web_id`、`install_time`）。
 *
 * ## 这些值为什么必须一致，以及现在用的是哪一套
 *
 * query 里再报另一套值会自相矛盾，所以 [screenWidth] / [screenHeight] / [cpuCoreNum] / [deviceMemory]
 * 这几个值**同时**进 query 和 a_bogus 的窗口指纹串。
 *
 * 另外 a_bogus 的字节里有两处和 UA 有关：`U[56]/U[57]/U[59]` 是 `SM3(s3-base64(RC4(UA)))` 的字节，
 * 而 [AbogusProfile.sdkMinor] 就是那条 RC4 的 key 的第三个字节 —— 所以 **UA、`sdkMinor`、
 * 窗口指纹串的平台段、query 里的 `browser_platform` / `os_name`、请求头里的 `sec-ch-ua-platform`
 * 必须成套**，改一处就要把整套一起改。
 *
 * 13 条 URL，10 条「反解 → 重签」逐字节一致）就是下面这些值：
 * UA 是 Linux x86_64 上的 Chrome 155、`platform` = `Linux x86_64`、窗口 1920x915、屏幕 1920x1080、
 * 16 核 / 16G。用这一套是因为**它和导入进来的 Cookie 来自同一台浏览器 / 同一个会话**，
 * 换一套（比如原来的 Windows / Chrome 141）就要重新抓一次、把 `sdkMinor` 重新解出来。
 *
 * [fingerprint] 是**实测原串**，直接给 [AbogusEnv.fingerprintOverride]，不按字段猜拼法
 * （拼法是从移植来源反推的，没被证实；见 `AbogusEnv` 的 KDoc）。13 条里 12 条是下面这个串，
 * 1 条（另一个 SDK 路径）是 `1920|915|1920|915|1920|1080|1920|1080|Linux x86_64`。
 *
 * [verifyFp] / [installTime] 是本地生成、存进 [KeyValueStore] 的，第一次用之后就一直不变
 *
 *
 * `rust/crates/feedhub-core/src/douyin/device.rs`：**环境常量是数据、生成规则是逻辑**，两样都搬过去了；
 * 这个类只剩薄门面（[env] 一个 record + 两三个转发）。随机量和时钟仍然由宿主注入
 * （[random] / [clock]），所以对拍能用同一条随机流逐字节比。
 *
 * **风控红线**：`ctx.store` 里已经有 `douyin.verify_fp` / `douyin.install_time` 时**原样沿用**，
 * Rust 那边是先读后写，不会重新生成（`verify_fp_is_stable_once_stored` 单测钉着）。
 */
internal class DouyinDevice(
    private val store: KeyValueStore,
    private val clock: () -> Long = System::currentTimeMillis,
    private val random: Random = Random(),
) {
    /** 抓包定下来的那套环境（第一次读属性时从 Rust 拿一次）。 */
    private val env: RustDouyinDevice.Env by lazy { RustDouyinDevice.env() }

    /** 和 UA 里的 Chrome 主版本必须一致。 */
    val chromeVersion: String get() = env.chromeVersion
    val browserMajor: String get() = env.browserMajor

    /**
     * 和这次抓包的 UA 逐字节一致（`sdkMinor` 就是拿它解出来的，改它必须重解 `sdkMinor`）：
     * 版本号跟着 [chromeVersion] 走，平台段是抓包那台浏览器报的 `X11; Linux x86_64`。
     */
    val userAgent: String get() = env.userAgent

    /**
     * 平台。三处都用它：窗口指纹串的最后一段、query 的 `browser_platform`、`os_version` 的来源。
     * 必须是 UA 里那个平台串（Linux 的 Chrome 报 `Linux x86_64`）。
     */
    val platform: String get() = env.platform

    /** `sec-ch-ua-platform` 的写法（带引号），和 [platform] / UA 一致。 */
    val uaPlatform: String get() = env.uaPlatform
    val osName: String get() = env.osName

    /** 这个浏览器的 `os_version`：Chrome 在 Linux 上报的是内核架构串。 */
    val osVersion: String get() = env.osVersion
    val libraDivert: String get() = env.libraDivert

    /** 屏幕 / 机器：query 和签名共用。 */
    val screenWidth: Int get() = env.screenWidth
    val screenHeight: Int get() = env.screenHeight
    val cpuCoreNum: Int get() = env.cpuCoreNum
    val deviceMemory: Int get() = env.deviceMemory

    /** 浏览器窗口（比屏幕小：有标题栏和地址栏），只进窗口指纹串。 */
    val windowWidth: Int get() = env.windowWidth
    val windowHeight: Int get() = env.windowHeight

    /**
     * 窗口指纹串的**实测原串**。
     * 平台段必须和 [platform] / UA 一致 —— `DouyinSourceTest` 有一条测试把关。
     */
    val fingerprint: String get() = env.fingerprint

    /**
     * 本地生成、存进 [KeyValueStore] 的 `s_v_web_id` 兜底值（第一次用之后就一直不变）。
     *
     * [DouyinSource] 会**优先用 Cookie 里的 `s_v_web_id`**，只有没导入时才用这个生成值。
     */
    val verifyFp: String by lazy { RustDouyinDevice.verifyFp(store, clock(), random::nextDouble) }

    /** 装"装了这个 App / 第一次打开"的时间，秒。来源不可得，首次使用时取当前时间并固定下来。 */
    val installTime: Long by lazy { RustDouyinDevice.installTime(store, clock()) }

    /**
     * a_bogus 用的浏览器环境。和上面那些 query 值是同源的；指纹串用实测原串（[fingerprint]），
     * 所以下面这些字段只用来描述同一个环境、解释这个串是怎么来的。
     */
    val abogusEnv: AbogusEnv by lazy {
        AbogusEnv(
            userAgent = userAgent,
            platform = platform,
            innerWidth = windowWidth,
            innerHeight = windowHeight,
            outerWidth = windowWidth,
            outerHeight = windowHeight,
            availWidth = screenWidth,
            availHeight = screenHeight,
            // screen.sizeWidth / sizeHeight 不是标准属性，真实 Chrome 里是 undefined → >> 0 得 0；
            // 实测串里的那两个 1920/1080 是 bdms 用自己的路径取到的，这里照串读，不再建模。
            screenSizeWidth = screenWidth,
            screenSizeHeight = screenHeight,
            // 真正的指纹串：实测原串优先，上面的字段不参与（见 AbogusEnv.fingerprintOverride）
            fingerprintOverride = fingerprint,
        )
    }

    /**
     * `biz_trace_id`：8 位十六进制（参考实现是 UUID 去掉横线后取前 8 位）。
     *
     * **每个扫码请求现生成一个、不落盘**；随机量还是这个实例的 [random]（生成规则在 Rust，
     * 要几个随机数由 Rust 说了算：`douyinTraceIdDraws`）。
     */
    fun traceId(): String = RustDouyinDevice.traceId(random::nextDouble)
}
