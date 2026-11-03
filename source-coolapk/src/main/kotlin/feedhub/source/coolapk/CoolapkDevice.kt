package feedhub.source.coolapk

import feedhub.core.KeyValueStore
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.util.Base64
import kotlin.random.Random

/**
 * 冒充的酷安 App 版本。UA、`X-App-Version` / `X-App-Code` / `X-Api-Version` / `X-App-Supported` 都从这里取，保持一致。
 *
 * 同一天查到：
 * - 13.4.1 打 `/v6/feed/replyList` 回 403「评论接口异常，请尝试升级到新版酷安」（按版本拦评论）；
 * - 试着升到当时的最新正式版 16.6.4 / 2609291 / API 16（`/v6/apk/detail?id=com.coolapk.market` 查到的），
 *   评论改成要人机验证（`err_request_captcha_v2`），按规矩停了，而且新版本号下信息流没验过。
 * App 里酷安评论先不请求（`feedhub.app.detail.commentsSupported`）。
 */
internal object AppVersion {
    const val NAME = "13.4.1"
    const val CODE = "2312121"
    const val API = "13"
}

/**
 * 一台假的 Android 设备。第一次用时随机生成，存进 `ctx.store`（键 [STORE_KEY]），以后一直用同一台，
 * 这样设备码、UA、`X-Sdk-Int` 始终对得上。
 *
 * 设备码（`X-App-Device`）是官方 App 请求头里的格式：
 * `"数字联盟 id; ; ; mac; 厂商; 品牌; 型号; 版本号; null"` 做 base64，整个字符串倒过来，再去掉换行和 "="。
 */
@Serializable
internal data class CoolapkDevice(
    /** 数字联盟 id，随机 32 位大写十六进制。 */
    val szlmId: String,
    val mac: String,
    val manufacturer: String,
    val brand: String,
    val model: String,
    /** Build.ID / Build.DISPLAY。 */
    val buildId: String,
    val androidVersion: String,
    val sdkInt: Int,
) {
    val deviceInfo: String get() = "$szlmId; ; ; $mac; $manufacturer; $brand; $model; $buildId; null"

    /** `X-App-Device`：只算一次（base64 + reverse + 去换行），每个请求头 / token 都复用它。 */
    val code: String by lazy { encodeDeviceCode(deviceInfo) }

    /**
     * 官方 App 的 UA = 系统 `http.agent` + 设备信息 + 版本：
     * `Dalvik/2.1.0 (Linux; U; Android 14; 型号 Build/版本号) (#Build; 品牌; 型号; 版本号; 14) +CoolMarket/13.4.1-2312121-universal`
     */
    val userAgent: String
        get() = userAgentFor(AppVersion.NAME, AppVersion.CODE)

    /** 换一套版本号时的 UA（评论接口用 16.2.0 / 2604201 的独立身份，见 [CoolapkSource]）。 */
    fun userAgentFor(versionName: String, versionCode: String): String =
        "Dalvik/2.1.0 (Linux; U; Android $androidVersion; $model Build/$buildId) " +
            "(#Build; $brand; $model; $buildId; $androidVersion) +CoolMarket/$versionName-$versionCode-universal"

    /** 官方 App 每个请求都带的固定请求头（不含随时间变化的 `X-App-Token`）。 */
    fun headers(): Map<String, String> = linkedMapOf(
        "User-Agent" to userAgent,
        "X-Requested-With" to "XMLHttpRequest",
        "X-Sdk-Int" to sdkInt.toString(),
        "X-Sdk-Locale" to "zh-CN",
        "X-App-Id" to "com.coolapk.market",
        "X-App-Version" to AppVersion.NAME,
        "X-App-Code" to AppVersion.CODE,
        "X-Api-Version" to AppVersion.API,
        "X-App-Device" to code,
        "X-Dark-Mode" to "0",
        "X-App-Channel" to "coolapk",
        "X-App-Mode" to "universal",
        "X-App-Supported" to AppVersion.CODE,
    )

    companion object {
        const val STORE_KEY = "coolapk.device"
        private val json = Json { ignoreUnknownKeys = true }

        fun encodeDeviceCode(deviceInfo: String): String {
            val b64 = Base64.getEncoder().encodeToString(deviceInfo.encodeToByteArray())
            return b64.reversed().filterNot { it == '=' || it == '\r' || it == '\n' }
        }

        /** 存过就读出来，没存过就生成一台并存下。存的内容坏了也重新生成。 */
        fun loadOrCreate(store: KeyValueStore, random: Random = Random.Default): CoolapkDevice {
            store.get(STORE_KEY)?.let { saved ->
                runCatching { return json.decodeFromString(serializer(), saved) }
            }
            return random(random).also { store.put(STORE_KEY, json.encodeToString(serializer(), it)) }
        }

        fun random(random: Random): CoolapkDevice {
            val p = PROFILES[random.nextInt(PROFILES.size)]
            return CoolapkDevice(
                szlmId = random.nextBytes(16).joinToString("") { "%02X".format(it) },
                mac = random.nextBytes(6).joinToString(":") { "%02x".format(it) },
                manufacturer = p.manufacturer,
                brand = p.brand,
                model = p.model,
                buildId = p.buildId,
                androidVersion = p.androidVersion,
                sdkInt = p.sdkInt,
            )
        }

        private class Profile(
            val manufacturer: String,
            val brand: String,
            val model: String,
            val buildId: String,
            val androidVersion: String,
            val sdkInt: Int,
        )

        /** 几款常见国行机型（Android 14）。只是让设备信息看起来正常，没有别的讲究。 */
        private val PROFILES = listOf(
            Profile("Xiaomi", "Xiaomi", "23127PN0CC", "UKQ1.230804.001", "14", 34),
            Profile("Xiaomi", "Redmi", "23113RKC6C", "UKQ1.230804.001", "14", 34),
            Profile("OnePlus", "OnePlus", "PJD110", "UKQ1.230924.001", "14", 34),
            Profile("vivo", "vivo", "V2309A", "UP1A.231005.007", "14", 34),
            Profile("OPPO", "OPPO", "PHZ110", "UKQ1.231108.001", "14", 34),
        )
    }
}
