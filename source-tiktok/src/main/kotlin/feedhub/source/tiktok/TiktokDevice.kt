package feedhub.source.tiktok

import feedhub.core.FeedCookieJar
import feedhub.core.KeyValueStore
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.util.Random

/**
 * TikTok 网页端请求要用的一堆"环境"值：UA、`sec-ch-ua*`、屏幕、`clientABVersions`，
 * 以及几个**要稳定不变 / 要从浏览器侧拿**的标识（`device_id` / `odinId` / `region` / `verifyFp` /
 * `WebIdLastTime`）。
 *
 * ## 值是从哪来的
 *
 * UA、`sec-ch-ua*`、`accept-language`、`clientABVersions`、屏幕尺寸、`tz_name` 是一整套，来自
 * **Chrome 153 / Windows 的抓包**：`cv-cat/TiktokApis: builder/auth.py`
 * 的 `TiktokAuth`（UA / `SEC_CH_UA` / `CLIENT_AB_VERSIONS` / `ACCEPT_LANGUAGE`）与
 * `builder/params.py`（`screen_height=1440` / `screen_width=2560` / `tz_name=Asia/Shanghai`）。
 * **这套值要成套**：query 里的 `browser_version` 是 UA 去掉 `Mozilla/` 之后的那一段，
 * `browser_platform` / `os` / `sec-ch-ua-platform` 都要和 UA 里的平台一致，
 * X-Dynosaur 的 `0x30` 又是 **UA 的 hash_state**（所以签名和请求头里的 UA 必须逐字节相同）。
 *
 * 服务端校验的 `envcode` / `ubcode`（65 / 8）在 [TiktokSign] 里，是 f2 按 Chrome 实测定的；
 * 这两个值和这套 UA 不是一回事，但一样会随 SDK 版本变。
 *
 * ## 哪些值是"从 Cookie / 首页拿"的
 *
 * - `device_id` / `odinId`：`multi_sids` Cookie 的第一段（TiktokApis 的 `_sync_ids_from_cookies`），
 *   没有就靠首页 hydration 的 `webapp.app-context.wid` / `odinId` 补（catbus 的 `bootstrap()`），
 *   拿到后写进 `ctx.store`（键加 `tiktok.` 前缀）**一直不变**。
 *   **两个都没有时就不往 query 里塞**（不编设备号；TiktokApis 会随机生成一个，我们不那样做）。
 * - `region`：首页 hydration 的 `region` → `store-country-code` Cookie → 兜底 `SG`
 * - `WebIdLastTime`：首页 hydration 的 `webIdCreatedTime` → 兜底"首次用到时的那一刻"并记下来。
 * - `verifyFp`（= `s_v_web_id`）：**优先用 Cookie 里的真值**，没有才本地生成一个固定值（catbus 就是这么补的）。
 * - `msToken`：**只用 Cookie（或 [KeyValueStore]）里的真值**（`TiktokSource` 里取），
 *   没有就是空字符串——f2 的实测：伪造的 msToken 会让 `item_list` 返回空 body + `tt_orcas_res: 1`。
 */
