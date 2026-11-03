package feedhub.rust

import feedhub.core.KeyValueStore
import uniffi.feedhub_ffi.douyinDeviceEnv
import uniffi.feedhub_ffi.douyinInstallTime
import uniffi.feedhub_ffi.douyinTraceId
import uniffi.feedhub_ffi.douyinTraceIdDraws
import uniffi.feedhub_ffi.douyinVerifyFp
import uniffi.feedhub_ffi.douyinVerifyFpDraws

/**
 * 抖音的设备环境 / 本地设备身份。
 *
 * 只读、不按机器探测——它必须和导入进来的 Cookie 来自同一台浏览器，换一套就要重抓、重解 `sdkMinor`。
 *
 * **本地身份**（[verifyFp] / [installTime]）**第一次用的时候生成一次、写进 `ctx.store`，之后原样沿用**：
 * 风控红线"不伪造设备或身份"，已经在 store 里的值一个字节都不许重新生成（先读后写的规则在 Rust 里）。
 * [traceId] 是每个扫码请求现生成的 8 位十六进制串，不落盘。
 *
 * 随机量由宿主注入（[random] 是 `Math.random()` 口径的 `[0, 1)` 流），Rust 把自己要几个写在
 * `douyinVerifyFpDraws` / `douyinTraceIdDraws` 里——**同一个随机流喂进去两边结果逐字节一样**
 */
object RustDouyinDevice {
    /** 抓包定下来的那套浏览器环境（和 Kotlin `DouyinDevice` 的属性一一对应）。 */
    class Env(
        val chromeVersion: String,
        val browserMajor: String,
        val userAgent: String,
        val platform: String,
        val uaPlatform: String,
        val osName: String,
        val osVersion: String,
        val libraDivert: String,
        val screenWidth: Int,
        val screenHeight: Int,
        val cpuCoreNum: Int,
        val deviceMemory: Int,
        val windowWidth: Int,
        val windowHeight: Int,
        val fingerprint: String,
    )

    fun env(): Env = load {
        val e = douyinDeviceEnv()
        Env(
            chromeVersion = e.chromeVersion,
            browserMajor = e.browserMajor,
            userAgent = e.userAgent,
            platform = e.platform,
            uaPlatform = e.uaPlatform,
            osName = e.osName,
            osVersion = e.osVersion,
            libraDivert = e.libraDivert,
            screenWidth = e.screenWidth,
            screenHeight = e.screenHeight,
            cpuCoreNum = e.cpuCoreNum,
            deviceMemory = e.deviceMemory,
            windowWidth = e.windowWidth,
            windowHeight = e.windowHeight,
            fingerprint = e.fingerprint,
        )
    }

    /** `verify_fp` 生成器要几个随机数。 */
    fun verifyFpDraws(): Int = load { douyinVerifyFpDraws().toInt() }

    /** 生成 `trace_id` 要几个随机数。 */
    fun traceIdDraws(): Int = load { douyinTraceIdDraws().toInt() }

    /** `ctx.store` 里的 `s_v_web_id` 兜底值：**有就原样返回**（哪怕是空串），没有才生成一次并写回去。 */
    fun verifyFp(store: KeyValueStore, nowMs: Long, random: () -> Double): String = load {
        douyinVerifyFp(RustStoreHost(store), nowMs, RustRandomHost(random))
    }

    /** `ctx.store` 里的安装时间（秒）：有就返回，没有才取 [nowMs] / 1000 并写回去。 */
    fun installTime(store: KeyValueStore, nowMs: Long): Long = load {
        douyinInstallTime(RustStoreHost(store), nowMs)
    }

    /** `biz_trace_id`：8 位十六进制，**每次现生成**。 */
    fun traceId(random: () -> Double): String = load { douyinTraceId(RustRandomHost(random)) }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