internal class TiktokDevice(
    private val store: KeyValueStore,
    private val cookies: FeedCookieJar,
    private val clock: () -> Long = System::currentTimeMillis,
    private val random: Random = Random(),
) {
    /** 抓包那台浏览器：Chrome 153 / Windows（TiktokApis 的 `TiktokAuth.UA`）。 */
    val chromeVersion: String = "153.0.0.0"

    /** 必须和签名里 `0x30` 的 hash_state 用同一个串（签名拦截器直接读请求头里的 UA）。 */
    val userAgent: String =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) " +
            "Chrome/$chromeVersion Safari/537.36"

    /** `sec-ch-ua`：抓包原值（TiktokApis 的 `SEC_CH_UA`，Chrome 153）。 */
    val secChUa: String = "\"Google Chrome\";v=\"153\", \"Not_A Brand\";v=\"8\", \"Chromium\";v=\"153\""

    /** `sec-ch-ua-platform`：和 UA 里的平台一致。 */
    val uaPlatform: String = "\"Windows\""

    val acceptLanguage: String = "zh-CN,zh;q=0.9,en;q=0.8,zh-TW;q=0.7,ja;q=0.6"

    /** query 里的 `browser_version`：UA 去掉 `Mozilla/` 之后的那一段（cv-cat 就是这么切）。 */
    val browserVersion: String = userAgent.substringAfter("Mozilla/")

    val browserName: String = "Mozilla"
    val browserPlatform: String = "Win32"
    val os: String = "windows"
    val browserLanguage: String = "zh-CN"
    val appLanguage: String = "zh-Hans"

    /** 屏幕 / CPU 核数：抓包那台机器（TiktokApis 的 `screen_height/width`、`cpu_core_number=20`）。 */
    val screenWidth: Int = 2560
    val screenHeight: Int = 1440
    val cpuCoreNumber: Int = 20

    val tzName: String = "Asia/Shanghai"

    /**
     * 抓包里的 `clientABVersions`（TiktokApis 的 `CLIENT_AB_VERSIONS`，Chrome 153）。
     * 这是 Chrome 从服务端下发的一串实验号，会随版本变；`ctx.store` 的 `tiktok.client_ab_versions` 能覆盖。
     */
    val clientAbVersions: String =
        store.get(KEY_CLIENT_AB)?.takeIf { it.isNotBlank() } ?: DEFAULT_CLIENT_AB_VERSIONS

    // ---------- 身份标识（store 优先，其次 Cookie / 首页 hydration 补进来的） ----------

    /** `device_id`：`multi_sids` 第一段（TiktokApis）/ 首页 hydration 的 `wid`（catbus）。拿不到就是 null。 */
    val deviceId: String? get() = stored(KEY_DEVICE_ID) ?: multiSids()

    /** `odinId`：和 [deviceId] 同源；缺失时 TiktokApis 退回 `device_id`。 */
    val odinId: String? get() = stored(KEY_ODIN_ID) ?: multiSids() ?: deviceId

    /** 地区：首页 hydration 的 `region` → `store-country-code` Cookie → `SG`（TiktokApis / f2 的默认）。 */
    val region: String
        get() = stored(KEY_REGION)
            ?: cookies.find(WWW, "store-country-code")?.trim()?.uppercase()?.takeIf { it.length == 2 }
            ?: "SG"

    /** `priority_region`：和 [region] 同一个值。 */
    val priorityRegion: String get() = region

    /** query 里的 `WebIdLastTime`：首页 hydration 的 `webIdCreatedTime`，没有就记下首次用到的那一刻。 */
    val webIdLastTime: Long
        get() = stored(KEY_WEB_ID_TIME)?.toLongOrNull() ?: (clock() / 1000).also { store.put(KEY_WEB_ID_TIME, it.toString()) }

    /**
     * `s_v_web_id`（= query 里的 `verifyFp`）。**优先 Cookie 里的真值**，没有才生成并固定下来。
     * 浏览器那里是 `verify_<毫秒时间戳的 base36>_<8>_<4>_4<3>_[89ab]<3>_<12>`（catbus `svWebId`）。
     */
    val verifyFp: String
        get() = cookies.find(WWW, "s_v_web_id")?.takeIf { it.isNotBlank() }
            ?: stored(KEY_VERIFY_FP)
            ?: generateVerifyFp().also { store.put(KEY_VERIFY_FP, it) }

    /** `msToken`：**只用 Cookie / store 里的真值**，没有就是空字符串。 */
    val msToken: String
        get() = stored(KEY_MS_TOKEN) ?: cookies.find(WWW, "msToken")?.takeIf { it.isNotBlank() } ?: ""

    private fun stored(key: String): String? = store.get(key)?.takeIf { it.isNotBlank() }

    /** `multi_sids` 的第一段（`<device_id>%3A<...>` 或 `:` 分隔）。TiktokApis 的 `_sync_ids_from_cookies`。 */
    private fun multiSids(): String? {
        val raw = cookies.find(WWW, "multi_sids")?.takeIf { it.isNotBlank() } ?: return null
        val first = raw.replace("%3A", ":").replace("%3a", ":").substringBefore(':').trim()
        return first.takeIf { it.length in 16..20 && it.all(Char::isDigit) }
    }

    /** 把首页 hydration 里的服务端设备数据记下来（`bootstrap`，catbus 同款）。 */
    fun rememberDevice(wid: String?, odinId: String?, region: String?, webIdCreatedTime: Long?) {
        wid?.takeIf { it.isNotBlank() }?.let { store.put(KEY_DEVICE_ID, it) }
        odinId?.takeIf { it.isNotBlank() }?.let { store.put(KEY_ODIN_ID, it) }
        region?.takeIf { it.isNotBlank() }?.let { store.put(KEY_REGION, it.uppercase()) }
        webIdCreatedTime?.takeIf { it > 0 }?.let { store.put(KEY_WEB_ID_TIME, it.toString()) }
    }

    /** 服务端在响应里下发了新的 msToken 时记下来（`x-ms-token` 头 / Set-Cookie 由 Cookie 罐处理）。 */
    fun rememberMsToken(token: String?) {
        token?.takeIf { it.isNotBlank() }?.let { store.put(KEY_MS_TOKEN, it) }
    }

    private fun generateVerifyFp(): String {
        val base36 = java.lang.Long.toString(clock(), 36)
        fun rnd(n: Int) = buildString { repeat(n) { append(CHARSET[random.nextInt(CHARSET.length)]) } }
        val groups = listOf(rnd(8), rnd(4), "4" + rnd(3), "89ab"[random.nextInt(4)] + rnd(3), rnd(12))
        return "verify_${base36}_${groups.joinToString("_")}"
    }

    private companion object {
        const val KEY_DEVICE_ID = "tiktok.device_id"
        const val KEY_ODIN_ID = "tiktok.odin_id"
        const val KEY_REGION = "tiktok.region"
        const val KEY_WEB_ID_TIME = "tiktok.web_id_last_time"
        const val KEY_VERIFY_FP = "tiktok.verify_fp"
        const val KEY_MS_TOKEN = "tiktok.ms_token"
        const val KEY_CLIENT_AB = "tiktok.client_ab_versions"

        val WWW = "https://www.tiktok.com/".toHttpUrl()

        const val CHARSET = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"

        /**
         * Chrome 153 抓包里的 `clientABVersions`（TiktokApis `TiktokAuth.CLIENT_AB_VERSIONS` 原值）。
         * 这是数据不是算法：随版本变，
         * 或者用 `ctx.store` 的 `tiktok.client_ab_versions` 覆盖。
         */
        const val DEFAULT_CLIENT_AB_VERSIONS =
            "70508271,72437276,73720540,75360573,75657507,75843653,75878361," +
                "76074824,76122410,76179551,76403724,76499369,76626930,76651230," +
                "76669176,76713103,76740102,76792575,76805936,76813520,76815102," +
                "76826749,76839930,76840792,76887350,76889673,76907405,76933422," +
                "76945954,76950455,77005875,77011497,77014490,77056281,77064650," +
                "77071444,77093404,77120364,77141446,77150562,77155134,70138197," +
                "70156809,70405643,71057832,71200802,71381811,71803300,72360691," +
                "72408100,72854054,72892778,73171280,73208420,73989921,74276218," +
                "74844724,75330961"
    }
}
